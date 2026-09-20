//! The parts every dialog is made of: the shell, and the controls inside it.
//!
//! TaiX draws its own settings surfaces rather than using
//! `AdwPreferencesPage`, so that `theme.rs` paints them like the rest of the
//! app. That decision costs a widget kit, and this is it: one sidebar shell
//! and the handful of controls the pages fill themselves with.
//!
//! Nothing here knows what it is editing. A control takes its current value
//! and a closure to call with the next one; who saves, and where, is the
//! caller's business.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

/// The furniture of a settings-shaped dialog, handed back so the caller can
/// fill the stack and drive the nav.
pub(crate) struct Shell {
    pub dialog: adw::Dialog,
    pub toasts: adw::ToastOverlay,
    /// The page title and its line of explanation, rewritten per page.
    pub h1: gtk::Label,
    pub h2: gtk::Label,
    pub nav: gtk::ListBox,
    pub stack: gtk::Stack,
    /// The count label beside a nav row, by nav id, for the rows that asked
    /// for one.
    pub counts: Vec<(&'static str, gtk::Label)>,
    /// Where a page may put a control of its own, left of the close button.
    pub head_slot: gtk::Box,
}

impl Shell {
    pub fn count(&self, id: &str) -> Option<&gtk::Label> {
        self.counts.iter().find(|(k, _)| *k == id).map(|(_, l)| l)
    }
}

/// Build the shell. `nav` is `(id, label, wants a count)` in sidebar order;
/// `brand` and `where_line` fill the sidebar head, `foot` its footer.
pub(crate) fn shell(
    title: &str,
    brand: &str,
    where_line: &str,
    where_tip: &str,
    foot_text: &str,
    nav_items: &[(&'static str, &str, bool)],
) -> Shell {
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(920)
        .content_height(640)
        .build();
    let toasts = adw::ToastOverlay::new();

    let h1 = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["set-h1"])
        .build();
    let h2 = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["set-h2"])
        .build();

    // Sidebar -------------------------------------------------------------
    let side = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(196)
        .css_classes(["set-side"])
        .build();
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .css_classes(["set-side-top"])
        .build();
    top.append(
        &gtk::Label::builder()
            .label(brand)
            .xalign(0.0)
            .css_classes(["set-brand"])
            .build(),
    );
    let where_label = gtk::Label::builder()
        .label(where_line)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["set-where", "mono"])
        .build();
    if !where_tip.is_empty() {
        where_label.set_tooltip_text(Some(where_tip));
    }
    top.append(&where_label);
    side.append(&top);

    let nav = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["set-nav"])
        .build();
    let mut counts = Vec::new();
    for (id, label, wants_count) in nav_items {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        line.append(
            &gtk::Box::builder()
                .css_classes(["set-dot"])
                .valign(gtk::Align::Center)
                .build(),
        );
        line.append(&gtk::Label::builder().label(*label).xalign(0.0).build());
        if *wants_count {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_hexpand(true);
            line.append(&spacer);
            let count = gtk::Label::builder().css_classes(["count"]).build();
            line.append(&count);
            counts.push((*id, count));
        }
        nav.append(&gtk::ListBoxRow::builder().child(&line).build());
    }
    side.append(&nav);
    let fill = gtk::Box::new(gtk::Orientation::Vertical, 0);
    fill.set_vexpand(true);
    side.append(&fill);
    let foot_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(7)
        .css_classes(["set-foot"])
        .build();
    foot_box.append(
        &gtk::Box::builder()
            .css_classes(["set-dot"])
            .valign(gtk::Align::Center)
            .build(),
    );
    foot_box.append(&gtk::Label::new(Some(foot_text)));
    side.append(&foot_box);

    // Head ----------------------------------------------------------------
    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .css_classes(["set-head"])
        .build();
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    titles.set_hexpand(true);
    titles.set_valign(gtk::Align::Center);
    titles.append(&h1);
    titles.append(&h2);
    head.append(&titles);
    let head_slot = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .build();
    head.append(&head_slot);
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close (Esc)")
        .valign(gtk::Align::Center)
        .css_classes(["set-close"])
        .build();
    close.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    head.append(&close);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(120)
        .build();
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    body.append(&head);
    body.append(&stack);

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["set-shell"])
        .build();
    root.append(&side);
    root.append(&body);
    toasts.set_child(Some(&root));
    dialog.set_child(Some(&toasts));

    Shell {
        dialog,
        toasts,
        h1,
        h2,
        nav,
        stack,
        counts,
        head_slot,
    }
}

