//! Application state and every state transition.
//!
//! Single-writer discipline: the tmux reader thread only pushes onto
//! `taix_tmux::Bus`; everything here runs on the GTK main thread inside one
//! `RefCell`. No `Arc<Mutex<..>>` in the render path.
//!
//! Memory shape, which is the reason the architecture looks like this: exactly
//! one agent owns a live emulator (the focused one, ~95 KiB). Every other pane
//! is redrawn from `capture-pane` at a fixed interval and keeps only its
//! markup string. tmux owns all scrollback.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};

use taix_core::{Agent, AgentId, AgentState, Config, Project, ProjectId, Store, Watcher};
use taix_term::{Live, Size};
use taix_tmux::{Client, Ev, PaneId, PaneInfo, cmd};
use taix_web::proto::Cmd as WebCmd;

use crate::keys;
use crate::pango;
use crate::presets::Preset;
use crate::tree::{Node, Side};
use crate::ui::{self, AgentCard, Widgets};
use crate::web::Web;
mod browse;
mod git;
mod memory;
mod redraw;
mod web;

/// Frame budget: many `%output` notifications coalesce into one redraw.
pub const TICK: Duration = Duration::from_millis(16);
/// How often web snapshots are published to browsers. 20 Hz to a phone must
/// not visibly regress keystroke echo, but the desktop should not wake at
/// 60 Hz merely because a browser is watching.
pub const WEB_INTERVAL: Duration = Duration::from_millis(50);
/// A harness list costs a `PATH` lookup each and a tint costs a CSS
/// lookup; neither changes between two frames.
pub const SLOW_INTERVAL: Duration = Duration::from_secs(5);
/// Unfocused panes cost one `capture-pane` fork per refresh, so they are
/// deliberately slower than the focused pane.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(400);
/// How often derived agent states are written back to the store while
/// output is arriving: a "needs you" transition should not wait longer
/// than this. With every pane silent, states can only drift to Idle, and
/// the pass runs on the memory cadence instead.
const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);
/// The tmux session is created at this size; panes are re-tiled per window.
const SESSION_COLS: usize = 240;
const SESSION_ROWS: usize = 60;
/// How long a widget size must hold before tmux is told about it. Long enough
/// that a divider drag issues one resize, short enough to feel immediate.
const RESIZE_SETTLE: Duration = Duration::from_millis(180);
/// Zoom bounds, in points.
const FONT_MIN: f64 = 6.0;
const FONT_MAX: f64 = 24.0;
/// How often memory is measured. A PSS reading walks `/proc` for every pane
/// process tree, so it is deliberately far slower than the frame rate.
const MEM_INTERVAL: Duration = Duration::from_millis(2000);
/// How often every pane's scrollback is copied out for a later relaunch.
/// A full `capture-pane -S -` per pane, so seconds, not frames; the window
/// close does one more.
const HISTORY_INTERVAL: Duration = Duration::from_secs(30);
/// How often a worktree window's branch and dirty count are re-read. One
/// `git` invocation per worktree, so it shares the slow cadence rather than
/// the frame.
const GIT_INTERVAL: Duration = Duration::from_millis(4000);
/// How much of a diff is worth putting in a dialog. Past this you want a
/// pager, not a window.
const DIFF_CAP: usize = 200_000;
/// Lines a wheel notch moves the scrollback view.
const SCROLL_STEP: i32 = 3;

/// Park a pasted or dropped image where a harness can read it, and return
/// the path to type. The bytes go where an upload from a phone goes - see
/// `taix_core::stash` - so both fronts hand an agent the same kind of
/// path; the timestamp is the name, because a clipboard has none.
fn stash_image(png: &[u8]) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    taix_core::stash::stash(png, &format!("paste-{stamp}.png"))
}

/// The `.pane` padding from `theme::LAYOUT`, which sits inside the measured
/// viewport: the text starts this far in and cannot use the same margin on
/// the far side, so both are taken off the space tmux is offered.
///
/// Pointer mapping does NOT use these: GTK hands a widget's input
/// coordinates relative to its content box, so `ui::text_origin` is already
/// past the padding.
const PANE_PAD_X: f64 = 14.0;
const PANE_PAD_Y: f64 = 12.0;

pub type Shared = Rc<RefCell<App>>;

/// Pane search state.
///
/// The needle is matched against a pane's whole tmux history, so a hit's
/// `line` indexes that history oldest-first - not the visible screen, which
/// is why jumping to one is a scrollback move rather than a cursor move.
#[derive(Default)]
struct Find {
    target: Option<AgentId>,
    needle: String,
    hits: Vec<crate::find::Hit>,
    current: usize,
}

/// Pending git worker result, applied on the next tick.
type GitResult = (Option<String>, Vec<(AgentId, Option<String>)>);
thread_local! {
    static PENDING_GIT_RESULT: RefCell<Option<GitResult>> = const { RefCell::new(None) };
}

/// Hand the top of the heap back to the OS.
///
/// glibc keeps freed pages in its arena, so dropping an emulator shows up in
/// `free()` and not in RSS. Called once when the window goes out of sight,
/// never on a frame: trimming walks the arena and is not free.
#[cfg(target_env = "gnu")]
fn trim_heap() {
    // SAFETY: `malloc_trim` takes a byte count and touches only glibc's own
    // arena bookkeeping.
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(not(target_env = "gnu"))]
fn trim_heap() {}

/// This machine's name, for the web page's header: it is the one thing a
/// phone cannot infer, and it is fixed for the life of the process.
fn hostname() -> &'static str {
    static NAME: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|name| name.trim().to_string())
            .unwrap_or_else(|_| "this computer".to_string())
    });
    &NAME
}

/// `/home/you/code` as `~/code`, for a bar that has to fit on a phone.
fn tilde(path: &Path) -> String {
    crate::kit::short_path(&path.to_string_lossy())
}

fn mib(kib: u64) -> String {
    let mb = kib as f64 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

struct Row {
    agent: Agent,
    watcher: Watcher,
    live: Option<Live>,
    card: AgentCard,
    dirty: bool,
    last_snapshot: Instant,
    /// Lines this pane is scrolled above the live bottom; 0 is live. While it
    /// is non-zero the pane is drawn from a history capture instead of the
    /// emulator, because tmux - not TaiX - holds the scrollback.
    scroll: usize,
    /// Branch and dirty count for a worktree window, as last read.
    git: Option<String>,
    /// PSS of this window's own process tree, as last measured.
    mem_kib: Option<u64>,
    /// CPU of that tree as a percentage of one core, over the interval
    /// between the last two measurements, and the cumulative tick count it
    /// was derived from.
    cpu: f64,
    ticks: u64,
    /// Transcript log, opened on this window's first output.
    log: Option<taix_core::log::Log>,
    /// Last time this window produced output, for the idle reaper.
    active: Instant,
    /// Suppresses repeat notifications while an agent sits in the same
    /// attention-worthy state.
    notified_state: Option<AgentState>,
    /// Grid size last pushed to tmux, and when the widget last changed size.
    /// Resizing tmux is a fork plus a reflow, so it is debounced rather than
    /// done on every frame of a divider drag.
    applied_size: Option<Size>,
    resize_pending_since: Option<Instant>,
    /// Output events stamped at or below this were already on the screen
    /// the emulator was seeded from, so feeding them would print them twice.
    seed_seq: u64,
    /// This pane's screen as HTML, for the web front end, and when it was
    /// last built from a capture. Only maintained while a browser is
    /// watching; the desktop draws from the same chunks into Pango instead.
    web_html: String,
    web_snap: Instant,
    /// The grid `web_html` was rendered at. Not `applied_size`: a pane a
    /// browser is watching in a project this screen is not showing has no
    /// widget, so no layout size, but tmux still has a grid for it.
    web_size: Size,
    /// The grid a browser driving this window asked for, if one does. It
    /// outranks the widget: whoever is typing decides how wide the terminal
    /// is, and on a phone that is the only way the text is legible.
    web_grid: Option<Size>,
    /// Last rendered markup, held to skip redundant set_markup calls.
    last_markup: String,
}

pub struct App {
    pub cfg: Config,
    pub store: Store,
    tmux: Client,
    pub projects: Vec<Project>,
    rows: Vec<Row>,
    pub selected: Option<ProjectId>,
    focus: Option<AgentId>,
    font_pt: f64,
    saved_layout: crate::layout::Layout,
    /// Each project's pane arrangement. Absent until the project is shown.
    trees: HashMap<ProjectId, Node>,
    /// What is on screen: the shape it was rendered from and its root, so a
    /// rebuild can read the dividers back and skip re-rendering the same shape.
    laid_out: Option<(String, gtk::Widget)>,
    /// One live web view per browser window, kept alive while its project
    /// is not on screen: a page an agent is driving must not be thrown away
    /// because somebody looked at another project.
    #[cfg(feature = "browser")]
    browsers: HashMap<AgentId, crate::browser::Browser>,
    /// Whether the file tree is attached. The panel itself always exists;
    /// this is what says its rows should.
    files_open: bool,
    w: Widgets,
    app: adw::Application,
    markup: String,
    last_reconcile: Instant,
    /// Pointer requests, applied on the next frame.
    ///
    /// A click handler cannot touch `App`: it fires while the caller already
    /// holds the `RefCell` borrow. Parking the intent in a shared cell and
    /// draining it in `tick` keeps every mutation on one path.
    pending: Pending,
    /// When a pane was last repainted; output-driven wake-ups are capped by
    /// it, so a stream paints at the frame rate and a lone echo paints now.
    last_paint: Instant,
    /// Set by `key` when `TAIX_TRACE` is on; cleared by the paint that follows.
    trace_key: Option<Instant>,
    /// Set when a click changed what the sidebar should show.
    sidebar_dirty: bool,
    /// True while the compositor says nobody can see this window. Drawing is
    /// the only thing that stops; output, states and notifications do not.
    asleep: bool,
    /// Projects whose window list is folded away. Persisted with the layout.
    folded: Vec<ProjectId>,
    /// Live sidebar cards, kept so they can be updated instead of rebuilt.
    sidebar: Vec<ui::SidebarCard>,
    /// The project/window shape the current widgets were built from.
    sidebar_sig: SidebarShape,
    /// The store file as `reload` last saw it; see `store_changed_externally`.
    store_stamp: Option<(std::time::SystemTime, u64)>,
    /// Last memory reading: (ui, tmux server, the selected project's panes,
    /// every pane) in KiB PSS. The bar's total is the whole session, not
    /// the project you happen to be looking at.
    mem: (u64, u64, u64, u64),
    /// Panes the tmux server is holding, read on the memory cadence.
    panes: usize,
    /// The server's pid and version: fixed for its life, asked once.
    server: Option<(u32, String)>,
    /// When the readings behind `mem` and each row's `cpu` were taken. CPU
    /// is a difference over an interval, so the interval has to be measured
    /// rather than assumed to be `MEM_INTERVAL`.
    measured_at: Instant,
    /// The selected project's branch line, with its dirty and ahead/behind
    /// counts. Read on the git cadence, not on every bar refresh: it used to
    /// fork `git` twice a second for a string that changes hourly.
    bar_git: Option<String>,
    last_mem: Instant,
    /// When every pane's scrollback was last copied out to the cache.
    last_history: Instant,
    /// The pane focused before the current one.
    ///
    /// Opening a window's menu focuses that window - the card's click gesture
    /// runs in the capture phase - so "compare with the other pane" has to
    /// mean the one you were in a moment ago, or it can only ever compare a
    /// window with itself.
    previous: Option<AgentId>,
    /// Pane arrangement, persisted with the layout.
    preset: Preset,
    /// One pane, filling the window: the escape hatch from a crowded grid.
    zoomed: Option<AgentId>,
    /// Projects whose agents must not raise a desktop notification.
    muted: Vec<ProjectId>,
    /// IPs we've raised pair notifications for; cleared when they leave pending.
    notified_pairs: Vec<String>,
    find: Find,
    /// Projects whose `.taix.toml` startup windows have already been opened,
    /// so showing a project twice in one run does not open them twice.
    started: Vec<ProjectId>,
    /// Fires due jobs while this window is open; the runner is always a
    /// child process, never work on the UI thread.
    jobs: taix_core::jobs::Ticker,
    /// When this window launched, for the `uptime` bar segment.
    launched: Instant,
    /// When the repositories on show were last read.
    last_git: Instant,
    /// When the web front end was last published, rate-limited to
    /// `WEB_INTERVAL` so an attached browser cannot pin the UI thread to
    /// the frame rate.
    last_web_publish: Instant,
    /// What only the web page asks for and nothing changes between frames.
    web_slow: WebSlow,
    /// The LAN front end: the server's life and the input lock both sides
    /// respect.
    pub web: Web,
    /// Whether a git worker thread is in flight. Only one at a time per repo.
    git_worker_busy: std::cell::Cell<bool>,
}

/// A project's name, or a placeholder if it vanished between menu and click.
fn current_name(project: Option<&Project>) -> String {
    project.map(|p| p.name.clone()).unwrap_or_default()
}

/// Facts the browser needs that cost real work to answer and change at
/// human speed: every harness is a `PATH` lookup, every tint is a CSS
/// colour lookup, and neither moves between two frames.
#[derive(Default)]
struct WebSlow {
    at: Option<Instant>,
    harnesses: Vec<taix_web::proto::HarnessView>,
    palette: Vec<taix_web::proto::BarItem>,
}

/// The colour a window wears: its own, else its harness's.
fn tint(cfg: &Config, agent: &Agent) -> Option<String> {
    agent
        .color
        .clone()
        .or_else(|| taix_core::by_id(cfg, &agent.kind).color)
}

/// The project/window shape the sidebar widgets were built from: per project,
/// its id and name, then its windows' ids and names. Compared to decide
/// whether widgets must be rebuilt or merely repainted.
type SidebarShape = Vec<(ProjectId, String, Vec<(AgentId, String)>)>;

/// Requests parked by GTK callbacks for the next tick - which each push
/// asks for, so a click is served on the next main-loop turn rather than
/// on a timer that would otherwise have to run all the time.
#[derive(Clone, Default)]
struct Pending(Rc<RefCell<Vec<Pointer>>>);

impl Pending {
    fn push(&self, request: Pointer) {
        self.0.borrow_mut().push(request);
        wake();
    }
    fn take(&self) -> Vec<Pointer> {
        std::mem::take(&mut self.0.borrow_mut())
    }
    fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
}

/// Which panel the side column is showing. The column holds one at a time:
/// two 280px panels side by side leave neither wide enough to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Files,
    Git,
}

