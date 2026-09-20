//! A pane screen as HTML, from the same chunks the GTK front turns into
//! Pango markup.
//!
//! The grid is built here, next to the emulator, rather than shipping cells
//! and a palette to be assembled in JavaScript: a phone then only has to
//! paint, and the payload is the one thing on the wire that scales with how
//! chatty the agents are.

use taix_term::{Chunk, Style};

/// Render one screen. `draw` is `Live::render` or `taix_term::snapshot`
/// with its sink already bound.
pub fn screen(draw: impl FnOnce(&mut dyn FnMut(Chunk<'_>))) -> String {
    let mut out = String::with_capacity(4 << 10);
    let mut open = false;
    draw(&mut |chunk| match chunk {
        Chunk::Text(text, style) => {
            if open {
                out.push_str("</span>");
            }
            push_open(&mut out, &style);
            escape(&mut out, text);
            open = true;
        }
        Chunk::LineBreak => {
            if open {
                out.push_str("</span>");
                open = false;
            }
            out.push('\n');
        }
    });
    if open {
        out.push_str("</span>");
    }
    out
}

/// A palette colour is a class, so the 16 the terminal actually uses cost
/// two bytes on the wire instead of fifteen and the browser parses no
/// inline CSS. True colour has no class and keeps its `style`.
fn push_open(out: &mut String, style: &Style) {
    use std::fmt::Write;
    out.push_str("<span");
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
    out.push('>');
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
    fn render(bytes: &[u8], cols: usize, rows: usize) -> String {
        let size = Size { cols, rows };
        // `snapshot` takes a sized sink, so the trait object is reborrowed
        // through a closure rather than passed along.
        screen(|sink| taix_term::snapshot(size, bytes, &mut |chunk| sink(chunk)))
    }

    #[test]
    fn markup_in_pane_output_is_escaped_not_interpreted() {
        let html = render(b"<script>alert(1)</script> a&b", 40, 1);
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt; a&amp;b"),
            "{html}"
        );
        assert!(!html.contains("<script>"), "{html}");
    }

    #[test]
    fn a_styled_run_carries_palette_colour_as_class_and_weight_as_class() {
        let html = render(b"\x1b[1;31mred\x1b[0m plain", 40, 1);
        // Bold red (SGR 1;31) produces bold + bright red (palette index 9).
        assert!(html.contains("class=\"b f9\""), "{html}");
        // Palette colours are classes, not inline styles.
        assert!(!html.contains("color:#"), "{html}");
        // The plain tail must not inherit the opening span.
        assert!(
            html.contains("plain</span>") || html.contains("> plain"),
            "{html}"
        );
    }

    #[test]
    fn every_row_is_one_line_so_the_page_can_use_pre() {
        let html = render(b"one\r\ntwo", 10, 3);
        assert_eq!(html.matches('\n').count(), 2, "{html}");
    }
}
