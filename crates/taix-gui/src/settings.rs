//! The settings dialog: `config.toml`, with a face.
//!
//! A sidebar of sections on the left, one scrolling page of rows on the
//! right, and no OK button: every control writes into a draft [`Config`],
//! saves it and tells the app to reload, so a palette can be judged against
//! the window behind the dialog. Text fields are the exception - they apply
//! on Enter or when they lose focus, since a half-typed command must not be
//! launched.
//!
//! The widgets are the app's own boxes and labels rather than
//! `AdwPreferencesPage` rows. Everything else in TaiX is drawn by
//! `theme.rs`; a stock preferences list in the middle of it read as a
//! different program, which is what this file exists to fix.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;

use taix_core::{AgentKind, Config, Entry};

use crate::icons;
use crate::kit::{self, chips, group, picker, row, stat, stepper, text};
use crate::ui::COLORS;

/// The palettes `theme.rs` knows, in the order the picker lists them.
const THEMES: [(&str, &str); 4] = [
    ("taix", "TaiX (dark glass)"),
    ("catppuccin-mocha", "Catppuccin Mocha"),
    ("catppuccin-latte", "Catppuccin Latte"),
    ("pywal", "pywal (desktop colours)"),
];

/// The bar segment ids `ui::set_bar` fills.
const SEGMENTS: [&str; 6] = ["where", "branch", "windows", "memory", "uptime", "empty"];

/// The sidebar, in order. The first six are groups of the General page and
/// scroll to it; `agents` is the other page.
const NAV: [(&str, &str, bool); 7] = [
    ("appearance", "Appearance", false),
    ("windows", "Windows", false),
    ("tmux", "tmux", false),
    ("panels", "Panels", false),
    ("web", "Web", false),
    ("mcp", "MCP", false),
    ("agents", "Agents", true),
];

const GENERAL_SUB: &str = "Appearance, window behaviour, the tmux backend, the side panels, the web front end and MCP integration";

/// Shared draft plus the two things every control needs: a way to save and a
/// way to complain.
struct Editor {
    draft: Rc<RefCell<Config>>,
    changed: Rc<dyn Fn()>,
    toasts: adw::ToastOverlay,
}

impl Editor {
    /// Mutate the draft, write it out, and let the window redraw.
    fn commit(&self, edit: impl FnOnce(&mut Config)) {
        {
            let mut draft = self.draft.borrow_mut();
            edit(&mut draft);
            // Tables that no longer change anything are dropped rather than
            // written as `[agents.claude]` with nothing under it.
            let builtin: Vec<String> = taix_core::catalog().into_iter().map(|h| h.id).collect();
            draft.agents.retain(|id, kind| {
                !(kind.is_empty() && (builtin.contains(id) || id == taix_core::TERMINAL))
            });
            if let Err(e) = draft.save() {
                self.toasts
                    .add_toast(adw::Toast::new(&format!("Cannot save settings: {e}")));
                return;
            }
        }
        (self.changed)();
    }

    /// The config table for a harness, created on demand.
    fn kind<R>(&self, id: &str, f: impl FnOnce(&mut AgentKind) -> R) -> R {
        let mut draft = self.draft.borrow_mut();
        f(draft.agents.entry(id.to_string()).or_default())
    }
}

/// Open the dialog over `parent`. `changed` runs after every saved edit.
pub fn open(
    parent: &adw::ApplicationWindow,
    cfg: Config,
    bridge: crate::web::Bridge,
    changed: impl Fn() + 'static,
) {
    let home = std::env::var("HOME").ok();
    let shell = kit::shell(
        "Settings",
        "Settings",
        &short_path(
            &Config::default_path().display().to_string(),
            home.as_deref(),
        ),
        "Saved on every change. Comments in a hand-edited file are not kept.",
        "saved automatically",
        &NAV,
    );
    let (dialog, nav, stack) = (shell.dialog.clone(), shell.nav.clone(), shell.stack.clone());
    let (h1, h2) = (shell.h1.clone(), shell.h2.clone());
    h1.set_label("General");
    h2.set_label(GENERAL_SUB);
    let nav_count = shell
        .count("agents")
        .cloned()
        .unwrap_or_else(|| gtk::Label::new(None));
    let ed = Rc::new(Editor {
        draft: Rc::new(RefCell::new(cfg)),
        toasts: shell.toasts.clone(),
        changed: Rc::new(changed),
    });

    let filter = gtk::SearchEntry::builder()
        .placeholder_text("Filter agents")
        .width_chars(16)
        .valign(gtk::Align::Center)
        .css_classes(["set-filter"])
        .visible(false)
        .build();
    shell.head_slot.append(&filter);

    let (general, page, groups) = general_page(&ed, &bridge);
    let (agents, show_agent_head) = agents_page(&ed, &h2, &nav_count, &filter);
    stack.add_named(&general, Some("general"));
    stack.add_named(&agents, Some("agents"));

    // Navigation ----------------------------------------------------------
    // One flag keeps the two directions apart: a click scrolls the page, a
    // scroll moves the selection, and whichever is driving silences the
    // other for the length of its own update. Without it, clicking "Panels"
    // on a short window scrolled to the end, which the spy read as still
    // being in "Appearance" and selected it right back.
    let syncing = Rc::new(Cell::new(false));
    nav.connect_row_selected({
        let (stack, h1, h2, filter) = (stack.clone(), h1.clone(), h2.clone(), filter.clone());
        let (general, groups, syncing) = (general.clone(), groups.clone(), syncing.clone());
        let page = page.clone();
        move |_, row| {
            let Some(row) = row else { return };
            let id = NAV[row.index().max(0) as usize].0;
            if id == "agents" {
                stack.set_visible_child_name("agents");
                h1.set_label("Agents");
                show_agent_head();
                filter.set_visible(true);
                return;
            }
            stack.set_visible_child_name("general");
            h1.set_label("General");
            h2.set_label(GENERAL_SUB);
            filter.set_visible(false);
            if syncing.get() {
                return;
            }
            if let Some((_, group)) = groups.iter().find(|(gid, _)| *gid == id) {
                let y = group.compute_bounds(&page).map_or(0.0, |r| r.y() as f64);
                syncing.set(true);
                general.vadjustment().set_value((y - 12.0).max(0.0));
                syncing.set(false);
            }
        }
    });
    general.vadjustment().connect_value_changed({
        let (nav, stack, groups) = (nav.clone(), stack.clone(), groups.clone());
        let syncing = syncing.clone();
        let page = page.clone();
        move |adj| {
            if syncing.get() || stack.visible_child_name().as_deref() != Some("general") {
                return;
            }
            // Bounds are in the page's own coordinates, so they do not move
            // with the scroll the way the viewport's do. At the end of the
            // scroll the last group wins whether or not it reached the top:
            // a page barely taller than its window can never put it there.
            let mut at = groups.len() - 1;
            if adj.value() < adj.upper() - adj.page_size() - 1.0 {
                let edge = adj.value() + 24.0;
                at = 0;
                for (i, (_, group)) in groups.iter().enumerate() {
                    if group.compute_bounds(&page).map_or(0.0, |r| r.y() as f64) <= edge {
                        at = i;
                    }
                }
            }
            if nav.selected_row().map(|r| r.index()) == Some(at as i32) {
                return;
            }
            syncing.set(true);
            nav.select_row(nav.row_at_index(at as i32).as_ref());
            syncing.set(false);
        }
    });
    nav.select_row(nav.row_at_index(0).as_ref());

    dialog.present(Some(parent));
}

