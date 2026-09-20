//! Theming: a self-contained design that the desktop cannot repaint.
//!
//! GTK loads `$XDG_CONFIG_HOME/gtk-4.0/gtk.css` at `USER` priority, which is
//! *above* `APPLICATION`, so a themed desktop - pywal exports, "make GTK4
//! match my rice" snippets, a Comic Sans `*` rule - used to win every
//! selector TaiX set and the app came out hot pink. A dashboard whose colours
//! mean something (working, waiting, failed) cannot let that happen: the
//! stylesheet goes in *above* the desktop's, and every widget TaiX shows is
//! styled here rather than inherited from Adwaita.
//!
//! Escape hatches stay: `$XDG_CONFIG_HOME/taix/gtk.css` is layered above
//! everything, and `theme = "pywal"` opts back into the desktop's colours.

use gtk::gdk;

/// Above the desktop's `gtk.css`, below the user's TaiX override.
///
/// Both are offsets from `USER` rather than absolute numbers: the constant is
/// what GTK documents, and hardcoding 800 would rot.
const PRIORITY_TAIX: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER + 50;
const PRIORITY_PANE_FONT: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER + 60;
const PRIORITY_OVERRIDE: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER + 100;

/// Derived tokens and the design scale.
///
/// Palettes define the `@taix_*` colours; everything else - glass, hairlines,
/// hovers, rings - is mixed from them here, so a new palette is a page of
/// hex and not a stylesheet. "Glass" is the foreground at low alpha rather
/// than white, so a light palette darkens where a dark one lightens.
const TOKENS: &str = "
@define-color taix_surface     mix(@taix_bg, @taix_fg, 0.04);
@define-color taix_surface_hi  mix(@taix_bg, @taix_fg, 0.08);
@define-color taix_hairline    alpha(@taix_fg, 0.07);
@define-color taix_hairline_hi alpha(@taix_fg, 0.09);
@define-color taix_glass       alpha(@taix_fg, 0.028);
@define-color taix_glass_hi    alpha(@taix_fg, 0.045);
@define-color taix_hover       alpha(@taix_fg, 0.05);
@define-color taix_active      alpha(@taix_fg, 0.09);
@define-color taix_ring        alpha(@taix_accent, 0.35);
@define-color taix_accent_soft alpha(@taix_accent, 0.12);
@define-color taix_accent_edge alpha(@taix_accent, 0.45);

/* libadwaita draws dialogs, popovers, rows and entries from *named colours*,
   not from selectors - which is exactly how a themed desktop repaints an
   app: it redefines `window_bg_color` and every widget follows. Defining
   them here, above the desktop's sheet, is what makes the whole widget set
   follow the TaiX palette instead, including the parts no selector below
   reaches. GTK3-era names are included because plenty of rice snippets
   still set them. */
@define-color window_bg_color      @taix_bg;
@define-color window_fg_color      @taix_fg;
@define-color view_bg_color        @taix_pane_bg;
@define-color view_fg_color        @taix_fg;
@define-color dialog_bg_color      @taix_bg_alt;
@define-color dialog_fg_color      @taix_fg;
@define-color popover_bg_color     @taix_surface_hi;
@define-color popover_fg_color     @taix_fg;
@define-color headerbar_bg_color   @taix_bg_alt;
@define-color headerbar_fg_color   @taix_fg;
@define-color headerbar_border_color @taix_hairline;
@define-color headerbar_backdrop_color @taix_bg_alt;
@define-color headerbar_shade_color  transparent;
@define-color sidebar_bg_color     @taix_bg_alt;
@define-color sidebar_fg_color     @taix_fg;
@define-color sidebar_backdrop_color @taix_bg_alt;
@define-color sidebar_shade_color  @taix_hairline;
@define-color secondary_sidebar_bg_color @taix_bg_alt;
@define-color secondary_sidebar_fg_color @taix_fg;
@define-color card_bg_color        @taix_surface;
@define-color card_fg_color        @taix_fg;
@define-color card_shade_color     @taix_hairline;
@define-color shade_color          alpha(black, 0.36);
@define-color scrollbar_outline_color transparent;
@define-color borders              @taix_hairline;
@define-color accent_color         @taix_accent;
@define-color accent_bg_color      @taix_accent;
@define-color accent_fg_color      @taix_bg;
@define-color destructive_color    @taix_failed;
@define-color destructive_bg_color @taix_failed;
@define-color destructive_fg_color @taix_bg;
@define-color error_color          @taix_failed;
@define-color error_bg_color       @taix_failed;
@define-color error_fg_color       @taix_bg;
@define-color warning_color        @taix_waiting;
@define-color warning_bg_color     @taix_waiting;
@define-color warning_fg_color     @taix_bg;
@define-color success_color        @taix_done;
@define-color success_bg_color     @taix_done;
@define-color success_fg_color     @taix_bg;
@define-color theme_bg_color       @taix_bg;
@define-color theme_fg_color       @taix_fg;
@define-color theme_base_color     @taix_pane_bg;
@define-color theme_text_color     @taix_fg;
@define-color theme_selected_bg_color @taix_accent;
@define-color theme_selected_fg_color @taix_bg;
@define-color insensitive_bg_color @taix_bg_alt;
@define-color insensitive_fg_color @taix_dim;
";

/// Neutralise inherited chrome, then design it.
///
/// Deliberately unscoped. Scoping to `window.taix` looked tidier and left
/// every *other* surface the app puts on screen - menus, tooltips and
/// especially `AdwAlertDialog`, which is its own toplevel - painted by the
/// desktop: the confirmation dialog came out purple with hot pink buttons
/// while the window behind it was correct. This process only ever shows its
/// own widgets, so element selectors are the right scope, and the provider
/// priority is what makes them win.
const CHROME: &str = r#"
/* One font and one text colour for everything. The `*` is deliberate: a
   desktop stylesheet setting `* { font-family }` or `label { color }` is the
   single most common way a GTK app ends up in Comic Sans and canary yellow,
   and TaiX cannot let its own text be replaced. Classes that mean something
   - a state badge, a dim subtitle - are set later and win. Sizes are in px
   because the comp is in px; GTK px are logical, so HiDPI scales them. */
* {
  font-family: "IBM Plex Sans", "Inter", "Cantarell", sans-serif;
  font-weight: 400;
  color: @taix_fg;
  outline-style: none;
}
/* Most of the app's small text is monospace by design - session names,
   badges, the bar - so the class is set once and used everywhere. */
.mono { font-family: "JetBrains Mono", "Adwaita Mono", monospace; }

/* The window is a sheet of dark glass over three coloured glows. There is
   no backdrop blur in GTK, so the glows are painted straight onto the window
   and the frame is the 62% tint the comp puts over them - edge to edge: the
   comp's inset frame is a window inside a window once a compositor already
   draws one. */
window { font-size: 13px; background: @taix_bg; }
window.taix {
  background-image:
    radial-gradient(ellipse 1100px 700px at 12% -10%, @taix_glow_a 0%, alpha(@taix_glow_a, 0) 60%),
    radial-gradient(ellipse 900px 620px at 92% 8%, @taix_glow_b 0%, alpha(@taix_glow_b, 0) 62%),
    radial-gradient(ellipse 800px 800px at 60% 115%, @taix_glow_c 0%, alpha(@taix_glow_c, 0) 65%);
  background-color: @taix_void;
}
.taix-frame { background: alpha(@taix_bg, 0.62); }

/* Buttons are flat by default: this window is a dashboard, and a wall of
   raised rectangles competes with the pane content for attention. */
button,
menubutton > button,
splitbutton > button {
  background: transparent;
  background-image: none;
  border: 1px solid transparent;
  border-radius: 8px;
  box-shadow: none;
  color: @taix_fg;
  min-height: 24px;
  min-width: 24px;
  padding: 3px 8px;
  text-shadow: none;
}
button:hover, menubutton > button:hover, splitbutton > button:hover {
  background: @taix_hover;
}
button:active, button:checked, menubutton > button:checked {
  background: @taix_active;
}
button:disabled { color: alpha(@taix_fg, 0.35); background: transparent; }
/* A desktop `button:focus { background }` lit whichever list item GTK gave
   initial focus to, which read as a hover on an item nobody was over. */
.harness-item:focus:not(:hover), .icon-cell:focus:not(:hover) { background: transparent; }
/* The one raised button in the app is the one that commits, and dialogs are
   the only place it appears. */
button.suggested-action {
  background: @taix_accent;
  color: @taix_bg;
  font-weight: 600;
  padding: 5px 14px;
}
button.destructive-action {
  background: @taix_failed;
  color: @taix_bg;
  font-weight: 600;
  padding: 5px 14px;
}
button.suggested-action:hover { background: mix(@taix_accent, @taix_fg, 0.15); }
button.destructive-action:hover { background: mix(@taix_failed, @taix_fg, 0.15); }
/* The label is its own CSS node, so the `*` colour above lands on it and a
   filled button came out with light text on a light accent. */
button.suggested-action label, button.destructive-action label { color: @taix_bg; }

/* Keyboard focus has to be visible without the pointer, and Adwaita's ring
   is drawn in the theme's accent - which the desktop can change.

   GTK sets `:focus-visible` on the focused widget AND every ancestor up to
   the window, so a desktop `*:focus-visible { outline }` - a common rice
   line - drew a cyan box around the header, the popover, its contents and
   the window the moment a button was clicked. Outline is a property our
   sheet never set on those nodes, so nothing stopped it. Now every node
   owns its outline (the `*` rule above), and the ring is re-enabled only on
   the widget that actually has focus, at a specificity that beats the
   desktop's rule under either cascade order. */
*:focus-visible, *:focus:focus-visible { outline-style: none; }
button:focus:focus-visible, entry:focus:focus-visible, row:focus:focus-visible {
  outline: 2px solid @taix_ring;
  outline-offset: -2px;
}

