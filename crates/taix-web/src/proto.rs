//! The wire: one snapshot down, one command up.
//!
//! Both directions are JSON, because the other end is a browser. The
//! snapshot is a whole picture rather than a diff - it is a few KB, it is
//! only sent when it changed, and a client that reconnects is instantly
//! right instead of replaying a log it missed.

use serde::{Deserialize, Serialize};

/// A device with no cookie asking to be let in, as the desktop's bar shows
/// it. In-process only: the answer is a click on this machine, so it never
/// goes over the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRequest {
    pub ip: String,
    pub agent: String,
}

/// A paired device still active in the last hour, as the desktop's device
/// panel shows it. In-process only: the GUI reads this from the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceView {
    pub ip: String,
    pub agent: String,
    pub secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Life {
    /// Disabled in settings.
    Off,
    /// Binding, or rebinding after a port change.
    Starting,
    Live,
    /// Bound nothing: the port is taken, or the address is not ours.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub life: Life,
    pub port: u16,
    /// The address to type into a phone, `ip:port`, or empty when down.
    pub addr: String,
    /// Full pairing URL with key, or empty when down.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub url: String,
    /// Browsers currently holding the event stream.
    pub clients: usize,
    /// Why `Error`, verbatim from the OS.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub error: String,
}

impl Health {
    pub fn off(port: u16) -> Health {
        Health {
            life: Life::Off,
            port,
            addr: String::new(),
            url: String::new(),
            clients: 0,
            error: String::new(),
        }
    }
}

/// Session facts: process uptime, shell, tmux version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SessionView {
    /// Seconds since the GUI process started, rounded to whole minutes.
    pub uptime: u64,
    /// Basename of $SHELL.
    pub shell: String,
    /// tmux server version.
    pub tmux: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowView {
    pub id: i64,
    pub name: String,
    /// Harness id (`terminal`, `claude`, ...), which is also its icon name.
    pub kind: String,
    pub icon: String,
    /// One of the eight tints, or absent for the harness default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<String>,
    /// `starting` | `working` | `waiting` | `done` | `failed` | `idle`.
    pub state: String,
    /// Has a pane. A window without one shows its "click to start" mark.
    pub alive: bool,
    /// Wants looking at: the state the desktop would notify for.
    pub attention: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_kib: Option<u64>,
    /// CPU usage as percentage of one core.
    pub cpu: f32,
    /// Shell line that attaches an ssh terminal to this window alone;
    /// absent while the window has no pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub folded: bool,
    pub windows: Vec<WindowView>,
}

/// One pane's screen, already rendered. The browser gets HTML rather than
/// cells and a palette: the grid is built once here, where the emulator
/// already is, instead of in JavaScript on a phone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneView {
    pub id: i64,
    pub cols: usize,
    pub rows: usize,
    /// Lines above the live bottom; 0 is live.
    pub scroll: usize,
    /// True when a program in the pane asked for the mouse, so a tap must
    /// go to it rather than move a text cursor.
    pub mouse: bool,
    /// Absent when this pane is unchanged since the frame the client last
    /// saw, which is most of them: only the pane being typed into moves,
    /// and its neighbours cost a full screen of HTML each to repeat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessView {
    pub id: String,
    pub label: String,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BarItem {
    pub id: String,
    pub text: String,
}

/// Everything a client draws from. Sent whole, only when it differs from
/// the last one published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Bumped by the hub, not the GUI; clients use it to drop duplicates.
    #[serde(default)]
    pub rev: u64,
    pub lock: LockView,
    pub host: String,
    pub session: SessionView,
    pub projects: Vec<ProjectView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoomed: Option<i64>,
    /// When present, this is a sparse frame: `panes` contains only changed
    /// panes, and this roster lets the client detect removals. When absent,
    /// `panes` is complete and the client replaces its held state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_ids: Option<Vec<i64>>,
    pub panes: Vec<PaneView>,
    pub bar: Vec<BarItem>,
    pub harnesses: Vec<HarnessView>,
    /// The header's transient notice, mirrored so both screens say the same
    /// thing about the same action.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub status: String,
    /// Tints as the installed palette resolves them, so the web page wears
    /// the user's own colours instead of a hardcoded copy.
    pub palette: Vec<BarItem>,
    pub health: Health,
}

/// Which terminals a browser is driving.
///
/// The claim is per window, not per application: the whole point of the
/// web front end is that someone on a laptop types into one pane while the
/// desktop keeps working in another, in another project. A window is
/// claimed by tapping it on the page, and given back by clicking it on the
/// desktop - the same gesture on both sides, and never a mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockView {
    /// Window ids a browser holds. Empty means the desktop has everything.
    pub remote: Vec<i64>,
}

impl PartialEq for Health {
    fn eq(&self, other: &Self) -> bool {
        // `clients` deliberately excluded: a browser connecting must not
        // count as a state change for every other browser, or opening a
        self.life == other.life
            && self.port == other.port
            && self.addr == other.addr
            && self.url == other.url
            && self.error == other.error
    }
}
impl PartialEq for SessionView {
    fn eq(&self, other: &Self) -> bool {
        // uptime rounded to minutes, so the snapshot doesn't change every second
        self.uptime / 60 == other.uptime / 60
            && self.shell == other.shell
            && self.tmux == other.tmux
    }
}

