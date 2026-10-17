//! What every window is running, and which of it is a server.
//!
//! An agent that starts `vite` or `cargo run` leaves a port behind that the
//! pane has long scrolled past. This lists each window's process tree with
//! the TCP ports in it - for the session over the bar's segment, for one
//! project over its sidebar header, for one window over its sidebar row or
//! pane header - and a port opens in the browser on click.
//!
//! One popover serves every anchor: it is re-parented to whichever one the
//! pointer entered, and unparented when it closes, so a row that goes away
//! never takes a parented popover down with it. Not autohiding, like the
//! perf monitor, so it never takes the keyboard from a pane; unlike it, the
//! pointer may travel into it, because the ports in it are clickable.

use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use taix_core::procs::{Proc, ports, url};
use taix_core::{AgentId, ProjectId};

/// How long the pointer may be over neither anchor nor popover - the gap
/// between them - before it closes.
const GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every window, by project: the bar's segment.
    All,
    /// One project's windows: its sidebar header.
    Project(ProjectId),
    /// One window: its sidebar row and its pane header.
    Window(AgentId),
}

/// One window with a live pane, as the popover shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub id: AgentId,
    pub name: String,
    pub project: String,
    /// The project on screen, which gets the accent.
    pub selected: bool,
    /// Its tree, root first, each process with what it listens on.
    pub procs: Vec<(Proc, Vec<SocketAddr>)>,
}

type Fill = Rc<RefCell<Option<Box<dyn Fn(Scope)>>>>;
type Open = Rc<RefCell<Option<Box<dyn Fn(AgentId, String)>>>>;
/// What was last drawn, to skip rebuilding an unchanged list.
type Shown = (Scope, Vec<Window>);

#[derive(Clone)]
pub struct Procs {
    /// The bar segment: a live dot and the ports, or a process count.
    pub button: gtk::Box,
    dot: gtk::Label,
    summary: gtk::Label,
    popover: gtk::Popover,
    title: gtk::Label,
    count: gtk::Label,
    list: gtk::Box,
    /// What it is showing, while it is open.
    scope: Rc<Cell<Option<Scope>>>,
    in_anchor: Rc<Cell<bool>>,
    in_popover: Rc<Cell<bool>>,
    on_show: Fill,
    on_open: Open,
    last: Rc<RefCell<Option<Shown>>>,
}