/* Entries: sunk into the glass, one hairline, accent on focus. */
entry, entry.search {
  background: alpha(@taix_void, 0.28);
  background-image: none;
  border: 1px solid alpha(@taix_fg, 0.08);
  border-radius: 8px;
  box-shadow: none;
  color: @taix_fg;
  min-height: 30px;
  padding: 0 10px;
  caret-color: @taix_accent;
}
entry:focus-within { border-color: @taix_accent_edge; }
entry > text > placeholder { color: @taix_dim; }
entry selection, label selection { background: @taix_accent_soft; color: @taix_fg; }

/* The `popover` node is normally transparent; a desktop sheet that paints it
   black draws a square behind our rounded contents. */
popover { background: transparent; border: none; box-shadow: none; padding: 0; }
popover > contents, popover.menu > contents {
  background: @taix_surface_hi;
  border: 1px solid @taix_hairline_hi;
  border-radius: 10px;
  box-shadow: 0 12px 28px -14px alpha(black, 0.8);
  padding: 4px;
  color: @taix_fg;
}
popover > arrow { background: @taix_surface_hi; border: 1px solid @taix_hairline_hi; }
popover.menu modelbutton {
  border-radius: 6px;
  min-height: 26px;
  padding: 2px 8px;
  color: @taix_fg;
}
popover.menu modelbutton:hover { background: @taix_accent_soft; }
popover.menu separator { background: @taix_hairline; margin: 4px 2px; }
popover.menu label.title {
  color: @taix_dim;
  font-family: "JetBrains Mono", "Adwaita Mono", monospace;
  font-size: 9.5px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
}

tooltip {
  background: @taix_surface_hi;
  border: 1px solid @taix_hairline_hi;
  border-radius: 8px;
  color: @taix_fg;
}
tooltip label { font-size: 11px; color: @taix_fg; }

/* Dialogs: confirmations and the diff viewer. `AdwAlertDialog` is a separate
   toplevel, so nothing scoped to the main window reaches it. */
dialog, window.dialog, .alert-dialog, .dialog-bg, .osd {
  background: @taix_bg_alt;
  color: @taix_fg;
}
/* libadwaita 1.6 draws a floating dialog as `floating-sheet > sheet` with a
   white outline and an 18px radius; that outline was the one white line
   left in the app. */
floating-sheet > sheet, dialog-host > dialog.alert sheet {
  background: @taix_bg_alt;
  border-radius: 12px;
  outline: 1px solid @taix_hairline_hi;
  outline-offset: -1px;
  box-shadow: 0 24px 48px -16px alpha(black, 0.8);
}
/* The `dialog` node fills the whole window while it is up; libadwaita gives
   it the dialog background, which blacked the app out behind a preferences
   dialog (alerts carry their own rule). Only the sheet is a surface. */
dialog { background: transparent; }
/* Whatever hosts the sheet - `floating-sheet`, `bottom-sheet`, `dialog-host`
   - dims the window behind it; a desktop sheet painting `dimming` opaque
   blacks the app out behind every dialog. */
dimming, floating-sheet > dimming, dialog-host > dimming { background: alpha(@taix_void, 0.55); }
dialog-host > dialog > sheet, dialog-host > dialog sheet { background: @taix_bg_alt; }
/* An alert presented while the parent cannot host a sheet opens as its own
   toplevel, `window.dialog-window`. */
window.dialog-window { background: @taix_bg_alt; border-radius: 12px; }
.alert-dialog > contents, .dialog-bg > contents {
  background: @taix_bg_alt;
  border: 1px solid @taix_hairline_hi;
  border-radius: 12px;
  color: @taix_fg;
}
.alert-dialog label.title, dialog label.title, label.heading {
  color: @taix_fg;
  font-size: 15px;
  font-weight: 600;
}
.alert-dialog label.body, dialog label.body { color: @taix_dim; }

/* Scrollbars: 8px, no trough. The panes are the content. */
scrollbar, scrollbar trough { background: transparent; border: none; }
/* Adwaita gives the slider negative margins when the scrollbar is an
   overlay indicator, so a bare `min-width` here computed to -10 and GTK
   warned on every frame. */
scrollbar slider {
  background: mix(@taix_bg, @taix_fg, 0.12);
  border: none;
  border-radius: 4px;
  margin: 2px;
  min-width: 6px;
  min-height: 6px;
}
scrollbar slider:hover { background: mix(@taix_bg, @taix_fg, 0.22); }

listview, listbox, scrolledwindow, viewport, box, grid, stack, overlay, paned, windowhandle {
  background: transparent;
}
listbox > row { background: transparent; border-radius: 8px; padding: 2px 6px; }
listbox > row:selected { background: @taix_accent_soft; color: @taix_fg; }
separator { background: @taix_hairline; }
label.dim-label { color: @taix_dim; }
"#;

/// The application's own furniture: header, sidebar, panes, bar, overlays.
/// Every number here is the comp's; nothing is eyeballed.
const LAYOUT: &str = r#"
/* Header --------------------------------------------------------------- */

/* A strip of lighter glass with a gradient falling off downwards, not a
   titlebar. `WindowHandle` keeps it draggable. */
.taix-header {
  min-height: 46px;
  padding: 0 10px 0 12px;
  background: linear-gradient(alpha(@taix_fg, 0.075), alpha(@taix_fg, 0.02));
  border-bottom: 1px solid alpha(@taix_fg, 0.08);
  box-shadow: inset 0 1px 0 alpha(@taix_fg, 0.06);
}
.taix-header button.icon {
  min-width: 30px;
  min-height: 30px;
  padding: 0;
  color: @taix_dim;
  -gtk-icon-size: 15px;
}
.taix-header button.icon:hover { background: mix(@taix_bg, @taix_fg, 0.07); color: @taix_fg; }
/* A panel button that is showing its panel. `:checked` is not available:
   these are plain buttons, by design - see ui.rs. */
.taix-header button.icon.on { background: alpha(@taix_fg, 0.1); color: @taix_fg; }
.taix-header button.icon.danger:hover { background: alpha(@taix_failed, 0.14); color: @taix_failed; }
.taix-header button.icon.close-window { background: alpha(@taix_fg, 0.07); color: @taix_dim; }
.taix-header button.icon.close-window:hover { background: alpha(@taix_failed, 0.18); color: @taix_failed; }

/* The one tinted control: what you press to get another terminal. */
splitbutton.new-terminal { border-radius: 8px; }
splitbutton.new-terminal > button {
  min-height: 30px;
  padding: 0 12px;
  border-radius: 8px 0 0 8px;
  background: alpha(@taix_accent, 0.14);
  border: 1px solid alpha(@taix_accent, 0.30);
  border-right: none;
}
splitbutton.new-terminal > button label {
  font-size: 11px;
  color: mix(@taix_accent, @taix_fg, 0.25);
  font-weight: 500;
}
splitbutton.new-terminal > button image { color: mix(@taix_accent, @taix_fg, 0.25); -gtk-icon-size: 14px; }
splitbutton.new-terminal > button:hover { background: alpha(@taix_accent, 0.24); }
splitbutton.new-terminal > menubutton > button {
  min-height: 30px;
  min-width: 22px;
  padding: 0 4px;
  border-radius: 0 8px 8px 0;
  background: alpha(@taix_accent, 0.14);
  border: 1px solid alpha(@taix_accent, 0.30);
  border-left: none;
  color: @taix_dim;
  -gtk-icon-size: 10px;
}
splitbutton.new-terminal > menubutton > button:hover { background: alpha(@taix_accent, 0.24); color: @taix_fg; }
/* Add project wears the same coat in another hue: the two things you make
   are a project and a window in it, and blue against the accent teal says
   which is which without reading. Both labels are pinned to 11px here
   rather than left to inherit - the sizes drifted apart when one did. */
.taix-header button.add-project {
  min-height: 30px;
  padding: 0 12px;
  border-radius: 8px;
  background: alpha(@taix_tint_blue, 0.14);
  border: 1px solid alpha(@taix_tint_blue, 0.30);
}
.taix-header button.add-project label {
  font-size: 11px;
  color: mix(@taix_tint_blue, @taix_fg, 0.25);
  font-weight: 500;
}
.taix-header button.add-project image { color: mix(@taix_tint_blue, @taix_fg, 0.25); -gtk-icon-size: 14px; }
.taix-header button.add-project:hover { background: alpha(@taix_tint_blue, 0.24); }

.taix-title { font-size: 12.5px; font-weight: 600; letter-spacing: 0.14em; color: mix(@taix_fg, white, 0.4); }
.taix-subtitle { font-size: 9.5px; letter-spacing: 0.1em; color: mix(@taix_dim, @taix_bg, 0.15); }
/* Transient notices share the subtitle's slot: one line, quiet, gone soon. */
.taix-status { font-size: 10.5px; color: @taix_dim; }

/* Sidebar -------------------------------------------------------------- */

.taix-sidebar {
  background: @taix_glass;
  border-right: 1px solid @taix_hairline;
}
.taix-sidebar-top { padding: 13px 12px 9px; }
.taix-eyebrow {
  font-size: 9.5px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: @taix_dim;
}
.taix-eyebrow-dim { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.25); }
.taix-sidebar-top button.icon { min-width: 22px; min-height: 22px; padding: 0; border-radius: 6px; color: @taix_dim; -gtk-icon-size: 13px; }
.taix-sidebar-top button.icon:hover { background: alpha(@taix_fg, 0.08); color: @taix_fg; }
.taix-filter { font-size: 11px; }
.taix-filter image { color: @taix_dim; -gtk-icon-size: 11px; }
.taix-key { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.15); }
.taix-sidebar-list { padding: 2px 8px 10px; }

/* A project is a group: caret, folder, name over path, count. */
.project-card { background: transparent; border-radius: 7px; }
.project-head { padding: 6px 8px; border-radius: 7px; min-height: 0; }
.project-head:hover { background: alpha(@taix_fg, 0.05); }
.project-card.selected .project-head { background: alpha(@taix_fg, 0.035); }
/* Where a dragged project would land. A straight bar across the full width
   of the list, square-ended, with a short glow falling away from it into
   the card it would push aside - it reads as "between these two rows", not
   as an outline around one. The card drops its radius while the bar is up
   so the line cannot bend into the corner.

   The bar itself is deliberately NOT animated: it is a position, and a
   position that cross-fades from one edge to the other looks like a glitch
   when the pointer crosses a card's middle. Only the wash behind it fades,
   which is what makes the move read as smooth. */
