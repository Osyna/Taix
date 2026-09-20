//! TUI state and the actions the key handler fires.
//!
//! The same model as the GTK front - one row per window, one live emulator on
//! the focused pane, everything else redrawn from `capture-pane` - because
//! both fronts sit on the same tmux server and the same store, and a
//! dashboard that disagrees with the other front is worse than no dashboard.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::text::Line;
use taix_core::{Agent, AgentId, AgentState, Config, Project, ProjectId, Store, Watcher, log::Log};
use taix_term::{Live, Size as TermSize};
use taix_tmux::{Client, PaneId, PaneInfo, cmd};

use crate::grid::{PaneView, Preset};
use crate::overlay::{Item, Palette, Prompt, Viewer};

/// Unfocused panes cost one `capture-pane` fork per refresh, so they are
/// deliberately slower than the focused pane.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(400);
/// How often derived states are written back to the store.
pub const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);
/// Reads `/proc` for a whole process tree; far slower than the frame rate.
pub const MEM_INTERVAL: Duration = Duration::from_millis(2000);
/// One `git` invocation per worktree window.
pub const GIT_INTERVAL: Duration = Duration::from_millis(4000);
/// How long a toast stays before the bar reverts to its key hint.
pub const STATUS_TTL: Duration = Duration::from_millis(4000);
/// Scrollback step. tmux owns the history, so a page is a fixed number of
/// lines rather than a viewport height: predictable beats exact.
pub const PAGE: i32 = 20;
/// How much of a diff is worth putting in a viewer. Past this you want a
/// pager, not a modal.
const DIFF_CAP: usize = 200_000;

/// One window: its record, its rendering, and what we have learned about it.
pub struct Row {
    pub agent: Agent,
    pub watcher: Watcher,
    /// Only the focused pane keeps an emulator; the rest are snapshots.
    pub live: Option<Live>,
    pub lines: Vec<Line<'static>>,
    pub last_snapshot: Instant,
    /// Lines above the live bottom. 0 means live.
    pub scroll: usize,
    pub git: Option<String>,
    pub log: Option<Log>,
    /// Last time this pane produced output, for the idle reaper.
    pub active: Instant,
    pub notified_state: Option<AgentState>,
    pub applied_size: Option<TermSize>,
    /// Output events stamped at or below this were already on the screen
    /// the emulator was seeded from, so feeding them would print them twice.
    pub seed_seq: u64,
}

/// What the keyboard is doing. A mode owns the keyboard until it is left.
pub enum Mode {
    Normal,
    /// Keystrokes go to the pane; `Ctrl-]` returns.
    Type,
    Palette(Palette),
    Prompt(Prompt, Answer),
    Viewer(Viewer),
    Find,
    Confirm(String, Confirm),
}

/// What a prompt's text is for once it is submitted.
pub enum Answer {
    Rename(AgentId),
    SaveSession,
}

/// What a yes/no question will do.
pub enum Confirm {
    Close(AgentId),
    Discard(AgentId),
    Merge(AgentId),
}

pub struct App {
    pub cfg: Config,
    pub store: Store,
    pub tmux: Client,
    /// Fixed for the server's life, so asked once.
    pub server_pid: Option<u32>,
    /// The store file as `reload` last saw it, so a `taix add` from another
    /// terminal shows up on the next pass: one `stat`, no load.
    pub store_stamp: Option<(std::time::SystemTime, u64)>,
    /// Opened repositories by project root; see `repo`.
    pub repos: HashMap<PathBuf, taix_git::Repo>,
    /// The selected project's branch and drift, read on the git cadence:
    /// the bar used to fork `git` twice per frame for it.
    pub bar_git: Option<String>,
    pub projects: Vec<Project>,
    pub rows: Vec<Row>,
    pub selected: Option<ProjectId>,
    pub focus: Option<AgentId>,
    /// The pane focused before this one, which is what "compare" compares
    /// against: choosing a card focuses it, so the interesting other side is
    /// always the previous one.
    pub previous: Option<AgentId>,
    pub mode: Mode,
    pub preset: Preset,
    pub zoomed: bool,
    pub muted: Vec<ProjectId>,
    pub folded: Vec<ProjectId>,
    pub find: Find,
    pub status: String,
    pub status_at: Instant,
    /// Projects whose `.taix.toml` startup windows have been opened; once per
    /// run, not once per reload.
    pub started: Vec<ProjectId>,
    pub launched: Instant,
    pub last_reconcile: Instant,
    pub last_mem: Instant,
    pub last_git: Instant,
    /// (ui, tmux server, the selected project's panes, every pane) in KiB.
    pub mem: (u64, u64, u64, u64),
    /// Fires due jobs while this front is the open window; the runner is a
    /// child process, never work on this thread.
    pub jobs: taix_core::jobs::Ticker,
    /// Terminal width of the last frame, so a modal can size its columns.
    pub width: u16,
    pub quit: bool,
    /// Frame needs rebuild: any state changed since the last draw.
    pub dirty: bool,
}

