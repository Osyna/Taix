//! The Automation dialog: scheduled jobs, with a face.
//!
//! The settings dialog's shell, filled with jobs: a sidebar, cards that open
//! into their own settings, and no OK button - every control writes through
//! on change, or on Enter for the text fields, where a half-typed command
//! must never be saved and fired.
//!
//! It holds its own `Store` handle rather than borrowing `App`. Every
//! mutation is `flock`ed, so a second handle on the same file is safe and
//! cheap, and a dialog callback that reached into `App` would hit the
//! re-entrancy trap the pointer queue exists for.
//!
//! Nothing here runs a job. "Run now" asks the app, which spawns
//! `taix jobs run <id>` as a child, so a job behaves the same whether it was
//! fired by hand, by the open window, or by the systemd timer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use taix_core::jobs as core;
use taix_core::{Action, Config, Job, JobId, NewJob, Notify, ProjectId, Run, Store};

use crate::icons;
use crate::kit::{self, badge, group, picker, row, row_live, stepper, text, toggle};

/// The sidebar, in order.
const NAV: [(&str, &str, bool); 3] = [
    ("jobs", "Jobs", true),
    ("history", "History", false),
    ("background", "Background", false),
];

const JOBS_SUB: &str = "What TaiX does on a clock, and what it did last time";
const HISTORY_SUB: &str = "Every run TaiX remembers, newest first";
const BACKGROUND_SUB: &str = "Running jobs while no TaiX window is open";

/// What the "When" row offers, and what each means. `Custom…` is the escape
/// hatch: it leaves the expression alone.
const PRESETS: [(&str, &str); 8] = [
    ("Every 5 minutes", "@every 5m"),
    ("Every 15 minutes", "@every 15m"),
    ("Hourly", "@hourly"),
    ("Every weekday at 09:00", "0 9 * * 1-5"),
    ("Daily at 09:00", "0 9 * * *"),
    ("Weekly, Monday 09:00", "0 9 * * 1"),
    ("Monthly, the 1st at 09:00", "0 9 1 * *"),
    ("Custom…", ""),
];

const SCHEDULE_HELP: &str = "minute hour day month weekday — each field takes *, */n, a-b \
                             or a,b. Numbers only: no month or day names. Shorthands: \
                             @hourly, @daily, @weekly, @monthly, @every 15m.";

const ACTIONS: [(&str, Action); 4] = [
    ("Run a command", Action::Shell),
    ("Ask an agent", Action::Agent),
    ("Restore a session", Action::Session),
    ("Sync with git", Action::Git),
];

/// The git operations a job may run, in the order the picker lists them.
const GIT_OPS: [(&str, &str); 3] = [
    ("Fetch", "fetch"),
    ("Pull (fast-forward only)", "pull"),
    ("Push", "push"),
];

const NOTIFIES: [(&str, Notify); 3] = [
    ("Never", Notify::Never),
    ("When it fails", Notify::Failure),
    ("Every run", Notify::Always),
];

/// What a "New job" starts life as. Disabled on purpose: a half-written
/// command must never fire because the dialog was left open.
const TEMPLATES: [(&str, &str, Action, &str); 4] = [
    (
        "Run a command",
        "Nightly backup",
        Action::Shell,
        "0 2 * * *",
    ),
    (
        "Ask an agent",
        "Morning triage",
        Action::Agent,
        "0 9 * * 1-5",
    ),
    (
        "Restore a session",
        "Open my desk",
        Action::Session,
        "0 9 * * 1-5",
    ),
    (
        "Sync with git",
        "Fetch everything",
        Action::Git,
        "@every 30m",
    ),
];

/// How long the "Run now" button watches the store for the child process it
/// asked for. A shell job answers in a moment; an agent job waits on a
/// prompt and can take most of this.
const WATCH_SECS: u32 = 30;

/// The handles every row needs: where to save, who to tell, how to complain.
struct Editor {
    cfg: Config,
    store: Rc<Store>,
    toasts: adw::ToastOverlay,
    parent: adw::ApplicationWindow,
    changed: Box<dyn Fn()>,
    run: Box<dyn Fn(JobId)>,
}

impl Editor {
    /// Write a definition back. A store that will not take it is a toast,
    /// never a silent loss.
    fn save(&self, job: &Job) {
        if let Err(e) = self.store.put_job(job) {
            self.complain(&format!("Cannot save job: {e}"));
            return;
        }
        (self.changed)();
    }

    fn complain(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }
}

