//! The LAN front end: the same TaiX, served over HTTP to any device on the
//! network.
//!
//! Deliberately not a second application. tmux is still the source of truth
//! and the GTK process still owns it; this crate is a second *front*, like
//! the TUI, and it reaches the session the same way every other front-end
//! callback does - by parking a request for the next frame. Nothing here
//! touches tmux, and nothing here runs on the UI thread.
//!
//! Transport is Server-Sent Events down and `POST /cmd` up, not a
//! WebSocket. A snapshot is a whole JSON picture published when it changes,
//! which is exactly what SSE is: no handshake, no framing, no SHA-1, and a
//! browser that reconnects by itself when a phone comes back from sleep.
//! Upstream messages are keystroke-sized and go over a kept-alive
//! connection, so the round trip on a LAN is a millisecond either way.

pub mod api;
pub mod html;
mod http;
pub mod proto;

/// One question from an out-of-process client, waiting for the GUI thread.
#[derive(Debug, Clone)]
pub struct Ask {
    pub id: u64,
    pub body: serde_json::Value,
}

/// Where an answer lands, and the connection thread parked on it.
type Slot = Arc<(Mutex<Option<serde_json::Value>>, Condvar)>;

/// Questions in flight at once. Past this a client is looping, not working.
const MAX_ASKS: usize = 32;
use std::collections::{HashMap, VecDeque};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use proto::{Cmd, Health, Life, Snapshot};

/// How long a parked event stream waits before looking up again. Only a
/// shutdown needs it: a published snapshot wakes every stream at once.
const IDLE_WAKE: Duration = Duration::from_millis(500);

/// How long a page's "I am showing these windows" claim stands without
/// being repeated. The page re-sends well inside this; a tab that went
/// away stops costing a capture.
const WATCH_TTL: Duration = Duration::from_secs(25);

/// The port the settings dialog starts from, and what the indicator shows
/// when the user has never changed it.
pub const DEFAULT_PORT: u16 = 4040;

/// What the GUI hands the server when it starts one.
pub struct Options {
    pub port: u16,
    pub key: String,
    /// The bundled symbolic SVGs, served as `/icons/<name>.svg` so the web
    /// page wears the same harness icons as the desktop.
    pub icons: &'static [(&'static str, &'static str)],
    pub bind: Option<std::net::Ipv4Addr>,
}

/// A running listener. Dropping it stops the listener and every stream.
pub struct Server {
    hub: Arc<Hub>,
}

impl Server {
    /// Bind and start serving. Never fails outwards: a port already in use
    /// is a health state the indicator can show, not a reason for the GUI
    /// to refuse to start.
    pub fn start(opts: Options) -> Server {
        let hub = Arc::new(Hub::new(opts.port, opts.key, opts.icons, opts.bind));
        let listener = hub.clone();
        std::thread::Builder::new()
            .name("taix-web".into())
            .spawn(move || accept_loop(listener))
            .expect("spawn web listener");
        Server { hub }
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.hub.shutdown();
    }
}

fn accept_loop(hub: Arc<Hub>) {
    let listener = match TcpListener::bind((
        hub.bind.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED),
        hub.port,
    )) {
        Ok(listener) => listener,
        Err(e) => {
            hub.fail(&e.to_string());
            return;
        }
    };
    hub.live();
    for stream in listener.incoming() {
        if hub.stopping() {
            break;
        }
        let Ok(stream) = stream else { continue };
        let hub = hub.clone();
        let _ = std::thread::Builder::new()
            .name("taix-web-conn".into())
            .stack_size(256 * 1024)
            .spawn(move || http::serve(stream, &hub));
    }
    hub.off();
}

/// A device with no cookie that asked for a page, and what the desktop
/// said about it.
struct Waiting {
    ip: String,
    agent: String,
    last: Instant,
    /// `None` while the desktop has not answered. `Some(true)` hands this
    /// address the cookie on its next page load; `Some(false)` keeps it off
    /// the bar rather than letting it ask again every two seconds.
    verdict: Option<bool>,
}

/// How long an unanswered request stands on the bar, and how long a
/// verdict stands after it: long enough to walk to the desktop and back.
const ASK_TTL: Duration = Duration::from_secs(120);
const VERDICT_TTL: Duration = Duration::from_secs(300);
/// Devices remembered at once. A LAN scanner must not be able to push the
/// one device the user is holding off the list.
const MAX_WAITING: usize = 8;

