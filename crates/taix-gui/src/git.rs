//! Source control beside the panes: what changed, what to stage, what to say
//! about it, and every way out of the mess afterwards.
//!
//! The panel owns the repository side and nothing else. Showing a diff needs
//! the app's text viewer and opening a file needs its editor, so both are an
//! [`Ask`] handed upwards, the way the files tree does it.
//!
//! Three rules hold the thing together:
//!
//! * **Git is the truth.** Nothing here predicts what a command did; every
//!   action re-reads the repository and redraws from what it finds.
//! * **Anything slow runs off the main thread.** Not just the network:
//!   checking out a branch or resetting a big tree takes seconds, and a
//!   frozen window during a merge is how work gets lost.
//! * **Menus outlive their rows.** A context menu is parented to the panel,
//!   not to the row it came from, so a refresh underneath it cannot yank it
//!   away mid-click - and a poll is skipped entirely while one is open.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use taix_git::{FileChange, Op, Repo, Reset, Side, Step};

/// How much of a diff is worth putting in a window. Past this you want a
/// pager.
const DIFF_CAP: usize = 200_000;
/// Commits per page of history.
const PAGE: usize = 15;

/// What the panel cannot do for itself.
pub enum Ask {
    /// A title and a unified diff, for the app's text viewer.
    Diff(String, String),
    /// Open this file in the editor.
    Open(PathBuf),
    Notice(String),
}

/// Where the panel is pointed. Kept whole so a refresh cannot half-swap a
/// root and a name.
struct Checkout {
    name: String,
    root: PathBuf,
    repo: Repo,
}

/// Everything one redraw needs, read in a single borrow so rendering can
/// never re-enter the cell it came from.
struct Snapshot {
    name: String,
    status: taix_git::Status,
    files: Vec<FileChange>,
    stashes: Vec<taix_git::Stash>,
    commits: Vec<taix_git::Commit>,
    more: bool,
    op: Option<Op>,
}

/// The app's answer to an [`Ask`], set once the app exists.
type AskFn = Rc<dyn Fn(Ask)>;

/// What the branch list is being picked *for*.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    Switch,
    Merge,
    Rebase,
}

pub struct Git {
    pub root: gtk::Box,
    /// What the panel asks the app for. Set once, by the app.
    ask: RefCell<Option<AskFn>>,
    at: RefCell<Option<Checkout>>,
    /// A git command is running; the tools are insensitive until it lands,
    /// so a double click cannot start two pushes.
    busy: Cell<bool>,
    /// A menu is open. Rebuilding the rows under it would close it.
    menu: Rc<Cell<bool>>,
    /// Sections the user folded away, by key.
    folded: RefCell<HashSet<String>>,
    /// How many commits the history is showing.
    shown: Cell<usize>,
    /// The commit whose file list is expanded, if any.
    opened: RefCell<Option<String>>,

    project: gtk::Label,
    branch: gtk::MenuButton,
    branch_label: gtk::Label,
    drift: gtk::Label,
    tools: gtk::Box,
    sync: gtk::Button,
    banner: gtk::Box,
    banner_label: gtk::Label,
    banner_tools: gtk::Box,
    changes: gtk::Box,
    commit_box: gtk::Box,
    message: gtk::TextView,
    hint: gtk::Label,
    amend: gtk::CheckButton,
    commit: adw::SplitButton,
    stashes: gtk::Box,
    history: gtk::Box,
    /// Last rendered changes, to skip rebuilds when unchanged.
    last_changes: RefCell<Vec<FileChange>>,
}

