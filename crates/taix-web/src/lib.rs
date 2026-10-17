//! The LAN front end: the same TaiX, served over HTTP to any device on the
//! network.
//!
//! Deliberately not a second application. tmux is still the source of truth
//! and the GTK process still owns it; this crate is a second *front*, like
//! the TUI, and it reaches the session the same way every other front-end
//! callback does - by parking a request for the next frame. Nothing here
//! touches tmux, and nothing here runs on the UI thread.
//!
//! Transport is one WebSocket per page (`/ws`): what that page has not seen
//! of the picture down whenever it changes, commands up in order on the
//! same connection.
//! `POST /cmd` stays for the moments a socket cannot carry a message - the
//! page being torn down (`sendBeacon`), or the gap before it reconnects.

pub mod api;
mod collab;
pub mod html;
mod http;
mod passkey;
pub mod proto;
mod proxy;
mod push;
mod ws;

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
use std::collections::{BTreeMap, HashMap, VecDeque};
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
        hub.passkeys
            .open_at(taix_core::data_dir().join("passkeys.json"));
        hub.collab
            .open_at(taix_core::data_dir().join("devices.json"));
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
    /// Ports the panes' processes listen on, from the last snapshot: all
    /// the preview proxy will connect to.
    ports: Mutex<Vec<u16>>,
    /// Each window's finished shell commands, as the GUI last handed them
    /// over; `/api/history` reads it without waking the UI thread.
    history: Mutex<Vec<(i64, Vec<taix_term::Command>)>>,
    /// Per page: client id, windows, when it was last said.
    watch: Mutex<Vec<(String, Vec<i64>, Instant)>>,
    clients: AtomicUsize,
    stop: AtomicBool,
    pending: Mutex<Vec<Waiting>>,
    /// Cookie-authenticated devices seen in the last hour: ip, agent, last.
    /// Pruned to the last hour on read; never wakes the UI thread (it is
    /// fed on every request). Capped at 32.
    ledger: Mutex<Vec<(String, String, Instant)>>,
    /// Devices that must prove themselves to the phone's authenticator.
    passkeys: passkey::Passkeys,
    /// Share links, device names, claims and hand-offs.
    collab: collab::Collab,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Ask/answer: connection threads park on their slot, the GUI thread
    /// drains the queue on tick and fills the slot when the page answers.
    ask_seq: AtomicU64,
    asks: Mutex<VecDeque<Ask>>,
    ask_pending: Mutex<HashMap<u64, Slot>>,
    push: Arc<push::Push>,
}

/// The snapshot without its screens, one serialised JSON value per
/// top-level field. Sockets send a section only when it differs from the
/// one that socket last sent.
pub(crate) type Head = BTreeMap<String, String>;

/// `{"name":json,...}` from sections that are already JSON.
pub(crate) fn object<'a>(sections: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::from("{");
    for (i, (name, json)) in sections.enumerate() {
        if i > 0 {
            out.push(',');
        }
        // Field names of `Snapshot`: plain identifiers, nothing to escape.
        out.push('"');
        out.push_str(name);
        out.push_str("\":");
        out.push_str(json);
    }
    out.push('}');
    out
}

/// The picture at one revision. `head` is the snapshot without its screens,
/// split into sections and serialised once here rather than once per page.
#[derive(Default)]
struct Frame {
    /// Bumped by a publish that changed something, and by a page changing
    /// what it watches: either way every socket has to look again.
    rev: u64,
    head: Arc<Head>,
    snap: Option<Arc<Snapshot>>,
}