/// Something the pointer asked for, in a pane, a menu or the sidebar.
///
/// Every one of these is parked and applied in `tick`: a menu action or a
/// dialog response fires while the caller may already hold the `RefCell`
/// borrow, so mutating `App` there would panic.
enum Pointer {
    /// Clicked a pane or a sidebar row: make this the live pane.
    Focus(AgentId),
    /// Dropped a file on a pane: focus it and type the path.
    Drop(AgentId, PathBuf),
    /// Close one window. Kills its process, keeps any worktree.
    Close(AgentId),
    /// Close it and discard its git worktree. Confirmed first.
    Discard(AgentId),
    /// Kill and re-launch the same harness in the same project.
    Restart(AgentId),
    /// Clicked the "click to start" mark of a window with no pane: open a
    /// pane for it again, old scrollback in front.
    Start(AgentId),
    /// A header was dropped on another pane: split it on that side, swap
    /// the two when dropped in the middle, and tab them when dropped on
    /// the other pane's header.
    Dock(AgentId, AgentId, Side),
    /// A browser window's page moved, so the window remembers where it was.
    #[cfg(feature = "browser")]
    BrowserUrl(AgentId, String),
    /// Send Ctrl-C to it.
    Interrupt(AgentId),
    /// Kill its process and keep the window, the way `Ctrl-D` at a prompt
    /// does: a harness becomes a "click to start" card, a terminal closes.
    Kill(AgentId),
    Rename(AgentId, String),
    Recolor(AgentId, Option<String>),
    CopyPane(AgentId),
    PastePane(AgentId),
    /// Middle-click: paste the primary selection.
    PastePrimary(AgentId),
    /// Select the whole visible pane, so it can be copied in one go.
    SelectAll(AgentId),
    /// Type a literal string into the focused pane: the tail of a paste.
    Type(String),
    /// A file from the tree: open it in the editor with this id, or the
    /// default one.
    OpenFile(PathBuf, Option<String>),
    /// A terminal window in this directory of the selected project.
    TerminalAt(PathBuf),
    /// One line for the status label.
    Notice(String),
    /// A diff the source-control panel already read: a title and the text.
    ShowDiff(String, String),
    /// Move one pane's view through tmux's history.
    Scroll(AgentId, i32),
    /// A pointer event a program in the pane asked to be told about: button,
    /// cell, and what happened to it.
    Mouse(AgentId, u8, usize, usize, taix_term::Kind),
    /// Jump one pane's view back to the live bottom.
    ScrollBottom(AgentId),
    /// Diff this window's output against the live pane's.
    Compare(AgentId),
    /// Show this worktree's diff against its base branch.
    Diff(AgentId),
    /// Merge this worktree's branch back into its base.
    /// Desktop pairing decision: allow or deny a device requesting access.
    Pair {
        ip: String,
        allow: bool,
    },
    Merge(AgentId),
    /// Copy the path of this window's transcript log.
    Transcript(AgentId),
    /// Header-menu intents. Parked like the rest: a menu callback fires while
    /// the caller may still hold the `RefCell` borrow.
    OpenFind,
    Zoom,
    Preset(String),
    SaveSession(String),
    RestoreSession(String),
    Mute,
    /// Show this project's windows.
    Select(ProjectId),
    /// Open a plain terminal in this project.
    NewTerminal(ProjectId),
    /// Open a window running the harness with this id.
    Spawn(ProjectId, String),
    RenameProject(ProjectId, String),
    CopyPath(ProjectId),
    OpenFolder(ProjectId),
    CloseAll(ProjectId),
    RemoveProject(ProjectId),
    ToggleFold(ProjectId),
    /// A project card dropped on another one: the dragged project lands
    /// before it, or after it when the pointer was past its middle.
    MoveProject(ProjectId, ProjectId, bool),
}

impl App {
    pub fn new(
        cfg: Config,
        store: Store,
        app: &adw::Application,
        w: Widgets,
    ) -> std::io::Result<App> {
        cmd::ensure_session(
            &cfg.tmux_socket,
            &cfg.tmux_session,
            SESSION_COLS,
            SESSION_ROWS,
        )?;
        // tmux, not the emulators, holds history. Set it once per server.
        // Tier 1: 4 busy panes cost 76.7 MB of tmux server memory at 20000 vs
        // 9.0 MB at 2000, at ~0.95 KB per line per pane.
        const HISTORY_LIMIT: usize = 2_000;
        let tmux = Client::attach(&cfg.tmux_socket, &cfg.tmux_session)?;
        let _ = cmd::set_history_limit(&tmux, HISTORY_LIMIT);
        let server = cmd::server(&tmux).ok();
        let font_pt = cfg.font_size;
        // Restore the divider the user last dragged before anything is shown.
        let saved_layout = crate::layout::Layout::load();
        let folded_projects = saved_layout.folded.clone();
        let muted = saved_layout.muted.clone();
        // An unknown id means a file written by a newer build: fall back
        // rather than refuse to start.
        let preset = Preset::from_id(&saved_layout.preset).unwrap_or(Preset::Balanced);
        let trees = saved_layout
            .trees
            .iter()
            .filter_map(|(id, text)| Some((*id, Node::decode(text)?)))
            .collect();
        w.outer.set_position(saved_layout.sidebar);

        // A notification button needs an `app.`-scoped action, and the shell
        // activates it long after the frame that raised it - so it parks the
        // intent like every other request instead of touching `App`.
        let pending = Pending::default();
        let focus_agent = gtk::gio::SimpleAction::new("focus-agent", Some(glib::VariantTy::INT64));
        let parked = pending.clone();
        let raise = w.window.clone();
        focus_agent.connect_activate(move |_, param| {
            if let Some(id) = param.and_then(|p| p.get::<i64>()) {
                parked.push(Pointer::Focus(id));
                raise.present();
            }
        });
        let pair_allow = gtk::gio::SimpleAction::new("pair-allow", Some(glib::VariantTy::STRING));
        {
            let parked = pending.clone();
            pair_allow.connect_activate(move |_, param| {
                if let Some(ip) = param.and_then(|p| p.str()).map(String::from) {
                    parked.push(Pointer::Pair { ip, allow: true });
                }
            });
        }
        app.add_action(&pair_allow);

        let pair_deny = gtk::gio::SimpleAction::new("pair-deny", Some(glib::VariantTy::STRING));
        {
            let parked = pending.clone();
            pair_deny.connect_activate(move |_, param| {
                if let Some(ip) = param.and_then(|p| p.str()).map(String::from) {
                    parked.push(Pointer::Pair { ip, allow: false });
                }
            });
        }
        app.add_action(&pair_deny);

        app.add_action(&focus_agent);

        // `taix restore <name>` reaches a window that is already open through
        // this action: GTK makes the second process remote, so its own
        // activate handler never runs in the primary.
        let restore = gtk::gio::SimpleAction::new("restore-session", Some(glib::VariantTy::STRING));
        let parked = pending.clone();
        let raise = w.window.clone();
        restore.connect_activate(move |_, param| {
            if let Some(name) = param.and_then(|p| p.str()) {
                parked.push(Pointer::RestoreSession(name.to_string()));
                raise.present();
            }
        });
        app.add_action(&restore);

        // Started before the first frame, so a phone that was already
        // watching reconnects while the window is still drawing itself.
        let web = Web::new(&cfg);
        let built = App {
            cfg,
            store,
            tmux,
            projects: Vec::new(),
            rows: Vec::new(),
            selected: None,
            focus: None,
            trees,
            laid_out: None,
            #[cfg(feature = "browser")]
            browsers: HashMap::new(),
            files_open: false,
            font_pt,
            saved_layout,
            w,
            app: app.clone(),
            markup: String::new(),
            last_reconcile: Instant::now(),
            pending,
            trace_key: None,
            last_paint: Instant::now(),
            sidebar_dirty: false,
            asleep: false,
            folded: folded_projects,
            sidebar: Vec::new(),
            sidebar_sig: Vec::new(),
            store_stamp: None,
            mem: (0, 0, 0, 0),
            panes: 0,
            server,
            measured_at: Instant::now(),
            bar_git: None,
            // Zero means "never measured", so the first frame reads it.
            last_mem: Instant::now() - MEM_INTERVAL,
            last_history: Instant::now(),
            previous: None,
            preset,
            zoomed: None,
            muted,
            notified_pairs: Vec::new(),
            find: Find::default(),
            started: Vec::new(),
            launched: Instant::now(),
            last_git: Instant::now() - GIT_INTERVAL,
            web_slow: WebSlow::default(),
            last_web_publish: Instant::now() - WEB_INTERVAL,
            jobs: taix_core::jobs::Ticker::default(),
            web,
            git_worker_busy: std::cell::Cell::new(false),
        };
        built.w.pair.set_callback({
            let pending = built.pending.clone();
            move |ip, allow| {
                pending.push(Pointer::Pair { ip, allow });
            }
        });
        built.refresh_editors();
        Ok(built)
    }