impl Git {
    pub fn new() -> Rc<Git> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["git-panel"])
            .build();

        // Head: which checkout, which branch, how far it has drifted, and the
        // handful of verbs that apply to the whole repository.
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["git-head"])
            .build();
        let project = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["git-project"])
            .build();
        let reload = icon_tool("view-refresh-symbolic", "Re-read the repository", false);
        let title_line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        title_line.append(&project);
        title_line.append(&reload);

        let branch_label = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(18)
            .css_classes(["git-branch-name", "mono"])
            .build();
        // No branch glyph: no icon theme reliably ships one, and a missing
        // icon draws a broken-image box. The caret says it is a menu.
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        face.append(&branch_label);
        face.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        let branch = gtk::MenuButton::builder()
            .child(&face)
            .tooltip_text("Switch branch, merge, rebase, or start one")
            .css_classes(["git-branch"])
            .build();
        let drift = gtk::Label::builder()
            .xalign(1.0)
            .hexpand(true)
            .css_classes(["git-drift", "mono"])
            .build();
        let branch_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        branch_line.append(&branch);
        branch_line.append(&drift);
        head.append(&title_line);
        head.append(&branch_line);

        let tools = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .css_classes(["git-tools"])
            .build();
        head.append(&tools);
        root.append(&head);

        // A merge or a rebase that stopped is the only thing that matters
        // while it is unfinished, so it gets a band of its own above
        // everything else.
        let banner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .visible(false)
            .css_classes(["git-banner"])
            .build();
        let banner_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["git-banner-text"])
            .build();
        let banner_tools = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        banner.append(&banner_label);
        banner.append(&banner_tools);
        root.append(&banner);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&body)
            .build();
        let changes = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&changes);

        // Commit: a box that stays put while the lists around it churn, so a
        // half-typed message survives a refresh.
        let commit_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["git-commit"])
            .build();
        let message = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(8)
            .right_margin(8)
            .css_classes(["git-message", "mono"])
            .build();
        let framed = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(56)
            .max_content_height(140)
            .child(&message)
            .css_classes(["git-message-frame"])
            .build();
        // A text view has no placeholder, and an empty box with no prompt is
        // the one control people ask about.
        let hint = gtk::Label::builder()
            .label("Message — Ctrl-Enter commits")
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_start(9)
            .margin_top(7)
            .can_target(false)
            .css_classes(["git-hint"])
            .build();
        let overlay = gtk::Overlay::builder().child(&framed).build();
        overlay.add_overlay(&hint);
        commit_box.append(&overlay);
        let commit_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let amend = gtk::CheckButton::builder()
            .label("Amend")
            .tooltip_text("Replace the last commit instead of adding one")
            .css_classes(["git-amend"])
            .build();
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let commit = adw::SplitButton::builder()
            .label("Commit")
            .sensitive(false)
            .tooltip_text("Commit what is staged (Ctrl-Enter)")
            .css_classes(["git-commit-button"])
            .build();
        commit_line.append(&amend);
        commit_line.append(&spacer);
        commit_line.append(&commit);
        commit_box.append(&commit_line);
        body.append(&commit_box);

        let stashes = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&stashes);
        let history = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&history);
        root.append(&scroller);

        let panel = Rc::new(Git {
            root,
            ask: RefCell::new(None),
            at: RefCell::new(None),
            busy: Cell::new(false),
            menu: Rc::new(Cell::new(false)),
            folded: RefCell::new(HashSet::new()),
            shown: Cell::new(PAGE),
            opened: RefCell::new(None),
            project,
            branch,
            branch_label,
            drift,
            tools,
            sync: tool_button("Sync", "Pull, then push"),
            banner,
            banner_label,
            banner_tools,
            changes,
            commit_box,
            message,
            hint,
            amend,
            commit,
            stashes,
            history,
            last_changes: RefCell::new(Vec::new()),
        });
        panel.wire(&reload);
        panel.clear();
        panel
    }

    /// What the panel asks the app for. Set once, by the app.
    pub fn connect(&self, f: impl Fn(Ask) + 'static) {
        *self.ask.borrow_mut() = Some(Rc::new(f));
    }

    /// Hand the app a request. The borrow is dropped before the call: the
    /// app answers by queueing a pointer, which may reach back in here.
    fn say(&self, ask: Ask) {
        let f = self.ask.borrow().clone();
        if let Some(f) = f {
            f(ask);
        }
    }

    /// Read something off the current checkout. The borrow ends with the
    /// call, so the closure must never redraw.
    fn with<T>(&self, f: impl FnOnce(&Checkout) -> T) -> Option<T> {
        let borrowed = self.at.borrow();
        borrowed.as_ref().map(f)
    }

    /// The buttons and the keys, once: everything below rebuilds, this does
    /// not.
    fn wire(self: &Rc<Self>, reload: &gtk::Button) {
        reload.connect_clicked({
            let this = self.clone();
            move |_| this.refresh()
        });

        self.sync.connect_clicked({
            let this = self.clone();
            move |_| this.run(Job::Sync)
        });
        self.tools.append(&self.sync);
        for (label, tip, job) in [
            ("Pull", "Fast-forward to the remote", Job::Pull),
            ("Push", "Send this branch to the remote", Job::Push(false)),
        ] {
            let button = tool_button(label, tip);
            button.connect_clicked({
                let this = self.clone();
                move |_| this.run(job.clone())
            });
            self.tools.append(&button);
        }
        // Everything rarer than those three, one level in. Built when it
        // opens so it can know whether there is a stash to pop.
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Everything else")
            .css_classes(["git-tool", "git-more"])
            .build();
        more.set_create_popup_func({
            let this = self.clone();
            move |button| {
                let pop = this.more_menu();
                button.set_popover(Some(&pop));
            }
        });
        self.tools.append(&more);

        self.branch.set_create_popup_func({
            let this = self.clone();
            move |button| {
                let pop = this.branch_menu(Pick::Switch);
                button.set_popover(Some(&pop));
            }
        });

        self.commit.connect_clicked({
            let this = self.clone();
            move |_| this.do_commit(After::Nothing)
        });
        self.commit.set_popover(Some(&self.commit_menu()));
        // Typing is the only thing that can make a commit possible, so the
        // button's state is decided here rather than on every refresh.
        self.message.buffer().connect_changed({
            let this = self.clone();
            move |_| this.retune_commit()
        });
        self.amend.connect_toggled({
            let this = self.clone();
            move |check| {
                // Amending with an empty box would silently discard the
                // message being replaced; seed it instead.
                if check.is_active() && this.message_text().trim().is_empty() {
                    let last = this
                        .with(|at| at.repo.last_message(&at.root).unwrap_or_default())
                        .unwrap_or_default();
                    this.message.buffer().set_text(last.trim());
                }
                this.retune_commit();
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let this = self.clone();
            move |_, key, _, state| {
                let enter = key == gdk::Key::Return || key == gdk::Key::KP_Enter;
                if enter && state.contains(gdk::ModifierType::CONTROL_MASK) {
                    this.do_commit(After::Nothing);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        self.message.add_controller(keys);
    }

    /// Point the panel at a project. Cheap when nothing moved: opening a
    /// repository is a `git` process, so the same root is not reopened.
    pub fn set_root(self: &Rc<Self>, name: &str, root: &Path) {
        if self
            .at
            .borrow()
            .as_ref()
            .is_some_and(|at| at.root == root && at.name == name)
        {
            return;
        }
        // A message typed for one project must not ride along to the next.
        self.message.buffer().set_text("");
        self.amend.set_active(false);
        self.shown.set(PAGE);
        *self.opened.borrow_mut() = None;
        match Repo::open(root) {
            Ok(repo) => {
                *self.at.borrow_mut() = Some(Checkout {
                    name: name.to_string(),
                    // The panel acts on the project directory it was given,
                    // not on the repository root: a project inside a
                    // worktree stages that worktree's files.
                    root: root.to_path_buf(),
                    repo,
                });
                self.refresh();
            }
            Err(_) => {
                *self.at.borrow_mut() = None;
                self.empty(&format!("{name} is not a git repository"));
            }
        }
    }

    pub fn clear(self: &Rc<Self>) {
        *self.at.borrow_mut() = None;
        self.empty("No project selected");
    }

    /// Re-read the repository and redraw. Everything that changes anything
    /// calls this; nothing else does.
    pub fn refresh(self: &Rc<Self>) {
        let Some(snap) = self.read() else { return };
        self.project.set_label(&snap.name);
        self.commit_box.set_visible(true);
        self.branch.set_sensitive(true);
        self.tools.set_sensitive(!self.busy.get());
        self.branch_label
            .set_label(if snap.status.branch.is_empty() {
                "no branch yet"
            } else {
                &snap.status.branch
            });
        self.drift.set_label(&drift_line(&snap.status));
        self.sync.set_label(&sync_label(&snap.status));

        self.fill_banner(snap.op);
        self.fill_changes(&snap.files);
        self.fill_stashes(&snap.stashes);
        self.fill_history(&snap.commits, snap.more);
        self.retune_commit();
    }

    /// One pass over the repository: one `taix_git::panel()` call.
    ///
    /// The checkout's own `Repo` is handed over rather than a fresh one: it
    /// has already paid for the git-dir and the remote URL, which `panel`
    /// would otherwise re-ask `git` for on every pass.
    fn read(&self) -> Option<Snapshot> {
        let want = self.shown.get();
        self.with(|at| match taix_git::panel(&at.repo, &at.root) {
            Ok(panel) => {
                let mut commits = panel.log;
                let more = commits.len() > want;
                commits.truncate(want);
                Snapshot {
                    name: at.name.clone(),
                    status: taix_git::Status {
                        branch: panel.branch,
                        dirty: if panel.dirty { 1 } else { 0 },
                        ahead: panel.ahead,
                        behind: panel.behind,
                    },
                    files: panel.changes,
                    stashes: panel.stashes,
                    commits,
                    more,
                    op: panel.op,
                }
            }
            Err(e) => {
                taix_core::trace!("git panel read failed: {e}");
                Snapshot {
                    name: at.name.clone(),
                    status: taix_git::Status::default(),
                    files: Vec::new(),
                    stashes: Vec::new(),
                    commits: Vec::new(),
                    more: false,
                    op: None,
                }
            }
        })
    }

    /// Refresh on the app's slow cadence. The app only calls this while the
    /// panel is on screen and the window has focus.
    pub fn poll(self: &Rc<Self>) {
        if !self.busy.get() && !self.menu.get() {
            self.refresh();
        }
    }

    fn empty(&self, why: &str) {
        self.project.set_label(why);
        self.branch_label.set_label("—");
        self.drift.set_label("");
        self.commit_box.set_visible(false);
        self.banner.set_visible(false);
        clear(&self.changes);
        clear(&self.stashes);
        clear(&self.history);
        self.branch.set_sensitive(false);
        self.tools.set_sensitive(false);
    }

    // Unfinished business -----------------------------------------------------

    /// A merge, rebase, cherry-pick or revert that stopped for a decision.
    /// Until it is finished or abandoned, nothing else in the panel means
    /// what it usually means, so it is announced at the top.
    fn fill_banner(self: &Rc<Self>, op: Option<Op>) {
        clear(&self.banner_tools);
        let Some(op) = op else {
            self.banner.set_visible(false);
            return;
        };
        self.banner.set_visible(true);
        self.banner_label.set_label(&format!(
            "{} — resolve the conflicts, then continue.",
            op.label()
        ));
        for (label, step, danger) in [
            ("Continue", Step::Continue, false),
            ("Skip", Step::Skip, false),
            ("Abort", Step::Abort, true),
        ] {
            // Only a rebase or a cherry-pick has commits to skip.
            if step == Step::Skip && op == Op::Merge {
                continue;
            }
            let button = tool_button(label, "");
            if danger {
                button.add_css_class("danger");
            }
            button.connect_clicked({
                let this = self.clone();
                move |_| this.run(Job::Step(step))
            });
            self.banner_tools.append(&button);
        }
    }
    fn fill_changes(self: &Rc<Self>, files: &[FileChange]) {
        // O1: Skip rebuild when changes are unchanged.
        if *self.last_changes.borrow() == files {
            return;
        }

        clear(&self.changes);
        if files.is_empty() {
            self.changes.append(
                &gtk::Label::builder()
                    .label("Nothing to commit — the tree is clean.")
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["git-quiet"])
                    .build(),
            );
            *self.last_changes.borrow_mut() = Vec::new();
            return;
        }
        for group in [
            Group::Conflicted,
            Group::Staged,
            Group::Changed,
            Group::Untracked,
        ] {
            let members: Vec<&FileChange> = files.iter().filter(|f| group.holds(f)).collect();
            if members.is_empty() {
                continue;
            }
            let paths: Vec<String> = members.iter().map(|f| f.path.clone()).collect();
            let mut tools: Vec<gtk::Widget> = Vec::new();
            if let Some(label) = group.bulk() {
                let stage = group.bulk_is_stage();
                let button = line_button(label);
                button.connect_clicked({
                    let (this, paths) = (self.clone(), paths.clone());
                    move |_| this.act(stage, &paths)
                });
                tools.push(button.upcast());
            }
            if matches!(group, Group::Changed | Group::Untracked) {
                let button = icon_tool("user-trash-symbolic", "Discard every change here", true);
                button.connect_clicked({
                    let (this, paths, untracked) =
                        (self.clone(), paths.clone(), group == Group::Untracked);
                    move |_| this.discard_many(&paths, untracked)
                });
                tools.push(button.upcast());
            }
            let body = self.section(
                &self.changes,
                group.title(),
                group.title(),
                Some(members.len()),
                &tools,
            );
            for file in members {
                body.append(&self.file_row(group, file));
            }
        }
        *self.last_changes.borrow_mut() = files.to_vec();
    }

    fn file_row(self: &Rc<Self>, group: Group, file: &FileChange) -> gtk::Box {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .css_classes(["git-file"])
            .build();
        let letter = gtk::Label::builder()
            .label(status_letter(file))
            .width_chars(1)
            .css_classes(["git-letter", letter_class(file), "mono"])
            .build();
        let shown = match &file.renamed_from {
            Some(from) => format!("{} ← {from}", file.path),
            None => file.path.clone(),
        };
        let path = gtk::Label::builder()
            .label(&shown)
            .xalign(0.0)
            .hexpand(true)
            // The end of a path is the part that identifies it.
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .tooltip_text(&shown)
            .css_classes(["git-path", "mono"])
            .build();
        let open = gtk::Button::builder()
            .css_classes(["git-file-head"])
            .tooltip_text("Show the diff")
            .hexpand(true)
            .build();
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        face.append(&letter);
        face.append(&path);
        open.set_child(Some(&face));
        open.connect_clicked({
            let (this, path, staged) = (self.clone(), file.path.clone(), group == Group::Staged);
            move |_| this.show_diff(&path, staged)
        });
        row.append(&open);

        if group == Group::Conflicted {
            // A conflict has three answers and they are all one click away:
            // take one side, take the other, or say it is settled.
            for (label, tip, side) in [
                ("Ours", "Keep this side's version", Some(Side::Ours)),
                ("Theirs", "Take the incoming version", Some(Side::Theirs)),
                ("✓", "Mark resolved as it stands", None),
            ] {
                let button = line_button(label);
                button.set_tooltip_text(Some(tip));
                button.connect_clicked({
                    let (this, path) = (self.clone(), file.path.clone());
                    move |_| match side {
                        Some(side) => this.resolve(&path, side),
                        None => this.act(true, std::slice::from_ref(&path)),
                    }
                });
                row.append(&button);
            }
        } else {
            let stage = group != Group::Staged;
            let move_it = icon_tool(
                if stage {
                    "list-add-symbolic"
                } else {
                    "list-remove-symbolic"
                },
                if stage { "Stage" } else { "Unstage" },
                false,
            );
            move_it.connect_clicked({
                let (this, path) = (self.clone(), vec![file.path.clone()]);
                move |_| this.act(stage, &path)
            });
            row.append(&move_it);
            if stage {
                let drop = icon_tool("user-trash-symbolic", "Discard these changes", true);
                drop.connect_clicked({
                    let (this, path, untracked) = (self.clone(), file.path.clone(), file.untracked);
                    move |_| this.discard(&path, untracked)
                });
                row.append(&drop);
            }
        }

        self.on_right_click(&row, {
            let (this, file, group) = (self.clone(), file.clone(), group);
            move |row, x, y| this.file_menu(row, x, y, &file, group)
        });
        row
    }

    /// Everything a file can be asked, in one place - the menu people reach
    /// for when the row's two buttons are not what they wanted.
    fn file_menu(self: &Rc<Self>, row: &gtk::Box, x: f64, y: f64, file: &FileChange, group: Group) {
        let path = file.path.clone();
        let staged = group == Group::Staged;
        let untracked = file.untracked;
        let full = self.with(|at| at.root.join(&path));
        let mut items = vec![
            item("Show diff", {
                let (this, path) = (self.clone(), path.clone());
                move || this.show_diff(&path, staged)
            }),
            item("Open file", {
                let (this, full) = (self.clone(), full.clone());
                move || {
                    if let Some(p) = &full {
                        this.say(Ask::Open(p.clone()));
                    }
                }
            })
            .live(full.is_some() && !matches!(file.work, 'D')),
            item("File history", {
                let (this, path) = (self.clone(), path.clone());
                move || this.file_history(&path)
            })
            .live(!untracked),
            sep(),
        ];
        if staged {
            items.push(item("Unstage", {
                let (this, path) = (self.clone(), vec![path.clone()]);
                move || this.act(false, &path)
            }));
        } else {
            items.push(item("Stage", {
                let (this, path) = (self.clone(), vec![path.clone()]);
                move || this.act(true, &path)
            }));
        }
        if untracked {
            items.push(item("Add to .gitignore", {
                let (this, path) = (self.clone(), path.clone());
                move || this.ignore(&path)
            }));
        }
        items.push(
            item("Copy path", {
                let (this, path) = (self.clone(), path.clone());
                move || this.copy(&path)
            })
            .live(true),
        );
        if !staged {
            items.push(sep());
            items.push(
                item(
                    if untracked {
                        "Delete file"
                    } else {
                        "Discard changes"
                    },
                    {
                        let (this, path) = (self.clone(), path.clone());
                        move || this.discard(&path, untracked)
                    },
                )
                .danger(),
            );
        }
        self.menu(row, Some((x, y)), items);
    }

    /// Stage or unstage, then read the repository back: git is the truth
    /// about what happened, never this panel's guess.
    fn act(self: &Rc<Self>, stage: bool, paths: &[String]) {
        let failed = self.with(|at| {
            let done = if stage {
                at.repo.stage(&at.root, paths)
            } else {
                at.repo.unstage(&at.root, paths)
            };
            done.err().map(|e| e.to_string())
        });
        self.report(failed.flatten());
    }

    fn resolve(self: &Rc<Self>, path: &str, side: Side) {
        let failed = self.with(|at| {
            at.repo
                .resolve(&at.root, path, side)
                .err()
                .map(|e| e.to_string())
        });
        self.report(failed.flatten());
    }

    fn ignore(self: &Rc<Self>, path: &str) {
        let failed = self.with(|at| at.repo.ignore(&at.root, path).err().map(|e| e.to_string()));
        self.report(failed.flatten());
    }

    fn discard(self: &Rc<Self>, path: &str, untracked: bool) {
        let body = if untracked {
            format!("{path} is not tracked. Discarding deletes the file.")
        } else {
            format!("Every uncommitted change in {path} is lost.")
        };
        let this = self.clone();
        let paths = vec![path.to_string()];
        crate::ui::confirm(
            &self.root,
            "Discard changes?",
            &body,
            "Discard",
            move || {
                let failed = this.with(|at| {
                    at.repo
                        .discard(&at.root, &paths)
                        .err()
                        .map(|e| e.to_string())
                });
                this.report(failed.flatten());
            },
        );
    }

    fn discard_many(self: &Rc<Self>, paths: &[String], untracked: bool) {
        let body = if untracked {
            format!("{} untracked files are deleted.", paths.len())
        } else {
            format!("Uncommitted changes in {} files are lost.", paths.len())
        };
        let this = self.clone();
        let paths = paths.to_vec();
        crate::ui::confirm(
            &self.root,
            "Discard changes?",
            &body,
            "Discard",
            move || {
                let failed = this.with(|at| {
                    at.repo
                        .discard(&at.root, &paths)
                        .err()
                        .map(|e| e.to_string())
                });
                this.report(failed.flatten());
            },
        );
    }

    /// Say what went wrong, if anything, then redraw either way.
    fn report(self: &Rc<Self>, failed: Option<String>) {
        if let Some(e) = failed {
            self.say(Ask::Notice(one_line(&e)));
        }
        self.refresh();
    }

    fn show_diff(self: &Rc<Self>, path: &str, staged: bool) {
        let text = self.with(|at| at.repo.file_diff(&at.root, path, staged, DIFF_CAP));
        let title = format!("{path}{}", if staged { " (staged)" } else { "" });
        match text {
            Some(Ok(diff)) if diff.trim().is_empty() => {
                self.say(Ask::Notice(format!("{path}: nothing to show")))
            }
            Some(Ok(diff)) => self.say(Ask::Diff(title, diff)),
            Some(Err(e)) => self.say(Ask::Notice(one_line(&e.to_string()))),
            None => {}
        }
    }

    fn file_history(self: &Rc<Self>, path: &str) {
        let commits = self.with(|at| at.repo.file_log(&at.root, path, 50).unwrap_or_default());
        let Some(commits) = commits else { return };
        if commits.is_empty() {
            self.say(Ask::Notice(format!("{path}: no commits")));
            return;
        }
        let now = taix_core::jobs::now();
        let text = commits
            .iter()
            .map(|c| {
                format!(
                    "{}  {}  {}  {}",
                    c.short,
                    taix_core::jobs::format_local(c.when, now),
                    c.author,
                    c.summary
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.say(Ask::Diff(format!("History of {path}"), text));
    }

    fn copy(&self, text: &str) {
        self.root.clipboard().set_text(text);
        self.say(Ask::Notice(format!("copied {}", one_line(text))));
    }

    // Commit ----------------------------------------------------------------

    fn message_text(&self) -> String {
        let buffer = self.message.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    /// A commit button that is live when a commit would actually work: a
    /// message, and either something staged or an amend.
    fn retune_commit(self: &Rc<Self>) {
        let staged = self
            .with(|at| {
                at.repo
                    .changes(&at.root)
                    .map(|files| files.iter().any(|f| f.staged))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        let text = self.message_text();
        let has_message = !text.trim().is_empty();
        self.hint.set_visible(text.is_empty());
        self.commit
            .set_sensitive(has_message && (staged || self.amend.is_active()));
    }

    /// The other ways to end a commit. Built once: none of it depends on
    /// what the repository currently looks like.
    fn commit_menu(self: &Rc<Self>) -> gtk::Popover {
        self.build_menu(vec![
            item("Commit and push", {
                let this = self.clone();
                move || this.do_commit(After::Push)
            }),
            item("Commit and sync", {
                let this = self.clone();
                move || this.do_commit(After::Sync)
            }),
            sep(),
            item("Stage everything and commit", {
                let this = self.clone();
                move || {
                    let failed =
                        this.with(|at| at.repo.stage_all(&at.root).err().map(|e| e.to_string()));
                    if let Some(e) = failed.flatten() {
                        this.say(Ask::Notice(one_line(&e)));
                        return;
                    }
                    this.retune_commit();
                    this.do_commit(After::Nothing);
                }
            }),
            item("Amend the last commit", {
                let this = self.clone();
                move || this.amend.set_active(!this.amend.is_active())
            }),
        ])
    }

    fn do_commit(self: &Rc<Self>, then: After) {
        if !self.commit.is_sensitive() {
            return;
        }
        let message = self.message_text();
        let amend = self.amend.is_active();
        let made = self.with(|at| at.repo.commit(&at.root, &message, amend));
        match made {
            Some(Ok(short)) => {
                self.message.buffer().set_text("");
                self.amend.set_active(false);
                self.say(Ask::Notice(format!("committed {}", short.trim())));
                match then {
                    After::Nothing => {}
                    After::Push => self.run(Job::Push(false)),
                    After::Sync => self.run(Job::Sync),
                }
            }
            Some(Err(e)) => self.say(Ask::Notice(one_line(&e.to_string()))),
            None => return,
        }
        self.refresh();
    }

    // Branches ---------------------------------------------------------------

    /// The branch list, in whichever role it was opened for. Built on every
    /// open: branches move, and this is the one place they are read.
    ///
    /// A branch's own actions are a second page of the same popover rather
    /// than a menu on top of it: a popover holds the pointer grab, so a
    /// second one opened over it never appears.
    fn branch_menu(self: &Rc<Self>, mode: Pick) -> gtk::Popover {
        let pop = gtk::Popover::builder()
            .css_classes(["git-branch-menu"])
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .transition_duration(120)
            .build();
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let current = self
            .with(|at| at.repo.current_branch().unwrap_or_default())
            .unwrap_or_default();
        if mode != Pick::Switch {
            outer.append(&heading(&match mode {
                Pick::Merge => format!("MERGE INTO {current}"),
                _ => format!("REBASE {current} ONTO"),
            }));
        }
        let filter = gtk::Entry::builder()
            .placeholder_text("Find a branch…")
            .css_classes(["set-entry"])
            .build();
        outer.append(&filter);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let locals = self
            .with(|at| at.repo.branches(&at.root).unwrap_or_default())
            .unwrap_or_default();
        let remotes = self
            .with(|at| at.repo.remote_branches(&at.root).unwrap_or_default())
            .unwrap_or_default();
        // Row widgets keyed by name, so typing can hide the rest.
        let mut rows: Vec<(String, gtk::Widget)> = Vec::new();
        if !locals.is_empty() {
            let head = heading("LOCAL");
            list.append(&head);
            rows.push((String::new(), head.upcast()));
        }
        for b in locals.iter().take(60) {
            let row = self.branch_row(b, mode, &pop, &stack, false);
            list.append(&row);
            rows.push((b.name.clone(), row.upcast()));
        }
        if !remotes.is_empty() {
            let head = heading("REMOTE");
            list.append(&head);
            rows.push((String::new(), head.upcast()));
        }
        for b in remotes.iter().take(60) {
            let row = self.branch_row(b, mode, &pop, &stack, true);
            list.append(&row);
            rows.push((b.name.clone(), row.upcast()));
        }
        filter.connect_changed(move |entry| {
            let needle = entry.text().to_lowercase();
            for (name, row) in &rows {
                // Section headings stay put; they are keyed with no name.
                row.set_visible(name.is_empty() || name.to_lowercase().contains(&needle));
            }
        });
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(280)
            .child(&list)
            .build();
        outer.append(&scroller);

        if mode == Pick::Switch {
            outer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let new = gtk::Entry::builder()
                .placeholder_text(format!("New branch from {current}…"))
                .css_classes(["set-entry"])
                .build();
            new.connect_activate({
                let (this, pop) = (self.clone(), pop.clone());
                move |entry| {
                    let name = entry.text().trim().to_string();
                    if name.is_empty() {
                        return;
                    }
                    entry.set_text("");
                    pop.popdown();
                    this.run(Job::Checkout(name, true));
                }
            });
            outer.append(&new);
        }
        stack.add_named(&outer, Some("list"));
        pop.set_child(Some(&stack));
        pop
    }

    fn branch_row(
        self: &Rc<Self>,
        b: &taix_git::Branch,
        mode: Pick,
        pop: &gtk::Popover,
        stack: &gtk::Stack,
        remote: bool,
    ) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        // The branch you are on is the one you cannot pick, so it says so
        // rather than just refusing the click.
        line.append(
            &gtk::Label::builder()
                .label(if b.current { "●" } else { " " })
                .css_classes(["git-branch-here"])
                .build(),
        );
        line.append(
            &gtk::Label::builder()
                .label(&b.name)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .tooltip_text(&b.name)
                .css_classes(["mono"])
                .build(),
        );
        let track = track_line(b);
        if !track.is_empty() {
            line.append(
                &gtk::Label::builder()
                    .label(&track)
                    .css_classes(["git-drift", "mono"])
                    .build(),
            );
        }
        if b.when > 0 {
            line.append(
                &gtk::Label::builder()
                    .label(taix_core::jobs::format_local(
                        b.when,
                        taix_core::jobs::now(),
                    ))
                    .css_classes(["git-when"])
                    .build(),
            );
        }
        // Checking out the branch you are on, or merging it into itself, is
        // the one thing the list cannot do.
        let pick = gtk::Button::builder()
            .child(&line)
            .hexpand(true)
            .sensitive(!b.current)
            .css_classes(["set-pick-item"])
            .build();
        pick.connect_clicked({
            let (this, pop) = (self.clone(), pop.clone());
            let name = local_name(&b.name, remote);
            let full = b.name.clone();
            move |_| {
                pop.popdown();
                match mode {
                    // A remote branch is checked out by its short name: git
                    // creates the local branch tracking it.
                    Pick::Switch => this.run(Job::Checkout(name.clone(), false)),
                    Pick::Merge => this.run(Job::Merge(full.clone(), false)),
                    Pick::Rebase => this.run(Job::Rebase(full.clone())),
                }
            }
        });
        row.append(&pick);

        if mode == Pick::Switch {
            let more = icon_tool("view-more-symbolic", "What else this branch can do", false);
            more.connect_clicked({
                let (this, pop, stack, b) = (self.clone(), pop.clone(), stack.clone(), b.clone());
                move |_| {
                    let page = this.branch_actions(&b, remote, &pop, &stack);
                    if let Some(old) = stack.child_by_name("actions") {
                        stack.remove(&old);
                    }
                    stack.add_named(&page, Some("actions"));
                    stack.set_visible_child_name("actions");
                }
            });
            row.append(&more);
        }
        row
    }

    /// The second page of the branch popover: what this one branch can do.
    fn branch_actions(
        self: &Rc<Self>,
        b: &taix_git::Branch,
        remote: bool,
        pop: &gtk::Popover,
        stack: &gtk::Stack,
    ) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let back = gtk::Button::builder()
            .child(&{
                let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                face.append(&gtk::Label::new(Some("‹")));
                face.append(
                    &gtk::Label::builder()
                        .label(&b.name)
                        .xalign(0.0)
                        .hexpand(true)
                        .ellipsize(gtk::pango::EllipsizeMode::Middle)
                        .css_classes(["mono"])
                        .build(),
                );
                face
            })
            .css_classes(["set-pick-item"])
            .build();
        back.connect_clicked({
            let stack = stack.clone();
            move |_| stack.set_visible_child_name("list")
        });
        page.append(&back);
        page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let name = b.name.clone();
        let short = local_name(&name, remote);
        let on_it = b.current;
        let act = |this: &Rc<Self>, pop: &gtk::Popover, job: Job| {
            let (pop, this) = (pop.clone(), this.clone());
            move || {
                pop.popdown();
                this.run(job.clone());
            }
        };
        page.append(&item_list(
            vec![
                item(
                    "Merge into current",
                    act(self, pop, Job::Merge(name.clone(), false)),
                )
                .live(!on_it),
                item(
                    "Merge with a merge commit",
                    act(self, pop, Job::Merge(name.clone(), true)),
                )
                .live(!on_it),
                item(
                    "Rebase current onto this",
                    act(self, pop, Job::Rebase(name.clone())),
                )
                .live(!on_it),
                sep(),
                item("Rename…", {
                    let (this, short, pop) = (self.clone(), short.clone(), pop.clone());
                    move || {
                        pop.popdown();
                        this.rename_branch(&short);
                    }
                })
                .live(!remote),
                item("Delete", {
                    let (this, short, pop) = (self.clone(), short.clone(), pop.clone());
                    move || {
                        pop.popdown();
                        this.delete_branch(&short, false);
                    }
                })
                .live(!remote && !on_it)
                .danger(),
                item("Delete even if unmerged", {
                    let (this, short, pop) = (self.clone(), short.clone(), pop.clone());
                    move || {
                        pop.popdown();
                        this.delete_branch(&short, true);
                    }
                })
                .live(!remote && !on_it)
                .danger(),
            ],
            None,
        ));
        page
    }

    fn rename_branch(self: &Rc<Self>, old: &str) {
        let this = self.clone();
        let name = old.to_string();
        crate::ui::prompt_text(&self.root, "Rename branch", old, "Rename", move |new| {
            this.run(Job::RenameBranch(name.clone(), new));
        });
    }

    fn delete_branch(self: &Rc<Self>, name: &str, force: bool) {
        let (this, name) = (self.clone(), name.to_string());
        let body = if force {
            format!("{name} is deleted even if its commits are on no other branch.")
        } else {
            format!("{name} is deleted. Merged commits stay where they were merged.")
        };
        crate::ui::confirm(&self.root, "Delete branch?", &body, "Delete", move || {
            this.run(Job::DeleteBranch(name.clone(), force))
        });
    }

    // Stashes -----------------------------------------------------------------

    fn fill_stashes(self: &Rc<Self>, stashes: &[taix_git::Stash]) {
        clear(&self.stashes);
        if stashes.is_empty() {
            return;
        }
        let body = self.section(
            &self.stashes,
            "STASHES",
            "STASHES",
            Some(stashes.len()),
            &[],
        );
        let now = taix_core::jobs::now();
        for stash in stashes {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(2)
                .css_classes(["git-file"])
                .build();
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.append(
                &gtk::Label::builder()
                    .label(format!("@{}", stash.index))
                    .css_classes(["git-hash", "mono"])
                    .build(),
            );
            let said = stash_message(stash);
            line.append(
                &gtk::Label::builder()
                    .label(&said)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .tooltip_text(format!("{} — on {}", said, stash.branch))
                    .css_classes(["git-path"])
                    .build(),
            );
            if stash.when > 0 {
                line.append(
                    &gtk::Label::builder()
                        .label(taix_core::jobs::format_local(stash.when, now))
                        .css_classes(["git-when"])
                        .build(),
                );
            }
            let head = gtk::Button::builder()
                .child(&line)
                .hexpand(true)
                .tooltip_text("Apply this stash, keeping it")
                .css_classes(["git-file-head"])
                .build();
            head.connect_clicked({
                let (this, index) = (self.clone(), stash.index);
                move |_| this.run(Job::Apply(index))
            });
            row.append(&head);
            let pop_it = line_button("Pop");
            pop_it.set_tooltip_text(Some("Apply it and drop it"));
            pop_it.connect_clicked({
                let (this, index) = (self.clone(), stash.index);
                move |_| this.run(Job::Pop(index))
            });
            row.append(&pop_it);
            let drop_it = icon_tool("user-trash-symbolic", "Throw this stash away", true);
            drop_it.connect_clicked({
                let (this, index, what) = (self.clone(), stash.index, said.clone());
                move |_| {
                    let this2 = this.clone();
                    crate::ui::confirm(
                        &this.root,
                        "Drop this stash?",
                        &format!("{what} cannot be recovered from the panel."),
                        "Drop",
                        move || this2.run(Job::Drop(index)),
                    );
                }
            });
            row.append(&drop_it);
            body.append(&row);
        }
    }

    // History ------------------------------------------------------------------

    fn fill_history(self: &Rc<Self>, commits: &[taix_git::Commit], more: bool) {
        clear(&self.history);
        if commits.is_empty() {
            return;
        }
        let body = self.section(&self.history, "HISTORY", "HISTORY", None, &[]);
        let now = taix_core::jobs::now();
        let opened = self.opened.borrow().clone();
        for c in commits {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.append(
                &gtk::Label::builder()
                    .label(&c.short)
                    .css_classes(["git-hash", "mono"])
                    .build(),
            );
            line.append(
                &gtk::Label::builder()
                    .label(&c.summary)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build(),
            );
            line.append(
                &gtk::Label::builder()
                    .label(taix_core::jobs::format_local(c.when, now))
                    .css_classes(["git-when"])
                    .build(),
            );
            let row = gtk::Button::builder()
                .child(&line)
                .tooltip_text(format!("{} — {}", c.author, c.summary))
                .css_classes(["git-file-head"])
                .build();
            // Clicking opens what the commit touched; the whole diff is one
            // step further in, because on a large commit it is unreadable.
            row.connect_clicked({
                let (this, hash) = (self.clone(), c.hash.clone());
                move |_| {
                    let mut open = this.opened.borrow_mut();
                    *open = match open.as_deref() {
                        Some(current) if current == hash => None,
                        _ => Some(hash.clone()),
                    };
                    drop(open);
                    this.refresh();
                }
            });
            self.on_right_click(&row, {
                let (this, c) = (self.clone(), c.clone());
                move |row, x, y| this.history_menu(row, x, y, &c)
            });
            body.append(&row);
            if opened.as_deref() == Some(c.hash.as_str()) {
                body.append(&self.commit_files(c));
            }
        }
        if more {
            let button = line_button("Show more");
            button.set_halign(gtk::Align::Start);
            button.connect_clicked({
                let this = self.clone();
                move |_| {
                    this.shown.set(this.shown.get() + PAGE);
                    this.refresh();
                }
            });
            body.append(&button);
        }
    }

    /// The files one commit touched, listed under it. Each opens that
    /// commit's diff of that file alone.
    fn commit_files(self: &Rc<Self>, c: &taix_git::Commit) -> gtk::Box {
        let box_ = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["git-commit-files"])
            .build();
        let files = self
            .with(|at| at.repo.commit_files(&at.root, &c.hash).unwrap_or_default())
            .unwrap_or_default();
        let whole = line_button("Whole diff");
        whole.set_halign(gtk::Align::Start);
        whole.connect_clicked({
            let (this, hash, title) = (
                self.clone(),
                c.hash.clone(),
                format!("{} {}", c.short, c.summary),
            );
            move |_| this.show_commit(&hash, &title)
        });
        box_.append(&whole);
        for f in files {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.append(
                &gtk::Label::builder()
                    .label(f.status.to_string())
                    .width_chars(1)
                    .css_classes(["git-letter", "git-staged", "mono"])
                    .build(),
            );
            line.append(
                &gtk::Label::builder()
                    .label(&f.path)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::Start)
                    .tooltip_text(&f.path)
                    .css_classes(["git-path", "mono"])
                    .build(),
            );
            let button = gtk::Button::builder()
                .child(&line)
                .css_classes(["git-file-head"])
                .build();
            button.connect_clicked({
                let (this, hash, path, short) = (
                    self.clone(),
                    c.hash.clone(),
                    f.path.clone(),
                    c.short.clone(),
                );
                move |_| {
                    let text =
                        this.with(|at| at.repo.commit_file_diff(&at.root, &hash, &path, DIFF_CAP));
                    match text {
                        Some(Ok(diff)) => this.say(Ask::Diff(format!("{short} {path}"), diff)),
                        Some(Err(e)) => this.say(Ask::Notice(one_line(&e.to_string()))),
                        None => {}
                    }
                }
            });
            box_.append(&button);
        }
        box_
    }

    fn show_commit(self: &Rc<Self>, hash: &str, title: &str) {
        let text = self.with(|at| at.repo.commit_diff(&at.root, hash, DIFF_CAP));
        match text {
            Some(Ok(diff)) => self.say(Ask::Diff(title.to_string(), diff)),
            Some(Err(e)) => self.say(Ask::Notice(one_line(&e.to_string()))),
            None => {}
        }
    }

    fn history_menu(self: &Rc<Self>, row: &gtk::Button, x: f64, y: f64, c: &taix_git::Commit) {
        let title = format!("{} {}", c.short, c.summary);
        let items = vec![
            item("Show diff", {
                let (this, hash, title) = (self.clone(), c.hash.clone(), title.clone());
                move || this.show_commit(&hash, &title)
            }),
            item("Copy hash", {
                let (this, hash) = (self.clone(), c.hash.clone());
                move || this.copy(&hash)
            }),
            item("Copy message", {
                let (this, summary) = (self.clone(), c.summary.clone());
                move || this.copy(&summary)
            }),
            sep(),
            item("Revert this commit", {
                let (this, hash) = (self.clone(), c.hash.clone());
                move || this.run(Job::Revert(hash.clone()))
            }),
            item("Cherry-pick onto this branch", {
                let (this, hash) = (self.clone(), c.hash.clone());
                move || this.run(Job::CherryPick(hash.clone()))
            }),
            sep(),
            item("Reset here, keep the changes", {
                let (this, hash) = (self.clone(), c.hash.clone());
                move || this.run(Job::Reset(hash.clone(), Reset::Soft))
            }),
            item("Reset here, throw the changes away", {
                let (this, hash, short) = (self.clone(), c.hash.clone(), c.short.clone());
                move || {
                    let (this2, hash) = (this.clone(), hash.clone());
                    crate::ui::confirm(
                        &this.root,
                        "Reset hard?",
                        &format!("Everything after {short}, committed or not, is gone."),
                        "Reset",
                        move || this2.run(Job::Reset(hash.clone(), Reset::Hard)),
                    );
                }
            })
            .danger(),
        ];
        self.menu(row, Some((x, y)), items);
    }

    // The overflow menu ---------------------------------------------------------

    fn more_menu(self: &Rc<Self>) -> gtk::Popover {
        let stashed = self
            .with(|at| at.repo.stash_list(&at.root).map(|s| s.len()).unwrap_or(0))
            .unwrap_or(0);
        let dirty = self
            .with(|at| at.repo.changes(&at.root).map(|c| c.len()).unwrap_or(0))
            .unwrap_or(0);
        let remote = self
            .with(|at| at.repo.remote_url(&at.root).ok().flatten())
            .flatten();
        self.build_menu(vec![
            item("Fetch", {
                let this = self.clone();
                move || this.run(Job::Fetch(false))
            }),
            item("Fetch and prune", {
                let this = self.clone();
                move || this.run(Job::Fetch(true))
            }),
            item("Force push (with lease)", {
                let this = self.clone();
                move || {
                    let this2 = this.clone();
                    crate::ui::confirm(
                        &this.root,
                        "Force push?",
                        "The remote branch is replaced by this one. Anything pushed there since \
                         your last fetch is refused, but everything else is overwritten.",
                        "Force push",
                        move || this2.run(Job::Push(true)),
                    );
                }
            })
            .danger(),
            sep(),
            item("Merge a branch into this one…", {
                let this = self.clone();
                move || this.pick_branch(Pick::Merge)
            }),
            item("Rebase this branch onto…", {
                let this = self.clone();
                move || this.pick_branch(Pick::Rebase)
            }),
            sep(),
            item("Stash every change", {
                let this = self.clone();
                move || {
                    let note = this.message_text().trim().to_string();
                    this.run(Job::Stash(note))
                }
            })
            .live(dirty > 0),
            item("Pop the newest stash", {
                let this = self.clone();
                move || this.run(Job::Pop(0))
            })
            .live(stashed > 0),
            sep(),
            item("Unstage everything", {
                let this = self.clone();
                move || {
                    let failed =
                        this.with(|at| at.repo.unstage_all(&at.root).err().map(|e| e.to_string()));
                    this.report(failed.flatten());
                }
            })
            .live(dirty > 0),
            item("Discard every change…", {
                let this = self.clone();
                move || this.discard_all(false)
            })
            .live(dirty > 0)
            .danger(),
            item("Discard everything, untracked too…", {
                let this = self.clone();
                move || this.discard_all(true)
            })
            .live(dirty > 0)
            .danger(),
            sep(),
            item("Copy the remote URL", {
                let (this, remote) = (self.clone(), remote.clone().unwrap_or_default());
                move || this.copy(&remote)
            })
            .live(remote.is_some()),
        ])
    }

    /// Open the branch list anchored on the tools, for a merge or a rebase.
    fn pick_branch(self: &Rc<Self>, mode: Pick) {
        let pop = self.branch_menu(mode);
        pop.set_parent(&self.root);
        if let Some(bounds) = self.tools.compute_bounds(&self.root) {
            pop.set_pointing_to(Some(&gdk::Rectangle::new(
                bounds.x() as i32,
                bounds.y() as i32,
                bounds.width() as i32,
                bounds.height() as i32,
            )));
        }
        self.hold(&pop);
        pop.popup();
    }

    fn discard_all(self: &Rc<Self>, untracked: bool) {
        let this = self.clone();
        let body = if untracked {
            "Every uncommitted change is lost and every untracked file is deleted."
        } else {
            "Every uncommitted change to a tracked file is lost."
        };
        crate::ui::confirm(
            &self.root,
            "Discard everything?",
            body,
            "Discard",
            move || this.run(Job::DiscardAll(untracked)),
        );
    }

    // Menus ----------------------------------------------------------------------

    /// Right-click anywhere on `on`, get `f`.
    fn on_right_click<W: IsA<gtk::Widget> + Clone + 'static>(
        self: &Rc<Self>,
        on: &W,
        f: impl Fn(&W, f64, f64) + 'static,
    ) {
        let gesture = gtk::GestureClick::builder().button(3).build();
        gesture.connect_pressed({
            let on = on.clone();
            move |_, _, x, y| f(&on, x, y)
        });
        on.add_controller(gesture);
    }

    /// A menu, parented to the panel rather than to the row it came from:
    /// the row is gone the moment the action redraws, and a popover whose
    /// parent died takes the warning with it.
    fn menu(
        self: &Rc<Self>,
        anchor: &impl IsA<gtk::Widget>,
        at: Option<(f64, f64)>,
        items: Vec<Item>,
    ) {
        let pop = self.build_menu(items);
        pop.set_has_arrow(at.is_none());
        pop.set_parent(&self.root);
        let rect = match at {
            Some((x, y)) => {
                let point = gtk::graphene::Point::new(x as f32, y as f32);
                let at = anchor
                    .as_ref()
                    .compute_point(&self.root, &point)
                    .unwrap_or(point);
                gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1)
            }
            None => match anchor.as_ref().compute_bounds(&self.root) {
                Some(b) => gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    b.width() as i32,
                    b.height() as i32,
                ),
                None => gdk::Rectangle::new(0, 0, 1, 1),
            },
        };
        pop.set_pointing_to(Some(&rect));
        pop.set_position(gtk::PositionType::Bottom);
        self.hold(&pop);
        pop.popup();
    }

    /// Hold the polling off while a menu is up, and take the popover back
    /// out of the widget tree when it closes.
    fn hold(self: &Rc<Self>, pop: &gtk::Popover) {
        let open = self.menu.clone();
        open.set(true);
        pop.connect_closed(move |pop| {
            open.set(false);
            // Not during the close itself: GTK is still walking the popover.
            let pop = pop.clone();
            glib::idle_add_local_once(move || pop.unparent());
        });
    }

    fn build_menu(self: &Rc<Self>, items: Vec<Item>) -> gtk::Popover {
        let pop = gtk::Popover::builder().css_classes(["git-menu"]).build();
        pop.set_child(Some(&item_list(items, Some(pop.clone()))));
        pop
    }

    /// A foldable section, appended to `into`. Returns the body to fill.
    fn section(
        self: &Rc<Self>,
        into: &gtk::Box,
        key: &str,
        title: &str,
        count: Option<usize>,
        tools: &[gtk::Widget],
    ) -> gtk::Box {
        let folded = self.folded.borrow().contains(key);
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .css_classes(["git-group"])
            .build();
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let chevron = gtk::Label::builder()
            .label(if folded { "▸" } else { "▾" })
            .css_classes(["git-fold"])
            .build();
        face.append(&chevron);
        face.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["git-group-title"])
                .build(),
        );
        if let Some(n) = count {
            face.append(
                &gtk::Label::builder()
                    .label(n.to_string())
                    .css_classes(["git-group-count"])
                    .build(),
            );
        }
        let toggle = gtk::Button::builder()
            .child(&face)
            .hexpand(true)
            .css_classes(["git-file-head"])
            .build();
        head.append(&toggle);
        for tool in tools {
            head.append(tool);
        }
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(!folded)
            .build();
        toggle.connect_clicked({
            let (this, key, body, chevron) =
                (self.clone(), key.to_string(), body.clone(), chevron.clone());
            move |_| {
                let open = !body.is_visible();
                body.set_visible(open);
                chevron.set_label(if open { "▾" } else { "▸" });
                let mut folded = this.folded.borrow_mut();
                if open {
                    folded.remove(&key);
                } else {
                    folded.insert(key.clone());
                }
            }
        });
        into.append(&head);
        into.append(&body);
        body
    }

    // Running git ------------------------------------------------------------------

    /// Everything that rewrites the tree or talks to a remote, off the main
    /// thread. The repository is reopened in the worker rather than shared:
    /// `Repo` caches a branch lookup and is not `Sync`, and opening is one
    /// process against work that is about to do several.
    fn run(self: &Rc<Self>, job: Job) {
        if self.busy.get() {
            return;
        }
        let Some(root) = self.with(|at| at.root.clone()) else {
            return;
        };
        self.busy.set(true);
        self.tools.set_sensitive(false);
        self.banner_tools.set_sensitive(false);
        self.drift.set_label(job.doing());

        let this = self.clone();
        glib::spawn_future_local(async move {
            let done = gio::spawn_blocking(move || {
                let repo = Repo::open(&root).map_err(|e| e.to_string())?;
                job.run(&repo, &root).map_err(|e| e.to_string())
            })
            .await;
            this.busy.set(false);
            this.tools.set_sensitive(true);
            this.banner_tools.set_sensitive(true);
            match done {
                Ok(Ok(said)) if said.trim().is_empty() => {}
                Ok(Ok(said)) => this.say(Ask::Notice(one_line(&said))),
                Ok(Err(e)) => this.say(Ask::Notice(one_line(&e))),
                // The worker panicked: the message is in the log, and the
                // panel must still come back to life.
                Err(_) => this.say(Ask::Notice("git did not finish".to_string())),
            }
            this.refresh();
        });
    }
}

