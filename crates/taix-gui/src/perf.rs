//! What the session costs, per project, over the bar's memory segment.
//!
//! The bar can only ever say what *this* project and the server cost right
//! now. The question behind that number is usually "which of them is eating
//! the machine", and answering it meant reading `top` beside the app. So the
//! same reading that fills the bar is kept per project and shown here: one
//! row each, with CPU and memory as a number and a meter.
//!
//! Hover-only and deliberately read-only: the popover is not autohiding, so
//! it never takes the keyboard from a pane, and nothing in it is clickable
//! because reaching into it with the pointer is what closes it.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

/// Meter width. Wide enough to read a share off, narrow enough that this
/// stays a gauge beside a number rather than a chart.
const METER: i32 = 54;

/// One project's line.
///
/// Owns its strings: they are assembled per refresh from rows the caller is
/// already borrowing elsewhere, and a popover that repaints twice a second
/// is not the place to fight lifetimes for two allocations.
#[derive(PartialEq, Clone)]
pub struct Row {
    pub name: String,
    /// Windows, how many hold a pane, and what is running in them.
    pub detail: String,
    /// The project currently on screen, which gets the accent.
    pub selected: bool,
    /// PSS of this project's pane trees, in KiB.
    pub kib: u64,
    /// Percentage of one core, since the previous reading.
    pub cpu: f64,
}

/// The footer: what the session costs as a whole, and which tmux it is.
#[derive(PartialEq, Clone)]
pub struct Totals {
    pub ui_kib: u64,
    pub tmux_kib: u64,
    pub panes_kib: u64,
    pub cpu: f64,
    /// Panes on this server, which is not the same as windows TaiX knows.
    pub panes: usize,
    pub version: Option<String>,
    pub socket: String,
    pub uptime: std::time::Duration,
}

/// "12%", "0.4%", or a dash for a project that is doing nothing. Idle is the
/// common case, and a column of "0.0%" reads as broken rather than quiet.
pub fn cpu_text(percent: f64) -> String {
    if percent < 0.05 {
        "—".to_string()
    } else if percent < 10.0 {
        format!("{percent:.1}%")
    } else {
        format!("{percent:.0}%")
    }
}

/// A project's second line: how many windows, how many are actually running,
/// how many want you, and what is running in them.
pub fn detail(windows: usize, running: usize, attention: usize, kinds: &[&str]) -> String {
    if windows == 0 {
        return "no windows".to_string();
    }
    let mut parts = vec![match windows {
        1 => "1 window".to_string(),
        n => format!("{n} windows"),
    }];
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if attention > 0 {
        parts.push(format!("{attention} waiting"));
    }
    let harnesses = harness_summary(kinds);
    if !harnesses.is_empty() {
        parts.push(harnesses);
    }
    parts.join(" · ")
}

