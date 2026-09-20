//! Widget construction and dialogs. No application logic lives here.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use taix_core::{Agent, Config, Project, ProjectId};

/// Handles the application needs to reach after construction.
#[derive(Clone)]
pub struct Widgets {
    pub window: adw::ApplicationWindow,
    /// Holder the pane tree is rebuilt into.
    pub panes: gtk::Box,
    /// sidebar | main divider.
    pub outer: gtk::Paned,
    /// panes | browser divider. Only the browser panel repositions it.
    #[cfg(feature = "browser")]
    pub main: gtk::Paned,
    #[cfg(feature = "browser")]
    pub browser: gtk::ToggleButton,
    /// (panes, browser) | files divider. The panel is attached on demand,
    /// so a `Paned` with no end child is what a closed tree looks like.
    pub files_split: gtk::Paned,
    pub files_toggle: gtk::Button,
    pub files: crate::files::Files,
    /// The side column itself: tabs over a stack of the two panels. The
    /// app attaches this to `files_split` when the column is open.
    pub panel_body: gtk::Box,
    pub panel_stack: gtk::Stack,
    pub files_tab: gtk::ToggleButton,
    pub git_tab: gtk::ToggleButton,
    pub git_toggle: gtk::Button,
    pub git: std::rc::Rc<crate::git::Git>,
    /// Holder the project cards are rebuilt into.
    pub sidebar_list: gtk::Box,
    /// Narrows the sidebar to windows whose name matches.
    pub filter: gtk::SearchEntry,
    /// Header label: transient notices only. Standing facts go in the bar.
    pub status: gtk::Label,
    /// Zero-size labels carrying the bar's accent classes, so `set_bar` can
    /// read the palette's colour for Pango markup - CSS cannot colour half a
    /// label, and markup cannot reference a CSS token.
    pub probe_live: gtk::Label,
    pub probe_total: gtk::Label,
    /// "N windows" beside the Projects eyebrow.
    pub session_count: gtk::Label,
    /// The `+` beside the eyebrow: a terminal in the selected project.
    pub sidebar_new: gtk::Button,
    /// Bottom bar: one label per configured segment, in `config.bar` order.
    /// Kept as (id, label) pairs so `set_bar` fills them by name and an
    /// unrecognised id costs nothing.
    pub bar: Vec<(String, gtk::Label)>,
    pub web: crate::web::Chip,
    pub pair: crate::web::Pair,
    /// Per-project cost, over the bar segment that totals it.
    pub perf: crate::perf::Perf,
    /// Pane search. Hidden until asked for.
    pub find: crate::find::FindBar,
    pub stack: gtk::Stack,
    pub empty: adw::StatusPage,
    pub add_agent: adw::SplitButton,
    pub add_project: gtk::Button,
}

/// What the bottom bar reports. Assembled once per refresh so the bar cannot
/// disagree with itself across labels.
pub struct Bar<'a> {
    pub root: Option<&'a std::path::Path>,
    pub project: Option<&'a str>,
    /// The selected project's branch, already carrying its dirty and
    /// ahead/behind counts: one `git` invocation, read on the slow cadence.
    pub branch: Option<&'a str>,
    pub windows: usize,
    pub live: usize,
    pub attention: usize,
    /// The window with the keyboard, if any.
    pub focus: Option<BarFocus<'a>>,
    /// KiB PSS: this process, the tmux server, this project's pane
    /// processes, and every pane the session is holding - the last is what
    /// the total is made of, so switching projects never shrinks it.
    pub ui_kib: u64,
    pub tmux_kib: u64,
    pub project_kib: u64,
    pub panes_kib: u64,
    /// The tmux server's version and how many panes it is holding - the
    /// server is a process TaiX started and does not otherwise account for.
    pub tmux_version: Option<&'a str>,
    pub panes: usize,
    /// How long this window has been up.
    pub uptime: std::time::Duration,
}

/// What the bar says about the focused window. Every field is something the
/// pane header cannot show without stealing width from the output.
pub struct BarFocus<'a> {
    pub name: &'a str,
    pub harness: &'a str,
    pub state: &'a str,
    /// The grid tmux is holding for it, as last applied.
    pub size: Option<(usize, usize)>,
    /// Lines above the live bottom; 0 is live.
    pub scroll: usize,
    /// Its own branch and diff, for a window on a git worktree.
    pub worktree: Option<&'a str>,
    /// PSS of this window's own process tree, in KiB.
    pub kib: Option<u64>,
}

/// One window's pane: header with name, harness, state badge and a close
/// button, then the grid.
#[derive(Clone)]
pub struct AgentCard {
    pub root: gtk::Box,
    pub body: gtk::Label,
    /// Clipping viewport around `body`. Measure THIS, never the label: a
    /// label's natural size follows its text, so sizing the tmux grid from it
    /// feeds content size back into available space and the row count ratchets
    /// upward every frame. A `GtkScrolledWindow` has a small minimum size
    /// independent of its child, which breaks that loop.
    pub viewport: gtk::ScrolledWindow,
    pub badge: gtk::Label,
    /// State-coloured dot before the name.
    pub dot: gtk::Image,
    /// Harness label under the name, hidden when it would echo the name.
    pub kind_label: gtk::Label,
    /// The window's name; changes on rename and on auto-naming.
    pub name_label: gtk::Label,
    /// Branch and dirty count for a window with its own worktree. Set by the
    /// caller, never derived here: it costs a `git` invocation, so it is
    /// refreshed on the slow cadence rather than on every card update.
    pub git_label: gtk::Label,
    /// How far above the live output this pane is scrolled. Hidden at the
    /// bottom, because a permanent "0" is noise.
    pub scroll_label: gtk::Label,
    /// Resident memory of the pane's process tree. Set by the caller on the
    /// slow cadence: it reads `/proc`, and a header is not worth a syscall
    /// storm per frame.
    pub mem_label: gtk::Label,
    /// Lit while a browser is typing into this window. A label, not an
    /// icon: GTK does not recolour the bundled SVGs from CSS, and this mark
    /// has to read as a state, not as decoration.
    pub remote: gtk::Label,
    /// Shown over the bottom of a scrolled-back pane: one click returns to
    /// the live output. Hidden while the pane is already live.
    pub jump: gtk::Button,
    /// Closes this window. Kills the process; never discards a worktree.
    pub close: gtk::Button,
    /// What the program in the pane asked to be told about the pointer, as
    /// of the last output the emulator saw. Read by the pointer handlers,
    /// which run outside any `App` borrow and so cannot ask the row.
    pub mouse: Rc<std::cell::Cell<taix_term::Mouse>>,
    /// Shown over the body while the window has no pane: the harness's mark
    /// and "click to start". Clicking it relaunches the harness.
    pub start: gtk::Box,
    start_icon: gtk::Image,
    start_label: gtk::Label,
    /// Where a dragged header would land: a wash over the half of the pane
    /// it would take, or the whole pane for a swap.
    hint: gtk::Box,
}

impl AgentCard {
    /// Show the worktree's branch and how much has changed in it, or nothing
    /// when the window has no worktree.
    pub fn set_git(&self, text: Option<&str>) {
        self.git_label.set_text(text.unwrap_or_default());
    }

    /// Show the scrollback position, or nothing when the pane is live at the
    /// bottom.
    pub fn set_scroll(&self, lines: usize) {
        let text = if lines > 0 {
            format!("▲ {lines}")
        } else {
            String::new()
        };
        self.scroll_label.set_text(&text);
        self.jump.set_visible(lines > 0);
    }

    /// Memory in KiB, or nothing for a window with no process.
    pub fn set_mem(&self, kib: Option<u64>) {
        self.mem_label
            .set_text(&kib.map(taix_core::mem::human_kib).unwrap_or_default());
    }

    /// Preview a drop: `None` clears it.
    pub fn show_drop(&self, side: Option<crate::tree::Side>) {
        use crate::tree::Side;
        use gtk::Align::{End, Fill, Start};
        self.hint.set_visible(side.is_some());
        let (h, v) = match side {
            Some(Side::Left) => (Start, Fill),
            Some(Side::Right) => (End, Fill),
            Some(Side::Top) => (Fill, Start),
            Some(Side::Bottom) => (Fill, End),
            _ => (Fill, Fill),
        };
        self.hint.set_halign(h);
        self.hint.set_valign(v);
        // Half the pane along the split axis; the overlay clips the rest.
        let (w, hgt) = (self.root.width() / 2, self.root.height() / 2);
        self.hint.set_size_request(
            if h == Fill { -1 } else { w.max(1) },
            if v == Fill { -1 } else { hgt.max(1) },
        );
    }
}

