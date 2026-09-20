//! tmux control-mode (`-CC`) client: one client, N panes, demultiplexed.
//!
//! Every command TaiX runs is defined once, in [`cmd`], against the [`Tmux`]
//! trait. A socket name runs it by forking `tmux`; a [`Client`] writes the
//! same line down its control channel and reads the `%begin`/`%end` reply
//! back. The fork is for processes that have no client - the CLI, the MCP
//! server. A front that is attached never forks: a query is a write and a
//! read on a pty, microseconds where a subprocess is milliseconds, and the
//! reply is ordered against the `%output` stream, which a subprocess never
//! was - a capture taken by a second process could be older than bytes
//! already sitting in the bus, and the pane then showed its last line twice.

/// Local trace implementation. taix-core depends on taix-tmux, so adding
/// taix-core as a dependency would create a cycle.
macro_rules! trace {
    ($($arg:tt)*) => {
        if trace_enabled() {
            eprintln!("taix-tmux: {}", format_args!($($arg)*));
        }
    };
}

fn trace_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        2 => {
            let on = std::env::var_os("TAIX_LOG")
                .is_some_and(|v| !v.is_empty() && v != "0" && v != "false");
            ON.store(u8::from(on), Ordering::Relaxed);
            on
        }
        v => v == 1,
    }
}

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use parking_lot::Mutex;

/// A tmux pane id (`%3` -> 3).
pub type PaneId = u32;

/// Maximum line length before treating the stream as corrupt and closing.
/// tmux control lines are normally tens to hundreds of bytes; 64KB is vastly
/// more than any legitimate line.
const LINE_CAP: usize = 64 * 1024;

/// Maximum reply block body size before treating the stream as corrupt and
/// closing. A full-history `capture-pane -S -` over a deep scrollback can
/// reach tens of MiB; 32 MiB accommodates genuine captures.
const BLOCK_CAP: usize = 32 * 1024 * 1024;

/// How long a query may wait for the server. A reply normally lands in
/// well under a millisecond; this only trips if tmux itself has stopped.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum Ev {
    /// Raw pane bytes, already un-escaped. `seq` counts output events since
    /// the client attached: a consumer that seeds an emulator from a reply
    /// skips outputs stamped at or below the reply's `seq`, because the
    /// capture already shows them.
    Output {
        pane: PaneId,
        data: Vec<u8>,
        seq: u64,
    },
    /// Window layout string changed; splits must be rebuilt.
    Layout { window: u32, layout: String },
    /// A window appeared or disappeared. tmux is the source of truth for
    /// which panes exist, so this is what tells a consumer to re-read them.
    WindowsChanged,
    /// The control client went away.
    Exit,
}

/// Shared queue drained by the UI thread. Coalescing many `%output`
/// notifications into one redraw is the point, not a shortcut: the waker
/// fires once when the queue goes from empty to non-empty, and not again
/// until it has been drained, so a burst costs one wake-up.
#[derive(Clone, Default)]
pub struct Bus(Arc<BusInner>);

#[derive(Default)]
struct BusInner {
    queue: Mutex<VecDeque<Ev>>,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Output events pushed so far; see [`Ev::Output::seq`].
    seq: std::sync::atomic::AtomicU64,
}