/// `/home/you/.config/taix/config.toml` as `~/.config/taix/config.toml`.
pub(crate) fn short_path(path: &str) -> String {
    shorten(path, std::env::var("HOME").ok().as_deref())
}

fn shorten(path: &str, home: Option<&str>) -> String {
    match home.filter(|h| !h.is_empty()) {
        Some(home) => path
            .strip_prefix(home)
            .map_or_else(|| path.to_string(), |rest| format!("~{rest}")),
        None => path.to_string(),
    }
}

/// A page that scrolls: the scroller, and the box inside it that group
/// positions are measured against - not the viewport, which moves.
pub(crate) fn page() -> (gtk::ScrolledWindow, gtk::Box) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(20)
        .css_classes(["set-page"])
        .build();
    // Without `vexpand` the scroller takes its minimum height and clips the
    // page to a sliver: the stack it sits in hands out no extra space.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&page)
        .build();
    (scroller, page)
}

// Controls -----------------------------------------------------------------

/// A titled block of rows. `count` is the number beside the title, `suffix`
/// the control at the far end of the header.
pub(crate) fn group(title: &str, count: Option<usize>, suffix: Option<&gtk::Widget>) -> gtk::Box {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(["set-group"])
        .build();
    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["set-group-head"])
        .build();
    head.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["set-group-title"])
            .build(),
    );
    if let Some(n) = count {
        head.append(
            &gtk::Label::builder()
                .label(n.to_string())
                .css_classes(["set-group-count"])
                .build(),
        );
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    head.append(&spacer);
    if let Some(w) = suffix {
        head.append(w);
    }
    root.append(&head);
    root
}

/// Title over subtitle on the left, one control on the right.
pub(crate) fn row(title: &str, subtitle: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let label = gtk::Label::builder()
        .label(subtitle)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["set-row-sub"])
        .build();
    row_live(title, (!subtitle.is_empty()).then_some(&label), control)
}

/// The same row with a subtitle the caller keeps a handle on, for the ones
/// that read back what was just typed - a schedule's next three runs.
pub(crate) fn row_live(
    title: &str,
    subtitle: Option<&gtk::Label>,
    control: &impl IsA<gtk::Widget>,
) -> gtk::Box {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(14)
        .css_classes(["set-row"])
        .build();
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    titles.set_hexpand(true);
    titles.set_valign(gtk::Align::Center);
    titles.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["set-row-title"])
            .build(),
    );
    if let Some(sub) = subtitle {
        titles.append(sub);
    }
    root.append(&titles);
    root.append(control);
    root
}

/// One number between a minus and a plus, with its unit after it. Typing in
/// it works too: a 2000 ms timeout is not worth twenty clicks.
pub(crate) fn stepper(
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    digits: usize,
    unit: &str,
    on: impl Fn(f64) + 'static,
) -> gtk::Box {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .valign(gtk::Align::Center)
        .css_classes(["set-stepper"])
        .build();
    let minus = gtk::Button::builder()
        .label("\u{2212}")
        .css_classes(["set-step"])
        .build();
    let entry = gtk::Entry::builder()
        .text(number(value, digits))
        .width_chars(5)
        .max_width_chars(5)
        .xalign(0.5)
        .build();
    let plus = gtk::Button::builder()
        .label("+")
        .css_classes(["set-step"])
        .build();
    root.append(&minus);
    root.append(&entry);
    root.append(&plus);
    if !unit.is_empty() {
        root.append(
            &gtk::Label::builder()
                .label(unit)
                .css_classes(["set-unit"])
                .build(),
        );
    }

    let at = Rc::new(Cell::new(value));
    let apply: Rc<dyn Fn(f64)> = Rc::new({
        let (entry, at) = (entry.clone(), at.clone());
        move |v: f64| {
            let v = v.clamp(min, max);
            entry.set_text(&number(v, digits));
            if (v - at.get()).abs() > f64::EPSILON {
                at.set(v);
                on(v);
            }
        }
    });
    minus.connect_clicked({
        let (apply, at) = (apply.clone(), at.clone());
        move |_| apply(at.get() - step)
    });
    plus.connect_clicked({
        let (apply, at) = (apply.clone(), at.clone());
        move |_| apply(at.get() + step)
    });
    entry.connect_activate({
        let (apply, at) = (apply.clone(), at.clone());
        move |e| apply(e.text().trim().parse().unwrap_or_else(|_| at.get()))
    });
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave({
        let (apply, at, entry) = (apply.clone(), at.clone(), entry.clone());
        move |_| apply(entry.text().trim().parse().unwrap_or_else(|_| at.get()))
    });
    entry.add_controller(focus);
    root
}