    /// Click to type here; drop a file to hand its path to whatever is
    /// running here.
    ///
    /// Dropping an image is the cheap way to give an agent a picture: they
    /// all read one from a path, so the path is all TaiX has to produce.
    fn wire_pointer(&self, card: &AgentCard, id: AgentId) {
        let click = gtk::GestureClick::new();
        // Primary only: a right-click opens the menu over whatever is
        // selected, and refocusing would redraw the label and lose that.
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        // Capture, because the body label is selectable and claims the press
        // in the bubble phase, so a click on the text never reached the card.
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let pending = self.pending.clone();
        click.connect_pressed(move |gesture, _, _, _| {
            pending.push(Pointer::Focus(id));
            // Take the focus, then hand the sequence back: claiming it in the
            // capture phase denied the label its drag, so text could not be
            // selected at all.
            gesture.set_state(gtk::EventSequenceState::Denied);
        });
        card.root.add_controller(click);

        // A full-screen program that turned mouse reporting on (anything
        // ratatui, htop, vim, an agent's picker) means its widgets are meant
        // to be clicked. Swallowing the press into a text selection is what
        // made every such UI dead inside TaiX while working in any other
        // terminal, so a plain press, drag and wheel go to the program and
        // Ctrl is what reaches the text underneath - the modifier a terminal
        // reserves for exactly this.
        let cell = {
            let (root, body) = (card.root.clone(), card.body.clone());
            move |x: f64, y: f64| -> Option<(usize, usize)> {
                let point = gtk::graphene::Point::new(x as f32, y as f32);
                let at = root.compute_point(&body, &point)?;
                let (cw, ch) = ui::cell_size(&body);
                // Where the text actually starts, asked of the label rather
                // than copied from the theme: the `.pane` padding has already
                // been changed once without the constant following, and a
                // padding that is wrong by one cell reports every click one
                // cell off.
                let (ox, oy) = ui::text_origin(&body);
                let (px, py) = (f64::from(at.x()) - ox, f64::from(at.y()) - oy);
                (px >= 0.0 && py >= 0.0).then(|| ((px / cw) as usize, (py / ch) as usize))
            }
        };
        // The cell the pointer last reported, so a move that stays inside one
        // cell costs nothing, and `held` marks a press this card is carrying.
        let hover: Rc<std::cell::Cell<Option<(usize, usize)>>> = Rc::default();
        let held = Rc::new(std::cell::Cell::new(false));
        // One `GestureDrag` rather than a click plus a motion controller: a
        // single sequence carries press, motion and release, so claiming it
        // cannot strand a button the program never sees released.
        let drag = gtk::GestureDrag::new();
        drag.set_button(gtk::gdk::BUTTON_PRIMARY);
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (cells, holding, mouse, pending, body) = (
            cell.clone(),
            held.clone(),
            card.mouse.clone(),
            self.pending.clone(),
            card.body.clone(),
        );
        drag.connect_drag_begin(move |gesture, x, y| {
            let ctrl = gesture
                .current_event_state()
                .contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let Some((col, row)) = cells(x, y).filter(|_| !ctrl) else {
                holding.set(false);
                return;
            };
            holding.set(true);
            pending.push(Pointer::Mouse(
                id,
                taix_term::button::LEFT,
                col,
                row,
                taix_term::Kind::Press,
            ));
            // Claim - which is what denies the label its selection - only
            // when the program is known to want the pointer. The event is
            // parked either way: this same click focuses the pane, and
            // focusing is what builds the emulator that knows.
            if mouse.get().wanted() {
                // A claimed press never reaches the label, so an old
                // selection would survive it - and a pane holding a
                // selection is deliberately not redrawn, which froze the
                // pane at the moment the program took the pointer.
                body.select_region(0, 0);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        let (cells, holding, at) = (cell.clone(), held.clone(), hover.clone());
        let pending = self.pending.clone();
        drag.connect_drag_update(move |gesture, dx, dy| {
            let Some((sx, sy)) = gesture.start_point().filter(|_| holding.get()) else {
                return;
            };
            let Some(now) = cells(sx + dx, sy + dy) else {
                return;
            };
            if at.replace(Some(now)) != Some(now) {
                pending.push(Pointer::Mouse(
                    id,
                    taix_term::button::LEFT,
                    now.0,
                    now.1,
                    taix_term::Kind::Motion,
                ));
            }
        });
        let (cells, holding, at) = (cell.clone(), held.clone(), hover.clone());
        let pending = self.pending.clone();
        drag.connect_drag_end(move |gesture, dx, dy| {
            if !holding.replace(false) {
                return;
            }
            // A release outside the pane still ends the button: report it at
            // the last cell the program was told about.
            let last = gesture
                .start_point()
                .and_then(|(sx, sy)| cells(sx + dx, sy + dy))
                .or_else(|| at.get());
            if let Some((col, row)) = last {
                pending.push(Pointer::Mouse(
                    id,
                    taix_term::button::LEFT,
                    col,
                    row,
                    taix_term::Kind::Release,
                ));
            }
        });
        card.root.add_controller(drag);

        // Hover, for the `1003` programs that highlight what is under the
        // pointer. Throttled to a cell change: a pixel is not a terminal
        // event, and every report costs a round trip to tmux.
        let motion = gtk::EventControllerMotion::new();
        let (cells, holding, at, mouse) = (cell, held, hover.clone(), card.mouse.clone());
        let pending = self.pending.clone();
        motion.connect_motion(move |controller, x, y| {
            let Some(now) = cells(x, y) else { return };
            // Ctrl means the pointer is selecting text; a program watching
            // hovers must not chase the cursor across its own widgets while
            // the user is dragging over them to copy.
            let ctrl = controller
                .current_event_state()
                .contains(gtk::gdk::ModifierType::CONTROL_MASK);
            if at.replace(Some(now)) == Some(now) || holding.get() || ctrl {
                return;
            }
            if mouse.get().motion {
                pending.push(Pointer::Mouse(
                    id,
                    taix_term::button::NONE,
                    now.0,
                    now.1,
                    taix_term::Kind::Motion,
                ));
            }
        });
        card.root.add_controller(motion);
        // GTK4 has no size-allocate signal, but the viewport's adjustments
        // follow its allocation, so a window resize, a divider drag or a
        // panel toggle asks for the tick that `autosize` runs in.
        for adjustment in [card.viewport.hadjustment(), card.viewport.vadjustment()] {
            adjustment.connect_page_size_notify(|_| wake());
        }

        // The "click to start" mark of a window with no pane.
        let start = gtk::GestureClick::new();
        let pending = self.pending.clone();
        start.connect_released(move |_, _, _, _| {
            pending.push(Pointer::Start(id));
        });
        card.start.add_controller(start);
        // "↓ live" over a scrolled-back pane.
        let pending = self.pending.clone();
        card.jump
            .connect_clicked(move |_| pending.push(Pointer::ScrollBottom(id)));
        // Middle-click pastes the primary selection, the habit every X11 and
        // Wayland terminal user already has.
        let middle = gtk::GestureClick::new();
        middle.set_button(gtk::gdk::BUTTON_MIDDLE);
        let pending = self.pending.clone();
        middle.connect_pressed(move |_, _, _, _| {
            pending.push(Pointer::PastePrimary(id));
        });
        card.root.add_controller(middle);

        // Real sources disagree about what they hand over: a file manager
        // sends a `GdkFileList`, some senders a single `GFile`, and a browser
        // dragging an image sends the pixels as a `GdkTexture` with no path
        // at all - which is why that case is stashed like a paste.
        let drop = gtk::DropTarget::new(gtk::glib::Type::INVALID, gtk::gdk::DragAction::COPY);
        drop.set_types(&[
            gtk::gdk::FileList::static_type(),
            gtk::gio::File::static_type(),
            gtk::gdk::Texture::static_type(),
        ]);
        let pending = self.pending.clone();
        drop.connect_drop(move |_, value, _, _| {
            let path = if let Ok(list) = value.get::<gtk::gdk::FileList>() {
                list.files().first().and_then(|f| f.path())
            } else if let Ok(file) = value.get::<gtk::gio::File>() {
                file.path()
            } else if let Ok(texture) = value.get::<gtk::gdk::Texture>() {
                stash_image(&texture.save_to_png_bytes()).ok()
            } else {
                None
            };
            match path {
                Some(path) => {
                    pending.push(Pointer::Drop(id, path));
                    true
                }
                None => false,
            }
        });
        card.root.add_controller(drop);

        // A pane header dragged here: the payload is the other window's id,
        // the side it hovers is where it lands.
        let dock = gtk::DropTarget::new(glib::Type::I64, gtk::gdk::DragAction::MOVE);
        // The header band is the tab zone: a card dropped on another card's
        // name joins it as a tab, the edges still split. The band's height
        // is asked of the widget rather than assumed, because the theme's
        // padding decides it.
        let side_at = {
            let root = card.root.clone();
            move |x: f64, y: f64| {
                let head = root.first_child().map_or(0, |h| h.height()) as f64;
                Side::at(x, y, root.width() as f64, root.height() as f64, head)
            }
        };
        let (hint, at) = (card.clone(), side_at.clone());
        dock.connect_motion(move |_, x, y| {
            hint.show_drop(Some(at(x, y)));
            gtk::gdk::DragAction::MOVE
        });
        let hint = card.clone();
        dock.connect_leave(move |_| hint.show_drop(None));
        let (hint, pending) = (card.clone(), self.pending.clone());
        dock.connect_drop(move |_, value, x, y| {
            hint.show_drop(None);
            let Ok(from) = value.get::<AgentId>() else {
                return false;
            };
            let side = side_at(x, y);
            // A window dropped on its own card means something only when it
            // is one tab of a group: it leaves the group and takes half the
            // pane. On a plain pane it is a no-op.
            if from == id && matches!(side, Side::Centre | Side::Tab) {
                return false;
            }
            pending.push(Pointer::Dock(from, id, side));
            true
        });
        card.root.add_controller(dock);

        // The wheel scrolls tmux's history, which is the only scrollback
        // there is. The viewport itself never scrolls: it clips. Inside a
        // program that asked for the mouse the wheel is its own - a list
        // scrolls, not the pane - and there is no scrollback there anyway.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let (pending, mouse, at) = (self.pending.clone(), card.mouse.clone(), hover);
        scroll.connect_scroll(move |_, _, dy| {
            let up = dy < 0.0;
            if mouse.get().wanted() {
                let (col, row) = at.get().unwrap_or((0, 0));
                let button = if up {
                    taix_term::button::WHEEL_UP
                } else {
                    taix_term::button::WHEEL_DOWN
                };
                pending.push(Pointer::Mouse(id, button, col, row, taix_term::Kind::Press));
            } else {
                pending.push(Pointer::Scroll(
                    id,
                    if up { SCROLL_STEP } else { -SCROLL_STEP },
                ));
            }
            glib::Propagation::Stop
        });
        card.root.add_controller(scroll);

        let pending = self.pending.clone();
        card.close.connect_clicked(move |_| {
            pending.push(Pointer::Close(id));
        });
        self.wire_window_actions(&card.root, id);
    }

    /// Apply what the pointer asked for, outside any other borrow.
    fn drain_pointer(&mut self) {
        let requests = self.pending.take();
        for request in requests {
            if let Err(e) = self.apply_pointer(request) {
                ui::set_status(&self.w, &e);
            }
        }
    }

    fn apply_pointer(&mut self, request: Pointer) -> Result<(), String> {
        match request {
            // Pressing into a pane on this screen is the take-back gesture
            // for *that* pane, so it runs before the press is delivered:
            // one click both reclaims the terminal and lands where it was
            // aimed. A browser driving another window is untouched.
            Pointer::Focus(id) => {
                self.take_keyboard(id);
                self.set_focus(id);
            }
            Pointer::Scroll(id, delta) => self.scroll_pane(id, delta),
            Pointer::Mouse(id, button, col, row, kind) => {
                if kind == taix_term::Kind::Press {
                    self.take_keyboard(id);
                }
                if !self.web.holds(id) {
                    self.send_mouse(id, button, col, row, kind);
                }
            }
            Pointer::ScrollBottom(id) => self.scroll_to_bottom(id),
            Pointer::Compare(id) => self.compare_with_live(id)?,
            Pointer::Diff(id) => self.open_diff(id)?,
            Pointer::Merge(id) => self.merge_worktree(id)?,
            Pointer::Transcript(id) => self.copy_transcript(id)?,
            Pointer::OpenFind => self.open_find(),
            Pointer::Zoom => self.toggle_zoom(),
            Pointer::Preset(id) => self.set_preset(&id),
            Pointer::Mute => self.toggle_mute(),
            Pointer::SaveSession(name) => self.save_session(&name)?,
            Pointer::RestoreSession(name) => self.restore_session(&name)?,
            Pointer::Drop(id, path) => {
                self.set_focus(id);
                let arg = keys::path_argument(&path);
                self.type_text(&arg);
            }
            // Closing keeps the worktree: killing a process is recoverable,
            // discarding unreviewed work is not, so that is a separate item.
            Pointer::Close(id) => self.kill_agent(id, false)?,
            Pointer::Discard(id) => self.kill_agent(id, true)?,
            Pointer::Restart(id) => self.restart(id)?,
            Pointer::Start(id) => self.relaunch(id)?,
            Pointer::Dock(from, onto, side) => self.dock(from, onto, side),
            #[cfg(feature = "browser")]
            Pointer::BrowserUrl(id, url) => self.browser_url(id, url),
            Pointer::Interrupt(id) => {
                self.set_focus(id);
                if let Some(pane) = self.focused_pane() {
                    let _ = cmd::send_key(&self.tmux, pane, "C-c");
                }
            }
            Pointer::Rename(id, name) => {
                self.store
                    .rename_agent(id, &name)
                    .map_err(|e| e.to_string())?;
                self.reload();
            }
            Pointer::Recolor(id, color) => {
                self.store
                    .set_color(id, color.as_deref())
                    .map_err(|e| e.to_string())?;
                self.reload();
            }
            Pointer::CopyPane(id) => {
                self.set_focus(id);
                // What was marked, or failing that the visible screen - not
                // the scrollback: "copy what I can see" is what a terminal's
                // copy means.
                let text = self.selected_text().unwrap_or_else(|| {
                    self.row(id)
                        .and_then(|r| r.agent.pane)
                        .and_then(|pane| cmd::capture_text(&self.tmux, pane, 0).ok())
                        .unwrap_or_default()
                });
                self.w.window.clipboard().set_text(&text);
                ui::set_status(&self.w, &format!("copied {} chars", text.chars().count()));
            }
            Pointer::Kill(id) => self.kill_pane(id)?,
            Pointer::PastePane(id) => {
                self.set_focus(id);
                self.paste();
            }
            Pointer::PastePrimary(id) => {
                self.set_focus(id);
                self.paste_primary();
            }
            Pointer::SelectAll(id) => {
                if let Some(row) = self.row(id) {
                    row.card.body.select_region(0, -1);
                }
            }
            Pointer::Type(text) => self.type_text(&text),
            Pointer::OpenFile(path, editor) => self.open_file(&path, editor.as_deref())?,
            Pointer::TerminalAt(dir) => self.terminal_at(&dir)?,
            Pointer::Notice(text) => ui::set_status(&self.w, &text),
            Pointer::ShowDiff(title, text) => {
                ui::show_text(&self.w.window, &title, &unified_markup(&text), false)
            }
            Pointer::Select(id) => self.select_project(id),
            Pointer::NewTerminal(project) => {
                self.select_project(project);
                let terminal = taix_core::terminal();
                self.spawn(project, &terminal).map(drop)?;
            }
            // A browser window is not a tmux window: it has a page rather
            // than a pane, so the menu entry routes past `spawn` entirely.
            Pointer::Spawn(project, harness) if harness == taix_core::BROWSER => {
                self.open_browser(project, None, false).map(drop)?;
            }
            Pointer::Spawn(project, harness) => {
                self.select_project(project);
                let harness = taix_core::by_id(&self.cfg, &harness);
                self.spawn(project, &harness).map(drop)?;
            }
            Pointer::RenameProject(id, name) => {
                self.store
                    .rename_project(id, &name)
                    .map_err(|e| e.to_string())?;
                self.reload();
            }
            Pointer::CopyPath(id) => {
                let Some(project) = self.project(id) else {
                    return Ok(());
                };
                let path = project.root.display().to_string();
                self.w.window.clipboard().set_text(&path);
                ui::set_status(&self.w, &format!("copied {path}"));
            }
            Pointer::OpenFolder(id) => {
                let Some(project) = self.project(id) else {
                    return Ok(());
                };
                // The desktop's own handler, so it lands in whatever file
                // manager the user actually has.
                let uri = format!("file://{}", project.root.display());
                gtk::gio::AppInfo::launch_default_for_uri(
                    &uri,
                    None::<&gtk::gio::AppLaunchContext>,
                )
                .map_err(|e| format!("cannot open folder: {e}"))?;
            }
            Pointer::CloseAll(id) => {
                let ids: Vec<AgentId> = self
                    .rows
                    .iter()
                    .filter(|r| r.agent.project == id)
                    .map(|r| r.agent.id)
                    .collect();
                for agent in ids {
                    self.kill_agent(agent, false)?;
                }
            }
            Pointer::RemoveProject(id) => self.remove_project(id)?,
            Pointer::ToggleFold(id) => {
                if self.folded.contains(&id) {
                    self.folded.retain(|p| *p != id);
                } else {
                    self.folded.push(id);
                }
                self.save_layout();
                self.sidebar_dirty = true;
            }
            Pointer::Pair { ip, allow } => {
                if allow {
                    self.web.approve(&ip);
                    ui::set_status(&self.w, &format!("Paired {ip}"));
                } else {
                    self.web.deny(&ip);
                    ui::set_status(&self.w, &format!("Denied {ip}"));
                }
            }
            Pointer::MoveProject(from, onto, below) => self.move_project(from, onto, below),
        }
        Ok(())
    }

    /// Kill a window and open a fresh one with the same harness, in the same
    /// project. What "restart this agent" means when the agent has wedged.
    fn restart(&mut self, id: AgentId) -> Result<(), String> {
        let Some(row) = self.row(id) else {
            return Ok(());
        };
        let project = row.agent.project;
        let harness = taix_core::by_id(&self.cfg, &row.agent.kind);
        self.kill_agent(id, false)?;
        self.spawn(project, &harness).map(drop)
    }

    /// End the process in a window and keep the window: what `Ctrl-D` at a
    /// prompt does, for a harness that will not take one. The scrollback is
    /// saved first, so the card that stays shows the last screen behind its
    /// "click to start" mark; a plain terminal is closed by the reload, as
    /// it would be by `Ctrl-D`.
    fn kill_pane(&mut self, id: AgentId) -> Result<(), String> {
        let Some(pane) = self.row(id).and_then(|r| r.agent.pane) else {
            return Ok(());
        };
        let _ = taix_core::history::save(&self.tmux, id, pane);
        cmd::kill_pane(&self.tmux, pane).map_err(|e| e.to_string())?;
        self.reload();
        Ok(())
    }

    /// Open a pane again for a window that lost its one - the harness quit,
    /// or the tmux server did not outlive the last TaiX. Nothing is started
    /// until the user asks: a relaunched agent costs a login, a model, a
    /// context, and a dashboard has no business spending those on its own.
    fn relaunch(&mut self, id: AgentId) -> Result<(), String> {
        let Some(row) = self.row(id) else {
            return Ok(());
        };
        if row.agent.pane.is_some() {
            return Ok(());
        }
        let agent = row.agent.clone();
        let project = self
            .project(agent.project)
            .ok_or("unknown project")?
            .clone();
        taix_core::spawn::relaunch(&self.store, &self.cfg, &project, &agent)?;
        self.reload();
        self.set_focus(id);
        Ok(())
    }

    /// Copy every live pane's scrollback out to the cache, so a window whose
    /// pane goes away can show and replay what it had. One `capture-pane`
    /// per pane, so it runs on the slow cadence and at exit, not per frame.
    pub fn save_histories(&self) {
        for row in &self.rows {
            if let Some(pane) = row.agent.pane
                && let Err(e) = taix_core::history::save(&self.tmux, row.agent.id, pane)
            {
                taix_core::trace!("history save for pane {pane} failed: {e}");
            }
        }
    }
    pub fn rename(&mut self, id: AgentId, name: &str) -> Result<(), String> {
        self.store
            .rename_agent(id, name)
            .map_err(|e| e.to_string())?;
        self.reload();
        Ok(())
    }

    pub fn toggle_fold_selected(&mut self) {
        if let Some(id) = self.selected {
            let _ = self.apply_pointer(Pointer::ToggleFold(id));
        }
    }

    /// Move to the previous or next project, wrapping. Keyboard equivalent of
    /// clicking another card.
    pub fn cycle_project(&mut self, delta: i32) {
        if self.projects.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|id| self.projects.iter().position(|p| p.id == id))
            .unwrap_or(0) as i32;
        let len = self.projects.len() as i32;
        let next = (at + delta).rem_euclid(len) as usize;
        let id = self.projects[next].id;
        self.select_project(id);
    }

    /// Focus the nth window of the selected project, ignoring an index that
    /// does not exist rather than wrapping - `Ctrl+Shift-9` should do nothing
    /// with three windows open, not jump to the third.
    pub fn focus_nth(&mut self, index: usize) {
        if let Some(id) = self.visible().get(index).copied() {
            self.set_focus(id);
        }
    }

    /// Notice panes that died without tmux telling us.
    ///
    /// `Ctrl-D` exits the shell and tmux closes the window, but the
    /// notification can arrive before the pane is actually reaped, so the
    /// reload that follows still sees it alive and the dead window lingered
    /// in the sidebar. One `list-panes` on a slow cadence is the safety net.
    fn reap_dead_panes(&mut self, panes: &[PaneInfo]) {
        let stale = self.rows.iter().any(|r| {
            r.agent
                .pane
                .is_some_and(|p| !panes.iter().any(|x| x.id == p))
        });
        if stale {
            self.reload();
        }
    }

    /// Rename windows after whatever is actually running in them.
    ///
    /// A window opened as a plain terminal in which the user then runs
    /// `claude` is a Claude Code window, and the sidebar should say so. A
    /// name the user typed is never overwritten, and the harness is recorded
    /// so `Restart` relaunches the right thing.
    fn adopt_running_harnesses(&mut self, panes: &[PaneInfo], table: &taix_core::mem::ProcTable) {
        let candidates: Vec<(AgentId, u32)> = self
            .rows
            .iter()
            .filter(|r| !r.agent.named)
            .filter_map(|r| r.agent.pane.map(|pane| (r.agent.id, pane)))
            .collect();
        if candidates.is_empty() {
            return;
        }
        let roots: Vec<u32> = candidates
            .iter()
            .filter_map(|(_, pane)| panes.iter().find(|p| p.id == *pane).map(|p| p.pid))
            .collect();
        if roots.len() != candidates.len() {
            // A pane died between listing and reading; try again next tick
            // rather than pairing the wrong pid with the wrong window.
            return;
        }
        let found = taix_core::running_harnesses(&self.cfg, &roots, table);
        let mut changed = false;
        for ((id, _), harness) in candidates.iter().zip(found) {
            let Some(row) = self.row(*id) else { continue };
            // What it should be called: the running harness, or the launch
            // harness once the agent exits and only a shell is left.
            let want = harness.unwrap_or_else(|| {
                if row.agent.kind == taix_core::terminal().id {
                    taix_core::terminal()
                } else {
                    taix_core::by_id(&self.cfg, &row.agent.kind)
                }
            });
            if row.agent.kind == want.id {
                continue;
            }
            let taken: Vec<Agent> = self
                .rows
                .iter()
                .filter(|r| r.agent.project == row.agent.project && r.agent.id != *id)
                .map(|r| r.agent.clone())
                .collect();
            let name = taix_core::next_name(&taken, &want);
            if self.store.set_harness(*id, &want.id, &name).is_ok() {
                changed = true;
            }
        }
        if changed {
            self.reload();
        }
    }

    // ---------- loading and reconciliation ----------

    /// Load persisted state and reattach to panes that survived a restart.
    ///
    /// An agent whose pane is gone is not "working" any more; leaving it
    /// claiming Working is the difference between a dashboard and a liar.
    pub fn reload(&mut self) {
        self.store_stamp = self.store.stamp();
        self.projects = self.store.projects().unwrap_or_default();
        self.jobs.rearm(
            &self.store.jobs().unwrap_or_default(),
            taix_core::jobs::now(),
        );
        let idle_after = Duration::from_millis(self.cfg.idle_after_ms);

        // One listing for the whole pass. A *failed* listing is not an empty
        // one: `unwrap_or_default` here meant that one busy or briefly
        // unavailable tmux made every pane look dead at once - plain
        // terminals were deleted from the store and agents were marked Done
        // with their pane forgotten, windows vanishing while their processes
        // were still running. Unknown means leave everything exactly as it
        // is and ask again next pass.
        let Ok(panes) = cmd::panes(&self.tmux) else {
            self.refresh_sidebar();
            return;
        };
        let Ok(agents) = self
            .store
            .reconcile(|pane| panes.iter().any(|p| p.id == pane))
        else {
            self.refresh_sidebar();
            return;
        };

        let mut kept: Vec<Row> = Vec::with_capacity(agents.len());
        for agent in agents {
            let existing = self.rows.iter().position(|r| r.agent.id == agent.id);
            match existing {
                Some(i) => {
                    let mut row = self.rows.swap_remove(i);
                    row.card
                        .update(&agent, &self.cfg, tint(&self.cfg, &agent).as_deref());
                    row.agent = agent;
                    row.dirty = true;
                    kept.push(row);
                }
                None => {
                    let card = ui::agent_card(&agent, &self.cfg);
                    self.wire_pointer(&card, agent.id);
                    // Seed the watcher from what is on screen. A row built
                    // after a restart has no output history, so a window that
                    // then goes quiet could never have a state derived again:
                    // it kept whatever the store last said - "working" for
                    // something long finished - and the idle reaper could
                    // never see it. The capture also gives the prompt
                    // heuristic something to read.
                    let mut watcher = Watcher::new(idle_after);
                    if let Some(pane) = agent.pane {
                        watcher.observe(&cmd::capture(&self.tmux, pane), Instant::now());
                    }
                    kept.push(Row {
                        card,
                        agent,
                        watcher,
                        live: None,
                        dirty: true,
                        last_snapshot: Instant::now() - SNAPSHOT_INTERVAL,
                        scroll: 0,
                        git: None,
                        mem_kib: None,
                        cpu: 0.0,
                        ticks: 0,
                        log: None,
                        active: Instant::now(),
                        notified_state: None,
                        applied_size: None,
                        resize_pending_since: None,
                        seed_seq: 0,
                        web_html: String::new(),
                        web_snap: Instant::now() - SNAPSHOT_INTERVAL,
                        web_size: Size { cols: 80, rows: 24 },
                        web_grid: None,
                        last_markup: String::new(),
                    });
                }
            }
        }
        kept.sort_by_key(|r| r.agent.id);
        self.rows = kept;

        if self
            .selected
            .is_none_or(|p| !self.projects.iter().any(|x| x.id == p))
        {
            self.selected = self.projects.first().map(|p| p.id);
        }
        self.refresh_sidebar();
        self.rebuild_grid();
        // Single point where the model is rebuilt, so the single place the
        // bar must be refreshed — otherwise an externally driven reload
        // leaves a stale window count on screen. The tree follows for the
        // same reason: removing a project moves the selection under it.
        self.show_project_panel();
        self.refresh_bar();
    }

    /// Bring the sidebar up to date: one card per project, its windows listed
    /// inside.
    ///
    /// Widgets are rebuilt ONLY when the set of projects or windows changes.
    /// A state change repaints the existing rows instead, because destroying
    /// a button between a pointer press and its release loses the click -
    /// which made every sidebar click intermittent until it was measured.
    fn refresh_sidebar(&mut self) {
        self.sidebar_dirty = false;
        let signature: SidebarShape = self
            .projects
            .iter()
            .map(|p| {
                (
                    p.id,
                    p.name.clone(),
                    self.rows
                        .iter()
                        .filter(|r| r.agent.project == p.id)
                        .map(|r| (r.agent.id, r.agent.name.clone()))
                        .collect(),
                )
            })
            .collect();

        if signature != self.sidebar_sig {
            self.sidebar_sig = signature;
            self.build_sidebar();
        }

        let attention = self.attention_counts();
        let folded = self.folded.clone();
        let remote = self.web.remote();
        for card in &self.sidebar {
            let windows: Vec<(AgentId, &'static str, Option<String>, String)> = self
                .rows
                .iter()
                .filter(|r| r.agent.project == card.project)
                .map(|r| {
                    (
                        r.agent.id,
                        r.agent.state.as_str(),
                        tint(&self.cfg, &r.agent),
                        r.agent.name.clone(),
                    )
                })
                .collect();
            card.refresh(
                self.selected == Some(card.project),
                folded.contains(&card.project),
                self.focus,
                &windows,
                attention.get(&card.project).copied().unwrap_or(0),
                &remote,
            );
        }
        // The pane says it too, on the screen the user is looking at.
        for row in &self.rows {
            row.card.remote.set_visible(remote.contains(&row.agent.id));
        }
        ui::apply_filter(&self.w, &self.sidebar);
    }

    /// Re-apply the sidebar filter. The entry is view state, so this reads
    /// it rather than storing a copy.
    pub fn filter_sidebar(&self) {
        ui::apply_filter(&self.w, &self.sidebar);
    }

    /// Construct the sidebar widgets and connect them.
    fn build_sidebar(&mut self) {
        let harnesses = taix_core::available(&self.cfg);
        let cards: Vec<ui::SidebarCard> = self
            .projects
            .iter()
            .map(|project| {
                let windows: Vec<(Agent, taix_core::Harness)> = self
                    .rows
                    .iter()
                    .filter(|r| r.agent.project == project.id)
                    .map(|r| {
                        let harness = taix_core::by_id(&self.cfg, &r.agent.kind);
                        (r.agent.clone(), harness)
                    })
                    .collect();
                ui::sidebar_card(project, &windows, &harnesses)
            })
            .collect();

        for card in &cards {
            let id = card.project;
            let pending = self.pending.clone();
            card.select
                .connect_clicked(move |_| pending.push(Pointer::Select(id)));
            let pending = self.pending.clone();
            card.new_terminal
                .connect_clicked(move |_| pending.push(Pointer::NewTerminal(id)));
            let pending = self.pending.clone();
            card.fold
                .connect_clicked(move |_| pending.push(Pointer::ToggleFold(id)));
            self.wire_project_actions(&card.root, id);
            self.wire_project_drop(&card.root, id);
            for row in &card.windows {
                let agent = row.id;
                let pending = self.pending.clone();
                row.open.connect_clicked(move |_| {
                    pending.push(Pointer::Select(id));
                    pending.push(Pointer::Focus(agent));
                });
                let pending = self.pending.clone();
                row.close.connect_clicked(move |_| {
                    pending.push(Pointer::Close(agent));
                });
                self.wire_window_actions(&row.root, agent);
            }
        }
        ui::set_sidebar(&self.w, &cards);
        self.sidebar = cards;
    }

    /// Accept another project card dropped on this one. The pointer's half
    /// of the card decides above or below, and the card draws that edge
    /// while the drag is over it - so the drop lands where the line is.
    fn wire_project_drop(&self, card: &gtk::Box, id: ProjectId) {
        let drop = gtk::DropTarget::new(glib::Type::U64, gtk::gdk::DragAction::MOVE);
        fn edge(card: &gtk::Box, below: Option<bool>) {
            card.remove_css_class("drop-above");
            card.remove_css_class("drop-below");
            match below {
                Some(true) => card.add_css_class("drop-below"),
                Some(false) => card.add_css_class("drop-above"),
                None => {}
            }
        }
        let host = card.clone();
        let below_at = move |y: f64| y > host.height() as f64 / 2.0;
        let (host, at) = (card.clone(), below_at.clone());
        drop.connect_motion(move |_, _, y| {
            edge(&host, Some(at(y)));
            gtk::gdk::DragAction::MOVE
        });
        // `enter` too, not only `motion`: the first frame over a new card has
        // no motion yet, and a card with no bar on it is a card showing the
        // desktop theme's own drop ring instead - which is the flicker.
        let (host, at) = (card.clone(), below_at.clone());
        drop.connect_enter(move |_, _, y| {
            edge(&host, Some(at(y)));
            gtk::gdk::DragAction::MOVE
        });
        let host = card.clone();
        drop.connect_leave(move |_| edge(&host, None));
        let (host, pending) = (card.clone(), self.pending.clone());
        drop.connect_drop(move |_, value, _, y| {
            edge(&host, None);
            match value.get::<u64>() {
                Ok(from) if from as ProjectId != id => {
                    pending.push(Pointer::MoveProject(from as ProjectId, id, below_at(y)));
                    true
                }
                _ => false,
            }
        });
        card.add_controller(drop);
    }

    /// Install the `project.*` actions the card's menu addresses.
    ///
    /// One group per card, inserted on the card itself, so every item acts on
    /// its own project without the menu model having to carry an id.
    fn wire_project_actions(&self, host: &impl IsA<gtk::Widget>, id: ProjectId) {
        let group = gtk::gio::SimpleActionGroup::new();
        let push = |name: &str, make: Box<dyn Fn() -> Pointer>| {
            let action = gtk::gio::SimpleAction::new(name, None);
            let pending = self.pending.clone();
            action.connect_activate(move |_, _| pending.push(make()));
            group.add_action(&action);
        };
        push("new-terminal", Box::new(move || Pointer::NewTerminal(id)));
        push("copy-path", Box::new(move || Pointer::CopyPath(id)));
        push("open-folder", Box::new(move || Pointer::OpenFolder(id)));
        push("fold", Box::new(move || Pointer::ToggleFold(id)));
        push("close-all", Box::new(move || Pointer::CloseAll(id)));

        // Parameterised: one action serves every harness in the submenu.
        let spawn = gtk::gio::SimpleAction::new("spawn", Some(glib::VariantTy::STRING));
        let pending = self.pending.clone();
        spawn.connect_activate(move |_, param| {
            if let Some(harness) = param.and_then(|p| p.str()) {
                pending.push(Pointer::Spawn(id, harness.to_string()));
            }
        });
        group.add_action(&spawn);

        // Both of these ask before acting, so they open a dialog and park the
        // answer rather than mutating from inside a menu callback.
        let rename = gtk::gio::SimpleAction::new("rename", None);
        let pending = self.pending.clone();
        let window = self.w.window.clone();
        let current = self.project(id).map(|p| p.name.clone()).unwrap_or_default();
        rename.connect_activate(move |_, _| {
            let pending = pending.clone();
            ui::prompt_text(&window, "Rename project", &current, "Rename", move |name| {
                pending.push(Pointer::RenameProject(id, name));
            });
        });
        group.add_action(&rename);

        let remove = gtk::gio::SimpleAction::new("remove", None);
        let pending = self.pending.clone();
        let window = self.w.window.clone();
        let name = current_name(self.project(id));
        let agents = self.agent_count(id);
        remove.connect_activate(move |_, _| {
            let pending = pending.clone();
            ui::confirm_remove_project(&window, &name, agents, move || {
                pending.push(Pointer::RemoveProject(id));
            });
        });
        group.add_action(&remove);

        host.as_ref().insert_action_group("project", Some(&group));
    }

    /// Install the `win.*` actions one window's menu addresses.
    fn wire_window_actions(&self, host: &impl IsA<gtk::Widget>, id: AgentId) {
        let group = gtk::gio::SimpleActionGroup::new();
        let push = |name: &str, make: Box<dyn Fn() -> Pointer>| {
            let action = gtk::gio::SimpleAction::new(name, None);
            let pending = self.pending.clone();
            action.connect_activate(move |_, _| pending.push(make()));
            group.add_action(&action);
        };
        push("focus", Box::new(move || Pointer::Focus(id)));
        push("copy", Box::new(move || Pointer::CopyPane(id)));
        push("select-all", Box::new(move || Pointer::SelectAll(id)));
        push("paste", Box::new(move || Pointer::PastePane(id)));
        push("interrupt", Box::new(move || Pointer::Interrupt(id)));
        push("kill", Box::new(move || Pointer::Kill(id)));
        push("restart", Box::new(move || Pointer::Restart(id)));
        push("close", Box::new(move || Pointer::Close(id)));
        push("compare", Box::new(move || Pointer::Compare(id)));
        push("transcript", Box::new(move || Pointer::Transcript(id)));
        push("diff", Box::new(move || Pointer::Diff(id)));

        let color = gtk::gio::SimpleAction::new("color", Some(glib::VariantTy::STRING));
        let pending = self.pending.clone();
        color.connect_activate(move |_, param| {
            let value = param.and_then(|p| p.str()).unwrap_or("none");
            let color = (value != "none").then(|| value.to_string());
            pending.push(Pointer::Recolor(id, color));
        });
        group.add_action(&color);

        let rename = gtk::gio::SimpleAction::new("rename", None);
        let pending = self.pending.clone();
        let window = self.w.window.clone();
        let current = self
            .row(id)
            .map(|r| r.agent.name.clone())
            .unwrap_or_default();
        rename.connect_activate(move |_, _| {
            let pending = pending.clone();
            ui::prompt_text(&window, "Rename window", &current, "Rename", move |name| {
                pending.push(Pointer::Rename(id, name));
            });
        });
        group.add_action(&rename);

        let merge = gtk::gio::SimpleAction::new("merge", None);
        let pending = self.pending.clone();
        let window = self.w.window.clone();
        let name = self
            .row(id)
            .map(|r| r.agent.name.clone())
            .unwrap_or_default();
        merge.connect_activate(move |_, _| {
            let pending = pending.clone();
            ui::confirm(
                &window,
                &format!("Merge {name} back?"),
                "Its branch is merged into the project's default branch with a merge commit. Uncommitted work is refused, not merged.",
                "Merge",
                move || pending.push(Pointer::Merge(id)),
            );
        });
        group.add_action(&merge);

        let discard = gtk::gio::SimpleAction::new("discard", None);
        let pending = self.pending.clone();
        let window = self.w.window.clone();
        let meta = self.agent_meta(id);
        discard.connect_activate(move |_, _| {
            let Some((_, name, has_worktree)) = meta.clone() else {
                return;
            };
            let pending = pending.clone();
            ui::confirm_kill(&window, &name, has_worktree, move |discard| {
                pending.push(if discard {
                    Pointer::Discard(id)
                } else {
                    Pointer::Close(id)
                });
            });
        });
        group.add_action(&discard);

        host.as_ref().insert_action_group("win", Some(&group));
    }

    /// Paste the primary selection - what middle-click means on a terminal.
    /// Text only: the primary selection is a text protocol.
    pub fn paste_primary(&self) {
        let primary = self.w.window.primary_clipboard();
        let pending = self.pending.clone();
        glib::spawn_future_local(async move {
            if let Ok(Some(text)) = primary.read_text_future().await {
                pending.push(Pointer::Type(text.to_string()));
            }
        });
    }

    /// Paste into the focused pane. An image is written to disk and its path
    /// typed, because that is how every agent CLI takes a picture.
    pub fn paste(&self) {
        let clipboard = self.w.window.clipboard();
        let pending = self.pending.clone();
        let w = self.w.clone();
        glib::spawn_future_local(async move {
            if let Ok(Some(texture)) = clipboard.read_texture_future().await {
                match stash_image(&texture.save_to_png_bytes()) {
                    Ok(path) => {
                        pending.push(Pointer::Type(keys::path_argument(&path)));
                        ui::set_status(&w, &format!("pasted image → {}", path.display()));
                    }
                    Err(e) => ui::set_status(&w, &format!("cannot save pasted image: {e}")),
                }
                return;
            }
            // Files copied in a file manager arrive as a file list, not text:
            // hand over their paths, the way a drop does.
            if let Ok(value) = clipboard
                .read_value_future(gtk::gdk::FileList::static_type(), glib::Priority::DEFAULT)
                .await
                && let Ok(list) = value.get::<gtk::gdk::FileList>()
            {
                let paths: String = list
                    .files()
                    .iter()
                    .filter_map(|f| f.path())
                    .map(|p| keys::path_argument(&p))
                    .collect();
                if !paths.is_empty() {
                    pending.push(Pointer::Type(paths));
                    return;
                }
            }
            if let Ok(Some(text)) = clipboard.read_text_future().await {
                pending.push(Pointer::Type(text.to_string()));
            }
        });
    }

    /// What the file tree asks for, from a widget that does not own `App`.
    /// It goes through the pointer queue like every other click, so it can
    /// never re-enter a borrow.
    pub fn ask_files(&self, ask: crate::files::Ask) {
        use crate::files::Ask;
        self.pending.push(match ask {
            Ask::Open(path, editor) => Pointer::OpenFile(path, editor),
            Ask::Insert(path) => Pointer::Type(keys::path_argument(&path)),
            Ask::Terminal(dir) => Pointer::TerminalAt(dir),
            Ask::Notice(text) => Pointer::Notice(text),
        });
    }

    /// The same road for the source-control panel: it hands over text to
    /// show or say, never a borrow of `App`.
    pub fn ask_git(&self, ask: crate::git::Ask) {
        use crate::git::Ask;
        self.pending.push(match ask {
            Ask::Diff(title, text) => Pointer::ShowDiff(title, text),
            Ask::Open(path) => Pointer::OpenFile(path, None),
            Ask::Notice(text) => Pointer::Notice(text),
        });
    }

    /// Fill the bottom bar: where you are, which branch, what you are typing
    /// into, what it all costs.
    fn refresh_bar(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_git) >= GIT_INTERVAL {
            self.last_git = now;
            self.refresh_git();
        }
        let project = self.selected.and_then(|id| self.project(id));
        let visible = self.visible();
        let focused = self.focus.and_then(|id| self.row(id));
        // `by_id` builds a `Harness`, so its label is owned: name it here
        // rather than borrowing a temporary inside the struct.
        let harness = focused.map(|row| taix_core::by_id(&self.cfg, &row.agent.kind).label);
        let focus = focused.map(|row| ui::BarFocus {
            name: row.agent.name.as_str(),
            harness: harness.as_deref().unwrap_or_default(),
            state: row.agent.state.as_str(),
            size: row.applied_size.map(|s| (s.cols, s.rows)),
            scroll: row.scroll,
            worktree: row.git.as_deref(),
            kib: row.mem_kib,
        });
        ui::set_bar(
            &self.w,
            &ui::Bar {
                root: project.map(|p| p.root.as_path()),
                project: project.map(|p| p.name.as_str()),
                branch: self.bar_git.as_deref(),
                windows: visible.len(),
                live: self.rows.iter().filter(|r| r.live.is_some()).count(),
                attention: self
                    .rows
                    .iter()
                    .filter(|r| r.agent.state.needs_attention())
                    .count(),
                focus,
                ui_kib: self.mem.0,
                tmux_kib: self.mem.1,
                project_kib: self.mem.2,
                panes_kib: self.mem.3,
                uptime: self.launched.elapsed(),
                tmux_version: self.server.as_ref().map(|(_, v)| v.as_str()),
                panes: self.panes,
            },
        );
    }

    // ---------- the web front end ----------

    fn attention_counts(&self) -> HashMap<ProjectId, usize> {
        let mut counts = HashMap::new();
        for row in &self.rows {
            if row.agent.state.needs_attention() {
                *counts.entry(row.agent.project).or_insert(0) += 1;
            }
        }
        counts
    }

    // ---------- selection and focus ----------

    pub fn select_project(&mut self, project: ProjectId) {
        if self.selected == Some(project) {
            return;
        }
        // The project going out of view keeps its rows and states, not its
        // rendered text: a Pango layout per pane is the one thing worth
        // dropping, and `dirty` brings it back the moment it is shown again.
        for row in self
            .rows
            .iter_mut()
            .filter(|r| Some(r.agent.project) == self.selected)
        {
            row.card.body.set_text("");
            row.dirty = true;
        }
        self.open_startup_windows(project);
        self.selected = Some(project);
        self.rebuild_grid();
        // The card's selected class, the bar's path and the file tree all
        // follow selection.
        self.sidebar_dirty = true;
        self.show_project_panel();
        self.refresh_bar();
    }

    /// The windows this screen lays out: the selected project's, less the
    /// headless browser windows, which exist for agents and have no card.
    fn visible(&self) -> Vec<AgentId> {
        self.rows
            .iter()
            .filter(|r| Some(r.agent.project) == self.selected && !r.agent.headless)
            .map(|r| r.agent.id)
            .collect()
    }

    /// Only the selected project's agents get widgets. Everything else is a
    /// `Row` with an unparented card and no emulator.
    ///
    /// The `GtkPaned` tree is rebuilt ONLY when its shape changes. Rebuilding
    /// on every tmux event would throw away divider positions the user
    /// dragged, and tmux resizes fire events constantly.
    fn rebuild_grid(&mut self) {
        self.ensure_browsers();
        let ids = self.visible();
        self.harvest();
        let tree = self
            .selected
            .and_then(|p| Node::sync(self.trees.remove(&p), &ids, self.preset));
        // Zoom shows one card; the rest keep their rows and their snapshots,
        // so unzooming is instant and nothing has to be re-seeded.
        let shown = match self.zoomed {
            Some(z) if ids.contains(&z) => Some(Node::Leaf(z)),
            _ => tree.clone(),
        };
        if let (Some(p), Some(t)) = (self.selected, tree) {
            self.trees.insert(p, t);
        }
        // Tabbed cards hide their own header; a card that has just left a
        // group gets it back. Done before rendering, because the strip is
        // built from the same tree.
        let tabbed = shown.as_ref().map(Node::tabbed).unwrap_or_default();
        for id in &ids {
            if let Some(row) = self.row(*id) {
                row.card.set_tabbed(tabbed.contains(id));
            }
        }
        let shape = shown.as_ref().map(Node::encode);
        let rebuilt = shape != self.laid_out.as_ref().map(|(s, _)| s.clone());
        if rebuilt {
            let widget = ui::set_pane_tree(&self.w, || {
                shown.as_ref().map(|t| {
                    t.render(&|id| self.card_widget(id), &|ids, active| {
                        self.tab_strip(ids, active)
                    })
                })
            });
            self.laid_out = shape.zip(widget);
        }
        for id in &ids {
            if let Some(row) = self.row_mut(*id) {
                row.dirty = true;
            }
        }
        let refocus = match self.focus {
            Some(f) if ids.contains(&f) => Some(f),
            _ => ids.first().copied(),
        };
        // Demote, do not merely forget: dropping the id while leaving the
        // emulator attached to the row stranded a second `Live` that nothing
        // could ever reclaim. Switching projects then left two emulators
        // running, which is exactly what this architecture exists to avoid.
        //
        // Only when something moved, though. A rebuild that changes neither
        // the shape nor the focused window used to drop the live emulator
        // and re-seed it from a capture, which is a visible blink in a pane
        // that is printing - and `reload` runs on every state change.
        if rebuilt || self.focus != refocus {
            self.demote_focus();
            if let Some(id) = refocus {
                self.set_focus(id);
            }
        }
        ui::set_empty(&self.w, ids.is_empty(), self.projects.is_empty());
    }

    /// Read the dividers the user dragged back into the model, so the next
    /// rebuild puts them where they were. Only when the tree on screen is the
    /// project's own: a zoomed pane has none to read.
    fn harvest(&mut self) {
        if let Some((shape, widget)) = &self.laid_out
            && let Some(tree) = self.selected.and_then(|p| self.trees.get_mut(&p))
            && tree.encode() == *shape
        {
            tree.harvest(widget);
        }
    }

    /// A header dropped on a pane: its edges split, its middle swaps, and
    /// its own header strip takes it as a tab.
    fn dock(&mut self, from: AgentId, onto: AgentId, side: Side) {
        let Some(project) = self.selected else { return };
        self.harvest();
        let Some(tree) = self.trees.remove(&project) else {
            return;
        };
        let tree = match side {
            Side::Centre => {
                let mut tree = tree;
                tree.swap(from, onto);
                tree
            }
            Side::Tab => tree.tab(from, onto),
            // Its own pane's edge: leave the tab group it is in.
            side if from == onto => tree.split_out(from, side),
            side => tree.dock(from, onto, side),
        };
        self.trees.insert(project, tree);
        self.rebuild_grid();
        self.save_layout();
    }

    /// Promote one agent to a live emulator and demote the previous one.
    pub fn set_focus(&mut self, id: AgentId) {
        if self.focus == Some(id) {
            return;
        }
        // Only a real pane counts as "the one before": `rebuild_grid`
        // demotes focus and re-takes it, so assigning unconditionally
        // recorded `None` and lost the pair on the next tmux event.
        if let Some(prev) = self.focus {
            self.previous = Some(prev);
        }
        self.demote_focus();
        if let Some(row) = self.row_mut(id) {
            row.card.root.add_css_class("focused");
        }
        self.focus = Some(id);
        // Focusing a window that is behind another tab has to bring it to
        // the front, or the focused card is one nobody can see.
        if self
            .selected
            .and_then(|p| self.trees.get_mut(&p))
            .is_some_and(|tree| tree.activate(id))
        {
            self.rebuild_grid();
        }
        self.reseed_focus();
        // The sidebar marks the focused window; it must move with the click,
        // not with the next reload.
        self.sidebar_dirty = true;
    }

    /// Drop the live emulator, if any. The single place `focus` is cleared.
    fn demote_focus(&mut self) {
        if let Some(prev) = self.focus.take()
            && let Some(row) = self.row_mut(prev)
        {
            row.live = None;
            row.dirty = true;
            row.card.root.remove_css_class("focused");
        }
    }

    /// Build the focused pane's live emulator at the pane's CURRENT size,
    /// primed from a capture plus the real cursor position.
    ///
    /// Called on focus change and after a resize: an emulator's grid is fixed
    /// at construction, so a stale one wraps every line at the old width.
    fn reseed_focus(&mut self) {
        let Some(id) = self.focus else { return };
        let Some(pane) = self.row(id).and_then(|r| r.agent.pane) else {
            return;
        };
        let Some(seed) = cmd::seed(&self.tmux, pane) else {
            return;
        };
        if let Some(row) = self.row_mut(id) {
            let mut live = Live::new(Size {
                cols: seed.cols,
                rows: seed.rows,
            });
            live.seed(&seed.screen, seed.cursor);
            // A capture carries text and colour, never modes, so without
            // tmux's own flags a pane running a TUI came back from every
            // focus change believing nothing wanted the pointer - and its
            // buttons went back to selecting text.
            live.feed(&seed.modes);
            row.card.mouse.set(live.mouse());
            row.live = Some(live);
            row.dirty = true;
            row.seed_seq = seed.seq;
        }
    }

    pub fn cycle_focus(&mut self) {
        let ids = self.visible();
        if ids.is_empty() {
            return;
        }
        let next = match self.focus.and_then(|f| ids.iter().position(|&v| v == f)) {
            Some(i) => ids[(i + 1) % ids.len()],
            None => ids[0],
        };
        self.set_focus(next);
    }

    pub fn focused(&self) -> Option<AgentId> {
        self.focus
    }

    fn row(&self, id: AgentId) -> Option<&Row> {
        self.rows.iter().find(|r| r.agent.id == id)
    }

    /// The focused pane's selected text. `Ctrl-C` belongs to the terminal
    /// now, so this is the only way text leaves a pane.
    pub fn selected_text(&self) -> Option<String> {
        let label = &self.row(self.focus?)?.card.body;
        let (from, to) = label.selection_bounds()?;
        let text = label.text();
        Some(
            text.chars()
                .skip(usize::try_from(from).ok()?)
                .take(usize::try_from(to - from).ok()?)
                .collect(),
        )
    }

    fn row_mut(&mut self, id: AgentId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.agent.id == id)
    }

    fn row_by_pane(&mut self, pane: PaneId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.agent.pane == Some(pane))
    }