.project-card {
  transition:
    background 110ms cubic-bezier(0.2, 0, 0.2, 1),
    opacity 140ms linear;
}
/* And kill the desktop theme's own drop highlight. Adwaita rings the whole
   drop target in the accent, rounded corners and all - the exact shape this
   bar replaces - and it flashed over the card whenever the pointer crossed
   into one. The bar rules below come later, so they still win. */
.project-card:drop(active),
.project-card:drop(active) > *,
.project-head:drop(active),
.taix-sidebar-list:drop(active) {
  box-shadow: none;
  outline: none;
  background-image: none;
}
.project-card.drop-above, .project-card.drop-below {
  border-radius: 0;
  background: linear-gradient(to bottom, alpha(@taix_accent, 0.1), transparent 70%);
}
.project-card.drop-below {
  background: linear-gradient(to top, alpha(@taix_accent, 0.1), transparent 70%);
}
.project-card.drop-above {
  box-shadow:
    inset 0 3px 0 @taix_accent,
    inset 0 10px 12px -10px alpha(@taix_accent, 0.6);
}
.project-card.drop-below {
  box-shadow:
    inset 0 -3px 0 @taix_accent,
    inset 0 -10px 12px -10px alpha(@taix_accent, 0.6);
}
/* The card being carried: still there, clearly lifted out of the list. */
.project-card.dragging { opacity: 0.3; }
.project-caret { color: mix(@taix_dim, @taix_fg, 0.45); -gtk-icon-size: 12px; }
.project-head:hover .project-caret { color: @taix_fg; }
.project-folder { color: mix(@taix_dim, @taix_fg, 0.35); -gtk-icon-size: 15px; }
.project-name { font-size: 11.5px; font-weight: 500; color: mix(@taix_fg, white, 0.3); }
.project-path { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.3); }
.count {
  font-size: 9.5px;
  color: mix(@taix_dim, @taix_fg, 0.3);
  background: alpha(@taix_fg, 0.07);
  border-radius: 6px;
  padding: 1px 6px;
}
.attention { font-size: 9.5px; color: @taix_waiting; font-weight: 600; }

/* Windows hang off a hairline under their project. */
.project-body {
  margin-left: 12px;
  padding-left: 8px;
  border-left: 1px solid @taix_hairline;
}
.window-line { border-radius: 8px; }
.window-row { padding: 7px 9px; min-height: 0; border-radius: 8px; }
.window-row:hover { background: alpha(@taix_fg, 0.05); }
.window-row label.window-name { font-size: 11.5px; color: mix(@taix_dim, @taix_fg, 0.45); }
/* The harness icon in a box, coloured by state. */
.dot {
  min-width: 11px;
  min-height: 11px;
  padding: 2px;
  border: 1.5px solid mix(@taix_bg, @taix_fg, 0.25);
  border-radius: 3px;
  color: mix(@taix_bg, @taix_fg, 0.25);
  -gtk-icon-size: 11px;
}
.dot.starting, .dot.working { border-color: mix(@taix_working, @taix_bg, 0.45); color: mix(@taix_working, @taix_bg, 0.45); }
.dot.waiting  { border-color: mix(@taix_waiting, @taix_bg, 0.45); color: mix(@taix_waiting, @taix_bg, 0.45); }
.dot.done     { border-color: mix(@taix_done, @taix_bg, 0.45);    color: mix(@taix_done, @taix_bg, 0.45); }
.dot.failed   { border-color: @taix_failed; color: @taix_failed; }
.window-state { font-size: 9px; letter-spacing: 0.06em; text-transform: uppercase; color: @taix_dim; }
.window-state.starting, .window-state.working { color: @taix_working; }
.window-state.waiting { color: @taix_waiting; }
.window-state.failed  { color: @taix_failed; }
.window-state.done    { color: @taix_done; }
.window-state.idle    { color: @taix_dim; }
/* The focused window is marked in the sidebar too: the pane ring is off
   screen when a project has more windows than fit. */
.window-row.current { background: alpha(@taix_accent, 0.12); box-shadow: inset 0 0 0 1px alpha(@taix_accent, 0.35); }
.window-row.current label.window-name { color: mix(@taix_fg, white, 0.5); }
.window-row.current .dot { border-color: @taix_accent; color: @taix_accent; }
.new-terminal { padding: 6px 9px; min-height: 0; border-radius: 8px; }
.new-terminal label { font-size: 11px; color: @taix_dim; }
.new-terminal:hover { background: alpha(@taix_fg, 0.05); }
.new-terminal:hover label { color: mix(@taix_dim, @taix_fg, 0.6); }
.fold { padding: 0; min-width: 12px; min-height: 12px; }

.taix-sidebar-foot {
  padding: 10px 14px;
  border-top: 1px solid @taix_hairline;
}
.taix-sidebar-foot label { font-size: 10px; color: mix(@taix_dim, @taix_bg, 0.15); }

/* Files ---------------------------------------------------------------- */

/* The mirror image of the sidebar: same glass, same hairline, hung on the
   other edge, so the window reads as one frame with two margins. */
.files-panel {
  background: @taix_glass;
  border-left: 1px solid @taix_hairline;
}
.files-head {
  padding: 11px 8px 9px 12px;
  border-bottom: 1px solid @taix_hairline;
}
.files-title {
  font-size: 9.5px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: @taix_dim;
}
.files-subtitle { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.3); }
.files-action {
  min-width: 22px;
  min-height: 22px;
  padding: 0;
  border-radius: 6px;
  color: @taix_dim;
  -gtk-icon-size: 13px;
}
.files-action:hover { background: alpha(@taix_fg, 0.08); color: @taix_fg; }
.files-action:checked { background: alpha(@taix_accent, 0.16); color: @taix_fg; }
.files-list { padding: 6px 6px 10px; }
/* The search sits between the head and the tree, one hairline from each.
   Explicit metrics: a stock GtkSearchEntry is 34px tall and pushes two rows
   of the tree off a short panel. */
.files-search-row {
  padding: 6px 8px 6px 10px;
  border-bottom: 1px solid @taix_hairline;
}
entry.files-search {
  min-height: 22px;
  padding: 0 4px;
  font-size: 10.5px;
  background: alpha(@taix_fg, 0.05);
  border: 1px solid transparent;
  border-radius: 6px;
  color: @taix_fg;
}
entry.files-search:focus-within {
  background: alpha(@taix_fg, 0.08);
  border-color: alpha(@taix_accent, 0.5);
}
.files-progress {
  font-size: 8.5px;
  letter-spacing: 0.04em;
  padding: 3px 10px 4px;
  color: mix(@taix_dim, @taix_bg, 0.25);
}

/* One row: caret, icon, name, when. The whole row is the target, and only
   the name grows - a date that moves as you scroll is unreadable. The row
   is a plain box, so its states are classes the panel sets: `selected` is
   the one the keys act on, `cut` is waiting for a paste. */
.file-row {
  padding: 3px 6px;
  min-height: 0;
  border-radius: 6px;
}
.file-row:hover { background: alpha(@taix_fg, 0.06); }
.file-row.selected { background: alpha(@taix_accent, 0.16); }
.file-row.selected:hover { background: alpha(@taix_accent, 0.22); }
.file-row:focus:focus-visible { outline: 2px solid @taix_ring; outline-offset: -2px; }
.file-row.cut { opacity: 0.45; }
.file-row.selected label.files-name,
.file-row.selected .files-caret { color: @taix_fg; }
.file-row.selected .files-when { color: @taix_dim; }
/* The rename entry sits exactly where the name was, one hairline around. */
.file-row-edit { padding: 1px 6px; }
.file-row-edit entry.files-edit {
  min-height: 20px;
  padding: 0 5px;
  font-size: 11px;
  border-radius: 4px;
}
.file-row label.files-name {
  font-size: 11px;
  color: mix(@taix_dim, @taix_fg, 0.55);
}
.file-row-dir label.files-name { color: mix(@taix_fg, white, 0.25); font-weight: 500; }
.file-row:hover label.files-name { color: @taix_fg; }
.files-when {
  font-size: 8.5px;
  letter-spacing: 0.04em;
  color: mix(@taix_dim, @taix_bg, 0.35);
}
.file-row:hover .files-when { color: @taix_dim; }
.files-caret { color: mix(@taix_dim, @taix_bg, 0.1); }
.file-row:hover .files-caret { color: @taix_fg; }
/* A folder wears the accent, a file the dim ink: the shape of the tree is
   readable before any name is. */
.file-dir { color: mix(@taix_accent, @taix_fg, 0.45); }
.file-icon { color: mix(@taix_dim, @taix_fg, 0.2); }
.files-more, .files-empty {
  font-size: 9.5px;
  color: mix(@taix_dim, @taix_bg, 0.3);
  padding: 4px 8px;
}

/* A result row: two lines, and a badge saying which of them matched. The
   badge carries the colour - accent for a name, the working green for a
   line of text - because a narrow panel has no room for a legend. */
.file-hit { padding: 4px 6px 5px; }
.files-why {
  font-size: 8px;
  letter-spacing: 0.06em;
  padding: 1px 4px;
  border-radius: 4px;
  background: alpha(@taix_fg, 0.07);
  color: @taix_dim;
}
.files-why.by-name { background: alpha(@taix_accent, 0.18); color: @taix_fg; }
.files-why.by-text { background: alpha(@taix_working, 0.16); color: mix(@taix_working, @taix_fg, 0.4); }
.files-hit-detail {
  font-size: 8.5px;
  padding-left: 22px;
  color: mix(@taix_dim, @taix_bg, 0.25);
}
.file-row:hover .files-hit-detail { color: @taix_dim; }

/* Pane header controls stay quiet until the header is under the pointer, so
   nine panes are names and states, not a wall of icons. */