/// What a browser asks for. Terminal input and window lifecycle go to the
/// GUI thread as these; everything read-only (files, git, jobs) is served
/// by the web threads themselves and never appears here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Cmd {
    /// Claim one terminal. Sent by a tap on a pane; the desktop takes it
    /// back by clicking the same pane there.
    Takeover {
        window: i64,
    },
    /// Give one back, or all of them when the page closes.
    Release {
        #[serde(default)]
        window: Option<i64>,
    },
    /// Which windows a browser is showing, so the desktop keeps their
    /// screens fresh even when it is looking at another project. Answered
    /// by the web threads: it never reaches the GUI.
    Watch {
        /// Per page load, so two browsers watching different projects both
        /// get what they asked for.
        client: String,
        windows: Vec<i64>,
    },
    /// Literal text, as typed or pasted.
    Keys {
        window: i64,
        text: String,
    },
    /// A tmux key name: `Enter`, `Tab`, `Up`, `C-c`, `BSpace`, `F5`.
    Key {
        window: i64,
        name: String,
    },
    Mouse {
        window: i64,
        button: u8,
        col: usize,
        row: usize,
        /// `press` | `release` | `motion`.
        motion: String,
    },
    Scroll {
        window: i64,
        delta: i32,
    },
    ScrollBottom {
        window: i64,
    },
    /// The grid this browser is drawing the terminal at.
    ///
    /// While a page drives a window, the window's size is the page's
    /// business: a phone with room for 45 columns cannot read a pane tmux
    /// is wrapping at 137, and the desktop's widget is not on the phone.
    Grid {
        window: i64,
        cols: usize,
        rows: usize,
    },
    NewTerminal {
        project: i64,
    },
    Spawn {
        project: i64,
        harness: String,
    },
    AddProject {
        path: String,
    },
    RemoveProject {
        project: i64,
    },
    RenameProject {
        project: i64,
        name: String,
    },
    CloseAll {
        project: i64,
    },
    Close {
        window: i64,
    },
    Kill {
        window: i64,
    },
    Interrupt {
        window: i64,
    },
    Restart {
        window: i64,
    },
    Start {
        window: i64,
    },
    Rename {
        window: i64,
        name: String,
    },
    Recolor {
        window: i64,
        #[serde(default)]
        color: Option<String>,
    },
    /// Type a path or a pasted blob into one pane without a newline.
    Type {
        window: i64,
        text: String,
    },
    /// A terminal in this directory of the selected project.
    TerminalAt {
        path: String,
    },
    /// Ask the desktop to open a file in its editor. The phone cannot, and
    /// "open on my computer" is the useful meaning anyway.
    OpenFile {
        path: String,
        #[serde(default)]
        with: Option<String>,
    },
    /// Multi-line text via tmux bracketed paste.
    Paste {
        window: i64,
        text: String,
    },
    /// Ask the desktop to raise and focus a window.
    Focus {
        window: i64,
    },
    /// Apply config changes and persist them.
    Config {
        patch: std::collections::HashMap<String, serde_json::Value>,
    },
    /// One line in both status labels.
    Notice {
        text: String,
    },
    Mute {
        project: i64,
    },
    Merge {
        window: i64,
    },
}

/// Apply a settings patch from a browser to a loaded config.
///
/// One list of keys, shared by the endpoint that persists the patch and
/// the GUI that applies it live: two copies drifted into two spellings
/// the moment they existed, and the live half quietly did nothing.
pub fn apply_patch(
    cfg: &mut taix_core::Config,
    patch: &std::collections::HashMap<String, serde_json::Value>,
) {
    let text = |key: &str| patch.get(key).and_then(|v| v.as_str());
    let number = |key: &str| patch.get(key).and_then(|v| v.as_u64());
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());

    if let Some(s) = text("theme") {
        cfg.theme = some(s);
    }
    if let Some(s) = text("editor") {
        cfg.editor = some(s);
    }
    if let Some(f) = patch.get("font_size").and_then(|v| v.as_f64()) {
        cfg.font_size = f.clamp(6.0, 24.0);
    }
    if let Some(b) = patch.get("isolate").and_then(|v| v.as_bool()) {
        cfg.isolate = b;
    }
    if let Some(n) = number("idle_after_ms") {
        cfg.idle_after_ms = n;
    }
    if let Some(n) = number("reap_idle_after_ms") {
        cfg.reap_idle_after_ms = (n > 0).then_some(n);
    }
    if let Some(n) = number("web_port") {
        cfg.web.port = (n as u16).max(1024);
    }
}

impl Cmd {
    /// The window this command types into, and so must have claimed.
    ///
    /// Window lifecycle is not in here: closing a finished agent from a
    /// phone while someone reads another pane on the desktop is not a
    /// collision, and making it one would mean tapping twice for
    /// everything. Neither is scrolling - reading is not typing.
    pub fn drives(&self) -> Option<i64> {
        match self {
            Cmd::Keys { window, .. }
            | Cmd::Key { window, .. }
            | Cmd::Type { window, .. }
            | Cmd::Paste { window, .. }
            | Cmd::Grid { window, .. }
            | Cmd::Mouse { window, .. } => Some(*window),
            Cmd::Merge { window } => Some(*window),
            _ => None,
        }
    }
}