/// Open the dialog over `parent`. `changed` runs after every saved edit;
/// `run` hands a "Run now" to the app, which owns child processes.
pub fn open(
    parent: &adw::ApplicationWindow,
    cfg: Config,
    store: Rc<Store>,
    changed: impl Fn() + 'static,
    run: impl Fn(JobId) + 'static,
) {
    let shell = kit::shell(
        "Automation",
        "Automation",
        &kit::short_path(&Config::store_path().display().to_string()),
        "Jobs live in the store beside your projects, not in config.toml.",
        "saved automatically",
        &NAV,
    );
    let ed = Rc::new(Editor {
        cfg,
        store,
        toasts: shell.toasts.clone(),
        parent: parent.clone(),
        changed: Box::new(changed),
        run: Box::new(run),
    });

    let filter = gtk::SearchEntry::builder()
        .placeholder_text("Filter jobs")
        .width_chars(16)
        .valign(gtk::Align::Center)
        .css_classes(["set-filter"])
        .build();
    shell.head_slot.append(&filter);

    let (jobs_scroller, jobs_page) = kit::page();
    let stats = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    jobs_page.append(&stats);
    jobs_page.append(&list);
    let pane = Rc::new(JobsPane {
        ed: ed.clone(),
        stats,
        list,
        count: shell
            .count("jobs")
            .cloned()
            .unwrap_or_else(|| gtk::Label::new(None)),
        filter: RefCell::new(String::new()),
        open: Cell::new(None),
    });
    pane.rebuild(&pane);
    filter.connect_search_changed({
        let pane = pane.clone();
        move |entry| {
            *pane.filter.borrow_mut() = entry.text().to_string();
            pane.rebuild(&pane);
        }
    });

    let (history_scroller, history_page) = kit::page();
    let history = Rc::new(HistoryPane {
        ed: ed.clone(),
        page: history_page,
    });
    history.rebuild();

    let (background_scroller, background_page) = kit::page();
    background_page.append(&background(&ed));

    shell.stack.add_named(&jobs_scroller, Some("jobs"));
    shell.stack.add_named(&history_scroller, Some("history"));
    shell
        .stack
        .add_named(&background_scroller, Some("background"));

    let (h1, h2, stack) = (shell.h1.clone(), shell.h2.clone(), shell.stack.clone());
    shell.nav.connect_row_selected(move |_, selected| {
        let Some(selected) = selected else { return };
        let (id, label, sub) = match NAV[selected.index().max(0) as usize].0 {
            "history" => ("history", "History", HISTORY_SUB),
            "background" => ("background", "Background", BACKGROUND_SUB),
            _ => ("jobs", "Jobs", JOBS_SUB),
        };
        // The history is read from the store on the way in: a job may have
        // run while the dialog sat open on another page.
        if id == "history" {
            history.rebuild();
        }
        stack.set_visible_child_name(id);
        h1.set_label(label);
        h2.set_label(sub);
        filter.set_visible(id == "jobs");
    });
    shell.nav.select_row(shell.nav.row_at_index(0).as_ref());
    shell.dialog.present(Some(parent));
}

// Jobs ---------------------------------------------------------------------

/// The Jobs page, rebuilt in place whenever the list, the filter or a job's
/// own summary changes.
struct JobsPane {
    ed: Rc<Editor>,
    stats: gtk::Box,
    list: gtk::Box,
    count: gtk::Label,
    filter: RefCell<String>,
    /// The card that was open before a rebuild, so deleting one job does not
    /// fold the one being edited.
    open: Cell<Option<JobId>>,
}

impl JobsPane {
    fn rebuild(&self, this: &Rc<JobsPane>) {
        let jobs = self.ed.store.jobs().unwrap_or_default();
        let now = core::now();
        let on = jobs.iter().filter(|j| j.enabled).count();
        let failing = jobs.iter().filter(|j| j.failing()).count();
        let next = core::next_due_at(&jobs, now)
            .map(|at| core::format_local(at, now))
            .unwrap_or_else(|| "—".to_string());

        clear(&self.stats);
        self.stats.append(&kit::stats(&[
            (on.to_string(), "On"),
            (next, "Next run"),
            (failing.to_string(), "Failing"),
        ]));
        self.count.set_label(&format!("{on}/{}", jobs.len()));

        let needle = self.filter.borrow().trim().to_string();
        let shown: Vec<Job> = jobs.into_iter().filter(|j| matches(j, &needle)).collect();

        clear(&self.list);
        let add = new_job_button(this);
        let group = group("Scheduled", Some(shown.len()), Some(add.upcast_ref()));
        if shown.is_empty() {
            group.append(&note(if needle.is_empty() {
                "Nothing scheduled yet. \"New job\" starts one, switched off until you are happy with it."
            } else {
                "No job matches that."
            }));
        }
        for job in shown {
            let id = job.id;
            let card = job_card(this, job);
            if self.open.get() == Some(id) {
                open_card(&card, true);
            }
            group.append(&card);
        }
        self.list.append(&group);
    }
}

/// A job matches what was typed in the filter box. Case is folded here, on
/// both sides, so no caller has to remember to do it.
fn matches(job: &Job, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {}",
        job.name, job.schedule, job.command, job.harness, job.session
    );
    haystack.to_lowercase().contains(&needle)
}

/// "New job", with the four kinds behind it so the first click already picks
/// what the job is for rather than leaving an empty shell command.
fn new_job_button(pane: &Rc<JobsPane>) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .child(&gtk::Label::new(Some("New job")))
        .valign(gtk::Align::Center)
        .css_classes(["set-chip-add"])
        .build();
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    for (label, name, action, schedule) in TEMPLATES {
        let item = gtk::Button::builder()
            .label(label)
            .css_classes(["set-pick-item"])
            .build();
        item.connect_clicked({
            let (pane, popover) = (pane.clone(), popover.clone());
            move |_| {
                popover.popdown();
                let mut new = NewJob {
                    name: name.to_string(),
                    enabled: false,
                    project: None,
                    schedule: schedule.to_string(),
                    action,
                };
                if action == Action::Agent {
                    // A job with no agent chosen cannot be saved, so the
                    // first installed one is a better start than nothing.
                    new.project = pane
                        .ed
                        .store
                        .projects()
                        .unwrap_or_default()
                        .first()
                        .map(|p| p.id);
                }
                let id = match pane.ed.store.add_job(&new) {
                    Ok(job) => job.id,
                    Err(e) => {
                        pane.ed.complain(&format!("Cannot add a job: {e}"));
                        return;
                    }
                };
                if action == Action::Git
                    && let Ok(Some(mut job)) = pane.ed.store.job(id)
                {
                    job.command = "fetch".to_string();
                    pane.ed.save(&job);
                }
                (pane.ed.changed)();
                pane.filter.borrow_mut().clear();
                pane.open.set(Some(id));
                pane.rebuild(&pane);
            }
        });
        menu.append(&item);
    }
    popover.set_child(Some(&menu));
    button.set_popover(Some(&popover));
    button
}