    // ---------- mutations ----------

    pub fn add_project(&mut self, root: PathBuf) -> Result<(), String> {
        let project_root = taix_git::project_root(&root).map_err(|e| e.to_string())?;
        let name = project_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| project_root.display().to_string());
        let project = self
            .store
            .add_project(&name, &project_root)
            .map_err(|e| e.to_string())?;
        self.selected = Some(project.id);
        self.reload();
        Ok(())
    }

    pub fn remove_project(&mut self, id: ProjectId) -> Result<(), String> {
        taix_core::purge_project(&self.store, id, |pane| {
            let _ = cmd::kill_pane(&self.tmux, pane);
        })?;
        self.rows.retain(|r| r.agent.project != id);
        self.selected = None;
        self.reload();
        Ok(())
    }

    /// Reorder the sidebar: `from` lands next to `onto`, above it or below
    /// it. The order lives in the store, so it is the same order the CLI and
    /// the TUI list, and it survives a restart.
    fn move_project(&mut self, from: ProjectId, onto: ProjectId, below: bool) {
        // Agent ids and project ids are both `i64`, so a pane header dropped
        // on the sidebar could carry a number that happens to name a project.
        // The drag payload's type keeps them apart; this is the second gate.
        if from == onto || self.project(from).is_none() {
            return;
        }
        let others: Vec<ProjectId> = self
            .projects
            .iter()
            .map(|p| p.id)
            .filter(|id| *id != from)
            .collect();
        let Some(at) = others.iter().position(|id| *id == onto) else {
            return;
        };
        let index = if below { at + 1 } else { at };
        if let Err(e) = self.store.move_project(from, index) {
            ui::set_status(&self.w, &e.to_string());
            return;
        }
        self.projects = self.store.projects().unwrap_or_default();
        self.store_stamp = self.store.stamp();
        self.sidebar_dirty = true;
    }

    pub fn project(&self, id: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Create an agent: optional git worktree, DB row, tmux window, in that
    /// order. Each step's failure leaves nothing dangling.
    ///
    /// This screen shows the result, because this screen asked for it.
    pub fn spawn(
        &mut self,
        project: ProjectId,
        harness: &taix_core::Harness,
    ) -> Result<AgentId, String> {
        self.spawn_isolated(project, harness, None, true)
    }

    /// The same, for a window a browser asked for: the new terminal opens
    /// where the person who asked for it is looking, and this screen keeps
    /// the project and the pane it was already on.
    pub fn spawn_elsewhere(
        &mut self,
        project: ProjectId,
        harness: &taix_core::Harness,
    ) -> Result<AgentId, String> {
        self.spawn_isolated(project, harness, None, false)
    }

    /// `isolate` overrides both the project file and the global setting, which
    /// is how a restored session reproduces the window it recorded rather
    /// than whatever the config says today. `adopt` selects and focuses the
    /// new window here.
    fn spawn_isolated(
        &mut self,
        project: ProjectId,
        harness: &taix_core::Harness,
        isolate: Option<bool>,
        adopt: bool,
    ) -> Result<AgentId, String> {
        let proj = self.project(project).ok_or("unknown project")?.clone();
        let existing: Vec<Agent> = self
            .rows
            .iter()
            .filter(|r| r.agent.project == project)
            .map(|r| r.agent.clone())
            .collect();
        let spawned =
            taix_core::spawn::window(&self.store, &self.cfg, &proj, harness, &existing, isolate)?;
        if adopt {
            self.selected = Some(project);
        }
        self.reload();
        if adopt {
            self.set_focus(spawned.agent.id);
        }
        Ok(spawned.agent.id)
    }

    /// Open a file the way the config says: a desktop editor is launched
    /// with the file, a terminal editor gets a window of its own in the
    /// project, which closes with it.
    ///
    /// `editor` is a catalogue id or a command from the "Open with" menu;
    /// `None` is the default - the config's, else the first installed.
    fn open_file(&mut self, path: &Path, editor: Option<&str>) -> Result<(), String> {
        use taix_core::editor;
        let chosen = match editor {
            Some(id) => editor::by_id(id).or_else(|| editor::from_command(id)),
            None => editor::resolve(&self.cfg),
        };
        let chosen = chosen.ok_or("no editor found - pick one in Settings")?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if chosen.terminal {
            let project = self.selected.ok_or("no project selected")?;
            let program = chosen
                .command
                .split_whitespace()
                .next()
                .unwrap_or(&chosen.label);
            let mut harness = taix_core::terminal();
            harness.label = format!("{program} {name}");
            harness.command = format!(
                "{} {}",
                chosen.command,
                taix_core::terminal::sh(&path.to_string_lossy())
            );
            return self.spawn(project, &harness).map(drop);
        }
        let info = gtk::gio::AppInfo::create_from_commandline(
            &chosen.command,
            Some(&chosen.label),
            gtk::gio::AppInfoCreateFlags::NONE,
        )
        .map_err(|e| e.to_string())?;
        info.launch(
            &[gtk::gio::File::for_path(path)],
            None::<&gtk::gio::AppLaunchContext>,
        )
        .map_err(|e| format!("{}: {e}", chosen.label))?;
        ui::set_status(&self.w, &format!("{name} → {}", chosen.label));
        Ok(())
    }

    /// A shell in this directory, as a window of the selected project. The
    /// directory is part of the command rather than the window's cwd, so
    /// the record stays a plain terminal of the project.
    fn terminal_at(&mut self, dir: &Path) -> Result<(), String> {
        let project = self.selected.ok_or("no project selected")?;
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut harness = taix_core::terminal();
        harness.label = format!("{name}/");
        harness.command = format!(
            "cd {} && exec \"${{SHELL:-sh}}\" -l",
            taix_core::terminal::sh(&dir.to_string_lossy())
        );
        self.spawn(project, &harness).map(drop)
    }

    /// Tell the file tree which editors this machine has and which one
    /// "open" means, so its menu can say so.
    fn refresh_editors(&self) {
        self.w.files.set_editors(
            taix_core::editor::installed(),
            taix_core::editor::resolve(&self.cfg),
        );
    }

    /// Change the pane font size. The next `autosize` pass picks up the new
    /// cell metrics and re-grids tmux to match, so zooming shows more or less
    /// of the agent's output rather than just scaling a fixed grid.
    pub fn zoom(&mut self, delta: f64) {
        let next = (self.font_pt + delta).clamp(FONT_MIN, FONT_MAX);
        if (next - self.font_pt).abs() < f64::EPSILON {
            return;
        }
        self.font_pt = next;
        crate::theme::set_pane_font(self.font_pt);
        ui::toast(&self.w, &format!("Pane font {:.0}pt", self.font_pt));
    }

    pub fn zoom_reset(&mut self) {
        self.zoom(self.cfg.font_size - self.font_pt);
    }

    /// A header button was pressed: open the column on that tab, or close
    /// it if that tab is already what is showing.
    pub fn click_panel(&mut self, tab: Panel) {
        let open = self.panel_tab() != Some(tab);
        self.toggle_panel(tab, open);
    }

    /// Attach or detach the side column, on the tab the caller asked for.
    ///
    /// The panel widgets outlive the column - they are boxes and labels -
    /// but their rows are dropped when it closes, so a tree left open on a
    /// large project costs nothing while it is hidden.
    pub fn toggle_panel(&mut self, tab: Panel, on: bool) {
        if !on {
            if self.files_open {
                self.saved_layout.files = self.files_width();
            }
            self.w.files_split.set_end_child(None::<&gtk::Widget>);
            self.w.files.clear();
            self.w.git.clear();
            self.files_open = false;
            self.sync_panel_toggles();
            return;
        }
        // Selecting the tab runs its own handler, which lights the header
        // button and wakes the panel: this must not do either itself.
        match tab {
            Panel::Files => self.w.files_tab.set_active(true),
            Panel::Git => self.w.git_tab.set_active(true),
        }
        if self.files_open {
            self.show_project_panel();
            self.sync_panel_toggles();
            return;
        }
        self.w.files_split.set_end_child(Some(&self.w.panel_body));
        self.files_open = true;
        self.show_project_panel();
        self.sync_panel_toggles();
        // `Paned` positions its divider from the left, so the panel's width
        // is the remainder: place it once the split knows how wide it is.
        let total = self.w.files_split.width();
        let want = match self.saved_layout.files {
            saved if saved > 120 => saved,
            _ => 280,
        };
        if total > want {
            self.w.files_split.set_position(total - want);
        }
    }

    /// Which tab the column is showing, if it is open at all.
    fn panel_tab(&self) -> Option<Panel> {
        if !self.files_open {
            return None;
        }
        Some(match self.w.panel_stack.visible_child_name().as_deref() {
            Some("git") => Panel::Git,
            _ => Panel::Files,
        })
    }

    /// The header buttons are a view of the column, not a second source of
    /// truth: exactly the open tab's button is lit.
    fn sync_panel_toggles(&self) {
        let tab = self.panel_tab();
        ui::light(&self.w.files_toggle, tab == Some(Panel::Files));
        ui::light(&self.w.git_toggle, tab == Some(Panel::Git));
    }

    /// Point whichever panel is open at the selected project, or empty it.
    fn show_project_panel(&self) {
        if !self.files_open {
            return;
        }
        match self.selected.and_then(|id| self.project(id)) {
            Some(project) => {
                self.w.files.set_root(&project.name, &project.root);
                self.w.git.set_root(&project.name, &project.root);
            }
            None => {
                self.w.files.clear();
                self.w.git.clear();
            }
        }
    }

    /// The panel's own width, which is what is worth remembering: the
    /// divider's position moves with the window.
    fn files_width(&self) -> i32 {
        (self.w.files_split.width() - self.w.files_split.position()).max(0)
    }

    /// Persist divider positions. Called on window close; a panel that was
    /// never opened keeps whatever width was last stored rather than
    /// recording a meaningless zero.
    pub fn save_layout(&self) {
        let files = if self.files_open {
            self.files_width()
        } else {
            self.saved_layout.files
        };

        crate::layout::Layout {
            sidebar: self.w.outer.position(),
            files,
            folded: self.folded.clone(),
            muted: self.muted.clone(),
            preset: self.preset.id().to_string(),
            trees: self.trees.iter().map(|(p, t)| (*p, t.encode())).collect(),
        }
        .save();
    }

    pub fn agent_count(&self, project: ProjectId) -> usize {
        self.rows
            .iter()
            .filter(|r| r.agent.project == project)
            .count()
    }

    /// Stop an agent's process. `discard_worktree` also removes its worktree,
    /// which throws away uncommitted work, so the caller must confirm.
    pub fn kill_agent(&mut self, id: AgentId, discard_worktree: bool) -> Result<(), String> {
        let Some(row) = self.row(id) else {
            return Ok(());
        };
        let agent = row.agent.clone();
        if let Some(pane) = agent.pane {
            let _ = cmd::kill_pane(&self.tmux, pane);
        }
        if discard_worktree
            && let (Some(wt), Some(proj)) = (&agent.worktree, self.project(agent.project))
            && let Ok(repo) = taix_git::Repo::open(&proj.root)
        {
            repo.remove_worktree(wt, true).map_err(|e| e.to_string())?;
        }
        if self.focus == Some(id) {
            self.focus = None;
        }
        self.rows.retain(|r| r.agent.id != id);
        self.store.remove_agent(id).map_err(|e| e.to_string())?;
        taix_core::history::remove(id);
        self.reload();
        Ok(())
    }

    /// The focused pane, if any. Every keystroke lands here.
    fn focused_pane(&self) -> Option<u32> {
        self.focus
            .and_then(|id| self.row(id))
            .and_then(|r| r.agent.pane)
    }

    /// Forward one keystroke to the focused pane.
    ///
    /// Written into the control-mode client, not a `tmux send-keys` process:
    /// the fork was ~2 ms on the main thread per key, and a burst of typing
    /// stacked them up in front of the frame that would have echoed them.
    pub fn key(&mut self, press: &keys::Press) {
        let Some(id) = self.focus else { return };
        if self.hand_off(id) {
            return;
        }
        self.send_keys(id, press);
    }

    /// Whether this keystroke belongs to a browser, and saying so once.
    ///
    /// Only about the pane being typed into: a browser driving a terminal
    /// in another project is none of this keystroke's business. Silent
    /// after the first swallowed key - the reminder is for the moment you
    /// start typing and nothing appears, not for every key after it.
    fn hand_off(&mut self, id: AgentId) -> bool {
        if !self.web.holds(id) {
            return false;
        }
        if !self.web.scolded {
            self.web.scolded = true;
            let name = self.window_name(id);
            ui::set_status(
                &self.w,
                &format!("A browser is driving {name} - click the pane to take it back"),
            );
        }
        true
    }

    /// A window's name, for a sentence about it.
    fn window_name(&self, id: AgentId) -> String {
        self.row(id)
            .map(|row| row.agent.name.clone())
            .unwrap_or_else(|| "that terminal".to_string())
    }

    /// Send one keystroke to one window's pane, whoever asked.
    fn send_keys(&mut self, id: AgentId, press: &keys::Press) {
        // Typing returns the pane to the live bottom, the way it does in every
        // terminal: you cannot type usefully into history.
        self.back_to_live(id);
        let Some(pane) = self.row(id).and_then(|r| r.agent.pane) else {
            return;
        };
        if std::env::var_os("TAIX_TRACE").is_some() {
            self.trace_key = Some(Instant::now());
        }
        let _ = match press {
            keys::Press::Text(text) => cmd::send_bytes(&self.tmux, pane, text.as_bytes()),
            keys::Press::Named(name) => cmd::send_key(&self.tmux, pane, name),
            keys::Press::Bytes(bytes) => cmd::send_bytes(&self.tmux, pane, bytes),
        };
    }

    /// Drop one pane back to live output.
    fn back_to_live(&mut self, id: AgentId) {
        if let Some(row) = self.row_mut(id)
            && row.scroll != 0
        {
            row.scroll = 0;
            row.dirty = true;
            row.card.set_scroll(0);
        }
    }

    /// Type a literal string into the focused pane, without a newline.
    ///
    /// Used by paste and by drop: the user, not TaiX, decides when to submit.
    pub fn type_text(&mut self, text: &str) {
        let Some(id) = self.focus else { return };
        if self.hand_off(id) {
            return;
        }
        self.send_text(id, text);
    }

    fn send_text(&mut self, id: AgentId, text: &str) {
        if text.is_empty() {
            return;
        }
        self.back_to_live(id);
        if let Some(pane) = self.row(id).and_then(|r| r.agent.pane) {
            let _ = cmd::send_text(&self.tmux, pane, text);
        }
    }

    fn paste_text(&mut self, id: AgentId, text: &str) {
        if text.is_empty() {
            return;
        }
        self.back_to_live(id);
        if let Some(pane) = self.row(id).and_then(|r| r.agent.pane) {
            let _ = taix_tmux::cmd::paste(&self.tmux, pane, text);
        }
    }

    // ---------- frame ----------

    /// Whether the compositor says this window is not on screen.
    ///
    /// `SUSPENDED` is the Wayland answer - the surface will not be shown, so
    /// frames for it are wasted - and it covers minimised, another workspace
    /// and fully occluded. `MINIMIZED` is the X11 spelling of the same thing.
    fn hidden(&self) -> bool {
        use gtk::prelude::*;
        // A focused window is never asleep, whatever the compositor claims.
        // Cheap insurance: the cost of a wrong `SUSPENDED` is a frozen
        // dashboard, and this reduces that to "only while you look away".
        if self.w.window.is_active() {
            return false;
        }
        if !self.w.window.is_visible() {
            return true;
        }
        let Some(surface) = self.w.window.surface() else {
            return true;
        };
        match surface.downcast::<gtk::gdk::Toplevel>() {
            Ok(toplevel) => {
                let state = toplevel.state();
                state.contains(gtk::gdk::ToplevelState::SUSPENDED)
                    || state.contains(gtk::gdk::ToplevelState::MINIMIZED)
            }
            Err(_) => false,
        }
    }

    /// See `RECONCILE_INTERVAL`: fast while any pane has spoken since the
    /// last pass, otherwise the memory cadence.
    fn reconcile_interval(&self) -> Duration {
        if self.rows.iter().any(|r| r.active > self.last_reconcile) {
            RECONCILE_INTERVAL
        } else {
            MEM_INTERVAL
        }
    }

    /// How long the next tick can wait: a frame if anything on screen is
    /// mid-change, otherwise until a due snapshot or the reconcile pass.
    /// This is what lets an idle window stop ticking altogether.
    fn next_due(&self) -> Duration {
        let now = Instant::now();
        let mut due = self
            .reconcile_interval()
            .saturating_sub(now.duration_since(self.last_reconcile));
        // A window nobody can see still has to feed the browsers watching
        // it, so "hidden" means hidden from everyone.
        if self.dormant() {
            return due;
        }
        if self.sidebar_dirty || !self.pending.is_empty() {
            return TICK;
        }
        for row in &self.rows {
            if row.resize_pending_since.is_some() {
                return TICK;
            }
            if !row.dirty || Some(row.agent.project) != self.selected {
                continue;
            }
            // A selection holds the frame; look again at the snapshot rate.
            let selecting = row.card.body.selection_bounds().is_some();
            let wait = if row.live.is_some() && !selecting {
                TICK
            } else {
                SNAPSHOT_INTERVAL.saturating_sub(now.duration_since(row.last_snapshot))
            };
            due = due.min(wait.max(TICK));
        }
        // A browser gets twenty frames a second, which is all a phone can
        // show: without this an attached client woke the UI thread sixty
        // times a second whether or not anything had changed.
        if self.web.clients() > 0 {
            let web_wait = WEB_INTERVAL.saturating_sub(self.last_web_publish.elapsed());
            due = due.min(web_wait.max(TICK));
        }
        due
    }

    /// Whether there is nobody to draw for: this window off-screen *and*
    /// no browser holding the event stream.
    fn dormant(&self) -> bool {
        self.hidden() && self.web.clients() == 0
    }

    pub fn tick(&mut self) {
        self.drain_pointer();
        self.drain_web();
        self.drain_browser_ops();
        self.apply_git_result();
        // A project can be shown without anyone selecting it - `reload` picks
        // the first one - so the startup windows are opened from the frame,
        // where no borrow is held and `spawn` can safely re-enter `reload`.
        if let Some(project) = self.selected {
            self.open_startup_windows(project);
        }
        // Cloned once per frame rather than per event: `row_by_pane` borrows
        // `self` mutably, so the config cannot be read through it.
        let cfg_log = self.cfg.clone();
        let mut topology_changed = false;
        for ev in self.tmux.bus.drain() {
            match ev {
                Ev::Output { pane, data, seq } => {
                    let now = Instant::now();
                    // A transcript is opened on first output - a window that
                    // never says anything should not leave an empty file
                    // behind - so the project is looked up only then, not on
                    // every chunk a streaming agent sends.
                    let needs_log = self
                        .row_by_pane(pane)
                        .is_some_and(|r| r.log.is_none())
                        .then(|| {
                            self.row_by_pane(pane)
                                .map(|r| r.agent.project)
                                .and_then(|p| self.project(p))
                                .map(|p| p.name.clone())
                        })
                        .flatten();
                    if let Some(row) = self.row_by_pane(pane) {
                        row.watcher.observe(&data, now);
                        row.active = now;
                        // The seed's screen already shows anything stamped
                        // before it; feeding it again printed the last line
                        // twice.
                        if seq > row.seed_seq
                            && let Some(live) = &mut row.live
                        {
                            live.feed(&data);
                            // A TUI turns reporting on when it starts and off
                            // when it exits, both in this stream, and the
                            // pointer handlers cannot borrow the row to ask.
                            row.card.mouse.set(live.mouse());
                        }
                        // Every pane's output passes through this one client,
                        // so a transcript costs a buffered append and nothing
                        // else; `flush_logs` puts it on disk on the slow
                        // cadence.
                        if let Some(project) = &needs_log {
                            row.log =
                                taix_core::log::Log::open(&cfg_log, project, &row.agent.name).ok();
                        }
                        if let Some(log) = &mut row.log {
                            let _ = log.append(&data);
                        }
                        row.dirty = true;
                    }
                }
                // tmux is the source of truth for which panes exist. One
                // resize notifies once per window; one reload serves them all.
                Ev::Layout { .. } | Ev::WindowsChanged => topology_changed = true,
                Ev::Exit => eprintln!("[tmux] control client exited"),
            }
        }
        if topology_changed {
            self.reload();
        }
        // Drawing is the whole per-frame cost: a `capture-pane` fork per
        // unfocused pane, a markup string, and a Pango reshape. None of it is
        // worth a cycle while nobody can see the window, and tmux keeps every
        // byte in the meantime. Output, watchers, transcripts, states and
        // notifications carry on above and below this - a dashboard that stops
        // paging you when you look away is useless.
        // Drawing is skipped only when there is nobody at all to draw for:
        // a browser watching is a viewer, and the emulator it reads is the
        // same one this window would have kept.
        let hidden = self.dormant();
        if hidden && !self.asleep {
            self.asleep = true;
            // The one allocation worth reclaiming: an emulator holds a grid.
            if let Some(id) = self.focus
                && let Some(row) = self.row_mut(id)
            {
                row.live = None;
            }
            trim_heap();
        } else if !hidden && self.asleep {
            self.asleep = false;
            // Everything on screen is now stale by however long we slept.
            for row in &mut self.rows {
                row.dirty = true;
            }
            self.reseed_focus();
        }
        if !hidden {
            self.autosize();
            self.redraw();
            if self.sidebar_dirty {
                self.refresh_sidebar();
            }
        }
        if self.last_reconcile.elapsed() >= self.reconcile_interval() {
            self.last_reconcile = Instant::now();
            if self.store_changed_externally() {
                self.reload();
            }
            // One listing over the channel serves states and every sweep.
            // A failed listing is unknown, not empty: nothing is derived
            // from it, and the next pass asks again.
            if let Ok(panes) = cmd::panes(&self.tmux) {
                self.reconcile_states(&panes);
                // Sweeps and states are not skipped while hidden:
                // notifications and the idle reaper are the reason to keep
                // a dashboard running when you are not looking at it. Only
                // the bar - which nobody can read, and whose memory segment
                // is the most expensive reading in the app - waits until
                // the window is back on screen.
                self.housekeeping(&panes);
            }
            if !hidden {
                self.refresh_bar();
            }
        }
        self.refresh_web_chip();
        self.publish_web();
    }

    /// `taix add`, `taix rm` and the MCP server write the same store file, so
    /// the window cannot assume it is the only writer. One `stat` per pass;
    /// the stamp is taken again by `reload`, which reads whatever changed.
    fn store_changed_externally(&self) -> bool {
        self.store.stamp() != self.store_stamp
    }

    /// Take the stamp after writing the store ourselves, so the change we
    /// just made does not read as somebody else's on the next pass.
    pub(super) fn restamp(&mut self) {
        self.store_stamp = self.store.stamp();
    }

    // ---------- scrollback ----------

    /// Move the focused pane's view through tmux's history.
    ///
    /// TaiX keeps no scrollback of its own - one focused emulator holds the
    /// live screen and tmux holds everything above it - so scrolling is a
    /// different capture, not a viewport move.
    pub fn scroll_focused(&mut self, delta: i32) {
        if let Some(id) = self.focus {
            self.scroll_pane(id, delta);
        }
    }

    /// Positive `delta` moves *up*, into history; negative moves back toward
    /// the live bottom.
    fn scroll_pane(&mut self, id: AgentId, delta: i32) {
        let Some(pane) = self.row(id).and_then(|r| r.agent.pane) else {
            return;
        };
        let history = cmd::history_size(&self.tmux, pane) as i32;
        let Some(row) = self.row_mut(id) else { return };
        let next = (row.scroll as i32 + delta).clamp(0, history);
        if next == row.scroll as i32 {
            return;
        }
        row.scroll = next as usize;
        row.dirty = true;
        row.card.set_scroll(row.scroll);
    }

    /// Hand one pointer event to whatever runs in the pane, in the encoding
    /// it asked for. Silent when it asked for nothing: the emulator's own
    /// modes are the only truth here, so a click on a shell prompt cannot
    /// print `[<0;4;9M` into the command line.
    fn send_mouse(
        &mut self,
        id: AgentId,
        button: u8,
        col: usize,
        row: usize,
        kind: taix_term::Kind,
    ) {
        // Scrolled back, the cells on screen are tmux's history and not the
        // program's screen, so no position may be reported from here. The
        // wheel still moves the view, which is the only way back down.
        if self.row(id).is_some_and(|r| r.scroll > 0) {
            match button {
                taix_term::button::WHEEL_UP => self.scroll_pane(id, SCROLL_STEP),
                taix_term::button::WHEEL_DOWN => self.scroll_pane(id, -SCROLL_STEP),
                _ => {}
            }
            return;
        }
        // Only the focused pane has the emulator that knows which modes the
        // program set, so a press on any other pane has to take the focus
        // before it can be encoded - and a press is what focuses a pane
        // anyway. Done here rather than trusting the click handler's own
        // `Focus` to be applied first, which depends on the order GTK runs
        // two controllers on one widget in. Motion and wheel are excluded on
        // purpose: passing over a pane must not steal the keyboard.
        if kind == taix_term::Kind::Press && button < taix_term::button::WHEEL_UP {
            self.set_focus(id);
        }
        let Some(target) = self.row(id) else { return };
        let (Some(pane), Some(live)) = (target.agent.pane, target.live.as_ref()) else {
            return;
        };
        if let Some(bytes) = taix_term::report(live.mouse(), button, col, row, kind) {
            let _ = cmd::send_bytes(&self.tmux, pane, &bytes);
        }
    }

    /// Back to the live bottom in one step, from however far up.
    fn scroll_to_bottom(&mut self, id: AgentId) {
        let Some(row) = self.row_mut(id) else { return };
        if row.scroll == 0 {
            return;
        }
        row.scroll = 0;
        row.dirty = true;
        row.card.set_scroll(0);
    }

    /// Put a history line into view, a third of a screen down so there is
    /// context above it.
    fn scroll_to_line(&mut self, id: AgentId, line: usize) {
        let Some(pane) = self.row(id).and_then(|r| r.agent.pane) else {
            return;
        };
        let history = cmd::history_size(&self.tmux, pane);
        let rows = pane_size(&self.tmux, pane).rows;
        // A hit's line counts from the oldest line of history, so the number
        // of lines between it and the live bottom is what we scroll by.
        let above = history.saturating_sub(line);
        let scroll = above.saturating_add(rows / 3).min(history);
        if let Some(row) = self.row_mut(id) {
            row.scroll = scroll;
            row.dirty = true;
            row.card.set_scroll(scroll);
        }
    }

    // ---------- find ----------

    /// Show the find bar and search the focused pane.
    pub fn open_find(&mut self) {
        self.find.target = self.focus;
        self.w.find.root.set_visible(true);
        self.w.find.entry.grab_focus();
        let needle = self.find.needle.clone();
        self.search(&needle);
    }

    pub fn close_find(&mut self) {
        self.w.find.root.set_visible(false);
        self.find.hits.clear();
        if let Some(id) = self.find.target
            && let Some(row) = self.row_mut(id)
        {
            row.scroll = 0;
            row.dirty = true;
            row.card.set_scroll(0);
        }
    }

    /// Re-run the search. Reads the pane's whole history, which is why this
    /// happens on a keystroke in the bar and not on every frame.
    pub fn search(&mut self, needle: &str) {
        self.find.needle = needle.to_string();
        self.find.current = 0;
        self.find.hits.clear();
        let target = self.find.target.or(self.focus);
        self.find.target = target;
        let text = target
            .and_then(|id| self.row(id))
            .and_then(|r| r.agent.pane)
            .and_then(|pane| cmd::history_text(&self.tmux, pane).ok())
            .unwrap_or_default();
        self.find.hits = crate::find::search(&text, needle);
        self.show_find_count();
        if !self.find.hits.is_empty()
            && let Some(id) = target
        {
            let line = self.find.hits[0].line;
            self.scroll_to_line(id, line);
        }
    }

    /// Step through the hits, wrapping - a search that stops at the end makes
    /// you retype it.
    pub fn find_step(&mut self, delta: i32) {
        if self.find.hits.is_empty() {
            return;
        }
        let n = self.find.hits.len() as i32;
        self.find.current = (self.find.current as i32 + delta).rem_euclid(n) as usize;
        self.show_find_count();
        if let Some(id) = self.find.target {
            let line = self.find.hits[self.find.current].line;
            self.scroll_to_line(id, line);
        }
    }

    fn show_find_count(&self) {
        let text = if self.find.needle.is_empty() {
            String::new()
        } else if self.find.hits.is_empty() {
            "no matches".to_string()
        } else {
            format!("{}/{}", self.find.current + 1, self.find.hits.len())
        };
        self.w.find.count.set_text(&text);
    }

    // ---------- palette ----------

    /// Everything reachable, as one searchable list: projects, windows, the
    /// harnesses that are installed, and the actions that are otherwise only
    /// a keystroke or a menu away.
    fn palette_items(&self) -> Vec<crate::palette::Item> {
        let mut items = Vec::new();
        for project in &self.projects {
            items.push(crate::palette::Item {
                id: format!("project:{}", project.id),
                group: "Projects",
                title: project.name.clone(),
                subtitle: project.root.display().to_string(),
            });
        }
        for row in &self.rows {
            let project = self
                .project(row.agent.project)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            items.push(crate::palette::Item {
                id: format!("window:{}", row.agent.id),
                group: "Windows",
                title: row.agent.name.clone(),
                subtitle: format!("{project} · {}", row.agent.state.as_str()),
            });
        }
        if let Some(project) = self.selected {
            for harness in taix_core::available(&self.cfg) {
                if harness.is_terminal() {
                    continue;
                }
                items.push(crate::palette::Item {
                    id: format!("spawn:{}", harness.id),
                    group: "Start here",
                    title: harness.label.clone(),
                    subtitle: current_name(self.project(project)),
                });
            }
        }
        for preset in Preset::ALL {
            items.push(crate::palette::Item {
                id: format!("preset:{}", preset.id()),
                group: "Layout",
                title: preset.label().to_string(),
                subtitle: String::new(),
            });
        }
        for name in taix_core::session::list(&self.cfg) {
            items.push(crate::palette::Item {
                id: format!("session:{name}"),
                group: "Sessions",
                title: name,
                subtitle: "restore".to_string(),
            });
        }
        for (id, title) in [
            ("action:new-terminal", "New terminal"),
            ("action:find", "Find in pane"),
            ("action:zoom", "Zoom the live pane"),
            ("action:save-session", "Save session…"),
            ("action:mute", "Mute this project's notifications"),
        ] {
            items.push(crate::palette::Item {
                id: id.to_string(),
                group: "Actions",
                title: title.to_string(),
                subtitle: String::new(),
            });
        }
        items
    }

    pub fn open_palette(&mut self) {
        let items = self.palette_items();
        let pending = self.pending.clone();
        let window = self.w.window.clone();
        // Captured, not parsed out of the id: "here" means the project that
        // was selected when the palette opened.
        let selected = self.selected;
        crate::palette::open(&self.w.window, items, move |id| {
            let Some((kind, rest)) = id.split_once(':') else {
                return;
            };
            let request = match kind {
                "project" => rest.parse().ok().map(Pointer::Select),
                "window" => rest.parse().ok().map(Pointer::Focus),
                "spawn" => None,
                "preset" => Some(Pointer::Preset(rest.to_string())),
                "session" => Some(Pointer::RestoreSession(rest.to_string())),
                "action" => match rest {
                    "find" => Some(Pointer::OpenFind),
                    "zoom" => Some(Pointer::Zoom),
                    "mute" => Some(Pointer::Mute),
                    _ => None,
                },
                _ => None,
            };
            // Spawning and saving need a project or a name, so they are
            // resolved where that is known rather than parsed out of the id.
            match (kind, rest, request) {
                (_, _, Some(request)) => pending.push(request),
                ("spawn", harness, None) => {
                    if let Some(project) = selected {
                        pending.push(Pointer::Spawn(project, harness.to_string()));
                    }
                }
                ("action", "new-terminal", None) => {
                    if let Some(project) = selected {
                        pending.push(Pointer::NewTerminal(project));
                    }
                }
                ("action", "save-session", None) => {
                    let pending = pending.clone();
                    ui::prompt_text(&window, "Save session", "work", "Save", move |name| {
                        pending.push(Pointer::SaveSession(name));
                    });
                }
                _ => {}
            }
        });
    }

    /// Choose a saved session.
    ///
    /// The palette is the picker: it is already a searchable list with a
    /// keyboard, and a second one would be a second thing to maintain.
    pub fn open_session_picker(&mut self) {
        let names = taix_core::session::list(&self.cfg);
        if names.is_empty() {
            ui::set_status(&self.w, "no saved sessions");
            return;
        }
        let items: Vec<crate::palette::Item> = names
            .into_iter()
            .map(|name| crate::palette::Item {
                id: name.clone(),
                group: "Sessions",
                title: name,
                subtitle: "restore".to_string(),
            })
            .collect();
        let pending = self.pending.clone();
        crate::palette::open(&self.w.window, items, move |name| {
            pending.push(Pointer::RestoreSession(name));
        });
    }

    /// Ask for a name, then save. Parked, because the dialog answers later.
    pub fn prompt_save_session(&self) {
        let pending = self.pending.clone();
        ui::prompt_text(
            &self.w.window,
            "Save session",
            "work",
            "Save",
            move |name| {
                pending.push(Pointer::SaveSession(name));
            },
        );
    }

    /// One pane, full window, and back. The grid is rebuilt because the pane
    /// tree is structural, not a visibility flag.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None => self.focus,
        };
        self.rebuild_grid();
    }

    pub fn set_preset(&mut self, id: &str) {
        let Some(preset) = Preset::from_id(id) else {
            return;
        };
        if self.preset == preset {
            return;
        }
        self.preset = preset;
        // The preset is the shape from now on: the project's own tree goes,
        // and with it the dividers - the tree changes shape by design.
        if let Some(p) = self.selected {
            self.trees.remove(&p);
        }
        self.laid_out = None;
        self.rebuild_grid();
        ui::set_status(&self.w, &format!("layout: {}", preset.label()));
    }

    /// Put `taix -t -s <id>` for the selected project on the clipboard: paste
    /// it in any terminal and the project's windows open there as tmux panes.
    pub fn copy_terminal_command(&self) {
        let Some(project) = self.selected.and_then(|id| self.project(id)) else {
            ui::toast(&self.w, "Select a project first");
            return;
        };
        let line = taix_core::terminal::taix_command(project);
        self.w.window.clipboard().set_text(&line);
        ui::set_status(&self.w, &format!("copied: {line}"));
    }

    /// The long form of the same thing: the tmux line itself, for a machine
    /// that has tmux but not taix. Built from the rows on screen and the
    /// preset in use, so it matches what the button's tooltip promises.
    pub fn copy_tmux_command(&self) {
        let Some(project) = self.selected.and_then(|id| self.project(id)) else {
            ui::toast(&self.w, "Select a project first");
            return;
        };
        let agents: Vec<Agent> = self
            .rows
            .iter()
            .filter(|r| r.agent.project == project.id)
            .map(|r| r.agent.clone())
            .collect();
        let layout = taix_core::terminal::layout_for_preset(self.preset.id());
        let line = taix_core::terminal::tmux_command(&self.cfg, project, &agents, layout, false);
        self.w.window.clipboard().set_text(&line);
        ui::set_status(&self.w, "copied the tmux command");
    }

    /// The Automation dialog changed a job: re-read the store so the gate
    /// is re-armed against the new schedule.
    pub fn jobs_changed(&mut self) {
        self.reload();
    }

    /// "Run now" from the Automation dialog. The dialog cannot borrow
    /// `App`, and `App` owns the child process, so the id is parked here
    /// and the next housekeeping pass spawns the runner.
    pub fn request_job_run(&mut self, id: taix_core::JobId) {
        self.jobs.request(id);
        ui::set_status(&self.w, "running the job…");
    }

    /// The settings dialog wrote a new config: read it back and redraw
    /// everything that was built from the old one.
    pub fn config_changed(&mut self) {
        let font_changed;
        {
            let fresh = Config::load();
            font_changed = fresh.font_size != self.cfg.font_size;
            self.cfg = fresh;
        }
        crate::theme::reinstall(self.cfg.theme.as_deref());
        if font_changed {
            self.font_pt = self.cfg.font_size;
            crate::theme::set_pane_font(self.font_pt);
        }
        ui::refresh_harness_menu(&self.w, &self.cfg);
        self.refresh_editors();
        // The switch and the port live in the same file, so this is where a
        // change to either is acted on: started, stopped, or rebound.
        let cfg = self.cfg.clone();
        self.web.reconcile(&cfg);
        self.web.painted = None;
        // Labels, icons and tints all derive from the config.
        for row in &self.rows {
            row.card.update(
                &row.agent,
                &self.cfg,
                tint(&self.cfg, &row.agent).as_deref(),
            );
        }
        self.sidebar_sig.clear();
        self.refresh_sidebar();
    }

    pub fn toggle_mute(&mut self) {
        let Some(project) = self.selected else { return };
        let name = current_name(self.project(project));
        if let Some(i) = self.muted.iter().position(|p| *p == project) {
            self.muted.remove(i);
            ui::set_status(&self.w, &format!("{name}: notifications on"));
        } else {
            self.muted.push(project);
            ui::set_status(&self.w, &format!("{name}: muted"));
        }
        self.save_layout();
    }

    // ---------- sessions ----------

    /// Save which projects and windows are open, so a working set can be
    /// reopened in one command instead of rebuilt by hand.
    fn save_session(&mut self, name: &str) -> Result<(), String> {
        let snap = taix_core::spawn::snapshot(&self.store, name)?;
        let path = taix_core::session::save(&self.cfg, &snap).map_err(|e| e.to_string())?;
        ui::toast(&self.w, &format!("saved {}", path.display()));
        Ok(())
    }

    /// Reopen a saved session. Projects that are already registered are
    /// reused, and a project that already has windows is left alone: restore
    /// must not duplicate what is in front of you.
    pub fn restore(&mut self, name: &str) -> Result<(), String> {
        self.restore_session(name)
    }

    fn restore_session(&mut self, name: &str) -> Result<(), String> {
        let snap = taix_core::session::load(&self.cfg, name).map_err(|e| e.to_string())?;
        let opened = taix_core::spawn::restore(&self.store, &self.cfg, &snap)?;
        self.reload();
        // A restored project may be the only one, and `reload` only picks a
        // selection when there was none.
        if self.selected.is_none() {
            self.selected = self.projects.first().map(|p| p.id);
        }
        ui::toast(&self.w, &format!("{name}: {opened} windows"));
        Ok(())
    }

    // ---------- worktrees ----------

    // ---------- reaper ----------

    /// Close windows nobody is using, when asked to.
    ///
    /// Off unless `reap_idle_after_ms` is set, and it only ever takes windows
    /// with no worktree and nothing running: an idle agent that holds work is
    /// not garbage, it is waiting for you.
    fn reap_idle(&mut self) {
        let Some(after) = self.cfg.reap_idle_after_ms else {
            return;
        };
        let after = Duration::from_millis(after);
        let stale: Vec<(AgentId, String)> = self
            .rows
            .iter()
            .filter(|r| {
                r.agent.worktree.is_none()
                    && matches!(r.agent.state, AgentState::Idle | AgentState::Done)
                    && r.active.elapsed() >= after
                    && Some(r.agent.id) != self.focus
            })
            .map(|r| (r.agent.id, r.agent.name.clone()))
            .collect();
        for (id, name) in &stale {
            let _ = self.kill_agent(*id, false);
            ui::set_status(&self.w, &format!("reaped idle window {name}"));
        }
        if !stale.is_empty() {
            self.reload();
        }
    }

    /// Open the windows a project's `.taix.toml` asks for, once per run.
    fn open_startup_windows(&mut self, project: ProjectId) {
        if self.started.contains(&project) {
            return;
        }
        self.started.push(project);
        let Some(root) = self.project(project).map(|p| p.root.clone()) else {
            return;
        };
        let specs = taix_core::spawn::startup_windows(&root);
        if specs.is_empty() || self.rows.iter().any(|r| r.agent.project == project) {
            return;
        }
        for spec in &specs {
            let harness = taix_core::by_id(&self.cfg, &spec.harness);
            if let Ok(agent) = self.spawn(project, &harness)
                && let Some(name) = &spec.name
            {
                let _ = self.rename(agent, name);
            }
        }
    }

    pub fn window(&self) -> adw::ApplicationWindow {
        self.w.window.clone()
    }

    /// Id, display name, and whether a worktree would be discarded on kill.
    pub fn agent_meta(&self, id: AgentId) -> Option<(AgentId, String, bool)> {
        let row = self.row(id)?;
        Some((
            row.agent.id,
            row.agent.name.clone(),
            row.agent.worktree.is_some(),
        ))
    }
}

