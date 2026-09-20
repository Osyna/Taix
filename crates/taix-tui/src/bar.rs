//! The bottom bar: configurable segments.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// What the bottom bar reports. `Default` is for the tests, which each care
/// about one field.
#[derive(Default)]
pub struct BarData<'a> {
    pub segments: &'a [String],
    pub root: Option<&'a std::path::Path>,
    pub branch: Option<&'a str>,
    pub windows: usize,
    pub live: usize,
    pub attention: usize,
    pub ui_kib: u64,
    pub tmux_kib: u64,
    pub project_kib: u64,
    pub panes_kib: u64,
    pub uptime: std::time::Duration,
    pub muted: bool,
    pub hint: &'a str,
}

/// Render one segment by id.
fn segment_text(id: &str, data: &BarData) -> String {
    match id {
        "where" => match data.root {
            Some(path) => taix_core::text::contract_home(path),
            None => "no project".to_string(),
        },
        "branch" => data.branch.unwrap_or("—").to_string(),
        "windows" => format!(
            "{} windows · {} live · {} attention",
            data.windows, data.live, data.attention
        ),
        "memory" => {
            let total = data.ui_kib + data.tmux_kib + data.panes_kib;
            let project = if data.project_kib == data.panes_kib {
                String::new()
            } else {
                format!("project {} · ", taix_core::mem::human_kib(data.project_kib))
            };
            format!(
                "ui {} · tmux {} · panes {} · {project}total {}",
                taix_core::mem::human_kib(data.ui_kib),
                taix_core::mem::human_kib(data.tmux_kib),
                taix_core::mem::human_kib(data.panes_kib),
                taix_core::mem::human_kib(total),
            )
        }
        "uptime" => format!("up {}", taix_core::text::human_secs(data.uptime.as_secs())),
        _ => String::new(),
    }
}

/// Render the bar as a Line with styled spans.
pub fn line(data: &BarData) -> Line<'static> {
    let mut spans = Vec::new();
    let has_left_anchor = data.segments.iter().any(|s| s == "where" || s == "empty");

    // Configured segments, joined with two spaces
    for (i, id) in data.segments.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  ".to_string()));
        }

        let text = segment_text(id, data);
        if text.is_empty() {
            continue;
        }

        let style = match id.as_str() {
            "where" => Style::default(),
            "branch" => Style::default().fg(Color::Cyan),
            _ => Style::default().fg(Color::DarkGray),
        };

        spans.push(Span::styled(text.to_string(), style));
    }

    // Push everything right if no left anchor
    if !has_left_anchor && !spans.is_empty() {
        spans.insert(0, Span::raw(" ".to_string()));
    }

    // Append MUTE/hint
    let mut suffix = Vec::new();
    if data.muted {
        suffix.push(Span::styled(
            "MUTE".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !data.hint.is_empty() {
        suffix.push(Span::styled(
            data.hint.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Two spaces between every part, including between the mode flags and
    // the hint: `MUTEproj-a: muted` reads as one word.
    for part in suffix {
        if !spans.is_empty() {
            spans.push(Span::raw("  ".to_string()));
        }
        spans.push(part);
    }

    Line::from(spans).style(Style::default().bg(Color::Rgb(30, 30, 30)))
}

/// Draw the bar.
pub fn draw(f: &mut Frame, area: Rect, data: &BarData) {
    let line = line(data);
    f.render_widget(line, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn missing_branch_renders_em_dash() {
        let segments = ["branch".to_string()];
        let data = BarData {
            segments: &segments,
            ..Default::default()
        };
        let result = line(&data);
        assert!(spans_text(&result).contains("—"));
    }

    #[test]
    fn memory_total_counts_every_pane_not_just_the_project() {
        let segments = ["memory".to_string()];
        let data = BarData {
            segments: &segments,
            ui_kib: 1024,
            tmux_kib: 1024,
            project_kib: 2 * 1024 * 1024,
            panes_kib: 6 * 1024 * 1024,
            ..Default::default()
        };
        let text = spans_text(&line(&data));
        assert!(text.contains("panes 6.0 GB"), "{text}");
        assert!(text.contains("project 2.0 GB"), "{text}");
        assert!(text.contains("total 6.0 GB"), "{text}");
    }

    #[test]
    fn one_project_leaves_the_slice_out() {
        let segments = ["memory".to_string()];
        let data = BarData {
            segments: &segments,
            project_kib: 4096,
            panes_kib: 4096,
            ..Default::default()
        };
        let text = spans_text(&line(&data));
        assert!(!text.contains("project"), "{text}");
        assert!(text.contains("total 4.0 MB"), "{text}");
    }

    #[test]
    fn windows_segment_matches_gui_format() {
        let segments = ["windows".to_string()];
        let data = BarData {
            segments: &segments,
            windows: 3,
            live: 2,
            attention: 1,
            ..Default::default()
        };
        let result = line(&data);
        assert_eq!(spans_text(&result), " 3 windows · 2 live · 1 attention");
    }

    #[test]
    fn unknown_segment_contributes_no_text() {
        let segments = ["unknown".to_string(), "branch".to_string()];
        let data = BarData {
            segments: &segments,
            branch: Some("main"),
            ..Default::default()
        };
        let result = line(&data);
        let text = spans_text(&result);
        assert!(!text.contains("unknown"));
        assert!(text.contains("main"));
    }

    #[test]
    fn muted_flag_appears() {
        let segments: Vec<String> = vec![];
        let data = BarData {
            segments: &segments,
            muted: true,
            ..Default::default()
        };
        let result = line(&data);
        let text = spans_text(&result);
        assert!(text.contains("MUTE"));

        let segments_off: Vec<String> = vec![];
        let data_off = BarData {
            segments: &segments_off,
            ..Default::default()
        };
        let result = line(&data_off);
        let text = spans_text(&result);
        assert!(!text.contains("MUTE"));
    }

    #[test]
    fn segment_order_follows_configuration() {
        let segments = ["branch".to_string(), "windows".to_string()];
        let data = BarData {
            segments: &segments,
            branch: Some("dev"),
            windows: 2,
            live: 1,
            ..Default::default()
        };
        let result = line(&data);
        let text = spans_text(&result);
        let dev_pos = text.find("dev").unwrap();
        let windows_pos = text.find("windows").unwrap();
        assert!(dev_pos < windows_pos);

        let segments = ["windows".to_string(), "branch".to_string()];
        let data_reversed = BarData {
            segments: &segments,
            branch: Some("dev"),
            windows: 2,
            live: 1,
            ..Default::default()
        };
        let result = line(&data_reversed);
        let text = spans_text(&result);
        let dev_pos = text.find("dev").unwrap();
        let windows_pos = text.find("windows").unwrap();
        assert!(windows_pos < dev_pos);
    }
}