.pane-close, .pane-menu {
  padding: 0 2px;
  min-height: 20px;
  min-width: 20px;
  color: @taix_dim;
  opacity: 0.0;
  -gtk-icon-size: 13px;
}
.agent-head:hover .pane-menu,
.agent-head:hover .pane-close { opacity: 1; }
.pane-menu:hover { color: @taix_fg; opacity: 1; }
.pane-close:hover { color: @taix_failed; opacity: 1; }
/* Sidebar rows carry no controls at all: a 262px column cannot spare 40px a
   row, and revealing them on hover reflowed the name under the pointer. The
   menu button still exists for right-click to anchor its popover on. */
.row-menu, .window-close {
  padding: 0;
  margin: 0;
  min-height: 0;
  min-width: 0;
  opacity: 0;
  -gtk-icon-size: 1px; /* 0 makes gtk_icon_theme_lookup assert on every paint */
}

/* Panes ---------------------------------------------------------------- */

.taix-grid { padding: 0; }
.pane {
  font-family: "JetBrains Mono", "Adwaita Mono", monospace;
  color: @taix_fg;
  padding: 12px 14px;
  /* The label is selectable so text can be copied, and a selectable label
     draws an insertion caret wherever it was last clicked. A terminal has
     exactly one insertion point, the block, and it is not where you clicked. */
  caret-color: transparent;
}
.pane selection { background: alpha(@taix_accent, 0.35); color: @taix_fg; }
/* Square, flush, and only a hairline between them: the panes are the
   content, and a tiled terminal wall reads better than floating cards. */
.agent-card {
  background: alpha(@taix_pane_bg, 0.55);
  border: none;
  border-radius: 0;
}
.agent-head {
  padding: 9px 12px;
  border-bottom: 1px solid @taix_hairline;
  background: @taix_glass_hi;
}
/* With no card border, focus is the header: a stronger tint and an accent
   line where the hairline was. A window tint (below) wins the background. */
.agent-card.focused .agent-head { background: alpha(@taix_accent, 0.12); border-bottom-color: @taix_accent_edge; }
.agent-dot { color: mix(@taix_bg, @taix_fg, 0.35); margin-right: 1px; -gtk-icon-size: 13px; }
.agent-dot.starting, .agent-dot.working { color: @taix_working; }
.agent-dot.waiting { color: @taix_waiting; }
.agent-dot.done    { color: @taix_done; }
.agent-dot.failed  { color: @taix_failed; }

/* The agent list under the split button: icon rows, tinted per harness. */
.harness-list { padding: 4px; min-width: 190px; }
.harness-item { padding: 5px 9px; min-height: 0; border-radius: 6px; }
.harness-item label { font-size: 11.5px; color: mix(@taix_fg, white, 0.2); }
.harness-item:hover { background: @taix_accent_soft; }
.harness-icon { color: mix(@taix_dim, @taix_fg, 0.5); }
.harness-icon.tint-red,    .harness-item.tint-red    .harness-icon { color: @taix_tint_red; }
.harness-icon.tint-orange, .harness-item.tint-orange .harness-icon { color: @taix_tint_orange; }
.harness-icon.tint-yellow, .harness-item.tint-yellow .harness-icon { color: @taix_tint_yellow; }
.harness-icon.tint-green,  .harness-item.tint-green  .harness-icon { color: @taix_tint_green; }
.harness-icon.tint-teal,   .harness-item.tint-teal   .harness-icon { color: @taix_tint_teal; }
.harness-icon.tint-blue,   .harness-item.tint-blue   .harness-icon { color: @taix_tint_blue; }
.harness-icon.tint-purple, .harness-item.tint-purple .harness-icon { color: @taix_tint_purple; }
.harness-icon.tint-pink,   .harness-item.tint-pink   .harness-icon { color: @taix_tint_pink; }

/* Settings: colour swatches and the icon grid. */
.swatch {
  min-width: 16px; min-height: 16px; padding: 0; margin: 0;
  border-radius: 999px; border: 2px solid transparent; background-clip: padding-box;
}
.swatch.tint-none   { background: transparent; border-color: mix(@taix_bg, @taix_fg, 0.3); }
.swatch.tint-red    { background: @taix_tint_red; }
.swatch.tint-orange { background: @taix_tint_orange; }
.swatch.tint-yellow { background: @taix_tint_yellow; }
.swatch.tint-green  { background: @taix_tint_green; }
.swatch.tint-teal   { background: @taix_tint_teal; }
.swatch.tint-blue   { background: @taix_tint_blue; }
.swatch.tint-purple { background: @taix_tint_purple; }
.swatch.tint-pink   { background: @taix_tint_pink; }
.swatch:hover { border-color: alpha(@taix_fg, 0.4); }
.swatch:checked { border-color: @taix_fg; }
.icon-pick { padding: 4px 6px; min-height: 0; }
.icon-grid { padding: 6px; }
.icon-cell { padding: 5px; min-width: 0; min-height: 0; border-radius: 6px; }
.icon-cell:hover { background: @taix_accent_soft; }
row.dim, row.dim .title, row.dim .subtitle { opacity: 0.55; }
.agent-name { font-size: 11.5px; font-weight: 500; color: mix(@taix_fg, white, 0.3); }
.agent-harness { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.2); }

/* Settings dialog ------------------------------------------------------ */

/* Two panes: a nav rail in the sidebar's own glass, and a page that scrolls
   under a fixed head. Every metric here is the comp's. */
.set-shell { background: @taix_bg_alt; }
.set-side { background: alpha(@taix_fg, 0.03); border-right: 1px solid @taix_hairline; }
.set-side-top { padding: 15px 14px 9px; }
.set-brand { font-size: 13px; font-weight: 600; color: mix(@taix_fg, white, 0.35); }
.set-where { font-size: 8.5px; color: mix(@taix_dim, @taix_bg, 0.25); }
.set-nav { padding: 6px 8px; }
.set-nav > row { padding: 7px 10px; border-radius: 8px; min-height: 0; }
.set-nav > row:hover { background: alpha(@taix_fg, 0.05); }
.set-nav > row:selected { background: alpha(@taix_fg, 0.07); }
.set-nav label { font-size: 11.5px; color: mix(@taix_dim, @taix_fg, 0.45); }
.set-nav > row:selected label { color: @taix_fg; }
.set-nav > row:selected label.count { color: mix(@taix_dim, @taix_fg, 0.5); }
/* The bullet is the selection: a full-width accent bar in a 196px rail
   shouted over the page it was pointing at. */
.set-dot { min-width: 5px; min-height: 5px; border-radius: 999px; background: mix(@taix_bg, @taix_fg, 0.28); }
.set-nav > row:selected .set-dot { background: @taix_accent; }
.set-foot { padding: 10px 14px; border-top: 1px solid @taix_hairline; }
.set-foot label { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.2); }
.set-foot .set-dot { background: @taix_done; }

.set-head { padding: 13px 14px 12px 18px; border-bottom: 1px solid @taix_hairline; }
.set-h1 { font-size: 14.5px; font-weight: 600; color: mix(@taix_fg, white, 0.3); }
.set-h2 { font-size: 10px; color: mix(@taix_dim, @taix_bg, 0.1); }
button.set-close {
  min-width: 26px; min-height: 26px; padding: 0;
  border-radius: 8px;
  background: alpha(@taix_fg, 0.07);
  color: @taix_dim;
}
button.set-close:hover { background: alpha(@taix_failed, 0.18); color: @taix_failed; }
entry.set-filter { min-height: 26px; font-size: 10.5px; }

.set-page { padding: 16px 18px 24px; }
.set-group-title {
  font-size: 9.5px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  font-weight: 600;
  color: mix(@taix_dim, @taix_bg, 0.05);
}
.set-group-count { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.35); }
.set-row { padding: 8px 2px; }
.set-row-title { font-size: 12px; color: mix(@taix_fg, white, 0.2); }
.set-row-sub { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.2); }
.set-note { font-size: 10px; color: mix(@taix_dim, @taix_bg, 0.2); }
.dim .set-row-title, .dim .set-card-name, .dim .set-card-meta { opacity: 0.55; }

/* Controls sit in the glass rather than in a box of their own: a row reads
   as label left, value right, with nothing drawn between them. */
.set-pick > button, .set-stepper, entry.set-entry {
  background: alpha(@taix_void, 0.3);
  border: 1px solid @taix_hairline_hi;
  border-radius: 8px;
}
.set-pick > button { padding: 3px 8px; min-height: 26px; }
.set-pick > button:hover { background: alpha(@taix_void, 0.16); border-color: @taix_accent_edge; }
.set-pick image { color: @taix_dim; -gtk-icon-size: 13px; }
.set-pick-label { font-size: 11px; color: mix(@taix_fg, white, 0.15); }
.set-pick-item { padding: 5px 8px; min-height: 0; border-radius: 6px; }
.set-pick-item label { font-size: 11.5px; }
.set-pick-item:hover { background: @taix_accent_soft; }
entry.set-entry {
  min-height: 26px;
  padding: 0 9px;
  font-size: 11px;
  font-family: "JetBrains Mono", "Adwaita Mono", monospace;
}
.set-stepper { padding: 1px; }
.set-stepper entry { background: transparent; border: none; min-height: 24px; padding: 0; font-size: 11px; }
.set-stepper button.set-step {
  min-width: 22px; min-height: 22px; padding: 0;
  border-radius: 6px;
  color: @taix_dim;
  font-size: 12px;
}
.set-stepper button.set-step:hover { background: alpha(@taix_fg, 0.09); color: @taix_fg; }
.set-unit { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.2); padding: 0 7px 0 2px; }
.set-chip { background: alpha(@taix_fg, 0.07); border-radius: 999px; padding: 1px 3px 1px 9px; }
.set-chip-label { font-size: 9.5px; color: mix(@taix_dim, @taix_fg, 0.55); }
button.set-chip-x {
  min-width: 15px; min-height: 15px; padding: 0;
  border-radius: 999px;
  color: mix(@taix_dim, @taix_bg, 0.1);
  -gtk-icon-size: 10px;
}
button.set-chip-x:hover { background: alpha(@taix_failed, 0.2); color: @taix_failed; }
/* Dashed, because it adds one rather than being one. */
.set-chip-add, .set-chip-add > button {
  border: 1px dashed mix(@taix_bg, @taix_fg, 0.22);
  border-radius: 999px;
  padding: 1px 9px;
  min-height: 19px;
}
.set-chip-add label { font-size: 9.5px; color: @taix_dim; }
.set-chip-add:hover label, .set-chip-add > button:hover label { color: @taix_fg; }