/// `/home/you/.config/taix/config.toml` as `~/.config/taix/config.toml`.
fn short_path(path: &str, home: Option<&str>) -> String {
    match home.filter(|h| !h.is_empty()) {
        Some(home) => path
            .strip_prefix(home)
            .map_or_else(|| path.to_string(), |rest| format!("~{rest}")),
        None => path.to_string(),
    }
}

// General ------------------------------------------------------------------

/// The scroller, the box inside it - group positions are measured against
/// that, not the viewport, which moves as you scroll - and the groups in
/// nav order.
fn general_page(
    ed: &Rc<Editor>,
    bridge: &crate::web::Bridge,
) -> (
    gtk::ScrolledWindow,
    gtk::Box,
    Rc<Vec<(&'static str, gtk::Box)>>,
) {
    let cfg = ed.draft.borrow().clone();
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(20)
        .css_classes(["set-page"])
        .build();

    // Appearance.
    let look = group("Appearance", None, None);
    let labels: Vec<String> = THEMES.iter().map(|(_, l)| (*l).to_string()).collect();
    let current = cfg.theme.as_deref().unwrap_or("taix");
    let chosen = THEMES
        .iter()
        .position(|(id, _)| *id == current)
        .unwrap_or(0);
    let palette = picker(
        &labels,
        chosen,
        |i| Some(swatch_strip(THEMES[i].0).upcast()),
        {
            let ed = ed.clone();
            move |i| {
                let id = THEMES[i].0;
                ed.commit(|c| c.theme = (id != "taix").then(|| id.to_string()));
            }
        },
    );
    look.append(&row("Palette", "Applied immediately", &palette));
    look.append(&row(
        "Pane font size",
        "Points. Ctrl+Shift +/- zooms at runtime",
        &stepper(cfg.font_size, 6.0, 24.0, 0.5, 1, "", {
            let ed = ed.clone();
            move |v| ed.commit(|c| c.font_size = v)
        }),
    ));
    look.append(&row(
        "Bottom bar segments",
        "Left to right, in this order",
        &chips(
            cfg.bar.clone(),
            SEGMENTS.iter().map(|s| s.to_string()).collect(),
            {
                let ed = ed.clone();
                move |v| ed.commit(|c| c.bar = v)
            },
        ),
    ));
    page.append(&look);

    // Windows.
    let windows = group("Windows", None, None);
    let isolate = gtk::Switch::builder()
        .active(cfg.isolate)
        .valign(gtk::Align::Center)
        .build();
    isolate.connect_active_notify({
        let ed = ed.clone();
        move |s| ed.commit(|c| c.isolate = s.is_active())
    });
    windows.append(&row(
        "Isolate each window in a git worktree",
        "A branch per window. A project's .taix.toml can override this",
        &isolate,
    ));
    windows.append(&row(
        "Idle after",
        "Silence before a working agent reads as waiting",
        &stepper(cfg.idle_after_ms as f64, 200.0, 60_000.0, 100.0, 0, "ms", {
            let ed = ed.clone();
            move |v| ed.commit(|c| c.idle_after_ms = v as u64)
        }),
    ));
    windows.append(&row(
        "Stop idle agents after",
        "0 never stops anything",
        &stepper(
            cfg.reap_idle_after_ms
                .map_or(0.0, |ms| ms as f64 / 60_000.0),
            0.0,
            1440.0,
            5.0,
            0,
            "min",
            {
                let ed = ed.clone();
                move |v| {
                    ed.commit(|c| c.reap_idle_after_ms = (v > 0.0).then_some((v * 60_000.0) as u64))
                }
            },
        ),
    ));
    page.append(&windows);

    // tmux.
    let tmux = group("tmux", None, None);
    tmux.append(&row(
        "Server socket",
        "Takes effect the next time TaiX starts",
        &text(&cfg.tmux_socket, 14, "tmux -L <socket>", {
            let ed = ed.clone();
            move |t| ed.commit(|c| c.tmux_socket = t)
        }),
    ));
    tmux.append(&row(
        "Session name",
        "The session every window is created in",
        &text(&cfg.tmux_session, 14, "tmux -t <session>", {
            let ed = ed.clone();
            move |t| ed.commit(|c| c.tmux_session = t)
        }),
    ));
    page.append(&tmux);

    // Panels.
    let panels = group("Panels", None, None);
    panels.append(&row(
        "Browser home page",
        "Blank opens the TaiX start page",
        &text(
            &cfg.browser_home,
            18,
            "Needs a build with --features browser",
            {
                let ed = ed.clone();
                move |t| ed.commit(|c| c.browser_home = t)
            },
        ),
    ));
    panels.append(&editor_row(ed, &cfg));
    page.append(&panels);

    let web = web_group(ed, &cfg, bridge);
    page.append(&web);

    let mcp = mcp_group(ed, bridge);
    page.append(&mcp);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&page)
        .build();
    let groups = Rc::new(vec![
        ("appearance", look),
        ("windows", windows),
        ("tmux", tmux),
        ("panels", panels),
        ("web", web),
        ("mcp", mcp),
    ]);
    (scroller, page, groups)
}