/// What to do once a commit lands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    Push,
    Sync,
}

/// Work that runs off the main thread. Anything that only touches the index
/// is done inline instead: it is one fast process and the redraw wants the
/// result immediately.
#[derive(Clone)]
enum Job {
    Fetch(bool),
    Pull,
    Push(bool),
    Sync,
    Stash(String),
    Pop(usize),
    Apply(usize),
    Drop(usize),
    Merge(String, bool),
    Rebase(String),
    Step(Step),
    Revert(String),
    CherryPick(String),
    Reset(String, Reset),
    Checkout(String, bool),
    DeleteBranch(String, bool),
    RenameBranch(String, String),
    DiscardAll(bool),
}

impl Job {
    /// What the drift label says while this is in flight.
    fn doing(&self) -> &'static str {
        match self {
            Job::Fetch(_) => "fetching…",
            Job::Pull => "pulling…",
            Job::Push(_) => "pushing…",
            Job::Sync => "syncing…",
            Job::Stash(_) => "stashing…",
            Job::Pop(_) | Job::Apply(_) => "restoring…",
            Job::Drop(_) => "dropping…",
            Job::Merge(..) => "merging…",
            Job::Rebase(_) => "rebasing…",
            Job::Step(_) => "working…",
            Job::Revert(_) => "reverting…",
            Job::CherryPick(_) => "cherry-picking…",
            Job::Reset(..) => "resetting…",
            Job::Checkout(..) => "switching…",
            Job::DeleteBranch(..) => "deleting…",
            Job::RenameBranch(..) => "renaming…",
            Job::DiscardAll(_) => "discarding…",
        }
    }

    fn run(self, repo: &Repo, root: &Path) -> taix_git::Result<String> {
        Ok(match self {
            Job::Fetch(prune) => {
                repo.fetch(root, prune)?;
                if prune {
                    "fetched, pruned".into()
                } else {
                    "fetched".into()
                }
            }
            Job::Pull => repo.pull(root)?,
            Job::Push(force) => repo.push(root, true, force)?,
            Job::Sync => {
                let pulled = repo.pull(root)?;
                let pushed = repo.push(root, true, false)?;
                let said: Vec<String> = [pulled, pushed]
                    .into_iter()
                    .map(|s| one_line(&s))
                    .filter(|s| !s.is_empty())
                    .collect();
                said.join(" · ")
            }
            Job::Stash(note) => {
                let note = if note.is_empty() {
                    "taix".to_string()
                } else {
                    note
                };
                repo.stash_push(root, &note)?;
                "stashed".into()
            }
            Job::Pop(index) => {
                repo.stash_pop(root, index)?;
                "stash restored".into()
            }
            Job::Apply(index) => {
                repo.stash_apply(root, index)?;
                "stash applied".into()
            }
            Job::Drop(index) => {
                repo.stash_drop(root, index)?;
                "stash dropped".into()
            }
            Job::Merge(branch, no_ff) => {
                let said = repo.merge(root, &branch, no_ff)?;
                if said.trim().is_empty() {
                    format!("merged {branch}")
                } else {
                    said
                }
            }
            Job::Rebase(onto) => {
                repo.rebase(root, &onto)?;
                format!("rebased onto {onto}")
            }
            Job::Step(step) => repo.step(root, step)?,
            Job::Revert(hash) => {
                repo.revert(root, &hash)?;
                "reverted".into()
            }
            Job::CherryPick(hash) => {
                repo.cherry_pick(root, &hash)?;
                "cherry-picked".into()
            }
            Job::Reset(hash, mode) => {
                repo.reset(root, &hash, mode)?;
                "reset".into()
            }
            Job::Checkout(branch, create) => {
                repo.checkout(root, &branch, create)?;
                format!("on {branch}")
            }
            Job::DeleteBranch(name, force) => {
                repo.delete_branch(root, &name, force)?;
                format!("deleted {name}")
            }
            Job::RenameBranch(old, new) => {
                repo.rename_branch(root, &old, &new)?;
                format!("{old} is now {new}")
            }
            Job::DiscardAll(untracked) => {
                repo.discard_all(root, untracked)?;
                "discarded".into()
            }
        })
    }
}