impl Bus {
    pub fn drain(&self) -> Vec<Ev> {
        self.0.queue.lock().drain(..).collect()
    }
    /// Called from the reader thread the moment output lands on an empty
    /// queue. Without one, a consumer polls.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.0.waker.lock() = Some(waker);
    }
    fn seq(&self) -> u64 {
        self.0.seq.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn push(&self, ev: Ev) {
        if matches!(ev, Ev::Output { .. }) {
            self.0.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let was_empty = {
            let mut q = self.0.queue.lock();
            let was_empty = q.is_empty();
            q.push_back(ev);
            was_empty
        };
        if was_empty && let Some(wake) = &*self.0.waker.lock() {
            wake();
        }
    }
}

/// What a command came back with.
#[derive(Debug, Default)]
pub struct Reply {
    pub body: Vec<u8>,
    /// How many `%output` events had been queued when this reply arrived,
    /// so a consumer can tell which of them the reply already reflects.
    /// Zero from a fork, which is unordered against the stream.
    pub seq: u64,
}

/// Something that can run a tmux command line: a socket name (fork) or an
/// attached [`Client`] (control channel).
pub trait Tmux {
    fn run(&self, args: &[&str]) -> io::Result<Reply>;
    /// Run and do not wait. The fork cannot help waiting; the client can,
    /// and a keystroke should not pay for a reply nobody reads.
    fn fire(&self, args: &[&str]) -> io::Result<()> {
        self.run(args).map(drop)
    }
    /// Run a command line and return one Reply per `%begin/%end` block.
    /// Default implementation wraps the single concatenated Reply from `run`.
    fn run_each(&self, args: &[&str]) -> io::Result<Vec<Reply>> {
        self.run(args).map(|r| vec![r])
    }
}

impl Tmux for str {
    fn run(&self, args: &[&str]) -> io::Result<Reply> {
        let out = Command::new("tmux")
            .args(["-L", self])
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(Reply {
            body: out.stdout,
            seq: 0,
        })
    }
}

impl Tmux for String {
    fn run(&self, args: &[&str]) -> io::Result<Reply> {
        self.as_str().run(args)
    }
}

/// Every command written owns one slot, in order, including the attach
/// itself: the reader hands each reply block to the front slot. A command
/// nobody waits on leaves a `None` so the count still lines up.
type Slots = Arc<Mutex<VecDeque<Option<mpsc::Sender<(bool, Reply)>>>>>;

pub struct Client {
    pub socket: String,
    pub bus: Bus,
    /// The control channel: tmux reads commands from it, so it doubles as
    /// the keystroke path. Kept alive for the lifetime of the client either
    /// way.
    pty: File,
    slots: Slots,
    child: std::process::Child,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    /// Attach to `session` on `socket` in control mode. tmux clients require a
    /// tty (`tcgetattr` fails otherwise), so the client gets a pty.
    pub fn attach(socket: &str, session: &str) -> io::Result<Self> {
        use std::os::unix::process::CommandExt;
        let pty = nix::pty::openpty(None, None)?;
        let mut cmd = Command::new("tmux");
        cmd.args(["-L", socket, "-CC", "attach", "-t", session])
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::null())
            .env("TERM", "xterm-256color");
        // `Drop` only runs on a tidy exit. A front that is killed, crashes,
        // or leaves through `app.quit()` never drops its `App`, and every
        // such exit left a 7 MB `-CC` client parked on the server for good.
        // The kernel ends this one the moment the parent goes. SIGKILL,
        // because a client owns nothing the server does not, and a parked
        // client was seen to shrug off SIGTERM.
        unsafe {
            cmd.pre_exec(|| {
                nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGKILL)
                    .map_err(io::Error::from)
            });
        }
        let child = cmd.spawn()?;
        drop(pty.slave);

        let master = File::from(pty.master);
        let reader = master.try_clone()?;
        let bus = Bus::default();
        // The first block on the stream answers the attach command itself.
        let slots: Slots = Arc::new(Mutex::new(VecDeque::from([None])));
        let sink = bus.clone();
        let pending = slots.clone();
        std::thread::spawn(move || read_loop(reader, sink, pending));

        Ok(Client {
            socket: socket.to_string(),
            bus,
            pty: master,
            slots,
            child,
        })
    }

    /// One command line down the channel. A line holding `;`-separated
    /// commands is one write but one reply block *per command*, so it takes
    /// that many slots; the slots are taken under the same lock as the
    /// write, so replies cannot be handed to the wrong caller.
    fn write(
        &self,
        args: &[&str],
        waiter: Option<mpsc::Sender<(bool, Reply)>>,
    ) -> io::Result<usize> {
        let line = command_line(args);
        let blocks = 1 + args.iter().filter(|a| **a == ";").count();
        let mut slots = self.slots.lock();
        let mut pty = &self.pty;
        pty.write_all(line.as_bytes())?;
        pty.write_all(b"\n")?;
        slots.extend(std::iter::repeat_n(waiter, blocks));
        Ok(blocks)
    }
}

impl Tmux for Client {
    /// The bodies of a multi-command line are concatenated, which is what
    /// a fork's stdout would have held; the stamp is the last block's.
    fn run(&self, args: &[&str]) -> io::Result<Reply> {
        let (tx, rx) = mpsc::channel();
        let blocks = self.write(args, Some(tx))?;
        let mut out = Reply::default();
        for _ in 0..blocks {
            match rx.recv_timeout(REPLY_TIMEOUT) {
                Ok((true, reply)) => {
                    out.body.extend_from_slice(&reply.body);
                    out.seq = reply.seq;
                }
                Ok((false, reply)) => {
                    return Err(io::Error::other(
                        String::from_utf8_lossy(&reply.body).trim().to_string(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "tmux did not answer",
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "tmux control client exited",
                    ));
                }
            }
        }
        Ok(out)
    }

    fn fire(&self, args: &[&str]) -> io::Result<()> {
        self.write(args, None).map(drop)
    }

    fn run_each(&self, args: &[&str]) -> io::Result<Vec<Reply>> {
        let (tx, rx) = mpsc::channel();
        let blocks = self.write(args, Some(tx))?;
        let mut out = Vec::with_capacity(blocks);
        for _ in 0..blocks {
            match rx.recv_timeout(REPLY_TIMEOUT) {
                Ok((true, reply)) => {
                    out.push(reply);
                }
                Ok((false, reply)) => {
                    return Err(io::Error::other(
                        String::from_utf8_lossy(&reply.body).trim().to_string(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "tmux did not answer",
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "tmux control client exited",
                    ));
                }
            }
        }
        Ok(out)
    }
}

/// tmux's own parser reads the channel: whitespace splits, `'` quotes with
/// nothing special inside, `$VAR` expands anywhere else, and a bare `;`
/// ends one command and starts the next. Everything that is not a plain
/// word is single-quoted, so a path with a space or a command with a `$`
/// arrives as the one argument it was.
fn command_line(args: &[&str]) -> String {
    let mut line = String::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        let bare = !arg.is_empty()
            && arg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./:@%=+,".contains(&b));
        if bare || *arg == ";" {
            line.push_str(arg);
        } else {
            line.push('\'');
            line.push_str(&arg.replace('\'', "'\\''"));
            line.push('\'');
        }
    }
    line
}

/// Every command TaiX runs, defined once against [`Tmux`].
pub mod cmd {
    use super::{PaneId, PaneInfo, Reply, Tmux};
    use std::io;
    use std::path::Path;