/// Colour a unified diff by line prefix.
///
/// The two-column `compare` view diffs two *captures*; this is git's own
/// diff, which already carries its markers - all it needs is the colour.
fn unified_markup(diff: &str) -> String {
    let mut out = String::with_capacity(diff.len() + diff.len() / 4);
    for line in diff.lines() {
        let colour = match line.as_bytes().first() {
            Some(b'+') => Some("#a6e3a1"),
            Some(b'-') => Some("#f38ba8"),
            Some(b'@') => Some("#89b4fa"),
            _ => None,
        };
        match colour {
            Some(c) => {
                out.push_str("<span foreground=\"");
                out.push_str(c);
                out.push_str("\">");
                out.push_str(&pango::escape(line));
                out.push_str("</span>");
            }
            None => out.push_str(&pango::escape(line)),
        }
        out.push('\n');
    }
    out
}

/// The last `n` lines of a capture, so a saved scrollback fits the grid it
/// is drawn into rather than scrolling the first screen off the top.
fn tail_lines(bytes: &[u8], n: usize) -> &[u8] {
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let mut seen = 0;
    for (i, b) in body.iter().enumerate().rev() {
        if *b == b'\n' {
            seen += 1;
            if seen == n {
                return &bytes[i + 1..];
            }
        }
    }
    bytes
}