impl Procs {
    pub fn new() -> Self {
        let dot = gtk::Label::builder()
            .label("●")
            .css_classes(["procs-dot"])
            .build();
        // Ellipsized, so a long port list can never widen the bar - and with
        // it the window's minimum width - past the screen.
        let summary = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["mono"])
            .build();
        let button = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        button.add_css_class("bar-procs");
        button.append(&dot);
        button.append(&summary);

        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["procs-title"])
            .build();
        let count = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["procs-count"])
            .build();
        let head = gtk::Box::new(gtk::Orientation::Vertical, 2);
        head.add_css_class("procs-head");
        head.append(&title);
        head.append(&count);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("procs-list");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(560)
            .build();

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // On the box, not the scroller: with no horizontal scrollbar a
        // `ScrolledWindow` takes its child's width and ignores its own
        // minimum.
        body.set_width_request(540);
        body.add_css_class("perf");
        body.append(&head);
        body.append(&scroll);

        let popover = gtk::Popover::builder()
            .child(&body)
            .autohide(false)
            .css_classes(["perf-popover", "procs-popover"])
            .build();

        let this = Self {
            button,
            dot,
            summary,
            popover,
            title,
            count,
            list,
            scope: Rc::default(),
            in_anchor: Rc::default(),
            in_popover: Rc::default(),
            on_show: Rc::default(),
            on_open: Rc::default(),
            last: Rc::default(),
        };
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let this = this.clone();
            move |_, _, _| this.in_popover.set(true)
        });
        motion.connect_leave({
            let this = this.clone();
            move |_| {
                this.in_popover.set(false);
                this.close_soon();
            }
        });
        body.add_controller(motion);
        this.attach(&this.button.clone(), Scope::All, gtk::PositionType::Top);
        this
    }

    /// Open beside `anchor`, on its `side`, while the pointer is on it.
    pub fn attach(&self, anchor: &impl IsA<gtk::Widget>, scope: Scope, side: gtk::PositionType) {
        let anchor = anchor.as_ref();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let this = self.clone();
            let anchor = anchor.downgrade();
            move |_, _, _| {
                if let Some(anchor) = anchor.upgrade() {
                    this.open(&anchor, scope, side);
                }
            }
        });
        motion.connect_leave({
            let this = self.clone();
            move |_| {
                this.in_anchor.set(false);
                this.close_soon();
            }
        });
        anchor.add_controller(motion);
        // A pane closed or the sidebar rebuilt under the pointer: let go
        // now, before the anchor can be finalized with us still in it.
        anchor.connect_unmap({
            let this = self.clone();
            move |anchor| {
                if this.popover.parent().as_ref() == Some(anchor) {
                    this.close();
                }
            }
        });
    }

    fn open(&self, anchor: &gtk::Widget, scope: Scope, side: gtk::PositionType) {
        self.in_anchor.set(true);
        if self.popover.parent().as_ref() != Some(anchor) {
            self.close();
            self.popover.set_parent(anchor);
        }
        self.popover.set_position(side);
        self.scope.set(Some(scope));
        if let Some(f) = self.on_show.borrow().as_ref() {
            f(scope);
        }
        self.popover.popup();
    }

    fn close_soon(&self) {
        let this = self.clone();
        glib::timeout_add_local_once(GRACE, move || {
            if !this.in_anchor.get() && !this.in_popover.get() {
                this.close();
            }
        });
    }

    fn close(&self) {
        self.popover.popdown();
        if self.popover.parent().is_some() {
            self.popover.unparent();
        }
        self.scope.set(None);
        self.in_popover.set(false);
    }

    /// Called as it opens, to fill it for a scope.
    pub fn connect_show(&self, f: impl Fn(Scope) + 'static) {
        *self.on_show.borrow_mut() = Some(Box::new(f));
    }

    /// Called with the window and URL of a clicked port.
    pub fn connect_open(&self, f: impl Fn(AgentId, String) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(f));
    }

    /// What it is showing, or None while closed: nobody is reading.
    pub fn open_scope(&self) -> Option<Scope> {
        self.scope.get()
    }

    /// The bar segment: every distinct port across the session, or how
    /// many processes the panes are running when none of them serves.
    pub fn set_summary(&self, ports: &[u16], procs: usize) {
        let text = match ports {
            [] => format!("{procs} proc{}", plural(procs)),
            _ => {
                let mut text: Vec<String> = ports.iter().take(3).map(|p| format!(":{p}")).collect();
                if ports.len() > 3 {
                    text.push(format!("+{}", ports.len() - 3));
                }
                text.join(" ")
            }
        };
        self.summary.set_text(&text);
        if ports.is_empty() {
            self.button.remove_css_class("live");
        } else {
            self.button.add_css_class("live");
        }
        self.dot.set_text(if ports.is_empty() { "○" } else { "●" });
    }

    pub fn set(&self, scope: Scope, windows: &[Window]) {
        if let Some((s, w)) = self.last.borrow().as_ref()
            && *s == scope
            && w == windows
        {
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let procs: usize = windows.iter().map(|w| w.procs.len()).sum();
        let servers = servers(windows);
        let serving = match servers.len() {
            0 => "nothing serving".to_string(),
            n => format!("{n} serving"),
        };
        self.count.set_text(&format!(
            "{serving} · {procs} process{}",
            if procs == 1 { "" } else { "es" }
        ));
        let title = match scope {
            Scope::All => "Processes",
            Scope::Project(_) => windows.first().map_or("Project", |w| w.project.as_str()),
            Scope::Window(_) => windows.first().map_or("Window", |w| w.name.as_str()),
        };
        self.title.set_text(title);

        if !servers.is_empty() {
            self.list.append(&eyebrow("Serving"));
            for (w, p, addr) in servers {
                self.list.append(&self.server(scope, w, p, addr));
            }
        }
        if windows.is_empty() {
            self.list.append(&empty(match scope {
                Scope::Window(_) => "Nothing is running in this window.",
                _ => "No window is running anything.",
            }));
        }
        let mut project: Option<&str> = None;
        for w in windows {
            if scope == Scope::All && project != Some(w.project.as_str()) {
                project = Some(&w.project);
                let head = eyebrow(&w.project);
                if w.selected {
                    head.add_css_class("procs-on");
                }
                self.list.append(&head);
            }
            self.list.append(&self.window(scope, w));
        }
        *self.last.borrow_mut() = Some((scope, windows.to_vec()));
    }

    /// One server: the port first and large, since it is what is looked
    /// for, then what serves it, from where, and who can reach it.
    fn server(&self, scope: Scope, w: &Window, p: &Proc, addr: &SocketAddr) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("procs-server");
        row.set_tooltip_text(Some(&p.cmd));
        row.append(&self.port(w.id, addr));
        let name = text(&p.name, "procs-name");
        let at = match scope {
            Scope::All => format!("{} · {}", w.name, w.project),
            Scope::Project(_) => w.name.clone(),
            Scope::Window(_) => String::new(),
        };
        let at = text(&at, "procs-where");
        at.set_hexpand(true);
        row.append(&name);
        row.append(&at);
        let reach = reach(addr);
        let tag = text(reach, "procs-reach");
        tag.add_css_class(reach);
        row.append(&tag);
        row
    }

    /// A window as a card: its name, then its tree drawn with guides.
    fn window(&self, scope: Scope, w: &Window) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("procs-window");
        if !matches!(scope, Scope::Window(_)) {
            let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            head.add_css_class("procs-window-head");
            let name = text(&w.name, "procs-window-name");
            name.set_hexpand(true);
            head.append(&name);
            let n = w.procs.len();
            head.append(&text(
                &format!("{n} process{}", if n == 1 { "" } else { "es" }),
                "procs-pid",
            ));
            card.append(&head);
        }
        let depths: Vec<usize> = w.procs.iter().map(|(p, _)| p.depth).collect();
        for ((p, addrs), guide) in w.procs.iter().zip(guides(&depths)) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.add_css_class("procs-line");
            row.set_tooltip_text(Some(&format!("{}  (pid {})", p.cmd, p.pid)));
            if !guide.is_empty() {
                row.append(&text(&guide, "procs-guide"));
            }
            let name = text(&p.name, "procs-name");
            name.set_margin_end(10);
            row.append(&name);
            let args = text(&args(&p.cmd), "procs-cmd");
            args.set_hexpand(true);
            row.append(&args);
            for addr in distinct(addrs) {
                let chip = self.port(w.id, addr);
                chip.set_margin_start(6);
                row.append(&chip);
            }
            let pid = text(&p.pid.to_string(), "procs-pid");
            pid.set_width_chars(7);
            pid.set_xalign(1.0);
            row.append(&pid);
            card.append(&row);
        }
        card
    }

    /// A port as a chip that opens it.
    fn port(&self, id: AgentId, addr: &SocketAddr) -> gtk::Button {
        let target = url(addr);
        let button = gtk::Button::builder()
            .label(format!(":{}", addr.port()))
            .tooltip_text(format!("Open {target} (bound to {addr})"))
            .css_classes(["flat", "procs-port", "mono"])
            .valign(gtk::Align::Center)
            .build();
        let this = self.clone();
        button.connect_clicked(move |_| {
            this.close();
            if let Some(f) = this.on_open.borrow().as_ref() {
                f(id, target.clone());
            }
        });
        button
    }
}

