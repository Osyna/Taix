//! The web front end from the desktop's side: the server's life, who holds
//! the keyboard, and the chip in the bar that says so.
//!
//! The server itself lives in `taix-web` and knows nothing about GTK. This
//! module is the seam: it starts and stops it, keeps the one piece of state
//! the two fronts share - the input lock - and paints the indicator.

use std::cell::RefCell;
use std::rc::Rc;

use adw::glib;
use adw::prelude::*;
use gtk::gio;

use taix_core::Config;
use taix_web::proto::{Health, Life};
use taix_web::{Hub, Options, Server};

/// What the settings dialog can ask for, beyond the switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Start,
    Stop,
    Restart,
}

/// How the settings dialog reaches a running server.
///
/// Two closures rather than the `App` itself: a dialog callback fires while
/// the caller may still hold the `RefCell`, and these are the only
/// things it needs.
#[derive(Clone)]
pub struct Bridge {
    pub health: Rc<dyn Fn() -> Health>,
    pub act: Rc<dyn Fn(Act)>,
    pub devices: Rc<dyn Fn() -> Vec<taix_web::proto::DeviceView>>,
    pub forget: Rc<dyn Fn(&str)>,
    pub rotate: Rc<dyn Fn() -> String>,
}

pub struct Web {
    server: Option<Server>,
    /// The port the running server is on, which is not `cfg.web.port` while
    /// the user is mid-edit in settings: that is how `reconcile` knows a
    /// restart is due.
    port: u16,
    /// The address it is bound to, `None` for every interface. Set from
    /// `cfg.web.tailscale`, and the other half of what tells `reconcile` a
    /// restart is due.
    bind: Option<std::net::Ipv4Addr>,
    /// Windows a browser is driving. Per window, not per application: the
    /// desktop keeps typing in its own pane while a laptop on the LAN
    /// drives another one, in another project.
    held: Vec<taix_core::AgentId>,
    /// Set when a desktop keystroke was swallowed, so the bar can say why
    /// once rather than on every key.
    pub scolded: bool,
    /// What the bar was last painted from, so a frame that changed nothing
    /// does not touch a widget. The last field is the addresses waiting to
    /// be let in.
    pub painted: Option<(Life, usize, usize, String)>,
}

impl Web {
    /// Start the server if the config says so. Called once at startup and
    /// again whenever settings are saved.
    pub fn new(cfg: &Config) -> Web {
        let mut web = Web {
            server: None,
            port: cfg.web.port,
            bind: None,
            held: Vec::new(),
            scolded: false,
            painted: None,
        };
        if cfg.web.enabled {
            web.start(cfg);
        }
        web
    }

    fn start(&mut self, cfg: &Config) {
        let key = taix_core::web_key().unwrap_or_else(|_| taix_core::random_key());
        let bind = if cfg.web.tailscale {
            taix_core::tailscale::status().and_then(|t| t.ip)
        } else {
            None
        };
        let server = Server::start(Options {
            port: cfg.web.port,
            key,
            icons: crate::icons::BUNDLED,
            bind,
        });
        server.hub().set_waker(Box::new(|| {
            glib::MainContext::default()
                .invoke_with_priority(glib::Priority::DEFAULT, crate::app::wake);
        }));
        self.port = cfg.web.port;
        self.bind = bind;
        self.server = Some(server);
    }

    /// Bring the server in line with the config: started, stopped, or
    /// rebound after a port change.
    pub fn reconcile(&mut self, cfg: &Config) {
        let bind = if cfg.web.tailscale {
            taix_core::tailscale::status().and_then(|t| t.ip)
        } else {
            None
        };
        match (cfg.web.enabled, self.server.is_some()) {
            (true, false) => self.start(cfg),
            (true, true) if cfg.web.port != self.port || bind != self.bind => {
                self.server = None;
                self.start(cfg);
            }
            (false, true) => self.server = None,
            _ => {}
        }
    }

    pub fn act(&mut self, act: Act, cfg: &Config) {
        match act {
            Act::Start => {
                if self.server.is_none() {
                    self.start(cfg);
                }
            }
            Act::Stop => {
                self.server = None;
                self.held.clear();
            }
            Act::Restart => {
                self.server = None;
                self.start(cfg);
            }
        }
    }

    pub fn hub(&self) -> Option<&Hub> {
        self.server.as_ref().map(|s| s.hub().as_ref())
    }

    pub fn health(&self) -> Health {
        match &self.server {
            Some(server) => server.hub().health(),
            None => Health::off(self.port),
        }
    }

    pub fn clients(&self) -> usize {
        self.hub().map_or(0, Hub::clients)
    }

    /// Whether a browser is driving this terminal, so the desktop's own
    /// keys into it are on hold.
    pub fn holds(&self, id: taix_core::AgentId) -> bool {
        self.server.is_some() && self.held.contains(&id)
    }

    pub fn remote(&self) -> Vec<taix_core::AgentId> {
        if self.server.is_some() {
            self.held.clone()
        } else {
            Vec::new()
        }
    }