fn prune(pending: &mut Vec<Waiting>, now: Instant) {
    pending.retain(|w| {
        let ttl = if w.verdict.is_some() {
            VERDICT_TTL
        } else {
            ASK_TTL
        };
        now.duration_since(w.last) < ttl
    });
}

/// The published picture, and the queue of what clients asked for.
///
/// The GUI publishes into it on its frame and drains commands from it on
/// the next one; the web threads read the picture and push commands. One
/// lock each way, never held across I/O.
pub struct Hub {
    port: u16,
    key: Mutex<String>,
    icons: &'static [(&'static str, &'static str)],
    bind: Option<std::net::Ipv4Addr>,
    frame: Mutex<Frame>,
    bell: Condvar,
    health: Mutex<Health>,
    cmds: Mutex<VecDeque<Cmd>>,
    roots: Mutex<Vec<PathBuf>>,
    watch: Mutex<Vec<(String, Vec<i64>, Instant)>>,
    clients: AtomicUsize,
    rev: AtomicU64,
    stop: AtomicBool,
    pending: Mutex<Vec<Waiting>>,
    /// Cookie-authenticated devices seen in the last hour: ip, agent, last.
    /// Pruned to the last hour on read; never wakes the UI thread (it is
    /// fed on every request). Capped at 32.
    ledger: Mutex<Vec<(String, String, Instant)>>,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Ask/answer: connection threads park on their slot, the GUI thread
    /// drains the queue on tick and fills the slot when the page answers.
    ask_seq: AtomicU64,
    asks: Mutex<VecDeque<Ask>>,
    ask_pending: Mutex<HashMap<u64, Slot>>,
}

/// The picture at one revision, in two forms: whole, for a browser that
/// has just connected or fallen behind, and thin, for one that saw the
/// revision before this - which is every browser, almost always.
#[derive(Default)]
struct Frame {
    rev: u64,
    json: Arc<str>,
    thin: Option<Arc<str>>,
    /// The HTML each pane was last published with, to know what to drop.
    panes: Vec<(i64, String)>,
}

impl Hub {
    /// A hub with no listener, for the endpoint tests: they drive `route`
    /// directly and must not bind a port to do it.
    #[cfg(test)]
    pub(crate) fn detached() -> Hub {
        Hub::new(0, "test0000000000000000".into(), &[], None)
    }

    fn new(
        port: u16,
        key: String,
        icons: &'static [(&'static str, &'static str)],
        bind: Option<std::net::Ipv4Addr>,
    ) -> Hub {
        Hub {
            port,
            key: Mutex::new(key),
            icons,
            bind,
            frame: Mutex::new(Frame::default()),
            bell: Condvar::new(),
            health: Mutex::new(Health {
                life: Life::Starting,
                port,
                addr: address(port),
                url: String::new(),
                clients: 0,
                error: String::new(),
            }),
            cmds: Mutex::new(VecDeque::new()),
            roots: Mutex::new(Vec::new()),
            watch: Mutex::new(Vec::new()),
            clients: AtomicUsize::new(0),
            rev: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            pending: Mutex::new(Vec::new()),
            ledger: Mutex::new(Vec::new()),
            waker: Mutex::new(None),
            ask_seq: AtomicU64::new(0),
            asks: Mutex::new(VecDeque::new()),
            ask_pending: Mutex::new(HashMap::new()),
        }
    }

    /// Hand the current picture to every connected browser, if it differs
    /// from the last one. The comparison is on the serialised form: it is a
    /// `memcmp` against a string that has to be built anyway, and it is
    /// what stops an idle session sending sixty identical frames a second.
    ///
    /// A pane's HTML is a whole screen, and a session usually has one pane
    /// moving and the rest still, so the frame is published twice: whole,
    /// and again without the screens that did not change.
    pub fn publish(&self, mut snap: Snapshot) {
        let rev = self.rev.load(Ordering::Relaxed) + 1;
        snap.rev = rev;
        snap.pane_ids = None;
        let Ok(json) = serde_json::to_string(&snap) else {
            return;
        };
        let mut frame = self.frame.lock();
        if *frame.json == *json {
            return;
        }
        let mut panes = Vec::with_capacity(snap.panes.len());
        let mut changed = Vec::new();
        for pane in &snap.panes {
            let pane_json = serde_json::to_string(pane).unwrap_or_default();
            let same = frame
                .panes
                .iter()
                .any(|(id, seen)| *id == pane.id && *seen == pane_json);
            if !same {
                changed.push(pane.clone());
            }
            panes.push((pane.id, pane_json));
        }
        let thin = if !changed.is_empty() && changed.len() < snap.panes.len() {
            let pane_ids: Vec<i64> = snap.panes.iter().map(|p| p.id).collect();
            let mut sparse = snap.clone();
            sparse.panes = changed;
            sparse.pane_ids = Some(pane_ids);
            serde_json::to_string(&sparse).ok().map(Arc::from)
        } else {
            None
        };
        self.rev.store(rev, Ordering::Relaxed);
        frame.rev = rev;
        frame.json = Arc::from(json);
        frame.thin = thin;
        frame.panes = panes;
        drop(frame);
        self.bell.notify_all();
    }