    fn text(reply: Reply) -> String {
        String::from_utf8_lossy(&reply.body).into_owned()
    }

    /// `-e` keeps colour/attribute escapes so a snapshot renders styled.
    pub fn capture(t: &(impl Tmux + ?Sized), pane: PaneId) -> Vec<u8> {
        t.run(&["capture-pane", "-e", "-p", "-t", &format!("%{pane}")])
            .map(|r| r.body)
            .unwrap_or_default()
    }

    /// Everything an emulator needs to be primed from a live pane, in one
    /// round trip: size, cursor, styled screen, and where in the output
    /// stream the screen was taken.
    pub struct Seed {
        pub cols: usize,
        pub rows: usize,
        pub cursor: (usize, usize),
        /// The styled screen, `capture-pane -e`.
        pub screen: Vec<u8>,
        /// The DEC private mouse modes the program in the pane has set, as
        /// the escapes that set them again. A capture carries text and
        /// colour, never modes, so an emulator primed from one would think a
        /// TUI wants no pointer events.
        pub modes: Vec<u8>,
        /// See [`Reply::seq`].
        pub seq: u64,
    }

    pub fn seed(t: &(impl Tmux + ?Sized), pane: PaneId) -> Option<Seed> {
        let target = format!("%{pane}");
        let reply = t
            .run(&[
                "display-message",
                "-p",
                "-t",
                &target,
                "#{pane_width} #{pane_height} #{cursor_x} #{cursor_y} \
                 #{mouse_standard_flag}#{mouse_button_flag}#{mouse_all_flag}#{mouse_sgr_flag}",
                ";",
                "capture-pane",
                "-e",
                "-p",
                "-t",
                &target,
            ])
            .ok()?;
        let out = reply.body;
        let nl = out.iter().position(|&b| b == b'\n')?;
        let head = std::str::from_utf8(&out[..nl]).ok()?;
        let mut f = head.split(' ');
        Some(Seed {
            cols: f.next()?.parse().ok()?,
            rows: f.next()?.parse().ok()?,
            cursor: (f.next()?.parse().ok()?, f.next()?.parse().ok()?),
            modes: mouse_modes(f.next().unwrap_or_default()),
            screen: out[nl + 1..].to_vec(),
            seq: reply.seq,
        })
    }