/// Ask tmux for the pane's real geometry; a snapshot has no intrinsic size and
/// guessing it wraps every line at the wrong column.
fn pane_size(tmux: &Client, pane: PaneId) -> Size {
    cmd::panes(tmux)
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.id == pane)
        .map(|p| Size {
            cols: p.cols.max(1),
            rows: p.rows.max(1),
        })
        .unwrap_or(Size { cols: 80, rows: 24 })
}

/// Drives `App::tick` from the GTK main loop: one timer, re-armed after
/// every tick for exactly as long as the next piece of work can wait.
///
/// The frame rate is a ceiling, not a heartbeat. A window with output
/// landing, a divider being dragged or a snapshot due ticks at `TICK`; a
/// window with none of those wakes for the reconcile pass and nothing
/// else - two wake-ups a second where a fixed 16 ms timer was sixty. Output
/// and pointer requests call `wake` and are served the moment they arrive,
/// capped at the frame rate by `last_paint`.
struct Ticker {
    app: std::rc::Weak<RefCell<App>>,
    timer: RefCell<Option<glib::SourceId>>,
}

thread_local! {
    static TICKER: RefCell<Option<Rc<Ticker>>> = const { RefCell::new(None) };
}

impl Ticker {
    /// Tick now, then arm the timer for whenever the next work is due.
    fn run(self: &Rc<Self>) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let next = match app.try_borrow_mut() {
            Ok(mut app) => {
                app.tick();
                app.next_due()
            }
            // Something upstream holds the app; a frame later is soon enough.
            Err(_) => TICK,
        };
        self.arm(next);
    }

    fn arm(self: &Rc<Self>, after: Duration) {
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
        let ticker = self.clone();
        *self.timer.borrow_mut() = Some(glib::timeout_add_local_once(after, move || {
            // Fired, so already gone: `run` must not try to remove it.
            ticker.timer.take();
            ticker.run();
        }));
    }

    /// Something arrived. Tick if a frame has passed since the last paint,
    /// or make sure a tick is coming when one has.
    fn wake(self: &Rc<Self>) {
        let since_paint = self
            .app
            .upgrade()
            .and_then(|app| app.try_borrow().ok().map(|app| app.last_paint.elapsed()))
            .unwrap_or(TICK);
        if since_paint >= TICK {
            self.run();
        } else {
            self.arm(TICK - since_paint);
        }
    }
}