    /// Everything clients have asked for since the last frame.
    pub fn take_cmds(&self) -> Vec<Cmd> {
        self.cmds.lock().drain(..).collect()
    }

    pub fn key(&self) -> String {
        self.key.lock().clone()
    }

    /// Constant-time key comparison.
    pub fn check_auth(&self, provided: &str) -> bool {
        let key = self.key.lock();
        if key.len() != provided.len() {
            return false;
        }
        key.bytes()
            .zip(provided.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }

    /// What every connected page is showing, deduplicated.
    ///
    /// A browser watching a project the desktop is not looking at is the
    /// reason this exists: those panes have no widget here, so nothing
    /// would capture them.
    pub fn watched(&self) -> Vec<i64> {
        let mut seen: Vec<i64> = Vec::new();
        let now = Instant::now();
        let mut watch = self.watch.lock();
        watch.retain(|(_, _, at)| now.duration_since(*at) < WATCH_TTL);
        for (_, windows, _) in watch.iter() {
            for id in windows {
                if !seen.contains(id) {
                    seen.push(*id);
                }
            }
        }
        seen
    }

    pub fn push_cmd(&self, cmd: Cmd) {
        // A watch is bookkeeping for the capture loop, not an action: it
        // must not wake the UI thread, and it must not queue up behind a
        // frame.
        if let Cmd::Watch { client, windows } = cmd {
            let mut watch = self.watch.lock();
            match watch.iter_mut().find(|(id, ..)| *id == client) {
                Some(slot) => *slot = (client, windows, Instant::now()),
                None => watch.push((client, windows, Instant::now())),
            }
            return;
        }
        {
            let mut queue = self.cmds.lock();
            // A phone held against a leg can generate scroll events faster
            // than frames; a bounded queue drops the oldest rather than
            // letting a wedged UI thread grow it without limit.
            if queue.len() > 512 {
                queue.pop_front();
            }
            queue.push_back(cmd);
        }
        self.wake();
    }

    /// How the GUI asks for a frame from another thread. Set to the same
    /// main-context hop the tmux bus uses.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock() = Some(waker);
    }

    fn wake(&self) {
        if let Some(wake) = &*self.waker.lock() {
            wake();
        }
    }