/* Agents: three counts, then a card per harness that opens into its own
   settings - nine harnesses expanded at once is a wall nobody reads. */
.set-stat {
  background: alpha(@taix_fg, 0.035);
  border: 1px solid @taix_hairline;
  border-radius: 10px;
  padding: 11px 13px;
}
.set-stat-n { font-size: 19px; font-weight: 600; color: mix(@taix_fg, white, 0.4); }
.set-stat-l { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.15); }
.set-card { background: alpha(@taix_fg, 0.035); border: 1px solid @taix_hairline; border-radius: 10px; }
button.set-card-head { padding: 9px 12px; border: none; border-radius: 10px; background: transparent; }
button.set-card-head:hover { background: alpha(@taix_fg, 0.04); }
.set-card-icon { min-width: 26px; min-height: 26px; border-radius: 7px; background: alpha(@taix_fg, 0.07); }
.set-card-icon image { margin: 5px; }
.set-card-name { font-size: 11.5px; font-weight: 500; color: mix(@taix_fg, white, 0.25); }
.set-card-meta { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.3); }
.set-card-chevron { color: mix(@taix_dim, @taix_bg, 0.1); -gtk-icon-size: 13px; }
.set-card-body { padding: 2px 12px 8px; border-top: 1px solid @taix_hairline; }
.set-card-foot { padding-top: 6px; }
button.set-tool { min-height: 22px; padding: 2px 9px; border-radius: 7px; color: @taix_dim; font-size: 10.5px; }
button.set-tool:hover { background: alpha(@taix_fg, 0.08); color: @taix_fg; }
button.set-tool.danger:hover { background: alpha(@taix_failed, 0.16); color: @taix_failed; }
button.set-tool image { -gtk-icon-size: 13px; }
/* State is a hairline pill in its own colour on its own dark tint. */
.badge {
  font-size: 9px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  padding: 2px 8px;
  border-radius: 999px;
  background: mix(@taix_pane_bg, @taix_fg, 0.06);
  border: 1px solid mix(@taix_pane_bg, @taix_fg, 0.12);
  color: mix(@taix_dim, @taix_fg, 0.25);
}
.badge.starting, .badge.working {
  background: mix(@taix_pane_bg, @taix_working, 0.12);
  border-color: mix(@taix_pane_bg, @taix_working, 0.25);
  color: @taix_working;
}
.badge.waiting {
  background: mix(@taix_pane_bg, @taix_waiting, 0.12);
  border-color: mix(@taix_pane_bg, @taix_waiting, 0.25);
  color: @taix_waiting;
}
/* "ready" is the settings dialog's word for an agent that is installed; it
   is the same green as a finished window, on purpose. */
.badge.done, .badge.ready {
  background: mix(@taix_pane_bg, @taix_done, 0.12);
  border-color: mix(@taix_pane_bg, @taix_done, 0.25);
  color: @taix_done;
}
.badge.failed {
  background: mix(@taix_pane_bg, @taix_failed, 0.14);
  border-color: mix(@taix_pane_bg, @taix_failed, 0.3);
  color: @taix_failed;
}

/* Automation ----------------------------------------------------------- */

/* The switch lives beside the card's head button rather than inside it, so
   flipping a job off does not also fold it open; the padding is the head
   button's, mirrored, or the switch would sit against the edge. */
.set-card-line { padding-right: 12px; }
.set-stats { padding-bottom: 2px; }
.run-row { padding: 5px 2px; }
.run-row:not(:last-child) { border-bottom: 1px solid alpha(@taix_fg, 0.04); }

/* Source control -------------------------------------------------------- */

/* Two tabs over one column: the panel is 280px, and two of these side by
   side would leave neither wide enough to read a path in. */
.panel-tabs { padding: 6px 8px; border-bottom: 1px solid @taix_hairline; }
button.panel-tab {
  min-height: 22px;
  padding: 2px 10px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: @taix_dim;
  font-size: 10.5px;
}
button.panel-tab:hover { background: alpha(@taix_fg, 0.05); color: @taix_fg; }
button.panel-tab:checked { background: alpha(@taix_fg, 0.09); color: @taix_fg; }

.git-panel { background: @taix_pane_bg; }
.git-head { padding: 9px 10px; border-bottom: 1px solid @taix_hairline; }
.git-project { font-size: 11px; font-weight: 500; color: mix(@taix_fg, white, 0.2); }
button.git-branch {
  min-height: 24px;
  padding: 1px 8px;
  border-radius: 7px;
  background: alpha(@taix_fg, 0.06);
  color: @taix_fg;
}
button.git-branch:hover { background: alpha(@taix_fg, 0.1); }
.git-branch-name { font-size: 10px; color: @taix_accent; }
.git-branch image { -gtk-icon-size: 12px; color: @taix_dim; }
.git-drift { font-size: 9.5px; color: @taix_waiting; }
button.git-tool {
  min-height: 22px;
  padding: 2px 6px;
  border: 1px solid @taix_hairline;
  border-radius: 7px;
  background: transparent;
  color: @taix_dim;
  font-size: 9.5px;
}
button.git-tool:hover { background: alpha(@taix_fg, 0.07); color: @taix_fg; }
button.git-tool:disabled { opacity: 0.45; }

/* Group headers repeat git's own vocabulary - staged, changed, untracked -
   rather than inventing kinder words for them. */
.git-group { padding: 9px 10px 3px; }
.git-group-title {
  font-size: 9px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  font-weight: 600;
  color: mix(@taix_dim, @taix_bg, 0.05);
}
.git-group-count { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.3); }
button.git-line-tool {
  min-height: 20px;
  min-width: 20px;
  padding: 0 6px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: @taix_dim;
  font-size: 9.5px;
}
button.git-line-tool:hover { background: alpha(@taix_fg, 0.08); color: @taix_fg; }
button.git-line-tool.danger:hover { background: alpha(@taix_failed, 0.16); color: @taix_failed; }
button.git-line-tool image { -gtk-icon-size: 12px; }
/* Row tools are there the moment you look at the row, and invisible the
   rest of the time - so a list of forty files is paths, not buttons. They
   keep their space, so nothing jumps when you arrive. Keyboard focus counts
   as looking. */
.git-file button.git-line-tool { opacity: 0; }
.git-file:hover button.git-line-tool,
.git-file:focus-within button.git-line-tool { opacity: 1; }
.git-file { padding: 0 6px 0 4px; }
.git-fold { font-size: 8px; color: @taix_dim; }
/* What git stopped in the middle of. Loud, because nothing else in the
   panel means what it usually means until it is finished. */
.git-banner {
  padding: 8px 10px;
  background: alpha(@taix_failed, 0.12);
  border-bottom: 1px solid alpha(@taix_failed, 0.35);
}
.git-banner-text { font-size: 10px; color: mix(@taix_fg, white, 0.1); }
.git-menu { padding: 4px; }
.git-menu .set-pick-item { min-height: 24px; padding: 4px 10px; }
/* Destructive entries are red wherever the list is drawn - the context
   menus and the branch popover's second page share one renderer. */
popover .set-pick-item.danger label { color: @taix_failed; }
popover separator { margin: 3px 4px; background: @taix_hairline; }
.git-commit-files { padding-left: 14px; }
.git-hint { font-size: 10.5px; color: mix(@taix_dim, @taix_bg, 0.15); }
button.git-file-head {
  padding: 3px 6px;
  border: none;
  border-radius: 6px;
  background: transparent;
}
button.git-file-head:hover { background: alpha(@taix_fg, 0.05); }
.git-path { font-size: 10px; color: mix(@taix_dim, @taix_fg, 0.55); }
/* The status letter carries the colour: a whole row tinted red reads as an
   error rather than as a deleted file. */
.git-letter { font-size: 10px; font-weight: 600; }
.git-letter.git-staged { color: @taix_done; }
.git-letter.git-dirty { color: @taix_waiting; }
.git-letter.git-new { color: @taix_accent; }
.git-letter.git-conflict { color: @taix_failed; }
.git-quiet { padding: 10px; font-size: 10px; color: mix(@taix_dim, @taix_bg, 0.2); }

.git-commit { padding: 10px; border-top: 1px solid @taix_hairline; }
.git-message-frame {
  border: 1px solid @taix_hairline_hi;
  border-radius: 8px;
  background: alpha(@taix_bg, 0.5);
}
.git-message, .git-message text { background: transparent; font-size: 10.5px; color: @taix_fg; }
.git-amend { font-size: 10px; color: @taix_dim; }
/* The commit control is a split button: the skin has to land on the buttons
   inside it, because the splitbutton node itself draws nothing and its two
   halves are not direct children. */
.git-commit-button { border: none; background: transparent; padding: 0; }
.git-commit-button button {
  min-height: 24px;
  padding: 2px 10px;
  border-radius: 7px;
  background: @taix_accent_soft;
  border: 1px solid @taix_accent_edge;
  color: @taix_fg;
  font-size: 10.5px;
}
.git-commit-button button:hover { background: alpha(@taix_accent, 0.22); }
.git-commit-button:disabled button { opacity: 0.4; }
button.git-tool.danger { color: @taix_failed; border-color: alpha(@taix_failed, 0.4); }
button.git-tool.danger:hover { background: alpha(@taix_failed, 0.16); }
button.git-tool.git-more { min-width: 22px; padding: 0 4px; }
.git-hash { font-size: 9.5px; color: @taix_accent; }
.git-when { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.3); }
/* Wide enough that a branch name is a name rather than `feat…nel`. */
.git-branch-menu { padding: 6px; min-width: 300px; }
.git-branch-menu .set-pick-item { min-height: 24px; }
.git-branch-here { font-size: 8px; color: @taix_accent; }