/// The LAN front end: on or off, which port, and what it is doing right
/// now.
///
/// The status line is polled rather than pushed: the server's health lives
/// on another thread behind a mutex, the dialog is open for seconds at a
/// time, and a second's delay on "binding" is invisible.
fn web_group(ed: &Rc<Editor>, cfg: &Config, bridge: &crate::web::Bridge) -> gtk::Box {
    use taix_web::proto::Life;

    let group = group("Web", None, None);
    let enabled = gtk::Switch::builder()
        .active(cfg.web.enabled)
        .valign(gtk::Align::Center)
        .build();
    enabled.connect_active_notify({
        let ed = ed.clone();
        move |s| ed.commit(|c| c.web.enabled = s.is_active())
    });
    group.append(&row(
        "Serve the UI on this network",
        "A device asks, and the bar asks you before it gets in",
        &enabled,
    ));
    group.append(&row(
        "Port",
        "Rebinds as soon as you change it",
        &stepper(cfg.web.port as f64, 1024.0, 65535.0, 1.0, 0, "", {
            let ed = ed.clone();
            move |v| ed.commit(|c| c.web.port = v as u16)
        }),
    ));

    let ts_sub = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["set-row-sub"])
        .build();
    let ts_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    ts_switch.set_active(cfg.web.tailscale);
    ts_switch.connect_active_notify({
        let ed = ed.clone();
        move |s| {
            ed.commit(|c| c.web.tailscale = s.is_active());
            taix_core::tailscale::invalidate();
        }
    });
    group.append(&kit::row_live("Tailscale only", Some(&ts_sub), &ts_switch));

    let serve_sub = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["set-row-sub"])
        .build();
    let serve_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    serve_switch.connect_active_notify({
        let (serve_sub, cfg) = (serve_sub.clone(), cfg.web.port);
        move |s| {
            let result = if s.is_active() {
                taix_core::tailscale::serve_on(cfg)
            } else {
                taix_core::tailscale::serve_off()
            };
            if let Err(e) = result {
                serve_sub.set_text(&e);
                s.set_active(!s.is_active());
            }
        }
    });
    group.append(&kit::row_live(
        "HTTPS over Tailscale",
        Some(&serve_sub),
        &serve_switch,
    ));

    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["set-row-sub", "mono"])
        .build();
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for (label, act) in [
        ("Start", crate::web::Act::Start),
        ("Stop", crate::web::Act::Stop),
        ("Restart", crate::web::Act::Restart),
    ] {
        let button = gtk::Button::builder()
            .label(label)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        button.connect_clicked({
            let act_on = bridge.act.clone();
            move |_| act_on(act)
        });
        tools.append(&button);
    }
    let open = gtk::Button::builder()
        .label("Open")
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    open.connect_clicked({
        let health = bridge.health.clone();
        move |_| {
            let addr = health().addr;
            if !addr.is_empty() {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(
                    &format!("http://{addr}"),
                    gtk::gio::AppLaunchContext::NONE,
                );
            }
        }
    });
    tools.append(&open);
    group.append(&kit::row_live("Status", Some(&status), &tools));

    let dev_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    let dev_sub = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["set-row-sub"])
        .build();
    let dev_tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let rotate_btn = gtk::Button::builder()
        .label("New pairing key")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    rotate_btn.connect_clicked({
        let (rotate, dev_sub) = (bridge.rotate.clone(), dev_sub.clone());
        move |_| {
            rotate();
            dev_sub.set_text("every paired device has to be let in again");
        }
    });
    dev_tools.append(&rotate_btn);
    let dev_row = kit::row_live("Devices", Some(&dev_sub), &dev_tools);
    dev_row.append(&dev_list);
    group.append(&dev_row);

    let paint = {
        let (
            status,
            ts_sub,
            ts_switch,
            serve_sub,
            serve_switch,
            dev_list,
            dev_sub,
            health,
            devices,
            forget_dev,
        ) = (
            status.clone(),
            ts_sub.clone(),
            ts_switch.clone(),
            serve_sub.clone(),
            serve_switch.clone(),
            dev_list.clone(),
            dev_sub.clone(),
            bridge.health.clone(),
            bridge.devices.clone(),
            bridge.forget.clone(),
        );
        let last_ips = Rc::new(RefCell::new(Vec::<String>::new()));
        move || {
            let now = health();
            let tailnet = taix_core::tailscale::status();
            let running = tailnet.as_ref().filter(|t| t.state == "Running");
            let serve_url = taix_core::tailscale::serve_url();
            status.set_text(&match now.life {
                Life::Off => "stopped".to_string(),
                Life::Starting => format!("binding port {}", now.port),
                Life::Live => {
                    let mut text =
                        format!("live on http://{} · {} watching", now.addr, now.clients);
                    if let Some(ts) = running.filter(|t| !t.name.is_empty()) {
                        text.push_str(&format!("\nalso http://{}:{}", ts.name, now.port));
                    }
                    if let Some(url) = &serve_url {
                        text.push_str(&format!("\nalso {url}"));
                    }
                    text
                }
                Life::Error => format!("port {} failed: {}", now.port, now.error),
            });
            ts_switch.set_sensitive(running.is_some());
            ts_sub.set_text(&match (&tailnet, running) {
                (_, Some(ts)) => match ts.ip {
                    Some(ip) => format!("reachable only from {} · {ip}", ts.name),
                    None => format!("{} has no IPv4 address", ts.name),
                },
                (Some(ts), None) => format!("{}, run `tailscale up`", ts.state.to_lowercase()),
                (None, None) => "not installed".to_string(),
            });
            serve_switch.set_sensitive(running.is_some());
            serve_switch.set_active(serve_url.is_some());
            if serve_url.is_some() {
                serve_sub.set_text("phones opening this URL get TLS, which lets the page use the clipboard and notifications");
            } else {
                serve_sub.set_text("");
            }

            let paired = devices();
            let ips: Vec<String> = paired.iter().map(|d| d.ip.clone()).collect();
            if ips != *last_ips.borrow() {
                while let Some(child) = dev_list.first_child() {
                    dev_list.remove(&child);
                }
                if paired.is_empty() {
                    dev_sub.set_text("nothing paired");
                } else {
                    dev_sub.set_text("");
                    for dev in paired {
                        let tile = gtk::Box::builder()
                            .orientation(gtk::Orientation::Horizontal)
                            .spacing(8)
                            .css_classes(["set-row"])
                            .build();
                        let ip = gtk::Label::builder()
                            .label(&dev.ip)
                            .xalign(0.0)
                            .css_classes(["set-dev-ip", "mono"])
                            .build();
                        let who = gtk::Label::builder()
                            .label(&dev.agent)
                            .xalign(0.0)
                            .css_classes(["set-dev-who"])
                            .build();
                        let time_text = if dev.secs < 60 {
                            format!("seen {}s ago", dev.secs)
                        } else {
                            format!("seen {}m ago", dev.secs / 60)
                        };
                        let time = gtk::Label::builder()
                            .label(&time_text)
                            .xalign(0.0)
                            .css_classes(["set-dev-who"])
                            .build();
                        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                        spacer.set_hexpand(true);
                        let forget = gtk::Button::builder()
                            .label("Forget")
                            .valign(gtk::Align::Center)
                            .css_classes(["flat", "set-dev-forget"])
                            .build();
                        forget.connect_clicked({
                            let (forget_fn, ip) = (forget_dev.clone(), dev.ip.clone());
                            move |_| forget_fn(&ip)
                        });
                        tile.append(&ip);
                        tile.append(&who);
                        tile.append(&time);
                        tile.append(&spacer);
                        tile.append(&forget);
                        dev_list.append(&tile);
                    }
                }
                *last_ips.borrow_mut() = ips;
            }
        }
    };
    paint();
    glib::timeout_add_seconds_local(1, {
        let status = status.clone();
        move || {
            // The dialog was closed: the widget is no longer in a window,
            // and nothing is watching this timer any more.
            if status.root().is_none() {
                return glib::ControlFlow::Break;
            }
            paint();
            glib::ControlFlow::Continue
        }
    });
    group
}

