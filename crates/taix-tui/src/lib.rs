//! The terminal front: the same dashboard as the GTK window, in a terminal.
//!
//! Layout, scrollback, transcripts, worktrees and sessions all come from the
//! shared crates, so the two fronts cannot drift: this file is the keyboard,
//! the frame loop, and the drawing.

mod app;
mod bar;
mod compare;
mod grid;
mod overlay;
mod render;

use app::{Answer, App, Confirm, Mode, PAGE, RECONCILE_INTERVAL};
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use grid::Preset;
use overlay::{Item, Palette, Prompt, Viewer};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::collections::HashMap;
use std::io::{Write, stdout};
use std::{panic, time::Duration};
use taix_core::{AgentId, Config, Store};
use taix_term::Size as TermSize;
use taix_tmux::{Client, Ev, cmd};

/// Frame budget while output is flowing: many `%output` notifications
/// coalesce into one redraw.
const TICK: Duration = Duration::from_millis(50);
/// The wait between frames once a frame drained nothing. A key returns from
/// the poll at once either way; only the first chunk of output after a
/// silence waits this long, and after it the loop is back at `TICK`.
const IDLE_TICK: Duration = Duration::from_millis(250);
/// tmux keeps the history, so the emulator does not. Costs ~1.9 MB per pane.
const HISTORY_LIMIT: usize = 2_000;

/// RAII guard that restores the terminal on drop, even on panic.
struct TerminalGuard;

impl TerminalGuard {
    fn setup() -> Result<Self, Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    let _guard = TerminalGuard::setup()?;

    let cfg = Config::load();
    let store = Store::open(&Config::store_path())?;
    cmd::ensure_session(&cfg.tmux_socket, &cfg.tmux_session, 240, 60)?;
    let client = Client::attach(&cfg.tmux_socket, &cfg.tmux_session)?;
    if let Err(e) = cmd::set_history_limit(&client, HISTORY_LIMIT) {
        taix_core::trace!("set_history_limit failed: {e}");
    }

    let mut app = App::new(cfg, store, client);
    app.reload();

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    while !app.quit {
        let area = Rect {
            x: 0,
            y: 0,
            width: terminal.size()?.width,
            height: terminal.size()?.height,
        };
        if app.width != area.width {
            app.mark_dirty();
        }
        app.width = area.width;
        let sizes = pane_sizes(&app, area);
        let focus_size = app
            .focus
            .and_then(|id| sizes.get(&id).copied())
            .unwrap_or(TermSize { cols: 80, rows: 24 });

        // A project can be shown without anyone selecting it - `reload` picks
        // the first one - so startup windows are opened from the frame.
        if let Some(project) = app.selected {
            app.open_startup_windows(project, focus_size);
        }
        let events = app.tmux.bus.drain();
        let busy = !events.is_empty();
        for ev in events {
            match ev {
                Ev::Output { pane, data, seq } => app.observe(pane, &data, seq),
                Ev::WindowsChanged | Ev::Layout { .. } => {
                    app.reload();
                    app.reseed_focus(focus_size);
                }
                Ev::Exit => app.quit = true,
            }
        }
        // A seed is answered over the same channel the output arrives on,
        // so it is ordered against the stream and can be taken any time.
        if app
            .focus
            .and_then(|id| app.row(id))
            .is_some_and(|r| r.live.is_none())
        {
            app.reseed_focus(focus_size);
        }

        if app.last_reconcile.elapsed() > RECONCILE_INTERVAL {
            if app.store.stamp() != app.store_stamp {
                app.reload();
            }
            for (id, name, state) in app.reconcile_states() {
                alert(&mut app, id, &name, state.as_str());
            }
        }
        app.housekeeping();
        if app.last_git.elapsed() > app::GIT_INTERVAL {
            app.refresh_git();
        }
        app.autosize(&sizes);
        app.refresh_snapshots(&sizes);
        app.refresh_live();
        if app.dirty {
            terminal.draw(|f| draw(f, &app))?;
            app.dirty = false;
        }

        if event::poll(if busy { TICK } else { IDLE_TICK })?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key, focus_size);
            app.mark_dirty();
        }
    }

    Ok(())
}

/// Ring the terminal bell and say which window wants a human.
///
/// The GTK front raises a desktop notification with a Focus button; a
/// terminal front has one channel for "look at me", and using it is the
/// difference between a dashboard and a thing you have to watch.
fn alert(app: &mut App, id: AgentId, name: &str, state: &str) {
    let mut out = stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
    let index = app
        .visible()
        .iter()
        .position(|x| *x == id)
        .map(|i| format!(" ({})", i + 1))
        .unwrap_or_default();
    app.say(format!("{name}{index}: {state}"));
}

