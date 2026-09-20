//! The project's own files, as a tree beside the panes.
//!
//! Not a file manager, but the things one reaches for while an agent is
//! writing: open what it just wrote, hand it a path, rename what it
//! misnamed, copy a fixture next to itself, throw a stray file away. Each is
//! one gesture from the row - a key, a drag, the menu - and every change is
//! read back from disk afterwards, never assumed.
//!
//! Only expanded directories are read, and only visible rows are built: a
//! `node_modules` a hundred thousand files deep costs nothing until someone
//! opens it, and then costs one screenful. A rebuild keeps every row whose
//! entry did not change, so opening a folder builds that folder's rows and
//! re-parents the rest.
//!
//! The panel owns the filesystem side. Anything that needs the config, the
//! focused pane or the status line - which editor, typing a path, a window
//! in a directory - is an [`Ask`] handed to the app.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::search::{self, Hit, Why};
use taix_core::editor::Editor;

/// Entries listed per directory before the rest are summarised. A generated
/// directory can hold tens of thousands of files, and no one reads them in a
/// 300px column; the "more" row shows the next page on click.
const PAGE: usize = 500;
/// Rows are indented by this much per level.
const INDENT: i32 = 13;

/// One row's worth of filesystem, already sorted and classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    /// Seconds since the epoch, or `None` if the entry has no readable mtime.
    pub mtime: Option<i64>,
}

/// Read one directory: directories first, then names, case-insensitively.
///
/// Errors are a reason to show nothing, never to fail: a directory that
/// cannot be read is one the agent cannot write either.
pub fn read_dir(dir: &Path, show_hidden: bool) -> Vec<Entry> {
    let Ok(iter) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = iter
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            let meta = e.metadata().ok();
            Some(Entry {
                dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                mtime: meta.as_ref().and_then(mtime_secs),
                path: e.path(),
                name,
            })
        })
        .collect();
    // One lowercase per entry, not two per comparison.
    out.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    out
}

fn mtime_secs(meta: &std::fs::Metadata) -> Option<i64> {
    let t = meta.modified().ok()?;
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs() as i64),
        // Before 1970: real on a restored archive, and not worth a branch
        // anywhere else.
        Err(e) => Some(-(e.duration().as_secs() as i64)),
    }
}

/// When something changed, at the precision a human wants: the clock for
/// today, the date for this year, the year for anything older.
pub fn human_mtime(secs: i64, now: i64) -> String {
    let Some(then) = glib::DateTime::from_unix_local(secs).ok() else {
        return String::new();
    };
    let Some(now) = glib::DateTime::from_unix_local(now).ok() else {
        return String::new();
    };
    let fmt = if then.ymd() == now.ymd() {
        "%H:%M"
    } else if then.year() == now.year() {
        "%d %b"
    } else {
        "%Y-%m"
    };
    then.format(fmt).map(Into::into).unwrap_or_default()
}

/// The symbolic icon GIO would give this file in a file manager, so a `.rs`,
/// a `.png` and a `.zip` do not all read as "generic file".
fn icon_for(name: &str, dir: bool, expanded: bool) -> gtk::Image {
    let icon = if dir {
        gio::ThemedIcon::new(if expanded {
            "folder-open-symbolic"
        } else {
            "folder-symbolic"
        })
        .upcast::<gio::Icon>()
    } else {
        let (content_type, _) = gio::content_type_guess(Some(name), None);
        gio::content_type_get_symbolic_icon(&content_type)
    };
    let image = gtk::Image::from_gicon(&icon);
    image.set_pixel_size(14);
    image.add_css_class(if dir { "file-dir" } else { "file-icon" });
    image
}

/// The name without its extension, in characters: what a rename selects,
/// since the extension is the part one almost never means to change.
fn stem_chars(name: &str) -> i32 {
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].chars().count() as i32,
        _ => name.chars().count() as i32,
    }
}

/// `name`, or `name (copy)`, `name (copy 2)`... - the first that is free in
/// `dir`. `symlink_metadata`, so a dangling link counts as taken.
pub fn unique(dir: &Path, name: &str) -> PathBuf {
    let taken = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    (1..)
        .map(|n| {
            dir.join(if n == 1 {
                format!("{stem} (copy){ext}")
            } else {
                format!("{stem} (copy {n}){ext}")
            })
        })
        .find(|p| !taken(p))
        .expect("an unbounded range always finds a free name")
}

/// Copy a file, a symlink as a symlink, or a directory tree.
///
/// ponytail: blocking, on the main thread. A paste of a `node_modules`
/// would need a thread and a progress row; nobody has asked.
pub fn copy_path(src: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dest)
    } else if meta.is_dir() {
        std::fs::create_dir(dest)?;
        for child in std::fs::read_dir(src)?.flatten() {
            copy_path(&child.path(), &dest.join(child.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dest).map(drop)
    }
}

/// Rename, or copy and remove when the two sit on different filesystems.
pub fn move_path(src: &Path, dest: &Path) -> std::io::Result<()> {
    if std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    copy_path(src, dest)?;
    if std::fs::symlink_metadata(src)?.is_dir() {
        std::fs::remove_dir_all(src)
    } else {
        std::fs::remove_file(src)
    }
}

/// Create an empty file, or a directory, at `path`; parents are made on the
/// way, so `a/b/c.rs` typed into the new-file row builds the path.
fn create(path: &Path, folder: bool) -> std::io::Result<()> {
    if folder {
        return std::fs::create_dir_all(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
}

/// Show the file selected in the desktop's file manager, falling back to
/// opening its folder when no file manager answers on the bus.
fn reveal(path: &Path) {
    let uri = gio::File::for_path(path).uri().to_string();
    let folder = gio::File::for_path(path.parent().unwrap_or(path)).uri();
    let fallback = move || {
        let _ = gio::AppInfo::launch_default_for_uri(&folder, None::<&gio::AppLaunchContext>);
    };
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        fallback();
        return;
    };
    bus.call(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        Some(&(vec![uri], String::new()).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        None::<&gio::Cancellable>,
        move |reply| {
            if reply.is_err() {
                fallback();
            }
        },
    );
}

/// What the panel asks the app for: the things that need the config, the
/// focused pane or the status line.
pub enum Ask {
    /// Open in the editor with this id, or the default when `None`.
    Open(PathBuf, Option<String>),
    /// Type the path into the focused pane.
    Insert(PathBuf),
    /// A terminal window in this directory.
    Terminal(PathBuf),
    /// One line for the status bar.
    Notice(String),
}

/// The app's answer to an [`Ask`]. Set once; cloned out before it is
/// called so it can never hold the panel's borrow.
type AskFn = Rc<dyn Fn(Ask)>;

/// What a row can be asked to do, whichever way it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Open,
    OpenSystem,
    Insert,
    Terminal,
    Copy,
    Cut,
    Paste,
    Duplicate,
    Rename,
    NewFile,
    NewFolder,
    CopyPath,
    CopyRelative,
    Reveal,
    ShowInTree,
    Trash,
    Reload,
}

impl Act {
    /// `files.<name>` action names, so the menu model, the key map and the
    /// action group all speak the same word.
    const ALL: [(Act, &'static str); 17] = [
        (Act::Open, "open"),
        (Act::OpenSystem, "open-system"),
        (Act::Insert, "insert"),
        (Act::Terminal, "terminal"),
        (Act::Copy, "copy"),
        (Act::Cut, "cut"),
        (Act::Paste, "paste"),
        (Act::Duplicate, "duplicate"),
        (Act::Rename, "rename"),
        (Act::NewFile, "new-file"),
        (Act::NewFolder, "new-folder"),
        (Act::CopyPath, "copy-path"),
        (Act::CopyRelative, "copy-relative"),
        (Act::Reveal, "reveal"),
        (Act::ShowInTree, "show-in-tree"),
        (Act::Trash, "trash"),
        (Act::Reload, "reload"),
    ];

    fn detailed(self) -> String {
        let name = Act::ALL.iter().find(|(a, _)| *a == self).map(|(_, n)| *n);
        format!("files.{}", name.unwrap_or_default())
    }
}

/// The paths copied or cut, waiting for a paste.
struct Clip {
    paths: Vec<PathBuf>,
    cut: bool,
}

/// An entry sitting in the tree where a name would be.
#[derive(Clone, PartialEq, Eq)]
enum Edit {
    Rename(PathBuf),
    /// A new entry under `dir`.
    Create {
        dir: PathBuf,
        folder: bool,
    },
}

/// One line of the tree, flattened in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Entry {
        entry: Entry,
        depth: i32,
    },
    More {
        dir: PathBuf,
        depth: i32,
        rest: usize,
    },
    /// A search result: no place in the tree, but a reason it is here.
    Hit(Hit),
}

impl Row {
    /// The filesystem entry this row stands for, if it stands for one.
    fn entry(&self) -> Option<&Entry> {
        match self {
            Row::Entry { entry, .. } => Some(entry),
            Row::Hit(hit) => Some(&hit.entry),
            Row::More { .. } => None,
        }
    }
}

/// What a built row looked like; a row is reused while this holds.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RowKey {
    depth: i32,
    dir: bool,
    open: bool,
    mtime: Option<i64>,
    /// The day the "when" column was formatted for: "14:02" must become
    /// "09 Sep" after midnight.
    day: i64,
    /// A result row and a tree row for the same path are different rows.
    hit: bool,
}