/// What a coding agent needs to reach this window: the stdio command to
/// register, and the address its browser tools talk to. Both are here
/// because the alternative is reading the README with the app open.
fn mcp_group(ed: &Rc<Editor>, bridge: &crate::web::Bridge) -> gtk::Box {
    use taix_web::proto::Life;

    let group = group("MCP", None, None);

    // A user who has not installed TaiX needs the full path, and the
    // registration line they paste has to work from anywhere.
    let exe = match taix_core::which("taix") {
        Some(_) => "taix".to_string(),
        None => std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "taix".into()),
    };
    let cmd = format!("{exe} mcp");

    let copy_button = |ed: &Rc<Editor>, text: Rc<dyn Fn() -> Option<String>>| {
        let button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Copy")
            .css_classes(["flat"])
            .build();
        button.connect_clicked({
            let toasts = ed.toasts.clone();
            move |b| {
                let Some(text) = text() else { return };
                if let Some(window) = b.root().and_downcast::<adw::ApplicationWindow>() {
                    window.clipboard().set_text(&text);
                    toasts.add_toast(adw::Toast::new("copied"));
                }
            }
        });
        button
    };

    let fixed = |title: &str, text: String| {
        let value = gtk::Label::builder()
            .label(&text)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .css_classes(["set-row-sub", "mono"])
            .build();
        let copy = copy_button(ed, Rc::new(move || Some(text.clone())));
        kit::row_live(title, Some(&value), &copy)
    };

    group.append(&fixed("Register with an agent", cmd.clone()));
    group.append(&fixed("Claude Code", format!("claude mcp add taix {cmd}")));
    group.append(&fixed("Codex", format!("codex mcp add taix {cmd}")));
    group.append(&fixed(
        "Config file",
        format!(r#"{{"mcpServers":{{"taix":{{"command":"{exe}","args":["mcp"]}}}}}}"#),
    ));

    // The browser tools reach this window over the same listener the phone
    // uses, so they say what it says - including that it is off.
    let endpoint = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .css_classes(["set-row-sub", "mono"])
        .build();
    let copy = copy_button(ed, {
        let health = bridge.health.clone();
        Rc::new(move || {
            let h = health();
            (h.life == Life::Live).then(|| format!("http://{}/api/browser", h.addr))
        })
    });
    group.append(&kit::row_live(
        "Browser tools endpoint",
        Some(&endpoint),
        &copy,
    ));
    group.append(&kit::row(
        "Needs the desktop",
        "The browser tools drive this window's pages, so they answer only while it is open.",
        &gtk::Box::new(gtk::Orientation::Horizontal, 0),
    ));

    let paint = {
        let (endpoint, health) = (endpoint.clone(), bridge.health.clone());
        move || {
            let h = health();
            endpoint.set_text(&match h.life {
                Life::Live => format!("http://{}/api/browser", h.addr),
                _ => "off - turn the web server on above".to_string(),
            });
        }
    };
    paint();
    glib::timeout_add_seconds_local(1, {
        let endpoint = endpoint.clone();
        move || {
            if endpoint.root().is_none() {
                return glib::ControlFlow::Break;
            }
            paint();
            glib::ControlFlow::Continue
        }
    });

    group
}