/// One entry in a menu. A separator is an entry with nothing to do.
struct Item {
    label: String,
    danger: bool,
    live: bool,
    act: Option<Box<dyn Fn()>>,
}

fn item(label: impl Into<String>, act: impl Fn() + 'static) -> Item {
    Item {
        label: label.into(),
        danger: false,
        live: true,
        act: Some(Box::new(act)),
    }
}

fn sep() -> Item {
    Item {
        label: String::new(),
        danger: false,
        live: false,
        act: None,
    }
}

impl Item {
    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Shown but refused, rather than hidden: a missing entry reads as a
    /// missing feature, a greyed one reads as "not right now".
    fn live(mut self, yes: bool) -> Self {
        self.live = yes;
        self
    }
}

/// Items as a column of buttons. `close` is the popover they live in, if
/// they live in one: a menu dismisses itself before it acts, a drill-down
/// page inside an open popover does not.
fn item_list(items: Vec<Item>, close: Option<gtk::Popover>) -> gtk::Box {
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(1)
        .build();
    for entry in items {
        let Some(act) = entry.act else {
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            continue;
        };
        let label = gtk::Label::builder()
            .label(&entry.label)
            .xalign(0.0)
            .hexpand(true)
            .build();
        let button = gtk::Button::builder()
            .child(&label)
            .sensitive(entry.live)
            .css_classes(["set-pick-item"])
            .build();
        if entry.danger {
            button.add_css_class("danger");
        }
        button.connect_clicked({
            let close = close.clone();
            move |_| {
                if let Some(pop) = &close {
                    pop.popdown();
                }
                act();
            }
        });
        list.append(&button);
    }
    list
}

