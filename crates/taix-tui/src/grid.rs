//! Pane grid layout and rendering.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Balanced,
    Columns,
    Rows,
    MainLeft,
    MainTop,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Balanced,
        Preset::Columns,
        Preset::Rows,
        Preset::MainLeft,
        Preset::MainTop,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Preset::Balanced => "balanced",
            Preset::Columns => "columns",
            Preset::Rows => "rows",
            Preset::MainLeft => "main-left",
            Preset::MainTop => "main-top",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::Balanced => "Balanced",
            Preset::Columns => "Columns",
            Preset::Rows => "Rows",
            Preset::MainLeft => "Main Left",
            Preset::MainTop => "Main Top",
        }
    }

    pub fn from_id(id: &str) -> Option<Preset> {
        match id {
            "balanced" => Some(Preset::Balanced),
            "columns" => Some(Preset::Columns),
            "rows" => Some(Preset::Rows),
            "main-left" => Some(Preset::MainLeft),
            "main-top" => Some(Preset::MainTop),
            _ => None,
        }
    }
}

pub struct PaneView {
    pub name: String,
    pub harness: String,
    pub state: taix_core::AgentState,
    pub focused: bool,
    pub scroll: usize,
    pub badge: Option<String>,
    pub lines: Vec<Line<'static>>,
}

const MIN_COLS: u16 = 8;
const MIN_ROWS: u16 = 3;

pub fn areas(preset: Preset, area: Rect, count: usize) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![area];
    }

    match preset {
        Preset::Balanced => balanced_areas(area, count, false),
        Preset::Columns => linear_areas(area, count, Direction::Horizontal),
        Preset::Rows => linear_areas(area, count, Direction::Vertical),
        Preset::MainLeft => main_side_areas(area, count, Direction::Horizontal),
        Preset::MainTop => main_side_areas(area, count, Direction::Vertical),
    }
}

fn balanced_areas(area: Rect, count: usize, vertical: bool) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![area];
    }

    let mid = count.div_ceil(2);
    let direction = if vertical {
        Direction::Vertical
    } else {
        Direction::Horizontal
    };

    let chunks = Layout::default()
        .direction(direction)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let mut result = Vec::new();
    result.extend(balanced_areas(chunks[0], mid, !vertical));
    result.extend(balanced_areas(chunks[1], count - mid, !vertical));

    result.into_iter().filter(is_usable).collect()
}

fn linear_areas(area: Rect, count: usize, direction: Direction) -> Vec<Rect> {
    let max_fit = max_cells_that_fit(area, count, direction);
    if max_fit == 0 {
        return Vec::new();
    }

    let constraints: Vec<_> = (0..max_fit)
        .map(|_| Constraint::Percentage(100 / max_fit as u16))
        .collect();

    Layout::default()
        .direction(direction)
        .constraints(constraints)
        .split(area)
        .iter()
        .filter(|r| is_usable(r))
        .copied()
        .collect()
}

fn main_side_areas(area: Rect, count: usize, direction: Direction) -> Vec<Rect> {
    if count == 1 {
        return vec![area];
    }

    let main_split = Layout::default()
        .direction(direction)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let mut result = vec![main_split[0]];

    // The stack runs perpendicular to the main split, which is the whole point
    // of the preset: stacking the rest along the same axis just produces evenly
    // spaced columns, i.e. `Columns` with extra steps.
    let stack_direction = match direction {
        Direction::Horizontal => Direction::Vertical,
        Direction::Vertical => Direction::Horizontal,
    };

    let rest = linear_areas(main_split[1], count - 1, stack_direction);
    result.extend(rest);

    result.into_iter().filter(is_usable).collect()
}

fn is_usable(rect: &Rect) -> bool {
    rect.width >= MIN_COLS && rect.height >= MIN_ROWS
}

fn max_cells_that_fit(area: Rect, requested: usize, direction: Direction) -> usize {
    let (size, min_size) = match direction {
        Direction::Horizontal => (area.width, MIN_COLS),
        Direction::Vertical => (area.height, MIN_ROWS),
    };

    let max = (size / min_size) as usize;
    max.min(requested)
}