    pub fn health(&self) -> Health {
        let mut health = self.health.lock().clone();
        health.clients = self.clients.load(Ordering::Relaxed);
        health
    }

    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        *self.roots.lock() = roots;
    }

    /// Whether a path may be read or written through the web API: inside a
    /// project the user added, and free of `..`.
    pub fn allows(&self, path: &std::path::Path) -> bool {
        let Ok(path) = path.canonicalize() else {
            // A path that does not exist yet is allowed if its parent is:
            // writing `.gitignore` in a project root is a real operation.
            return match path.parent() {
                Some(parent) if parent != path => self.allows(parent),
                _ => false,
            };
        };
        self.roots
            .lock()
            .iter()
            .any(|root| root.canonicalize().is_ok_and(|root| path.starts_with(root)))
    }

    pub fn clients(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }

    pub(crate) fn icons(&self) -> &'static [(&'static str, &'static str)] {
        self.icons
    }

    /// Park until the picture is newer than `seen`, or the server stops.
    /// Returns the frame to send, or `None` when it is time to hang up.
    ///
    /// A client that saw the revision immediately before this one can be
    /// sent the thin frame; one that just connected, or that was writing
    /// while two frames went by, needs the whole picture.
    pub(crate) fn wait_frame(&self, seen: u64) -> Option<(u64, Arc<str>)> {
        let mut frame = self.frame.lock();
        while frame.rev == seen && !self.stopping() {
            if self.bell.wait_for(&mut frame, IDLE_WAKE).timed_out() && frame.rev == seen {
                // Nothing new, and nothing wrong: let the caller send a
                // keep-alive comment so an idle proxy does not hang up.
                return Some((seen, Arc::from("")));
            }
        }
        if self.stopping() {
            return None;
        }
        let body = match &frame.thin {
            Some(thin) if seen + 1 == frame.rev => thin,
            _ => &frame.json,
        };
        Some((frame.rev, body.clone()))
    }

    pub(crate) fn joined(&self) {
        self.clients.fetch_add(1, Ordering::Relaxed);
        self.wake();
    }

    pub(crate) fn left(&self) {
        self.clients.fetch_sub(1, Ordering::Relaxed);
        self.wake();
    }

    fn live(&self) {
        let mut health = self.health.lock();
        health.life = Life::Live;
        let addr = match self.bind {
            Some(ip) => format!("{ip}:{}", self.port),
            None => address(self.port),
        };
        health.addr = addr.clone();
        health.url = format!("http://{addr}/?k={}", self.key());
        health.error.clear();
        drop(health);
        self.wake();
    }

    fn fail(&self, error: &str) {
        let mut health = self.health.lock();
        health.life = Life::Error;
        health.addr.clear();
        health.url.clear();
        health.error = error.to_string();
        drop(health);
        self.wake();
    }

    fn off(&self) {
        let mut health = self.health.lock();
        if health.life != Life::Error {
            health.life = Life::Off;
            health.addr.clear();
            health.url.clear();
        }
        drop(health);
        self.wake();
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// A device with no cookie asked for a page. Recorded for the desktop
    /// to answer; a verdict already given is left exactly as it is, so a
    /// page polling every two seconds cannot re-raise a denial.
    pub(crate) fn pair_request(&self, ip: &str, agent: &str) {
        let now = Instant::now();
        let mut pending = self.pending.lock();
        prune(&mut pending, now);
        if let Some(entry) = pending.iter_mut().find(|w| w.ip == ip) {
            entry.last = now;
            return;
        }
        if pending.len() >= MAX_WAITING
            && let Some(oldest) = pending
                .iter()
                .enumerate()
                .min_by_key(|(_, w)| w.last)
                .map(|(i, _)| i)
        {
            pending.remove(oldest);
        }
        pending.push(Waiting {
            ip: ip.to_string(),
            agent: agent.to_string(),
            last: now,
            verdict: None,
        });
        drop(pending);
        // Only an address nobody has seen before is worth a frame.
        self.wake();
    }

    /// Whether the desktop said yes to this address recently enough that
    /// its next page load should be handed the cookie.
    pub(crate) fn approved(&self, ip: &str) -> bool {
        let mut pending = self.pending.lock();
        prune(&mut pending, Instant::now());
        pending
            .iter()
            .any(|w| w.ip == ip && w.verdict == Some(true))
    }

    /// Who is waiting on an answer, the one the bar has kept waiting
    /// longest last.
    pub fn pending(&self) -> Vec<proto::PairRequest> {
        let now = Instant::now();
        let mut pending = self.pending.lock();
        prune(&mut pending, now);
        let mut waiting: Vec<&Waiting> = pending.iter().filter(|w| w.verdict.is_none()).collect();
        waiting.sort_by_key(|w| std::cmp::Reverse(w.last));
        waiting
            .into_iter()
            .map(|w| proto::PairRequest {
                ip: w.ip.clone(),
                agent: w.agent.clone(),
            })
            .collect()
    }

    pub fn approve(&self, ip: &str) -> bool {
        self.verdict(ip, true)
    }

    pub fn deny(&self, ip: &str) -> bool {
        self.verdict(ip, false)
    }

    fn verdict(&self, ip: &str, allow: bool) -> bool {
        let mut pending = self.pending.lock();
        let Some(entry) = pending.iter_mut().find(|w| w.ip == ip) else {
            return false;
        };
        entry.verdict = Some(allow);
        entry.last = Instant::now();
        drop(pending);
        self.wake();
        true
    }

    /// Cookie-authenticated devices seen in the last hour, newest first.
    pub fn paired(&self) -> Vec<proto::DeviceView> {
        const LEDGER_TTL: Duration = Duration::from_secs(3600);
        let now = Instant::now();
        let mut ledger = self.ledger.lock();
        ledger.retain(|(_, _, last)| now.duration_since(*last) < LEDGER_TTL);
        ledger.sort_by_key(|(_, _, last)| std::cmp::Reverse(*last));
        ledger
            .iter()
            .map(|(ip, agent, last)| proto::DeviceView {
                ip: ip.clone(),
                agent: agent.clone(),
                secs: now.duration_since(*last).as_secs(),
            })
            .collect()
    }

    /// Drop from the ledger and deny future asks.
    pub fn forget(&self, ip: &str) -> bool {
        let mut ledger = self.ledger.lock();
        let found = ledger.iter().position(|(addr, ..)| addr == ip);
        if let Some(i) = found {
            ledger.remove(i);
        }
        drop(ledger);
        self.deny(ip);
        found.is_some()
    }

    /// Replace the key; every existing cookie stops working.
    pub fn rotate_key(&self, key: &str) {
        *self.key.lock() = key.to_string();
        self.pending.lock().clear();
        self.ledger.lock().clear();
        let mut health = self.health.lock();
        let addr = match self.bind {
            Some(ip) => format!("{ip}:{}", self.port),
            None => address(self.port),
        };
        health.url = format!("http://{addr}/?k={key}");
        drop(health);
        self.wake();
    }

    /// Record a cookie-authenticated request from this device. Does not wake.
    pub(crate) fn record_device(&self, ip: &str, agent: &str) {
        const MAX_LEDGER: usize = 32;
        let now = Instant::now();
        let mut ledger = self.ledger.lock();
        if let Some(entry) = ledger.iter_mut().find(|(addr, ..)| addr == ip) {
            entry.2 = now;
            return;
        }
        if ledger.len() >= MAX_LEDGER
            && let Some(oldest) = ledger
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, last))| last)
                .map(|(i, _)| i)
        {
            ledger.remove(oldest);
        }
        ledger.push((ip.to_string(), agent.to_string(), now));
    }

    /// What the waiting page is told: a verdict if the desktop gave one,
    /// else whether this address is already holding a cookie.
    pub(crate) fn pair_state(&self, ip: &str) -> &'static str {
        let verdict = self
            .pending
            .lock()
            .iter()
            .find(|w| w.ip == ip)
            .and_then(|w| w.verdict);
        match verdict {
            Some(false) => "denied",
            Some(true) => "paired",
            None if self.ledger.lock().iter().any(|(addr, ..)| addr == ip) => "paired",
            None => "waiting",
        }
    }

    /// Queue a question for the desktop and park until it answers.
    ///
    /// Called on an HTTP thread. The answer arrives whole frames later - a
    /// page decides when it has loaded - so this is the one place in the
    /// crate that blocks a connection thread on the UI thread.
    pub fn ask(
        &self,
        body: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let id = self.ask_seq.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new((Mutex::new(None), Condvar::new()));
        {
            let mut asks = self.asks.lock();
            // A runaway agent must not be able to grow this without bound;
            // refusing is information, a queue thirty deep is not.
            if asks.len() >= MAX_ASKS {
                return Err("too many browser requests are already in flight".into());
            }
            asks.push_back(Ask { id, body });
            self.ask_pending.lock().insert(id, slot.clone());
        }
        self.wake();

        let (lock, bell) = &*slot;
        let mut answer = lock.lock();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(answer) = answer.take() {
                self.ask_pending.lock().remove(&id);
                return Ok(answer);
            }
            if Instant::now() >= deadline {
                self.ask_pending.lock().remove(&id);
                self.asks.lock().retain(|a| a.id != id);
                return Err(format!(
                    "the desktop did not answer in {}s",
                    timeout.as_secs()
                ));
            }
            bell.wait_until(&mut answer, deadline);
        }
    }

    /// Called on the GUI thread: everything asked since the last frame.
    pub fn take_asks(&self) -> Vec<Ask> {
        self.asks.lock().drain(..).collect()
    }

    /// Called on the GUI thread, possibly long after `take_asks`: hand one
    /// answer back to the connection waiting for it.
    ///
    /// The slot is cloned out before it is filled. Holding both locks here
    /// would take them in the opposite order to `ask`, which is a deadlock
    /// waiting for a slow page.
    pub fn answer(&self, id: u64, result: serde_json::Value) {
        let slot = self.ask_pending.lock().get(&id).cloned();
        if let Some(slot) = slot {
            let (lock, bell) = &*slot;
            *lock.lock() = Some(result);
            bell.notify_one();
        }
    }

    /// Whether the GUI has anything to answer, without taking the queue.
    pub fn asks_waiting(&self) -> bool {
        !self.asks.lock().is_empty()
    }

    /// Stop accepting, hang up every stream, and unblock the listener by
    /// knocking on it: `accept` has no timeout, and a dropped `TcpListener`
    /// in another thread does not wake it.
    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.bell.notify_all();
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// This machine's LAN address, as the phone has to type it.
///
/// Asked of the routing table rather than of a name lookup: a UDP socket
/// "connected" to an off-link address sends nothing, and its local address
/// is the interface the kernel would route out of - which is the one the
/// phone is on. `hostname` resolution answers 127.0.0.1 on most distros and
/// enumerating interfaces needs `getifaddrs` and a dependency.
pub fn local_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    // RFC 5737 documentation space: nothing on any LAN answers to it, so the
    // kernel always picks the default route - the interface the phone is on.
    // A guessed gateway like 192.168.1.1 picks the wrong one on a LAN that
    // happens to own that prefix elsewhere.
    socket.connect(("203.0.113.1", 9)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn address(port: u16) -> String {
    match local_ip() {
        Some(ip) => format!("{ip}:{port}"),
        None => format!("localhost:{port}"),
    }
}

/// The pairing URL with the key, shown to the user to open.
pub fn pair_url(port: u16, key: &str) -> String {
    format!("http://{}/?k={}", address(port), key)
}
/// The URL to open in a browser, or to show beside the indicator.
pub fn url(port: u16) -> String {
    format!("http://{}", address(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: i64, html: &str) -> proto::PaneView {
        proto::PaneView {
            id,
            cols: 80,
            rows: 24,
            scroll: 0,
            mouse: false,
            html: Some(html.to_string()),
        }
    }
    fn snap(panes: Vec<proto::PaneView>) -> Snapshot {
        Snapshot {
            rev: 0,
            lock: proto::LockView { remote: Vec::new() },
            host: "test".into(),
            session: proto::SessionView {
                uptime: 0,
                shell: String::new(),
                tmux: String::new(),
            },
            projects: Vec::new(),
            selected: None,
            focus: None,
            zoomed: None,
            pane_ids: None,
            panes,
            bar: Vec::new(),
            harnesses: Vec::new(),
            status: String::new(),
            palette: Vec::new(),
            health: proto::Health {
                life: proto::Life::Live,
                port: 0,
                addr: String::new(),
                url: String::new(),
                clients: 0,
                error: String::new(),
            },
        }
    }

    #[test]
    fn a_pane_that_did_not_change_is_left_out_of_the_next_frame() {
        let hub = Hub::detached();
        hub.publish(snap(vec![pane(1, "one"), pane(2, "two")]));
        let (first, whole) = hub.wait_frame(0).unwrap();
        assert!(whole.contains("one") && whole.contains("two"), "{whole}");

        hub.publish(snap(vec![pane(1, "one"), pane(2, "moved")]));
        let (_, thin) = hub.wait_frame(first).unwrap();
        assert!(thin.contains("moved"), "{thin}");
        assert!(!thin.contains("one"), "unchanged pane resent: {thin}");

        // A browser that has just connected has seen nothing, so it cannot
        // fill the gap from a previous frame and must get every screen.
        let (_, fresh) = hub.wait_frame(0).unwrap();
        assert!(fresh.contains("one") && fresh.contains("moved"), "{fresh}");
    }

    #[test]
    fn sparse_frame_merge_reproduces_full_snapshot() {
        let hub = Hub::detached();
        // Publish initial full snapshot with 3 panes
        let full1 = snap(vec![pane(1, "one"), pane(2, "two"), pane(3, "three")]);
        hub.publish(full1.clone());
        let (_rev1, frame1) = hub.wait_frame(0).unwrap();

        // Update only pane 2, publish sparse frame
        let updated = snap(vec![
            pane(1, "one"),
            pane(2, "two-updated"),
            pane(3, "three"),
        ]);
        hub.publish(updated.clone());
        let (_rev2, frame2) = hub.wait_frame(_rev1).unwrap();

        // Parse both frames
        let full_snap: Snapshot = serde_json::from_str(&frame1).unwrap();
        let sparse_snap: Snapshot = serde_json::from_str(&frame2).unwrap();

        // Verify sparse frame has pane_ids and only changed pane
        assert!(
            sparse_snap.pane_ids.is_some(),
            "sparse frame should have pane_ids"
        );
        assert_eq!(
            sparse_snap.panes.len(),
            1,
            "sparse frame should only have changed pane"
        );
        assert_eq!(sparse_snap.panes[0].id, 2);

        // Apply the merge logic (mirrors JS: merge changed panes into held state)
        let held: std::collections::HashMap<i64, proto::PaneView> =
            full_snap.panes.iter().map(|p| (p.id, p.clone())).collect();
        let incoming: std::collections::HashMap<i64, proto::PaneView> = sparse_snap
            .panes
            .iter()
            .map(|p| (p.id, p.clone()))
            .collect();
        let merged: Vec<proto::PaneView> = sparse_snap
            .pane_ids
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|id| incoming.get(id).or_else(|| held.get(id)).cloned())
            .collect();

        // Verify merge reproduces the full updated snapshot
        assert_eq!(merged.len(), 3, "merged should have all 3 panes");
        assert_eq!(merged[0].html.as_ref().unwrap(), "one");
        assert_eq!(merged[1].html.as_ref().unwrap(), "two-updated");
        assert_eq!(merged[2].html.as_ref().unwrap(), "three");
    }

    #[test]
    fn pane_with_changed_non_html_field_appears_in_sparse_frame() {
        let hub = Hub::detached();
        // Publish initial snapshot with 2 panes
        let snap1 = snap(vec![pane(1, "output"), pane(2, "other")]);
        hub.publish(snap1);
        let (rev1, _frame1) = hub.wait_frame(0).unwrap();

        // Update pane 1 with same html but different cols (simulating resize)
        // Keep pane 2 unchanged
        let mut p1 = pane(1, "output");
        p1.cols = 120; // changed from 80
        let snap2 = snap(vec![p1, pane(2, "other")]);
        hub.publish(snap2);
        let (_rev2, frame2) = hub.wait_frame(rev1).unwrap();

        // Parse sparse frame
        let sparse: Snapshot = serde_json::from_str(&frame2).unwrap();

        // Even though html is unchanged, pane 1 should be in the sparse frame
        // because cols changed, but pane 2 should not be included
        assert!(sparse.pane_ids.is_some(), "should be a sparse frame");
        assert_eq!(
            sparse.panes.len(),
            1,
            "only changed pane should be in sparse frame"
        );
        assert_eq!(sparse.panes[0].id, 1, "changed pane should be pane 1");
        assert_eq!(sparse.panes[0].cols, 120, "updated cols should be present");
    }

    #[test]
    fn ask_that_is_answered_returns_the_answer() {
        let hub = Arc::new(Hub::detached());
        let hub2 = hub.clone();

        let handle = std::thread::spawn(move || {
            hub2.ask(
                serde_json::json!({"action": "test"}),
                Duration::from_secs(5),
            )
        });

        std::thread::sleep(Duration::from_millis(50));
        let asks = hub.take_asks();
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].body["action"], "test");

        hub.answer(asks[0].id, serde_json::json!({"result": "ok"}));

        let result = handle.join().unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["result"], "ok");
    }

    #[test]
    fn ask_that_times_out_returns_error_and_cleans_up() {
        let hub = Arc::new(Hub::detached());
        let hub2 = hub.clone();

        let handle = std::thread::spawn(move || {
            hub2.ask(
                serde_json::json!({"action": "timeout"}),
                Duration::from_millis(100),
            )
        });

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(hub.take_asks().len(), 1);

        // Nothing answers it: the caller gets the timeout, not a hang.
        let result = handle.join().unwrap();
        let message = result.unwrap_err();
        assert!(message.contains("did not answer"), "{message}");
        assert_eq!(
            hub.ask_pending.lock().len(),
            0,
            "a timed-out ask must not leave its slot behind"
        );
    }

    #[test]
    fn ask_refuses_when_queue_is_full() {
        let hub = Hub::detached();

        // Fill the queue to capacity (32)
        for i in 0..32 {
            hub.asks.lock().push_back(Ask {
                id: i,
                body: serde_json::json!({"n": i}),
            });
        }

        // Next ask should be refused
        let result = hub.ask(
            serde_json::json!({"overflow": true}),
            Duration::from_secs(1),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too many"));
    }
}