/// The head of a card: everything about a job that is readable without
/// opening it, and the one place that redraws when an edit lands.
struct Head {
    card: gtk::Box,
    icon: gtk::Image,
    name: gtk::Label,
    meta: gtk::Label,
    state: gtk::Label,
}

impl Head {
    fn show(&self, cfg: &Config, job: &Job) {
        self.name.set_label(&job.name);
        self.meta.set_label(&meta_line(job));
        icons::set(&self.icon, action_icon(cfg, job).as_deref());
        let (text, class) = state_badge(job);
        self.state.set_label(text);
        for c in ["ready", "failed", "waiting"] {
            self.state.remove_css_class(c);
        }
        if let Some(c) = class {
            self.state.add_css_class(c);
        }
        if job.enabled {
            self.card.remove_css_class("dim");
        } else {
            self.card.add_css_class("dim");
        }
    }
}

/// One row's write path: mutate the draft, save it, redraw the head.
struct Commit {
    ed: Rc<Editor>,
    draft: Rc<RefCell<Job>>,
    head: Rc<Head>,
}

impl Commit {
    fn apply(&self, edit: impl FnOnce(&mut Job)) {
        let mut job = self.draft.borrow_mut();
        edit(&mut job);
        // Say what is missing rather than saving something that cannot run:
        // the store takes it either way, and the card would read as ready.
        if let Err(why) = job.validate() {
            self.ed.complain(&format!("{}: {why}", job.name));
        }
        self.ed.save(&job);
        self.head.show(&self.ed.cfg, &job);
    }
}

/// The line under a job's name: when it next runs, and how it went last.
fn meta_line(job: &Job) -> String {
    let now = core::now();
    let mut out = match core::parse(&job.schedule) {
        Err(e) => return format!("{} · {e}", job.schedule),
        Ok(schedule) => {
            let next = schedule
                .next_after(job.last_run.unwrap_or(job.created))
                .map_or("never".to_string(), |t| core::format_local(t.max(now), now));
            format!("{} · next {next}", job.schedule)
        }
    };
    if let Some(run) = job.last() {
        out.push_str(&format!(" · last {}", run_line(run, now)));
    }
    out
}

/// One run, as a person reads it: when, how it ended, how long it took.
fn run_line(run: &Run, now: i64) -> String {
    let when = core::format_local(run.at, now);
    let took = duration(run.ms);
    match (&run.error, run.exit) {
        (Some(e), _) => format!("{when} · {e}"),
        (None, Some(0)) => format!("{when} · ok · {took}"),
        (None, Some(code)) => format!("{when} · exit {code} · {took}"),
        // An action that is not a process has no exit code of its own.
        (None, None) => format!("{when} · ok · {took}"),
    }
}

fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        1000..=59_999 => format!("{:.1}s", ms as f64 / 1000.0),
        _ => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    }
}

/// The pill at the end of a card: the one word that says what this job is
/// doing to you today.
fn state_badge(job: &Job) -> (&'static str, Option<&'static str>) {
    if !job.enabled {
        return ("off", None);
    }
    if core::parse(&job.schedule).is_err() || job.validate().is_err() {
        return ("broken", Some("failed"));
    }
    if job.failing() {
        return ("failed", Some("failed"));
    }
    if job.last().is_none() {
        return ("waiting", Some("waiting"));
    }
    ("ok", Some("ready"))
}

/// The glyph on a job's card. `icons::set` looks names up in TaiX's own
/// bundle - `taix-<name>-symbolic` - so a desktop icon name here would draw
/// a broken-image box, which is what it did until this was fixed.
fn action_icon(cfg: &Config, job: &Job) -> Option<String> {
    match job.action {
        Action::Agent => taix_core::by_id(cfg, &job.harness).icon,
        Action::Shell => Some("terminal".to_string()),
        Action::Session => Some("star".to_string()),
        Action::Git => Some("bolt".to_string()),
    }
}

