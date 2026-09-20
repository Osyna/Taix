//! Styled runs from `taix-term` to Pango markup for a `GtkLabel`.
//!
//! This is the whole GTK-specific part of pane rendering; a Ratatui front
//! would consume the same [`Chunk`] stream and build spans instead.
//! ponytail: markup on a label rather than a custom text renderer.

use std::fmt::Write;

use taix_term::{Chunk, Style};

/// Append one chunk. Reusing the caller's `String` keeps a 60 Hz redraw of the
/// focused pane at one allocation per frame.
///
/// `cursor` is `(block, ink)`: the colour of the insertion block and the
/// colour the character under it is drawn in. `None` for a snapshot - an
/// unfocused pane has no insertion point, because keys never go there.
pub fn push(out: &mut String, chunk: Chunk<'_>, cursor: Option<(&str, &str)>) {
    match chunk {
        Chunk::LineBreak => out.push('\n'),
        Chunk::Text(text, style) if style.is_plain() => escape_into(out, text),
        Chunk::Text(text, style) => {
            out.push_str("<span");
            attrs(out, style, cursor);
            out.push('>');
            escape_into(out, text);
            out.push_str("</span>");
        }
    }
}

/// The pane's own text colour, for the cursor block. Read from the label
/// rather than the palette so a user `gtk.css` or a pywal import recolours the
/// cursor along with everything else.
pub fn hex(c: gtk::gdk::RGBA) -> String {
    let byte = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(c.red()),
        byte(c.green()),
        byte(c.blue())
    )
}

fn attrs(out: &mut String, s: Style, cursor: Option<(&str, &str)>) {
    // The cursor is an inverse block, the way every terminal draws one: the
    // pane's text colour as background and the pane's background as ink, so
    // it is solid whatever is under it and the character there stays
    // readable. A translucent wash was invisible over an autosuggestion.
    match cursor.filter(|_| s.cursor) {
        Some((block, ink)) => {
            let _ = write!(out, " foreground=\"{ink}\" background=\"{block}\"");
        }
        None => {
            if let Some(c) = s.fg {
                let _ = write!(out, " foreground=\"#{:02x}{:02x}{:02x}\"", c.r, c.g, c.b);
            }
            if let Some(c) = s.bg {
                let _ = write!(out, " background=\"#{:02x}{:02x}{:02x}\"", c.r, c.g, c.b);
            }
        }
    }
    if s.bold {
        out.push_str(" weight=\"bold\"");
    }
    if s.italic {
        out.push_str(" style=\"italic\"");
    }
    if s.underline {
        out.push_str(" underline=\"single\"");
    }
    if s.strike {
        out.push_str(" strikethrough=\"true\"");
    }
}

/// Escape text that is not a pane chunk - a diff, a git error - for the same
/// reason: one `<` makes Pango reject the whole label.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

/// Pane text is arbitrary agent output; unescaped `<` or `&` would make Pango
/// reject the whole label.
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

#[cfg(test)]
mod tests {
    use super::*;
    use taix_term::Rgb;

    fn render(chunks: &[Chunk<'_>]) -> String {
        render_with(chunks, Some(("#cdd6f4", "#1e1e2e")))
    }

    fn render_with(chunks: &[Chunk<'_>], cursor: Option<(&str, &str)>) -> String {
        let mut out = String::new();
        for c in chunks {
            push(
                &mut out,
                match c {
                    Chunk::LineBreak => Chunk::LineBreak,
                    Chunk::Text(t, s) => Chunk::Text(t, *s),
                },
                cursor,
            );
        }
        out
    }

    #[test]
    fn plain_runs_emit_no_span() {
        assert_eq!(
            render(&[Chunk::Text("hi", Style::default()), Chunk::LineBreak]),
            "hi\n"
        );
    }

    #[test]
    fn styled_run_emits_attributes() {
        let style = Style {
            fg: Some(Rgb {
                r: 0xcd,
                g: 0,
                b: 0,
            }),
            bold: true,
            ..Style::default()
        };
        assert_eq!(
            render(&[Chunk::Text("RED", style)]),
            "<span foreground=\"#cd0000\" weight=\"bold\">RED</span>"
        );
    }

    #[test]
    fn markup_metacharacters_in_pane_output_are_escaped() {
        // Agent output like `if a<b && c>d` must not break the label.
        assert_eq!(
            render(&[Chunk::Text("a<b&c>d", Style::default())]),
            "a&lt;b&amp;c&gt;d"
        );
    }

    #[test]
    fn metacharacters_inside_a_styled_run_are_escaped_too() {
        let style = Style {
            italic: true,
            ..Style::default()
        };
        assert_eq!(
            render(&[Chunk::Text("<x>", style)]),
            "<span style=\"italic\">&lt;x&gt;</span>"
        );
    }

    #[test]
    fn the_cursor_cell_is_an_inverse_block() {
        let style = Style {
            cursor: true,
            ..Style::default()
        };
        assert_eq!(
            render(&[Chunk::Text(" ", style)]),
            "<span foreground=\"#1e1e2e\" background=\"#cdd6f4\"> </span>"
        );
    }

    #[test]
    fn the_cursor_block_replaces_the_cells_own_colours() {
        // Two `background` attributes in one span is not valid markup, and
        // whichever Pango honoured would be a coin toss. The cell's own
        // colours go too: a dim autosuggestion under the block is drawn in
        // the ink colour, or the block hides it.
        let style = Style {
            cursor: true,
            fg: Some(Rgb {
                r: 0x60,
                g: 0x60,
                b: 0x60,
            }),
            bg: Some(Rgb {
                r: 0xcd,
                g: 0,
                b: 0,
            }),
            ..Style::default()
        };
        let out = render(&[Chunk::Text("x", style)]);
        assert_eq!(out.matches("background=").count(), 1);
        assert_eq!(out.matches("foreground=").count(), 1);
        assert!(out.contains("foreground=\"#1e1e2e\" background=\"#cdd6f4\""));
    }

    #[test]
    fn a_snapshot_never_draws_the_block() {
        // Only the focused pane receives keys, so only it gets a cursor -
        // `None` here is the snapshot path, cursor flag or not.
        let style = Style {
            cursor: true,
            ..Style::default()
        };
        assert_eq!(
            render_with(&[Chunk::Text("x", style)], None),
            "<span>x</span>"
        );
    }
}
