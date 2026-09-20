//! Command palette: fuzzy ranking and the dialog that shows it.

use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub struct Item {
    pub id: String,
    pub group: &'static str,
    pub title: String,
    pub subtitle: String,
}

/// Fuzzy subsequence score, higher is better; `None` when `needle` does not match.
///
/// Case-insensitive. Rewards consecutive runs, word-boundary matches, and prefix
/// matches. Penalises distance between matched characters.
pub fn score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }

    let needle_lower = needle.to_lowercase();
    let haystack_lower = haystack.to_lowercase();
    let needle_chars: Vec<char> = needle_lower.chars().collect();
    let haystack_chars: Vec<char> = haystack_lower.chars().collect();

    let mut haystack_idx = 0;
    let mut matched_positions = Vec::new();

    // Check if subsequence exists and collect positions
    for &nc in &needle_chars {
        let pos = haystack_chars[haystack_idx..]
            .iter()
            .position(|&hc| hc == nc)?;
        haystack_idx += pos;
        matched_positions.push(haystack_idx);
        haystack_idx += 1;
    }

    let mut points = 100;

    // Prefix match: first needle char matches first haystack char
    if matched_positions[0] == 0 {
        points += 50;
    }

    // Consecutive run bonus and distance penalty
    for i in 1..matched_positions.len() {
        let gap = matched_positions[i] - matched_positions[i - 1];
        if gap == 1 {
            points += 15; // consecutive
        } else {
            points -= (gap as i32 - 1) * 2; // distance penalty
        }
    }

    // Word boundary bonus: match at start or after space/punctuation
    let is_word_boundary = |idx: usize| {
        idx == 0
            || matches!(
                haystack_chars.get(idx - 1),
                Some(' ' | '-' | '_' | '/' | '\\' | '.')
            )
    };

    for &pos in &matched_positions {
        if is_word_boundary(pos) {
            points += 10;
        }
    }

    Some(points)
}

/// Indices of `items` that match, best first; stable for equal scores.
pub fn rank(items: &[Item], needle: &str) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            // Either field may match: a project's path lives in the
            // subtitle, so typing a directory fragment has to find it. The
            // title wins a tie because it is what you are reading.
            let title = score(needle, &item.title);
            let subtitle = score(needle, &item.subtitle).map(|s| s - 10);
            match (title, subtitle) {
                (Some(t), Some(s)) => Some((idx, t.max(s))),
                (Some(t), None) => Some((idx, t)),
                (None, Some(s)) => Some((idx, s)),
                (None, None) => None,
            }
        })
        .collect();

    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.into_iter().map(|(idx, _)| idx).collect()
}