fn job_card(pane: &Rc<JobsPane>, job: Job) -> gtk::Box {
    let ed = pane.ed.clone();
    let id = job.id;
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["set-card"])
        .build();

    let icon = icons::image(action_icon(&ed.cfg, &job).as_deref(), 16);
    icon.add_css_class("harness-icon");
    let icon_box = gtk::Box::builder()
        .css_classes(["set-card-icon"])
        .valign(gtk::Align::Center)
        .build();
    icon_box.append(&icon);

    let name = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["set-card-name"])
        .build();
    let meta = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["set-card-meta", "mono"])
        .build();
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    titles.set_hexpand(true);
    titles.set_valign(gtk::Align::Center);
    titles.append(&name);
    titles.append(&meta);

    let state = badge("", None);
    let head = Rc::new(Head {
        card: card.clone(),
        icon,
        name,
        meta,
        state: state.clone(),
    });
    head.show(&ed.cfg, &job);

    let sparks = sparkline(&job.runs);
    let enabled = toggle(job.enabled, {
        let (ed, head) = (ed.clone(), head.clone());
        move |on| {
            if let Err(e) = ed.store.set_job_enabled(id, on) {
                ed.complain(&format!("Cannot save job: {e}"));
                return;
            }
            (ed.changed)();
            if let Ok(Some(fresh)) = ed.store.job(id) {
                head.show(&ed.cfg, &fresh);
            }
        }
    });
    enabled.set_tooltip_text(Some("Run this job on its schedule"));

    let chevron = gtk::Image::from_icon_name("pan-end-symbolic");
    chevron.add_css_class("set-card-chevron");
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.append(&icon_box);
    line.append(&titles);
    line.append(&sparks);
    line.append(&state);
    line.append(&chevron);
    // The switch sits outside the button: a click on it must not also fold
    // the card open.
    let head_button = gtk::Button::builder()
        .child(&line)
        .hexpand(true)
        .css_classes(["set-card-head"])
        .build();
    let head_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    head_line.append(&head_button);
    head_line.append(&enabled);
    head_line.add_css_class("set-card-line");

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["set-card-body"])
        .build();
    let revealer = gtk::Revealer::builder()
        .child(&body)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .build();
    head_button.connect_clicked({
        let (revealer, chevron, pane) = (revealer.clone(), chevron.clone(), pane.clone());
        move |_| {
            let open = !revealer.reveals_child();
            revealer.set_reveal_child(open);
            chevron.set_icon_name(Some(if open {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            }));
            pane.open.set(open.then_some(id));
        }
    });
    card.append(&head_line);
    card.append(&revealer);

    let draft = Rc::new(RefCell::new(job));
    let commit = Rc::new(Commit {
        ed: ed.clone(),
        draft: draft.clone(),
        head: head.clone(),
    });

    // Name ------------------------------------------------------------------
    body.append(&row(
        "Name",
        "What this job is called, in menus and notifications",
        &text(&draft.borrow().name, 22, "", {
            let commit = commit.clone();
            move |t| commit.apply(|job| job.name = t.clone())
        }),
    ));

    // When ------------------------------------------------------------------
    let hint = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["set-row-sub", "mono"])
        .label(schedule_hint(&draft.borrow().schedule))
        .build();
    let schedule = text(&draft.borrow().schedule, 18, SCHEDULE_HELP, {
        let (commit, hint) = (commit.clone(), hint.clone());
        move |t| {
            hint.set_label(&schedule_hint(&t));
            commit.apply(|job| job.schedule = t.clone());
        }
    });
    let preset_labels: Vec<String> = PRESETS.iter().map(|(l, _)| l.to_string()).collect();
    let when = picker(
        &preset_labels,
        preset_index(&draft.borrow().schedule),
        |_| None,
        {
            let (commit, hint, schedule) = (commit.clone(), hint.clone(), schedule.clone());
            move |i| {
                let expr = PRESETS[i].1;
                if expr.is_empty() {
                    return;
                }
                schedule.set_text(expr);
                hint.set_label(&schedule_hint(expr));
                commit.apply(|job| job.schedule = expr.to_string());
            }
        },
    );
    body.append(&row(
        "When",
        "A starting point; the expression below is what actually runs",
        &when,
    ));
    body.append(&row_live("Schedule", Some(&hint), &schedule));

    // Project ---------------------------------------------------------------
    let mut project_ids: Vec<Option<ProjectId>> = vec![None];
    let mut project_labels = vec!["Every project".to_string()];
    for p in ed.store.projects().unwrap_or_default() {
        project_ids.push(Some(p.id));
        project_labels.push(p.name);
    }
    let at = project_ids
        .iter()
        .position(|p| *p == draft.borrow().project)
        .unwrap_or(0);
    body.append(&row(
        "Project",
        "Where a command runs and which project an agent opens in; git syncs them all",
        &picker(&project_labels, at, |_| None, {
            let commit = commit.clone();
            move |i| {
                let chosen = project_ids.get(i).copied().flatten();
                commit.apply(|job| job.project = chosen);
            }
        }),
    ));

    // Action ----------------------------------------------------------------
    let action_labels: Vec<String> = ACTIONS.iter().map(|(l, _)| l.to_string()).collect();
    let at = ACTIONS
        .iter()
        .position(|(_, a)| *a == draft.borrow().action)
        .unwrap_or(0);

    // Shell.
    let command = row(
        "Command",
        "Run with sh -c. {project}, {root}, {date} and {time} are expanded",
        &text(&draft.borrow().command, 28, "", {
            let commit = commit.clone();
            move |t| commit.apply(|job| job.command = t.clone())
        }),
    );
    let cwd = row(
        "Working directory",
        "Empty means the project root, or your home directory",
        &text(
            &draft
                .borrow()
                .cwd
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            24,
            "",
            {
                let commit = commit.clone();
                move |t| {
                    let dir = (!t.trim().is_empty()).then(|| std::path::PathBuf::from(t.trim()));
                    commit.apply(|job| job.cwd = dir);
                }
            },
        ),
    );
    let limit = row(
        "Time limit",
        "Killed after this long, so a hung command cannot block the next run",
        &stepper(
            draft.borrow().timeout_secs.unwrap_or(900) as f64,
            30.0,
            7200.0,
            30.0,
            0,
            "s",
            {
                let commit = commit.clone();
                move |v| commit.apply(|job| job.timeout_secs = Some(v as u64))
            },
        ),
    );

    // Agent.
    let available = taix_core::available(&ed.cfg);
    let mut harness_ids: Vec<String> = available.iter().map(|h| h.id.clone()).collect();
    let mut harness_labels: Vec<String> = available.iter().map(|h| h.label.clone()).collect();
    let chosen = draft.borrow().harness.clone();
    if !chosen.is_empty() && !harness_ids.contains(&chosen) {
        // A harness this machine does not have must still read as what the
        // job says, rather than silently as another one.
        harness_ids.push(chosen.clone());
        harness_labels.push(format!("{chosen} (not on PATH)"));
    }
    let at_harness = harness_ids.iter().position(|h| *h == chosen).unwrap_or(0);
    let icons_for = {
        let (cfg, ids) = (ed.cfg.clone(), harness_ids.clone());
        move |i: usize| {
            let icon = ids.get(i).map(|id| taix_core::by_id(&cfg, id).icon)?;
            let image = icons::image(icon.as_deref(), 14);
            image.add_css_class("harness-icon");
            Some(image.upcast::<gtk::Widget>())
        }
    };
    let agent = row(
        "Agent",
        "Opened in the project, then asked",
        &picker(&harness_labels, at_harness, icons_for, {
            let (commit, ids) = (commit.clone(), harness_ids.clone());
            move |i| {
                let Some(id) = ids.get(i).cloned() else {
                    return;
                };
                commit.apply(|job| job.harness = id.clone());
            }
        }),
    );
    let prompt = row(
        "Prompt",
        "Typed in once the agent is ready. Empty just opens the window",
        &text(&draft.borrow().prompt, 28, "", {
            let commit = commit.clone();
            move |t| commit.apply(|job| job.prompt = t.clone())
        }),
    );
    let reuse = row(
        "Use a window that is already open",
        "Otherwise every run opens a new one",
        &toggle(draft.borrow().reuse, {
            let commit = commit.clone();
            move |on| commit.apply(|job| job.reuse = on)
        }),
    );

    // Session.
    let sessions = {
        let mut names = taix_core::session::list(&ed.cfg);
        let saved = draft.borrow().session.clone();
        if !saved.is_empty() && !names.contains(&saved) {
            names.push(saved);
        }
        names
    };
    let at_session = sessions
        .iter()
        .position(|s| *s == draft.borrow().session)
        .unwrap_or(0);
    let session = row(
        "Session",
        if sessions.is_empty() {
            "Nothing saved yet — Ctrl+Shift-S saves one"
        } else {
            "Reopens its projects and windows"
        },
        &picker(&sessions, at_session, |_| None, {
            let (commit, sessions) = (commit.clone(), sessions.clone());
            move |i| {
                let Some(name) = sessions.get(i).cloned() else {
                    return;
                };
                commit.apply(|job| job.session = name.clone());
            }
        }),
    );

    // Git.
    let git_labels: Vec<String> = GIT_OPS.iter().map(|(l, _)| l.to_string()).collect();
    let at_git = GIT_OPS
        .iter()
        .position(|(_, op)| *op == draft.borrow().command)
        .unwrap_or(0);
    let git = row(
        "Operation",
        "Never interactive: a push that needs a password fails rather than hangs",
        &picker(&git_labels, at_git, |_| None, {
            let commit = commit.clone();
            move |i| {
                let op = GIT_OPS[i].1.to_string();
                commit.apply(|job| job.command = op.clone());
            }
        }),
    );

    // Built once and shown or hidden: changing the action must not rebuild
    // the rows under the user's cursor.
    let show = {
        let (command, cwd, limit) = (command.clone(), cwd.clone(), limit.clone());
        let (agent, prompt, reuse) = (agent.clone(), prompt.clone(), reuse.clone());
        let (session, git) = (session.clone(), git.clone());
        move |action: Action| {
            for (w, on) in [
                (&command, action == Action::Shell),
                (&cwd, action == Action::Shell),
                (&limit, action == Action::Shell),
                (&agent, action == Action::Agent),
                (&prompt, action == Action::Agent),
                (&reuse, action == Action::Agent),
                (&session, action == Action::Session),
                (&git, action == Action::Git),
            ] {
                w.set_visible(on);
            }
        }
    };
    show(draft.borrow().action);
    let action = picker(&action_labels, at, |_| None, {
        let (commit, draft) = (commit.clone(), draft.clone());
        move |i| {
            let chosen = ACTIONS[i].1;
            // A git job with no operation yet cannot be saved, and "fetch"
            // is the one that cannot surprise anyone.
            if chosen == Action::Git && !GIT_OPS.iter().any(|(_, op)| *op == draft.borrow().command)
            {
                commit.apply(|job| job.command = "fetch".to_string());
            }
            commit.apply(|job| job.action = chosen);
            show(chosen);
        }
    });
    body.append(&row(
        "Action",
        "What happens when its slot comes round",
        &action,
    ));
    for w in [
        &command, &cwd, &limit, &agent, &prompt, &reuse, &session, &git,
    ] {
        body.append(w);
    }

    // Notifications and catch-up ---------------------------------------------
    let notify_labels: Vec<String> = NOTIFIES.iter().map(|(l, _)| l.to_string()).collect();
    let at_notify = NOTIFIES
        .iter()
        .position(|(_, n)| *n == draft.borrow().notify)
        .unwrap_or(0);
    body.append(&row(
        "Tell me",
        "A desktop notification when the run is over",
        &picker(&notify_labels, at_notify, |_| None, {
            let commit = commit.clone();
            move |i| {
                let choice = NOTIFIES[i].1;
                commit.apply(|job| job.notify = choice);
            }
        }),
    ));
    body.append(&row(
        "Catch up after downtime",
        "Run a slot the machine slept through. Off means wait for the next one",
        &toggle(draft.borrow().catch_up, {
            let commit = commit.clone();
            move |on| commit.apply(|job| job.catch_up = on)
        }),
    ));

    // Last run, and the tools ------------------------------------------------
    let last = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["set-note", "mono"])
        .label(last_run_text(&draft.borrow()))
        .build();
    let tools = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["set-card-foot"])
        .build();
    tools.append(&last);

    let run_now = gtk::Button::builder()
        .label("Run now")
        .tooltip_text("Run it now, whatever the schedule says")
        .css_classes(["set-tool"])
        .build();
    run_now.connect_clicked({
        let (ed, last, draft, head) = (ed.clone(), last.clone(), draft.clone(), head.clone());
        move |button| {
            (ed.run)(id);
            button.set_sensitive(false);
            last.set_label("running…");
            // The runner is a child process, so the record changes long
            // after this callback returned: watch it for a while, then stop.
            let (ed, last, draft, head, button) = (
                ed.clone(),
                last.clone(),
                draft.clone(),
                head.clone(),
                button.clone(),
            );
            let before = draft.borrow().runs.len();
            let mut left = WATCH_SECS;
            glib::timeout_add_seconds_local(1, move || {
                left -= 1;
                let Ok(Some(fresh)) = ed.store.job(id) else {
                    button.set_sensitive(true);
                    return glib::ControlFlow::Break;
                };
                let done = fresh.runs.len() != before;
                if done || left == 0 {
                    last.set_label(&last_run_text(&fresh));
                    head.show(&ed.cfg, &fresh);
                    *draft.borrow_mut() = fresh;
                    button.set_sensitive(true);
                    return glib::ControlFlow::Break;
                }
                glib::ControlFlow::Continue
            });
        }
    });
    tools.append(&run_now);

    let log = gtk::Button::builder()
        .label("Log")
        .tooltip_text("What it printed, last few hundred lines")
        .css_classes(["set-tool"])
        .build();
    log.connect_clicked({
        let (ed, draft) = (ed.clone(), draft.clone());
        move |_| {
            let text = core::log_tail(&ed.cfg, id, 400);
            let text = if text.trim().is_empty() {
                "nothing logged yet".to_string()
            } else {
                text
            };
            crate::ui::show_text(
                &ed.parent,
                &format!("{} — log", draft.borrow().name),
                &glib::markup_escape_text(&text),
                false,
            );
        }
    });
    tools.append(&log);

    let copy = gtk::Button::builder()
        .label("Duplicate")
        .tooltip_text("A disabled copy, to change without touching this one")
        .css_classes(["set-tool"])
        .build();
    copy.connect_clicked({
        let (pane, draft) = (pane.clone(), draft.clone());
        move |_| {
            let source = draft.borrow().clone();
            let made = pane.ed.store.add_job(&NewJob {
                name: format!("{} (copy)", source.name),
                enabled: false,
                project: source.project,
                schedule: source.schedule.clone(),
                action: source.action,
            });
            match made {
                Ok(mut fresh) => {
                    // `add_job` only takes the five fields a new job needs;
                    // the rest of the definition is copied over it.
                    fresh.command = source.command.clone();
                    fresh.harness = source.harness.clone();
                    fresh.prompt = source.prompt.clone();
                    fresh.reuse = source.reuse;
                    fresh.session = source.session.clone();
                    fresh.cwd = source.cwd.clone();
                    fresh.timeout_secs = source.timeout_secs;
                    fresh.notify = source.notify;
                    fresh.catch_up = source.catch_up;
                    pane.ed.save(&fresh);
                    pane.open.set(Some(fresh.id));
                    pane.rebuild(&pane);
                }
                Err(e) => pane.ed.complain(&format!("Cannot copy it: {e}")),
            }
        }
    });
    tools.append(&copy);

    let delete = gtk::Button::builder()
        .label("Delete")
        .css_classes(["set-tool", "danger"])
        .build();
    delete.connect_clicked({
        let pane = pane.clone();
        move |_| {
            if let Err(e) = pane.ed.store.remove_job(id) {
                pane.ed.complain(&format!("Cannot delete it: {e}"));
                return;
            }
            (pane.ed.changed)();
            pane.open.set(None);
            pane.rebuild(&pane);
        }
    });
    tools.append(&delete);
    body.append(&tools);
    card
}