struct State {
    root: Option<PathBuf>,
    /// Directories the user has opened, by absolute path.
    expanded: HashSet<PathBuf>,
    /// Extra entries to show past `PAGE`, per directory.
    pages: HashMap<PathBuf, usize>,
    show_hidden: bool,
    /// mtime of every directory currently on screen, so a rebuild happens
    /// when the agent writes a file and not on a timer.
    seen: HashMap<PathBuf, i64>,
    /// The rows on screen, in order: what the keys walk.
    rows: Vec<Row>,
    /// For every child of the list, the index of its row - `None` for a
    /// "more" line or the edit row - so a click maps back to an entry.
    slots: Vec<Option<usize>>,
    /// Row widgets by path, kept across rebuilds while their key holds.
    built: HashMap<PathBuf, (gtk::Widget, RowKey)>,
    selected: Option<PathBuf>,
    clip: Option<Clip>,
    edit: Option<Edit>,
    /// What is in the search entry, trimmed. Empty means the tree is
    /// showing and everything below is idle.
    query: String,
    /// The running walk. Dropping it stops the thread.
    search: Option<search::Search>,
    /// What the walk has found so far, best first.
    hits: Vec<Hit>,
    /// Bumped per search, so the poll of a query the user has already
    /// replaced stops instead of drawing over the new one.
    generation: u64,
    /// Installed editors, for "Open with".
    editors: Vec<Editor>,
    /// The one "Open" uses, named in the menu.
    default_editor: Option<Editor>,
}

impl State {
    fn entry(&self, path: &Path) -> Option<&Entry> {
        self.rows
            .iter()
            .filter_map(Row::entry)
            .find(|e| e.path == path)
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entry(self.selected.as_deref()?)
    }

    /// Where a paste or a new file lands: the selected folder, the selected
    /// file's folder, or the root.
    fn target_dir(&self) -> Option<PathBuf> {
        match self.selected_entry() {
            Some(e) if e.dir => Some(e.path.clone()),
            Some(e) => e.path.parent().map(Path::to_path_buf),
            None => self.root.clone(),
        }
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.entry().is_some_and(|e| e.path == path))
    }
}

/// The panel: a header with the project's name and the tree under it.
#[derive(Clone)]
pub struct Files {
    pub root: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    query: gtk::SearchEntry,
    halt: gtk::Button,
    progress: gtk::Label,
    list: gtk::Box,
    /// Header menu: open the project folder itself - in an editor, or in a
    /// terminal window. Its model is rebuilt when the editor list arrives.
    open_root: gtk::MenuButton,
    popover: gtk::PopoverMenu,
    state: Rc<RefCell<State>>,
    ask: Rc<RefCell<Option<AskFn>>>,
}