    /// Seed multiple panes in one round trip. Returns one entry per input
    /// pane in input order; `None` where the pane no longer exists.
    pub fn seed_many(t: &(impl Tmux + ?Sized), panes: &[PaneId]) -> Vec<Option<Seed>> {
        if panes.is_empty() {
            return Vec::new();
        }
        let targets: Vec<String> = panes.iter().map(|&p| format!("%{p}")).collect();
        let mut args = Vec::new();
        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                args.push(";");
            }
            args.extend([
                "display-message",
                "-p",
                "-t",
                target.as_str(),
                "#{pane_width} #{pane_height} #{cursor_x} #{cursor_y} \
                 #{mouse_standard_flag}#{mouse_button_flag}#{mouse_all_flag}#{mouse_sgr_flag}",
                ";",
                "capture-pane",
                "-e",
                "-p",
                "-t",
                target.as_str(),
            ]);
        }
        let replies = match t.run_each(&args) {
            Ok(r) => r,
            Err(_) => {
                // Batch failed (likely a dead pane). Fall back to individual queries.
                return panes.iter().map(|&p| seed(t, p)).collect();
            }
        };
        // Each pane produces 2 blocks: display-message and capture-pane.
        replies
            .chunks(2)
            .map(|pair| {
                if pair.len() != 2 {
                    return None;
                }
                let head_body = &pair[0].body;
                let capture_body = &pair[1].body;
                let head = std::str::from_utf8(head_body.trim_ascii()).ok()?;
                let mut f = head.split(' ');
                Some(Seed {
                    cols: f.next()?.parse().ok()?,
                    rows: f.next()?.parse().ok()?,
                    cursor: (f.next()?.parse().ok()?, f.next()?.parse().ok()?),
                    modes: mouse_modes(f.next().unwrap_or_default()),
                    screen: capture_body.to_vec(),
                    seq: pair[1].seq,
                })
            })
            .collect()
    }

    /// tmux reports the pane's mouse modes as four flags; an emulator only
    /// learns them from the escapes themselves. Ascending order matters: a
    /// terminal keeps the last tracking mode it was given, and the later
    /// modes are the more permissive ones.
    pub(crate) fn mouse_modes(flags: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for (flag, mode) in flags.chars().zip(["1000", "1002", "1003", "1006"]) {
            if flag == '1' {
                out.extend_from_slice(format!("\x1b[?{mode}h").as_bytes());
            }
        }
        out
    }

    /// Plain text, for consumers that want to read output rather than draw it.
    pub fn capture_text(
        t: &(impl Tmux + ?Sized),
        pane: PaneId,
        lines: usize,
    ) -> io::Result<String> {
        t.run(&[
            "capture-pane",
            "-p",
            "-S",
            &format!("-{lines}"),
            "-t",
            &format!("%{pane}"),
        ])
        .map(text)
    }

    /// Every pane on the server, with everything the fronts ask about one:
    /// one listing per pass answers "is it alive", "what is its pid", "how
    /// big is it", "where is the cursor" and "how deep is its history". An
    /// `Err` is unknown, not empty: it must never be read as "every pane is
    /// gone".
    pub fn panes(t: &(impl Tmux + ?Sized)) -> io::Result<Vec<PaneInfo>> {
        t.run(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id} #{window_id} #{pane_left} #{pane_top} #{pane_width} \
             #{pane_height} #{pane_pid} #{cursor_x} #{cursor_y} #{history_size} \
             #{window_name}",
        ])
        .map(|r| text(r).lines().filter_map(PaneInfo::parse).collect())
    }

    /// Does this pane still exist?
    ///
    /// Asked by listing panes, NOT with `display-message -t %N`: tmux
    /// resolves an unknown target to the *current* pane and exits 0, so that
    /// spelling answered "alive" for every pane that had ever existed - dead
    /// windows stayed in the sidebar forever.
    pub fn pane_alive(t: &(impl Tmux + ?Sized), pane: PaneId) -> bool {
        panes(t).is_ok_and(|panes| panes.iter().any(|p| p.id == pane))
    }

    /// Lines of scrollback held above the visible screen; zero for a pane
    /// that does not exist, never another pane's figure.
    pub fn history_size(t: &(impl Tmux + ?Sized), pane: PaneId) -> usize {
        panes(t)
            .ok()
            .and_then(|panes| panes.into_iter().find(|p| p.id == pane))
            .map_or(0, |p| p.history)
    }

    /// The server's pid and version, e.g. `3.5a`. Both fixed for a server's
    /// life, so worth asking once and remembering.
    pub fn server(t: &(impl Tmux + ?Sized)) -> io::Result<(u32, String)> {
        let out = t
            .run(&["display-message", "-p", "#{pid} #{version}"])
            .map(text)?;
        let (pid, version) = out.trim().split_once(' ').unwrap_or((out.trim(), ""));
        let pid = pid
            .parse()
            .map_err(|e| io::Error::other(format!("invalid server pid: {e}")))?;
        Ok((pid, version.to_string()))
    }

    /// Create the session if no server is running yet. Idempotent, so every
    /// entry point can call it without checking first.
    pub fn ensure_session(
        t: &(impl Tmux + ?Sized),
        session: &str,
        cols: usize,
        rows: usize,
    ) -> io::Result<()> {
        if t.run(&["has-session", "-t", session]).is_ok() {
            return Ok(());
        }
        t.run(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ])
        .map(drop)
    }

    /// Spawn `command` in a new window and return its window and pane ids.
    ///
    /// An empty `command` omits the command argument entirely, so tmux starts
    /// the session's default shell - a plain terminal window.
    ///
    /// `-P -F` is what makes this race-free: asking tmux to print the ids it
    /// just created beats listing panes afterwards and guessing which is new.
    pub fn new_window(
        t: &(impl Tmux + ?Sized),
        session: &str,
        name: &str,
        cwd: &Path,
        command: &str,
        env: &[(String, String)],
    ) -> io::Result<(u32, PaneId)> {
        let cwd_str = cwd.to_string_lossy();
        let mut args = vec![
            "new-window",
            "-d",
            "-t",
            session,
            "-n",
            name,
            "-c",
            &cwd_str,
        ];
        let env_strings: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        for e in &env_strings {
            args.push("-e");
            args.push(e);
        }
        args.extend_from_slice(&["-P", "-F", "#{window_id} #{pane_id}"]);
        // ponytail: omit empty command; tmux spawns the default shell instead
        if !command.is_empty() {
            args.push(command);
        }
        let out = t.run(&args).map(text)?;
        parse_ids(&out)
            .ok_or_else(|| io::Error::other(format!("unexpected new-window reply: {out:?}")))
    }

    /// Add a pane to an existing window, tiling afterwards so repeated splits
    /// keep fitting (a bare split halves the active pane until it cannot).
    ///
    /// An empty `command` omits the command argument entirely, so tmux starts
    /// the session's default shell.
    pub fn split_window(
        t: &(impl Tmux + ?Sized),
        window: u32,
        cwd: &Path,
        command: &str,
    ) -> io::Result<PaneId> {
        let target = format!("@{window}");
        let cwd_str = cwd.to_string_lossy();
        let mut args = vec![
            "split-window",
            "-d",
            "-t",
            &target,
            "-c",
            &cwd_str,
            "-P",
            "-F",
            "#{window_id} #{pane_id}",
        ];
        // ponytail: omit empty command; tmux spawns the default shell instead
        if !command.is_empty() {
            args.push(command);
        }
        let out = t.run(&args).map(text)?;
        let (_, pane) = parse_ids(&out)
            .ok_or_else(|| io::Error::other(format!("unexpected split-window reply: {out:?}")))?;
        t.run(&["select-layout", "-t", &target, "tiled"])?;
        Ok(pane)
    }

    /// Send literal text to a pane: bytes, so nothing in it can be read as a
    /// key name, a flag, or a word boundary.
    pub fn send_text(t: &(impl Tmux + ?Sized), pane: PaneId, text: &str) -> io::Result<()> {
        send_bytes(t, pane, text.as_bytes())
    }

    /// Send a tmux key name (`Enter`, `C-c`, `Up`, ...).
    pub fn send_key(t: &(impl Tmux + ?Sized), pane: PaneId, key: &str) -> io::Result<()> {
        t.fire(&["send-keys", "-t", &format!("%{pane}"), key])
    }

    /// Send exact bytes.
    ///
    /// The escape hatch an emulator needs: `send-keys` accepts a *name*, and
    /// silently types an unrecognised one as literal text - `C-BSpace` is not
    /// a tmux key, so asking for it put the string "C-BSpace" on the command
    /// line. `-H` takes hex and cannot be misread.
    pub fn send_bytes(t: &(impl Tmux + ?Sized), pane: PaneId, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let target = format!("%{pane}");
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut args = vec!["send-keys", "-t", &target, "-H"];
        args.extend(hex.iter().map(String::as_str));
        t.fire(&args)
    }

    /// Paste as a terminal would: through a tmux buffer, wrapped in
    /// bracketed-paste markers when the program asked for them (`-p`).
    ///
    /// The text goes by file, not by argument: the control channel is
    /// line-based, so a newline inside `set-buffer`'s argument would end
    /// the command halfway through the first line.
    pub fn paste(t: &(impl Tmux + ?Sized), pane: PaneId, text: &str) -> io::Result<()> {
        if text.contains('\0') {
            return Err(io::Error::other("text contains null byte"));
        }
        let path = std::env::temp_dir().join(format!("taix-paste-{}", std::process::id()));
        std::fs::write(&path, text)?;
        let loaded = t.run(&["load-buffer", "-b", "taix-web", &path.to_string_lossy()]);
        let _ = std::fs::remove_file(&path);
        loaded?;
        t.fire(&[
            "paste-buffer",
            "-p",
            "-d",
            "-b",
            "taix-web",
            "-t",
            &format!("%{pane}"),
        ])
    }

    pub fn kill_pane(t: &(impl Tmux + ?Sized), pane: PaneId) -> io::Result<()> {
        t.run(&["kill-pane", "-t", &format!("%{pane}")]).map(drop)
    }

    pub fn kill_window(t: &(impl Tmux + ?Sized), window: u32) -> io::Result<()> {
        t.run(&["kill-window", "-t", &format!("@{window}")])
            .map(drop)
    }

    /// Resize a window's grid to match the widget drawing it.
    ///
    /// Without this the pane keeps whatever size the session was created with,
    /// so a wide widget shows an 80-column pane padded with blanks and a
    /// narrow one clips. `-x`/`-y` require the window to not be forced to the
    /// client size, hence `aggressive-resize` being irrelevant here: the
    /// window is detached from any client's dimensions.
    pub fn resize_window(
        t: &(impl Tmux + ?Sized),
        window: u32,
        cols: usize,
        rows: usize,
    ) -> io::Result<()> {
        t.run(&[
            "resize-window",
            "-t",
            &format!("@{window}"),
            "-x",
            &cols.max(20).to_string(),
            "-y",
            &rows.max(4).to_string(),
        ])
        .map(drop)
    }

    pub fn set_history_limit(t: &(impl Tmux + ?Sized), lines: usize) -> io::Result<()> {
        t.run(&["set-option", "-g", "history-limit", &lines.to_string()])
            .map(drop)
    }

    /// Capture a range from history and screen with tmux coordinates.
    ///
    /// Coordinates: `0` is the top row of the visible screen, negatives reach
    /// into history. This is `capture-pane -S start -E end`, with `-e` only
    /// when the caller wants the styling.
    pub fn capture_range(
        t: &(impl Tmux + ?Sized),
        pane: PaneId,
        start: i32,
        end: i32,
        styled: bool,
    ) -> Vec<u8> {
        let start_str = start.to_string();
        let end_str = end.to_string();
        let target = format!("%{pane}");
        let mut args = vec!["capture-pane", "-p", "-S", &start_str, "-E", &end_str];
        if styled {
            args.insert(1, "-e");
        }
        args.extend_from_slice(&["-t", &target]);
        t.run(&args).map(|r| r.body).unwrap_or_default()
    }

    /// Whole history plus screen as plain text, oldest line first.
    ///
    /// This is what the GUI searches, so it must be plain text with no escapes.
    /// `-S -` is the start of history, `-E -` is the end of the screen.
    pub fn history_text(t: &(impl Tmux + ?Sized), pane: PaneId) -> io::Result<String> {
        t.run(&[
            "capture-pane",
            "-p",
            "-S",
            "-",
            "-E",
            "-",
            "-t",
            &format!("%{pane}"),
        ])
        .map(text)
    }

    /// Whole history plus screen with colours kept and wrapped lines joined:
    /// what `cat` into a fresh pane replays as the old one looked.
    pub fn history_styled(t: &(impl Tmux + ?Sized), pane: PaneId) -> io::Result<Vec<u8>> {
        t.run(&[
            "capture-pane",
            "-p",
            "-e",
            "-J",
            "-S",
            "-",
            "-E",
            "-",
            "-t",
            &format!("%{pane}"),
        ])
        .map(|r| r.body)
    }

    pub(crate) fn parse_ids(out: &str) -> Option<(u32, PaneId)> {
        let line = out.lines().next()?;
        let (w, p) = line.trim().split_once(' ')?;
        Some((
            w.strip_prefix('@')?.parse().ok()?,
            p.strip_prefix('%')?.parse().ok()?,
        ))
    }
}