/// Which list a change belongs in. A file can be in two at once - staged
/// edits plus newer unstaged ones - and then it is listed in both, because
/// that is exactly what will and will not be committed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Conflicted,
    Staged,
    Changed,
    Untracked,
}

impl Group {
    fn holds(self, f: &FileChange) -> bool {
        match self {
            Group::Conflicted => f.conflicted,
            Group::Staged => f.staged && !f.conflicted,
            Group::Changed => f.unstaged && !f.untracked && !f.conflicted,
            Group::Untracked => f.untracked && !f.conflicted,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Group::Conflicted => "CONFLICTED",
            Group::Staged => "STAGED",
            Group::Changed => "CHANGED",
            Group::Untracked => "UNTRACKED",
        }
    }

    /// The one bulk action that makes sense for the group.
    fn bulk(self) -> Option<&'static str> {
        match self {
            Group::Staged => Some("Unstage all"),
            Group::Changed | Group::Untracked => Some("Stage all"),
            Group::Conflicted => None,
        }
    }

    fn bulk_is_stage(self) -> bool {
        self != Group::Staged
    }
}

/// The letter git itself would print, picked from whichever column applies.
fn status_letter(f: &FileChange) -> String {
    if f.conflicted {
        return "U".to_string();
    }
    if f.untracked {
        return "?".to_string();
    }
    let letter = if f.staged { f.index } else { f.work };
    match letter {
        ' ' | '\0' => "•".to_string(),
        other => other.to_string(),
    }
}