pub fn draw(f: &mut Frame, area: Rect, panes: &[PaneView], preset: Preset, zoomed: Option<usize>) {
    if let Some(idx) = zoomed {
        if let Some(pane) = panes.get(idx) {
            draw_pane(f, area, pane);
        }
        return;
    }

    let cells = areas(preset, area, panes.len());
    for (i, rect) in cells.iter().enumerate() {
        if let Some(pane) = panes.get(i) {
            draw_pane(f, *rect, pane);
        }
    }
}

fn draw_pane(f: &mut Frame, area: Rect, pane: &PaneView) {
    let title = build_title(pane);

    let border_style = if pane.focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible_lines: Vec<_> = pane
        .lines
        .iter()
        .take(inner.height as usize)
        .cloned()
        .collect();
    let paragraph = Paragraph::new(visible_lines);
    f.render_widget(paragraph, inner);
}

fn build_title(pane: &PaneView) -> Line<'static> {
    let mut spans = Vec::new();

    spans.push(Span::styled(
        pane.name.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ));

    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        pane.harness.clone(),
        Style::default().fg(Color::DarkGray),
    ));

    let (state_text, state_style) = crate::render::state_display(pane.state);
    spans.push(Span::raw(" "));
    spans.push(Span::styled(state_text, state_style));

    if let Some(ref badge_text) = pane.badge {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            badge_text.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if pane.scroll > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::raw(format!("▲{}", pane.scroll)));
    }

    Line::from(spans)
}

pub fn body_size(cell: Rect) -> taix_term::Size {
    let inner = Block::default().borders(Borders::ALL).inner(cell);
    taix_term::Size {
        cols: inner.width as usize,
        rows: inner.height as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_pane_fills_rect() {
        let area = Rect::new(0, 0, 80, 24);
        let cells = areas(Preset::Balanced, area, 1);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0], area);
    }

    #[test]
    fn columns_partitions_width() {
        let area = Rect::new(0, 0, 90, 24);
        let cells = areas(Preset::Columns, area, 3);
        assert_eq!(cells.len(), 3);

        // No overlap
        for i in 0..cells.len() {
            for j in (i + 1)..cells.len() {
                let overlap = cells[i].intersection(cells[j]);
                assert_eq!(overlap.area(), 0, "cells {} and {} overlap", i, j);
            }
        }

        // All cells contained in area
        for cell in &cells {
            assert!(cell.x >= area.x);
            assert!(cell.y >= area.y);
            assert!(cell.x + cell.width <= area.x + area.width);
            assert!(cell.y + cell.height <= area.y + area.height);
        }
    }

    #[test]
    fn balanced_four_panes_non_overlapping() {
        let area = Rect::new(0, 0, 80, 24);
        let cells = areas(Preset::Balanced, area, 4);
        assert_eq!(cells.len(), 4);

        // No overlap
        for i in 0..cells.len() {
            for j in (i + 1)..cells.len() {
                let overlap = cells[i].intersection(cells[j]);
                assert_eq!(overlap.area(), 0, "cells {} and {} overlap", i, j);
            }
        }

        // All cells contained in area
        for cell in &cells {
            assert!(cell.x >= area.x);
            assert!(cell.y >= area.y);
            assert!(cell.x + cell.width <= area.x + area.width);
            assert!(cell.y + cell.height <= area.y + area.height);
        }
    }

    #[test]
    fn main_left_layout() {
        let area = Rect::new(0, 0, 80, 24);
        let cells = areas(Preset::MainLeft, area, 3);
        assert_eq!(cells.len(), 3);

        // First pane on the left half
        assert!(cells[0].x < area.width / 2);
        assert!(cells[0].width <= area.width / 2 + 1);

        // Remaining panes on the right
        for cell in cells.iter().skip(1) {
            assert!(cell.x >= area.width / 2 - 1);
        }
    }

    #[test]
    fn too_small_returns_fewer_cells() {
        let area = Rect::new(0, 0, 20, 6);
        let cells = areas(Preset::Columns, area, 10);

        // Should only fit 2 cells at 8 cols minimum
        assert!(cells.len() < 10);
        assert!(cells.len() <= 2);

        // All returned cells meet minimum
        for cell in &cells {
            assert!(cell.width >= MIN_COLS);
            assert!(cell.height >= MIN_ROWS);
        }
    }
}