/// Find state. `hits` are indices into the pane's whole history, oldest
/// first, so jumping to one is a scrollback move rather than a cursor move.
#[derive(Default)]
pub struct Find {
    pub target: Option<AgentId>,
    pub needle: String,
    pub hits: Vec<usize>,
    pub current: usize,
}

impl App {
    pub fn new(cfg: Config, store: Store, tmux: Client) -> Self {
        let server_pid = cmd::server(&tmux).ok().map(|(pid, _)| pid);
        App {
            cfg,
            store,
            tmux,
            server_pid,
            store_stamp: None,
            repos: HashMap::new(),
            bar_git: None,
            projects: Vec::new(),
            rows: Vec::new(),
            selected: None,
            focus: None,
            previous: None,
            mode: Mode::Normal,
            preset: Preset::Balanced,
            zoomed: false,
            muted: Vec::new(),
            folded: Vec::new(),
            find: Find::default(),
            status: String::new(),
            status_at: Instant::now(),
            started: Vec::new(),
            launched: Instant::now(),
            // Zero means "never measured", so the first pass reads them.
            last_reconcile: Instant::now() - RECONCILE_INTERVAL,
            last_mem: Instant::now() - MEM_INTERVAL,
            last_git: Instant::now() - GIT_INTERVAL,
            mem: (0, 0, 0, 0),
            jobs: taix_core::jobs::Ticker::default(),
            width: 80,
            quit: false,
            dirty: true,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn say(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_at = Instant::now();
        self.mark_dirty();
    }

    /// The toast, while it is fresh. After that the bar shows its key hint.
    pub fn fresh_status(&self) -> Option<&str> {
        (!self.status.is_empty() && self.status_at.elapsed() < STATUS_TTL)
            .then_some(self.status.as_str())
    }

    // ---------- loading ----------

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
        // One listing for the whole pass. Unknown is not empty: a failed
        // listing must not close every window and empty the store.
        let Ok(panes) = cmd::panes(&self.tmux) else {
            return;
        };
        let Ok(agents) = self
            .store
            .reconcile(|pane| panes.iter().any(|p| p.id == pane))
        else {
            return;
        };

        let mut kept: Vec<Row> = Vec::with_capacity(agents.len());
        for agent in agents {
            match self.rows.iter().position(|r| r.agent.id == agent.id) {
                Some(i) => {
                    let mut row = self.rows.swap_remove(i);
                    row.agent = agent;
                    kept.push(row);
                }
                None => {
                    // Seed the watcher from what is on screen: a row built
                    // after a restart has no output history, so a quiet
                    // window could never have a state derived again and the
                    // idle reaper would never see it.
                    let mut watcher = Watcher::new(idle_after);
                    if let Some(pane) = agent.pane {
                        watcher.observe(&cmd::capture(&self.tmux, pane), Instant::now());
                    }
                    kept.push(Row {
                        agent,
                        watcher,
                        live: None,
                        lines: Vec::new(),
                        last_snapshot: Instant::now() - SNAPSHOT_INTERVAL,
                        scroll: 0,
                        git: None,
                        log: None,
                        active: Instant::now(),
                        notified_state: None,
                        applied_size: None,
                        seed_seq: 0,
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
        if self.focus.is_none_or(|id| self.row(id).is_none()) {
            self.focus = self.visible().first().copied();
        }
        self.mark_dirty();
    }

    // ---------- lookups ----------

    pub fn row(&self, id: AgentId) -> Option<&Row> {
        self.rows.iter().find(|r| r.agent.id == id)
    }

    pub fn row_mut(&mut self, id: AgentId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.agent.id == id)
    }

    pub fn row_by_pane(&mut self, pane: PaneId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.agent.pane == Some(pane))
    }

    pub fn project(&self, id: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// The windows of the selected project, in id order.
    pub fn visible(&self) -> Vec<AgentId> {
        self.rows
            .iter()
            .filter(|r| Some(r.agent.project) == self.selected)
            .map(|r| r.agent.id)
            .collect()
    }

    pub fn focused_pane(&self) -> Option<PaneId> {
        self.focus
            .and_then(|id| self.row(id))
            .and_then(|r| r.agent.pane)
    }

    /// A state that a human must act on. Attention is for *agents*: a plain
    /// terminal at a shell prompt is technically waiting for input, and
    /// counting it floods the bar.
    pub fn wants_attention(&self, row: &Row) -> bool {
        row.agent.state.needs_attention() && row.agent.kind != taix_core::terminal().id
    }

    pub fn attention(&self) -> usize {
        self.rows.iter().filter(|r| self.wants_attention(r)).count()
    }

    // ---------- focus ----------

    pub fn set_focus(&mut self, id: AgentId, size: TermSize) {
        if self.focus == Some(id) {
            return;
        }
        if let Some(prev) = self.focus.take() {
            self.previous = Some(prev);
            if let Some(row) = self.row_mut(prev) {
                row.live = None;
                row.scroll = 0;
            }
        }
        self.focus = Some(id);
        // No capture here: the emulator is built by the frame loop, once the
        // pending %output has been drained. Seeding first fed the same bytes
        // twice and the pane showed its last line duplicated.
        let _ = size;
        if let Some(row) = self.row_mut(id) {
            row.live = None;
        }
        self.mark_dirty();
    }

    /// Build the focused pane's emulator at the pane's current size.
    ///
    /// An emulator's grid is fixed at construction, so a stale one wraps
    /// every line at the old width.
    pub fn reseed_focus(&mut self, size: TermSize) {
        let Some(id) = self.focus else { return };
        let Some(pane) = self.row(id).and_then(|r| r.agent.pane) else {
            if let Some(row) = self.row_mut(id) {
                row.live = None;
            }
            return;
        };
        let Some(seed) = cmd::seed(&self.tmux, pane) else {
            return;
        };
        if let Some(row) = self.row_mut(id) {
            let mut live = Live::new(size);
            live.seed(&seed.screen, seed.cursor);
            row.live = Some(live);
            row.seed_seq = seed.seq;
        }
        self.mark_dirty();
    }

    pub fn cycle_focus(&mut self, delta: i32, size: TermSize) {
        let ids = self.visible();
        if ids.is_empty() {
            return;
        }
        let at = self
            .focus
            .and_then(|id| ids.iter().position(|x| *x == id))
            .unwrap_or(0) as i32;
        let next = (at + delta).rem_euclid(ids.len() as i32) as usize;
        self.set_focus(ids[next], size);
    }

    /// Focus the nth window of this project, ignoring an index that does not
    /// exist rather than wrapping.
    pub fn focus_nth(&mut self, index: usize, size: TermSize) {
        if let Some(id) = self.visible().get(index).copied() {
            self.set_focus(id, size);
        }
    }

    pub fn cycle_project(&mut self, delta: i32, size: TermSize) {
        if self.projects.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|id| self.projects.iter().position(|p| p.id == id))
            .unwrap_or(0) as i32;
        let next = (at + delta).rem_euclid(self.projects.len() as i32) as usize;
        self.select_project(self.projects[next].id, size);
    }

    pub fn select_project(&mut self, project: ProjectId, size: TermSize) {
        if self.selected == Some(project) {
            return;
        }
        self.selected = Some(project);
        self.zoomed = false;
        if let Some(prev) = self.focus.take()
            && let Some(row) = self.row_mut(prev)
        {
            row.live = None;
        }
        self.focus = self.visible().first().copied();
        if let Some(id) = self.focus
            && let Some(row) = self.row_mut(id)
        {
            row.live = None;
        }
        let _ = size;
        self.mark_dirty();
    }

    // ---------- typing ----------

    /// Typing returns the pane to the live bottom, the way it does in every
    /// terminal: you cannot type usefully into history.
    fn back_to_live(&mut self, size: TermSize) {
        let Some(id) = self.focus else { return };
        let scrolled = self.row(id).is_some_and(|r| r.scroll != 0);
        if scrolled {
            if let Some(row) = self.row_mut(id) {
                row.scroll = 0;
            }
            self.reseed_focus(size);
        }
    }

    pub fn send_key(&mut self, key: &str, size: TermSize) {
        self.back_to_live(size);
        if let Some(pane) = self.focused_pane()
            && let Err(e) = cmd::send_key(&self.tmux, pane, key)
        {
            taix_core::trace!("send_key failed: {e}");
        }
    }

    pub fn send_text(&mut self, text: &str, size: TermSize) {
        if text.is_empty() {
            return;
        }
        self.back_to_live(size);
        if let Some(pane) = self.focused_pane()
            && let Err(e) = cmd::send_text(&self.tmux, pane, text)
        {
            taix_core::trace!("send_text failed: {e}");
        }
    }

    /// Exact bytes, for combinations tmux has no name for: `send-keys` types
    /// an unrecognised name out as literal text.
    pub fn send_bytes(&mut self, bytes: &[u8], size: TermSize) {
        self.back_to_live(size);
        if let Some(pane) = self.focused_pane()
            && let Err(e) = cmd::send_bytes(&self.tmux, pane, bytes)
        {
            taix_core::trace!("send_bytes failed: {e}");
        }
    }

    pub fn interrupt(&mut self) {
        if let Some(pane) = self.focused_pane()
            && let Err(e) = cmd::send_key(&self.tmux, pane, "C-c")
        {
            taix_core::trace!("interrupt failed: {e}");
        }
    }

    // ---------- scrollback ----------

    /// Move the focused pane through tmux's history.
    ///
    /// The emulator only holds the live screen, so this is a different
    /// capture rather than a viewport move.
    pub fn scroll_focused(&mut self, delta: i32, size: TermSize) {
        let Some(id) = self.focus else { return };
        let Some(pane) = self.row(id).and_then(|r| r.agent.pane) else {
            return;
        };
        let history = cmd::history_size(&self.tmux, pane);
        let Some(row) = self.row_mut(id) else { return };
        let next = (row.scroll as i32 + delta).clamp(0, history as i32) as usize;
        if next == row.scroll {
            return;
        }
        row.scroll = next;
        if next == 0 {
            self.reseed_focus(size);
        }
        self.mark_dirty();
    }

    pub fn back_to_bottom(&mut self, size: TermSize) {
        self.back_to_live(size);
    }

    // ---------- windows ----------

    pub fn spawn(
        &mut self,
        project: ProjectId,
        harness: &taix_core::Harness,
        size: TermSize,
    ) -> Result<AgentId, String> {
        let proj = self.project(project).ok_or("unknown project")?.clone();
        let existing: Vec<Agent> = self
            .rows
            .iter()
            .filter(|r| r.agent.project == project)
            .map(|r| r.agent.clone())
            .collect();
        let spawned =
            taix_core::spawn::window(&self.store, &self.cfg, &proj, harness, &existing, None)?;
        let id = spawned.agent.id;
        self.selected = Some(project);
        self.reload();
        self.set_focus(id, size);
        Ok(id)
    }

    pub fn rename(&mut self, id: AgentId, name: &str) -> Result<(), String> {
        self.store
            .rename_agent(id, name)
            .map_err(|e| e.to_string())?;
        self.reload();
        Ok(())
    }

    /// Close a window. `discard_worktree` removes the worktree, which throws
    /// away uncommitted work, so the caller must have asked.
    pub fn close(&mut self, id: AgentId, discard_worktree: bool) -> Result<(), String> {
        let Some(row) = self.row(id) else {
            return Ok(());
        };
        let pane = row.agent.pane;
        let worktree = row.agent.worktree.clone();
        let root = self.project(row.agent.project).map(|p| p.root.clone());
        if let Some(pane) = pane
            && let Err(e) = cmd::kill_pane(&self.tmux, pane)
        {
            taix_core::trace!("kill_pane failed: {e}");
        }
        if discard_worktree
            && let (Some(worktree), Some(root)) = (worktree, root)
            && let Ok(repo) = taix_git::Repo::open(&root)
        {
            repo.remove_worktree(&worktree, true)
                .map_err(|e| e.to_string())?;
        }
        self.store.remove_agent(id).map_err(|e| e.to_string())?;
        if self.focus == Some(id) {
            self.focus = None;
        }
        if self.previous == Some(id) {
            self.previous = None;
        }
        self.reload();
        Ok(())
    }

    /// Relaunch the harness in a fresh window and drop the old one.
    pub fn restart(&mut self, id: AgentId, size: TermSize) -> Result<(), String> {
        let Some(row) = self.row(id) else {
            return Ok(());
        };
        let project = row.agent.project;
        let harness = taix_core::by_id(&self.cfg, &row.agent.kind);
        self.close(id, false)?;
        self.spawn(project, &harness, size)?;
        Ok(())
    }

    // ---------- worktrees ----------

    fn worktree_of(&self, id: AgentId) -> Option<(PathBuf, PathBuf, String)> {
        let row = self.row(id)?;
        let worktree = row.agent.worktree.clone()?;
        let root = self.project(row.agent.project)?.root.clone();
        Some((root, worktree, row.agent.name.clone()))
    }

    /// The diff of a worktree window against the project's default branch.
    pub fn diff(&self, id: AgentId) -> Result<(String, String), String> {
        let (root, worktree, name) = self
            .worktree_of(id)
            .ok_or("no worktree for this window".to_string())?;
        let repo = taix_git::Repo::open(&root).map_err(|e| e.to_string())?;
        let base = repo.default_branch().map_err(|e| e.to_string())?;
        let diff = repo
            .diff_text(&worktree, &base, DIFF_CAP)
            .map_err(|e| e.to_string())?;
        let body = if diff.trim().is_empty() {
            format!("no changes against {base}")
        } else {
            diff
        };
        Ok((format!("{name} against {base}"), body))
    }

    /// Merge a worktree's branch back. Git's own refusal - dirty worktree,
    /// dirty base, conflict - is what the user needs to read, so it is
    /// returned verbatim.
    pub fn merge(&mut self, id: AgentId) -> Result<String, String> {
        let (root, worktree, name) = self
            .worktree_of(id)
            .ok_or("no worktree for this window".to_string())?;
        let repo = taix_git::Repo::open(&root).map_err(|e| e.to_string())?;
        let base = repo.default_branch().map_err(|e| e.to_string())?;
        repo.merge_back(&worktree, &base)
            .map_err(|e| e.to_string())?;
        Ok(format!("merged {name} into {base}"))
    }

    /// Re-read the branch and change count for every worktree window, so
    /// isolation is visible rather than implied.
    pub fn refresh_git(&mut self) {
        self.last_git = Instant::now();
        self.bar_git = self
            .selected
            .and_then(|id| self.project(id))
            .and_then(|p| taix_git::status(&p.root).ok())
            .map(|s| s.line());
        let targets: Vec<(AgentId, PathBuf, PathBuf)> = self
            .rows
            .iter()
            .filter_map(|r| {
                let worktree = r.agent.worktree.clone()?;
                let root = self.project(r.agent.project)?.root.clone();
                Some((r.agent.id, root, worktree))
            })
            .collect();
        for (id, root, worktree) in targets {
            let label = self
                .repo(&root)
                .and_then(|repo| repo.worktree_line(&worktree).ok());
            if let Some(row) = self.row_mut(id) {
                row.git = label;
            }
        }
        self.mark_dirty();
    }

    /// The repository at `root`, opened once and kept: opening is a `git`
    /// process, and the base branch it remembers is several more.
    fn repo(&mut self, root: &Path) -> Option<&taix_git::Repo> {
        if !self.repos.contains_key(root) {
            self.repos
                .insert(root.to_path_buf(), taix_git::Repo::open(root).ok()?);
        }
        self.repos.get(root)
    }

    // ---------- housekeeping ----------

    /// The slow lane: process memory, dead panes, harness adoption, reaping.
    /// One pane listing over the channel serves every step; an `Err` is
    /// unknown, not empty, and the pass waits for the next one.
    pub fn housekeeping(&mut self) {
        if self.last_mem.elapsed() < MEM_INTERVAL {
            return;
        }
        self.last_mem = Instant::now();
        // Before the pane listing: a tmux hiccup must not stall the
        // scheduler, which does not need tmux to run a shell job.
        if self.jobs.tick() {
            self.reload();
        }
        let Ok(panes) = cmd::panes(&self.tmux) else {
            return;
        };
        // One walk of the pane trees serves memory and harness detection.
        let roots: Vec<u32> = self
            .server_pid
            .into_iter()
            .chain(panes.iter().map(|p| p.pid))
            .collect();
        let table = taix_core::mem::ProcTable::read(&roots);
        self.mem = self.measure(&panes, &table);
        self.reap_dead_panes(&panes);
        self.adopt_running_harnesses(&panes, &table);
        self.reap_idle();
    }

    /// PSS, not RSS: RSS would count the libraries TaiX and tmux share and
    /// report a session as costing far more than it does.
    fn measure(
        &self,
        panes: &[PaneInfo],
        table: &taix_core::mem::ProcTable,
    ) -> (u64, u64, u64, u64) {
        let own = taix_core::mem::pss_kib(std::process::id()).unwrap_or(0);
        let server = self
            .server_pid
            .and_then(taix_core::mem::pss_kib)
            .unwrap_or(0);
        let pids: Vec<(taix_core::ProjectId, u32)> = self
            .rows
            .iter()
            .filter_map(|r| r.agent.pane.map(|pane| (r.agent.project, pane)))
            .filter_map(|(project, pane)| {
                panes
                    .iter()
                    .find(|p| p.id == pane)
                    .map(|p| (project, p.pid))
            })
            .collect();
        let all: Vec<u32> = pids.iter().map(|(_, pid)| *pid).collect();
        let mine: Vec<u32> = pids
            .iter()
            .filter(|(project, _)| Some(*project) == self.selected)
            .map(|(_, pid)| *pid)
            .collect();
        let panes_kib = taix_core::mem::tree_pss_kib(&all, table);
        // The panes are the server's children: charging both doubles them.
        (
            own,
            server.saturating_sub(panes_kib),
            taix_core::mem::tree_pss_kib(&mine, table),
            panes_kib,
        )
    }

    /// Notice panes that died without tmux telling us: the notification can
    /// arrive before the pane is actually reaped.
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

    /// Rename windows after whatever is actually running in them: a terminal
    /// in which the user then runs `claude` is a Claude Code window. A name
    /// the user typed is never overwritten.
    fn adopt_running_harnesses(&mut self, panes: &[PaneInfo], table: &taix_core::mem::ProcTable) {
        let candidates: Vec<(AgentId, PaneId)> = self
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
            // A pane died between listing and reading; try again next pass
            // rather than pairing the wrong pid with the wrong window.
            return;
        }
        let found = taix_core::running_harnesses(&self.cfg, &roots, table);
        let mut changed = false;
        for ((id, _), harness) in candidates.iter().zip(found) {
            let Some(row) = self.row(*id) else { continue };
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
            let _ = self.close(*id, false);
            self.say(format!("reaped idle window {name}"));
        }
    }

    /// Open the windows a project's `.taix.toml` asks for, once per run.
    pub fn open_startup_windows(&mut self, project: ProjectId, size: TermSize) {
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
            if let Ok(id) = self.spawn(project, &harness, size)
                && let Some(name) = &spec.name
            {
                let _ = self.rename(id, name);
            }
        }
    }

    /// Push derived states into the store. Returns the windows that just
    /// started needing a human, for the caller to alert on: only transitions
    /// alert, so a long Waiting does not ring the bell forever.
    pub fn reconcile_states(&mut self) -> Vec<(AgentId, String, AgentState)> {
        self.last_reconcile = Instant::now();
        let now = Instant::now();
        // A failed listing is unknown, not empty: it must not turn every
        // window Done.
        let Ok(panes) = cmd::panes(&self.tmux) else {
            return Vec::new();
        };
        let terminal = taix_core::terminal().id;
        let mut alerts = Vec::new();
        let mut updates: Vec<(AgentId, AgentState)> = Vec::new();

        for row in &mut self.rows {
            let derived = match row.agent.pane {
                Some(pane) if panes.iter().any(|p| p.id == pane) => row.watcher.state(now),
                Some(_) => Some(row.watcher.finished()),
                None => None,
            };
            // A plain terminal sitting at a shell prompt is technically
            // waiting for input - that is what a prompt is - so collapse it
            // here, where state is derived, instead of filtering it
            // everywhere state is read.
            let derived = derived.map(|state| {
                if state.needs_attention() && row.agent.kind == terminal {
                    AgentState::Idle
                } else {
                    state
                }
            });
            let Some(state) = derived else { continue };
            if state == row.agent.state {
                continue;
            }
            row.agent.state = state;
            updates.push((row.agent.id, state));
            let wants = state.needs_attention() && row.agent.kind != terminal;
            if wants && row.notified_state != Some(state) {
                alerts.push((row.agent.id, row.agent.name.clone(), state));
                row.notified_state = Some(state);
            } else if !wants {
                row.notified_state = None;
            }
        }
        for (id, state) in updates {
            let _ = self.store.set_state(id, state);
        }
        alerts.retain(|(id, _, _)| {
            self.row(*id)
                .is_some_and(|r| !self.muted.contains(&r.agent.project))
        });
        alerts
    }

    // ---------- find ----------

    /// Search the focused pane's whole history, not just the visible screen.
    pub fn search(&mut self, needle: &str) {
        self.find.needle = needle.to_string();
        self.find.current = 0;
        self.find.hits.clear();
        self.find.target = self.focus;
        if needle.is_empty() {
            return;
        }
        let Some(pane) = self.focused_pane() else {
            return;
        };
        let text = cmd::history_text(&self.tmux, pane).unwrap_or_default();
        // Smartcase: a lowercase needle matches anything, a needle with an
        // uppercase letter means the user meant it.
        let sensitive = needle.chars().any(char::is_uppercase);
        for (i, line) in text.lines().enumerate() {
            let hit = if sensitive {
                line.contains(needle)
            } else {
                line.to_lowercase().contains(&needle.to_lowercase())
            };
            if hit {
                self.find.hits.push(i);
            }
        }
        self.jump_to_hit();
        self.mark_dirty();
    }

    pub fn find_step(&mut self, delta: i32) {
        if self.find.hits.is_empty() {
            return;
        }
        let len = self.find.hits.len() as i32;
        self.find.current = ((self.find.current as i32 + delta).rem_euclid(len)) as usize;
        self.jump_to_hit();
        self.mark_dirty();
    }

    /// Put the current hit into view, a third of a screen down so there is
    /// context above it - a match pinned to the last row tells you nothing
    /// about what led to it.
    fn jump_to_hit(&mut self) {
        let Some(&line) = self.find.hits.get(self.find.current) else {
            return;
        };
        let Some(id) = self.find.target else { return };
        let Some(row) = self.row(id) else { return };
        let Some(pane) = row.agent.pane else { return };
        let rows = row.applied_size.map_or(24, |s| s.rows);
        let history = cmd::history_size(&self.tmux, pane);
        // A hit's line counts from the oldest line of history, so the number
        // of lines between it and the live bottom is what we scroll by.
        let above = history.saturating_sub(line);
        let scroll = above.saturating_add(rows / 3).min(history);
        if let Some(row) = self.row_mut(id) {
            row.scroll = scroll;
        }
        self.mark_dirty();
    }

    pub fn close_find(&mut self, size: TermSize) {
        self.find.hits.clear();
        self.find.needle.clear();
        self.back_to_live(size);
        self.mark_dirty();
    }

    // ---------- modes and toggles ----------

    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
        self.mark_dirty();
    }

    pub fn toggle_mute(&mut self) {
        let Some(project) = self.selected else { return };
        let name = self
            .project(project)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        if let Some(i) = self.muted.iter().position(|p| *p == project) {
            self.muted.remove(i);
            self.say(format!("{name}: unmuted"));
        } else {
            self.muted.push(project);
            self.say(format!("{name}: muted"));
        }
    }

    pub fn toggle_fold(&mut self) {
        let Some(project) = self.selected else { return };
        if let Some(i) = self.folded.iter().position(|p| *p == project) {
            self.folded.remove(i);
        } else {
            self.folded.push(project);
        }
        self.mark_dirty();
    }

    pub fn cycle_preset(&mut self) {
        let at = Preset::ALL
            .iter()
            .position(|p| *p == self.preset)
            .unwrap_or(0);
        self.preset = Preset::ALL[(at + 1) % Preset::ALL.len()];
        self.say(format!("layout: {}", self.preset.label()));
    }

    // ---------- sessions ----------

    pub fn save_session(&mut self, name: &str) -> Result<(), String> {
        let snap = taix_core::spawn::snapshot(&self.store, name)?;
        let path = taix_core::session::save(&self.cfg, &snap).map_err(|e| e.to_string())?;
        self.say(format!("saved {}", path.display()));
        Ok(())
    }

    pub fn restore_session(&mut self, name: &str, size: TermSize) -> Result<(), String> {
        let snap = taix_core::session::load(&self.cfg, name).map_err(|e| e.to_string())?;
        let opened = taix_core::spawn::restore(&self.store, &self.cfg, &snap)?;
        self.reload();
        let _ = size;
        self.say(format!("{name}: {opened} windows"));
        Ok(())
    }

    // ---------- palette ----------

    /// Everything reachable by name: projects, windows, harnesses that are
    /// installed, layouts, saved sessions, and the actions that are
    /// otherwise a keystroke away.
    pub fn palette_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for project in &self.projects {
            items.push(Item {
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
            items.push(Item {
                id: format!("window:{}", row.agent.id),
                group: "Windows",
                title: row.agent.name.clone(),
                subtitle: format!("{project} · {}", row.agent.state.as_str()),
            });
        }
        if self.selected.is_some() {
            for harness in taix_core::available(&self.cfg) {
                if harness.is_terminal() {
                    continue;
                }
                items.push(Item {
                    id: format!("spawn:{}", harness.id),
                    group: "Start here",
                    title: harness.label.clone(),
                    subtitle: harness.command.clone(),
                });
            }
        }
        for preset in Preset::ALL {
            items.push(Item {
                id: format!("preset:{}", preset.id()),
                group: "Layout",
                title: preset.label().to_string(),
                subtitle: String::new(),
            });
        }
        for name in taix_core::session::list(&self.cfg) {
            items.push(Item {
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
            ("action:diff", "Show this window's diff"),
            ("action:merge", "Merge this window's branch back"),
            ("action:compare", "Compare with the previous pane"),
            ("action:transcript", "Show the transcript path"),
            ("action:help", "Keys"),
        ] {
            items.push(Item {
                id: id.to_string(),
                group: "Actions",
                title: title.to_string(),
                subtitle: String::new(),
            });
        }
        items
    }

    /// The jobs palette: run and enable/disable, which are the operational
    /// verbs. Creating and editing one is `taix jobs add` or the GUI.
    pub fn job_items(&self) -> Vec<Item> {
        let now = taix_core::jobs::now();
        let mut items = Vec::new();
        for job in self.store.jobs().unwrap_or_default() {
            let when = match taix_core::jobs::parse(&job.schedule) {
                Err(e) => format!("bad schedule: {e}"),
                Ok(_) if !job.enabled => "off".to_string(),
                Ok(schedule) => schedule
                    .next_after(job.last_run.unwrap_or(job.created))
                    .map_or("never".to_string(), |t| {
                        format!("next {}", taix_core::jobs::format_local(t.max(now), now))
                    }),
            };
            let last = match job.last() {
                Some(run) if run.error.is_some() || run.exit.is_some_and(|c| c != 0) => {
                    "last failed"
                }
                Some(_) => "last ok",
                None => "never run",
            };
            let subtitle = format!("{} · {when} · {last}", job.schedule);
            items.push(Item {
                id: format!("job:run:{}", job.id),
                group: "Jobs",
                title: format!("{} — run now", job.name),
                subtitle: subtitle.clone(),
            });
            items.push(Item {
                id: format!("job:toggle:{}", job.id),
                group: "Jobs",
                title: format!(
                    "{} — {}",
                    job.name,
                    if job.enabled { "disable" } else { "enable" }
                ),
                subtitle,
            });
        }
        items
    }

    // ---------- transcripts ----------

    /// The transcript file for a window, once it has produced output.
    pub fn transcript(&self, id: AgentId) -> Result<PathBuf, String> {
        self.row(id)
            .and_then(|r| r.log.as_ref().map(|l| l.path().to_path_buf()))
            .ok_or_else(|| "this window has produced no output yet".to_string())
    }

    /// Plain text of two panes, for the comparison view. The left side is the
    /// given window, the right side the pane focused before it.
    pub fn compare_sides(&self, id: AgentId) -> Result<(String, String, String, String), String> {
        let other = match self.focus {
            Some(focus) if focus != id => Some(focus),
            _ => self.previous,
        };
        let other = other.ok_or("focus another pane first, then compare".to_string())?;
        if other == id {
            return Err("that is the same pane".into());
        }
        let text = |target: AgentId| -> Result<(String, String), String> {
            let row = self.row(target).ok_or("window is gone".to_string())?;
            let pane = row.agent.pane.ok_or("window has no pane".to_string())?;
            let body = cmd::capture_text(&self.tmux, pane, 200).map_err(|e| e.to_string())?;
            Ok((row.agent.name.clone(), body))
        };
        let (left_name, left) = text(id)?;
        let (right_name, right) = text(other)?;
        Ok((left_name, left, right_name, right))
    }

    // ---------- drawing input ----------

    /// Rows of the selected project, as the grid wants them.
    pub fn pane_views(&self) -> Vec<PaneView> {
        self.visible()
            .into_iter()
            .filter_map(|id| self.row(id))
            .map(|row| PaneView {
                name: row.agent.name.clone(),
                harness: taix_core::by_id(&self.cfg, &row.agent.kind).label,
                state: row.agent.state,
                focused: self.focus == Some(row.agent.id),
                scroll: row.scroll,
                badge: row.git.clone(),
                lines: row.lines.clone(),
            })
            .collect()
    }

    /// Index of the focused pane among the visible ones, for zoom.
    pub fn focused_index(&self) -> Option<usize> {
        let ids = self.visible();
        self.focus.and_then(|id| ids.iter().position(|x| *x == id))
    }

    pub fn open_log(&mut self, id: AgentId) {
        let Some(row) = self.row(id) else { return };
        if row.log.is_some() {
            return;
        }
        let Some(project) = self
            .project(row.agent.project)
            .map(|p| p.name.clone())
            .filter(|_| true)
        else {
            return;
        };
        let name = row.agent.name.clone();
        let log = Log::open(&self.cfg, &project, &name).ok();
        if let Some(row) = self.row_mut(id) {
            row.log = log;
        }
    }

    /// Watchers, transcripts and the idle clock, fed by one `%output`.
    pub fn observe(&mut self, pane: PaneId, data: &[u8], seq: u64) {
        let now = Instant::now();
        let id = self.row_by_pane(pane).map(|r| r.agent.id);
        let Some(id) = id else { return };
        self.open_log(id);
        if let Some(row) = self.row_mut(id) {
            row.watcher.observe(data, now);
            row.active = now;
            if let Some(log) = &mut row.log
                && let Err(e) = log.append(data)
            {
                taix_core::trace!("log append failed: {e}");
            }
            // The seed's screen already shows anything stamped before it.
            if seq > row.seed_seq
                && let Some(live) = &mut row.live
            {
                live.feed(data);
            }
        }
        self.mark_dirty();
    }

    /// Re-render every pane that is not the focused emulator, on the slow
    /// cadence: each one costs a `capture-pane` fork.
    pub fn refresh_snapshots(&mut self, sizes: &HashMap<AgentId, TermSize>) {
        let now = Instant::now();
        for id in self.visible() {
            let Some(row) = self.row(id) else { continue };
            let Some(pane) = row.agent.pane else { continue };
            let size = sizes
                .get(&id)
                .copied()
                .unwrap_or(TermSize { cols: 80, rows: 24 });
            let scrolled = row.scroll > 0;
            let is_focus = self.focus == Some(id) && !scrolled;
            if is_focus {
                continue;
            }
            if !scrolled && now.duration_since(row.last_snapshot) < SNAPSHOT_INTERVAL {
                continue;
            }
            // Scrolled back: the emulator only holds the live screen, so the
            // view comes from tmux's history instead.
            let captured = if scrolled {
                let up = row.scroll as i32;
                cmd::capture_range(&self.tmux, pane, -up, size.rows as i32 - 1 - up, true)
            } else {
                cmd::capture(&self.tmux, pane)
            };
            let mut lines = Vec::new();
            let mut spans = Vec::new();
            taix_term::snapshot(size, &captured, &mut |chunk| match chunk {
                taix_term::Chunk::Text(text, style) => {
                    spans.push(crate::render::to_owned_span(text, style))
                }
                taix_term::Chunk::LineBreak => lines.push(Line::from(std::mem::take(&mut spans))),
            });
            if !spans.is_empty() {
                lines.push(Line::from(spans));
            }
            if let Some(row) = self.row_mut(id) {
                row.lines = lines;
                row.last_snapshot = now;
                self.mark_dirty();
            }
        }
    }

    /// Render the focused emulator into its row's lines.
    pub fn refresh_live(&mut self) {
        let Some(id) = self.focus else { return };
        let Some(row) = self.row_mut(id) else { return };
        if row.scroll > 0 {
            return;
        }
        let Some(live) = &row.live else { return };
        let mut lines = Vec::new();
        let mut spans = Vec::new();
        live.render(&mut |chunk| match chunk {
            taix_term::Chunk::Text(text, style) => {
                spans.push(crate::render::to_owned_span(text, style))
            }
            taix_term::Chunk::LineBreak => lines.push(Line::from(std::mem::take(&mut spans))),
        });
        if !spans.is_empty() {
            lines.push(Line::from(spans));
        }
        row.lines = lines;
        self.mark_dirty();
    }

    /// Keep tmux's idea of each window's size equal to the cell it is drawn
    /// in, and rebuild an emulator whose grid no longer matches.
    pub fn autosize(&mut self, sizes: &HashMap<AgentId, TermSize>) {
        let mut reseed = false;
        for (id, size) in sizes {
            let Some(row) = self.row(*id) else { continue };
            if row.applied_size == Some(*size) {
                continue;
            }
            let Some(window) = row.agent.window else {
                continue;
            };
            if let Err(e) = cmd::resize_window(&self.tmux, window, size.cols, size.rows) {
                taix_core::trace!("resize_window failed: {e}");
            }
            if let Some(row) = self.row_mut(*id) {
                row.applied_size = Some(*size);
            }
            if self.focus == Some(*id) {
                reseed = true;
            }
        }
        if reseed
            && let Some(id) = self.focus
            && let Some(size) = sizes.get(&id).copied()
        {
            self.reseed_focus(size);
        }
    }
}
