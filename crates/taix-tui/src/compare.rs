//! Side-by-side diff view.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

pub fn rows(left: &str, right: &str) -> Vec<taix_core::diff::Row> {
    taix_core::diff::align(left, right)
}

pub fn lines(rows: &[taix_core::diff::Row], width: u16) -> Vec<Line<'static>> {
    let col_width = ((width.saturating_sub(1)) / 2) as usize;

    rows.iter()
        .map(|row| {
            let (left_text, right_text, style) = match row.side {
                taix_core::diff::Side::Same => (
                    truncate(&row.left, col_width),
                    truncate(&row.right, col_width),
                    Style::default(),
                ),
                taix_core::diff::Side::Left => (
                    truncate(&row.left, col_width),
                    truncate("", col_width),
                    Style::default().fg(Color::Red),
                ),
                taix_core::diff::Side::Right => (
                    truncate("", col_width),
                    truncate(&row.right, col_width),
                    Style::default().fg(Color::Green),
                ),
            };

            let left_span = match row.side {
                taix_core::diff::Side::Left => Span::styled(pad_right(left_text, col_width), style),
                _ => Span::raw(pad_right(left_text, col_width)),
            };

            let right_span = match row.side {
                taix_core::diff::Side::Right => {
                    Span::styled(pad_right(right_text, col_width), style)
                }
                _ => Span::raw(pad_right(right_text, col_width)),
            };

            Line::from(vec![left_span, Span::raw(" "), right_span])
        })
        .collect()
}

fn truncate(text: &str, max_width: usize) -> String {
    if text.chars().count() <= max_width {
        text.to_string()
    } else {
        text.chars().take(max_width).collect()
    }
}

fn pad_right(text: String, width: usize) -> String {
    let len = text.chars().count();
    if len < width {
        format!("{}{}", text, " ".repeat(width - len))
    } else {
        text
    }
}

/// Colour a unified diff for the viewer.
///
/// The diff already carries its own markers, so all it needs is the colour;
/// `+++`/`---` headers must not read as additions and removals, which is why
/// they are checked first.
pub fn diff_lines(diff: &str) -> Vec<Line<'static>> {
    diff.lines()
        .map(|line| {
            let style = if line.starts_with("+++") || line.starts_with("---") {
                Style::default().add_modifier(ratatui::style::Modifier::BOLD)
            } else if line.starts_with('+') {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') {
                Style::default().fg(Color::Red)
            } else if line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(Span::styled(line.to_string(), style))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_wide_row() {
        let rows = vec![taix_core::diff::Row {
            side: taix_core::diff::Side::Same,
            left: "this is a very long line that should be truncated".to_string(),
            right: "another long line".to_string(),
        }];

        let lines = lines(&rows, 20);
        assert_eq!(lines.len(), 1);

        // Width 20, col_width = (20-1)/2 = 9
        let line_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(line_text.chars().count() <= 20);
    }
}