fn open_card(card: &gtk::Box, open: bool) {
    let mut child = card.first_child();
    while let Some(widget) = child {
        if let Ok(revealer) = widget.clone().downcast::<gtk::Revealer>() {
            revealer.set_reveal_child(open);
        }
        child = widget.next_sibling();
    }
}

/// The Schedule row's own reading: the parse error, or the next three runs.
fn schedule_hint(expr: &str) -> String {
    let now = core::now();
    match core::parse(expr) {
        Err(e) => e,
        Ok(schedule) => {
            let runs: Vec<String> = schedule
                .next_runs(now, 3)
                .into_iter()
                .map(|t| core::format_local(t, now))
                .collect();
            if runs.is_empty() {
                "nothing matches that".to_string()
            } else {
                format!("next: {}", runs.join(", "))
            }
        }
    }
}

fn last_run_text(job: &Job) -> String {
    match job.last() {
        Some(run) => run_line(run, core::now()),
        None => "never run".to_string(),
    }
}

/// Which preset an expression is, or `Custom…`.
fn preset_index(expr: &str) -> usize {
    PRESETS
        .iter()
        .position(|(_, preset)| *preset == expr)
        .unwrap_or(PRESETS.len() - 1)
}

/// The last ten runs as ticks: green for a clean exit, red for anything
/// else. Painted rather than styled - the colours come from the palette at
/// runtime, which CSS classes alone cannot reach.
fn sparkline(runs: &[Run]) -> gtk::DrawingArea {
    const TICKS: usize = 10;
    let recent: Vec<bool> = runs
        .iter()
        .rev()
        .take(TICKS)
        .rev()
        // An agent or git run has no exit code; only an error means it
        // failed, which is what `Job::failing` says too.
        .map(|r| r.error.is_none() && r.exit.is_none_or(|code| code == 0))
        .collect();
    let area = gtk::DrawingArea::builder()
        .content_width(TICKS as i32 * 5)
        .content_height(14)
        .valign(gtk::Align::Center)
        .tooltip_text("The last ten runs, oldest first")
        .build();
    let ok = crate::theme::color("taix_done", "#6fbf8f");
    let bad = crate::theme::color("taix_failed", "#ef8f8f");
    area.set_draw_func(move |_, cr, width, height| {
        let start = width as f64 - recent.len() as f64 * 5.0;
        for (i, good) in recent.iter().enumerate() {
            let hex = if *good { &ok } else { &bad };
            let rgba = gtk::gdk::RGBA::parse(hex).unwrap_or(gtk::gdk::RGBA::WHITE);
            cr.set_source_rgba(
                rgba.red() as f64,
                rgba.green() as f64,
                rgba.blue() as f64,
                if *good { 0.55 } else { 0.9 },
            );
            cr.rectangle(start + i as f64 * 5.0, 2.0, 3.0, height as f64 - 4.0);
            let _ = cr.fill();
        }
    });
    area
}