/// "Claude Code ×2 · shell": each harness once, in the order it first
/// appears, with a count only where there is something to count.
fn harness_summary(kinds: &[&str]) -> String {
    let mut counted: Vec<(&str, usize)> = Vec::new();
    for kind in kinds {
        match counted.iter_mut().find(|(name, _)| name == kind) {
            Some((_, n)) => *n += 1,
            None => counted.push((kind, 1)),
        }
    }
    counted
        .iter()
        .map(|(name, n)| match n {
            1 => (*name).to_string(),
            n => format!("{name} ×{n}"),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A part of a whole, for a meter: 0 when there is no whole to be part of.
pub fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    (part as f64 / whole as f64).clamp(0.0, 1.0)
}

/// The "fill me, I am about to be seen" hook, in the shape a widget can hold.
type Fill = Rc<RefCell<Option<Box<dyn Fn()>>>>;

/// Cloneable like every other member of `Widgets`: a clone is another handle
/// on the same popover, not a second monitor.
#[derive(Clone)]
pub struct Perf {
    popover: gtk::Popover,
    /// One child per project, rebuilt per refresh: a handful of rows on a
    /// 2 s cadence, and diffing them would be more code than it saves.
    list: gtk::Box,
    uptime: gtk::Label,
    cost: gtk::Label,
    facts: gtk::Label,
    on_show: Fill,
    /// Last rendered data, to skip rebuilds when nothing changed.
    last_data: std::cell::RefCell<Option<(Vec<Row>, Totals)>>,
}

impl Perf {
    /// Build the popover and hang it on `anchor` - the bar segment whose
    /// numbers it expands.
    pub fn new(anchor: &impl IsA<gtk::Widget>) -> Self {
        let title = gtk::Label::builder()
            .label("PERFORMANCE")
            .xalign(0.0)
            .css_classes(["perf-title"])
            .build();
        let uptime = gtk::Label::builder()
            .xalign(1.0)
            .hexpand(true)
            .css_classes(["perf-uptime", "mono"])
            .build();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.add_css_class("perf-head");
        head.append(&title);
        head.append(&uptime);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        list.add_css_class("perf-list");

        let cost = gtk::Label::builder()
            .xalign(0.0)
            .use_markup(true)
            .css_classes(["perf-cost", "mono"])
            .build();
        let facts = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["perf-facts", "mono"])
            .build();
        let foot = gtk::Box::new(gtk::Orientation::Vertical, 2);
        foot.add_css_class("perf-foot");
        foot.append(&cost);
        foot.append(&facts);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("perf");
        body.append(&head);
        body.append(&list);
        body.append(&foot);

        // Not autohiding: an autohide popover grabs the pointer and the
        // keyboard, and a monitor that steals your typing is a bug. It also
        // means nothing dismisses it but the pointer leaving, which is
        // exactly the intended gesture.
        let popover = gtk::Popover::builder()
            .child(&body)
            .autohide(false)
            .position(gtk::PositionType::Top)
            .css_classes(["perf-popover"])
            .build();
        popover.set_parent(anchor);

        let on_show: Fill = Rc::new(RefCell::new(None));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let popover = popover.clone();
            let on_show = on_show.clone();
            move |_, _, _| {
                // Fill it before it is on screen: the readings are already
                // taken, so this is string building, not measurement.
                if let Some(f) = on_show.borrow().as_ref() {
                    f();
                }
                popover.popup();
            }
        });
        motion.connect_leave({
            let popover = popover.clone();
            move |_| popover.popdown()
        });
        anchor.as_ref().add_controller(motion);

        Self {
            popover,
            list,
            uptime,
            cost,
            facts,
            on_show,
            last_data: std::cell::RefCell::new(None),
        }
    }

    /// Called as the popover opens, to fill it.
    pub fn connect_show(&self, f: impl Fn() + 'static) {
        *self.on_show.borrow_mut() = Some(Box::new(f));
    }

    /// Whether it is worth refreshing: while it is down, nobody is reading.
    pub fn is_open(&self) -> bool {
        self.popover.is_visible()
    }

    pub fn set(&self, rows: &[Row], totals: &Totals) {
        // O1: Skip rebuild when data is unchanged.
        if let Some((last_rows, last_totals)) = self.last_data.borrow().as_ref()
            && last_rows == rows
            && last_totals == totals
        {
            return;
        }

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        // Memory meters are shares of the panes' total, so the rows compare
        // with each other rather than each filling its own bar.
        for row in rows {
            self.list.append(&project_row(row, totals.panes_kib));
        }
        if rows.is_empty() {
            self.list.append(
                &gtk::Label::builder()
                    .label("no projects yet")
                    .xalign(0.0)
                    .css_classes(["perf-empty"])
                    .build(),
            );
        }
        let human = taix_core::mem::human_kib;
        let total = totals.ui_kib + totals.tmux_kib + totals.panes_kib;
        self.cost.set_markup(&format!(
            "ui {} · tmux {} · panes {} · <b>total {}</b>",
            human(totals.ui_kib),
            human(totals.tmux_kib),
            human(totals.panes_kib),
            human(total),
        ));
        self.facts.set_text(&format!(
            "tmux {} · socket {} · {} pane{} · {} cpu",
            totals.version.as_deref().unwrap_or("?"),
            totals.socket,
            totals.panes,
            if totals.panes == 1 { "" } else { "s" },
            cpu_text(totals.cpu),
        ));
        self.uptime.set_text(&format!(
            "up {}",
            taix_core::text::human_secs(totals.uptime.as_secs())
        ));

        // Cache the rendered data.
        *self.last_data.borrow_mut() = Some((rows.to_vec(), totals.clone()));
    }
}

