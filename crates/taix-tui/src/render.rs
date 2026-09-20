use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use taix_term::{Chunk, Rgb};

/// Convert taix_term::Chunk iterator into ratatui Lines.
#[allow(dead_code)]
pub fn chunks_to_lines<'a>(chunks: impl Iterator<Item = Chunk<'a>>) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let mut spans = Vec::new();

    for chunk in chunks {
        match chunk {
            Chunk::Text(text, style) => {
                spans.push(to_owned_span(text, style));
            }
            Chunk::LineBreak => {
                lines.push(Line::from(std::mem::take(&mut spans)));
            }
        }
    }

    // Final line if any spans remain
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }

    lines
}

fn rgb_to_color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

pub fn to_owned_span(text: &str, style: taix_term::Style) -> Span<'static> {
    let mut ratatui_style = Style::default();

    if let Some(fg) = style.fg {
        ratatui_style = ratatui_style.fg(rgb_to_color(fg));
    }
    if let Some(bg) = style.bg {
        ratatui_style = ratatui_style.bg(rgb_to_color(bg));
    }

    let mut modifier = Modifier::empty();
    if style.bold {
        modifier |= Modifier::BOLD;
    }
    if style.italic {
        modifier |= Modifier::ITALIC;
    }
    if style.underline {
        modifier |= Modifier::UNDERLINED;
    }
    if style.strike {
        modifier |= Modifier::CROSSED_OUT;
    }
    // The insertion cursor. A terminal knows its own colours, so plain
    // reverse video is both correct and cheaper than the GTK front's
    // translucent block.
    if style.cursor {
        modifier |= Modifier::REVERSED;
    }
    if !modifier.is_empty() {
        ratatui_style = ratatui_style.add_modifier(modifier);
    }

    Span::styled(text.to_string(), ratatui_style)
}

/// Sidebar width clamped to 24..40 or 30% of total.
pub fn sidebar_width(total: u16) -> u16 {
    let pct = (total * 30) / 100;
    pct.clamp(24, 40).min(total.saturating_sub(20))
}

/// Visual representation of state for sidebar.
pub fn state_display(state: taix_core::AgentState) -> (&'static str, Style) {
    use taix_core::AgentState::*;
    match state {
        Starting => ("starting", Style::default().fg(Color::Cyan)),
        Working => ("working", Style::default().fg(Color::Green)),
        Waiting => (
            "waiting",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Idle => ("idle", Style::default().fg(Color::Blue)),
        Done => (
            "done",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
        ),
        Failed => (
            "failed",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taix_term::{Chunk, Rgb, Style as TermStyle};

    #[test]
    fn chunk_plain_text_no_color() {
        let chunks = vec![Chunk::Text("hello", TermStyle::default())];
        let lines = chunks_to_lines(chunks.into_iter());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "hello");
        // Default style: no fg/bg override
        assert_eq!(lines[0].spans[0].style.fg, None);
        assert_eq!(lines[0].spans[0].style.bg, None);
    }

    #[test]
    fn chunk_rgb_fg() {
        let style = TermStyle {
            fg: Some(Rgb {
                r: 255,
                g: 128,
                b: 0,
            }),
            ..TermStyle::default()
        };
        let chunks = vec![Chunk::Text("orange", style)];
        let lines = chunks_to_lines(chunks.into_iter());
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(255, 128, 0)));
    }

    #[test]
    fn chunk_modifiers() {
        let style = TermStyle {
            bold: true,
            underline: true,
            ..TermStyle::default()
        };
        let chunks = vec![Chunk::Text("bold+underline", style)];
        let lines = chunks_to_lines(chunks.into_iter());
        let modifiers = lines[0].spans[0].style.add_modifier;
        assert!(modifiers.contains(Modifier::BOLD));
        assert!(modifiers.contains(Modifier::UNDERLINED));
        assert!(!modifiers.contains(Modifier::ITALIC));
    }

    #[test]
    fn chunk_line_breaks() {
        let chunks = vec![
            Chunk::Text("line1", TermStyle::default()),
            Chunk::LineBreak,
            Chunk::Text("line2", TermStyle::default()),
            Chunk::LineBreak,
        ];
        let lines = chunks_to_lines(chunks.into_iter());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, "line1");
        assert_eq!(lines[1].spans[0].content, "line2");
    }

    #[test]
    fn sidebar_width_clamps() {
        assert_eq!(sidebar_width(50), 24); // 30% = 15, clamp to 24
        assert_eq!(sidebar_width(100), 30); // 30% = 30, within 24..40
        assert_eq!(sidebar_width(200), 40); // 30% = 60, clamp to 40
        assert_eq!(sidebar_width(30), 10); // Can't leave <20 for right; min(24, 10)
    }

    #[test]
    fn state_display_attention_differs_from_calm() {
        let (_, waiting_style) = state_display(taix_core::AgentState::Waiting);
        let (_, working_style) = state_display(taix_core::AgentState::Working);
        // Attention state (Waiting) must differ visually from calm (Working)
        assert_ne!(waiting_style, working_style);

        let (_, failed_style) = state_display(taix_core::AgentState::Failed);
        assert_ne!(failed_style, working_style);
    }
}
