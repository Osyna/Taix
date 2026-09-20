//! Pane search: matching and the find bar widget.

use gtk::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub line: usize,
    pub col: usize,
    pub len: usize,
}

/// Every match of `needle` in `text`, in reading order.
///
/// Smartcase: a needle that is all-lowercase matches case-insensitively,
/// otherwise exactly. Matches are non-overlapping: the search advances past
/// each match. Line and col are 0-based; col is a character offset, not byte.
pub fn search(text: &str, needle: &str) -> Vec<Hit> {
    if needle.is_empty() {
        return Vec::new();
    }

    let case_sensitive = needle.chars().any(|c| c.is_uppercase());
    let needle_to_match = if case_sensitive {
        needle.to_string()
    } else {
        needle.to_lowercase()
    };

    let mut hits = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let line_chars: Vec<char> = line.chars().collect();
        let line_lower: Vec<char> = if case_sensitive {
            Vec::new()
        } else {
            line.to_lowercase().chars().collect()
        };

        let mut col_offset = 0;

        while col_offset < line_chars.len() {
            let to_search: String = if case_sensitive {
                line_chars[col_offset..].iter().collect()
            } else {
                line_lower[col_offset..].iter().collect()
            };

            if let Some(byte_pos) = to_search.find(&needle_to_match) {
                // Convert byte position to char position
                let char_pos = to_search[..byte_pos].chars().count();
                let actual_col = col_offset + char_pos;

                hits.push(Hit {
                    line: line_idx,
                    col: actual_col,
                    len: needle.chars().count(),
                });

                // Advance past this match (non-overlapping)
                col_offset = actual_col + needle.chars().count();
            } else {
                break;
            }
        }
    }

    hits
}

/// Cloned into `Widgets`, which the app holds by value.
#[derive(Clone)]
pub struct FindBar {
    pub root: gtk::Widget,
    pub entry: gtk::SearchEntry,
    pub count: gtk::Label,
    pub prev: gtk::Button,
    pub next: gtk::Button,
    pub close: gtk::Button,
}

/// Build a find bar widget.
///
/// Returns unwired: the caller connects the buttons and the entry.
pub fn find_bar() -> FindBar {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("taix-find-bar");
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(6);
    bar.set_margin_bottom(6);

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Find...")
        .width_chars(20)
        .build();
    entry.add_css_class("taix-find-entry");

    let count = gtk::Label::builder().label("0/0").build();
    count.add_css_class("taix-find-count");

    let prev = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("Previous")
        .build();
    prev.add_css_class("taix-find-prev");

    let next = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text("Next")
        .build();
    next.add_css_class("taix-find-next");

    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close")
        .build();
    close.add_css_class("taix-find-close");

    bar.append(&entry);
    bar.append(&count);
    bar.append(&prev);
    bar.append(&next);
    bar.append(&close);

    FindBar {
        root: bar.upcast(),
        entry,
        count,
        prev,
        next,
        close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_multiple_hits_one_line() {
        let hits = search("foo bar foo", "foo");
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0],
            Hit {
                line: 0,
                col: 0,
                len: 3
            }
        );
        assert_eq!(
            hits[1],
            Hit {
                line: 0,
                col: 8,
                len: 3
            }
        );
    }

    #[test]
    fn search_across_lines() {
        let text = "first line\nsecond line\nthird line";
        let hits = search(text, "line");
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0],
            Hit {
                line: 0,
                col: 6,
                len: 4
            }
        );
        assert_eq!(
            hits[1],
            Hit {
                line: 1,
                col: 7,
                len: 4
            }
        );
        assert_eq!(
            hits[2],
            Hit {
                line: 2,
                col: 6,
                len: 4
            }
        );
    }

    #[test]
    fn search_smartcase_lowercase_needle() {
        let hits = search("Test test TEST", "test");
        assert_eq!(hits.len(), 3); // case-insensitive
    }

    #[test]
    fn search_smartcase_uppercase_needle() {
        let hits = search("Test test TEST", "Test");
        assert_eq!(hits.len(), 1); // case-sensitive, exact match
        assert_eq!(
            hits[0],
            Hit {
                line: 0,
                col: 0,
                len: 4
            }
        );
    }

    #[test]
    fn search_utf8_char_offset() {
        // "café" has 4 chars but 5 bytes (é is 2 bytes)
        let text = "café test café";
        let hits = search(text, "café");
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0],
            Hit {
                line: 0,
                col: 0,
                len: 4
            }
        );
        assert_eq!(
            hits[1],
            Hit {
                line: 0,
                col: 10,
                len: 4
            }
        );
    }

    #[test]
    fn search_needle_longer_than_text() {
        let hits = search("hi", "hello");
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn search_empty_needle() {
        let hits = search("anything", "");
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn search_non_overlapping() {
        // "aaa" should match once at position 0, not three times
        let hits = search("aaa", "aa");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0],
            Hit {
                line: 0,
                col: 0,
                len: 2
            }
        );
    }
}