impl AgentCard {
    pub fn update(&self, agent: &Agent, cfg: &taix_core::Config, tint: Option<&str>) {
        let state = agent.state.as_str();
        self.badge.set_text(state);
        self.badge.set_css_classes(&["badge", "mono", state]);
        self.dot.set_css_classes(&["agent-dot", state]);
        crate::icons::set(
            &self.dot,
            taix_core::by_id(cfg, &agent.kind).icon.as_deref(),
        );
        let subtitle = subtitle(agent, cfg);
        self.kind_label.set_text(&subtitle);
        self.kind_label.set_visible(!subtitle.is_empty());
        self.name_label.set_text(&agent.name);
        self.close
            .set_tooltip_text(Some(&format!("Close {}", agent.name)));
        // No pane: the window is a record, and the way back is one click.
        let harness = taix_core::by_id(cfg, &agent.kind);
        self.start.set_visible(agent.pane.is_none());
        crate::icons::set(&self.start_icon, harness.icon.as_deref());
        self.start_label
            .set_text(&format!("Click to start {}", harness.label));
        // Tint follows the same token set as the sidebar row, so a coloured
        // window is recognisable in both places.
        let mut classes: Vec<&str> = vec!["agent-card"];
        let tint = tint.map(|c| format!("tint-{c}"));
        if let Some(tint) = &tint {
            classes.push(tint);
        }
        if self.root.has_css_class("focused") {
            classes.push("focused");
        }
        self.root.set_css_classes(&classes);
    }
}

/// What to show under a window's name.
///
/// Generated names are the harness label ("Terminal", "Claude Code"), so
/// repeating it underneath says nothing. Prefer the branch when the window
/// has its own worktree, and otherwise show nothing rather than an echo.
fn subtitle(agent: &Agent, cfg: &taix_core::Config) -> String {
    if let Some(branch) = &agent.branch {
        return branch.clone();
    }
    let label = taix_core::by_id(cfg, &agent.kind).label;
    if agent.name == label {
        return String::new();
    }
    label
}

/// Every text in the header ellipsizes, and the reason is not the narrow
/// case: a `GtkPaned` with no set position divides at the ratio of its
/// children's *minimum* sizes, and an un-ellipsized label's minimum is its
/// text. One badge going from IDLE to WORKING widened its card's minimum
/// and every divider on screen re-ratioed. With minimums pinned, the card's
/// own `set_size_request` floor is the only minimum left.
pub fn agent_card(agent: &Agent, cfg: &taix_core::Config) -> AgentCard {
    let dot = crate::icons::image(taix_core::by_id(cfg, &agent.kind).icon.as_deref(), 13);
    dot.add_css_class("agent-dot");
    dot.set_valign(gtk::Align::Center);
    let name = gtk::Label::builder()
        .label(&agent.name)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["agent-name", "mono"])
        .build();
    let text = subtitle(agent, cfg);
    let kind_label = gtk::Label::builder()
        .label(&text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(48)
        .visible(!text.is_empty())
        .css_classes(["agent-harness", "mono"])
        .build();
    // Not ellipsized: the header is clipped rather than measured (see
    // `head_clip`), so the badge's width moves nothing - and `letter-spacing`
    // makes GTK under-measure an ellipsizable label, which truncated "IDLE"
    // to "IDL…".
    let badge = gtk::Label::builder()
        .label(agent.state.as_str())
        .valign(gtk::Align::Center)
        .build();

    // Built on first open, not here. A `GMenu` for one window costs ~0.3 MB
    // once its items, actions, labels and submenus exist, so a project with
    // twelve windows was paying 7 MB for menus nobody had opened yet.
    // `set_create_popup_func` is GTK's own hook for exactly this.
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .css_classes(["flat", "pane-menu"])
        .tooltip_text("Window menu (or right-click the header)")
        .valign(gtk::Align::Center)
        .build();
    let has_worktree = agent.worktree.is_some();
    menu.set_create_popup_func(move |button| {
        if button.menu_model().is_none() {
            button.set_menu_model(Some(&window_menu(has_worktree)));
        }
    });
    // Closing a window is a per-window action, so it lives on the window,
    // not behind a keystroke aimed at whichever pane happens to be focused.
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["flat", "pane-close"])
        .tooltip_text("Close this window")
        .valign(gtk::Align::Center)
        .build();

    // Figures the caller sets on its own cadence: a `git` invocation, the
    // scroll state, a `/proc` walk. They empty rather than hide, and they
    // ellipsize: hiding a label or letting one grow changes the header's
    // minimum width, and every unpinned divider re-ratios from those.
    let meta_label = |class: &str, tip: &str| {
        gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            // One cell whether empty or not: an ellipsizable label's floor
            // is otherwise the ellipsis itself, which is 0 when empty.
            .width_chars(1)
            .css_classes([class, "mono"])
            .tooltip_text(tip)
            .valign(gtk::Align::Center)
            .build()
    };
    let git_label = meta_label("pane-git", "Worktree branch and changes");
    let scroll_label = meta_label("pane-scroll", "Scrolled back — press End or type to return");
    let mem_label = meta_label("pane-mem", "Resident memory of this window's processes");

    // The web front end's mark, on the pane itself: the sidebar says which
    // terminal is being driven, and this says it on the screen you are
    // looking at.
    let remote = gtk::Label::builder()
        .label("web")
        .css_classes(["pane-remote", "mono"])
        .valign(gtk::Align::Center)
        .visible(false)
        .tooltip_text("A browser is typing here - click the pane to take it back")
        .build();
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    head.add_css_class("agent-head");
    head.append(&dot);
    head.append(&name);
    head.append(&kind_label);
    // The slack lives in its own widget: the meta label is hidden when it
    // would echo the name, and a hidden expander expands nothing.
    head.append(&gtk::Box::builder().hexpand(true).build());
    head.append(&scroll_label);
    head.append(&git_label);
    head.append(&mem_label);
    head.append(&remote);
    head.append(&badge);
    head.append(&menu);
    head.append(&close);
    wire_secondary_click(&head, &menu);
    // Drag the header onto another pane: its edges split, its middle swaps.
    // The payload is the agent id; `app` owns the drop side.
    let drag = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .content(&gtk::gdk::ContentProvider::for_value(&agent.id.to_value()))
        .build();
    let icon_of = head.clone();
    drag.connect_drag_begin(move |source, _| {
        let paintable = gtk::WidgetPaintable::new(Some(&icon_of));
        source.set_icon(Some(&paintable), 0, 0);
    });
    head.add_controller(drag);

    let body = gtk::Label::builder()
        .xalign(0.0)
        .yalign(0.0)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .wrap(false)
        .use_markup(true)
        .selectable(true)
        .css_classes(["pane"])
        .build();

    // `External` policy: no scrollbars and, crucially, no propagation of the
    // label's natural size, so the viewport's allocation reflects the space
    // the layout gives it rather than the amount of text in it. tmux owns the
    // scrollback, so clipping is the correct behaviour here, not scrolling.
    let viewport = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_width(false)
        .propagate_natural_height(false)
        .hexpand(true)
        .vexpand(true)
        .build();

    // Right-click on the output: a context menu at the pointer with what a
    // terminal's right-click means - copy, paste, kill - in place of the
    // label's own Cut/Copy/Paste/Delete, which is meant for an entry.
    // Capture and claim, so the label never sees the press. Built on first
    // use, like the window menu, and for the same reason.
    let context = gtk::GestureClick::new();
    context.set_button(gtk::gdk::BUTTON_SECONDARY);
    context.set_propagation_phase(gtk::PropagationPhase::Capture);
    let popover: std::cell::OnceCell<gtk::PopoverMenu> = std::cell::OnceCell::new();
    context.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let Some(anchor) = gesture.widget() else {
            return;
        };
        let popover = popover.get_or_init(|| {
            let popover = gtk::PopoverMenu::from_model(Some(&body_menu()));
            popover.set_parent(&anchor);
            popover.set_has_arrow(false);
            popover.set_halign(gtk::Align::Start);
            // A popover is a child of its parent but not in its layout, so
            // the parent's own dispose does not take it down.
            let orphan = popover.clone();
            anchor.connect_destroy(move |_| orphan.unparent());
            popover
        });
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    viewport.add_controller(context);

    let start_icon = gtk::Image::builder().pixel_size(48).build();
    start_icon.add_css_class("pane-start-icon");
    let start_label = gtk::Label::builder()
        .css_classes(["pane-start-label"])
        .build();
    let start_inner = gtk::Box::new(gtk::Orientation::Vertical, 10);
    start_inner.set_halign(gtk::Align::Center);
    start_inner.set_valign(gtk::Align::Center);
    start_inner.set_vexpand(true);
    start_inner.append(&start_icon);
    start_inner.append(&start_label);
    // Fills the body so the click lands anywhere in the pane, and so the
    // stale text behind it reads as history rather than as a live screen.
    let start = gtk::Box::new(gtk::Orientation::Vertical, 0);
    start.add_css_class("pane-start");
    start.set_cursor_from_name(Some("pointer"));
    start.append(&start_inner);
    let hint = gtk::Box::builder()
        .visible(false)
        .can_target(false)
        .css_classes(["pane-drop"])
        .build();
    // Scrolled back into history: one click to the live bottom, where the
    // text ends rather than up in the header, because that is where the eye
    // already is. `can_focus` off: it must not take the keys off the pane.
    let jump = gtk::Button::builder()
        .label("↓ live")
        .visible(false)
        .can_focus(false)
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .tooltip_text("Back to the live output")
        .css_classes(["pane-jump"])
        .build();
    jump.set_cursor_from_name(Some("pointer"));
    let stage = gtk::Overlay::builder().child(&viewport).build();
    stage.add_overlay(&start);
    stage.add_overlay(&jump);
    stage.add_overlay(&hint);
    stage.set_hexpand(true);
    stage.set_vexpand(true);

    // The header is clipped, not measured: a `GtkPaned` with no set position
    // divides at the ratio of its children's minimums, and a header whose
    // minimum followed its text - a badge growing from IDLE to WORKING, a
    // memory figure gaining a digit - moved every divider by a few columns,
    // resized every pane and made tmux re-lay every window out, on every
    // state change, under your fingers. `External` propagates no width;
    // `Never` keeps the height the header's own.
    let head_clip = gtk::ScrolledWindow::builder()
        .child(&head)
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("agent-card");
    root.append(&head_clip);
    root.append(&stage);
    root.set_hexpand(true);
    root.set_vexpand(true);
    // Floor for a divider drag, so a pane cannot be squeezed out of
    // existence - and, with the header and body both unmeasured, the whole
    // of a card's minimum, so every card weighs the same in a paned.
    root.set_size_request(200, 110);

    let card = AgentCard {
        root,
        body,
        viewport,
        badge,
        dot,
        kind_label,
        name_label: name,
        git_label,
        scroll_label,
        mem_label,
        remote,
        jump,
        close,
        start,
        start_icon,
        start_label,
        hint,
        mouse: Rc::default(),
    };
    let tint = agent
        .color
        .clone()
        .or_else(|| taix_core::by_id(cfg, &agent.kind).color);
    card.update(agent, cfg, tint.as_deref());
    card
}