fn letter_class(f: &FileChange) -> &'static str {
    if f.conflicted {
        "git-conflict"
    } else if f.untracked {
        "git-new"
    } else if f.staged {
        "git-staged"
    } else {
        "git-dirty"
    }
}

/// Ahead, behind and how many files are dirty, in the bar's own shorthand.
fn drift_line(status: &taix_git::Status) -> String {
    let mut parts = Vec::new();
    if status.ahead > 0 {
        parts.push(format!("↑{}", status.ahead));
    }
    if status.behind > 0 {
        parts.push(format!("↓{}", status.behind));
    }
    if status.dirty > 0 {
        parts.push(format!("{}~", status.dirty));
    }
    parts.join(" ")
}

/// What the sync button says it will move. Naming the traffic is the whole
/// reason to have one button instead of two.
fn sync_label(status: &taix_git::Status) -> String {
    match (status.ahead, status.behind) {
        (0, 0) => "Sync".to_string(),
        (a, 0) => format!("Sync ↑{a}"),
        (0, b) => format!("Sync ↓{b}"),
        (a, b) => format!("Sync ↑{a} ↓{b}"),
    }
}

/// A branch's own drift from its upstream, for the branch list.
fn track_line(b: &taix_git::Branch) -> String {
    match (b.ahead, b.behind) {
        (0, 0) => String::new(),
        (a, 0) => format!("↑{a}"),
        (0, b) => format!("↓{b}"),
        (a, b) => format!("↑{a} ↓{b}"),
    }
}

