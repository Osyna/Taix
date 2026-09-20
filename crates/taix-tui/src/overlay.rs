//! Overlay widgets: palette, prompt, viewer, and find bar.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

pub struct Item {
    pub id: String,
    pub group: &'static str,
    pub title: String,
    pub subtitle: String,
}

/// Fuzzy subsequence score, higher is better; `None` when `needle` does not match.
fn score(needle: &str, haystack: &str) -> Option<i32> {
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
            points += 15;
        } else {
            points -= (gap as i32 - 1) * 2;
        }
    }

    // Word boundary bonus: match at start or after space/punctuation
    let is_word_boundary = |idx: usize| {
        idx == 0
            || haystack_chars
                .get(idx.saturating_sub(1))
                .is_some_and(|c| !c.is_alphanumeric())
    };

    for &pos in &matched_positions {
        if is_word_boundary(pos) {
            points += 10;
        }
    }

    Some(points)
}

/// Items that match, best first; items without a match excluded.
pub fn rank<'a>(items: &'a [Item], query: &str) -> Vec<&'a Item> {
    let mut scored: Vec<(usize, &Item, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let title = score(query, &item.title);
            let subtitle = score(query, &item.subtitle).map(|s| s - 10);
            match (title, subtitle) {
                (Some(t), Some(s)) => Some((idx, item, t.max(s))),
                (Some(t), None) => Some((idx, item, t)),
                (None, Some(s)) => Some((idx, item, s)),
                (None, None) => None,
            }
        })
        .collect();

    scored.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    scored.into_iter().map(|(_, item, _)| item).collect()
}

pub struct Palette {
    pub title: String,
    pub query: String,
    pub cursor: usize,
    pub items: Vec<Item>,
}

impl Palette {
    pub fn new(title: impl Into<String>, items: Vec<Item>) -> Palette {
        Palette {
            title: title.into(),
            query: String::new(),
            cursor: 0,
            items,
        }
    }

    pub fn visible(&self) -> Vec<&Item> {
        rank(&self.items, &self.query)
    }

    pub fn selected(&self) -> Option<&Item> {
        let visible = self.visible();
        visible.get(self.cursor).copied()
    }

    pub fn step(&mut self, delta: i32) {
        let visible_count = self.visible().len();
        if visible_count == 0 {
            self.cursor = 0;
            return;
        }
        let new_cursor = (self.cursor as i32 + delta).clamp(0, visible_count as i32 - 1);
        self.cursor = new_cursor as usize;
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.cursor = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.cursor = 0;
    }
}

pub struct Prompt {
    pub title: String,
    pub value: String,
    pub hint: String,
}

impl Prompt {
    pub fn new(
        title: impl Into<String>,
        value: impl Into<String>,
        hint: impl Into<String>,
    ) -> Prompt {
        Prompt {
            title: title.into(),
            value: value.into(),
            hint: hint.into(),
        }
    }

    pub fn push(&mut self, c: char) {
        self.value.push(c);
    }

    pub fn backspace(&mut self) {
        self.value.pop();
    }
}

pub struct Viewer {
    pub title: String,
    pub body: Vec<Line<'static>>,
    pub scroll: u16,
}

impl Viewer {
    pub fn new(title: impl Into<String>, body: Vec<Line<'static>>) -> Viewer {
        Viewer {
            title: title.into(),
            body,
            scroll: 0,
        }
    }