/// Width and height of one monospace cell for `widget`'s current font.
///
/// Measured by laying out real text in the widget's own Pango context, not
/// from `FontMetrics`: `metrics().height()` disagreed with the rendered line
/// height badly enough to ask tmux for three times the rows that fit.
/// Two lines are measured so line spacing is included.
///
/// Fractional, from Pango units rather than `pixel_size`: rounding a 15.5px
/// line up to 16 is invisible at the top of a pane and a whole row out by
/// the bottom of it, which is where a click then lands on the wrong line.
pub fn cell_size(widget: &impl IsA<gtk::Widget>) -> (f64, f64) {
    const COLS: f64 = 10.0;
    const SCALE: f64 = gtk::pango::SCALE as f64;
    let layout = gtk::pango::Layout::new(&widget.as_ref().pango_context());
    layout.set_text("0000000000\n0000000000");
    let (_, logical) = layout.extents();
    (
        (f64::from(logical.width()) / SCALE / COLS).max(1.0),
        (f64::from(logical.height()) / SCALE / 2.0).max(1.0),
    )
}

/// Where a pane label's first cell sits inside the widget, in pixels.
///
/// This is the `.pane` padding, but read back from the layout GTK actually
/// draws rather than copied into a constant: the two drifted apart once
/// already, which cost the pane a column and every pointer report a cell.
pub fn text_origin(label: &gtk::Label) -> (f64, f64) {
    let (x, y) = label.layout_offsets();
    (f64::from(x), f64::from(y))
}