/// Name and detail on the left, then a number over a meter per metric - so
/// the meter under a figure is unambiguously that figure's.
fn project_row(row: &Row, panes_kib: u64) -> gtk::Box {
    let name = gtk::Label::builder()
        .label(&row.name)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["perf-name"])
        .build();
    let detail = gtk::Label::builder()
        .label(&row.detail)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        // Without this a long harness list sets the popover's width.
        .max_width_chars(44)
        .css_classes(["perf-detail"])
        .build();
    let left = gtk::Box::new(gtk::Orientation::Vertical, 1);
    left.set_hexpand(true);
    left.set_valign(gtk::Align::Center);
    left.append(&name);
    left.append(&detail);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    root.add_css_class("perf-row");
    if row.selected {
        root.add_css_class("perf-row-on");
    }
    root.append(&left);
    // One core is a full bar: a build that uses four reads as 100% here, and
    // scaling by core count would make every honest single-threaded agent
    // invisible.
    root.append(&gauge(&cpu_text(row.cpu), row.cpu / 100.0, "perf-cpu"));
    root.append(&gauge(
        &taix_core::mem::human_kib(row.kib),
        share(row.kib, panes_kib),
        "perf-mem",
    ));
    root
}

fn gauge(text: &str, fraction: f64, class: &str) -> gtk::Box {
    let value = gtk::Label::builder()
        .label(text)
        .xalign(1.0)
        .css_classes([class, "mono", "perf-value"])
        .build();
    let meter = gtk::ProgressBar::builder()
        .fraction(fraction.clamp(0.0, 1.0))
        .css_classes(["perf-meter", class])
        .build();
    let block = gtk::Box::new(gtk::Orientation::Vertical, 4);
    block.set_width_request(METER);
    block.set_valign(gtk::Align::Center);
    block.append(&value);
    block.append(&meter);
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_reads_as_a_dash_and_busy_loses_the_decimal() {
        assert_eq!(cpu_text(0.0), "—");
        assert_eq!(cpu_text(0.04), "—");
        assert_eq!(cpu_text(0.4), "0.4%");
        assert_eq!(cpu_text(9.94), "9.9%");
        assert_eq!(cpu_text(12.4), "12%");
        // More than a core is real, and reported rather than clamped.
        assert_eq!(cpu_text(310.0), "310%");
    }

    #[test]
    fn a_detail_line_names_only_what_is_there() {
        assert_eq!(detail(0, 0, 0, &[]), "no windows");
        assert_eq!(detail(1, 0, 0, &["shell"]), "1 window · shell");
        assert_eq!(
            detail(3, 2, 1, &["Claude Code", "Claude Code", "shell"]),
            "3 windows · 2 running · 1 waiting · Claude Code ×2 · shell"
        );
    }

    #[test]
    fn harnesses_are_counted_in_the_order_they_appear() {
        assert_eq!(harness_summary(&[]), "");
        assert_eq!(
            harness_summary(&["shell", "Codex", "shell", "shell"]),
            "shell ×3 · Codex"
        );
    }

    #[test]
    fn a_share_of_nothing_is_zero_and_a_share_never_overflows() {
        assert_eq!(share(0, 0), 0.0);
        assert_eq!(share(500, 0), 0.0);
        assert_eq!(share(250, 1000), 0.25);
        assert_eq!(share(2000, 1000), 1.0);
    }
}