/// What a stash is *about*. Git writes `WIP on main: 1a2b3c summary` or
/// `On main: my note`; the branch is already a column of its own, so only
/// the part the author chose is worth the width.
fn stash_message(stash: &taix_git::Stash) -> String {
    let text = stash.message.trim();
    let tail = text
        .strip_prefix("WIP on ")
        .or_else(|| text.strip_prefix("On "))
        .and_then(|rest| rest.split_once(": "))
        .map(|(_, said)| said.trim());
    match tail {
        Some(said) if !said.is_empty() => said.to_string(),
        _ => text.to_string(),
    }
}

/// The branch a remote ref would be checked out as: `origin/topic` is
/// `topic`, and git creates it tracking the remote.
fn local_name(name: &str, remote: bool) -> String {
    match remote {
        true => name
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(name)
            .to_string(),
        false => name.to_string(),
    }
}

/// Git says things in paragraphs; the status bar has one line.
fn one_line(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or("").to_string();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

fn tool_button(label: &str, tip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .hexpand(true)
        .css_classes(["git-tool"])
        .build();
    if !tip.is_empty() {
        button.set_tooltip_text(Some(tip));
    }
    button
}

/// A row-level button that only shows itself when the row is under the
/// pointer - the CSS does the hiding, so the layout never jumps.
fn line_button(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .css_classes(["git-line-tool"])
        .build()
}

fn icon_tool(icon: &str, tip: &str, danger: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tip)
        .css_classes(["git-line-tool"])
        .build();
    if danger {
        button.add_css_class("danger");
    }
    button
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["git-group-title"])
        .build()
}