// ---------- geometry ----------

/// Split the screen the way `draw` will, and report the emulator size of
/// every visible pane.
///
/// Shared with the frame loop on purpose: a snapshot rendered at a size the
/// pane is not drawn at wraps every line at the wrong column.
fn pane_sizes(app: &App, area: Rect) -> HashMap<AgentId, TermSize> {
    let mut sizes = HashMap::new();
    let panes = layout(app, area).panes;
    let ids = app.visible();
    if ids.is_empty() {
        return sizes;
    }
    if app.zoomed {
        if let Some(id) = app.focus {
            sizes.insert(id, grid::body_size(panes));
        }
        return sizes;
    }
    for (id, cell) in ids.iter().zip(grid::areas(app.preset, panes, ids.len())) {
        sizes.insert(*id, grid::body_size(cell));
    }
    sizes
}

struct Regions {
    sidebar: Rect,
    panes: Rect,
    find: Option<Rect>,
    bar: Rect,
}

fn layout(app: &App, area: Rect) -> Regions {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let (body, bar) = (rows[0], rows[1]);
    // The find bar is a mode, so it takes a line only while it is open.
    let (body, find) = if matches!(app.mode, Mode::Find) {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(body);
        (split[0], Some(split[1]))
    } else {
        (body, None)
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(render::sidebar_width(body.width)),
            Constraint::Min(0),
        ])
        .split(body);
    Regions {
        sidebar: columns[0],
        panes: columns[1],
        find,
        bar,
    }
}

// ---------- drawing ----------

fn draw(f: &mut Frame, app: &App) {
    let regions = layout(app, f.area());
    draw_sidebar(f, app, regions.sidebar);

    let views = app.pane_views();
    if views.is_empty() {
        let hint = if app.projects.is_empty() {
            "no projects - run `taix add <path>`"
        } else {
            "no windows - press n"
        };
        f.render_widget(
            Paragraph::new(hint)
                .style(Style::default().add_modifier(Modifier::DIM))
                .block(Block::default().borders(Borders::ALL)),
            regions.panes,
        );
    } else {
        let zoomed = if app.zoomed {
            app.focused_index()
        } else {
            None
        };
        grid::draw(f, regions.panes, &views, app.preset, zoomed);
    }

    if let Some(find) = regions.find {
        overlay::draw_find(
            f,
            find,
            &app.find.needle,
            app.find.hits.len(),
            app.find.current,
        );
    }
    draw_bar(f, app, regions.bar);

    match &app.mode {
        Mode::Palette(palette) => overlay::draw_palette(f, f.area(), palette),
        Mode::Prompt(prompt, _) => overlay::draw_prompt(f, f.area(), prompt),
        Mode::Viewer(viewer) => overlay::draw_viewer(f, f.area(), viewer),
        Mode::Confirm(question, _) => {
            overlay::draw_prompt(
                f,
                f.area(),
                &Prompt::new(
                    question.clone(),
                    String::new(),
                    "y to confirm, any key cancels",
                ),
            );
        }
        _ => {}
    }
}

fn draw_bar(f: &mut Frame, app: &App, area: Rect) {
    let project = app.selected.and_then(|id| app.project(id));
    let live = app.rows.iter().filter(|r| r.live.is_some()).count();
    let hint = app
        .fresh_status()
        .map(|s| s.to_string())
        .unwrap_or_else(|| mode_hint(app).to_string());
    let data = bar::BarData {
        segments: &app.cfg.bar,
        root: project.map(|p| p.root.as_path()),
        branch: app.bar_git.as_deref(),
        windows: app.visible().len(),
        live,
        attention: app.attention(),
        ui_kib: app.mem.0,
        tmux_kib: app.mem.1,
        project_kib: app.mem.2,
        panes_kib: app.mem.3,
        uptime: app.launched.elapsed(),
        muted: app.selected.is_some_and(|p| app.muted.contains(&p)),
        hint: &hint,
    };
    bar::draw(f, area, &data);
}

fn mode_hint(app: &App) -> &'static str {
    match app.mode {
        Mode::Type => "typing - Ctrl-] to leave",
        Mode::Find => "Enter next · Esc close",
        Mode::Viewer(_) => "j/k scroll · q close",
        Mode::Palette(_) => "type to filter · Enter choose · Esc close",
        Mode::Prompt(..) => "Enter confirm · Esc cancel",
        Mode::Confirm(..) => "y to confirm",
        Mode::Normal => "? keys · Enter type · n new · p palette · q quit",
    }
}