/// Every server in `windows`, by port. Pre-forked workers inherit their
/// parent's socket, and a dual-stack bind is two sockets: one server each.
fn servers(windows: &[Window]) -> Vec<(&Window, &Proc, &SocketAddr)> {
    let mut out: Vec<(&Window, &Proc, &SocketAddr)> = windows
        .iter()
        .flat_map(|w| {
            w.procs
                .iter()
                .flat_map(move |(p, addrs)| distinct(addrs).into_iter().map(move |a| (w, p, a)))
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    out.retain(|(w, _, a)| seen.insert((w.id, a.port())));
    out.sort_by_key(|(_, _, a)| a.port());
    out
}

/// One address per port, the first one bound.
fn distinct(addrs: &[SocketAddr]) -> Vec<&SocketAddr> {
    ports(addrs)
        .into_iter()
        .filter_map(|port| addrs.iter().find(|a| a.port() == port))
        .collect()
}

/// Who can reach a server: this machine only, or the network too.
fn reach(addr: &SocketAddr) -> &'static str {
    if addr.ip().is_loopback() {
        "local"
    } else {
        "network"
    }
}

/// Anchor text: `:5173`, `:5173 +2`, or `3 procs` when nothing listens.
/// Hidden when there is nothing to open.
pub fn chip(label: &gtk::Label, ports: &[u16], procs: usize) {
    let text = match ports {
        [] => format!("{procs} proc{}", plural(procs)),
        [p] => format!(":{p}"),
        [p, rest @ ..] => format!(":{p} +{}", rest.len()),
    };
    label.set_text(&text);
    label.set_visible(!ports.is_empty() || procs > 0);
    if ports.is_empty() {
        label.remove_css_class("live");
    } else {
        label.add_css_class("live");
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The arguments after the program, with the home directory as `~`.
fn args(cmd: &str) -> String {
    let rest = cmd.split_once(' ').map_or("", |(_, rest)| rest);
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => rest.replace(&home, "~"),
        _ => rest.to_string(),
    }
}

/// Box-drawing guides for a depth-first list of depths: `├─ ` for a child
/// with a later sibling, `└─ ` for the last, and `│  ` under an ancestor
/// whose siblings are still to come. The root gets none.
fn guides(depths: &[usize]) -> Vec<String> {
    // ponytail: quadratic, fine for the few dozen processes of a pane.
    let more = |i: usize| {
        depths[i + 1..]
            .iter()
            .take_while(|&&d| d >= depths[i])
            .any(|&d| d == depths[i])
    };
    (0..depths.len())
        .map(|i| {
            let mut s = String::new();
            for level in 1..depths[i] {
                let ancestor = (0..i).rev().find(|&j| depths[j] == level);
                s.push_str(if ancestor.is_some_and(more) {
                    "│  "
                } else {
                    "   "
                });
            }
            if depths[i] > 0 {
                s.push_str(if more(i) { "├─ " } else { "└─ " });
            }
            s
        })
        .collect()
}

fn text(s: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(s)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes([class, "mono"])
        .build()
}

fn eyebrow(s: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(s)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["procs-eyebrow"])
        .build()
}

fn empty(s: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(s)
        .xalign(0.0)
        .css_classes(["perf-empty"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guides_draw_siblings_and_the_lines_of_unfinished_ancestors() {
        // bash
        // ├─ node
        // │  └─ esbuild
        // └─ sleep
        assert_eq!(guides(&[0, 1, 2, 1]), ["", "├─ ", "│  └─ ", "└─ "]);
        // A last child's subtree carries no line under it.
        assert_eq!(guides(&[0, 1, 2, 2]), ["", "└─ ", "   ├─ ", "   └─ "]);
    }
}