    /// A browser claimed one terminal. Pressing into a pane is the whole
    /// gesture on both fronts, so this is called from the click path, not
    /// from a menu item.
    pub fn claim(&mut self, id: taix_core::AgentId) -> bool {
        if self.held.contains(&id) {
            return false;
        }
        self.held.push(id);
        self.scolded = false;
        true
    }

    /// The desktop took one back, by clicking it.
    pub fn reclaim(&mut self, id: taix_core::AgentId) -> bool {
        let before = self.held.len();
        self.held.retain(|held| *held != id);
        self.scolded = false;
        self.held.len() != before
    }

    /// A page closed: everything it was driving comes back.
    pub fn reclaim_all(&mut self) -> bool {
        let held = !self.held.is_empty();
        self.held.clear();
        self.scolded = false;
        held
    }

    /// Windows any browser is showing, which the desktop may not be: their
    /// screens still have to be captured or the page draws nothing.
    pub fn watched(&self) -> Vec<taix_core::AgentId> {
        self.hub().map(Hub::watched).unwrap_or_default()
    }

    pub fn pending(&self) -> Vec<taix_web::proto::PairRequest> {
        self.hub().map(Hub::pending).unwrap_or_default()
    }

    pub fn approve(&self, ip: &str) {
        if let Some(hub) = self.hub() {
            hub.approve(ip);
        }
    }

    pub fn deny(&self, ip: &str) {
        if let Some(hub) = self.hub() {
            hub.deny(ip);
        }
    }

    pub fn paired(&self) -> Vec<taix_web::proto::DeviceView> {
        self.hub().map(Hub::paired).unwrap_or_default()
    }

    pub fn forget(&self, ip: &str) {
        if let Some(hub) = self.hub() {
            hub.forget(ip);
        }
    }

    pub fn rotate_key(&self) -> String {
        let key = taix_core::random_key();
        let _ = taix_core::set_web_key(&key);
        if let Some(hub) = self.hub() {
            hub.rotate_key(&key);
        }
        key
    }
}

/// The bar's health chip: a globe, a coloured dot, `ip:port`.
///
/// Clicking it opens the page in the desktop's browser, which is the
/// shortest path from "it is running" to "show me".
#[derive(Clone)]
pub struct Chip {
    pub root: gtk::Button,
    /// The dot is painted entirely by the chip's state class, so only the
    /// text and the URL are ever touched from here.
    text: gtk::Label,
    url: Rc<RefCell<String>>,
}

impl Chip {
    pub fn new() -> Chip {
        let text = gtk::Label::builder().css_classes(["mono"]).build();
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        body.append(&crate::icons::image(Some("globe"), 12));
        body.append(
            &gtk::Label::builder()
                .label("●")
                .css_classes(["web-dot"])
                .build(),
        );
        body.append(&text);
        let root = gtk::Button::builder()
            .child(&body)
            .css_classes(["flat", "web-chip"])
            .cursor(&gtk::gdk::Cursor::from_name("pointer", None).unwrap())
            .build();
        let url = Rc::new(RefCell::new(String::new()));
        root.connect_clicked({
            let url = url.clone();
            move |_| {
                let url = url.borrow().clone();
                if !url.is_empty() {
                    let _ = gio::AppInfo::launch_default_for_uri(&url, gio::AppLaunchContext::NONE);
                }
            }
        });
        Chip { root, text, url }
    }

    /// Repaint from the server's own health. `driving` is how many
    /// terminals a browser is typing into: that is the one thing about the
    /// web front end that changes what this screen does, so the chip is
    /// where it is said.
    pub fn set(&self, health: &Health, clients: usize, driving: usize) {
        let (class, label, tip) = match health.life {
            Life::Off => (
                "off",
                "web off".to_string(),
                "The web front end is disabled - turn it on in Settings".to_string(),
            ),
            Life::Starting => (
                "starting",
                format!("binding :{}", health.port),
                "Binding the port".to_string(),
            ),
            Life::Live => (
                "live",
                health.addr.clone(),
                match clients {
                    0 => format!("Serving http://{} - nobody watching yet", health.addr),
                    1 => format!("Serving http://{} - 1 device watching", health.addr),
                    n => format!("Serving http://{} - {n} devices watching", health.addr),
                },
            ),
            Life::Error => (
                "error",
                format!("web :{} failed", health.port),
                health.error.clone(),
            ),
        };
        for state in ["off", "starting", "live", "error"] {
            if state == class {
                self.root.add_css_class(state);
            } else {
                self.root.remove_css_class(state);
            }
        }
        // A client watching is worth seeing from across the room; a client
        // *typing* is worth saying in words.
        let held = driving > 0;
        if held {
            self.root.add_css_class("held");
        } else {
            self.root.remove_css_class("held");
        }
        if clients > 0 && health.life == Life::Live {
            self.root.add_css_class("busy");
        } else {
            self.root.remove_css_class("busy");
        }
        let typing = match driving {
            0 => String::new(),
            1 => "web typing".to_string(),
            n => format!("web typing ×{n}"),
        };
        self.text.set_text(if held { &typing } else { &label });
        self.root.set_tooltip_text(Some(&if held {
            match driving {
                1 => "A browser is driving one terminal - click it to take it back".to_string(),
                n => format!("A browser is driving {n} terminals - click one to take it back"),
            }
        } else {
            tip
        }));
        self.root.set_sensitive(health.life == Life::Live);
        *self.url.borrow_mut() = health.url.clone();
    }
}

