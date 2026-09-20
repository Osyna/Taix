//! TaiX — a native GTK4 orchestrator for AI coding agents on top of tmux.
//!
//! tmux is the source of truth: it owns the processes, the layout and all
//! scrollback. TaiX renders it. One control-mode client demultiplexes every
//! pane, exactly one focused pane keeps a live emulator, and the rest are
//! redrawn from `capture-pane` snapshots.

mod app;
#[cfg(feature = "browser")]
pub mod browser;
mod compare;
mod files;
mod find;
mod git;
mod icons;
mod jobs;
mod keys;
mod kit;
mod layout;
mod palette;
mod pango;
mod perf;
mod presets;
mod search;
mod settings;
mod theme;
mod tree;
mod ui;
mod web;

use adw::glib;
use gtk::prelude::*;

use std::cell::RefCell;
use std::rc::Rc;

type Shared = Rc<RefCell<App>>;

use app::App;
use taix_core::{Config, Store};

const APP_ID: &str = "dev.taix.TaiX";

/// Lines a page key moves the scrollback view. Not the pane's real height:
/// a fixed step is predictable, and every pane is a different size.
const PAGE: i32 = 20;

/// Where tmux keeps this session's socket, for an error a user can act on.
fn socket_hint(socket: &str) -> String {
    let dir = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{dir}/tmux-{}/{socket}", libc_getuid())
}

/// The real uid, without pulling in a dependency for one number.
fn libc_getuid() -> u32 {
    // SAFETY: `getuid` takes no arguments, cannot fail, and touches no memory.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// GTK settings that only cost memory here. Measured: `cairo` instead of the
/// default `ngl` renderer is worth 110 MB PSS, and the a11y bridge 1.7 MB.
/// `GDK_DISABLE=gl` was tried and dropped: it changed nothing measurable,
/// because the cairo renderer never creates a GL context in the first place.
fn env_floor() -> &'static [(&'static str, &'static str)] {
    #[cfg(not(feature = "browser"))]
    const FLOOR: &[(&str, &str)] = &[("GSK_RENDERER", "cairo"), ("GTK_A11Y", "none")];
    #[cfg(feature = "browser")]
    const FLOOR: &[(&str, &str)] = &[("GSK_RENDERER", "cairo"), ("GTK_A11Y", "none")];
    FLOOR
}

pub fn run() -> i32 {
    // Measured on this machine, 12 windows, PSS / private-anon:
    //   default `ngl` renderer  178 / 99.9 MB
    //   `cairo`                  68 / 35.4 MB
    //   + no GL, no a11y bridge  61 / 28.1 MB
    // This UI is labels and boxes; there is nothing for a GPU to do, and a
    // dashboard has no assistive-technology surface worth 1.7 MB. An explicit
    // value in the environment always wins - these are defaults, not policy.
    for (key, value) in env_floor() {
        if std::env::var_os(key).is_none() {
            // SAFETY: This runs before any thread is spawned, which is exactly
            // the safety condition for `set_var` in edition 2024.
            unsafe { std::env::set_var(key, value) };
        }
    }

    // The only flag the window takes. `taix restore <name>` execs into here
    // rather than reimplementing spawn in the CLI.
    let restore = {
        let mut args = std::env::args().skip(1);
        let mut found = None;
        while let Some(arg) = args.next() {
            if arg == "--restore" {
                found = args.next();
                break;
            }
        }
        found
    };

    let cfg = Config::load();
    let store = match Store::open(&Config::store_path()) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("cannot open {}: {e}", Config::store_path().display());
            return 1;
        }
    };

    let app = adw::Application::builder().application_id(APP_ID).build();

    // Registering first tells us whether a window is already open. If one is,
    // `taix restore` hands the name to it and exits instead of starting a
    // second process that GTK would only turn into a remote activation.
    if let Some(name) = &restore
        && app.register(None::<&gtk::gio::Cancellable>).is_ok()
        && app.is_remote()
    {
        app.activate_action("restore-session", Some(&name.to_variant()));
        return 0;
    }
    // Built once on activate; a second activation just presents the window.
    let state: Rc<RefCell<Option<Shared>>> = Rc::new(RefCell::new(None));
    let boot = RefCell::new(Some((cfg, store)));

    app.connect_activate(move |app| {
        if let Some(shared) = state.borrow().as_ref() {
            shared.borrow().window().present();
            return;
        }
        let Some((cfg, store)) = boot.borrow_mut().take() else {
            return;
        };

        for path in theme::install(cfg.theme.as_deref(), cfg.font_size) {
            println!("[css] layered {path}");
        }
        // A pane is a selectable `GtkLabel`, and GTK selects a selectable
        // label's whole text when it takes focus by any means other than a
        // click. So coming back to the window - which restores focus to the
        // label that had it - selected the entire pane, and a pane with a
        // selection is deliberately not redrawn (`snapshot` skips it so the
        // selection survives), which read as a frozen terminal until the
        // user clicked the text away. Selection by drag is unaffected.
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_label_select_on_focus(false);
        }
        icons::install();
        let widgets = ui::build(app, &cfg);
        let window = widgets.window.clone();
        let session_socket = cfg.tmux_socket.clone();

        let inner = match App::new(cfg, store, app, widgets.clone()) {
            Ok(inner) => inner,
            Err(e) => {
                // A socket left behind by a dead server makes every tmux
                // client fail this way, including plain `tmux ls`, so say
                // where to look instead of just repeating tmux.
                eprintln!("cannot start tmux session: {e}");
                eprintln!(
                    "  socket: {}; if no tmux server is running, a stale socket file there \
                     will block a new one - remove it and try again",
                    socket_hint(&session_socket)
                );
                app.quit();
                return;
            }
        };
        let shared: Shared = Rc::new(RefCell::new(inner));
        shared.borrow_mut().reload();
        if let Some(name) = &restore {
            let result = shared.borrow_mut().restore(name);
            if let Err(e) = result {
                eprintln!("cannot restore {name}: {e}");
            }
        }
        // Nothing to say yet: the bar states what is open.
        wire(&shared, &widgets);
        app::start_ticking(&shared);
        *state.borrow_mut() = Some(shared);
        window.present();
    });

    // GTK must not try to parse our own flags as GApplication options.
    let exit_code = app.run_with_args::<&str>(&[]);
    exit_code.into()
}