pub fn build(app: &adw::Application, cfg: &Config) -> Widgets {
    let status = gtk::Label::builder()
        .css_classes(["taix-status", "mono"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    // Same shape as *New terminal*: the other thing you make, and the first
    // thing you do in an empty window, so it is a labelled button too - and
    // a different hue, because they are not the same thing.
    let add_project = gtk::Button::builder()
        .child(&labelled("folder-new-symbolic", "Add project"))
        .tooltip_text("Add a folder as a project")
        .css_classes(["add-project"])
        .valign(gtk::Align::Center)
        .build();
    // No remove-project button up here: one mis-aimed click beside *Files*
    // would tear down a project's windows. It lives in the project row's
    // own menu, where the target is the thing you right-clicked.
    // Settings is a destination, not an item in a list of destinations: it
    // stays on the bar rather than hiding one level in behind the menu.
    let settings = icon_button("emblem-system-symbolic", "Settings");
    settings.set_action_name(Some("taix.settings"));
    let automation = icon_button("alarm-symbolic", "Automation (Ctrl+Shift-J)");
    automation.set_action_name(Some("taix.automation"));
    // A labelled split button, not a bare `+`: the primary action is a plain
    // terminal and that has to be readable, not guessed from an icon.
    let add_agent = adw::SplitButton::new();
    add_agent.set_child(Some(&labelled(
        "utilities-terminal-symbolic",
        "New terminal",
    )));
    add_agent.set_tooltip_text(Some("New terminal — the arrow lists agents"));
    add_agent.add_css_class("new-terminal");
    add_agent.set_valign(gtk::Align::Center);

    // A plain popover of buttons rather than a `GMenu`: GTK4 menus do not
    // draw an item's icon, and the icon is the point. Its contents are
    // rebuilt by `refresh_harness_menu` when the settings change.
    let popover = gtk::Popover::builder()
        .css_classes(["harness-menu"])
        .has_arrow(false)
        .build();
    popover.set_child(Some(&harness_list(cfg)));
    add_agent.set_popover(Some(&popover));

    // Hover opens the agent list after a beat. Opening instantly made the
    // menu ambush anyone crossing the header on the way to another button.
    const HOVER_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
    let motion = gtk::EventControllerMotion::new();
    let pending_hover: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    motion.connect_enter({
        let popover = popover.clone();
        let pending = pending_hover.clone();
        move |_, _, _| {
            if popover.is_visible() || pending.borrow().is_some() {
                return;
            }
            let popover = popover.clone();
            let slot = pending.clone();
            let id = glib::timeout_add_local_once(HOVER_DELAY, move || {
                // Drop the handle first: the source has already fired, and
                // removing a finished source is a GLib warning.
                slot.borrow_mut().take();
                popover.popup();
            });
            *pending.borrow_mut() = Some(id);
        }
    });
    motion.connect_leave({
        let pending = pending_hover.clone();
        move |_| {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
        }
    });
    add_agent.add_controller(motion);
    #[cfg(feature = "browser")]
    let browser_toggle = gtk::ToggleButton::builder()
        .icon_name("web-browser-symbolic")
        .tooltip_text("Toggle browser panel (Ctrl-B)")
        .build();
    // Plain buttons, not toggles: a toggle's "off" would have to mean both
    // "close the column" and "the other tab is showing", and a widget that
    // switches itself off while the app switches it on deadlocks the two.
    // The lit one carries a class instead.
    let files_toggle = gtk::Button::builder()
        .icon_name("folder-symbolic")
        .tooltip_text("Project files (Ctrl+Shift-T)")
        .build();
    let git_toggle = gtk::Button::builder()
        .icon_name("network-server-symbolic")
        .tooltip_text("Source control (Ctrl+Shift-G)")
        .build();

    // Two ways to get this project into a plain terminal: the short `taix`
    // line, or the whole tmux line for a machine without taix on it. It
    // lives beside the project actions because it is one.
    let copy_menu = gtk::gio::Menu::new();
    copy_menu.append(Some("Copy taix command"), Some("taix.copy-terminal"));
    copy_menu.append(Some("Copy tmux command"), Some("taix.copy-tmux"));
    let copy_button = gtk::MenuButton::builder()
        .icon_name("edit-copy-symbolic")
        .menu_model(&copy_menu)
        .tooltip_text("Open this project's windows in a terminal")
        .css_classes(["icon"])
        .valign(gtk::Align::Center)
        .build();

    let app_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&app_menu())
        .tooltip_text("Palette, find, layout, sessions, settings")
        .css_classes(["icon"])
        .valign(gtk::Align::Center)
        .build();

    // Title: the product, and under it which tmux session this window is.
    let title = gtk::Label::builder()
        .label("TaiX")
        .css_classes(["taix-title"])
        .build();
    let subtitle = gtk::Label::builder()
        .label(format!("taix · {}", cfg.tmux_session))
        .css_classes(["taix-subtitle", "mono"])
        .build();
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 1);
    titles.set_valign(gtk::Align::Center);
    titles.set_hexpand(true);
    titles.append(&title);
    titles.append(&subtitle);

    let sidebar_toggle = icon_button("sidebar-show-symbolic", "Toggle sidebar");
    let close_window = icon_button("window-close-symbolic", "Close TaiX");
    close_window.add_css_class("close-window");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    header.add_css_class("taix-header");
    header.append(&sidebar_toggle);
    // The two ways to make something: a project, then a window in it. Same
    // shape, left to right in the order they are used.
    header.append(&add_project);
    header.append(&add_agent);
    header.append(&titles);
    // The one-line feedback channel. It was built but never packed once, so
    // every error and every toast rendered into an orphan label.
    header.append(&status);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    actions.set_margin_start(4);
    actions.append(&copy_button);
    actions.append(&git_toggle);
    // The two panels, side by side, in the order they sit on screen.
    #[cfg(feature = "browser")]
    actions.append(&browser_toggle);
    actions.append(&files_toggle);
    actions.append(&automation);
    actions.append(&settings);
    actions.append(&app_button);
    close_window.set_margin_start(4);
    actions.append(&close_window);
    header.append(&actions);
    // No `HeaderBar`, so the drag region has to be declared.
    let handle = gtk::WindowHandle::builder().child(&header).build();
    // Sidebar: eyebrow, filter, projects, key hints. A holder of project
    // cards rather than a `GtkListBox` of project names: a window belongs to
    // a project, so the sidebar shows that nesting.
    let eyebrow = gtk::Label::builder()
        .label("Projects")
        .css_classes(["taix-eyebrow", "mono"])
        .build();
    let session_count = gtk::Label::builder()
        .css_classes(["taix-eyebrow-dim", "mono"])
        .hexpand(true)
        .xalign(0.0)
        .build();
    let sidebar_new = icon_button("list-add-symbolic", "New terminal in the selected project");
    let eyebrow_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    eyebrow_row.append(&eyebrow);
    eyebrow_row.append(&session_count);
    eyebrow_row.append(&sidebar_new);
    let filter = gtk::SearchEntry::builder()
        .placeholder_text("Filter windows")
        .css_classes(["taix-filter", "mono"])
        .build();
    let sidebar_top = gtk::Box::new(gtk::Orientation::Vertical, 9);
    sidebar_top.add_css_class("taix-sidebar-top");
    sidebar_top.append(&eyebrow_row);
    sidebar_top.append(&filter);

    let sidebar_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
    sidebar_list.add_css_class("taix-sidebar-list");
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .child(&sidebar_list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    // The keys you reach for without thinking, where you look when you do.
    let foot = gtk::Grid::builder()
        .css_classes(["taix-sidebar-foot"])
        .row_spacing(6)
        .column_homogeneous(true)
        .build();
    for (i, (l, r)) in [("new  ⇧⌃N", "zoom  ⇧⌃Z"), ("cycle  ⌃Tab", "close  ⇧⌃W")]
        .iter()
        .enumerate()
    {
        let left = gtk::Label::builder()
            .label(*l)
            .xalign(0.0)
            .css_classes(["mono"])
            .build();
        let right = gtk::Label::builder()
            .label(*r)
            .xalign(1.0)
            .css_classes(["mono"])
            .build();
        foot.attach(&left, 0, i as i32, 1, 1);
        foot.attach(&right, 1, i as i32, 1, 1);
    }

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.add_css_class("taix-sidebar");
    sidebar.append(&sidebar_top);
    sidebar.append(&sidebar_scroll);
    sidebar.append(&foot);
    // A minimum, not a fixed width: the divider below is draggable, and
    // `shrink` off stops a drag from collapsing either side to nothing.
    sidebar.set_size_request(200, -1);
    sidebar_toggle.connect_clicked({
        let sidebar = sidebar.clone();
        move |_| sidebar.set_visible(!sidebar.is_visible())
    });

    // Holder for the pane tree. Rebuilt only when the agent set changes, so a
    // divider the user dragged survives every redraw and every tmux event.
    let panes = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panes.add_css_class("taix-grid");
    panes.set_hexpand(true);
    panes.set_vexpand(true);

    let empty = adw::StatusPage::builder()
        .icon_name("utilities-terminal-symbolic")
        .title("No windows")
        .description("Add a folder, then open a terminal.")
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&panes, Some("panes"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    // The find bar sits under the panes and starts hidden: it is a mode, and
    // a mode that occupies space when it is off is a bug.
    let find = crate::find::find_bar();
    find.root.set_visible(false);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&stack);
    content.append(&find.root);
    content.set_size_request(320, -1);

    // Panes | browser. The browser child is attached on demand: WebKit is
    // expensive enough that it must not exist until asked for.
    let main = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&content)
        .resize_start_child(true)
        .shrink_start_child(false)
        .resize_end_child(true)
        .shrink_end_child(false)
        .build();

    // Files and Git share the one side column: two panels at once would
    // leave neither wide enough to read, and they answer the same question
    // - what is in this project right now.
    let panel_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(90)
        .vexpand(true)
        .build();
    let files = crate::files::Files::new();
    let git = crate::git::Git::new();
    panel_stack.add_named(&files.root, Some("files"));
    panel_stack.add_named(&git.root, Some("git"));

    let panel_tabs = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .css_classes(["panel-tabs"])
        .build();
    let files_tab = gtk::ToggleButton::builder()
        .label("Files")
        .active(true)
        .hexpand(true)
        .css_classes(["panel-tab"])
        .build();
    let git_tab = gtk::ToggleButton::builder()
        .label("Git")
        .group(&files_tab)
        .hexpand(true)
        .css_classes(["panel-tab"])
        .build();
    panel_tabs.append(&files_tab);
    panel_tabs.append(&git_tab);
    // The tabs answer for themselves: the stack, the lit header button, and
    // a git panel that reads the repository the moment it is looked at.
    // Nothing here touches `App`, which is mid-borrow whenever a click from
    // the header arrives.
    for (tab, name) in [(&files_tab, "files"), (&git_tab, "git")] {
        tab.connect_toggled({
            let panel_stack = panel_stack.clone();
            let (files_toggle, git_toggle) = (files_toggle.clone(), git_toggle.clone());
            let git = git.clone();
            move |t| {
                if !t.is_active() {
                    return;
                }
                panel_stack.set_visible_child_name(name);
                light(&files_toggle, name == "files");
                light(&git_toggle, name == "git");
                if name == "git" {
                    git.refresh();
                }
            }
        });
    }

    let panel_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel_body.append(&panel_tabs);
    panel_body.append(&panel_stack);

    // (panes, browser) | panel divider. The panel is attached on demand,
    // so a `Paned` with no end child is what a closed panel looks like.
    let files_split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&main)
        .resize_start_child(true)
        .shrink_start_child(false)
        .resize_end_child(false)
        .shrink_end_child(false)
        .build();

    let outer = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&sidebar)
        .end_child(&files_split)
        .resize_start_child(false)
        .shrink_start_child(false)
        .resize_end_child(true)
        .shrink_end_child(false)
        .position(262)
        .vexpand(true)
        .css_classes(["taix-outer"])
        .build();

    // Bottom bar: one label per configured segment rather than one string, so
    // the layout survives a long project path - only `where` ellipsizes, and
    // it is also the only segment that takes the slack. A bar with no
    // `where` segment gets an invisible spacer instead, or every label would
    // be crammed against the left edge.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    bar.add_css_class("statusbar");
    // The web chip sits with the path, at the reading end of the bar: it is
    // an address someone has to type into a phone, not another number.
    let web = crate::web::Chip::new();
    let pair = crate::web::Pair::new();
    let mut chip_placed = false;
    let mut segments: Vec<(String, gtk::Label)> = Vec::new();
    let mut facts = 0;
    for id in &cfg.bar {
        let label = match id.as_str() {
            // Bounded rather than hexpanding, so the chip beside it stays
            // beside it; the slack goes to the spacer that follows them.
            "where" => gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Start)
                .max_width_chars(46)
                .css_classes(["bar-where", "mono"])
                .build(),
            "branch" => gtk::Label::builder()
                .css_classes(["bar-branch", "mono"])
                .build(),
            "empty" => gtk::Label::builder().hexpand(true).build(),
            _ => {
                // A hairline between facts on the right, never after the path.
                if facts > 0 {
                    let sep = gtk::Label::builder()
                        .label("│")
                        .css_classes(["bar-sep", "mono"])
                        .build();
                    bar.append(&sep);
                }
                facts += 1;
                gtk::Label::builder()
                    .css_classes(["bar-dim", "mono"])
                    .use_markup(true)
                    .build()
            }
        };
        bar.append(&label);
        if id == "where" {
            bar.append(&web.root);
            bar.append(&pair.root);
            bar.append(&gtk::Label::builder().hexpand(true).build());
            chip_placed = true;
        }

        segments.push((id.clone(), label));
    }
    if !chip_placed {
        bar.prepend(&web.root);
        bar.insert_child_after(&pair.root, Some(&web.root));
        if !cfg.bar.iter().any(|id| id == "empty") {
            bar.insert_child_after(
                &gtk::Label::builder().hexpand(true).build(),
                Some(&pair.root),
            );
        }
    }
    let probe_live = gtk::Label::builder().css_classes(["bar-live"]).build();
    let probe_total = gtk::Label::builder().css_classes(["bar-total"]).build();
    bar.append(&probe_live);
    bar.append(&probe_total);

    // The perf monitor hangs off the segment that totals the same numbers,
    // and that segment is marked so it reads as something to point at. A bar
    // configured without `memory` still gets the monitor, anchored on the
    // bar itself rather than silently losing it.
    let perf_anchor: gtk::Widget = match segments.iter().find(|(id, _)| id == "memory") {
        Some((_, label)) => {
            label.add_css_class("bar-perf");
            label.clone().upcast()
        }
        None => bar.clone().upcast(),
    };
    let perf = crate::perf::Perf::new(&perf_anchor);

    // The glass: header, body, bar inside one rounded frame over the glows.
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("taix-frame");
    frame.append(&handle);
    frame.append(&outer);
    frame.append(&bar);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(1500)
        .default_height(950)
        .content(&frame)
        .build();
    window.add_css_class("taix");
    close_window.connect_clicked({
        let window = window.clone();
        move |_| window.close()
    });
    #[cfg(feature = "browser")]
    browser_toggle.add_css_class("icon");
    files_toggle.add_css_class("icon");
    git_toggle.add_css_class("icon");

    Widgets {
        window,
        panes,
        outer,
        #[cfg(feature = "browser")]
        main,
        #[cfg(feature = "browser")]
        browser: browser_toggle,
        files_split,
        files_toggle,
        files,
        panel_body,
        panel_stack,
        files_tab,
        git_tab,
        git_toggle,
        git,
        sidebar_list,
        filter,
        status,
        probe_live,
        probe_total,
        bar: segments,
        web,
        pair,
        perf,
        find,
        stack,
        empty,
        add_agent,
        add_project,
        sidebar_new,
        session_count,
    }
}