// History ------------------------------------------------------------------

/// Every remembered run of every job, newest first. The store keeps the last
/// twenty per job, which is what a person actually looks back over.
struct HistoryPane {
    ed: Rc<Editor>,
    page: gtk::Box,
}

impl HistoryPane {
    fn rebuild(&self) {
        clear(&self.page);
        let jobs = self.ed.store.jobs().unwrap_or_default();
        let now = core::now();
        let mut rows: Vec<(&Job, &Run)> = jobs
            .iter()
            .flat_map(|job| job.runs.iter().map(move |run| (job, run)))
            .collect();
        rows.sort_by_key(|(_, run)| std::cmp::Reverse(run.at));

        let failed = rows
            .iter()
            .filter(|(_, r)| r.error.is_some() || r.exit != Some(0))
            .count();
        let group = group("Runs", Some(rows.len()), None);
        if rows.is_empty() {
            group.append(&note(
                "No job has run yet. A job records its last twenty runs here.",
            ));
        }
        for (job, run) in rows.iter().take(200) {
            group.append(&history_row(&self.ed, job, run, now));
        }
        let stats = kit::stats(&[
            (rows.len().to_string(), "Runs remembered"),
            (failed.to_string(), "Of those, failed"),
            (
                jobs.iter().filter(|j| j.enabled).count().to_string(),
                "Jobs on",
            ),
        ]);
        self.page.append(&stats);
        self.page.append(&group);
    }
}