/// The editor "open" uses: detected, or any installed one. A config value
/// the machine does not have - a command typed into the file - is listed
/// too, so the row never lies about what is set.
fn editor_row(ed: &Rc<Editor>, cfg: &Config) -> gtk::Box {
    use taix_core::editor;
    let installed = editor::installed();
    let detected = installed
        .first()
        .map_or("none found".to_string(), |e| e.label.clone());
    let mut ids: Vec<Option<String>> = vec![None];
    let mut labels = vec!["Automatic".to_string()];
    for e in &installed {
        ids.push(Some(e.id.clone()));
        labels.push(if e.terminal {
            format!("{} (window)", e.label)
        } else {
            e.label.clone()
        });
    }
    if let Some(chosen) = &cfg.editor
        && !ids.iter().flatten().any(|id| id == chosen)
    {
        ids.push(Some(chosen.clone()));
        labels.push(format!("{chosen} (config)"));
    }
    let at = ids.iter().position(|id| *id == cfg.editor).unwrap_or(0);
    let control = picker(&labels, at, |_| None, {
        let ed = ed.clone();
        move |i| {
            let chosen = ids.get(i).cloned().flatten();
            ed.commit(|c| c.editor = chosen);
        }
    });
    row(
        "Open files with",
        &format!("Automatic picks {detected}. A terminal editor opens in a window"),
        &control,
    )
}

// Agents -------------------------------------------------------------------

/// The agent page, rebuilt in place whenever the list or the filter changes.
struct AgentList {
    body: gtk::Box,
    /// The dialog's subtitle, which this page owns while it is showing.
    head: gtk::Label,
    /// The `2/9` in the sidebar.
    nav_count: gtk::Label,
    summary: RefCell<String>,
    needle: RefCell<String>,
    cards: RefCell<Vec<(String, gtk::Revealer)>>,
}

impl AgentList {
    fn rebuild(&self, ed: &Rc<Editor>, this: &Rc<AgentList>) {
        while let Some(child) = self.body.first_child() {
            self.body.remove(&child);
        }
        self.cards.borrow_mut().clear();

        let cfg = ed.draft.borrow().clone();
        let entries = taix_core::entries(&cfg);
        let total = entries.len();
        let ready = entries.iter().filter(|e| e.installed).count();
        let yours = entries.iter().filter(|e| !e.builtin).count();
        *self.summary.borrow_mut() =
            format!("{ready} of {total} CLI agents detected on this machine");
        self.head.set_label(&self.summary.borrow());
        self.nav_count.set_label(&format!("{ready}/{total}"));

        let stats = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .homogeneous(true)
            .build();
        stats.append(&stat(ready, "Ready to launch"));
        stats.append(&stat(total - ready, "Not installed"));
        stats.append(&stat(yours, "Yours"));
        self.body.append(&stats);

        let needle = self.needle.borrow().clone();
        let shown: Vec<&Entry> = entries.iter().filter(|e| matches(e, &needle)).collect();
        let (available, missing): (Vec<&Entry>, Vec<&Entry>) =
            shown.into_iter().partition(|e| e.installed);

        let add = gtk::Button::builder()
            .label("+ command of your own")
            .css_classes(["set-chip-add"])
            .valign(gtk::Align::Center)
            .build();
        add.connect_clicked({
            let (ed, this) = (ed.clone(), this.clone());
            move |_| this.add_custom(&ed, &this)
        });
        for (title, list, suffix) in [
            ("Available", available, Some(add.upcast::<gtk::Widget>())),
            ("Not installed", missing, None),
        ] {
            if list.is_empty() && suffix.is_none() {
                continue;
            }
            let group = group(title, Some(list.len()), suffix.as_ref());
            for entry in list {
                let card = agent_card(ed, this, entry);
                group.append(&card);
            }
            self.body.append(&group);
        }
        if total > 0 && self.body.last_child().is_none() {
            self.body.append(
                &gtk::Label::builder()
                    .label("Nothing matches that")
                    .css_classes(["set-note"])
                    .build(),
            );
        }
    }