/// Apply a mutation, then report the outcome.
///
/// The result is bound to a local BEFORE the arms run. Writing
/// `match shared.borrow_mut().f() { .. }` instead keeps the mutable guard
/// alive for the whole `match`, so the `borrow()` in an arm aborts the
/// process. Every mutating handler goes through here so that cannot recur.
fn apply(
    shared: &Shared,
    w: &ui::Widgets,
    context: &str,
    change: impl FnOnce(&mut App) -> Result<(), String>,
) {
    let result = change(&mut shared.borrow_mut());
    match result {
        // The bottom bar already carries the counts, live and always. The
        // status line is the transient channel: clearing it is what says
        // "that worked", and it drops the last error rather than leaving it
        // to be read as current.
        Ok(()) => ui::set_status(w, ""),
        Err(e) => ui::toast(w, &format!("{context}: {e}")),
    }
}

/// Connect every signal. Handlers never hold a borrow across a call that can
/// re-enter (dialogs, `select_row`), which is why the mutations below are
/// one-liners through `apply`.
fn wire(shared: &Shared, w: &ui::Widgets) {
    w.add_project.connect_clicked({
        let shared = shared.clone();
        let w = w.clone();
        move |_| {
            let shared = shared.clone();
            let w = w.clone();
            ui::choose_project_folder(&w.window.clone(), move |path| {
                apply(&shared, &w, "Cannot add project", |app| {
                    app.add_project(path)
                });
            });
        }
    });

    // Primary action: spawn a terminal window
    w.add_agent.connect_clicked({
        let shared = shared.clone();
        let w = w.clone();
        move |_| {
            let project = {
                let app = shared.borrow();
                app.selected
            };
            let Some(project) = project else { return };
            let terminal = taix_core::terminal();
            apply(&shared, &w, "Cannot open terminal", |app| {
                app.spawn(project, &terminal).map(drop)
            });
        }
    });
    // The sidebar's `+` is the same action, where the list of windows is.
    w.sidebar_new.connect_clicked({
        let shared = shared.clone();
        let w = w.clone();
        move |_| {
            let project = shared.borrow().selected;
            let Some(project) = project else { return };
            let terminal = taix_core::terminal();
            apply(&shared, &w, "Cannot open terminal", |app| {
                app.spawn(project, &terminal).map(drop)
            });
        }
    });
    w.filter.connect_search_changed({
        let shared = shared.clone();
        move |_| shared.borrow().filter_sidebar()
    });

    // Menu action: spawn with a specific harness. One action with the id as
    // its target, so the list can be rebuilt from a new config without
    // re-registering anything.
    let action_group = gtk::gio::SimpleActionGroup::new();
    let spawn = gtk::gio::SimpleAction::new("spawn", Some(glib::VariantTy::STRING));
    spawn.connect_activate({
        let shared = shared.clone();
        let w = w.clone();
        move |_, param| {
            let Some(id) = param.and_then(|p| p.str()) else {
                return;
            };
            let (project, harness) = {
                let app = shared.borrow();
                (app.selected, taix_core::by_id(&app.cfg, id))
            };
            let Some(project) = project else { return };
            apply(&shared, &w, "Cannot start agent", |app| {
                app.spawn(project, &harness).map(drop)
            });
        }
    });
    action_group.add_action(&spawn);
    w.window.insert_action_group("harness", Some(&action_group));

    // Header menu. Every item is also a keystroke; the menu is what makes
    // them discoverable, so both routes go through the same intents.
    let taix_actions = gtk::gio::SimpleActionGroup::new();
    let push = |name: &str, run: Box<dyn Fn(&Shared)>| {
        let action = gtk::gio::SimpleAction::new(name, None);
        let shared = shared.clone();
        action.connect_activate(move |_, _| run(&shared));
        taix_actions.add_action(&action);
    };
    push("palette", Box::new(|s| s.borrow_mut().open_palette()));
    push("find", Box::new(|s| s.borrow_mut().open_find()));
    push(
        "save-session",
        Box::new(|s| s.borrow().prompt_save_session()),
    );
    push(
        "restore-session",
        Box::new(|s| s.borrow_mut().open_session_picker()),
    );
    push("mute", Box::new(|s| s.borrow_mut().toggle_mute()));
    push(
        "copy-terminal",
        Box::new(|s| s.borrow().copy_terminal_command()),
    );
    push("copy-tmux", Box::new(|s| s.borrow().copy_tmux_command()));
    push(
        "settings",
        Box::new({
            let w = w.clone();
            move |s| {
                let cfg = s.borrow().cfg.clone();
                let bridge = web::Bridge {
                    health: Rc::new({
                        let s = s.clone();
                        move || s.borrow().web.health()
                    }),
                    act: Rc::new({
                        let s = s.clone();
                        move |act| {
                            let mut app = s.borrow_mut();
                            let cfg = app.cfg.clone();
                            app.web.act(act, &cfg);
                        }
                    }),
                    devices: Rc::new({
                        let s = s.clone();
                        move || s.borrow().web.paired()
                    }),
                    forget: Rc::new({
                        let s = s.clone();
                        move |ip| {
                            s.borrow().web.forget(ip);
                        }
                    }),
                    rotate: Rc::new({
                        let s = s.clone();
                        move || s.borrow().web.rotate_key()
                    }),
                };
                let s2 = s.clone();
                settings::open(&w.window, cfg, bridge, move || {
                    s2.borrow_mut().config_changed()
                });
            }
        }),
    );
    push(
        "automation",
        Box::new({
            let w = w.clone();
            move |s| {
                let cfg = s.borrow().cfg.clone();
                // A second handle on the same file: every mutation is
                // flocked, so this is cheaper than borrowing the app from a
                // dialog callback.
                match taix_core::Store::open(&taix_core::Config::store_path()) {
                    Ok(store) => {
                        let (a, b) = (s.clone(), s.clone());
                        jobs::open(
                            &w.window,
                            cfg,
                            std::rc::Rc::new(store),
                            move || a.borrow_mut().jobs_changed(),
                            move |id| b.borrow_mut().request_job_run(id),
                        );
                    }
                    Err(e) => ui::toast(&w, &format!("Cannot open the store: {e}")),
                }
            }
        }),
    );

    // One parameterised action for the five presets, the same shape the
    // per-window colour submenu uses.
    let preset = gtk::gio::SimpleAction::new("preset", Some(glib::VariantTy::STRING));
    {
        let shared = shared.clone();
        preset.connect_activate(move |_, param| {
            if let Some(id) = param.and_then(|p| p.str()) {
                shared.borrow_mut().set_preset(id);
            }
        });
    }
    taix_actions.add_action(&preset);
    w.window.insert_action_group("taix", Some(&taix_actions));

    // Find bar. Searching reads a pane's whole tmux history, so it runs on a
    // keystroke in the entry rather than on a timer.
    w.find.entry.connect_search_changed({
        let shared = shared.clone();
        move |entry| {
            let needle = entry.text().to_string();
            shared.borrow_mut().search(&needle);
        }
    });
    w.find.entry.connect_activate({
        let shared = shared.clone();
        move |_| shared.borrow_mut().find_step(1)
    });
    w.find.entry.connect_stop_search({
        let shared = shared.clone();
        move |_| shared.borrow_mut().close_find()
    });
    w.find.next.connect_clicked({
        let shared = shared.clone();
        move |_| shared.borrow_mut().find_step(1)
    });
    w.find.prev.connect_clicked({
        let shared = shared.clone();
        move |_| shared.borrow_mut().find_step(-1)
    });
    w.find.close.connect_clicked({
        let shared = shared.clone();
        move |_| shared.borrow_mut().close_find()
    });

    // One button per tab: it opens the column on that tab, and pressing the
    // lit one closes the column.
    w.files_toggle.connect_clicked({
        let shared = shared.clone();
        move |_| shared.borrow_mut().click_panel(app::Panel::Files)
    });
    w.git_toggle.connect_clicked({
        let shared = shared.clone();
        move |_| shared.borrow_mut().click_panel(app::Panel::Git)
    });
    // Everything the panels cannot do alone - the editor, the focused pane,
    // the status line, the diff viewer - through the pointer queue, because
    // the click happens while the panel is mid-borrow.
    w.files.connect({
        let shared = shared.clone();
        move |ask| shared.borrow().ask_files(ask)
    });
    w.git.connect({
        let shared = shared.clone();
        move |ask| shared.borrow().ask_git(ask)
    });

    // Hovering the bar's memory segment fills and shows the perf monitor.
    // `try_borrow`, because a pointer crossing the bar is not worth a panic
    // if something upstream is mid-mutation: the reading it draws is at most
    // two seconds old either way.
    w.perf.connect_show({
        let shared = shared.clone();
        move || {
            if let Ok(app) = shared.try_borrow() {
                app.refresh_perf();
            }
        }
    });

    w.window.connect_close_request({
        let shared = shared.clone();
        move |_| {
            let mut app = shared.borrow_mut();
            app.save_layout();
            app.save_histories();
            app.flush_logs();
            glib::Propagation::Proceed
        }
    });
    // A window that was asleep repaints the moment it is looked at again,
    // not on the next reconcile pass.
    w.window.connect_is_active_notify(|_| app::wake());
    w.window.connect_map(|_| app::wake());
    // The focused pane is a real terminal, so the keyboard belongs to it:
    // Tab completes, Ctrl-C interrupts, Escape reaches vim. TaiX therefore
    // owns exactly one namespace, `Ctrl+Shift`, plus `Ctrl+Tab`.
    //
    // Capture phase, because the pane label is selectable and would otherwise
    // eat the arrow keys before the terminal ever saw them.
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed({
        let shared = shared.clone();
        let w = w.clone();
        move |_, key, _, modifiers| {
            use gtk::gdk::{Key, ModifierType};
            let ctrl = modifiers.contains(ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(ModifierType::SHIFT_MASK);

            // Shift+Page is the scrollback habit every terminal already has.
            // tmux owns the history, so this is a different capture rather
            // than a viewport move.
            if shift && !ctrl {
                match key {
                    Key::Page_Up | Key::KP_Page_Up => {
                        shared.borrow_mut().scroll_focused(PAGE);
                        return glib::Propagation::Stop;
                    }
                    Key::Page_Down | Key::KP_Page_Down => {
                        shared.borrow_mut().scroll_focused(-PAGE);
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            if ctrl && matches!(key, Key::Tab | Key::ISO_Left_Tab) {
                // The bar names the focused window, so nothing to announce.
                shared.borrow_mut().cycle_focus();
                return glib::Propagation::Stop;
            }
            if ctrl && shift {
                match key {
                    Key::N | Key::n => w.add_agent.emit_clicked(),
                    Key::B | Key::b => shared.borrow_mut().open_browser_window(),
                    Key::T | Key::t => w.files_toggle.emit_clicked(),
                    Key::G | Key::g => w.git_toggle.emit_clicked(),
                    Key::V | Key::v => shared.borrow().paste(),
                    Key::C | Key::c => copy_selection(&shared, &w),
                    Key::plus | Key::equal | Key::KP_Add => shared.borrow_mut().zoom(1.0),
                    Key::minus | Key::underscore | Key::KP_Subtract => {
                        shared.borrow_mut().zoom(-1.0)
                    }
                    Key::_0 | Key::parenright | Key::KP_0 => shared.borrow_mut().zoom_reset(),
                    // Rename the focused window without reaching for a menu.
                    Key::R | Key::r => {
                        let Some((id, name, _)) = focused_agent(&shared) else {
                            return glib::Propagation::Stop;
                        };
                        let shared = shared.clone();
                        let w2 = w.clone();
                        ui::prompt_text(&w.window, "Rename window", &name, "Rename", move |new| {
                            apply(&shared, &w2, "Cannot rename", |app| app.rename(id, &new));
                        });
                    }
                    // Close without the dialog; `K` is the one that offers to
                    // discard a worktree, and that question must be asked.
                    Key::W | Key::w => {
                        let Some((id, ..)) = focused_agent(&shared) else {
                            return glib::Propagation::Stop;
                        };
                        apply(&shared, &w, "Cannot close window", |app| {
                            app.kill_agent(id, false)
                        });
                    }
                    Key::P | Key::p => shared.borrow_mut().open_palette(),
                    Key::F | Key::f => shared.borrow_mut().open_find(),
                    Key::J | Key::j => {
                        gtk::prelude::WidgetExt::activate_action(
                            &w.window,
                            "taix.automation",
                            None,
                        )
                        .ok();
                    }
                    // Fold moved off `F` when find took it: find is the one
                    // every terminal user reaches for without thinking.
                    Key::E | Key::e => shared.borrow_mut().toggle_fold_selected(),
                    Key::Z | Key::z => shared.borrow_mut().toggle_zoom(),
                    Key::M | Key::m => shared.borrow_mut().toggle_mute(),
                    Key::S | Key::s => shared.borrow().prompt_save_session(),
                    Key::O | Key::o => shared.borrow_mut().open_session_picker(),
                    Key::Up | Key::KP_Up => shared.borrow_mut().scroll_focused(1),
                    Key::Down | Key::KP_Down => shared.borrow_mut().scroll_focused(-1),
                    Key::Left => shared.borrow_mut().cycle_project(-1),
                    Key::Right => shared.borrow_mut().cycle_project(1),
                    // Jump straight to the nth window of this project.
                    Key::_1 | Key::exclam => shared.borrow_mut().focus_nth(0),
                    Key::_2 | Key::at => shared.borrow_mut().focus_nth(1),
                    Key::_3 | Key::numbersign => shared.borrow_mut().focus_nth(2),
                    Key::_4 | Key::dollar => shared.borrow_mut().focus_nth(3),
                    Key::_5 | Key::percent => shared.borrow_mut().focus_nth(4),
                    Key::_6 | Key::asciicircum => shared.borrow_mut().focus_nth(5),
                    Key::_7 | Key::ampersand => shared.borrow_mut().focus_nth(6),
                    Key::_8 | Key::asterisk => shared.borrow_mut().focus_nth(7),
                    Key::_9 | Key::parenleft => shared.borrow_mut().focus_nth(8),
                    Key::K | Key::k => {
                        let Some((id, name, has_worktree)) = focused_agent(&shared) else {
                            return glib::Propagation::Stop;
                        };
                        let shared = shared.clone();
                        let w2 = w.clone();
                        ui::confirm_kill(&w.window, &name, has_worktree, move |discard| {
                            apply(&shared, &w2, "Cannot stop window", |app| {
                                app.kill_agent(id, discard)
                            });
                        });
                    }
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            // The browser panel types for itself: the address bar (a
            // `GtkEntry` focuses its inner `GtkText`), the page, and the
            // developer console. Forwarding those to tmux is what made a
            // login form - and a `console.log` - land in a shell.
            if gtk::prelude::RootExt::focus(&w.window).is_some_and(owns_keys) {
                return glib::Propagation::Proceed;
            }
            match keys::translate(key, modifiers) {
                Some(press) => {
                    shared.borrow_mut().key(&press);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        }
    });
    w.window.add_controller(controller);
}

/// Whether the focused widget types for itself rather than for tmux.
///
/// Asked of the widget rather than of a list of places: every WebKit view -
/// the page and the inspector's own - is a `WebViewBase`, so one check
/// covers the panel however it grows. A row of the files tree is a key
/// target too: arrows walk it, Enter opens, F2 renames.
fn owns_keys(focus: gtk::Widget) -> bool {
    if focus.is::<gtk::Text>() || focus.is::<gtk::Entry>() || focus.has_css_class("file-row") {
        return true;
    }
    #[cfg(feature = "browser")]
    if focus.is::<webkit6::WebViewBase>() {
        return true;
    }
    false
}

/// Copy the focused pane's selection. `Ctrl-C` now belongs to the terminal,
/// so without this there is no way to get text out.
fn copy_selection(shared: &Shared, w: &ui::Widgets) {
    if let Some(text) = shared.borrow().selected_text() {
        w.window.clipboard().set_text(text.as_str());
        ui::set_status(w, &format!("copied {} chars", text.chars().count()));
    }
}

fn focused_agent(shared: &Shared) -> Option<(taix_core::AgentId, String, bool)> {
    let app = shared.borrow();
    let id = app.focused()?;
    app.agent_meta(id)
}