/// Open the palette: a list you type into.
///
/// Every row is an item, one-to-one. Group titles are drawn by
/// `set_header_func`, NOT appended as rows: a bare label added to a
/// `GtkListBox` gets wrapped in a selectable row of its own, so the initial
/// selection landed on a *header*, whose missing item index fell back to
/// index 0 - and Enter then silently chose the wrong thing.
///
/// Each row carries its item id as its widget name, so activation reads the
/// id it will return instead of doing index arithmetic that can be off.
pub fn open(parent: &adw::ApplicationWindow, items: Vec<Item>, chosen: impl Fn(String) + 'static) {
    let items = Rc::new(items);
    let chosen = Rc::new(chosen);

    let dialog = adw::Dialog::builder()
        .title("Command palette")
        .content_width(600)
        .content_height(420)
        .build();

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.add_css_class("taix-palette-container");

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search projects, windows, actions…")
        .hexpand(true)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    search.add_css_class("taix-palette-search");

    let list = gtk::ListBox::new();
    list.add_css_class("taix-palette-list");
    list.set_selection_mode(gtk::SelectionMode::Single);

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();

    vbox.append(&search);
    vbox.append(&scrolled);
    dialog.set_child(Some(&vbox));

    // Row index -> group, for the header function. Rows are built in group
    // order, so a header is drawn wherever the group changes.
    let groups: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let groups = Rc::clone(&groups);
        list.set_header_func(move |row, before| {
            let groups = groups.borrow();
            let Some(group) = groups.get(row.index() as usize).copied() else {
                return;
            };
            let previous = before.and_then(|b| groups.get(b.index() as usize).copied());
            if previous == Some(group) {
                row.set_header(None::<&gtk::Widget>);
                return;
            }
            let header = gtk::Label::builder()
                .label(group)
                .xalign(0.0)
                .margin_start(12)
                .margin_end(12)
                .margin_top(8)
                .margin_bottom(4)
                .build();
            header.add_css_class("taix-palette-group");
            row.set_header(Some(&header));
        });
    }

    let fill = {
        let items = Rc::clone(&items);
        let list = list.clone();
        let groups = Rc::clone(&groups);
        let last_ranking = Rc::new(RefCell::new(Vec::new()));
        move |query: &str| {
            let ranking = rank(&items, query);
            // O1: Skip rebuild if ranking unchanged.
            if *last_ranking.borrow() == ranking {
                return;
            }
            *last_ranking.borrow_mut() = ranking.clone();

            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let mut order = Vec::new();
            for &idx in &ranking {
                let item = &items[idx];
                let title = gtk::Label::builder()
                    .label(&item.title)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build();
                title.add_css_class("taix-palette-title");

                let row_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
                row_box.set_margin_start(12);
                row_box.set_margin_end(12);
                row_box.set_margin_top(6);
                row_box.set_margin_bottom(6);
                row_box.append(&title);
                if !item.subtitle.is_empty() {
                    let subtitle = gtk::Label::builder()
                        .label(&item.subtitle)
                        .xalign(0.0)
                        .ellipsize(gtk::pango::EllipsizeMode::Middle)
                        .build();
                    subtitle.add_css_class("taix-palette-subtitle");
                    row_box.append(&subtitle);
                }

                let row = gtk::ListBoxRow::builder()
                    .child(&row_box)
                    .activatable(true)
                    .selectable(true)
                    .name(&item.id)
                    .build();
                list.append(&row);
                order.push(item.group);
            }
            *groups.borrow_mut() = order;
            list.invalidate_headers();
            // Select the first row so Enter on an untouched query is
            // meaningful.
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        }
    };
    fill("");

    {
        let fill = fill.clone();
        search.connect_search_changed(move |entry| fill(&entry.text()));
    }

    // Enter takes the selection; a click activates the row it hit.
    let take = {
        let chosen = Rc::clone(&chosen);
        let dialog = dialog.clone();
        move |row: &gtk::ListBoxRow| {
            let id = row.widget_name().to_string();
            if !id.is_empty() {
                chosen(id);
            }
            dialog.close();
        }
    };
    {
        let list = list.clone();
        let take = take.clone();
        search.connect_activate(move |_| {
            if let Some(row) = list.selected_row() {
                take(&row);
            }
        });
    }
    {
        let take = take.clone();
        list.connect_row_activated(move |_, row| take(row));
    }

    // Up/Down from the entry walk the list, so the hands never leave it.
    let keys = gtk::EventControllerKey::new();
    {
        let list = list.clone();
        let dialog = dialog.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let step = match key {
                gtk::gdk::Key::Down => 1,
                gtk::gdk::Key::Up => -1,
                gtk::gdk::Key::Escape => {
                    dialog.close();
                    return gtk::glib::Propagation::Stop;
                }
                _ => return gtk::glib::Propagation::Proceed,
            };
            let current = list.selected_row().map(|r| r.index()).unwrap_or(0);
            if let Some(next) = list.row_at_index(current + step) {
                list.select_row(Some(&next));
                next.grab_focus();
            }
            gtk::glib::Propagation::Stop
        });
    }
    search.add_controller(keys);

    dialog.present(Some(parent));
    search.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_exact_prefix_beats_scattered() {
        let prefix_score = score("abc", "abcdef").unwrap();
        let scattered_score = score("abc", "axbxcx").unwrap();
        assert!(prefix_score > scattered_score);
    }

    #[test]
    fn score_non_match_returns_none() {
        assert_eq!(score("xyz", "abc"), None);
    }

    #[test]
    fn score_empty_needle_matches_everything() {
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn score_case_insensitive() {
        assert_eq!(score("ABC", "abc"), score("abc", "ABC"));
        assert!(score("test", "TestCase").is_some());
    }

    #[test]
    fn score_word_boundary_bonus() {
        let boundary_score = score("rc", "run_command").unwrap();
        let mid_score = score("rc", "force").unwrap();
        assert!(boundary_score > mid_score);
    }

    #[test]
    fn rank_stable_for_equal_scores() {
        let items = vec![
            Item {
                id: "a".into(),
                group: "G",
                title: "foo".into(),
                subtitle: "".into(),
            },
            Item {
                id: "b".into(),
                group: "G",
                title: "foo".into(),
                subtitle: "".into(),
            },
            Item {
                id: "c".into(),
                group: "G",
                title: "foo".into(),
                subtitle: "".into(),
            },
        ];
        let ranked = rank(&items, "foo");
        assert_eq!(ranked, vec![0, 1, 2]);
    }

    #[test]
    fn rank_empty_needle_keeps_order() {
        let items = vec![
            Item {
                id: "a".into(),
                group: "G",
                title: "zebra".into(),
                subtitle: "".into(),
            },
            Item {
                id: "b".into(),
                group: "G",
                title: "apple".into(),
                subtitle: "".into(),
            },
        ];
        let ranked = rank(&items, "");
        assert_eq!(ranked, vec![0, 1]);
    }

    #[test]
    fn rank_best_first() {
        let items = vec![
            Item {
                id: "scattered".into(),
                group: "G",
                title: "axbxcx".into(),
                subtitle: "".into(),
            },
            Item {
                id: "prefix".into(),
                group: "G",
                title: "abcdef".into(),
                subtitle: "".into(),
            },
        ];
        let ranked = rank(&items, "abc");
        assert_eq!(ranked[0], 1); // prefix should win
    }

    #[test]
    fn rank_considers_subtitle() {
        let items = vec![
            Item {
                id: "no_match".into(),
                group: "G",
                title: "xyz".into(),
                subtitle: "".into(),
            },
            Item {
                id: "subtitle_match".into(),
                group: "G",
                title: "xyz".into(),
                subtitle: "test".into(),
            },
        ];
        let ranked = rank(&items, "test");
        assert_eq!(ranked, vec![1]);
    }
}