    /// A new blank harness, first in the menu and already open to be filled in.
    fn add_custom(&self, ed: &Rc<Editor>, this: &Rc<AgentList>) {
        let id = {
            let draft = ed.draft.borrow();
            let taken: Vec<&String> = draft.agents.keys().collect();
            (1..)
                .map(|n| {
                    if n == 1 {
                        "custom".to_string()
                    } else {
                        format!("custom-{n}")
                    }
                })
                .find(|id| !taken.contains(&id))
                .unwrap()
        };
        ed.commit(|c| {
            c.agents.insert(
                id.clone(),
                AgentKind {
                    label: "My command".into(),
                    command: String::new(),
                    ..Default::default()
                },
            );
            c.harness_order.insert(0, id.clone());
        });
        self.rebuild(ed, this);
        if let Some((_, revealer)) = self.cards.borrow().iter().find(|(x, _)| *x == id) {
            revealer.set_reveal_child(true);
        }
    }

    /// Write the current order to the draft and redraw.
    fn moved(&self, ed: &Rc<Editor>, this: &Rc<AgentList>, id: &str, delta: isize) {
        let mut ids: Vec<String> = taix_core::entries(&ed.draft.borrow())
            .into_iter()
            .map(|e| e.harness.id)
            .collect();
        let Some(i) = ids.iter().position(|x| x == id) else {
            return;
        };
        let j = i as isize + delta;
        // The terminal is pinned at the top.
        if j < 1 || j as usize >= ids.len() {
            return;
        }
        ids.swap(i, j as usize);
        ids.retain(|x| x != taix_core::TERMINAL);
        ed.commit(|c| c.harness_order = ids);
        self.rebuild(ed, this);
    }
}

/// The page plus a closure that puts its summary back in the dialog head.
fn agents_page(
    ed: &Rc<Editor>,
    head: &gtk::Label,
    nav_count: &gtk::Label,
    filter: &gtk::SearchEntry,
) -> (gtk::ScrolledWindow, Box<dyn Fn()>) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .css_classes(["set-page"])
        .build();
    let list = Rc::new(AgentList {
        body: page.clone(),
        head: head.clone(),
        nav_count: nav_count.clone(),
        summary: RefCell::new(String::new()),
        needle: RefCell::new(String::new()),
        cards: RefCell::new(Vec::new()),
    });
    list.rebuild(ed, &list);
    filter.connect_search_changed({
        let (ed, list) = (ed.clone(), list.clone());
        move |e| {
            *list.needle.borrow_mut() = e.text().to_lowercase();
            list.rebuild(&ed, &list);
        }
    });
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&page)
        .build();
    let show_head = {
        let list = list.clone();
        Box::new(move || list.head.set_label(&list.summary.borrow()))
    };
    (scroller, show_head)
}

fn matches(entry: &Entry, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let h = &entry.harness;
    h.label.to_lowercase().contains(needle)
        || h.id.to_lowercase().contains(needle)
        || h.command.to_lowercase().contains(needle)
}