/// One block per project, its windows listed inside.
fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for project in &app.projects {
        let selected = app.selected == Some(project.id);
        let folded = app.folded.contains(&project.id);
        let windows: Vec<&app::Row> = app
            .rows
            .iter()
            .filter(|r| r.agent.project == project.id)
            .collect();
        let attention = windows.iter().filter(|r| app.wants_attention(r)).count();
        let mut head = vec![
            Span::styled(
                if folded { "› " } else { "⌄ " },
                Style::default().add_modifier(Modifier::DIM),
            ),
            Span::styled(
                project.name.clone(),
                if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                },
            ),
            Span::styled(
                format!("  {}", windows.len()),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ];
        if app.muted.contains(&project.id) {
            head.push(Span::styled(
                " mute",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        if attention > 0 {
            head.push(Span::styled(
                format!(" !{attention}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(head));
        if folded {
            continue;
        }
        for (i, row) in windows.iter().enumerate() {
            let (state, style) = render::state_display(row.agent.state);
            let focused = app.focus == Some(row.agent.id);
            // The number is the key that focuses it, so it is only shown
            // where that key works: the selected project.
            let index = if selected && i < 9 {
                format!("{} ", i + 1)
            } else {
                "  ".to_string()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if focused { " ▸ " } else { "   " },
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(index, Style::default().add_modifier(Modifier::DIM)),
                Span::styled(
                    row.agent.name.clone(),
                    if focused {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::raw(" "),
                Span::styled(state.to_string(), style),
            ]));
        }
    }
    let title = format!("Projects {}", app.projects.len());
    f.render_widget(
        Paragraph::new(lines).block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

// ---------- keys ----------

/// Tmux key representation for pass-through typing.
#[derive(Debug, PartialEq, Eq)]
enum TmuxKey {
    /// Literal text to send with `send-keys -l`.
    Text(String),
    /// Named key for `send-keys` (e.g. "Enter", "C-c", "BSpace").
    Named(String),
    /// Exact bytes, for combinations tmux has no name for.
    Bytes(Vec<u8>),
}

/// Pure key translation for pass-through mode.
fn tmux_key(key: KeyEvent) -> Option<TmuxKey> {
    use KeyCode::*;
    use KeyModifiers as Mod;

    let mods = key.modifiers;
    let ctrl = mods.contains(Mod::CONTROL);
    let alt = mods.contains(Mod::ALT);

    // Word-delete. A terminal sends `^H` for Ctrl-Backspace and leaves the
    // meaning to the shell, where it usually deletes one character - not what
    // anyone presses it for. Send what readline and zsh bind to
    // `backward-kill-word`, exactly as the GTK front does.
    if matches!(key.code, Backspace) && (ctrl || alt) {
        return Some(TmuxKey::Bytes(vec![0x1b, 0x7f]));
    }

    if ctrl {
        if let Char(c @ ('a'..='z' | 'A'..='Z' | '[' | ']' | '\\')) = key.code {
            let lower = c.to_ascii_lowercase();
            return Some(TmuxKey::Named(format!("C-{lower}")));
        }
        if matches!(key.code, Char(' ')) {
            return Some(TmuxKey::Named("C-Space".to_string()));
        }
        // tmux names these, and an arrow with Ctrl is word-motion in every
        // shell, so it is worth forwarding rather than dropping.
        let named = match key.code {
            Left => Some("C-Left"),
            Right => Some("C-Right"),
            Up => Some("C-Up"),
            Down => Some("C-Down"),
            Delete => Some("C-DC"),
            _ => None,
        };
        return named.map(|n| TmuxKey::Named(n.to_string()));
    }

    if alt {
        if let Char(c) = key.code {
            return Some(TmuxKey::Named(format!("M-{c}")));
        }
        return None;
    }

    if mods.is_empty() || mods == Mod::SHIFT {
        match key.code {
            Char(c) => return Some(TmuxKey::Text(c.to_string())),
            Enter => return Some(TmuxKey::Named("Enter".to_string())),
            Tab => return Some(TmuxKey::Named("Tab".to_string())),
            BackTab => return Some(TmuxKey::Named("BTab".to_string())),
            Backspace => return Some(TmuxKey::Named("BSpace".to_string())),
            Esc => return Some(TmuxKey::Named("Escape".to_string())),
            Up => return Some(TmuxKey::Named("Up".to_string())),
            Down => return Some(TmuxKey::Named("Down".to_string())),
            Left => return Some(TmuxKey::Named("Left".to_string())),
            Right => return Some(TmuxKey::Named("Right".to_string())),
            Home => return Some(TmuxKey::Named("Home".to_string())),
            End => return Some(TmuxKey::Named("End".to_string())),
            PageUp => return Some(TmuxKey::Named("PPage".to_string())),
            PageDown => return Some(TmuxKey::Named("NPage".to_string())),
            Delete => return Some(TmuxKey::Named("DC".to_string())),
            Insert => return Some(TmuxKey::Named("IC".to_string())),
            F(n) => return Some(TmuxKey::Named(format!("F{n}"))),
            _ => {}
        }
    }
    None
}

/// What pass-through mode does with a keystroke.
///
/// Split from the handler so the two decisions that matter - that `q` is
/// typed rather than quitting, and that only `Ctrl-]` leaves - are testable
/// without a tmux server.
#[derive(Debug, PartialEq, Eq)]
enum Typing {
    Leave,
    Send(TmuxKey),
    Ignore,
}

fn typing_intent(key: KeyEvent) -> Typing {
    // Ctrl-] is the classic telnet escape: no shell, editor or agent TUI
    // binds it, so it is the one key that can mean "give me TaiX back".
    //
    // Measured, not assumed: crossterm 0.29 decodes the byte 0x1D by mapping
    // 0x1C..=0x1F onto '4'..='7', so `Ctrl-]` arrives as `Char('5')` with
    // CONTROL, never as `Char(']')`. Both spellings are accepted because a
    // terminal that reports the obvious one must work too.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
    {
        return Typing::Leave;
    }
    match tmux_key(key) {
        Some(k) => Typing::Send(k),
        None => Typing::Ignore,
    }
}

fn handle_key(app: &mut App, key: KeyEvent, size: TermSize) {
    // The mode is moved out so a handler can own its buffers while it also
    // mutates the app - the two used to fight over `app.mode` and a text mode
    // could not be left.
    let mode = std::mem::replace(&mut app.mode, Mode::Normal);
    let next = match mode {
        Mode::Normal => {
            normal_key(app, key, size);
            std::mem::replace(&mut app.mode, Mode::Normal)
        }
        Mode::Type => type_key(app, key, size),
        Mode::Find => find_key(app, key, size),
        Mode::Palette(palette) => palette_key(app, key, palette, size),
        Mode::Prompt(prompt, answer) => prompt_key(app, key, prompt, answer, size),
        Mode::Viewer(viewer) => viewer_key(key, viewer),
        Mode::Confirm(question, what) => confirm_key(app, key, question, what, size),
    };
    app.mode = next;
}

fn normal_key(app: &mut App, key: KeyEvent, size: TermSize) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let focus = app.focus;
    match key.code {
        KeyCode::Char('q') => app.quit = true,
        // Selection and focus are the same thing here: there is one live
        // emulator, and it belongs to whatever you are looking at.
        KeyCode::Char('j') | KeyCode::Down if !shift => app.cycle_focus(1, size),
        KeyCode::Char('k') | KeyCode::Up if !shift => app.cycle_focus(-1, size),
        KeyCode::Tab => app.cycle_focus(1, size),
        KeyCode::BackTab => app.cycle_focus(-1, size),
        KeyCode::Char('J') | KeyCode::Left => app.cycle_project(-1, size),
        KeyCode::Char('K') | KeyCode::Right => app.cycle_project(1, size),
        KeyCode::Char(c @ '1'..='9') => app.focus_nth(c as usize - '1' as usize, size),
        // Scrollback: tmux owns the history, so this is a different capture
        // rather than a viewport move.
        KeyCode::PageUp => app.scroll_focused(PAGE, size),
        KeyCode::PageDown => app.scroll_focused(-PAGE, size),
        KeyCode::Up if shift => app.scroll_focused(1, size),
        KeyCode::Down if shift => app.scroll_focused(-1, size),
        KeyCode::End => app.back_to_bottom(size),
        KeyCode::Enter => app.mode = Mode::Type,
        KeyCode::Char('c') if !ctrl => app.interrupt(),
        KeyCode::Char('n') => app.mode = Mode::Palette(new_window_palette(app)),
        KeyCode::Char('t') => {
            if let Some(project) = app.selected {
                let harness = taix_core::terminal();
                if let Err(e) = app.spawn(project, &harness, size) {
                    app.say(e);
                }
            }
        }
        KeyCode::Char('r') => {
            if let Some(id) = focus
                && let Some(row) = app.row(id)
            {
                let name = row.agent.name.clone();
                app.mode = Mode::Prompt(
                    Prompt::new("Rename window", name, "Enter to rename"),
                    Answer::Rename(id),
                );
            }
        }
        KeyCode::Char('R') => {
            if let Some(id) = focus
                && let Err(e) = app.restart(id, size)
            {
                app.say(e);
            }
        }
        KeyCode::Char('x') => {
            if let Some(id) = focus
                && let Some(row) = app.row(id)
            {
                let name = row.agent.name.clone();
                app.mode = Mode::Confirm(format!("Close {name}?"), Confirm::Close(id));
            }
        }
        // Discarding a worktree throws away uncommitted work, so it is a
        // separate key and a separate question.
        KeyCode::Char('X') => {
            if let Some(id) = focus
                && let Some(row) = app.row(id)
            {
                if row.agent.worktree.is_none() {
                    app.say("this window has no worktree");
                } else {
                    let name = row.agent.name.clone();
                    app.mode = Mode::Confirm(
                        format!("Close {name} AND delete its worktree?"),
                        Confirm::Discard(id),
                    );
                }
            }
        }
        KeyCode::Char('p') => {
            app.mode = Mode::Palette(Palette::new("Palette", app.palette_items()))
        }
        KeyCode::Char('A') => app.mode = Mode::Palette(Palette::new("Jobs", app.job_items())),
        KeyCode::Char('/') => {
            app.find.target = app.focus;
            app.mode = Mode::Find;
        }
        KeyCode::Char('f') => app.toggle_fold(),
        KeyCode::Char('z') => app.toggle_zoom(),
        KeyCode::Char('L') => app.cycle_preset(),
        KeyCode::Char('m') => app.toggle_mute(),
        KeyCode::Char('s') => {
            app.mode = Mode::Prompt(
                Prompt::new("Save session", "work", "Enter to save"),
                Answer::SaveSession,
            );
        }
        KeyCode::Char('o') => {
            let names = taix_core::session::list(&app.cfg);
            if names.is_empty() {
                app.say("no saved sessions");
            } else {
                let items = names
                    .into_iter()
                    .map(|name| Item {
                        id: format!("session:{name}"),
                        group: "Sessions",
                        title: name,
                        subtitle: "restore".to_string(),
                    })
                    .collect();
                app.mode = Mode::Palette(Palette::new("Restore session", items));
            }
        }
        KeyCode::Char('g') => match focus.map(|id| app.diff(id)) {
            Some(Ok((title, body))) => {
                app.mode = Mode::Viewer(Viewer::new(title, compare::diff_lines(&body)))
            }
            Some(Err(e)) => app.say(e),
            None => {}
        },
        KeyCode::Char('M') => {
            if let Some(id) = focus
                && let Some(row) = app.row(id)
            {
                if row.agent.worktree.is_none() {
                    app.say("this window has no worktree");
                } else {
                    let name = row.agent.name.clone();
                    app.mode = Mode::Confirm(format!("Merge {name} back?"), Confirm::Merge(id));
                }
            }
        }
        KeyCode::Char('C') => match focus.map(|id| app.compare_sides(id)) {
            Some(Ok((left_name, left, right_name, right))) => {
                let rows = compare::rows(&left, &right);
                app.mode = Mode::Viewer(Viewer::new(
                    format!("{left_name} | {right_name}"),
                    compare::lines(&rows, compare_width(app)),
                ));
            }
            Some(Err(e)) => app.say(e),
            None => {}
        },
        KeyCode::Char('T') => match focus.map(|id| app.transcript(id)) {
            Some(Ok(path)) => app.say(path.display().to_string()),
            Some(Err(e)) => app.say(e),
            None => {}
        },
        KeyCode::Char('?') => app.mode = Mode::Viewer(Viewer::new("Keys", help_lines())),
        KeyCode::Esc => app.say(""),
        _ => {}
    }
}

fn type_key(app: &mut App, key: KeyEvent, size: TermSize) -> Mode {
    match typing_intent(key) {
        Typing::Leave => return Mode::Normal,
        Typing::Ignore => {}
        Typing::Send(press) => match press {
            TmuxKey::Text(s) => app.send_text(&s, size),
            TmuxKey::Named(k) => app.send_key(&k, size),
            TmuxKey::Bytes(b) => app.send_bytes(&b, size),
        },
    }
    Mode::Type
}

fn find_key(app: &mut App, key: KeyEvent, size: TermSize) -> Mode {
    match key.code {
        KeyCode::Esc => {
            app.close_find(size);
            return Mode::Normal;
        }
        KeyCode::Enter | KeyCode::Down => app.find_step(1),
        KeyCode::Up => app.find_step(-1),
        KeyCode::Backspace => {
            let mut needle = app.find.needle.clone();
            needle.pop();
            app.search(&needle);
        }
        KeyCode::Char(c) => {
            let needle = format!("{}{c}", app.find.needle);
            app.search(&needle);
        }
        _ => {}
    }
    Mode::Find
}

fn new_window_palette(app: &App) -> Palette {
    let items = taix_core::available(&app.cfg)
        .into_iter()
        .map(|h| Item {
            id: format!("spawn:{}", h.id),
            group: "New window",
            title: h.label.clone(),
            subtitle: h.command.clone(),
        })
        .collect();
    Palette::new("New window", items)
}

fn palette_key(app: &mut App, key: KeyEvent, mut palette: Palette, size: TermSize) -> Mode {
    match key.code {
        KeyCode::Esc => return Mode::Normal,
        KeyCode::Up => palette.step(-1),
        KeyCode::Down => palette.step(1),
        KeyCode::Backspace => palette.backspace(),
        KeyCode::Enter => {
            let Some(id) = palette.selected().map(|i| i.id.clone()) else {
                return Mode::Normal;
            };
            return act(app, &id, size);
        }
        KeyCode::Char(c) => palette.push(c),
        _ => {}
    }
    Mode::Palette(palette)
}

/// Run a palette item. The ids are the GTK front's, so a habit transfers.
fn act(app: &mut App, id: &str, size: TermSize) -> Mode {
    let Some((kind, rest)) = id.split_once(':') else {
        return Mode::Normal;
    };
    match (kind, rest) {
        ("project", rest) => {
            if let Ok(project) = rest.parse() {
                app.select_project(project, size);
            }
        }
        ("window", rest) => {
            if let Ok(agent) = rest.parse::<AgentId>() {
                // Focusing a window in another project has to switch to it,
                // or the palette would appear to do nothing.
                if let Some(project) = app.row(agent).map(|r| r.agent.project) {
                    app.select_project(project, size);
                }
                app.set_focus(agent, size);
            }
        }
        ("job", rest) => {
            let Some((verb, id)) = rest.split_once(':') else {
                return Mode::Normal;
            };
            let Ok(id) = id.parse::<taix_core::JobId>() else {
                return Mode::Normal;
            };
            let Ok(Some(job)) = app.store.job(id) else {
                app.say("that job is gone");
                return Mode::Normal;
            };
            match verb {
                "run" => {
                    app.jobs.request(id);
                    app.say(format!("running {}", job.name));
                }
                "toggle" => {
                    let on = !job.enabled;
                    match app.store.set_job_enabled(id, on) {
                        Ok(()) => {
                            app.reload();
                            app.say(format!("{} is {}", job.name, if on { "on" } else { "off" }));
                        }
                        Err(e) => app.say(e.to_string()),
                    }
                }
                _ => {}
            }
        }
        ("spawn", harness) => {
            if let Some(project) = app.selected {
                let harness = taix_core::by_id(&app.cfg, harness);
                if let Err(e) = app.spawn(project, &harness, size) {
                    app.say(e);
                }
            }
        }
        ("preset", rest) => {
            if let Some(preset) = Preset::from_id(rest) {
                app.preset = preset;
                app.say(format!("layout: {}", preset.label()));
            }
        }
        ("session", name) => {
            if let Err(e) = app.restore_session(name, size) {
                app.say(e);
            }
        }
        ("action", "new-terminal") => {
            if let Some(project) = app.selected {
                let harness = taix_core::terminal();
                if let Err(e) = app.spawn(project, &harness, size) {
                    app.say(e);
                }
            }
        }
        ("action", "find") => {
            app.find.target = app.focus;
            return Mode::Find;
        }
        ("action", "zoom") => app.toggle_zoom(),
        ("action", "mute") => app.toggle_mute(),
        ("action", "save-session") => {
            return Mode::Prompt(
                Prompt::new("Save session", "work", "Enter to save"),
                Answer::SaveSession,
            );
        }
        ("action", "help") => return Mode::Viewer(Viewer::new("Keys", help_lines())),
        ("action", "diff") => match app.focus.map(|id| app.diff(id)) {
            Some(Ok((title, body))) => {
                return Mode::Viewer(Viewer::new(title, compare::diff_lines(&body)));
            }
            Some(Err(e)) => app.say(e),
            None => {}
        },
        ("action", "merge") => {
            if let Some(id) = app.focus {
                let name = app
                    .row(id)
                    .map(|r| r.agent.name.clone())
                    .unwrap_or_default();
                return Mode::Confirm(format!("Merge {name} back?"), Confirm::Merge(id));
            }
        }
        ("action", "compare") => match app.focus.map(|id| app.compare_sides(id)) {
            Some(Ok((left_name, left, right_name, right))) => {
                let rows = compare::rows(&left, &right);
                return Mode::Viewer(Viewer::new(
                    format!("{left_name} | {right_name}"),
                    compare::lines(&rows, compare_width(app)),
                ));
            }
            Some(Err(e)) => app.say(e),
            None => {}
        },
        ("action", "transcript") => match app.focus.map(|id| app.transcript(id)) {
            Some(Ok(path)) => app.say(path.display().to_string()),
            Some(Err(e)) => app.say(e),
            None => {}
        },
        _ => {}
    }
    Mode::Normal
}

/// Inner width of the viewer modal, which is 90% of the screen minus its
/// border - the two columns have to be sized to what they are drawn in.
fn compare_width(app: &App) -> u16 {
    (app.width * 9 / 10).saturating_sub(2).max(20)
}

fn prompt_key(
    app: &mut App,
    key: KeyEvent,
    mut prompt: Prompt,
    answer: Answer,
    size: TermSize,
) -> Mode {
    match key.code {
        KeyCode::Esc => return Mode::Normal,
        KeyCode::Backspace => prompt.backspace(),
        KeyCode::Char(c) => prompt.push(c),
        KeyCode::Enter => {
            let value = prompt.value.trim().to_string();
            if value.is_empty() {
                return Mode::Normal;
            }
            let result = match answer {
                Answer::Rename(id) => app.rename(id, &value),
                Answer::SaveSession => app.save_session(&value),
            };
            if let Err(e) = result {
                app.say(e);
            }
            let _ = size;
            return Mode::Normal;
        }
        _ => {}
    }
    Mode::Prompt(prompt, answer)
}

fn viewer_key(key: KeyEvent, mut viewer: Viewer) -> Mode {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return Mode::Normal,
        KeyCode::Char('j') | KeyCode::Down => viewer.step(1, 1),
        KeyCode::Char('k') | KeyCode::Up => viewer.step(-1, 1),
        KeyCode::PageDown | KeyCode::Char(' ') => viewer.step(1, PAGE as u16),
        KeyCode::PageUp => viewer.step(-1, PAGE as u16),
        _ => {}
    }
    Mode::Viewer(viewer)
}

fn confirm_key(
    app: &mut App,
    key: KeyEvent,
    question: String,
    what: Confirm,
    size: TermSize,
) -> Mode {
    // Anything other than an explicit `y` cancels: these kill a process or
    // move commits, so a stray keypress must not confirm one.
    if key.code != KeyCode::Char('y') {
        let _ = question;
        return Mode::Normal;
    }
    let result = match what {
        Confirm::Close(id) => app.close(id, false),
        Confirm::Discard(id) => app.close(id, true),
        Confirm::Merge(id) => app.merge(id).map(|msg| app.say(msg)),
    };
    match result {
        Ok(()) => {}
        Err(e) => app.say(e),
    }
    app.reseed_focus(size);
    Mode::Normal
}

fn help_lines() -> Vec<Line<'static>> {
    const KEYS: &[(&str, &str)] = &[
        ("j / k, Tab", "focus the next / previous window"),
        ("J / K, ← / →", "previous / next project"),
        ("1 - 9", "focus the nth window of this project"),
        ("A", "scheduled jobs: run one, switch one off"),
        ("Enter", "type into the focused pane (Ctrl-] leaves)"),
        ("c", "send Ctrl-C to the focused pane"),
        ("n / t", "new window from a harness / plain terminal"),
        ("r / R", "rename / restart the focused window"),
        ("x / X", "close it / close it and delete its worktree"),
        ("p", "command palette"),
        ("/", "find in the pane's whole history"),
        ("PgUp / PgDn", "scroll the pane through tmux's history"),
        ("Shift-↑ / ↓", "scroll it one line"),
        ("End", "back to live output"),
        ("z / L", "zoom the focused pane / cycle layout preset"),
        ("f", "fold this project in the sidebar"),
        ("m", "mute this project's alerts"),
        ("s / o", "save a session / open a saved one"),
        ("g / M", "show this window's diff / merge it back"),
        ("C", "compare this pane with the previous one"),
        ("T", "show the transcript path"),
        ("q", "quit"),
    ];
    KEYS.iter()
        .map(|(k, what)| {
            Line::from(vec![
                Span::styled(
                    format!("{k:>14}  "),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(*what),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn tmux_key_translates_plain_char() {
        assert_eq!(
            tmux_key(key(KeyCode::Char('a'))),
            Some(TmuxKey::Text("a".to_string()))
        );
        assert_eq!(
            tmux_key(key(KeyCode::Char('Q'))),
            Some(TmuxKey::Text("Q".to_string()))
        );
    }

    #[test]
    fn tmux_key_translates_control_sequences() {
        assert_eq!(
            tmux_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(TmuxKey::Named("C-c".to_string()))
        );
        assert_eq!(
            tmux_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL)),
            Some(TmuxKey::Named("C-c".to_string()))
        );
    }

    #[test]
    fn control_backspace_deletes_a_word() {
        // tmux has no name for this one and types an unrecognised name out as
        // literal text, so it must go as bytes - the same bytes the GTK front
        // sends.
        assert_eq!(
            tmux_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL)),
            Some(TmuxKey::Bytes(vec![0x1b, 0x7f]))
        );
        assert_eq!(
            tmux_key(key(KeyCode::Backspace)),
            Some(TmuxKey::Named("BSpace".to_string()))
        );
    }

    #[test]
    fn only_ctrl_bracket_leaves_pass_through() {
        // The whole point of the mode: `q` is typed, not a quit command, and
        // Esc belongs to whatever is running in the pane.
        assert_eq!(
            typing_intent(key(KeyCode::Char('q'))),
            Typing::Send(TmuxKey::Text("q".to_string()))
        );
        assert_eq!(
            typing_intent(key(KeyCode::Esc)),
            Typing::Send(TmuxKey::Named("Escape".to_string()))
        );
        assert_eq!(
            typing_intent(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL)),
            Typing::Leave
        );
        // What crossterm 0.29 actually delivers for Ctrl-]; matching only
        // `']'` left the mode impossible to leave.
        assert_eq!(
            typing_intent(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::CONTROL)),
            Typing::Leave
        );
    }

    #[test]
    fn tmux_key_translates_special_keys() {
        assert_eq!(
            tmux_key(key(KeyCode::PageUp)),
            Some(TmuxKey::Named("PPage".to_string()))
        );
        assert_eq!(
            tmux_key(key(KeyCode::F(5))),
            Some(TmuxKey::Named("F5".to_string()))
        );
        assert_eq!(
            tmux_key(key(KeyCode::Enter)),
            Some(TmuxKey::Named("Enter".to_string()))
        );
    }

    #[test]
    fn help_covers_every_normal_mode_key() {
        // The help text is the only discoverability a TUI has, so an added
        // key must appear in it.
        let text: String = help_lines()
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        for key in [
            "j / k", "1 - 9", "A", "Enter", "n / t", "r / R", "x / X", "p", "/", "PgUp", "End",
            "z / L", "f", "b", "m", "s / o", "g / M", "C", "T", "q",
        ] {
            assert!(text.contains(key), "help is missing {key}");
        }
    }

    #[test]
    fn dirty_flag_controls_rendering() {
        use taix_core::{Config, Store};
        use taix_tmux::Client;

        let cfg = Config::load();
        let store = Store::open_memory().unwrap();
        let socket = format!("/tmp/taix-tui-test-{}", std::process::id());
        let session = "test";
        let _ = std::process::Command::new("tmux")
            .args(["-L", &socket, "new-session", "-d", "-s", session])
            .output();

        let client = match Client::attach(&socket, session) {
            Ok(c) => c,
            Err(_) => return,
        };

        let mut app = App::new(cfg, store, client);

        assert!(app.dirty, "app starts dirty");

        app.dirty = false;
        assert!(!app.dirty, "dirty cleared");

        app.mark_dirty();
        assert!(app.dirty, "mark_dirty sets dirty");

        app.dirty = false;
        app.say("test");
        assert!(app.dirty, "say marks dirty");

        app.dirty = false;
        app.toggle_zoom();
        assert!(app.dirty, "toggle_zoom marks dirty");

        let _ = std::process::Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
    }
}