.pane-git { font-size: 9.5px; color: @taix_accent; }
.pane-scroll { font-size: 9.5px; color: @taix_waiting; font-weight: 500; }
.pane-mem { font-size: 9.5px; color: mix(@taix_dim, @taix_fg, 0.3); }

/* Where a dragged header will land: the half it takes, or the whole pane
   for a swap. An accent wash with an inset edge, so it reads over any output. */
.pane-drop {
  background: alpha(@taix_accent, 0.18);
  box-shadow: inset 0 0 0 2px @taix_accent;
}

/* Tab strip: horizontal, icon + name per tab, active marked, close on active.
   Same glass and hairline language as the pane header. */
.tab-strip {
  padding: 6px 8px;
  background: @taix_glass_hi;
  border-bottom: 1px solid @taix_hairline;
}
.tab-button {
  padding: 0;
  min-height: 0;
  border-radius: 6px;
}
.tab {
  padding: 5px 10px;
  border-radius: 6px;
  background: transparent;
  transition: background 110ms cubic-bezier(0.2, 0, 0.2, 1);
}
.tab label {
  font-size: 11px;
  color: mix(@taix_dim, @taix_fg, 0.45);
}
.tab image {
  color: mix(@taix_dim, @taix_fg, 0.35);
  -gtk-icon-size: 13px;
}
.tab-button:hover .tab {
  background: alpha(@taix_fg, 0.05);
}
.tab.active {
  background: alpha(@taix_fg, 0.08);
}
.tab.active label {
  color: mix(@taix_fg, white, 0.3);
  font-weight: 500;
}
.tab.active image {
  color: mix(@taix_fg, white, 0.3);
}
.tab-close {
  padding: 0 2px;
  min-height: 18px;
  min-width: 18px;
  color: @taix_dim;
  -gtk-icon-size: 12px;
}
.tab-close:hover {
  color: @taix_failed;
}

/* Dividers: a 1px hairline that still catches the pointer for a drag. The
   focused pane is marked by its header tint, not a border, so the divider is
   the only line between two panes. */
paned > separator {
  min-width: 1px;
  min-height: 1px;
  background-color: @taix_hairline;
  background-image: none;
  border-radius: 0;
}
paned > separator:hover { background-color: alpha(@taix_accent, 0.5); }
/* The sidebar boundary is a hairline the sidebar already draws. */
.taix-outer > separator { min-width: 1px; background-image: none; }

/* Per-window tints: a translucent wash of the colour over the pane header
   and the sidebar row. A wash reads as "this window is red" from across the
   room; a coloured edge read as a focus ring. The card border stays. */
.window-line.tint-red    > .window-row, .agent-card.tint-red    .agent-head { background: alpha(@taix_tint_red, 0.18); }
.window-line.tint-orange > .window-row, .agent-card.tint-orange .agent-head { background: alpha(@taix_tint_orange, 0.18); }
.window-line.tint-yellow > .window-row, .agent-card.tint-yellow .agent-head { background: alpha(@taix_tint_yellow, 0.18); }
.window-line.tint-green  > .window-row, .agent-card.tint-green  .agent-head { background: alpha(@taix_tint_green, 0.18); }
.window-line.tint-teal   > .window-row, .agent-card.tint-teal   .agent-head { background: alpha(@taix_tint_teal, 0.18); }
.window-line.tint-blue   > .window-row, .agent-card.tint-blue   .agent-head { background: alpha(@taix_tint_blue, 0.18); }
.window-line.tint-purple > .window-row, .agent-card.tint-purple .agent-head { background: alpha(@taix_tint_purple, 0.18); }
.window-line.tint-pink   > .window-row, .agent-card.tint-pink   .agent-head { background: alpha(@taix_tint_pink, 0.18); }

/* Bottom bar ----------------------------------------------------------- */

.statusbar {
  min-height: 30px;
  padding: 0 12px;
  border-top: 1px solid @taix_hairline;
  background: alpha(@taix_fg, 0.035);
}
/* Monospace, so the memory figures do not shuffle sideways every two
   seconds as their digits change width. */
.statusbar label { font-size: 10.5px; color: @taix_dim; }
.bar-where { color: mix(@taix_dim, @taix_fg, 0.3); }
.bar-branch { color: @taix_accent; }
.bar-live { color: @taix_accent; }
.bar-total { color: mix(@taix_dim, @taix_fg, 0.3); }
.bar-sep { color: mix(@taix_bg, @taix_fg, 0.14); }

/* The segment the monitor hangs off says so on hover, and nowhere else: a
   permanently highlighted bar segment reads as a warning. */
.bar-perf:hover { color: mix(@taix_dim, @taix_fg, 0.55); }

/* The web chip ----------------------------------------------------------

   A pill in the bar, beside the path: a globe, a state dot and the address
   a phone has to be pointed at. The dot carries the colour, not the whole
   pill - a bar segment painted teal reads as a warning, and this one is up
   almost all the time. */
.web-chip {
  min-height: 20px;
  padding: 1px 9px;
  margin: 3px 0;
  border-radius: 999px;
  background: @taix_glass;
  border: 1px solid @taix_hairline;
  box-shadow: none;
  transition:
    background 140ms cubic-bezier(0.2, 0, 0.2, 1),
    border-color 140ms cubic-bezier(0.2, 0, 0.2, 1);
}
.web-chip label { font-size: 10px; color: @taix_dim; }
.web-chip image { color: @taix_dim; -gtk-icon-size: 12px; }
.web-chip .web-dot { font-size: 7px; color: mix(@taix_bg, @taix_fg, 0.3); }
.web-chip:hover { background: @taix_glass_hi; border-color: @taix_hairline_hi; }
.web-chip:disabled { background: transparent; border-color: transparent; }

.web-chip.off label,
.web-chip.off image { color: mix(@taix_dim, @taix_bg, 0.35); }

.web-chip.starting .web-dot { color: @taix_waiting; }
.web-chip.starting label { color: mix(@taix_waiting, @taix_fg, 0.4); }
.web-chip.starting { border-color: alpha(@taix_waiting, 0.35); }

.web-chip.live .web-dot { color: @taix_working; }
.web-chip.live label { color: mix(@taix_dim, @taix_fg, 0.45); }
.web-chip.live image { color: mix(@taix_working, @taix_fg, 0.3); }

/* Somebody is watching: the pill lights, because that is a fact about who
   can see this screen. */
.web-chip.busy {
  background: alpha(@taix_accent, 0.1);
  border-color: alpha(@taix_accent, 0.3);
}
.web-chip.busy label { color: @taix_accent; }

/* Somebody is typing: this machine's keyboard is on hold, so it is said in
   words and in the colour everything else that wants you uses. */
.web-chip.held {
  background: alpha(@taix_waiting, 0.14);
  border-color: alpha(@taix_waiting, 0.45);
}
.web-chip.held label,
.web-chip.held image,
.web-chip.held .web-dot { color: @taix_waiting; }

.web-chip.error {
  background: alpha(@taix_failed, 0.12);
  border-color: alpha(@taix_failed, 0.4);
}
.web-chip.error label,
.web-chip.error image,
.web-chip.error .web-dot { color: @taix_failed; }

/* A device on the network asking to be let in. Amber, like everything
   else that wants the user, and scoped with `>` so the popover it opens
   does not inherit the pill's colour on its own labels. */
.pair-chip {
  min-height: 20px;
  padding: 1px 9px;
  margin: 3px 0 3px 6px;
  border-radius: 999px;
  background: alpha(@taix_waiting, 0.14);
  border: 1px solid alpha(@taix_waiting, 0.45);
  box-shadow: none;
  transition:
    background 140ms cubic-bezier(0.2, 0, 0.2, 1),
    border-color 140ms cubic-bezier(0.2, 0, 0.2, 1);
}
.pair-chip > box > label { font-size: 10px; color: @taix_waiting; }
.pair-chip > box > image { color: @taix_waiting; }
.pair-chip:hover { background: alpha(@taix_waiting, 0.2); border-color: alpha(@taix_waiting, 0.6); }

/* The card it opens: one question, one device, two answers.

   Every label here is qualified with its element: the popover hangs off a
   button inside `.statusbar`, and `.statusbar label` would otherwise win
   on specificity and paint the whole card dim. */
.pair-list { margin: 13px 15px; min-width: 250px; }
.pair-pop label.pair-head {
  font-size: 8.5px;
  font-weight: 700;
  letter-spacing: 0.1em;
  color: @taix_waiting;
  margin-bottom: 11px;
}
.pair-tile {
  min-width: 32px;
  min-height: 32px;
  border-radius: 10px;
  background: alpha(@taix_waiting, 0.12);
  border: 1px solid alpha(@taix_waiting, 0.28);
}
.pair-pop .pair-tile image { color: @taix_waiting; }
.pair-pop label.pair-dev { font-size: 12.5px; color: mix(@taix_fg, white, 0.15); }
.pair-pop label.pair-ip { font-size: 11px; color: mix(@taix_dim, @taix_fg, 0.25); }
.pair-pop label.pair-note {
  font-size: 9.5px;
  color: mix(@taix_dim, @taix_bg, 0.3);
  margin-top: 12px;
}
.pair-pop separator { margin: 12px 0; background: @taix_hairline; }

.pair-pop button.pair-yes,
.pair-pop button.pair-no {
  min-height: 27px;
  padding: 0 15px;
  border-radius: 8px;
}
.pair-pop button.pair-yes { background: @taix_accent; }
.pair-pop button.pair-yes label { font-size: 11px; font-weight: 700; color: @taix_bg; }
.pair-pop button.pair-yes:hover { background: mix(@taix_accent, @taix_fg, 0.15); }
.pair-pop button.pair-no { background: alpha(@taix_fg, 0.07); }
.pair-pop button.pair-no label { font-size: 11px; color: mix(@taix_dim, @taix_fg, 0.45); }
.pair-pop button.pair-no:hover { background: alpha(@taix_failed, 0.16); }
.pair-pop button.pair-no:hover label { color: @taix_failed; }