/// A 30px square that shows an icon and nothing else until hovered.
fn icon_button(icon: &str, tip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tip)
        .css_classes(["icon"])
        .valign(gtk::Align::Center)
        .build()
}

/// Icon then label, for the two buttons that make something. Built as a box
/// rather than a button's `icon-name` + `label`, which GTK4 does not offer
/// together: `icon_name` replaces the child.
fn labelled(icon: &str, text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(&gtk::Label::new(Some(text)));
    row
}

/// The agent list under the split button: one row per offered harness,
/// icon then label, each firing `harness.spawn::<id>`.
fn harness_list(cfg: &Config) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 1);
    list.add_css_class("harness-list");
    for harness in taix_core::available(cfg) {
        // The terminal is the primary action, so it is not repeated here.
        if harness.is_terminal() {
            continue;
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        let icon = crate::icons::image(harness.icon.as_deref(), 15);
        icon.set_css_classes(&["harness-icon"]);
        row.append(&icon);
        row.append(
            &gtk::Label::builder()
                .label(&harness.label)
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        let button = gtk::Button::builder()
            .child(&row)
            .css_classes(["flat", "harness-item"])
            // Pointer-only: GTK hands the first item focus on popup, and the
            // ring that draws reads as a selection nobody made.
            .focusable(false)
            .action_name("harness.spawn")
            .action_target(&harness.id.to_variant())
            .build();
        if let Some(color) = &harness.color {
            button.add_css_class(&format!("tint-{color}"));
        }
        // A button with an action does not close the popover it sits in.
        button.connect_clicked(|b| {
            if let Some(p) = b.ancestor(gtk::Popover::static_type()) {
                p.downcast::<gtk::Popover>().unwrap().popdown();
            }
        });
        list.append(&button);
    }
    if list.first_child().is_none() {
        list.append(
            &gtk::Label::builder()
                .label("No agent CLIs found on PATH")
                .css_classes(["dim"])
                .margin_start(8)
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build(),
        );
    }
    list
}

/// Rebuild the agent list after the settings changed what is offered.
pub fn refresh_harness_menu(w: &Widgets, cfg: &Config) {
    if let Some(popover) = w.add_agent.popover() {
        popover.set_child(Some(&harness_list(cfg)));
    }
}

/// Take a discarded tree apart so the cards inside it can be reused.
///
/// Dropping the tree's root does not unparent its grandchildren, and
/// `unparent` is not the way to detach a `GtkPaned` child - the paned keeps
/// its own pointer to it. Clearing the slots is. Without this, going from
/// two panes to three failed GTK's "child already has a parent" assertion
/// and every surviving pane vanished, leaving only one card on screen.
fn detach_tree(widget: &gtk::Widget) {
    let Some(paned) = widget.downcast_ref::<gtk::Paned>() else {
        return;
    };
    if let Some(child) = paned.start_child() {
        paned.set_start_child(None::<&gtk::Widget>);
        detach_tree(&child);
    }
    if let Some(child) = paned.end_child() {
        paned.set_end_child(None::<&gtk::Widget>);
        detach_tree(&child);
    }
}

/// Install a fresh pane tree. The old one is torn down *before* `build`
/// runs, because a card can only be re-parented once it has no parent.
/// Called only when the arrangement changes shape, so dividers the user
/// dragged are not reset by ordinary redraws.
pub fn set_pane_tree(
    w: &Widgets,
    build: impl FnOnce() -> Option<gtk::Widget>,
) -> Option<gtk::Widget> {
    while let Some(child) = w.panes.first_child() {
        w.panes.remove(&child);
        detach_tree(&child);
    }
    let tree = build();
    if let Some(tree) = &tree {
        w.panes.append(tree);
    }
    tree
}

pub fn set_status(w: &Widgets, text: &str) {
    w.status.set_text(text);
}

pub fn set_empty(w: &Widgets, no_agents: bool, no_projects: bool) {
    w.stack
        .set_visible_child_name(if no_agents { "empty" } else { "panes" });
    w.add_agent.set_sensitive(!no_projects);
    if no_projects {
        w.empty.set_title("No projects");
        w.empty
            .set_description(Some("Add a folder to get started."));
    } else {
        w.empty.set_title("No windows");
        w.empty
            .set_description(Some("Open a terminal or start an agent."));
    }
}

/// The eight accents a window can be tinted with, plus the theme default.
///
/// A fixed set, not a colour picker: these map to CSS classes the theme
/// defines, so a tinted window still follows a light or dark palette.
pub const COLORS: [(&str, &str); 8] = [
    ("red", "Red"),
    ("orange", "Orange"),
    ("yellow", "Yellow"),
    ("green", "Green"),
    ("teal", "Teal"),
    ("blue", "Blue"),
    ("purple", "Purple"),
    ("pink", "Pink"),
];

/// One project's sidebar card: a foldable header carrying the window count,
/// then its windows, then the way to make another one.
///
/// Returned unwired - `app` connects the buttons, because nothing in this
/// file may touch application state. Held by `App` and updated in place:
/// rebuilding it whenever a window changed state destroyed these buttons
/// between a pointer press and its release, which swallowed clicks.
pub struct SidebarCard {
    pub root: gtk::Box,
    pub project: ProjectId,
    /// Selects the project.
    pub select: gtk::Button,
    /// Folds and unfolds the window list.
    pub fold: gtk::Button,
    pub fold_icon: gtk::Image,
    /// Opens a plain terminal in it.
    pub new_terminal: gtk::Button,
    pub count: gtk::Label,
    pub attention: gtk::Label,
    /// Shown on a folded project when a browser is driving one of its
    /// terminals: the row that would have said so is not on screen.
    pub remote: gtk::Label,
    /// The part that folds away.
    pub body: gtk::Box,
    pub windows: Vec<SidebarRow>,
}

pub struct SidebarRow {
    pub id: taix_core::AgentId,
    pub root: gtk::Box,
    /// Focuses the window.
    pub open: gtk::Button,
    /// Closes it.
    pub close: gtk::Button,
    /// Harness icon in a box, recoloured per state without rebuilding the row.
    pub dot: gtk::Image,
    pub label: gtk::Label,
    /// Lit while a browser is typing into this window.
    pub remote: gtk::Label,
    /// The state's name, uppercase, in its colour.
    pub state: gtk::Label,
}

/// Menu model for a project. Actions resolve against the `project` group the
/// card carries, so every card's items address its own project.
fn project_menu(harnesses: &[taix_core::Harness]) -> gtk::gio::Menu {
    let menu = gtk::gio::Menu::new();

    let open = gtk::gio::Menu::new();
    open.append(Some("New terminal"), Some("project.new-terminal"));
    let agents = gtk::gio::Menu::new();
    for harness in harnesses {
        if harness.is_terminal() {
            continue;
        }
        agents.append(
            Some(&harness.label),
            Some(&format!("project.spawn::{}", harness.id)),
        );
    }
    if agents.n_items() > 0 {
        open.append_submenu(Some("Start agent"), &agents);
    }
    menu.append_section(None, &open);

    let edit = gtk::gio::Menu::new();
    edit.append(Some("Rename…"), Some("project.rename"));
    edit.append(Some("Copy path"), Some("project.copy-path"));
    edit.append(Some("Open folder"), Some("project.open-folder"));
    edit.append(Some("Fold"), Some("project.fold"));
    menu.append_section(None, &edit);

    let danger = gtk::gio::Menu::new();
    danger.append(Some("Close all windows"), Some("project.close-all"));
    danger.append(Some("Remove project…"), Some("project.remove"));
    menu.append_section(None, &danger);
    menu
}

/// Menu model for the window itself, in the header. Resolves against the
/// `taix` group installed on the window.
///
/// Everything here is also a keystroke; the menu exists so the keystrokes are
/// discoverable, which is the same reason `New terminal` is a labelled button
/// rather than a bare `+`.
fn app_menu() -> gtk::gio::Menu {
    let menu = gtk::gio::Menu::new();

    let jump = gtk::gio::Menu::new();
    jump.append(Some("Command palette…"), Some("taix.palette"));
    jump.append(Some("Find in pane…"), Some("taix.find"));
    menu.append_section(None, &jump);

    let arrange = gtk::gio::Menu::new();
    for preset in crate::presets::Preset::ALL {
        arrange.append(
            Some(preset.label()),
            Some(&format!("taix.preset::{}", preset.id())),
        );
    }
    menu.append_submenu(Some("Layout"), &arrange);

    let session = gtk::gio::Menu::new();
    session.append(Some("Save session…"), Some("taix.save-session"));
    session.append(Some("Restore session…"), Some("taix.restore-session"));
    menu.append_section(None, &session);

    let notice = gtk::gio::Menu::new();
    notice.append(Some("Mute this project"), Some("taix.mute"));
    menu.append_section(None, &notice);

    let prefs = gtk::gio::Menu::new();
    prefs.append(Some("Automation…"), Some("taix.automation"));
    prefs.append(Some("Settings…"), Some("taix.settings"));
    menu.append_section(None, &prefs);
    menu
}

/// Menu model for one window.
fn window_menu(has_worktree: bool) -> gtk::gio::Menu {
    let menu = gtk::gio::Menu::new();

    let top = gtk::gio::Menu::new();
    top.append(Some("Focus"), Some("win.focus"));
    top.append(Some("Rename…"), Some("win.rename"));
    let colors = gtk::gio::Menu::new();
    colors.append(Some("Default"), Some("win.color::none"));
    for (id, label) in COLORS {
        colors.append(Some(label), Some(&format!("win.color::{id}")));
    }
    top.append_submenu(Some("Colour"), &colors);
    menu.append_section(None, &top);

    let clip = gtk::gio::Menu::new();
    clip.append(Some("Select all"), Some("win.select-all"));
    clip.append(Some("Copy"), Some("win.copy"));
    clip.append(Some("Paste"), Some("win.paste"));
    menu.append_section(None, &clip);

    let run = gtk::gio::Menu::new();
    run.append(Some("Interrupt (Ctrl-C)"), Some("win.interrupt"));
    run.append(Some("Kill process"), Some("win.kill"));
    run.append(Some("Restart"), Some("win.restart"));
    // No picker needed: the other side is the pane you were in before this
    // one, which is what you want to compare against anyway.
    run.append(Some("Compare with last pane"), Some("win.compare"));
    run.append(Some("Copy transcript path"), Some("win.transcript"));
    menu.append_section(None, &run);

    if has_worktree {
        let git = gtk::gio::Menu::new();
        git.append(Some("Open diff"), Some("win.diff"));
        git.append(Some("Merge back…"), Some("win.merge"));
        menu.append_section(None, &git);
    }

    let danger = gtk::gio::Menu::new();
    danger.append(Some("Close"), Some("win.close"));
    if has_worktree {
        danger.append(Some("Close and discard worktree…"), Some("win.discard"));
    }
    menu.append_section(None, &danger);
    menu
}

/// The output's own right-click menu: the short list a terminal user expects
/// there. Everything else is a header away.
fn body_menu() -> gtk::gio::Menu {
    let menu = gtk::gio::Menu::new();
    let clip = gtk::gio::Menu::new();
    clip.append(Some("Copy"), Some("win.copy"));
    clip.append(Some("Paste"), Some("win.paste"));
    clip.append(Some("Select all"), Some("win.select-all"));
    menu.append_section(None, &clip);
    let run = gtk::gio::Menu::new();
    run.append(Some("Interrupt (Ctrl-C)"), Some("win.interrupt"));
    run.append(Some("Kill process"), Some("win.kill"));
    menu.append_section(None, &run);
    let danger = gtk::gio::Menu::new();
    danger.append(Some("Close window"), Some("win.close"));
    menu.append_section(None, &danger);
    menu
}

/// Open a popover on right-click as well as on its button, because a context
/// menu nobody can right-click for is not a context menu.
fn wire_secondary_click(widget: &impl IsA<gtk::Widget>, menu: &gtk::MenuButton) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let menu = menu.clone();
    gesture.connect_pressed(move |_, _, _, _| menu.popup());
    widget.as_ref().add_controller(gesture);
}