/// Ask for a tick: output landed, the pointer asked for something, a pane
/// changed size. Safe from any main-thread callback, including ones that
/// fire while the app is borrowed.
pub fn wake() {
    let ticker = TICKER.with(|t| t.borrow().clone());
    if let Some(ticker) = ticker {
        ticker.wake();
    }
}

/// The notification id for "a browser has this terminal", so raising it
/// twice replaces rather than stacks, and giving the terminal back can
/// withdraw exactly the one that is no longer true.
fn claim_tag(id: AgentId) -> String {
    format!("taix-web-claim-{id}")
}

/// Notification id for a pairing request, so raising/withdrawing is stable.
fn pair_tag(ip: &str) -> String {
    format!("taix-web-pair-{ip}")
}

pub fn start_ticking(shared: &Shared) {
    let ticker = Rc::new(Ticker {
        app: Rc::downgrade(shared),
        timer: RefCell::new(None),
    });
    TICKER.with(|t| *t.borrow_mut() = Some(ticker.clone()));
    // The reader thread cannot hold the `Rc`, so it hands the wake to the
    // main context. The bus only wakes on an empty queue, so a burst of
    // output is one wake, not one per `%output` line.
    shared.borrow().tmux.bus.set_waker(Box::new(|| {
        glib::MainContext::default().invoke_with_priority(glib::Priority::DEFAULT, wake);
    }));
    ticker.run();
}