impl Files {
    pub fn new() -> Files {
        // `max_width_chars(1)`: an ellipsizing label still *asks* for its
        // whole text, so without a cap the header sets the panel's minimum
        // width and a long path pushes the tree out over the panes.
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(1)
            .css_classes(["files-title"])
            .build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .max_width_chars(1)
            .css_classes(["files-subtitle", "mono"])
            .build();
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
        titles.set_hexpand(true);
        titles.append(&title);
        titles.append(&subtitle);

        let action_button = |icon: &str, tip: &str| {
            gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .css_classes(["flat", "files-action"])
                .valign(gtk::Align::Center)
                .build()
        };
        let new_file = action_button("document-new-symbolic", "New file (Ctrl+N)");
        let new_folder = action_button("folder-new-symbolic", "New folder");
        let hidden = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Show dotfiles")
            .css_classes(["flat", "files-action"])
            .valign(gtk::Align::Center)
            .active(true)
            .build();
        let reload = action_button("view-refresh-symbolic", "Re-read the tree");
        // Opens the project folder itself, which is the one thing the tree
        // could not do: every other action here is about a row. The model
        // is filled by `set_editors`, so the list is what this machine has.
        let open_root = gtk::MenuButton::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Open this folder")
            .css_classes(["flat", "files-action"])
            .valign(gtk::Align::Center)
            .build();

        let head = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        head.add_css_class("files-head");
        head.append(&titles);
        head.append(&new_file);
        head.append(&new_folder);
        head.append(&hidden);
        head.append(&reload);
        head.append(&open_root);

        // The entry waits for a pause in the typing before it emits
        // `search-changed`, which is exactly the debounce a walk of the
        // project wants; the stop button appears only while one is running.
        let query = gtk::SearchEntry::builder()
            .placeholder_text("Search files")
            .tooltip_text(concat!(
                "Names match out of order, contents match literally.\n",
                "Skipped: .git, node_modules, target, dist, build, venvs and caches"
            ))
            .hexpand(true)
            .css_classes(["files-search"])
            .build();
        let halt = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Stop searching, keep what was found")
            .css_classes(["flat", "files-action"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        search_row.add_css_class("files-search-row");
        search_row.append(&query);
        search_row.append(&halt);
        let progress = gtk::Label::builder()
            .xalign(0.0)
            .visible(false)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(1)
            .css_classes(["files-progress", "mono"])
            .build();

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("files-list");
        list.set_vexpand(true);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("files-panel");
        root.append(&head);
        root.append(&search_row);
        root.append(&progress);
        root.append(&scroller);
        // Narrow enough to sit beside three panes, wide enough for a name
        // and a date.
        root.set_size_request(220, -1);

        // One menu for the whole tree, its model swapped per click. A child
        // of the list but not in its layout, so the list's own dispose does
        // not take it down.
        let popover = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        popover.set_parent(&list);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        let orphan = popover.clone();
        list.connect_destroy(move |_| orphan.unparent());

        let files = Files {
            root,
            title,
            subtitle,
            query,
            halt,
            progress,
            list,
            open_root: open_root.clone(),
            popover,
            state: Rc::new(RefCell::new(State {
                root: None,
                expanded: HashSet::new(),
                pages: HashMap::new(),
                show_hidden: true,
                seen: HashMap::new(),
                rows: Vec::new(),
                slots: Vec::new(),
                built: HashMap::new(),
                selected: None,
                clip: None,
                edit: None,
                query: String::new(),
                search: None,
                hits: Vec::new(),
                generation: 0,
                editors: Vec::new(),
                default_editor: None,
            })),
            ask: Rc::new(RefCell::new(None)),
        };

        // Every way of asking - key, menu, header button - lands in `act`.
        let group = gio::SimpleActionGroup::new();
        for (act, name) in Act::ALL {
            let action = gio::SimpleAction::new(name, None);
            let files = files.clone();
            action.connect_activate(move |_, _| files.act(act));
            group.add_action(&action);
        }
        let open_with = gio::SimpleAction::new("open-with", Some(glib::VariantTy::STRING));
        open_with.connect_activate({
            let files = files.clone();
            move |_, id| {
                if let Some(id) = id.and_then(|v| v.str()) {
                    files.open_with(Some(id.to_string()));
                }
            }
        });
        group.add_action(&open_with);
        let open_root_with =
            gio::SimpleAction::new("open-root-with", Some(glib::VariantTy::STRING));
        open_root_with.connect_activate({
            let files = files.clone();
            move |_, id| {
                let root = files.state.borrow().root.clone();
                if let (Some(root), Some(id)) = (root, id.and_then(|v| v.str())) {
                    // An empty id is "a terminal here": one action rather
                    // than two, because the menu is one list of places to
                    // open the folder.
                    files.ask(match id {
                        "" => Ask::Terminal(root),
                        id => Ask::Open(root, Some(id.to_string())),
                    });
                }
            }
        });
        group.add_action(&open_root_with);
        files.root.insert_action_group("files", Some(&group));

        new_file.connect_clicked({
            let files = files.clone();
            move |_| files.act(Act::NewFile)
        });
        new_folder.connect_clicked({
            let files = files.clone();
            move |_| files.act(Act::NewFolder)
        });
        hidden.connect_toggled({
            let files = files.clone();
            move |button| {
                files.state.borrow_mut().show_hidden = button.is_active();
                files.cancel_edit();
                files.rebuild();
            }
        });
        reload.connect_clicked({
            let files = files.clone();
            move |_| files.act(Act::Reload)
        });

        // The panel's own search. `search-changed` arrives after a pause in
        // the typing, so a walk starts per query rather than per keystroke.
        files.query.connect_search_changed({
            let files = files.clone();
            move |entry| files.set_query(&entry.text())
        });
        // Enter with nothing picked takes the top result: type, Enter, open.
        files.query.connect_activate({
            let files = files.clone();
            move |_| {
                if files.state.borrow().selected.is_none() {
                    files.jump(0);
                }
                files.activate();
            }
        });
        // Escape leaves the search and puts the tree back.
        files.query.connect_stop_search(|entry| entry.set_text(""));
        // Down walks out of the entry into the results.
        let into_results = gtk::EventControllerKey::new();
        into_results.connect_key_pressed({
            let files = files.clone();
            move |_, key, _, _| match key {
                gdk::Key::Down | gdk::Key::KP_Down => files.jump(0),
                _ => glib::Propagation::Proceed,
            }
        });
        files.query.add_controller(into_results);
        files.halt.connect_clicked({
            let files = files.clone();
            move |_| files.stop_search()
        });

        // One gesture for the whole tree, hit-tested to a row: a press
        // selects, a second press opens, the right button asks for the menu.
        // The inline entry claims its own presses, so this never fires on it.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.connect_pressed({
            let files = files.clone();
            move |gesture, n_press, x, y| {
                // Hit-test against what is on screen, then drop any
                // half-typed name: clicking away is not a "yes", and the
                // rebuild that follows would move the rows under the
                // pointer.
                let hit = files.row_at(x, y);
                files.cancel_edit();
                let path = hit.as_ref().map(|e| e.path.clone());
                files.select(path);
                match (gesture.current_button(), hit) {
                    (gdk::BUTTON_SECONDARY, hit) => files.menu_at(hit.as_ref(), x, y),
                    (gdk::BUTTON_PRIMARY, Some(entry)) => {
                        if entry.dir {
                            if n_press == 1 {
                                files.toggle(&entry.path);
                            }
                        } else if n_press == 2 {
                            files.open_with(None);
                        }
                    }
                    _ => {}
                }
            }
        });
        files.list.add_controller(click);

        // A row is a file: drop it on a pane and the pane types its path,
        // exactly as a file dragged in from a file manager does.
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::COPY);
        drag.connect_prepare({
            let files = files.clone();
            move |source, x, y| {
                let entry = files.row_at(x, y)?;
                if let Some(row) = files.widget_of(&entry.path) {
                    source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
                }
                let file = gio::File::for_path(&entry.path);
                Some(gdk::ContentProvider::for_value(&file.to_value()))
            }
        });
        files.list.add_controller(drag);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let files = files.clone();
            move |_, key, _, modifiers| files.key(key, modifiers)
        });
        files.list.add_controller(keys);

        files
    }

    /// What the panel asks the app for. Set once, by the app.
    pub fn connect(&self, f: impl Fn(Ask) + 'static) {
        *self.ask.borrow_mut() = Some(Rc::new(f));
    }

    fn ask(&self, ask: Ask) {
        let f = self.ask.borrow().clone();
        if let Some(f) = f {
            f(ask);
        }
    }

    fn notice(&self, text: impl Into<String>) {
        self.ask(Ask::Notice(text.into()));
    }

    /// The editors "Open with" offers, and the one "Open" means. Also fills
    /// the header's "Open this folder" menu, which is the same list applied
    /// to the project root instead of to a row.
    pub fn set_editors(&self, installed: Vec<Editor>, default: Option<Editor>) {
        let menu = gio::Menu::new();
        let places = gio::Menu::new();
        for editor in &installed {
            let item = gio::MenuItem::new(Some(&format!("Open folder in {}", editor.label)), None);
            item.set_action_and_target_value(
                Some("files.open-root-with"),
                Some(&editor.id.to_variant()),
            );
            places.append_item(&item);
        }
        menu.append_section(None, &places);
        let shell = gio::Menu::new();
        let item = gio::MenuItem::new(Some("Open folder in a new terminal"), None);
        item.set_action_and_target_value(Some("files.open-root-with"), Some(&"".to_variant()));
        shell.append_item(&item);
        menu.append_section(None, &shell);
        self.open_root.set_menu_model(Some(&menu));

        let mut state = self.state.borrow_mut();
        state.editors = installed;
        state.default_editor = default;
    }

    /// Point the tree at a project. Re-pointing it at the same project keeps
    /// what the user had opened; a different one starts closed.
    pub fn set_root(&self, name: &str, root: &Path) {
        {
            let mut state = self.state.borrow_mut();
            if state.root.as_deref() == Some(root) {
                return;
            }
            state.root = Some(root.to_path_buf());
            state.expanded.clear();
            state.pages.clear();
            state.selected = None;
            state.edit = None;
        }
        self.title.set_text(name);
        self.subtitle
            .set_text(&taix_core::text::contract_home(root));
        // A query is about the project it was typed in; this is another.
        self.query.set_text("");
        self.rebuild();
    }

    pub fn clear(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.root = None;
            state.expanded.clear();
            state.pages.clear();
            state.seen.clear();
            state.rows.clear();
            state.selected = None;
            state.edit = None;
        }
        self.title.set_text("No project");
        self.subtitle.set_text("");
        self.query.set_text("");
        self.fill();
    }

    /// Re-read every directory on screen if any of them changed.
    ///
    /// A directory's mtime moves when an entry is added, removed or renamed,
    /// which is exactly when this tree is wrong. One `stat` per open
    /// directory on the slow cadence, rather than a watch per directory or a
    /// rebuild per frame. Not while a name is being typed: the rebuild would
    /// take the entry with it.
    pub fn poll(&self) {
        let stale = {
            let state = self.state.borrow();
            state.edit.is_none()
                && state.seen.iter().any(|(dir, seen)| {
                    std::fs::metadata(dir)
                        .ok()
                        .as_ref()
                        .and_then(mtime_secs)
                        .is_none_or(|now| now != *seen)
                })
        };
        if stale {
            self.rebuild();
        }
    }

    /// Re-read every visible directory and rebuild the rows.
    pub fn rebuild(&self) {
        {
            let mut state = self.state.borrow_mut();
            if !state.query.is_empty() {
                // The tree is not what is on screen; the results are.
                drop(state);
                self.show_results();
                return;
            }
            let Some(root) = state.root.clone() else {
                return;
            };
            let mut rows = Vec::new();
            let mut seen = HashMap::new();
            walk(
                &root,
                0,
                state.show_hidden,
                &state.expanded,
                &state.pages,
                &mut rows,
                &mut seen,
            );
            state.seen = seen;
            state.rows = rows;
        }
        self.fill();
    }

    /// Rebuild the widgets from the rows already read, reusing every row
    /// whose key still holds. The widget tree is touched only after the
    /// state borrow is released: removing a focused entry fires focus
    /// signals that come straight back here.
    fn fill(&self) {
        let keep_focus = self.list.focus_child().is_some();
        let now = glib::DateTime::now_local()
            .map(|d| d.to_unix())
            .unwrap_or_default();
        let day = now.div_euclid(86_400);

        let mut focus: Option<gtk::Widget> = None;
        let mut widgets: Vec<gtk::Widget> = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            let State {
                rows,
                slots,
                built,
                expanded,
                selected,
                clip,
                edit,
                root,
                ..
            } = &mut *state;
            slots.clear();
            let mut fresh: HashMap<PathBuf, (gtk::Widget, RowKey)> =
                HashMap::with_capacity(rows.len());
            // A cut row is dimmed until it is pasted; a copied one is not
            // going anywhere, so it looks like itself.
            let is_cut = |path: &Path| {
                clip.as_ref()
                    .is_some_and(|c| c.cut && c.paths.iter().any(|p| p == path))
            };

            // A new name typed at the root sits above everything.
            if let Some(Edit::Create { dir, folder }) = edit
                && root.as_deref() == Some(dir.as_path())
            {
                let (row, text) = self.edit_row(0, "", *folder);
                focus = Some(text.upcast());
                widgets.push(row);
                slots.push(None);
            }
            for (i, row) in rows.iter().enumerate() {
                match row {
                    Row::More { depth, rest, .. } => {
                        let label = gtk::Label::builder()
                            .label(format!("… {rest} more"))
                            .xalign(0.0)
                            .css_classes(["files-more"])
                            .margin_start(depth * INDENT + 20)
                            .build();
                        widgets.push(label.upcast());
                        slots.push(None);
                    }
                    Row::Entry { entry, depth } => {
                        let open = entry.dir && expanded.contains(&entry.path);
                        let key = RowKey {
                            depth: *depth,
                            dir: entry.dir,
                            open,
                            mtime: entry.mtime,
                            day,
                            hit: false,
                        };
                        let renaming = matches!(edit, Some(Edit::Rename(p)) if *p == entry.path);
                        let widget = match built.remove(&entry.path) {
                            Some((widget, was)) if was == key && !renaming => widget,
                            _ if renaming => {
                                let (row, text) = self.edit_row(*depth, &entry.name, entry.dir);
                                focus = Some(text.upcast());
                                row
                            }
                            _ => self.row(entry, *depth, open, now),
                        };
                        let chosen = selected.as_deref() == Some(entry.path.as_path());
                        set_class(&widget, "selected", chosen);
                        set_class(&widget, "cut", is_cut(&entry.path));
                        if chosen && keep_focus && focus.is_none() {
                            focus = Some(widget.clone());
                        }
                        if !renaming {
                            fresh.insert(entry.path.clone(), (widget.clone(), key));
                        }
                        widgets.push(widget);
                        slots.push(Some(i));
                        if let Some(Edit::Create { dir, folder }) = edit
                            && *dir == entry.path
                        {
                            let (row, text) = self.edit_row(depth + 1, "", *folder);
                            focus = Some(text.upcast());
                            widgets.push(row);
                            slots.push(None);
                        }
                    }
                    Row::Hit(hit) => {
                        let entry = &hit.entry;
                        let key = RowKey {
                            depth: 0,
                            dir: entry.dir,
                            open: false,
                            mtime: None,
                            day,
                            hit: true,
                        };
                        // `built` is emptied when the query changes, so a
                        // row is only reused for the query it was made for.
                        let widget = match built.remove(&entry.path) {
                            Some((widget, was)) if was == key => widget,
                            _ => self.hit_row(hit, root.as_deref()),
                        };
                        let chosen = selected.as_deref() == Some(entry.path.as_path());
                        set_class(&widget, "selected", chosen);
                        if chosen && keep_focus && focus.is_none() {
                            focus = Some(widget.clone());
                        }
                        fresh.insert(entry.path.clone(), (widget.clone(), key));
                        widgets.push(widget);
                        slots.push(Some(i));
                    }
                }
            }
            *built = fresh;
        }

        // The popover is a child of the list as well - not in its layout,
        // but in its child list - and removing it would unparent it.
        let rows: Vec<gtk::Widget> =
            std::iter::successors(self.list.first_child(), |c| c.next_sibling())
                .filter(|c| !c.is::<gtk::Popover>())
                .collect();
        for child in &rows {
            self.list.remove(child);
        }
        if widgets.is_empty() {
            let empty = gtk::Label::builder()
                .label(self.empty_line())
                .css_classes(["files-empty"])
                .xalign(0.0)
                .build();
            self.list.append(&empty);
        }
        for widget in &widgets {
            self.list.append(widget);
        }
        if let Some(widget) = focus {
            // After this frame's layout, so a fresh entry exists to focus and
            // a row scrolls into view at its real position.
            glib::idle_add_local_once(move || {
                match widget.downcast_ref::<gtk::Entry>() {
                    Some(entry) => {
                        // `grab_focus` on an entry selects everything; the
                        // rename selects the stem, so the selection is set
                        // after the focus.
                        entry.grab_focus_without_selecting();
                        entry.select_region(0, stem_chars(&entry.text()));
                    }
                    None => {
                        widget.grab_focus();
                    }
                }
            });
        }
    }

    /// One tree row: caret, icon, name, when. No controllers of its own -
    /// the list's gesture finds it by position.
    fn row(&self, entry: &Entry, depth: i32, open: bool, now: i64) -> gtk::Widget {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.add_css_class("file-row");
        if entry.dir {
            line.add_css_class("file-row-dir");
        }
        line.set_margin_start(depth * INDENT);
        line.set_focusable(true);
        line.set_tooltip_text(Some(&entry.path.to_string_lossy()));
        line.append(&caret(entry.dir, open));
        line.append(&icon_for(&entry.name, entry.dir, open));
        let name = gtk::Label::builder()
            .label(&entry.name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(1)
            .css_classes(["files-name"])
            .build();
        line.append(&name);
        let when = gtk::Label::builder()
            .label(entry.mtime.map(|t| human_mtime(t, now)).unwrap_or_default())
            .css_classes(["files-when", "mono"])
            .build();
        line.append(&when);
        line.upcast()
    }

    /// One result row: the name with a badge saying how it was found, and
    /// under it the folder it sits in, or the line that matched.
    fn hit_row(&self, hit: &Hit, root: Option<&Path>) -> gtk::Widget {
        let entry = &hit.entry;
        let line = gtk::Box::new(gtk::Orientation::Vertical, 0);
        line.add_css_class("file-row");
        line.add_css_class("file-hit");
        if entry.dir {
            line.add_css_class("file-row-dir");
        }
        line.set_focusable(true);
        line.set_tooltip_text(Some(&entry.path.to_string_lossy()));

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&icon_for(&entry.name, entry.dir, false));
        let name = gtk::Label::builder()
            .label(&entry.name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(1)
            .css_classes(["files-name"])
            .build();
        top.append(&name);
        // The badge is the answer to "why is this here": the name matched,
        // or the text did and this is where.
        let (badge, class) = match &hit.why {
            Why::Name => ("name".to_string(), "by-name"),
            Why::Content { line, .. } => (format!("L{line}"), "by-text"),
        };
        top.append(
            &gtk::Label::builder()
                .label(badge)
                .css_classes(["files-why", class, "mono"])
                .build(),
        );
        line.append(&top);

        // Where it is, or what it said. A hit at the root has neither, and
        // stays one line tall.
        let (detail, ellipsize) = match &hit.why {
            Why::Name => (
                root.and_then(|r| entry.path.parent()?.strip_prefix(r).ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                gtk::pango::EllipsizeMode::Start,
            ),
            Why::Content { text, .. } => (text.clone(), gtk::pango::EllipsizeMode::End),
        };
        if !detail.is_empty() {
            line.append(
                &gtk::Label::builder()
                    .label(detail)
                    .xalign(0.0)
                    .ellipsize(ellipsize)
                    .max_width_chars(1)
                    .css_classes(["files-hit-detail", "mono"])
                    .build(),
            );
        }
        line.upcast()
    }

    /// A row with an entry where the name would be: a rename in place, or a
    /// new file or folder. Enter commits, Escape or leaving cancels.
    fn edit_row(&self, depth: i32, initial: &str, folder: bool) -> (gtk::Widget, gtk::Entry) {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.add_css_class("file-row");
        line.add_css_class("file-row-edit");
        line.set_margin_start(depth * INDENT);
        line.append(&caret(folder, false));
        line.append(&icon_for(initial, folder, false));
        let entry = gtk::Entry::builder()
            .text(initial)
            .hexpand(true)
            .placeholder_text(if folder { "folder name" } else { "file name" })
            .css_classes(["files-edit"])
            .build();
        entry.connect_activate({
            let files = self.clone();
            move |entry| files.commit_edit(entry.text().trim())
        });
        let escape = gtk::EventControllerKey::new();
        escape.connect_key_pressed({
            let files = self.clone();
            move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    files.cancel_edit();
                    files.focus_selected();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        entry.add_controller(escape);
        let leave = gtk::EventControllerFocus::new();
        leave.connect_leave({
            let files = self.clone();
            // Only when focus went to another widget: the window losing
            // focus - alt-tab, a virtual keyboard reconnecting - fires this
            // too, with the entry still the window's focus widget, and a
            // half-typed name must survive that. `try_borrow`: focus also
            // leaves when a rebuild removes the entry, and that rebuild may
            // be the one this panel is running.
            move |controller| {
                let entry = controller.widget();
                let moved = entry
                    .and_then(|e| Some((e.root()?.focus()?, e)))
                    .is_some_and(|(focus, entry)| !focus.is_ancestor(&entry));
                if moved && files.state.try_borrow().is_ok_and(|s| s.edit.is_some()) {
                    files.cancel_edit();
                }
            }
        });
        entry.add_controller(leave);
        line.append(&entry);
        (line.upcast(), entry)
    }

    /// The entry under the pointer, if the pointer is on a row.
    fn row_at(&self, x: f64, y: f64) -> Option<Entry> {
        let hit = self.list.pick(x, y, gtk::PickFlags::DEFAULT)?;
        let row = std::iter::successors(Some(hit), |w| w.parent())
            .take_while(|w| *w != self.list)
            .find(|w| w.has_css_class("file-row"))?;
        let position = std::iter::successors(row.prev_sibling(), |w| w.prev_sibling())
            .filter(|w| !w.is::<gtk::Popover>())
            .count();
        let state = self.state.borrow();
        let index = (*state.slots.get(position)?)?;
        state.rows.get(index)?.entry().cloned()
    }

    fn widget_of(&self, path: &Path) -> Option<gtk::Widget> {
        self.state.borrow().built.get(path).map(|(w, _)| w.clone())
    }

    /// Make this the selected row: the one the keys and the menu act on.
    fn select(&self, path: Option<PathBuf>) {
        let previous = {
            let mut state = self.state.borrow_mut();
            if state.selected == path {
                None
            } else {
                std::mem::replace(&mut state.selected, path.clone())
            }
        };
        if let Some(old) = previous.and_then(|p| self.widget_of(&p)) {
            old.remove_css_class("selected");
        }
        if let Some(row) = path.and_then(|p| self.widget_of(&p)) {
            row.add_css_class("selected");
            row.grab_focus();
        }
    }

    fn focus_selected(&self) {
        let selected = self.state.borrow().selected.clone();
        if let Some(row) = selected.and_then(|p| self.widget_of(&p)) {
            row.grab_focus();
        }
    }

    /// Open or close a directory - or, in the results, go to where it
    /// lives: there is nothing to unfold in a flat list of hits.
    fn toggle(&self, dir: &Path) {
        if !self.state.borrow().query.is_empty() {
            self.reveal_in_tree(dir);
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            if !state.expanded.remove(dir) {
                state.expanded.insert(dir.to_path_buf());
            }
        }
        self.rebuild();
    }

    fn set_expanded(&self, dir: &Path, open: bool) -> bool {
        let mut state = self.state.borrow_mut();
        if open {
            state.expanded.insert(dir.to_path_buf())
        } else {
            state.expanded.remove(dir)
        }
    }

    // ---------- search ----------

    /// Put the cursor in the search entry.
    pub fn focus_search(&self) {
        self.query.grab_focus();
    }

    /// Leave the results and show `path` where it lives, with every folder
    /// down to it open.
    fn reveal_in_tree(&self, path: &Path) {
        {
            let mut state = self.state.borrow_mut();
            let root = state.root.clone();
            for dir in path.ancestors().skip(1) {
                if !root.as_deref().is_some_and(|r| dir.starts_with(r)) {
                    break;
                }
                state.expanded.insert(dir.to_path_buf());
            }
        }
        // Clearing the entry ends the search and rebuilds the tree.
        self.query.set_text("");
        self.select(Some(path.to_path_buf()));
    }

    /// A new query: stop whatever was running and start walking, or go back
    /// to the tree when the entry is empty.
    fn set_query(&self, text: &str) {
        let text = text.trim().to_string();
        let (root, hidden, generation) = {
            let mut state = self.state.borrow_mut();
            if state.query == text {
                return;
            }
            text.clone_into(&mut state.query);
            // Dropping the old search stops its thread; the rows it fed are
            // not this query's answer either.
            state.search = None;
            state.hits.clear();
            state.built.clear();
            state.selected = None;
            state.generation += 1;
            (state.root.clone(), state.show_hidden, state.generation)
        };
        self.cancel_edit();
        self.halt.set_visible(false);
        self.progress.set_visible(!text.is_empty());
        self.progress.set_text("searching…");
        let Some(root) = root.filter(|_| !text.is_empty()) else {
            self.rebuild();
            return;
        };
        self.state.borrow_mut().search = Some(search::Search::start(root, &text, hidden));
        self.halt.set_visible(true);
        self.show_results();
        // Fast enough that hits appear as they are found, slow enough that
        // a walk of a large tree does not spend its time redrawing.
        glib::timeout_add_local(std::time::Duration::from_millis(80), {
            let files = self.clone();
            move || files.pump(generation)
        });
    }

    /// Give up on the rest of the walk, keeping what it found.
    pub fn stop_search(&self) {
        if let Some(search) = &self.state.borrow().search {
            search.stop();
        }
    }

    /// Drain the walk into the list. Runs until the walk is done or the
    /// query it belongs to has been replaced.
    fn pump(&self, generation: u64) -> glib::ControlFlow {
        let (changed, done) = {
            let mut state = self.state.borrow_mut();
            if state.generation != generation {
                return glib::ControlFlow::Break;
            }
            let Some(search) = &state.search else {
                return glib::ControlFlow::Break;
            };
            let found = search.take();
            let (scanned, done) = search.progress();
            let changed = !found.is_empty();
            state.hits.extend(found);
            search::order(&mut state.hits);
            let line = format!(
                "{} hit{} · {scanned} scanned{}",
                state.hits.len(),
                if state.hits.len() == 1 { "" } else { "s" },
                if done { "" } else { " …" }
            );
            self.progress.set_text(&line);
            if done {
                state.search = None;
            }
            (changed, done)
        };
        if done {
            self.halt.set_visible(false);
        }
        // The last pass redraws even with nothing new: "searching…" has to
        // become "no matches" when the walk comes back empty.
        if changed || done {
            self.show_results();
        }
        if done {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    }

    /// Draw the hits found so far.
    fn show_results(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.rows = state.hits.iter().cloned().map(Row::Hit).collect();
            // Nothing on screen is a directory listing, so nothing to poll.
            state.seen.clear();
        }
        self.fill();
    }

    /// What an empty list says, which is never the same thing twice.
    fn empty_line(&self) -> &'static str {
        let state = self.state.borrow();
        match (state.query.is_empty(), state.search.is_some()) {
            (true, _) => "nothing here",
            (false, true) => "searching…",
            (false, false) => "no matches",
        }
    }

    // ---------- keys ----------

    fn key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> glib::Propagation {
        use gdk::Key;
        // A key the inline entry did not take - F2, an arrow at the end of
        // the text - bubbles up here; while a name is being typed, none of
        // these are for the tree.
        if self.state.borrow().edit.is_some() {
            return glib::Propagation::Proceed;
        }
        let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        let act = match key {
            Key::Return | Key::KP_Enter => {
                self.activate();
                return glib::Propagation::Stop;
            }
            Key::Up | Key::KP_Up => return self.step(-1),
            Key::Down | Key::KP_Down => return self.step(1),
            Key::Home | Key::KP_Home => return self.jump(0),
            Key::End | Key::KP_End => return self.jump(usize::MAX),
            Key::Left | Key::KP_Left => return self.left(),
            Key::Right | Key::KP_Right => return self.right(),
            Key::BackSpace => return self.parent(),
            Key::f | Key::F if ctrl => {
                self.focus_search();
                return glib::Propagation::Stop;
            }
            // Escape leaves the results; in the tree it is not ours.
            Key::Escape if !self.state.borrow().query.is_empty() => {
                self.query.set_text("");
                return glib::Propagation::Stop;
            }
            Key::space => Act::Insert,
            Key::F2 => Act::Rename,
            Key::Delete | Key::KP_Delete => Act::Trash,
            Key::c | Key::C if ctrl => Act::Copy,
            Key::x | Key::X if ctrl => Act::Cut,
            Key::v | Key::V if ctrl => Act::Paste,
            Key::d | Key::D if ctrl => Act::Duplicate,
            Key::n | Key::N if ctrl => Act::NewFile,
            _ if !ctrl => {
                // Type-ahead: the next name starting with this letter.
                return match key.to_unicode().filter(|c| c.is_alphanumeric()) {
                    Some(c) => self.jump_to_letter(c),
                    None => glib::Propagation::Proceed,
                };
            }
            _ => return glib::Propagation::Proceed,
        };
        self.act(act);
        glib::Propagation::Stop
    }

    /// Enter: a folder opens or closes, a file opens in the editor.
    fn activate(&self) {
        let selected = self.state.borrow().selected_entry().cloned();
        match selected {
            Some(e) if e.dir => self.toggle(&e.path),
            Some(_) => self.open_with(None),
            None => {}
        }
    }

    /// Select the entry `delta` rows away, skipping "more" lines.
    fn step(&self, delta: isize) -> glib::Propagation {
        let next = {
            let state = self.state.borrow();
            let entries: Vec<usize> = (0..state.rows.len())
                .filter(|i| state.rows[*i].entry().is_some())
                .collect();
            let at = state
                .selected
                .as_deref()
                .and_then(|p| state.index_of(p))
                .and_then(|i| entries.iter().position(|e| *e == i));
            let pos = match at {
                Some(pos) => (pos as isize + delta).clamp(0, entries.len() as isize - 1) as usize,
                None if delta < 0 => entries.len().saturating_sub(1),
                None => 0,
            };
            entries
                .get(pos)
                .and_then(|i| state.rows.get(*i))
                .and_then(|r| Some(r.entry()?.path.clone()))
        };
        if next.is_some() {
            self.select(next);
        }
        glib::Propagation::Stop
    }

    fn jump(&self, index: usize) -> glib::Propagation {
        let target = {
            let state = self.state.borrow();
            let mut entries = state
                .rows
                .iter()
                .filter_map(|r| Some(r.entry()?.path.clone()));
            if index == 0 {
                entries.next()
            } else {
                entries.next_back()
            }
        };
        if target.is_some() {
            self.select(target);
        }
        glib::Propagation::Stop
    }

    fn jump_to_letter(&self, c: char) -> glib::Propagation {
        let target = {
            let state = self.state.borrow();
            let start = state
                .selected
                .as_deref()
                .and_then(|p| state.index_of(p))
                .map_or(0, |i| i + 1);
            let n = state.rows.len();
            (0..n).map(|k| (start + k) % n).find_map(|i| {
                let entry = state.rows[i].entry()?;
                entry
                    .name
                    .chars()
                    .next()
                    .filter(|first| first.eq_ignore_ascii_case(&c))
                    .map(|_| entry.path.clone())
            })
        };
        match target {
            Some(_) => {
                self.select(target);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    /// Left closes an open folder, otherwise goes to the parent.
    fn left(&self) -> glib::Propagation {
        let selected = self.state.borrow().selected_entry().cloned();
        let Some(entry) = selected else {
            return glib::Propagation::Stop;
        };
        if entry.dir && self.set_expanded(&entry.path, false) {
            self.rebuild();
            return glib::Propagation::Stop;
        }
        self.parent()
    }

    /// Right opens a closed folder, otherwise steps into it.
    fn right(&self) -> glib::Propagation {
        let selected = self.state.borrow().selected_entry().cloned();
        let Some(entry) = selected else {
            return glib::Propagation::Stop;
        };
        if !entry.dir {
            return glib::Propagation::Stop;
        }
        if self.set_expanded(&entry.path, true) {
            self.rebuild();
            return glib::Propagation::Stop;
        }
        self.step(1)
    }

    fn parent(&self) -> glib::Propagation {
        let parent = {
            let state = self.state.borrow();
            state
                .selected
                .as_deref()
                .and_then(Path::parent)
                .filter(|p| state.root.as_deref() != Some(*p))
                .map(Path::to_path_buf)
        };
        if parent.is_some() {
            self.select(parent);
        }
        glib::Propagation::Stop
    }

    // ---------- menu ----------

    /// The context menu for a row, or for the empty space under the rows.
    fn menu_at(&self, target: Option<&Entry>, x: f64, y: f64) {
        let menu = gio::Menu::new();
        let (editors, default, has_clip, searching) = {
            let state = self.state.borrow();
            (
                state.editors.clone(),
                state.default_editor.clone(),
                state.clip.is_some(),
                !state.query.is_empty(),
            )
        };
        let item = |label: &str, act: Act| gio::MenuItem::new(Some(label), Some(&act.detailed()));

        if let Some(entry) = target {
            let open = gio::Menu::new();
            let verb = match &default {
                Some(e) if e.terminal => format!("Open in {} window", e.label),
                Some(e) => format!("Open in {}", e.label),
                None => "Open".to_string(),
            };
            open.append_item(&item(&verb, Act::Open));
            if editors.len() > 1 {
                let with = gio::Menu::new();
                for editor in &editors {
                    let label = if editor.terminal {
                        format!("{} · terminal window", editor.label)
                    } else {
                        editor.label.clone()
                    };
                    let item = gio::MenuItem::new(Some(&label), None);
                    item.set_action_and_target_value(
                        Some("files.open-with"),
                        Some(&editor.id.to_variant()),
                    );
                    with.append_item(&item);
                }
                open.append_submenu(Some("Open with"), &with);
            }
            open.append_item(&item("Open with system default", Act::OpenSystem));
            open.append_item(&item("Insert path into pane", Act::Insert));
            if entry.dir {
                open.append_item(&item("Open terminal here", Act::Terminal));
            }
            menu.append_section(None, &open);

            let clip = gio::Menu::new();
            clip.append_item(&item("Cut", Act::Cut));
            clip.append_item(&item("Copy", Act::Copy));
            if entry.dir || has_clip {
                clip.append_item(&item(
                    if entry.dir {
                        "Paste into"
                    } else {
                        "Paste here"
                    },
                    Act::Paste,
                ));
            }
            clip.append_item(&item("Duplicate", Act::Duplicate));
            menu.append_section(None, &clip);

            let edit = gio::Menu::new();
            edit.append_item(&item("Rename…", Act::Rename));
            edit.append_item(&item("New file", Act::NewFile));
            edit.append_item(&item("New folder", Act::NewFolder));
            menu.append_section(None, &edit);

            let paths = gio::Menu::new();
            paths.append_item(&item("Copy path", Act::CopyPath));
            paths.append_item(&item("Copy relative path", Act::CopyRelative));
            paths.append_item(&item("Reveal in file manager", Act::Reveal));
            if searching {
                paths.append_item(&item("Show in tree", Act::ShowInTree));
            }
            menu.append_section(None, &paths);

            let danger = gio::Menu::new();
            danger.append_item(&item("Move to trash", Act::Trash));
            menu.append_section(None, &danger);
        } else {
            let make = gio::Menu::new();
            make.append_item(&item("New file", Act::NewFile));
            make.append_item(&item("New folder", Act::NewFolder));
            make.append_item(&item("Paste", Act::Paste));
            menu.append_section(None, &make);
            let here = gio::Menu::new();
            here.append_item(&item("Open terminal here", Act::Terminal));
            here.append_item(&item("Reveal in file manager", Act::Reveal));
            here.append_item(&item("Re-read the tree", Act::Reload));
            menu.append_section(None, &here);
        }

        self.popover.set_menu_model(Some(&menu));
        self.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        self.popover.popup();
    }

    // ---------- actions ----------

    fn act(&self, act: Act) {
        let selected = self.state.borrow().selected_entry().cloned();
        match act {
            Act::Open => self.open_with(None),
            Act::OpenSystem => {
                if let Some(e) = selected {
                    let uri = gio::File::for_path(&e.path).uri();
                    if let Err(err) =
                        gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
                    {
                        self.notice(format!("cannot open {}: {err}", e.name));
                    }
                }
            }
            Act::Insert => {
                if let Some(e) = selected {
                    self.ask(Ask::Insert(e.path));
                }
            }
            Act::Terminal => {
                if let Some(dir) = self.state.borrow().target_dir() {
                    self.ask(Ask::Terminal(dir));
                }
            }
            Act::Copy => self.clip(false),
            Act::Cut => self.clip(true),
            Act::Paste => self.paste(),
            Act::Duplicate => {
                let Some(e) = selected else { return };
                let Some(parent) = e.path.parent() else {
                    return;
                };
                let dest = unique(parent, &e.name);
                match copy_path(&e.path, &dest) {
                    Ok(()) => self.after_change(Some(dest)),
                    Err(err) => self.notice(format!("cannot duplicate {}: {err}", e.name)),
                }
            }
            Act::Rename => {
                if let Some(e) = selected {
                    self.begin_edit(Edit::Rename(e.path));
                }
            }
            Act::NewFile | Act::NewFolder => {
                let Some(dir) = self.state.borrow().target_dir() else {
                    return;
                };
                self.begin_edit(Edit::Create {
                    dir,
                    folder: act == Act::NewFolder,
                });
            }
            Act::CopyPath | Act::CopyRelative => {
                let Some(e) = selected else { return };
                let text = if act == Act::CopyPath {
                    e.path.to_string_lossy().into_owned()
                } else {
                    let root = self.state.borrow().root.clone();
                    root.as_deref()
                        .and_then(|r| e.path.strip_prefix(r).ok())
                        .unwrap_or(&e.path)
                        .to_string_lossy()
                        .into_owned()
                };
                self.root.clipboard().set_text(&text);
                self.notice(format!("copied {text}"));
            }
            Act::Reveal => {
                let target = selected
                    .map(|e| e.path)
                    .or_else(|| self.state.borrow().root.clone());
                if let Some(path) = target {
                    reveal(&path);
                }
            }
            Act::ShowInTree => {
                if let Some(e) = selected {
                    self.reveal_in_tree(&e.path);
                }
            }
            Act::Trash => {
                let Some(e) = selected else { return };
                let file = gio::File::for_path(&e.path);
                match file.trash(None::<&gio::Cancellable>) {
                    Ok(()) => {
                        self.notice(format!("{} moved to trash", e.name));
                        self.after_change(None);
                    }
                    Err(err) => self.notice(format!("cannot trash {}: {err}", e.name)),
                }
            }
            Act::Reload => {
                self.cancel_edit();
                // In the results, "re-read" means walk again, not redraw
                // what the last walk found.
                let query = std::mem::take(&mut self.state.borrow_mut().query);
                if query.is_empty() {
                    self.rebuild();
                } else {
                    self.set_query(&query);
                }
            }
        }
    }

    /// Open the selection in an editor: the named one, or the default.
    fn open_with(&self, editor: Option<String>) {
        let selected = self.state.borrow().selected.clone();
        if let Some(path) = selected {
            self.ask(Ask::Open(path, editor));
        }
    }

    /// Copy or cut the selection: remembered here for the paste, and put on
    /// the clipboard as a file and as text - so `Ctrl+V` in a pane types the
    /// path, in an editor pastes it, in a file manager pastes the file.
    fn clip(&self, cut: bool) {
        let Some(entry) = self.state.borrow().selected_entry().cloned() else {
            return;
        };
        let text = entry.path.to_string_lossy().into_owned();
        let files = gdk::FileList::from_array(&[gio::File::for_path(&entry.path)]);
        let provider = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_value(&files.to_value()),
            gdk::ContentProvider::for_value(&text.to_value()),
        ]);
        let _ = self.root.clipboard().set_content(Some(&provider));
        self.state.borrow_mut().clip = Some(Clip {
            paths: vec![entry.path],
            cut,
        });
        self.notice(format!(
            "{} {}",
            if cut { "cut" } else { "copied" },
            entry.name
        ));
        self.fill();
    }

    /// Paste what was copied or cut here, else whatever files another
    /// application put on the clipboard.
    fn paste(&self) {
        let (target, local) = {
            let state = self.state.borrow();
            let local = state.clip.as_ref().map(|c| (c.paths.clone(), c.cut));
            (state.target_dir(), local)
        };
        let Some(target) = target else { return };
        match local {
            Some((paths, cut)) => self.paste_into(&target, &paths, cut),
            None => {
                let files = self.clone();
                glib::spawn_future_local(async move {
                    let value = files
                        .root
                        .clipboard()
                        .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
                        .await;
                    match value.ok().and_then(|v| v.get::<gdk::FileList>().ok()) {
                        Some(list) => {
                            let paths: Vec<PathBuf> =
                                list.files().iter().filter_map(|f| f.path()).collect();
                            files.paste_into(&target, &paths, false);
                        }
                        None => files.notice("nothing to paste"),
                    }
                });
            }
        }
    }

    fn paste_into(&self, target: &Path, sources: &[PathBuf], cut: bool) {
        let mut last = None;
        let mut done = 0;
        for src in sources {
            let Some(name) = src.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if target.starts_with(src) && src.is_dir() {
                self.notice(format!("cannot paste {name} into itself"));
                continue;
            }
            if cut && src.parent() == Some(target) {
                continue;
            }
            let dest = unique(target, name);
            let result = if cut {
                move_path(src, &dest)
            } else {
                copy_path(src, &dest)
            };
            match result {
                Ok(()) => {
                    done += 1;
                    last = Some(dest);
                }
                Err(err) => self.notice(format!("cannot paste {name}: {err}")),
            }
        }
        if cut {
            self.state.borrow_mut().clip = None;
        }
        if done > 0 {
            self.notice(format!(
                "{} {done} item{}",
                if cut { "moved" } else { "pasted" },
                if done == 1 { "" } else { "s" }
            ));
        }
        self.set_expanded(target, true);
        self.after_change(last);
    }

    /// Read the tree back after a change, and land on `select` if given.
    fn after_change(&self, select: Option<PathBuf>) {
        {
            // A result the change renamed or threw away is not a result any
            // more, and the walk that found it is long finished.
            let mut state = self.state.borrow_mut();
            if !state.query.is_empty() {
                state
                    .hits
                    .retain(|h| std::fs::symlink_metadata(&h.entry.path).is_ok());
            }
        }
        self.rebuild();
        if select.is_some() {
            self.select(select);
        }
    }

    // ---------- inline edits ----------

    fn begin_edit(&self, edit: Edit) {
        if let Edit::Create { dir, .. } = &edit {
            self.set_expanded(dir, true);
        }
        self.state.borrow_mut().edit = Some(edit);
        self.rebuild();
    }

    fn cancel_edit(&self) {
        if self.state.borrow_mut().edit.take().is_some() {
            self.fill();
        }
    }

    fn commit_edit(&self, text: &str) {
        let Some(edit) = self.state.borrow_mut().edit.take() else {
            return;
        };
        if text.is_empty() {
            self.fill();
            return;
        }
        let result = match &edit {
            Edit::Rename(path) => {
                let Some(parent) = path.parent() else { return };
                if text.contains('/') {
                    Err(std::io::Error::other("a name cannot contain /"))
                } else {
                    let dest = parent.join(text);
                    if dest == *path {
                        Ok(dest)
                    } else if std::fs::symlink_metadata(&dest).is_ok() {
                        Err(std::io::Error::other("already exists"))
                    } else {
                        std::fs::rename(path, &dest).map(|_| dest)
                    }
                }
            }
            Edit::Create { dir, folder } => {
                let dest = dir.join(text.trim_matches('/'));
                create(&dest, *folder).map(|_| dest)
            }
        };
        match result {
            Ok(dest) => {
                // Every folder typed on the way is opened, so the new name
                // is on screen.
                if let Edit::Create { dir, .. } = &edit {
                    let mut state = self.state.borrow_mut();
                    for ancestor in dest.ancestors().skip(1).take_while(|a| a.starts_with(dir)) {
                        state.expanded.insert(ancestor.to_path_buf());
                    }
                }
                self.after_change(Some(dest));
                self.focus_selected();
            }
            Err(err) => {
                self.notice(format!(
                    "cannot {}: {err}",
                    if matches!(edit, Edit::Rename(_)) {
                        "rename"
                    } else {
                        "create"
                    }
                ));
                self.fill();
            }
        }
    }
}

impl Default for Files {
    fn default() -> Self {
        Files::new()
    }
}

fn caret(dir: bool, open: bool) -> gtk::Widget {
    if dir {
        let caret = gtk::Image::from_icon_name(if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        });
        caret.set_pixel_size(11);
        caret.add_css_class("files-caret");
        caret.upcast()
    } else {
        // The caret's width, so names line up under their folder.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_size_request(11, -1);
        spacer.upcast()
    }
}

fn set_class(widget: &gtk::Widget, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// Flatten the open part of the tree, depth-first.
///
/// Recursion is bounded by what the user has opened, not by the depth of the
/// filesystem: a directory nobody expanded is never read.
pub fn walk(
    dir: &Path,
    depth: i32,
    show_hidden: bool,
    expanded: &HashSet<PathBuf>,
    pages: &HashMap<PathBuf, usize>,
    out: &mut Vec<Row>,
    seen: &mut HashMap<PathBuf, i64>,
) {
    if let Some(t) = std::fs::metadata(dir).ok().as_ref().and_then(mtime_secs) {
        seen.insert(dir.to_path_buf(), t);
    }
    let entries = read_dir(dir, show_hidden);
    let limit = PAGE + pages.get(dir).copied().unwrap_or(0);
    let rest = entries.len().saturating_sub(limit);
    for entry in entries.into_iter().take(limit) {
        let open = entry.dir && expanded.contains(&entry.path);
        let path = entry.path.clone();
        out.push(Row::Entry { entry, depth });
        if open {
            walk(&path, depth + 1, show_hidden, expanded, pages, out, seen);
        }
    }
    if rest > 0 {
        out.push(Row::More {
            dir: dir.to_path_buf(),
            depth,
            rest,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("taix-files-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rows(dir: &Path, expanded: &HashSet<PathBuf>) -> (Vec<String>, usize) {
        let mut out = Vec::new();
        let mut seen = HashMap::new();
        walk(
            dir,
            0,
            false,
            expanded,
            &HashMap::new(),
            &mut out,
            &mut seen,
        );
        (
            out.iter()
                .map(|r| match r {
                    Row::Entry { entry, depth } => format!("{depth}:{}", entry.name),
                    Row::More { rest, .. } => format!("more:{rest}"),
                    Row::Hit(hit) => format!("hit:{}", hit.entry.name),
                })
                .collect(),
            seen.len(),
        )
    }

    #[test]
    fn directories_come_first_then_names_ignoring_case() {
        let dir = tmp("sort");
        for name in ["zeta.rs", "Alpha.rs", "beta"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        for name in ["src", "Docs"] {
            std::fs::create_dir(dir.join(name)).unwrap();
        }
        let names: Vec<String> = read_dir(&dir, false).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Docs", "src", "Alpha.rs", "beta", "zeta.rs"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dotfiles_can_be_hidden() {
        let dir = tmp("hidden");
        std::fs::write(dir.join("visible"), b"x").unwrap();
        std::fs::write(dir.join(".env"), b"x").unwrap();
        std::fs::create_dir(dir.join(".git")).unwrap();
        assert_eq!(read_dir(&dir, false).len(), 1);
        assert_eq!(read_dir(&dir, true).len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_expanded_directories_are_read() {
        // The whole point of the design: a huge subtree costs nothing while
        // it is closed, and exactly one level when it is opened.
        let dir = tmp("walk");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/inner.txt"), b"x").unwrap();
        std::fs::write(dir.join("a/b/deep.txt"), b"x").unwrap();
        std::fs::write(dir.join("top.txt"), b"x").unwrap();

        let (closed, watched) = rows(&dir, &HashSet::new());
        assert_eq!(closed, ["0:a", "0:top.txt"]);
        assert_eq!(watched, 1, "only the root is watched while nothing is open");

        let one: HashSet<PathBuf> = [dir.join("a")].into_iter().collect();
        let (opened, watched) = rows(&dir, &one);
        assert_eq!(opened, ["0:a", "1:b", "1:inner.txt", "0:top.txt"]);
        assert_eq!(watched, 2, "the opened directory joins the watch set");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_huge_directory_is_summarised_and_pages_on_request() {
        let dir = tmp("many");
        for i in 0..PAGE + 7 {
            std::fs::write(dir.join(format!("f{i:05}")), b"x").unwrap();
        }
        let (built, _) = rows(&dir, &HashSet::new());
        assert_eq!(built.len(), PAGE + 1);
        assert_eq!(built.last().map(String::as_str), Some("more:7"));

        let pages: HashMap<PathBuf, usize> = [(dir.clone(), PAGE)].into_iter().collect();
        let mut out = Vec::new();
        walk(
            &dir,
            0,
            false,
            &HashSet::new(),
            &pages,
            &mut out,
            &mut HashMap::new(),
        );
        assert_eq!(
            out.len(),
            PAGE + 7,
            "the next page shows the rest, and no more line"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mtime_reads_as_a_clock_a_date_or_a_year() {
        // Anchored at local noon, not at "now": a minute before midnight is
        // yesterday, and the column would read as a date through no fault
        // of the code. The zone is still the one the test runs in.
        let today = glib::DateTime::now_local().unwrap();
        let noon = glib::DateTime::from_local(
            today.year(),
            today.month(),
            today.day_of_month(),
            12,
            0,
            0.0,
        )
        .unwrap()
        .to_unix();
        let clock = human_mtime(noon - 60, noon);
        assert!(clock.contains(':'), "{clock}");
        let last_week = human_mtime(noon - 8 * 86_400, noon);
        assert!(
            !last_week.contains(':') && last_week.len() >= 5,
            "{last_week}"
        );
        let old = human_mtime(noon - 800 * 86_400, noon);
        assert!(old.contains('-'), "{old}");
    }

    #[test]
    fn an_unreadable_directory_shows_nothing_rather_than_failing() {
        assert!(read_dir(Path::new("/definitely/not/here"), false).is_empty());
    }

    #[test]
    fn a_copy_lands_beside_the_original_with_a_free_name() {
        let dir = tmp("unique");
        std::fs::write(dir.join("a.tar.gz"), b"x").unwrap();
        assert_eq!(unique(&dir, "b.rs"), dir.join("b.rs"));
        assert_eq!(unique(&dir, "a.tar.gz"), dir.join("a.tar (copy).gz"));
        std::fs::write(dir.join("a.tar (copy).gz"), b"x").unwrap();
        assert_eq!(unique(&dir, "a.tar.gz"), dir.join("a.tar (copy 2).gz"));
        // A dotfile has no extension to keep.
        std::fs::write(dir.join(".env"), b"x").unwrap();
        assert_eq!(unique(&dir, ".env"), dir.join(".env (copy)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tree_copies_whole_and_a_move_leaves_nothing_behind() {
        let dir = tmp("copy");
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join("src/deep/x.rs"), b"fn").unwrap();
        std::os::unix::fs::symlink("deep/x.rs", dir.join("src/link")).unwrap();
        copy_path(&dir.join("src"), &dir.join("dup")).unwrap();
        assert_eq!(std::fs::read(dir.join("dup/deep/x.rs")).unwrap(), b"fn");
        assert!(
            std::fs::symlink_metadata(dir.join("dup/link"))
                .unwrap()
                .is_symlink()
        );
        move_path(&dir.join("dup"), &dir.join("moved")).unwrap();
        assert!(!dir.join("dup").exists());
        assert_eq!(std::fs::read(dir.join("moved/deep/x.rs")).unwrap(), b"fn");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_file_makes_its_folders_and_never_clobbers() {
        let dir = tmp("create");
        create(&dir.join("a/b/c.rs"), false).unwrap();
        assert!(dir.join("a/b/c.rs").is_file());
        assert!(
            create(&dir.join("a/b/c.rs"), false).is_err(),
            "an existing file is not truncated"
        );
        create(&dir.join("d/e"), true).unwrap();
        assert!(dir.join("d/e").is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rename_selects_the_stem() {
        assert_eq!(stem_chars("main.rs"), 4);
        assert_eq!(stem_chars(".env"), 4);
        assert_eq!(stem_chars("Makefile"), 8);
        assert_eq!(stem_chars("été.txt"), 3);
    }
}