pub fn sidebar_card(
    project: &Project,
    windows: &[(Agent, taix_core::Harness)],
    harnesses: &[taix_core::Harness],
) -> SidebarCard {
    let fold_icon = gtk::Image::builder()
        .icon_name("pan-down-symbolic")
        .css_classes(["project-caret"])
        .build();
    let fold = gtk::Button::builder()
        .child(&fold_icon)
        .css_classes(["flat", "fold"])
        .tooltip_text("Fold this project")
        .valign(gtk::Align::Center)
        .build();
    let folder = gtk::Image::builder()
        .icon_name("folder-symbolic")
        .css_classes(["project-folder"])
        .valign(gtk::Align::Center)
        .build();
    let name = gtk::Label::builder()
        .label(&project.name)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["project-name", "mono"])
        .build();
    let path = gtk::Label::builder()
        .label(taix_core::text::contract_home(&project.root))
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["project-path", "mono"])
        .build();
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_hexpand(true);
    titles.append(&name);
    titles.append(&path);
    // Always present, hidden when zero: a conditional child would have to be
    // added and removed, which is the rebuilding this design avoids.
    let attention = gtk::Label::builder()
        .css_classes(["attention", "mono"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    // The web front end's mark. Built with the card, hidden until a browser
    // claims something: the folded header is the only place left to say it.
    let remote = gtk::Label::builder()
        .label("web")
        .css_classes(["row-remote", "mono"])
        .valign(gtk::Align::Center)
        .visible(false)
        .tooltip_text("A browser is typing in this project")
        .build();
    let count = gtk::Label::builder()
        .css_classes(["count", "mono"])
        .valign(gtk::Align::Center)
        .build();
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    head.append(&fold);
    head.append(&folder);
    head.append(&titles);
    head.append(&remote);
    head.append(&attention);
    head.append(&count);
    let select = gtk::Button::builder()
        .child(&head)
        .hexpand(true)
        .css_classes(["flat", "project-head"])
        .tooltip_text(project.root.display().to_string())
        .build();
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .css_classes(["flat", "row-menu"])
        .tooltip_text("Project menu (or right-click)")
        .valign(gtk::Align::Center)
        .build();
    // Same deal as the pane menu, and this one carries a harness submenu.
    let owned: Vec<taix_core::Harness> = harnesses.to_vec();
    menu.set_create_popup_func(move |button| {
        if button.menu_model().is_none() {
            button.set_menu_model(Some(&project_menu(&owned)));
        }
    });

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header.append(&select);
    header.append(&menu);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
    root.add_css_class("project-card");
    root.append(&header);
    // Drag a project onto another to reorder the sidebar. The payload is the
    // project id as a `u64`: the pane dock already claims `i64` for agent
    // ids, and both ids are `i64`, so the *type* is what keeps a pane header
    // dropped on the sidebar - and a project dropped on a pane - from being
    // read as the wrong thing. `app` owns where it lands.
    let drag = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .content(&gtk::gdk::ContentProvider::for_value(
            &(project.id as u64).to_value(),
        ))
        .build();
    let (icon_of, card) = (select.clone(), root.clone());
    drag.connect_drag_begin(move |source, _| {
        // The card hangs off the pointer at the grab height, not from its own
        // corner, so what moves under the hand is what was picked up.
        let paintable = gtk::WidgetPaintable::new(Some(&icon_of));
        let hot = icon_of.height() / 2;
        source.set_icon(Some(&paintable), 18, hot);
        card.add_css_class("dragging");
    });
    let card = root.clone();
    drag.connect_drag_end(move |_, _, _| card.remove_css_class("dragging"));
    select.add_controller(drag);
    wire_secondary_click(&select, &menu);
    wire_secondary_click(&root, &menu);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
    body.add_css_class("project-body");
    root.append(&body);

    let mut rows = Vec::with_capacity(windows.len());
    for (agent, harness) in windows {
        let dot = crate::icons::image(harness.icon.as_deref(), 11);
        dot.set_css_classes(&["dot"]);
        dot.set_valign(gtk::Align::Center);
        let label = gtk::Label::builder()
            .label(&agent.name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["window-name", "mono"])
            .build();
        let state = gtk::Label::builder()
            .label(agent.state.as_str())
            .css_classes(["window-state", "mono"])
            .build();
        let row_remote = gtk::Label::builder()
            .label("web")
            .css_classes(["row-remote", "mono"])
            .valign(gtk::Align::Center)
            .visible(false)
            .tooltip_text("A browser is typing into this terminal")
            .build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 9);
        line.append(&dot);
        line.append(&label);
        line.append(&row_remote);
        line.append(&state);
        let open = gtk::Button::builder()
            .child(&line)
            .hexpand(true)
            .css_classes(["flat", "window-row"])
            .tooltip_text(&harness.label)
            .build();
        let row_menu = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .css_classes(["flat", "row-menu"])
            .tooltip_text("Window menu (or right-click)")
            .valign(gtk::Align::Center)
            .build();
        let row_worktree = agent.worktree.is_some();
        row_menu.set_create_popup_func(move |button| {
            if button.menu_model().is_none() {
                button.set_menu_model(Some(&window_menu(row_worktree)));
            }
        });
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .css_classes(["flat", "window-close"])
            .tooltip_text(format!("Close {}", agent.name))
            .valign(gtk::Align::Center)
            .build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("window-line");
        row.append(&open);
        row.append(&row_menu);
        row.append(&close);
        wire_secondary_click(&open, &row_menu);
        body.append(&row);
        rows.push(SidebarRow {
            id: agent.id,
            root: row,
            open,
            close,
            dot,
            label,
            remote: row_remote,
            state,
        });
    }

    // The list is also where you make a window: an icon in the header is not
    // where anyone looks for "another terminal in this project".
    let new_terminal = gtk::Button::builder()
        .label("+  new terminal")
        .halign(gtk::Align::Fill)
        .css_classes(["flat", "new-terminal", "mono"])
        .build();
    if let Some(label) = new_terminal.child() {
        label.set_halign(gtk::Align::Start);
    }
    body.append(&new_terminal);

    SidebarCard {
        root,
        project: project.id,
        select,
        fold,
        fold_icon,
        new_terminal,
        count,
        attention,
        remote,
        body,
        windows: rows,
    }
}