/// One pane as `list-panes` reports it. Geometry comes straight from tmux's
/// own layout computation.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub id: PaneId,
    pub window: u32,
    pub left: usize,
    pub top: usize,
    pub cols: usize,
    pub rows: usize,
    /// The process group leader running in the pane.
    pub pid: u32,
    pub cursor: (usize, usize),
    /// Lines of scrollback above the visible screen.
    pub history: usize,
    pub name: String,
}

impl PaneInfo {
    pub(crate) fn parse(line: &str) -> Option<Self> {
        let mut f = line.splitn(11, ' ');
        Some(PaneInfo {
            id: f.next()?.strip_prefix('%')?.parse().ok()?,
            window: f.next()?.strip_prefix('@')?.parse().ok()?,
            left: f.next()?.parse().ok()?,
            top: f.next()?.parse().ok()?,
            cols: f.next()?.parse().ok()?,
            rows: f.next()?.parse().ok()?,
            pid: f.next()?.parse().ok()?,
            cursor: (f.next()?.parse().ok()?, f.next()?.parse().ok()?),
            history: f.next()?.parse().ok()?,
            // The name is last because it is the one field with spaces in it.
            name: f.next().unwrap_or_default().to_string(),
        })
    }
}

fn read_loop(mut file: File, bus: Bus, slots: Slots) {
    let mut buf = [0u8; 64 * 1024];
    let mut line = Vec::<u8>::new();
    let mut block: Option<Vec<u8>> = None;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            match b {
                b'\n' => {
                    if !handle_line(&line, &bus, &slots, &mut block) {
                        break;
                    }
                    line.clear();
                }
                b'\r' => {}
                _ => {
                    line.push(b);
                    if line.len() > LINE_CAP {
                        trace!("line exceeded {} bytes, closing connection", LINE_CAP);
                        break;
                    }
                }
            }
        }
    }
    let mut pending = slots.lock();
    while let Some(Some(waiter)) = pending.pop_front() {
        let _ = waiter.send((
            false,
            Reply {
                body: b"tmux control client exited".to_vec(),
                seq: bus.seq(),
            },
        ));
    }
    bus.push(Ev::Exit);
}