fn number(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

/// A short text field that applies on Enter or when it loses focus, never on
/// a keystroke: a half-typed command must not be launched.
pub(crate) fn text(
    value: &str,
    width: i32,
    tooltip: &str,
    apply: impl Fn(String) + 'static,
) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .text(value)
        .width_chars(width)
        .max_width_chars(width)
        .valign(gtk::Align::Center)
        .css_classes(["set-entry"])
        .build();
    if !tooltip.is_empty() {
        entry.set_tooltip_text(Some(tooltip));
    }
    let last = Rc::new(RefCell::new(value.to_string()));
    let commit: Rc<dyn Fn(&gtk::Entry)> = Rc::new(move |e: &gtk::Entry| {
        let now = e.text().to_string();
        if now == *last.borrow() {
            return;
        }
        *last.borrow_mut() = now.clone();
        apply(now);
    });
    entry.connect_activate({
        let commit = commit.clone();
        move |e| commit(e)
    });
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave({
        let (commit, entry) = (commit.clone(), entry.clone());
        move |_| commit(&entry)
    });
    entry.add_controller(focus);
    entry
}

/// A switch, aligned like every other control on a row.
pub(crate) fn toggle(on: bool, apply: impl Fn(bool) + 'static) -> gtk::Switch {
    let sw = gtk::Switch::builder()
        .active(on)
        .valign(gtk::Align::Center)
        .build();
    sw.connect_state_set(move |_, state| {
        apply(state);
        glib::Propagation::Proceed
    });
    sw
}

/// A list of ids as removable pills, plus a menu of the ones not used yet.
pub(crate) fn chips(
    values: Vec<String>,
    pool: Vec<String>,
    on: impl Fn(Vec<String>) + 'static,
) -> gtk::Box {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(5)
        .valign(gtk::Align::Center)
        .css_classes(["set-chips"])
        .build();
    let state = Rc::new(RefCell::new(values));
    let pool = Rc::new(pool);
    let on: Rc<dyn Fn(Vec<String>)> = Rc::new(on);
    fill_chips(&root, &state, &pool, &on);
    root
}

fn fill_chips(
    root: &gtk::Box,
    state: &Rc<RefCell<Vec<String>>>,
    pool: &Rc<Vec<String>>,
    on: &Rc<dyn Fn(Vec<String>)>,
) {
    while let Some(child) = root.first_child() {
        root.remove(&child);
    }
    let current = state.borrow().clone();
    for (i, name) in current.iter().enumerate() {
        let chip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .valign(gtk::Align::Center)
            .css_classes(["set-chip"])
            .build();
        chip.append(
            &gtk::Label::builder()
                .label(name)
                .css_classes(["set-chip-label", "mono"])
                .build(),
        );
        let drop = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Remove")
            .css_classes(["set-chip-x"])
            .build();
        drop.connect_clicked({
            let (root, state, pool, on) = (root.clone(), state.clone(), pool.clone(), on.clone());
            move |_| {
                state.borrow_mut().remove(i);
                on(state.borrow().clone());
                fill_chips(&root, &state, &pool, &on);
            }
        });
        chip.append(&drop);
        root.append(&chip);
    }

    let rest: Vec<String> = pool
        .iter()
        .filter(|s| !current.iter().any(|v| v == *s))
        .cloned()
        .collect();
    let add = gtk::MenuButton::builder()
        // A child rather than `label`: a MenuButton with a label draws an
        // arrow beside it, and the chip is already shaped like a menu.
        .child(&gtk::Label::new(Some("+ add")))
        .valign(gtk::Align::Center)
        .sensitive(!rest.is_empty())
        .css_classes(["set-chip-add"])
        .build();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let popover = gtk::Popover::new();
    for name in rest {
        let item = gtk::Button::builder()
            .label(&name)
            .css_classes(["set-pick-item"])
            .build();
        item.connect_clicked({
            let (root, state, pool, on, popover) = (
                root.clone(),
                state.clone(),
                pool.clone(),
                on.clone(),
                popover.clone(),
            );
            move |_| {
                popover.popdown();
                state.borrow_mut().push(name.clone());
                on(state.borrow().clone());
                fill_chips(&root, &state, &pool, &on);
            }
        });
        menu.append(&item);
    }
    popover.set_child(Some(&menu));
    add.set_popover(Some(&popover));
    root.append(&add);
}

