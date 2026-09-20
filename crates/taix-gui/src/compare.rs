//! Side-by-side comparison of two panes' output.

use adw::prelude::*;

pub struct Pair {
    pub left: String,
    pub right: String,
}

/// Line-level diff of two plain-text captures, as two Pango markup blocks
/// aligned line-for-line so the two labels scroll together.
///
/// ponytail: LCS over lines, capped at 400 lines. This runs on live panes;
/// the algorithm is O(n*m) and agent output can be long.
pub fn diff_markup(left: &str, right: &str) -> Pair {
    let rows = taix_core::diff::align(left, right);
    let mut left_out = String::new();
    let mut right_out = String::new();

    for row in rows {
        match row.side {
            taix_core::diff::Side::Same => {
                escape_into(&mut left_out, &row.left);
                left_out.push('\n');
                escape_into(&mut right_out, &row.right);
                right_out.push('\n');
            }
            taix_core::diff::Side::Left => {
                left_out.push_str("<span foreground=\"#f38ba8\">");
                escape_into(&mut left_out, &row.left);
                left_out.push_str("</span>\n");
                right_out.push('\n');
            }
            taix_core::diff::Side::Right => {
                left_out.push('\n');
                right_out.push_str("<span foreground=\"#a6e3a1\">");
                escape_into(&mut right_out, &row.right);
                right_out.push_str("</span>\n");
            }
        }
    }

    Pair {
        left: left_out,
        right: right_out,
    }
}

/// Pane text is arbitrary agent output; unescaped `<` or `&` would make Pango
/// reject the whole label. Reused from pango.rs.
fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\0' => out.push(' '),
            c => out.push(c),
        }
    }
}

/// Open a side-by-side comparison dialog.
pub fn open(parent: &adw::ApplicationWindow, left_title: &str, right_title: &str, pair: &Pair) {
    let dialog = adw::Dialog::new();
    dialog.set_title("Compare Panes");
    dialog.set_content_width(1000);
    dialog.set_content_height(600);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.set_homogeneous(true);
    header.set_margin_start(12);
    header.set_margin_end(12);
    header.set_margin_top(12);

    let left_label = gtk::Label::new(Some(left_title));
    left_label.add_css_class("heading");
    left_label.set_halign(gtk::Align::Start);
    header.append(&left_label);

    let right_label = gtk::Label::new(Some(right_title));
    right_label.add_css_class("heading");
    right_label.set_halign(gtk::Align::Start);
    header.append(&right_label);

    let content_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content_box.set_homogeneous(true);
    content_box.set_margin_start(12);
    content_box.set_margin_end(12);
    content_box.set_margin_bottom(12);

    let left_pane = gtk::Label::new(None);
    left_pane.set_markup(&pair.left);
    left_pane.set_xalign(0.0);
    left_pane.set_yalign(0.0);
    left_pane.set_selectable(true);
    left_pane.add_css_class("pane");

    let right_pane = gtk::Label::new(None);
    right_pane.set_markup(&pair.right);
    right_pane.set_xalign(0.0);
    right_pane.set_yalign(0.0);
    right_pane.set_selectable(true);
    right_pane.add_css_class("pane");

    let left_scroll = gtk::ScrolledWindow::new();
    left_scroll.set_child(Some(&left_pane));
    left_scroll.set_vexpand(true);

    let right_scroll = gtk::ScrolledWindow::new();
    right_scroll.set_child(Some(&right_pane));
    right_scroll.set_vexpand(true);

    // Link the scrollbars so they move together.
    let left_adj = left_scroll.vadjustment();
    let right_adj = right_scroll.vadjustment();
    left_adj.connect_value_changed({
        let right = right_adj.clone();
        move |adj| {
            right.set_value(adj.value());
        }
    });
    right_adj.connect_value_changed({
        let left = left_adj.clone();
        move |adj| {
            left.set_value(adj.value());
        }
    });

    content_box.append(&left_scroll);
    content_box.append(&right_scroll);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&header);
    outer.append(&content_box);

    let toolbar = adw::ToolbarView::new();
    toolbar.set_content(Some(&outer));
    dialog.set_child(Some(&toolbar));

    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_produce_equal_line_counts() {
        let text = "line 1\nline 2\nline 3";
        let pair = diff_markup(text, text);
        let left_count = pair.left.lines().count();
        let right_count = pair.right.lines().count();
        assert_eq!(left_count, right_count);
        assert_eq!(left_count, 3);
    }

    #[test]
    fn identical_inputs_have_no_diff_styling() {
        let text = "line 1\nline 2";
        let pair = diff_markup(text, text);
        assert!(!pair.left.contains("<span"));
        assert!(!pair.right.contains("<span"));
    }

    #[test]
    fn pure_insertion_keeps_line_counts_equal() {
        let left = "line 1\nline 3";
        let right = "line 1\nline 2\nline 3";
        let pair = diff_markup(left, right);
        let left_count = pair.left.lines().count();
        let right_count = pair.right.lines().count();
        assert_eq!(left_count, right_count);
        assert_eq!(left_count, 3);
        // Left should have a blank filler.
        assert_eq!(pair.left.lines().nth(1).unwrap(), "");
    }

    #[test]
    fn pure_deletion_keeps_line_counts_equal() {
        let left = "line 1\nline 2\nline 3";
        let right = "line 1\nline 3";
        let pair = diff_markup(left, right);
        let left_count = pair.left.lines().count();
        let right_count = pair.right.lines().count();
        assert_eq!(left_count, right_count);
        assert_eq!(left_count, 3);
        // Right should have a blank filler.
        assert_eq!(pair.right.lines().nth(1).unwrap(), "");
    }

    #[test]
    fn mixed_change_keeps_line_counts_equal() {
        let left = "a\nb\nc\nd";
        let right = "a\nx\nc\ny";
        let pair = diff_markup(left, right);
        let left_count = pair.left.lines().count();
        let right_count = pair.right.lines().count();
        assert_eq!(left_count, right_count);
    }

    #[test]
    fn markup_metacharacters_are_escaped() {
        let left = "<tag> & entity";
        let right = "<tag> & entity";
        let pair = diff_markup(left, right);
        assert!(pair.left.contains("&lt;tag&gt;"));
        assert!(pair.left.contains("&amp;"));
        assert!(pair.right.contains("&lt;tag&gt;"));
        assert!(pair.right.contains("&amp;"));
    }

    #[test]
    fn capped_at_400_lines() {
        let left: String = (0..500).map(|i| format!("line {}\n", i)).collect();
        let right: String = (0..500).map(|i| format!("line {}\n", i)).collect();
        let pair = diff_markup(&left, &right);
        let left_count = pair.left.lines().count();
        let right_count = pair.right.lines().count();
        // 400 lines, plus one trailing newline creates an empty line.
        assert!(left_count <= 401, "left has {} lines", left_count);
        assert!(right_count <= 401, "right has {} lines", right_count);
        assert_eq!(left_count, right_count);
    }
}