/* Settings device list: paired devices with forget buttons.
   Element-qualified to win over popover/dialog label rules, same as .pair-pop.
   No `margin: 0 auto` — GTK CSS rejects `auto` and logs a parser error. */
label.set-dev-ip {
  font-size: 11px;
  color: @taix_fg;
}
label.set-dev-who {
  font-size: 10px;
  color: @taix_dim;
}
button.set-dev-forget {
  min-height: 24px;
  padding: 0 10px;
  font-size: 10px;
}

/* A terminal somebody on the network is typing into. The same amber as
   every other "this wants you" state, on the window's row, on its folded
   project and on the pane itself, so one glance answers "which one". */
.row-remote, .pane-remote {
  font-size: 8.5px;
  font-weight: 600;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: @taix_waiting;
  background: alpha(@taix_waiting, 0.14);
  border: 1px solid alpha(@taix_waiting, 0.4);
  border-radius: 999px;
  padding: 0px 5px;
}

/* Perf monitor ---------------------------------------------------------- */

/* Same glass, hairlines and eyebrow as the sidebar and the file tree: it is
   another panel, so it should not look like a tooltip. */
.perf { padding: 0; }
.perf-head {
  padding: 9px 12px 7px;
  border-bottom: 1px solid @taix_hairline;
}
.perf-title {
  font-size: 9.5px;
  letter-spacing: 0.16em;
  color: @taix_dim;
  font-weight: 600;
}
.perf-uptime { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.3); }
.perf-list { padding: 6px; }
.perf-row { padding: 5px 6px; border-radius: 7px; }
.perf-row:hover { background: alpha(@taix_fg, 0.05); }
/* The project on screen, marked the way the sidebar marks it. */
.perf-row-on { background: alpha(@taix_accent, 0.09); }
.perf-name { font-size: 11.5px; font-weight: 500; color: @taix_fg; }
.perf-row-on .perf-name { color: mix(@taix_accent, @taix_fg, 0.35); }
.perf-detail { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.25); }
.perf-value { font-size: 10px; }
.perf-empty { font-size: 9.5px; color: mix(@taix_dim, @taix_bg, 0.3); padding: 4px 6px; }

/* Number and meter share a colour, which is what says which meter belongs
   to which number: cpu is the accent, memory the cooler ink. */
label.perf-cpu { color: mix(@taix_accent, @taix_fg, 0.3); }
label.perf-mem { color: mix(@taix_dim, @taix_fg, 0.45); }
.perf-meter {
  min-height: 3px;
  padding: 0;
}
.perf-meter trough {
  min-height: 3px;
  border: none;
  border-radius: 2px;
  background: alpha(@taix_fg, 0.1);
}
.perf-meter progress {
  min-height: 3px;
  border: none;
  border-radius: 2px;
}
.perf-meter.perf-cpu progress { background: @taix_accent; }
.perf-meter.perf-mem progress { background: mix(@taix_dim, @taix_fg, 0.5); }

.perf-foot {
  padding: 8px 12px 9px;
  border-top: 1px solid @taix_hairline;
}
.perf-cost { font-size: 10px; color: mix(@taix_dim, @taix_fg, 0.3); }
.perf-facts { font-size: 9px; color: mix(@taix_dim, @taix_bg, 0.25); }

/* Empty state ---------------------------------------------------------- */

statuspage .icon { color: @taix_dim; -gtk-icon-size: 48px; }
statuspage .title { font-size: 15px; font-weight: 600; color: @taix_fg; }
statuspage .description { font-size: 12px; color: @taix_dim; }

/* A window with no pane: its last screen dimmed under the harness's mark.
   The wash is the pane background at high alpha, so the history is still
   legible as history and the mark reads as the one thing to click. */
.pane-start { background: alpha(@taix_pane_bg, 0.82); }
.pane-start:hover { background: alpha(@taix_pane_bg, 0.9); }
.pane-start-icon { color: @taix_dim; -gtk-icon-size: 48px; }
.pane-start:hover .pane-start-icon { color: @taix_fg; }
.pane-start-label { font-size: 12px; color: @taix_dim; }
.pane-start:hover .pane-start-label { color: @taix_fg; }

/* "↓ live", floating over the bottom of a pane that is scrolled back. A
   glass pill in the accent, lit from its own edge: dark enough that the
   history behind it still reads, bright enough to be the one thing to
   click. The ring breathes while it waits and locks solid under the
   pointer, so the offer is visible without being chrome. */
.pane-jump {
  margin: 0 14px 12px 0;
  padding: 4px 13px 5px;
  min-height: 0;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.3px;
  color: mix(@taix_accent, @taix_fg, 0.35);
  border: 1px solid alpha(@taix_accent, 0.5);
  background-color: alpha(@taix_bg, 0.72);
  background-image: linear-gradient(
    to bottom,
    alpha(@taix_accent, 0.22),
    alpha(@taix_accent, 0.08)
  );
  box-shadow:
    inset 0 1px 0 alpha(@taix_accent, 0.35),
    0 0 10px alpha(@taix_accent, 0.28),
    0 3px 10px alpha(black, 0.45);
  transition: all 180ms cubic-bezier(0.2, 0, 0.2, 1);
  animation: pane-jump-breathe 2.6s ease-in-out infinite;
}
/* The label is its own node, so the `*` colour above lands on it and the
   pill came out with light text on a light accent - see the filled buttons
   near the top of this sheet. */
.pane-jump label { color: mix(@taix_accent, @taix_fg, 0.45); }
.pane-jump:hover label, .pane-jump:active label { color: @taix_bg; }
@keyframes pane-jump-breathe {
  from { box-shadow:
    inset 0 1px 0 alpha(@taix_accent, 0.35),
    0 0 8px alpha(@taix_accent, 0.22),
    0 3px 10px alpha(black, 0.45); }
  50% { box-shadow:
    inset 0 1px 0 alpha(@taix_accent, 0.45),
    0 0 18px alpha(@taix_accent, 0.5),
    0 3px 10px alpha(black, 0.45); }
  to { box-shadow:
    inset 0 1px 0 alpha(@taix_accent, 0.35),
    0 0 8px alpha(@taix_accent, 0.22),
    0 3px 10px alpha(black, 0.45); }
}
.pane-jump:hover {
  animation: none;
  color: @taix_bg;
  border-color: mix(@taix_accent, @taix_fg, 0.3);
  background-color: @taix_accent;
  background-image: linear-gradient(
    to bottom,
    mix(@taix_accent, @taix_fg, 0.22),
    @taix_accent
  );
  box-shadow:
    inset 0 1px 0 alpha(white, 0.25),
    0 0 22px alpha(@taix_accent, 0.65),
    0 4px 14px alpha(black, 0.5);
}
.pane-jump:active {
  animation: none;
  color: @taix_bg;
  background-image: linear-gradient(to bottom, @taix_accent, mix(@taix_accent, @taix_bg, 0.2));
  box-shadow:
    inset 0 2px 5px alpha(black, 0.3),
    0 0 12px alpha(@taix_accent, 0.45);
}

/* Find bar: one row under the panes, in the pane's own font so a match reads
   like the text it came from. */
.taix-find-bar {
  padding: 5px 8px;
  border-top: 1px solid @taix_hairline;
  background: alpha(@taix_fg, 0.035);
}
.taix-find-entry { font-family: "JetBrains Mono", "Adwaita Mono", monospace; font-size: 12px; }
.taix-find-count {
  font-size: 10.5px;
  color: @taix_dim;
  font-family: "JetBrains Mono", "Adwaita Mono", monospace;
  min-width: 60px;
}

/* Palette: a list you type into. Groups are labels, not headings, so ten
   results stay one glance. */
.taix-palette-container { padding: 8px; }
.taix-palette-search { margin-bottom: 8px; }
.taix-palette-list { background: transparent; }
.taix-palette-group {
  font-family: "JetBrains Mono", "Adwaita Mono", monospace;
  font-size: 9.5px;
  letter-spacing: 0.16em;
  color: @taix_dim;
  text-transform: uppercase;
  padding: 6px 6px 2px 6px;
}
.taix-palette-title { color: @taix_fg; font-size: 13px; }
.taix-palette-subtitle {
  font-size: 10.5px;
  color: @taix_dim;
  font-family: "JetBrains Mono", "Adwaita Mono", monospace;
}
"#;

const TAIX: &str = include_str!("../themes/taix.css");
const MOCHA: &str = include_str!("../themes/catppuccin-mocha.css");
const LATTE: &str = include_str!("../themes/catppuccin-latte.css");

/// Whether a palette is dark, so libadwaita can be told rather than asked.
fn is_dark(name: Option<&str>) -> bool {
    !matches!(name, Some("catppuccin-latte" | "latte" | "light"))
}

fn palette(name: Option<&str>) -> &'static str {
    match name {
        Some("catppuccin-mocha" | "mocha") => MOCHA,
        _ if !is_dark(name) => LATTE,
        _ => TAIX,
    }
}

/// The palette CSS currently installed, kept for the one surface GTK cannot
/// paint: the browser's start page is a web document, so it needs the hexes
/// rather than the `@taix_*` names.
fn active() -> &'static std::thread::LocalKey<std::cell::RefCell<String>> {
    thread_local! {
        static ACTIVE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    }
    &ACTIVE
}

/// A `@taix_*` colour of the installed palette as a hex string.
pub fn color(token: &str, fallback: &str) -> String {
    active()
        .with(|a| hex(&a.borrow(), token))
        .unwrap_or_else(|| fallback.to_string())
}