fn clear(box_: &gtk::Box) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(index: char, work: char) -> FileChange {
        FileChange {
            path: "src/main.rs".to_string(),
            index,
            work,
            staged: index != ' ' && index != '?',
            unstaged: work != ' ' && work != '?',
            untracked: index == '?',
            conflicted: false,
            renamed_from: None,
        }
    }

    #[test]
    fn a_file_staged_and_then_edited_again_is_in_both_lists() {
        let f = change('M', 'M');
        assert!(Group::Staged.holds(&f));
        assert!(Group::Changed.holds(&f));
        assert!(!Group::Untracked.holds(&f));
    }

    #[test]
    fn a_conflict_is_only_ever_a_conflict() {
        let mut f = change('U', 'U');
        f.conflicted = true;
        assert!(Group::Conflicted.holds(&f));
        for group in [Group::Staged, Group::Changed, Group::Untracked] {
            assert!(!group.holds(&f), "{}", group.title());
        }
        assert_eq!(status_letter(&f), "U");
    }

    #[test]
    fn the_letter_follows_the_column_that_applies() {
        // Staged add, no further edits: the index column is the news.
        assert_eq!(status_letter(&change('A', ' ')), "A");
        // Unstaged edit only: the worktree column is.
        assert_eq!(status_letter(&change(' ', 'D')), "D");
        assert_eq!(status_letter(&change('?', '?')), "?");
    }

    #[test]
    fn drift_reads_as_the_bar_writes_it() {
        let mut status = taix_git::Status::default();
        assert_eq!(drift_line(&status), "");
        status.ahead = 2;
        status.dirty = 3;
        assert_eq!(drift_line(&status), "↑2 3~");
        status.behind = 1;
        assert_eq!(drift_line(&status), "↑2 ↓1 3~");
    }

    #[test]
    fn the_sync_button_names_the_traffic_and_ignores_the_dirt() {
        // Uncommitted work is not something sync moves, so it must not
        // appear on the button.
        let mut status = taix_git::Status {
            dirty: 4,
            ..Default::default()
        };
        assert_eq!(sync_label(&status), "Sync");
        status.behind = 2;
        assert_eq!(sync_label(&status), "Sync ↓2");
        status.ahead = 1;
        assert_eq!(sync_label(&status), "Sync ↑1 ↓2");
    }

    #[test]
    fn a_remote_ref_checks_out_under_its_own_short_name() {
        assert_eq!(local_name("origin/feature/panel", true), "feature/panel");
        assert_eq!(local_name("feature/panel", false), "feature/panel");
        // No slash at all: there is nothing to strip.
        assert_eq!(local_name("origin", true), "origin");
    }

    #[test]
    fn a_git_paragraph_becomes_one_line() {
        assert_eq!(one_line("  already up to date.  "), "already up to date.");
        assert_eq!(
            one_line("error: cannot pull\n\nhint: commit first\n"),
            "error: cannot pull …"
        );
        assert_eq!(one_line("\n\n"), "");
    }

    #[test]
    fn a_stash_row_shows_the_note_not_gits_preamble() {
        let stash = |message: &str| taix_git::Stash {
            index: 0,
            message: message.to_string(),
            branch: "main".to_string(),
            when: 0,
        };
        assert_eq!(
            stash_message(&stash("On main: a stash to look at")),
            "a stash to look at"
        );
        // An unnamed stash has git's own summary and nothing else, so the
        // summary is what there is to show.
        assert_eq!(
            stash_message(&stash("WIP on main: 1a2b3c4 add a library")),
            "1a2b3c4 add a library"
        );
        // Anything that is not in git's shape is left exactly as it is.
        assert_eq!(stash_message(&stash("plain note")), "plain note");
    }
}