/// A combo the app draws itself: a face, a popover of rows, and an optional
/// widget in front of each - the palette rows carry their own colours.
pub(crate) fn picker(
    labels: &[String],
    selected: usize,
    prefix: impl Fn(usize) -> Option<gtk::Widget> + 'static,
    on: impl Fn(usize) + 'static,
) -> gtk::MenuButton {
    let labels: Rc<Vec<String>> = Rc::new(labels.to_vec());
    let prefix = Rc::new(prefix);
    let face_prefix = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let face_label = gtk::Label::builder()
        .css_classes(["set-pick-label"])
        .build();
    let face = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    face.append(&face_prefix);
    face.append(&face_label);
    face.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
    let button = gtk::MenuButton::builder()
        .child(&face)
        .valign(gtk::Align::Center)
        .css_classes(["set-pick"])
        .build();

    let show: Rc<dyn Fn(usize)> = Rc::new({
        let (face_prefix, face_label, labels, prefix) = (
            face_prefix.clone(),
            face_label.clone(),
            labels.clone(),
            prefix.clone(),
        );
        move |i: usize| {
            while let Some(child) = face_prefix.first_child() {
                face_prefix.remove(&child);
            }
            if let Some(w) = prefix(i) {
                face_prefix.append(&w);
            }
            face_label.set_label(labels.get(i).map_or("", String::as_str));
        }
    });
    show(selected.min(labels.len().saturating_sub(1)));

    let on = Rc::new(on);
    let popover = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    for (i, label) in labels.iter().enumerate() {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        if let Some(w) = prefix(i) {
            item.append(&w);
        }
        item.append(&gtk::Label::new(Some(label)));
        let button = gtk::Button::builder()
            .child(&item)
            .css_classes(["set-pick-item"])
            .build();
        button.connect_clicked({
            let (show, on, popover) = (show.clone(), on.clone(), popover.clone());
            move |_| {
                popover.popdown();
                show(i);
                on(i);
            }
        });
        list.append(&button);
    }
    popover.set_child(Some(&list));
    button.set_popover(Some(&popover));
    button
}

/// A card of one number: how many agents are ready, how many jobs are on.
pub(crate) fn stat(n: impl std::fmt::Display, label: &str) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .css_classes(["set-stat"])
        .build();
    card.append(
        &gtk::Label::builder()
            .label(n.to_string())
            .xalign(0.0)
            .css_classes(["set-stat-n"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .css_classes(["set-stat-l"])
            .build(),
    );
    card
}

/// A row of stat cards.
pub(crate) fn stats(cards: &[(String, &str)]) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .homogeneous(true)
        .css_classes(["set-stats"])
        .build();
    for (n, label) in cards {
        row.append(&stat(n, label));
    }
    row
}

/// The pill that says what state something is in: a badge with no extra
/// class is the neutral one.
pub(crate) fn badge(text: &str, class: Option<&str>) -> gtk::Label {
    let mut classes = vec!["badge"];
    if let Some(c) = class {
        classes.push(c);
    }
    gtk::Label::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .css_classes(classes)
        .build()
}