/// One harness: a card that opens into its settings.
fn agent_card(ed: &Rc<Editor>, list: &Rc<AgentList>, entry: &Entry) -> gtk::Box {
    let h = &entry.harness;
    let id = h.id.clone();
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["set-card"])
        .build();
    if entry.hidden {
        card.add_css_class("dim");
    }

    let icon = icons::image(h.icon.as_deref(), 16);
    icon.add_css_class("harness-icon");
    if let Some(c) = &h.color {
        icon.add_css_class(&format!("tint-{c}"));
    }
    let badge_of = gtk::Box::builder()
        .css_classes(["set-card-icon"])
        .valign(gtk::Align::Center)
        .build();
    badge_of.append(&icon);

    let name = gtk::Label::builder()
        .label(h.label.as_str())
        .xalign(0.0)
        .css_classes(["set-card-name"])
        .build();
    let meta = gtk::Label::builder()
        .label(meta_line(entry))
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["set-card-meta", "mono"])
        .build();
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    titles.set_hexpand(true);
    titles.set_valign(gtk::Align::Center);
    titles.append(&name);
    titles.append(&meta);

    let state = gtk::Label::builder()
        .label(if entry.installed { "ready" } else { "missing" })
        .valign(gtk::Align::Center)
        .css_classes(if entry.installed {
            ["badge", "ready"].as_slice()
        } else {
            ["badge"].as_slice()
        })
        .build();
    let chevron = gtk::Image::from_icon_name("pan-end-symbolic");
    chevron.add_css_class("set-card-chevron");

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.append(&badge_of);
    line.append(&titles);
    line.append(&state);
    line.append(&chevron);
    let head = gtk::Button::builder()
        .child(&line)
        .css_classes(["set-card-head"])
        .build();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["set-card-body"])
        .build();
    let revealer = gtk::Revealer::builder()
        .child(&body)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .build();
    head.connect_clicked({
        let (revealer, chevron) = (revealer.clone(), chevron.clone());
        move |_| {
            let open = !revealer.reveals_child();
            revealer.set_reveal_child(open);
            chevron.set_icon_name(Some(if open {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            }));
        }
    });
    card.append(&head);
    card.append(&revealer);
    list.cards.borrow_mut().push((id.clone(), revealer));

    // Label and command.
    body.append(&row(
        "Label",
        "Shown in menus and as the window name",
        &text(&h.label, 16, "", {
            let (ed, id, name) = (ed.clone(), id.clone(), name.clone());
            move |t| {
                ed.kind(&id, |k| k.label = t.clone());
                ed.commit(|_| {});
                name.set_label(&t);
            }
        }),
    ));
    if h.is_terminal() {
        body.append(&row(
            "Command",
            "Nothing is launched",
            &gtk::Label::builder()
                .label("your login shell")
                .valign(gtk::Align::Center)
                .css_classes(["set-note", "mono"])
                .build(),
        ));
    } else {
        body.append(&row(
            "Command",
            "Run in the project directory by tmux",
            &text(&h.command, 20, "", {
                let (ed, id, meta) = (ed.clone(), id.clone(), meta.clone());
                move |t| {
                    ed.kind(&id, |k| k.command = t.clone());
                    ed.commit(|_| {});
                    let on_path =
                        taix_core::which(t.split_whitespace().next().unwrap_or("")).is_some();
                    meta.set_label(&command_line(&id, &t, on_path));
                }
            }),
        ));
    }

    // Icon. `Default` falls back to the catalogue's, not the user's.
    let stock = if h.is_terminal() {
        taix_core::terminal().icon
    } else {
        taix_core::catalog()
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.icon)
    };
    let picker = icon_picker(h.icon.as_deref(), stock.clone(), {
        let (ed, id, icon) = (ed.clone(), id.clone(), icon.clone());
        move |choice| {
            ed.kind(&id, |k| k.icon = choice.clone());
            ed.commit(|_| {});
            icons::set(&icon, choice.as_deref().or(stock.as_deref()));
        }
    });
    body.append(&row("Icon", "", &picker));

    // Colour: nine swatches, one group of toggles.
    let swatches = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    swatches.set_valign(gtk::Align::Center);
    let mut first: Option<gtk::ToggleButton> = None;
    for (value, label) in std::iter::once(("none", "Default")).chain(COLORS) {
        let swatch = gtk::ToggleButton::builder()
            .css_classes(["swatch", &format!("tint-{value}")])
            .tooltip_text(label)
            .build();
        if let Some(f) = &first {
            swatch.set_group(Some(f));
        } else {
            first = Some(swatch.clone());
        }
        swatch.set_active(h.color.as_deref().unwrap_or("none") == value);
        swatch.connect_toggled({
            let (ed, id, icon) = (ed.clone(), id.clone(), icon.clone());
            move |b| {
                if !b.is_active() {
                    return;
                }
                let color = (value != "none").then(|| value.to_string());
                ed.kind(&id, |k| k.color = color.clone());
                ed.commit(|_| {});
                let mut classes = vec!["harness-icon".to_string()];
                if let Some(c) = &color {
                    classes.push(format!("tint-{c}"));
                }
                let refs: Vec<&str> = classes.iter().map(String::as_str).collect();
                icon.set_css_classes(&refs);
            }
        });
        swatches.append(&swatch);
    }
    body.append(&row(
        "Colour",
        "Tints its windows unless one is recoloured on its own",
        &swatches,
    ));

    // Offered or not.
    if !h.is_terminal() {
        let shown = gtk::Switch::builder()
            .active(!entry.hidden)
            .valign(gtk::Align::Center)
            .build();
        shown.connect_active_notify({
            let (ed, id, card) = (ed.clone(), id.clone(), card.clone());
            move |s| {
                let hidden = !s.is_active();
                ed.kind(&id, |k| k.hidden = hidden);
                ed.commit(|_| {});
                if hidden {
                    card.add_css_class("dim");
                } else {
                    card.remove_css_class("dim");
                }
            }
        });
        body.append(&row("Show in menus", "", &shown));
    }

    // Order and reset.
    let tools = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::End)
        .css_classes(["set-card-foot"])
        .build();
    for (icon_name, tip, delta) in [
        ("go-up-symbolic", "Move up", -1isize),
        ("go-down-symbolic", "Move down", 1),
    ] {
        let button = gtk::Button::builder()
            .icon_name(icon_name)
            .tooltip_text(tip)
            .css_classes(["set-tool"])
            .build();
        button.connect_clicked({
            let (ed, list, id) = (ed.clone(), list.clone(), id.clone());
            move |_| list.moved(&ed, &list, &id, delta)
        });
        tools.append(&button);
    }
    let reset = gtk::Button::builder()
        .label(if entry.builtin { "Reset" } else { "Remove" })
        .css_classes(["set-tool", "danger"])
        .build();
    reset.set_sensitive(!entry.builtin || ed.draft.borrow().agents.contains_key(&id));
    reset.connect_clicked({
        let (ed, list, id) = (ed.clone(), list.clone(), id.clone());
        move |_| {
            ed.commit(|c| {
                c.agents.remove(&id);
                c.harness_order.retain(|x| x != &id);
            });
            list.rebuild(&ed, &list);
        }
    });
    tools.append(&reset);
    body.append(&tools);
    card
}