/// What the bar does with an answer. Parked like every other pointer
/// intent: the button fires while `App` may still be borrowed.
type Decide = Rc<dyn Fn(String, bool)>;

/// The bar's pairing control: hidden until a device on the network asks to
/// be let in, then one popover row per asking device.
#[derive(Clone)]
pub struct Pair {
    pub root: gtk::MenuButton,
    label: gtk::Label,
    list: gtk::Box,
    /// The addresses the rows were built for, so a repaint does not rebuild
    /// the popover under the pointer.
    ips: Rc<RefCell<Vec<String>>>,
    on_decision: Rc<RefCell<Option<Decide>>>,
}

impl Pair {
    pub fn new() -> Pair {
        let label = gtk::Label::builder().css_classes(["mono"]).build();
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        body.append(&crate::icons::image(Some("globe"), 11));
        body.append(&label);
        let root = gtk::MenuButton::builder()
            .child(&body)
            .css_classes(["flat", "pair-chip"])
            .cursor(&gtk::gdk::Cursor::from_name("pointer", None).unwrap())
            .visible(false)
            .build();
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .css_classes(["pair-list"])
            .build();
        let popover = gtk::Popover::builder()
            .child(&list)
            .css_classes(["pair-pop"])
            .has_arrow(true)
            .build();
        root.set_popover(Some(&popover));
        Pair {
            root,
            label,
            list,
            ips: Rc::new(RefCell::new(Vec::new())),
            on_decision: Rc::new(RefCell::new(None)),
        }
    }

    pub fn set_callback(&self, cb: impl Fn(String, bool) + 'static) {
        *self.on_decision.borrow_mut() = Some(Rc::new(cb));
    }

    /// Repaint from who is waiting. Rows are rebuilt only when the set of
    /// addresses changes, so a popover cannot be rebuilt under the pointer
    /// mid-click. No age is shown: the server drops a request two minutes
    /// after it stops asking, so everything here is current by definition.
    pub fn set(&self, waiting: &[taix_web::proto::PairRequest]) {
        self.root.set_visible(!waiting.is_empty());
        if waiting.is_empty() {
            return;
        }
        // One device is named in the bar: the address is the whole of the
        // decision, and reading it should not cost a click.
        self.label.set_text(&match waiting {
            [only] => format!("{} wants in", only.ip),
            many => format!("{} devices want in", many.len()),
        });
        let ips: Vec<String> = waiting.iter().map(|r| r.ip.clone()).collect();
        if ips == *self.ips.borrow() {
            return;
        }
        *self.ips.borrow_mut() = ips;
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.list.append(
            &gtk::Label::builder()
                .label("Let this device in?")
                .xalign(0.0)
                .css_classes(["pair-head"])
                .build(),
        );
        for (i, req) in waiting.iter().enumerate() {
            if i > 0 {
                self.list
                    .append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            self.list.append(&self.row(req));
        }
        self.list.append(
            &gtk::Label::builder()
                .label("Pairing lasts until this device clears its cookie.")
                .xalign(0.0)
                .wrap(true)
                .max_width_chars(34)
                .css_classes(["pair-note"])
                .build(),
        );
    }

    fn row(&self, req: &taix_web::proto::PairRequest) -> gtk::Box {
        let who = gtk::Box::new(gtk::Orientation::Vertical, 1);
        who.set_hexpand(true);
        who.append(
            &gtk::Label::builder()
                .label(&req.agent)
                .xalign(0.0)
                .css_classes(["pair-dev"])
                .build(),
        );
        who.append(
            &gtk::Label::builder()
                .label(&req.ip)
                .xalign(0.0)
                .css_classes(["pair-ip", "mono"])
                .build(),
        );

        let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tile.add_css_class("pair-tile");
        tile.append(&crate::icons::image(Some("globe"), 15));
        head.append(&tile);
        head.append(&who);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        for (text, allow, class) in [("Deny", false, "pair-no"), ("Pair", true, "pair-yes")] {
            let button = gtk::Button::builder()
                .label(text)
                .css_classes(["flat", class])
                .cursor(&gtk::gdk::Cursor::from_name("pointer", None).unwrap())
                .build();
            button.connect_clicked({
                let ip = req.ip.clone();
                let decided = self.on_decision.clone();
                let popover = self.root.popover();
                move |_| {
                    if let Some(cb) = decided.borrow().as_ref() {
                        cb(ip.clone(), allow);
                    }
                    if let Some(popover) = &popover {
                        popover.popdown();
                    }
                }
            });
            buttons.append(&button);
        }

        let row = gtk::Box::new(gtk::Orientation::Vertical, 10);
        row.add_css_class("pair-row");
        row.append(&head);
        row.append(&buttons);
        row
    }
}