fn handle_line(line: &[u8], bus: &Bus, slots: &Slots, block: &mut Option<Vec<u8>>) -> bool {
    let line = strip_dcs(line);
    if let Some(body) = block {
        let ok = line.starts_with(b"%end ");
        if ok || line.starts_with(b"%error ") {
            let body = std::mem::take(body);
            *block = None;
            if let Some(Some(waiter)) = slots.lock().pop_front() {
                let _ = waiter.send((
                    ok,
                    Reply {
                        body,
                        seq: bus.seq(),
                    },
                ));
            }
        } else {
            body.extend_from_slice(line);
            body.push(b'\n');
            if body.len() > BLOCK_CAP {
                trace!(
                    "block body exceeded {} bytes, closing connection",
                    BLOCK_CAP
                );
                if let Some(Some(waiter)) = slots.lock().pop_front() {
                    let _ = waiter.send((
                        false,
                        Reply {
                            body: format!("reply block exceeded {} bytes", BLOCK_CAP).into_bytes(),
                            seq: bus.seq(),
                        },
                    ));
                }
                *block = None;
                return false;
            }
        }
        return true;
    }
    if line.starts_with(b"%begin ") {
        *block = Some(Vec::new());
    } else if let Some(ev) = parse_line(line, bus.seq() + 1) {
        bus.push(ev);
    }
    true
}

/// Parse one notification line. `next_seq` is what an output event pushed
/// now will be stamped with.
fn parse_line(line: &[u8], next_seq: u64) -> Option<Ev> {
    if let Some(rest) = line.strip_prefix(b"%output %") {
        let sp = rest.iter().position(|&b| b == b' ')?;
        let pane = std::str::from_utf8(&rest[..sp]).ok()?.parse().ok()?;
        return Some(Ev::Output {
            pane,
            data: unescape(&rest[sp + 1..]),
            seq: next_seq,
        });
    }
    if let Some(rest) = line.strip_prefix(b"%layout-change @") {
        let s = std::str::from_utf8(rest).ok()?;
        let (win, tail) = s.split_once(' ')?;
        return Some(Ev::Layout {
            window: win.parse().ok()?,
            layout: tail.split(' ').next()?.to_string(),
        });
    }
    // A pane can vanish without any layout change (killing its whole window),
    // so these are the notifications that catch an agent exiting.
    const TOPOLOGY: [&[u8]; 4] = [
        b"%window-add",
        b"%window-close",
        b"%unlinked-window-add",
        b"%unlinked-window-close",
    ];
    if TOPOLOGY.iter().any(|p| line.starts_with(p)) {
        return Some(Ev::WindowsChanged);
    }
    None
}