/// What a page's socket wakes up to.
pub(crate) struct Wake {
    pub rev: u64,
    pub head: Arc<Head>,
    pub snap: Option<Arc<Snapshot>>,
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
            ports: Mutex::new(Vec::new()),
            history: Mutex::new(Vec::new()),
            watch: Mutex::new(Vec::new()),
            clients: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            pending: Mutex::new(Vec::new()),
            ledger: Mutex::new(Vec::new()),
            passkeys: passkey::Passkeys::default(),
            collab: collab::Collab::default(),
            waker: Mutex::new(None),
            ask_seq: AtomicU64::new(0),
            asks: Mutex::new(VecDeque::new()),
            ask_pending: Mutex::new(HashMap::new()),
            push: Arc::new(push::Push::new(taix_core::data_dir())),
        }
    }

    /// Hand the current picture to every connected browser, if it differs
    /// from the last one. The screens are compared as they are and the rest
    /// as its JSON, which has to be built for the pages anyway; an idle
    /// session wakes nobody.
    pub fn publish(&self, mut snap: Snapshot) {
        *self.ports.lock() = snap
            .projects
            .iter()
            .flat_map(|p| &p.windows)
            .flat_map(|w| w.ports.iter().copied())
            .collect();
        snap.lock.holders = self.collab.holders(&snap.lock.remote);
        let panes = std::mem::take(&mut snap.panes);
        let Ok(json) = serde_json::to_string(&snap) else {
            return;
        };
        // Each top-level field keeps its exact text, so a page can be sent
        // just the ones that moved.
        let Ok(fields) =
            serde_json::from_str::<BTreeMap<&str, &serde_json::value::RawValue>>(&json)
        else {
            return;
        };
        let head: Head = fields
            .into_iter()
            .map(|(name, json)| (name.to_string(), json.get().to_string()))
            .collect();
        snap.panes = panes;
        let mut frame = self.frame.lock();
        let moved = *frame.head != head;
        if !moved && frame.snap.as_ref().is_some_and(|s| s.panes == snap.panes) {
            return;
        }
        frame.rev += 1;
        // An unmoved head keeps its `Arc`, which is how a page knows it has
        // nothing to compare.
        if moved {
            frame.head = Arc::new(head);
        }
        frame.snap = Some(Arc::new(snap));
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

    /// Web Push senders and subscriptions.
    pub(crate) fn push(&self) -> &Arc<push::Push> {
        &self.push
    }

    /// A window has stopped to ask for a human: tell the subscribed phones.
    /// The caller is the desktop's own notification edge, which fires once
    /// per transition, so there is nothing to debounce here.
    pub fn alert(&self, window: i64, title: &str, body: &str, waiting: Option<usize>) {
        self.push.alert(window, title, body, waiting);
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

    /// The windows one page said it is showing.
    pub(crate) fn watching(&self, client: &str) -> Vec<i64> {
        self.watch
            .lock()
            .iter()
            .find(|(id, ..)| id == client)
            .map(|(_, windows, _)| windows.clone())
            .unwrap_or_default()
    }

    /// The one door for a browser's command: `origin` is who sent it, and
    /// `collab` decides what, if anything, the desktop gets to see of it.
    /// Everything a page or a link visitor sends comes through here.
    pub(crate) fn command(&self, cmd: Cmd, origin: &collab::Origin) {
        for cmd in self.collab.admit(cmd, origin) {
            self.push_cmd(cmd);
        }
    }

    pub fn push_cmd(&self, cmd: Cmd) {
        // A watch is bookkeeping, not an action: it never wakes the UI
        // thread, which captures what is watched on its own cadence.
        if let Cmd::Watch { client, windows } = cmd {
            let mut watch = self.watch.lock();
            let moved = match watch.iter_mut().find(|(id, ..)| *id == client) {
                Some(slot) => {
                    let moved = slot.1 != windows;
                    *slot = (client, windows, Instant::now());
                    moved
                }
                None => {
                    watch.push((client, windows, Instant::now()));
                    true
                }
            };
            drop(watch);
            // The page's socket sends screens by this list: a pane it just
            // switched to must not wait for something to print.
            if moved {
                self.frame.lock().rev += 1;
                self.bell.notify_all();
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

    /// What the preview proxy may connect to.
    pub(crate) fn ports(&self) -> Vec<u16> {
        self.ports.lock().clone()
    }

    pub fn set_history(&self, history: Vec<(i64, Vec<taix_term::Command>)>) {
        *self.history.lock() = history;
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

    /// Park until the picture is newer than `seen`, the server stops, or
    /// `IDLE_WAKE` passes - then the same `rev` comes back, and the caller
    /// may send a keep-alive.
    pub(crate) fn wait_frame(&self, seen: u64) -> Option<Wake> {
        let mut frame = self.frame.lock();
        if frame.rev == seen && !self.stopping() {
            self.bell.wait_for(&mut frame, IDLE_WAKE);
        }
        if self.stopping() {
            return None;
        }
        Some(Wake {
            rev: frame.rev,
            head: frame.head.clone(),
            snap: frame.snap.clone(),
        })
    }

    /// The whole head as one JSON object, for a page's first paint; `None`
    /// until the GUI has published anything.
    pub(crate) fn head_json(&self) -> Option<String> {
        let frame = self.frame.lock();
        frame.snap.as_ref()?;
        Some(object(
            frame.head.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        ))
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
        self.passkeys.forget_ip(ip);
        found.is_some()
    }

    /// Replace the key; every existing cookie stops working.
    pub fn rotate_key(&self, key: &str) {
        *self.key.lock() = key.to_string();
        self.pending.lock().clear();
        self.ledger.lock().clear();
        self.passkeys.clear();
        self.collab.revoke_all();
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

/// The URL to open in a browser, or to show beside the indicator.
pub fn url(port: u16) -> String {
    format!("http://{}", address(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of `text`'s lines; one that starts with `*` is bold.
    fn screen(text: &str) -> html::Screen {
        use taix_term::{Chunk, Style};
        html::screen(|sink| {
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    sink(Chunk::LineBreak);
                }
                let (line, bold) = match line.strip_prefix('*') {
                    Some(rest) => (rest, true),
                    None => (line, false),
                };
                if !line.is_empty() {
                    sink(Chunk::Text(
                        line,
                        Style {
                            bold,
                            ..Style::default()
                        },
                    ));
                }
            }
        })
    }
    pub(crate) fn pane(id: i64, text: &str) -> proto::PaneView {
        proto::PaneView {
            id,
            cols: 80,
            rows: 24,
            scroll: 0,
            mouse: false,
            alt: false,
            screen: screen(text),
        }
    }
    pub(crate) fn snap(panes: Vec<proto::PaneView>) -> Snapshot {
        Snapshot {
            lock: proto::LockView::default(),
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

    fn next(hub: &Hub, page: &mut ws::Page) -> Option<serde_json::Value> {
        let wake = hub.wait_frame(0).unwrap();
        page.update(
            &wake.head,
            wake.snap.as_deref().unwrap(),
            &hub.watching("c"),
        )
        .map(|text| serde_json::from_str(&text).unwrap())
    }

    fn watch(hub: &Hub, windows: Vec<i64>) {
        hub.push_cmd(Cmd::Watch {
            client: "c".into(),
            windows,
        })
    }

    #[test]
    fn a_page_is_sent_only_the_rows_that_moved_of_the_panes_it_shows() {
        let hub = Hub::detached();
        let mut page = ws::Page::default();
        let next = |page: &mut ws::Page| next(&hub, page);
        let watch = |windows| watch(&hub, windows);

        hub.publish(snap(vec![pane(1, "a\nb"), pane(2, "secret")]));
        watch(vec![1]);
        let first = next(&mut page).unwrap();
        assert_eq!(first["head"]["host"], "test");
        assert_eq!(
            first["panes"][0]["lines"],
            serde_json::json!([["a"], ["b"]])
        );
        assert_eq!(first["panes"][0]["alt"], false);
        assert!(
            first.get("styles").is_none(),
            "plain text needs none: {first}"
        );
        assert!(!first.to_string().contains("secret"), "{first}");
        assert!(next(&mut page).is_none(), "nothing moved, nothing sent");

        hub.publish(snap(vec![pane(1, "a\nB"), pane(2, "secret")]));
        let moved = next(&mut page).unwrap();
        assert!(moved.get("head").is_none(), "{moved}");
        assert_eq!(moved["panes"][0]["edit"], serde_json::json!([[1, ["B"]]]));
        assert_eq!(moved["panes"][0]["n"], 2);
        assert!(moved["panes"][0].get("shift").is_none(), "{moved}");

        // A resize that happens not to change a row still has to arrive.
        let mut wide = pane(1, "a\nB");
        wide.cols = 120;
        hub.publish(snap(vec![wide, pane(2, "secret")]));
        let resized = next(&mut page).unwrap();
        assert_eq!(resized["panes"][0]["cols"], 120);
        assert!(resized["panes"][0].get("edit").is_none(), "{resized}");

        // So does a program entering the alternate screen.
        let mut vim = pane(1, "a\nB");
        vim.cols = 120;
        vim.alt = true;
        hub.publish(snap(vec![vim, pane(2, "secret")]));
        assert_eq!(next(&mut page).unwrap()["panes"][0]["alt"], true);

        // A pane that stops being watched is forgotten, so coming back to it
        // starts from the whole screen rather than from rows the page may
        // have dropped.
        watch(vec![2]);
        assert_eq!(
            next(&mut page).unwrap()["panes"][0]["lines"],
            serde_json::json!([["secret"]])
        );
        watch(vec![1]);
        assert_eq!(
            next(&mut page).unwrap()["panes"][0]["lines"],
            serde_json::json!([["a"], ["B"]])
        );
    }

    #[test]
    fn only_the_head_sections_that_moved_are_sent_and_a_vanished_one_is_null() {
        let hub = Hub::detached();
        let mut page = ws::Page::default();
        watch(&hub, vec![1]);

        hub.publish(snap(vec![pane(1, "a")]));
        let first = next(&hub, &mut page).unwrap();
        let sections = first["head"].as_object().unwrap();
        for name in [
            "lock", "host", "session", "projects", "bar", "palette", "health",
        ] {
            assert!(sections.contains_key(name), "{name} missing: {first}");
        }
        assert!(!sections.contains_key("panes"), "{first}");

        let mut said = snap(vec![pane(1, "a")]);
        said.status = "building".into();
        hub.publish(said);
        let moved = next(&hub, &mut page).unwrap();
        assert_eq!(moved["head"], serde_json::json!({"status": "building"}));
        assert_eq!(moved["panes"], serde_json::json!([]));

        hub.publish(snap(vec![pane(1, "a")]));
        assert_eq!(
            next(&hub, &mut page).unwrap()["head"],
            serde_json::json!({"status": null})
        );
        assert!(next(&hub, &mut page).is_none());

        // The boot page gets the same sections whole, `</` and `<!` aside.
        let boot: serde_json::Value = serde_json::from_str(&hub.head_json().unwrap()).unwrap();
        assert_eq!(boot["host"], "test");
        assert!(boot.get("status").is_none(), "{boot}");
    }

    /// A page applies a pane message by the contract's own rules: drop
    /// `shift` rows from the top, pad or cut to `n` with empty rows, then
    /// overwrite the `edit` ones. Whatever it ends up holding must be the
    /// screen that was published, whichever way the server chose to say it.
    #[test]
    fn scrolled_screens_arrive_as_a_shift_and_edits_that_rebuild_them() {
        use serde_json::{Value, json};
        let hub = Hub::detached();
        let mut page = ws::Page::default();
        watch(&hub, vec![1]);
        let mut styles = HashMap::new();
        let mut held: Vec<Value> = Vec::new();

        // Every third line is bold, by its own number, so a line keeps its
        // look wherever it lands.
        let lines = |from: usize, count: usize| {
            (from..from + count)
                .map(|n| format!("{}line {n} of the build output", ["*", "", ""][n % 3]))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut step = |text: String, want: Option<u64>, edits: usize| {
            hub.publish(snap(vec![pane(1, &text)]));
            let msg = next(&hub, &mut page).unwrap();
            for style in msg["styles"].as_array().into_iter().flatten() {
                styles.insert(
                    style[0].as_u64().unwrap(),
                    style[1].as_str().unwrap().to_string(),
                );
            }
            let pane = &msg["panes"][0];
            if let Some(whole) = pane.get("lines") {
                held = whole.as_array().unwrap().clone();
            } else {
                let shift = pane.get("shift").and_then(Value::as_u64);
                assert_eq!(shift, want, "{pane}");
                held.drain(..(shift.unwrap_or(0) as usize).min(held.len()));
                held.resize(pane["n"].as_u64().unwrap() as usize, json!([]));
                for edit in pane
                    .get("edit")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    held[edit[0].as_u64().unwrap() as usize] = edit[1].clone();
                }
                let sent = pane
                    .get("edit")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                assert_eq!(sent, edits, "{pane}");
            }
            let html = held
                .iter()
                .map(|row| {
                    row.as_array()
                        .unwrap()
                        .iter()
                        .map(|run| match run {
                            Value::String(text) => format!("<span>{text}</span>"),
                            run => format!(
                                "<span{}>{}</span>",
                                styles[&run[0].as_u64().unwrap()],
                                run[1].as_str().unwrap()
                            ),
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(html, screen(&text).to_html());
        };

        step(lines(0, 24), None, 0);
        // Scrolled up three lines: three new rows at the bottom.
        step(lines(3, 24), Some(3), 3);
        // One row changed: a shift would cost more than the one edit.
        step(lines(3, 24) + "!", None, 1);
        // Scrolled and shorter: the page cuts what it has, nothing to edit.
        step(lines(5, 17), Some(2), 0);
        // Scrolled and taller: rows past what the page holds are padding
        // until edited.
        step(lines(10, 30), Some(5), 18);
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