/// The line under an agent's name: what it runs, and what is wrong with it.
fn meta_line(entry: &Entry) -> String {
    let h = &entry.harness;
    if h.is_terminal() {
        return format!("{} · login shell", h.id);
    }
    let mut s = command_line(&h.id, &h.command, entry.installed);
    if !entry.builtin {
        s.push_str(" · yours");
    }
    s
}

fn command_line(id: &str, command: &str, installed: bool) -> String {
    if command.is_empty() {
        return format!("{id} · no command yet");
    }
    if installed {
        format!("{id} · {command}")
    } else {
        format!("{id} · not on PATH")
    }
}

/// Three colours of a palette, painted rather than styled: CSS cannot carry
/// a hex that is only known at runtime without a provider per swatch.
fn swatch_strip(theme: &str) -> gtk::DrawingArea {
    let colors = crate::theme::preview(theme);
    let area = gtk::DrawingArea::builder()
        .content_width(30)
        .content_height(9)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |_, cr, _, height| {
        for (i, hex) in colors.iter().enumerate() {
            let rgba = gtk::gdk::RGBA::parse(hex).unwrap_or(gtk::gdk::RGBA::WHITE);
            cr.set_source_rgb(rgba.red() as f64, rgba.green() as f64, rgba.blue() as f64);
            cr.rectangle(i as f64 * 11.0, (height as f64 - 9.0) / 2.0, 9.0, 9.0);
            let _ = cr.fill();
        }
    });
    area
}

/// A button showing the current icon; its popover offers every bundled icon,
/// an image file, and the harness's own default.
fn icon_picker(
    current: Option<&str>,
    stock: Option<String>,
    on_pick: impl Fn(Option<String>) + 'static,
) -> gtk::MenuButton {
    let on_pick = Rc::new(on_pick);
    let shown = icons::image(current.or(stock.as_deref()), 18);
    let button = gtk::MenuButton::builder()
        .child(&shown)
        .css_classes(["set-pick", "icon-pick"])
        .valign(gtk::Align::Center)
        .build();
    let popover = gtk::Popover::new();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.add_css_class("icon-grid");
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(7)
        .max_children_per_line(7)
        .row_spacing(2)
        .column_spacing(2)
        .build();
    for (name, _) in icons::BUNDLED {
        let b = gtk::Button::builder()
            .child(&icons::image(Some(name), 18))
            .css_classes(["flat", "icon-cell"])
            .tooltip_text(*name)
            .build();
        b.connect_clicked({
            let (on_pick, shown, popover) = (on_pick.clone(), shown.clone(), popover.clone());
            move |_| {
                icons::set(&shown, Some(name));
                on_pick(Some(name.to_string()));
                popover.popdown();
            }
        });
        grid.insert(&b, -1);
    }
    body.append(&grid);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let file = gtk::Button::builder()
        .label("Image file…")
        .css_classes(["flat"])
        .build();
    file.connect_clicked({
        let (on_pick, shown, popover) = (on_pick.clone(), shown.clone(), popover.clone());
        move |b| {
            popover.popdown();
            let window = b.root().and_downcast::<gtk::Window>();
            let filter = gtk::FileFilter::new();
            filter.add_pixbuf_formats();
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let dialog = gtk::FileDialog::builder()
                .title("Choose an icon")
                .default_filter(&filter)
                .filters(&filters)
                .build();
            let (on_pick, shown) = (on_pick.clone(), shown.clone());
            dialog.open(
                window.as_ref(),
                None::<&gtk::gio::Cancellable>,
                move |result| {
                    if let Ok(f) = result
                        && let Some(path) = f.path()
                    {
                        let path = path.to_string_lossy().to_string();
                        icons::set(&shown, Some(&path));
                        on_pick(Some(path));
                    }
                },
            );
        }
    });
    let default = gtk::Button::builder()
        .label("Default")
        .css_classes(["flat"])
        .build();
    default.connect_clicked({
        let (on_pick, popover, shown) = (on_pick.clone(), popover.clone(), shown.clone());
        move |_| {
            icons::set(&shown, stock.as_deref());
            on_pick(None);
            popover.popdown();
        }
    });
    actions.append(&file);
    actions.append(&default);
    body.append(&actions);
    popover.set_child(Some(&body));
    button.set_popover(Some(&popover));
    button
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_path_is_shown_from_home() {
        assert_eq!(
            short_path("/home/you/.config/taix/config.toml", Some("/home/you")),
            "~/.config/taix/config.toml"
        );
        // Not under it, no HOME at all, and an empty HOME - which would
        // otherwise turn every absolute path into a tilde.
        assert_eq!(
            short_path("/etc/taix.toml", Some("/home/you")),
            "/etc/taix.toml"
        );
        assert_eq!(short_path("/etc/taix.toml", None), "/etc/taix.toml");
        assert_eq!(short_path("/etc/taix.toml", Some("")), "/etc/taix.toml");
    }
}
