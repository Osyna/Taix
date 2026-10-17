//! A pane screen as rows of styled runs, from the same chunks the GTK front
//! turns into Pango markup.
//!
//! The grid is built here, next to the emulator, rather than shipping cells
//! and a palette to be assembled in JavaScript: a phone then only has to
//! paint, and the payload is the one thing on the wire that scales with how
//! chatty the agents are.

use std::collections::HashMap;

use taix_term::{Chunk, Style};

/// One screen: rows of runs, each run a stretch of text under one span's
/// attributes. The attribute strings (` class="b f1"`, empty for plain
/// text) are kept once per screen and a run names its string by index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screen {
    pub(crate) attrs: Vec<String>,
    pub(crate) rows: Vec<Vec<(usize, String)>>,
}

impl Screen {
    /// Nothing was drawn: a pane with no capture yet.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The screen as HTML, one `\n`-separated row per line of the grid,
    /// every row closing its own spans so rows stand alone. What a share
    /// link's page paints.
    pub fn to_html(&self) -> String {
        let mut out = String::with_capacity(4 << 10);
        for (i, row) in self.rows.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            for (style, text) in row {
                out.push_str("<span");
                out.push_str(&self.attrs[*style]);
                out.push('>');
                escape(&mut out, text);
                out.push_str("</span>");
            }
        }
        out
    }
}

/// Render one screen. `draw` is `Live::render` or `taix_term::snapshot`
/// with its sink already bound.
pub fn screen(draw: impl FnOnce(&mut dyn FnMut(Chunk<'_>))) -> Screen {
    let mut out = Screen::default();
    let mut known: HashMap<String, usize> = HashMap::new();
    let mut attrs = String::new();
    let (mut row, mut drawn) = (Vec::new(), false);
    draw(&mut |chunk| {
        drawn = true;
        match chunk {
            Chunk::Text(text, style) => {
                attrs.clear();
                push_attrs(&mut attrs, &style);
                let id = known.get(&attrs).copied().unwrap_or_else(|| {
                    out.attrs.push(attrs.clone());
                    known.insert(attrs.clone(), out.attrs.len() - 1);
                    out.attrs.len() - 1
                });
                row.push((id, text.to_string()));
            }
            Chunk::LineBreak => out.rows.push(std::mem::take(&mut row)),
        }
    });
    if drawn {
        out.rows.push(row);
    }
    out
}

/// A span's attributes. A palette colour is a class, so the 16 the
/// terminal actually uses cost two bytes on the wire instead of fifteen
/// and the browser parses no inline CSS. True colour has no class and
/// keeps its `style`.
fn push_attrs(out: &mut String, style: &Style) {
    use std::fmt::Write;
    let fg = style.fg.map(|rgb| (rgb, palette_index(rgb)));
    let bg = style.bg.map(|rgb| (rgb, palette_index(rgb)));
    let mut classes = false;
    let mut open_class = |out: &mut String| {
        out.push_str(if classes { " " } else { " class=\"" });
        classes = true;
    };
    for (on, name) in [
        (style.bold, "b"),
        (style.italic, "i"),
        (style.underline, "u"),
        (style.strike, "s"),
        (style.cursor, "cur"),
    ] {
        if on {
            open_class(out);
            out.push_str(name);
        }
    }
    if let Some((_, Some(index))) = fg {
        open_class(out);
        let _ = write!(out, "f{index}");
    }
    if let Some((_, Some(index))) = bg {
        open_class(out);
        let _ = write!(out, "b{index}");
    }
    if classes {
        out.push('"');
    }
    let fg = fg.and_then(|(rgb, index)| index.is_none().then_some(rgb));
    let bg = bg.and_then(|(rgb, index)| index.is_none().then_some(rgb));
    if fg.is_some() || bg.is_some() {
        out.push_str(" style=\"");
        if let Some(rgb) = fg {
            out.push_str("color:");
            hex(out, rgb);
            out.push(';');
        }
        if let Some(rgb) = bg {
            out.push_str("background:");
            hex(out, rgb);
        }
        out.push('"');
    }
}

fn hex(out: &mut String, rgb: taix_term::Rgb) {
    use std::fmt::Write;
    let _ = write!(out, "#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b);
}

/// Which of the first sixteen palette entries this colour is, if any. The
/// emulator resolves indices to RGB before we see them, so the way back is
/// to ask the same table it used.
fn palette_index(rgb: taix_term::Rgb) -> Option<u8> {
    (0..16).find(|&i| taix_term::indexed(i) == rgb)
}

/// `&`, `<` and `>` only. A pane's text is not an attribute value, and
/// escaping quotes as well would show `&quot;` in anything that prints
/// JSON - which agents do constantly.
fn escape(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taix_term::Size;
    fn render(bytes: &[u8], cols: usize, rows: usize) -> Screen {
        let size = Size { cols, rows };
        // `snapshot` takes a sized sink, so the trait object is reborrowed
        // through a closure rather than passed along.
        screen(|sink| taix_term::snapshot(size, bytes, &mut |chunk| sink(chunk)))
    }

    #[test]
    fn markup_in_pane_output_is_escaped_not_interpreted() {
        let html = render(b"<script>alert(1)</script> a&b", 40, 1).to_html();
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt; a&amp;b"),
            "{html}"
        );
        assert!(!html.contains("<script>"), "{html}");
    }

    #[test]
    fn a_styled_run_carries_palette_colour_as_class_and_weight_as_class() {
        let screen = render(b"\x1b[1;31mred\x1b[0m plain", 40, 1);
        let html = screen.to_html();
        // Bold red (SGR 1;31) produces bold + bright red (palette index 9).
        assert!(html.contains("class=\"b f9\""), "{html}");
        // Palette colours are classes, not inline styles.
        assert!(!html.contains("color:#"), "{html}");
        // The plain tail must not inherit the opening span.
        assert!(
            html.contains("plain</span>") || html.contains("> plain"),
            "{html}"
        );
        // Runs name their attributes by index; plain text is the empty one.
        let [(red, text), (plain, tail)] = &screen.rows[0][..] else {
            panic!("{:?}", screen.rows)
        };
        assert_eq!((text.as_str(), tail.as_str()), ("red", " plain"));
        assert_eq!(screen.attrs[*red], " class=\"b f9\"");
        assert_eq!(screen.attrs[*plain], "");
    }

    #[test]
    fn nothing_drawn_is_empty_and_a_trailing_break_is_an_empty_row() {
        assert!(screen(|_| {}).is_empty());
        assert_eq!(screen(|_| {}).to_html(), "");
        let lines = screen(|sink| {
            sink(Chunk::Text("a", Style::default()));
            sink(Chunk::LineBreak);
        });
        assert_eq!(lines.rows.len(), 2);
        assert_eq!(lines.to_html(), "<span>a</span>\n");
    }

    #[test]
    fn every_row_is_one_line_so_the_page_can_use_pre() {
        let html = render(b"one\r\ntwo", 10, 3).to_html();
        assert_eq!(html.matches('\n').count(), 2, "{html}");
    }
}