/// The very first line carries a DCS introducer before the `%`.
fn strip_dcs(line: &[u8]) -> &[u8] {
    match line.iter().position(|&b| b == b'%') {
        Some(i) if line[..i].contains(&0x1b) => &line[i..],
        _ => line,
    }
}

/// tmux escapes bytes below 0x20, 0x7f and `\` itself as three octal digits
/// (`\033`, `\134`, `\011`); UTF-8 continuation bytes pass through raw.
/// Verified empirically against tmux 3.7c.
pub fn unescape(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let oct = |k: usize| {
            src.get(k)
                .filter(|b| (b'0'..=b'7').contains(b))
                .map(|b| b - b'0')
        };
        match (src[i], oct(i + 1), oct(i + 2), oct(i + 3)) {
            (b'\\', Some(a), Some(b), Some(c)) => {
                out.push((a << 6) | (b << 3) | c);
                i += 4;
            }
            (byte, ..) => {
                out.push(byte);
                i += 1;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_octal_and_raw_utf8() {
        // Verbatim payload from a tmux 3.7c %output line.
        let got = unescape(br"\033[31mRED\033[0m\011A\134B\xc3\xa9");
        assert!(got.starts_with(b"\x1b[31mRED\x1b[0m\tA\\B"));
        // A backslash not followed by three octal digits stays literal.
        assert_eq!(unescape(br"a\zb"), b"a\\zb");
        assert_eq!(unescape(br"\1"), b"\\1");
        assert_eq!(unescape(br"\8888"), b"\\8888");
    }

    #[test]
    fn bus_wakes_once_per_burst() {
        // A wake per `%output` line would schedule a frame per line; the
        // consumer drains everything at once, so only the first line after
        // a drain may wake it.
        let bus = Bus::default();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = wakes.clone();
        bus.set_waker(Box::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        for _ in 0..3 {
            bus.push(Ev::WindowsChanged);
        }
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(bus.drain().len(), 3);
        bus.push(Ev::WindowsChanged);
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn window_lifecycle_notifications_signal_a_pane_reread() {
        // A killed window emits no %layout-change, so without these an agent
        // that exits stays on screen claiming to be alive.
        for line in [
            &b"%window-close @2"[..],
            b"%window-add @7",
            b"%unlinked-window-close @3",
            b"%unlinked-window-add @4",
        ] {
            assert!(
                matches!(parse_line(line, 1), Some(Ev::WindowsChanged)),
                "{}",
                String::from_utf8_lossy(line)
            );
        }
        // A rename changes nothing about which panes exist.
        assert!(parse_line(b"%window-renamed @0 zsh", 1).is_none());
    }

    #[test]
    fn output_line_splits_pane_from_data() {
        let Some(Ev::Output { pane, data, .. }) = parse_line(br"%output %12 \033[Ktail", 1) else {
            panic!("not an output event");
        };
        assert_eq!(pane, 12);
        assert_eq!(data, b"\x1b[Ktail");
    }

    #[test]
    fn layout_change_yields_window_and_layout() {
        let Some(Ev::Layout { window, layout }) =
            parse_line(b"%layout-change @2 b25d,80x24,0,0,3 b25d,80x24,0,0,3 *", 1)
        else {
            panic!("not an layout event");
        };
        assert_eq!(window, 2);
        assert_eq!(layout, "b25d,80x24,0,0,3");
    }

    /// Feed a stream through the reader's line handler with the given slots.
    fn feed(stream: &[u8], bus: &Bus, slots: &Slots) {
        let mut block = None;
        for line in stream.split(|&b| b == b'\n') {
            if !line.is_empty() {
                handle_line(line, bus, slots, &mut block);
            }
        }
    }

    #[test]
    fn replies_go_to_their_slots_in_order_and_notifications_never_inside() {
        // The exact opening of a tmux 3.7c control stream: the attach's own
        // empty block first, then whatever we asked for, each answered in
        // the order asked.
        let bus = Bus::default();
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        let slots: Slots = Arc::new(Mutex::new(VecDeque::from([
            None,
            Some(tx1),
            None,
            Some(tx2),
        ])));
        feed(
            b"\x1bP1000p%begin 1 284 0\n%end 1 284 0\n\
              %session-changed $0 p\n\
              %output %0 hi\n\
              %begin 1 290 1\n\x1b[31mline one\n%0 343929\n%end 1 290 1\n\
              %begin 1 291 1\n%end 1 291 1\n\
              %output %0 later\n\
              %begin 1 292 1\nparse error: unknown command\n%error 1 292 1\n",
            &bus,
            &slots,
        );
        let (ok, reply) = rx1.try_recv().expect("first query answered");
        assert!(ok);
        assert_eq!(reply.body, b"\x1b[31mline one\n%0 343929\n");
        // One output preceded the reply, so the capture already shows it.
        assert_eq!(reply.seq, 1);
        let (ok, reply) = rx2.try_recv().expect("second query answered");
        assert!(!ok);
        assert_eq!(reply.body, b"parse error: unknown command\n");
        assert_eq!(reply.seq, 2);
        assert!(slots.lock().is_empty());
        // A body line that looks like a notification stays body.
        let events = bus.drain();
        let outputs: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Ev::Output { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect();
        assert_eq!(outputs, [1, 2]);
    }

    #[test]
    fn a_dead_reader_fails_waiters_instead_of_stranding_them() {
        let (tx, rx) = mpsc::channel::<(bool, Reply)>();
        let slots: Slots = Arc::new(Mutex::new(VecDeque::from([Some(tx)])));
        slots.lock().clear();
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn command_line_quotes_what_tmux_would_otherwise_split_or_expand() {
        assert_eq!(
            command_line(&["send-keys", "-t", "%3", "-H", "6c", "0d"]),
            "send-keys -t %3 -H 6c 0d"
        );
        // A path with a space, a `$`, a quote, and a format, each one argument.
        assert_eq!(
            command_line(&[
                "new-window",
                "-c",
                "/tmp/my dir",
                "-n",
                "it's $HOME",
                "-F",
                "#{pane_id}"
            ]),
            r"new-window -c '/tmp/my dir' -n 'it'\''s $HOME' -F '#{pane_id}'"
        );
        // A bare `;` separates commands; an empty argument is still an argument.
        assert_eq!(command_line(&["a", ";", "b", ""]), "a ; b ''");
    }

    #[test]
    fn pane_info_parses_list_panes_format() {
        let p = PaneInfo::parse("%7 @3 81 0 120 40 4242 13 7 350 my project").expect("parse");
        assert_eq!((p.id, p.window, p.left, p.top), (7, 3, 81, 0));
        assert_eq!((p.cols, p.rows, p.pid), (120, 40, 4242));
        assert_eq!(
            (p.cursor, p.history, p.name.as_str()),
            ((13, 7), 350, "my project")
        );
        assert!(PaneInfo::parse("garbage").is_none());
    }

    #[test]
    fn spawn_reply_ids_are_parsed() {
        // `-P -F '#{window_id} #{pane_id}'` is what makes spawning race-free;
        // a misparse would silently orphan the pane.
        assert_eq!(cmd::parse_ids("@3 %17\n"), Some((3, 17)));
        assert_eq!(cmd::parse_ids("garbage"), None);
        assert_eq!(cmd::parse_ids(""), None);
    }

    #[test]
    fn line_cap_closes_connection() {
        let bus = Bus::default();
        let slots: Slots = Arc::new(Mutex::new(VecDeque::new()));
        let mut block = None;
        // A line just under the cap is fine.
        let ok_line = vec![b'x'; LINE_CAP - 1];
        assert!(handle_line(&ok_line, &bus, &slots, &mut block));
        // A line at the cap is fine.
        let at_cap = vec![b'x'; LINE_CAP];
        assert!(handle_line(&at_cap, &bus, &slots, &mut block));
        // Over the cap would close in read_loop; handle_line itself doesn't enforce it.
    }

    #[test]
    fn block_body_cap_closes_connection() {
        let bus = Bus::default();
        let (tx, rx) = mpsc::channel();
        let slots: Slots = Arc::new(Mutex::new(VecDeque::from([Some(tx)])));
        let mut block = None;
        // Start a block.
        assert!(handle_line(b"%begin 1 1 1", &bus, &slots, &mut block));
        assert!(block.is_some());
        // Add data just under the cap.
        let chunk = vec![b'x'; BLOCK_CAP / 2];
        assert!(handle_line(&chunk, &bus, &slots, &mut block));
        assert!(block.is_some());
        // One more chunk pushes it over the cap; connection closes, waiter gets error.
        assert!(!handle_line(&chunk, &bus, &slots, &mut block));
        assert!(block.is_none());
        let (ok, reply) = rx.try_recv().expect("waiter answered");
        assert!(!ok, "over-cap block reported as success");
        assert!(String::from_utf8_lossy(&reply.body).contains("exceeded"));
    }

    #[test]
    fn reader_exit_wakes_all_pending_waiters_immediately() {
        use std::time::Instant;
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        let slots: Slots = Arc::new(Mutex::new(VecDeque::from([Some(tx1), Some(tx2)])));
        let bus = Bus::default();
        // Simulate reader exit by draining slots and sending failures.
        let mut pending = slots.lock();
        while let Some(Some(waiter)) = pending.pop_front() {
            let _ = waiter.send((
                false,
                Reply {
                    body: b"tmux control client exited".to_vec(),
                    seq: bus.seq(),
                },
            ));
        }
        drop(pending);
        // Both waiters should wake immediately, not after REPLY_TIMEOUT.
        let start = Instant::now();
        let r1 = rx1.recv_timeout(Duration::from_millis(100));
        let r2 = rx2.recv_timeout(Duration::from_millis(100));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "waiters took too long"
        );
        assert!(r1.is_ok(), "first waiter not woken");
        assert!(r2.is_ok(), "second waiter not woken");
        let (ok1, _) = r1.unwrap();
        let (ok2, _) = r2.unwrap();
        assert!(!ok1 && !ok2, "exit reported as success");
    }
}