impl SidebarCard {
    /// Repaint what changes often: fold, counts, selection, focus, states,
    /// per-window colour, and which terminals a browser is driving.
    pub fn refresh(
        &self,
        selected: bool,
        folded: bool,
        focused: Option<taix_core::AgentId>,
        windows: &[(taix_core::AgentId, &'static str, Option<String>, String)],
        attention: usize,
        remote: &[taix_core::AgentId],
    ) {
        if selected {
            self.root.add_css_class("selected");
        } else {
            self.root.remove_css_class("selected");
        }
        self.body.set_visible(!folded);
        self.fold_icon.set_icon_name(Some(if folded {
            "pan-end-symbolic"
        } else {
            "pan-down-symbolic"
        }));
        self.fold
            .set_tooltip_text(Some(if folded { "Unfold" } else { "Fold" }));
        // The count is what a folded project still has to tell you.
        self.count.set_text(&self.windows.len().to_string());
        self.attention.set_visible(attention > 0);
        if attention > 0 {
            self.attention.set_text(&attention.to_string());
        }
        // A driven terminal says so on its own row; a folded project says it
        // on the header, because that row is not on screen.
        self.remote
            .set_visible(folded && self.windows.iter().any(|row| remote.contains(&row.id)));
        for row in &self.windows {
            if let Some((_, state, color, name)) = windows.iter().find(|(id, ..)| *id == row.id) {
                row.dot.set_css_classes(&["dot", state]);
                row.state.set_text(state);
                row.state.set_css_classes(&["window-state", "mono", state]);
                row.label.set_text(name);
                let mut classes: Vec<&str> = vec!["window-line"];
                let tint = color.as_ref().map(|c| format!("tint-{c}"));
                if let Some(tint) = &tint {
                    classes.push(tint);
                }
                row.root.set_css_classes(&classes);
            }
            if focused == Some(row.id) {
                row.open.add_css_class("current");
            } else {
                row.open.remove_css_class("current");
            }
            row.remote.set_visible(remote.contains(&row.id));
        }
    }
}

/// Narrow the sidebar to windows whose name contains the filter, and hide a
/// project left with none. Case-insensitive; an empty filter shows all.
pub fn apply_filter(w: &Widgets, cards: &[SidebarCard]) {
    let needle = w.filter.text().to_lowercase();
    let total: usize = cards.iter().map(|c| c.windows.len()).sum();
    w.session_count.set_text(&format!(
        "{total} window{}",
        if total == 1 { "" } else { "s" }
    ));
    for card in cards {
        let mut shown = 0;
        for row in &card.windows {
            let hit = needle.is_empty() || row.label.text().to_lowercase().contains(&needle);
            row.root.set_visible(hit);
            shown += usize::from(hit);
        }
        card.root.set_visible(needle.is_empty() || shown > 0);
    }
}

pub fn set_sidebar(w: &Widgets, cards: &[SidebarCard]) {
    while let Some(child) = w.sidebar_list.first_child() {
        w.sidebar_list.remove(&child);
    }
    for card in cards {
        w.sidebar_list.append(&card.root);
    }
}

/// Ask for one line of text. Used for renames, so it seeds the current value
/// and selects it - the common case is replacing the whole thing.
///
/// Anchored on any widget, like [`confirm`]: a panel asking for a branch name
/// knows where it is without knowing who owns the window.
pub fn prompt_text(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    initial: &str,
    accept: &str,
    done: impl Fn(String) + 'static,
) {
    let entry = gtk::Entry::builder()
        .text(initial)
        .activates_default(true)
        .build();
    entry.select_region(0, -1);
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .extra_child(&entry)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", accept);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, {
        let entry = entry.clone();
        move |dialog, response| {
            if response == "ok" {
                let text = entry.text().trim().to_string();
                if !text.is_empty() {
                    done(text);
                }
            }
            dialog.close();
        }
    });
    dialog.present(Some(parent));
    entry.grab_focus();
}