fn history_row(ed: &Rc<Editor>, job: &Job, run: &Run, now: i64) -> gtk::Box {
    let ok = run.error.is_none() && run.exit.is_none_or(|code| code == 0);
    let line = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .css_classes(["set-row", "run-row"])
        .build();
    line.append(
        &gtk::Label::builder()
            .label(core::format_local(run.at, now))
            .xalign(0.0)
            .width_chars(12)
            .css_classes(["set-note", "mono"])
            .build(),
    );
    let name = gtk::Label::builder()
        .label(&job.name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["set-row-title"])
        .build();
    line.append(&name);
    if let Some(e) = &run.error {
        line.append(
            &gtk::Label::builder()
                .label(e)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(40)
                .css_classes(["set-row-sub"])
                .build(),
        );
    }
    line.append(
        &gtk::Label::builder()
            .label(duration(run.ms))
            .css_classes(["set-note", "mono"])
            .build(),
    );
    line.append(&badge(
        &match (run.exit, ok) {
            (Some(0), _) | (None, true) => "ok".to_string(),
            (Some(code), _) => format!("exit {code}"),
            (None, false) => "failed".to_string(),
        },
        Some(if ok { "ready" } else { "failed" }),
    ));
    let open = gtk::Button::builder()
        .label("Log")
        .css_classes(["set-tool"])
        .build();
    open.connect_clicked({
        let (ed, id, title) = (ed.clone(), job.id, job.name.clone());
        move |_| {
            let text = core::log_tail(&ed.cfg, id, 400);
            crate::ui::show_text(
                &ed.parent,
                &format!("{title} — log"),
                &glib::markup_escape_text(&text),
                false,
            );
        }
    });
    line.append(&open);
    line
}