/// Three colours of a palette that is not necessarily the installed one:
/// the swatches a picker shows beside its name. pywal has no sheet until it
/// is installed, so it previews whatever is on screen now.
pub fn preview(theme: &str) -> [String; 3] {
    const TOKENS: [&str; 3] = ["taix_accent", "taix_tint_blue", "taix_tint_yellow"];
    const FALLBACK: [&str; 3] = ["#7ad6bd", "#6fb2ff", "#e0b46c"];
    if theme == "pywal" {
        return std::array::from_fn(|i| color(TOKENS[i], FALLBACK[i]));
    }
    let css = palette(Some(theme));
    std::array::from_fn(|i| hex(css, TOKENS[i]).unwrap_or_else(|| FALLBACK[i].to_string()))
}

/// Last definition wins, as in CSS: pywal is layered over the base palette.
fn hex(css: &str, token: &str) -> Option<String> {
    css.lines().rev().find_map(|line| {
        let (name, value) = line
            .trim()
            .strip_prefix("@define-color ")?
            .split_once(' ')?;
        let value = value.trim().trim_end_matches(';').trim();
        (name == token && value.starts_with('#')).then(|| value.to_string())
    })
}

/// Install the stylesheet stack. Call once, after the display exists.
///
/// Returns the paths that were layered on top of the built-in palette, for
/// the startup log — silent theming failures are miserable to debug.
pub fn install(theme: Option<&str>, font_points: f64) -> Vec<String> {
    let display = gdk::Display::default().expect("no display");
    let mut loaded = Vec::new();

    // libadwaita reads the desktop's light/dark preference and its accent
    // colour. TaiX ships its own palette, and a half-applied system
    // preference is what makes an app look broken rather than themed.
    let manager = adw::StyleManager::default();
    manager.set_color_scheme(if is_dark(theme) {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });

    for sheet in [palette(theme), TOKENS, CHROME, LAYOUT] {
        add(&display, sheet, PRIORITY_TAIX);
    }
    // Only the sheets that depend on the config are tracked; the font
    // provider is replaced in place by `set_pane_font`.
    gtk::style_context_add_provider_for_display(&display, &font_provider(), PRIORITY_PANE_FONT);
    set_pane_font(font_points);

    // pywal is opt-in now. Importing it whenever the cache existed meant a
    // ricing setup silently recoloured a dashboard whose colours carry
    // meaning, and "why is TaiX purple" had no visible cause.
    if theme == Some("pywal")
        && let Some(wal) = pywal_path().filter(|p| p.exists())
    {
        let css = std::fs::read_to_string(&wal).unwrap_or_default();
        if let Some(mapped) = map_pywal(&css) {
            add(&display, &mapped, PRIORITY_TAIX + 1);
            loaded.push(wal.display().to_string());
        }
    }

    if let Some(user) = user_css_path().filter(|p| p.exists()) {
        let provider = gtk::CssProvider::new();
        provider.load_from_path(&user);
        gtk::style_context_add_provider_for_display(&display, &provider, PRIORITY_OVERRIDE);
        loaded.push(user.display().to_string());
    }
    loaded
}

/// Pane font size lives in its own provider so zooming replaces one rule
/// instead of reloading the whole stylesheet stack.
fn font_provider() -> gtk::CssProvider {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }
    PROVIDER.with(|p| p.clone())
}

pub fn set_pane_font(points: f64) {
    // Family stays with the `.pane` rule in LAYOUT; only the size lives here.
    font_provider().load_from_string(&format!(".pane {{ font-size: {points:.1}pt; }}"));
}

/// The providers the config chose, so that changing it can take them out
/// again instead of piling a second palette on top of the first.
fn themed() -> &'static std::thread::LocalKey<std::cell::RefCell<Vec<gtk::CssProvider>>> {
    thread_local! {
        static THEMED: std::cell::RefCell<Vec<gtk::CssProvider>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    &THEMED
}

fn add(display: &gdk::Display, css: &str, priority: u32) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    gtk::style_context_add_provider_for_display(display, &provider, priority);
    themed().with(|t| t.borrow_mut().push(provider));
    active().with(|a| a.borrow_mut().push_str(css));
}

/// Swap the palette for another at runtime. The pane font is left alone:
/// it is zoom state, and `set_pane_font` owns it.
pub fn reinstall(theme: Option<&str>) {
    let display = gdk::Display::default().expect("no display");
    let old: Vec<gtk::CssProvider> = themed().with(|t| std::mem::take(&mut *t.borrow_mut()));
    for provider in old {
        gtk::style_context_remove_provider_for_display(&display, &provider);
    }
    active().with(|a| a.borrow_mut().clear());
    let manager = adw::StyleManager::default();
    manager.set_color_scheme(if is_dark(theme) {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });
    for sheet in [palette(theme), TOKENS, CHROME, LAYOUT] {
        add(&display, sheet, PRIORITY_TAIX);
    }
    if theme == Some("pywal")
        && let Some(wal) = pywal_path().filter(|p| p.exists())
        && let Some(mapped) = map_pywal(&std::fs::read_to_string(&wal).unwrap_or_default())
    {
        add(&display, &mapped, PRIORITY_TAIX + 1);
    }
}

fn user_css_path() -> Option<std::path::PathBuf> {
    Some(config_dir()?.join("taix/gtk.css"))
}

fn pywal_path() -> Option<std::path::PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".cache")))?;
    Some(cache.join("wal/colors.css"))
}

fn config_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config")))
}

/// Translate pywal's `--colorN`/`@define-color colorN` names onto TaiX tokens.
///
/// Returns `None` when the file yields nothing usable, so a stale or empty
/// cache does not blank the UI by defining colours to garbage.
fn map_pywal(css: &str) -> Option<String> {
    let mut colors = std::collections::HashMap::new();
    for line in css.lines() {
        let line = line.trim();
        let rest = line.strip_prefix("@define-color ").unwrap_or(line);
        if let Some((name, value)) = rest.split_once(char::is_whitespace) {
            let value = value.trim().trim_end_matches(';').trim();
            if value.starts_with('#') {
                colors.insert(name.trim().to_string(), value.to_string());
            }
        }
    }
    let get = |k: &str| colors.get(k).cloned();
    let bg = get("background").or_else(|| get("color0"))?;
    let fg = get("foreground").or_else(|| get("color7"))?;
    Some(format!(
        "@define-color taix_bg {bg};\n\
         @define-color taix_bg_alt {bg};\n\
         @define-color taix_pane_bg {bg};\n\
         @define-color taix_fg {fg};\n\
         @define-color taix_border {};\n\
         @define-color taix_accent {};\n\
         @define-color taix_working {};\n\
         @define-color taix_waiting {};\n\
         @define-color taix_failed {};\n\
         @define-color taix_done {};\n",
        get("color8").unwrap_or_else(|| fg.clone()),
        get("color4").unwrap_or_else(|| fg.clone()),
        get("color2").unwrap_or_else(|| fg.clone()),
        get("color3").unwrap_or_else(|| fg.clone()),
        get("color1").unwrap_or_else(|| fg.clone()),
        get("color6").unwrap_or_else(|| fg.clone()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_hexes_read_back_with_the_last_definition_winning() {
        // The web view cannot resolve `@taix_*`, so the start page reads
        // hexes out of the installed sheets - where pywal is layered over
        // the base palette and `taix_bg` must not match `taix_bg_alt`.
        let css = "@define-color taix_bg #0e1115;\n\
                   @define-color taix_bg_alt #12161a;\n\
                   @define-color taix_bg #202020;\n";
        assert_eq!(hex(css, "taix_bg").as_deref(), Some("#202020"));
        assert_eq!(hex(css, "taix_bg_alt").as_deref(), Some("#12161a"));
        assert_eq!(hex(TAIX, "taix_accent").as_deref(), Some("#7ad6bd"));
        // Derived tokens are mixes, not colours a browser can parse.
        assert_eq!(hex(TOKENS, "taix_surface"), None);
    }

    #[test]
    fn pywal_colours_map_onto_taix_tokens() {
        let css = "@define-color background #101010;\n\
                   @define-color foreground #eeeeee;\n\
                   @define-color color1 #ff0000;\n";
        let out = map_pywal(css).expect("mapped");
        assert!(out.contains("@define-color taix_bg #101010;"));
        assert!(out.contains("@define-color taix_fg #eeeeee;"));
        assert!(out.contains("@define-color taix_failed #ff0000;"));
    }

    #[test]
    fn unusable_pywal_cache_is_ignored() {
        // A stale/empty cache must not define tokens to nothing, which would
        // blank the UI rather than leave the shipped palette alone.
        assert!(map_pywal("").is_none());
        assert!(map_pywal("@define-color color9 #123456;").is_none());
    }

    // The bug this file exists to prevent: GTK loads ~/.config/gtk-4.0/gtk.css
    // at USER priority, so anything at APPLICATION priority loses and the app
    // comes out in the desktop's colours. The user's own TaiX override still
    // has the last word. Checked when the file compiles, not when tests run.
    const _: () = {
        assert!(PRIORITY_TAIX > gtk::STYLE_PROVIDER_PRIORITY_USER);
        assert!(PRIORITY_TAIX > gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        assert!(PRIORITY_PANE_FONT > PRIORITY_TAIX);
        assert!(PRIORITY_OVERRIDE > PRIORITY_PANE_FONT);
    };

    #[test]
    fn every_palette_defines_what_the_layout_references() {
        // A palette missing a token renders that property as transparent, and
        // the failure looks like a layout bug rather than a missing colour.
        let referenced: std::collections::HashSet<&str> = [TOKENS, CHROME, LAYOUT]
            .iter()
            .flat_map(|sheet| sheet.split("@taix_").skip(1))
            .map(|rest| {
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                &rest[..end]
            })
            .collect();
        let derived: std::collections::HashSet<&str> = TOKENS
            .split("@define-color taix_")
            .skip(1)
            .map(|rest| &rest[..rest.find(' ').unwrap_or(rest.len())])
            .collect();
        for sheet in [TAIX, MOCHA, LATTE] {
            for token in &referenced {
                if derived.contains(token) {
                    continue;
                }
                assert!(
                    sheet.contains(&format!("@define-color taix_{token} ")),
                    "palette is missing taix_{token}"
                );
            }
        }
    }
}