    pub fn step(&mut self, delta: i32, page: u16) {
        let max_scroll = self.body.len().saturating_sub(1);
        let delta_lines = delta * page as i32;
        let new_scroll = (self.scroll as i32 + delta_lines).clamp(0, max_scroll as i32);
        self.scroll = new_scroll as u16;
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let width = area.width.min((area.width * percent_x) / 100);
    let height = area.height.min((area.height * percent_y) / 100);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

pub fn draw_palette(f: &mut Frame, area: Rect, palette: &Palette) {
    let modal = centered_rect(70, 70, area);
    let visible = palette.visible();

    let mut lines = vec![Line::from(vec![
        Span::raw(&palette.title),
        Span::raw(": "),
        Span::styled(
            &palette.query,
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ])];

    let mut current_group: Option<&'static str> = None;
    for (idx, item) in visible.iter().enumerate() {
        if current_group != Some(item.group) {
            current_group = Some(item.group);
            lines.push(Line::from(Span::styled(
                item.group,
                Style::default().fg(Color::DarkGray),
            )));
        }

        let is_selected = idx == palette.cursor;
        let style = if is_selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };

        let width = modal.width.saturating_sub(4) as usize;
        let subtitle_len = item.subtitle.len();

        let title_max = if subtitle_len > 0 {
            width.saturating_sub(subtitle_len).saturating_sub(1)
        } else {
            width
        };

        let title = if item.title.len() > title_max {
            if title_max > 1 {
                format!("{}…", &item.title[..title_max - 1])
            } else {
                String::from("…")
            }
        } else {
            item.title.clone()
        };

        let mut spans = vec![Span::styled(title.clone(), style)];
        if !item.subtitle.is_empty() {
            let padding = width
                .saturating_sub(title.len())
                .saturating_sub(subtitle_len);
            spans.push(Span::styled(" ".repeat(padding), style));
            spans.push(Span::styled(&item.subtitle, style.fg(Color::DarkGray)));
        }
        lines.push(Line::from(spans));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(palette.title.clone());
    let para = Paragraph::new(lines).block(block);
    // A modal over a terminal grid must blank what is under it, or the pane
    // borders and output show through every short row.
    f.render_widget(Clear, modal);
    f.render_widget(para, modal);
}

pub fn draw_prompt(f: &mut Frame, area: Rect, prompt: &Prompt) {
    let modal = centered_rect(50, 20, area);

    let mut lines = vec![
        Line::from(Span::styled(
            &prompt.title,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::raw(&prompt.value),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ]),
    ];

    if !prompt.hint.is_empty() {
        lines.push(Line::from(Span::styled(
            &prompt.hint,
            Style::default().fg(Color::DarkGray),
        )));
    }

    let block = Block::default().borders(Borders::ALL);
    let para = Paragraph::new(lines).block(block);
    f.render_widget(Clear, modal);
    f.render_widget(para, modal);
}

pub fn draw_viewer(f: &mut Frame, area: Rect, viewer: &Viewer) {
    let modal = centered_rect(90, 85, area);

    let inner_height = modal.height.saturating_sub(2) as usize;
    let scroll_max = viewer.body.len();
    let scroll_indicator = if scroll_max > inner_height {
        format!(" {}/{} ", viewer.scroll, scroll_max)
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(viewer.title.clone())
        .title_bottom(scroll_indicator);

    let visible_lines: Vec<Line> = viewer
        .body
        .iter()
        .skip(viewer.scroll as usize)
        .take(inner_height)
        .cloned()
        .collect();

    let para = Paragraph::new(visible_lines).block(block);
    f.render_widget(Clear, modal);
    f.render_widget(para, modal);
}

pub fn draw_find(f: &mut Frame, area: Rect, needle: &str, hits: usize, current: usize) {
    let text = if needle.is_empty() {
        String::new()
    } else if hits == 0 {
        format!("find: {} — no matches", needle)
    } else {
        format!("find: {} — {}/{}", needle, current + 1, hits)
    };

    let para = Paragraph::new(text);
    f.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_exact_prefix_beats_scattered() {
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
        assert_eq!(ranked[0].id, "prefix");
    }

    #[test]
    fn rank_non_match_excluded() {
        let items = vec![
            Item {
                id: "match".into(),
                group: "G",
                title: "foo".into(),
                subtitle: "".into(),
            },
            Item {
                id: "no_match".into(),
                group: "G",
                title: "bar".into(),
                subtitle: "".into(),
            },
        ];
        let ranked = rank(&items, "foo");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].id, "match");
    }

    #[test]
    fn rank_empty_query_preserves_order() {
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
                title: "bar".into(),
                subtitle: "".into(),
            },
            Item {
                id: "c".into(),
                group: "G",
                title: "baz".into(),
                subtitle: "".into(),
            },
        ];
        let ranked = rank(&items, "");
        assert_eq!(
            ranked.iter().map(|i| &*i.id).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn palette_selected_tracks_filtered_list() {
        let mut p = Palette::new(
            "Test",
            vec![
                Item {
                    id: "a".into(),
                    group: "G",
                    title: "apple".into(),
                    subtitle: "".into(),
                },
                Item {
                    id: "b".into(),
                    group: "G",
                    title: "banana".into(),
                    subtitle: "".into(),
                },
            ],
        );
        assert_eq!(p.selected().unwrap().id, "a");
        p.push('b');
        assert_eq!(p.selected().unwrap().id, "b");
    }

    #[test]
    fn palette_selected_none_when_cursor_out_of_range() {
        let mut p = Palette::new("Test", vec![]);
        assert!(p.selected().is_none());
        p.cursor = 5;
        assert!(p.selected().is_none());
    }

    #[test]
    fn palette_step_clamps_at_bounds() {
        let mut p = Palette::new(
            "Test",
            vec![
                Item {
                    id: "a".into(),
                    group: "G",
                    title: "a".into(),
                    subtitle: "".into(),
                },
                Item {
                    id: "b".into(),
                    group: "G",
                    title: "b".into(),
                    subtitle: "".into(),
                },
            ],
        );
        p.step(-1);
        assert_eq!(p.cursor, 0);
        p.step(10);
        assert_eq!(p.cursor, 1);
        p.step(10);
        assert_eq!(p.cursor, 1);
    }

    #[test]
    fn viewer_step_clamps_at_bounds() {
        let mut v = Viewer::new(
            "Test",
            vec![
                Line::raw("line 1"),
                Line::raw("line 2"),
                Line::raw("line 3"),
            ],
        );
        v.step(-1, 1);
        assert_eq!(v.scroll, 0);
        v.step(10, 1);
        assert_eq!(v.scroll, 2);
        v.step(10, 1);
        assert_eq!(v.scroll, 2);
    }

    #[test]
    fn viewer_step_page_delta() {
        let mut v = Viewer::new(
            "Test",
            (0..20).map(|i| Line::raw(format!("line {}", i))).collect(),
        );
        v.step(1, 5);
        assert_eq!(v.scroll, 5);
        v.step(-1, 3);
        assert_eq!(v.scroll, 2);
    }
}