// Background ---------------------------------------------------------------

fn background(ed: &Rc<Editor>) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 20);
    let group = group("With TaiX closed", None, None);

    let status = core::background_status();
    let switch = toggle(status == core::Background::Enabled, {
        let ed = ed.clone();
        move |on| {
            let result = if on {
                core::background_install().map(Option::unwrap_or_default)
            } else {
                core::background_uninstall().map(|()| String::new())
            };
            match result {
                Ok(warning) if warning.is_empty() => {}
                Ok(warning) => ed.complain(&warning),
                Err(e) => ed.complain(&e),
            }
        }
    });
    if status == core::Background::Unavailable {
        switch.set_sensitive(false);
    }
    group.append(&row(
        "Run jobs when TaiX is closed",
        match status {
            core::Background::Unavailable => "systemctl was not found on this machine",
            _ => "A systemd user timer ticks once a minute and asks TaiX what is due",
        },
        &switch,
    ));

    group.append(&row(
        "Runner",
        "The binary the timer starts",
        &gtk::Label::builder()
            .label(
                core::taix_binary()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "taix not found next to taix-gui or on PATH".to_string()),
            )
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(40)
            .valign(gtk::Align::Center)
            .css_classes(["set-note", "mono"])
            .build(),
    ));

    let path = gtk::Label::builder()
        .label(core::background_path())
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(40)
        .valign(gtk::Align::Center)
        .css_classes(["set-note", "mono"])
        .build();
    path.set_tooltip_text(Some(core::background_path().as_str()));
    group.append(&row(
        "Search path",
        "A user service inherits nothing from your shell, so this is baked into the unit. \
         An agent that is not reachable from it will not start",
        &path,
    ));

    let now = gtk::Button::builder()
        .label("Run anything due now")
        .css_classes(["set-tool"])
        .build();
    now.connect_clicked({
        let ed = ed.clone();
        move |_| match core::spawn_runner() {
            Ok(_) => ed.complain("Asked the runner for anything due"),
            Err(e) => ed.complain(&format!("Cannot start the runner: {e}")),
        }
    });
    group.append(&row(
        "Right now",
        "The same pass the timer makes, without waiting for the minute",
        &now,
    ));
    page.append(&group);
    page
}

// Odds and ends -------------------------------------------------------------

fn clear(box_: &gtk::Box) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}

/// A quiet line where a list would be, when the list is empty.
fn note(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["set-note"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_job() -> Job {
        Job {
            id: 1,
            name: "nightly".to_string(),
            enabled: true,
            project: None,
            schedule: "@daily".to_string(),
            action: Action::Shell,
            command: "echo hi".to_string(),
            harness: String::new(),
            prompt: String::new(),
            reuse: false,
            session: String::new(),
            cwd: None,
            timeout_secs: None,
            notify: Notify::Failure,
            catch_up: true,
            created: 0,
            last_run: None,
            runs: Vec::new(),
        }
    }

    #[test]
    fn a_job_that_cannot_run_reads_as_broken() {
        let mut job = a_job();
        assert_eq!(state_badge(&job).0, "waiting");
        job.command = String::new();
        assert_eq!(state_badge(&job), ("broken", Some("failed")));
        job.command = "echo hi".to_string();
        job.schedule = "not a schedule".to_string();
        assert_eq!(state_badge(&job), ("broken", Some("failed")));
        job.enabled = false;
        // Off outranks broken: nothing is going to run either way, and "off"
        // is the reason.
        assert_eq!(state_badge(&job), ("off", None));
    }

    #[test]
    fn the_last_run_decides_the_badge() {
        let mut job = a_job();
        job.runs.push(Run {
            at: 10,
            exit: Some(1),
            ms: 5,
            error: None,
        });
        assert_eq!(state_badge(&job), ("failed", Some("failed")));
        job.runs.push(Run {
            at: 20,
            exit: Some(0),
            ms: 5,
            error: None,
        });
        assert_eq!(state_badge(&job), ("ok", Some("ready")));
    }

    #[test]
    fn durations_read_as_people_write_them() {
        assert_eq!(duration(0), "0ms");
        assert_eq!(duration(999), "999ms");
        assert_eq!(duration(1500), "1.5s");
        assert_eq!(duration(65_000), "1m05s");
    }

    #[test]
    fn presets_round_trip_through_their_expressions() {
        for (i, (_, expr)) in PRESETS.iter().enumerate() {
            if expr.is_empty() {
                continue;
            }
            assert_eq!(preset_index(expr), i, "{expr}");
        }
        assert_eq!(preset_index("13 4 * * *"), PRESETS.len() - 1);
    }

    #[test]
    fn the_filter_looks_at_more_than_the_name() {
        let mut job = a_job();
        job.name = "nightly".to_string();
        job.command = "restic backup".to_string();
        assert!(matches(&job, ""));
        assert!(matches(&job, "restic"));
        assert!(matches(&job, "NIGHT"));
        assert!(!matches(&job, "borg"));
    }
}