/// Fill the bottom bar.
///
/// Which facts appear, and in what order, is `config.bar` — so the segments
/// are looked up by id here rather than assigned to named labels. Memory is
/// PSS: TaiX and tmux map the same libraries, so adding RSS would count them
/// twice and overstate the total.
pub fn set_bar(w: &Widgets, bar: &Bar<'_>) {
    let live_hex = css_hex(&w.probe_live);
    let total_hex = css_hex(&w.probe_total);
    for (id, label) in &w.bar {
        match id.as_str() {
            "where" => label.set_markup(&match (bar.project, bar.root) {
                (Some(name), Some(path)) => format!(
                    "<span foreground=\"{live_hex}\">{}</span>  {}",
                    glib::markup_escape_text(name),
                    glib::markup_escape_text(&taix_core::text::contract_home(path))
                ),
                (_, Some(path)) => {
                    glib::markup_escape_text(&taix_core::text::contract_home(path)).to_string()
                }
                _ => "no project".to_string(),
            }),
            "branch" => label.set_text(bar.branch.unwrap_or("—")),
            // The window with the keyboard, spelled out: which one, what is
            // running in it, what it is doing, and the grid tmux holds for
            // it. Nothing here fits in a pane header without eating output.
            "window" => match &bar.focus {
                Some(f) => {
                    let mut text = format!(
                        "<span foreground=\"{live_hex}\">{}</span> · {} · {}",
                        glib::markup_escape_text(f.name),
                        glib::markup_escape_text(f.harness),
                        f.state
                    );
                    if let Some((cols, rows)) = f.size {
                        text.push_str(&format!(" · {cols}×{rows}"));
                    }
                    if let Some(kib) = f.kib {
                        text.push_str(&format!(" · {}", taix_core::mem::human_kib(kib)));
                    }
                    if let Some(branch) = f.worktree {
                        text.push_str(&format!(" · {}", glib::markup_escape_text(branch)));
                    }
                    if f.scroll > 0 {
                        text.push_str(&format!(" · ▲{}", f.scroll));
                    }
                    label.set_markup(&text);
                }
                None => label.set_text("no window focused"),
            },
            // The live count is the one number on the bar that says the app
            // is doing something, so it gets the accent. Markup, because CSS
            // cannot colour part of a label.
            "windows" => label.set_markup(&format!(
                "{} windows · <span foreground=\"{live_hex}\">{} live</span> · {} attention",
                bar.windows, bar.live, bar.attention
            )),
            // Memory, and what is holding it: the server's version and pane
            // count are here because this is the segment that charges you
            // for the server. The three figures before the total are what
            // it is made of; `project` is the slice of the panes belonging
            // to what is on screen, and is left out when it is all of them.
            "memory" => {
                let total = bar.ui_kib + bar.tmux_kib + bar.panes_kib;
                let project = if bar.project_kib == bar.panes_kib {
                    String::new()
                } else {
                    format!("project {} · ", taix_core::mem::human_kib(bar.project_kib))
                };
                label.set_markup(&format!(
                    "ui {} · tmux {} {} · {} pane{} {} · {project}<span foreground=\"{total_hex}\">total {}</span>",
                    taix_core::mem::human_kib(bar.ui_kib),
                    glib::markup_escape_text(bar.tmux_version.unwrap_or("?")),
                    taix_core::mem::human_kib(bar.tmux_kib),
                    bar.panes,
                    if bar.panes == 1 { "" } else { "s" },
                    taix_core::mem::human_kib(bar.panes_kib),
                    taix_core::mem::human_kib(total),
                ))
            }
            "uptime" => label.set_text(&format!(
                "up {}",
                taix_core::text::human_secs(bar.uptime.as_secs())
            )),
            // `empty` is a spacer, and an id from a newer build is not worth
            // failing a launch over.
            _ => {}
        }
    }
}

/// The CSS `color` a widget resolved to, as `#rrggbb` for Pango markup.
fn css_hex(widget: &impl IsA<gtk::Widget>) -> String {
    let c = widget.color();
    format!(
        "#{:02x}{:02x}{:02x}",
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8
    )
}

/// Folder picker for adding a project. `done` runs on the main thread.
pub fn choose_project_folder(
    parent: &adw::ApplicationWindow,
    done: impl Fn(std::path::PathBuf) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title("Select a project folder")
        .modal(true)
        .build();
    dialog.select_folder(Some(parent), gtk::gio::Cancellable::NONE, move |result| {
        if let Some(path) = result.ok().and_then(|f| f.path()) {
            done(path);
        }
    });
}

/// Confirmation before stopping an agent, with the destructive worktree option
/// spelled out — removing it discards uncommitted work.
pub fn confirm_kill(
    parent: &adw::ApplicationWindow,
    agent: &str,
    has_worktree: bool,
    done: impl Fn(bool) + 'static,
) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading(format!("Stop {agent}?"))
        .body(if has_worktree {
            "Its process is killed. Its git worktree is kept unless you discard it."
        } else {
            "Its process is killed."
        })
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("stop", "Stop");
    dialog.set_response_appearance("stop", adw::ResponseAppearance::Suggested);
    if has_worktree {
        dialog.add_response("discard", "Stop and discard worktree");
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    }
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |dialog, response| {
        match response {
            "stop" => done(false),
            "discard" => done(true),
            _ => {}
        }
        dialog.close();
    });
    dialog.present();
}

/// Show a block of monospace markup: a diff, or the reason an action was
/// refused. Read-only and selectable, so it can be copied out.
pub fn show_text(parent: &adw::ApplicationWindow, heading: &str, markup: &str, wrap: bool) {
    // A diff must keep its columns, so it scrolls sideways; a sentence has to
    // wrap or it is simply cut off at the dialog edge.
    let label = gtk::Label::builder()
        .use_markup(true)
        .label(markup)
        .xalign(0.0)
        .yalign(0.0)
        .selectable(true)
        .wrap(wrap)
        .max_width_chars(if wrap { 72 } else { -1 })
        .css_classes(["pane"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&label)
        .propagate_natural_width(true)
        .min_content_width(700)
        .min_content_height(460)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .extra_child(&scroller)
        .build();
    dialog.add_response("close", "Close");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present(Some(parent));
}

/// Ask before an action that writes somewhere the user cannot simply undo.
///
/// Anchored on any widget rather than the window: a panel that has to ask
/// knows where it is, not who owns it.
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    accept: &str,
    done: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", accept);
    // Everything routed through here destroys something - discarding
    // changes, dropping a stash, trashing a file - so the accept button is
    // red, not the friendly green of a suggestion.
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |dialog, response| {
        if response == "ok" {
            done();
        }
        dialog.close();
    });
    dialog.present(Some(parent));
}

/// Light the header button whose panel is on screen. A class, not a toggle
/// state: see the buttons' own comment.
pub fn light(button: &gtk::Button, on: bool) {
    if on {
        button.add_css_class("on");
    } else {
        button.remove_css_class("on");
    }
}

pub fn toast(w: &Widgets, message: &str) {
    // No toast overlay: the status label is already the app's one-line
    // feedback channel, and errors here are rare and non-fatal.
    w.status.set_text(message);
    let status = w.status.clone();
    let restore = message.to_string();
    glib::timeout_add_seconds_local_once(6, move || {
        if status.text() == restore {
            status.set_text("");
        }
    });
}

/// Confirmation before forgetting a project. Its agents' panes are killed and
/// their records dropped; the repository itself is never touched.
pub fn confirm_remove_project(
    parent: &adw::ApplicationWindow,
    project: &str,
    agents: usize,
    done: impl Fn() + 'static,
) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading(format!("Remove {project}?"))
        .body(format!(
            "{agents} agent(s) will be stopped and forgotten. The repository on disk is not \
             modified and worktrees are left in place."
        ))
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |dialog, response| {
        if response == "remove" {
            done();
        }
        dialog.close();
    });
    dialog.present();
}
