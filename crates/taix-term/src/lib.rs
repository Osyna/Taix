//! Headless terminal state: pane bytes in, styled runs out.
//!
//! A focused pane keeps a [`Live`] emulator alive and feeds it tmux `%output`.
//! An unfocused pane calls [`snapshot`] on a `capture-pane -e -p` blob, which
//! builds a grid, emits runs, and drops everything — its residency is whatever
//! the caller keeps, not an emulator.
//!
//! Deliberately UI-agnostic: renders into a [`Chunk`] sink so a GTK front can
//! build Pango markup and a Ratatui front can build spans from the same grid.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};

/// Pane geometry. `total_lines == screen_lines` is what pins scrollback to
/// zero: tmux already owns the history, so the emulator must not preallocate
/// per-line storage on top of it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Resolved cell styling. `None` colours mean "inherit the front-end's
/// default", which is what leaves user theming (gtk.css, pywal) in charge.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// This is the cell the next keystroke lands in. Front-ends draw it as a
    /// block; it is a `Style` field rather than a separate `Chunk` so it
    /// breaks a run exactly like any other styling difference.
    pub cursor: bool,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// One unit of rendered output. Text runs are already coalesced: consecutive
/// cells sharing a [`Style`] arrive as a single `Text`.
#[derive(Debug, PartialEq, Eq)]
pub enum Chunk<'a> {
    Text(&'a str, Style),
    LineBreak,
}

/// A live emulator for the focused pane.
pub struct Live {
    term: Term<VoidListener>,
    parser: Processor,
    size: Size,
}

impl Live {
    pub fn new(size: Size) -> Self {
        Live {
            term: new_term(size),
            parser: Processor::new(),
            size,
        }
    }

    /// Prime a freshly promoted pane from `capture-pane -e -p` so it is not
    /// blank until the next `%output`.
    ///
    /// `cursor` is tmux's `#{cursor_x}`/`#{cursor_y}`. Without it the emulator
    /// resumes wherever the capture's last row ended, and the next `%output`
    /// fragment lands mid-column — which visibly splits the first live line.
    pub fn seed(&mut self, captured: &[u8], cursor: (usize, usize)) {
        feed_capture(&mut self.term, &mut self.parser, self.size, captured);
        let (x, y) = (cursor.0 + 1, cursor.1 + 1);
        self.parser
            .advance(&mut self.term, format!("\x1b[{y};{x}H").as_bytes());
    }

    /// Feed a `%output` fragment. State persists across calls.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// The grid this emulator is running at, for a consumer drawing it
    /// somewhere other than the widget it was sized from.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Renders the cursor cell, which a snapshot cannot: only the focused
    /// pane has a live emulator, and only the focused pane receives keys, so
    /// the block appears exactly where typing will land and nowhere else.
    pub fn render(&self, sink: &mut impl FnMut(Chunk<'_>)) {
        // A full-screen program that hides the cursor (an agent's spinner, vim
        // in visual mode) means "no insertion point"; drawing one would lie.
        let cursor = self.term.mode().contains(TermMode::SHOW_CURSOR).then(|| {
            let p = self.term.grid().cursor.point;
            (p.column.0, p.line.0.max(0) as usize)
        });
        render(&self.term, cursor, sink);
    }

    /// The mouse modes the program has set, live: a TUI turns reporting on
    /// after it starts and off again when it exits, so this is read per
    /// event rather than remembered.
    pub fn mouse(&self) -> Mouse {
        let m = self.term.mode();
        Mouse {
            click: m.contains(TermMode::MOUSE_REPORT_CLICK),
            drag: m.contains(TermMode::MOUSE_DRAG),
            motion: m.contains(TermMode::MOUSE_MOTION),
            sgr: m.contains(TermMode::SGR_MOUSE),
        }
    }
}

/// What the program in a pane asked the terminal to report, from the DEC
/// private modes it set. A TUI (ratatui, htop, vim) is only clickable if
/// something forwards the pointer, and this is the only way to know it wants
/// it: nothing else in the pipeline can tell a full-screen program from a
/// shell.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mouse {
    /// `1000`: presses and releases.
    pub click: bool,
    /// `1002`: motion too, while a button is held.
    pub drag: bool,
    /// `1003`: motion with no button held.
    pub motion: bool,
    /// `1006`: decimal coordinates, which is the only encoding that works
    /// past column 223.
    pub sgr: bool,
}

impl Mouse {
    /// Does the pointer belong to the program at all?
    pub fn wanted(&self) -> bool {
        self.click || self.drag || self.motion
    }
}

/// What happened to the button. A wheel has only [`Kind::Press`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Press,
    Release,
    Motion,
}

/// Button numbers as a mouse report spells them.
pub mod button {
    pub const LEFT: u8 = 0;
    pub const MIDDLE: u8 = 1;
    pub const RIGHT: u8 = 2;
    /// No button held: what a bare hover reports.
    pub const NONE: u8 = 3;
    pub const WHEEL_UP: u8 = 64;
    pub const WHEEL_DOWN: u8 = 65;
}

/// Encode one pointer event the way the program in the pane reads it, or
/// `None` when this event is not part of what it asked for - a hover under
/// `1002`, anything at all under no mouse mode, a wheel release, or an
/// off-screen cell the legacy encoding cannot express.
///
/// `col`/`row` are zero-based cells on the visible screen.
pub fn report(mouse: Mouse, btn: u8, col: usize, row: usize, kind: Kind) -> Option<Vec<u8>> {
    let wanted = match kind {
        Kind::Motion if btn == button::NONE => mouse.motion,
        Kind::Motion => mouse.drag || mouse.motion,
        _ => mouse.wanted(),
    };
    // A wheel click has no release: sending one is a second scroll event to
    // every reader that decodes the button rather than the final byte.
    if !wanted || (btn >= button::WHEEL_UP && kind != Kind::Press) {
        return None;
    }
    let moving = if kind == Kind::Motion { 32 } else { 0 };
    if mouse.sgr {
        let end = if kind == Kind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{};{};{}{end}", btn + moving, col + 1, row + 1).into_bytes());
    }
    // The 1979 encoding: one byte per field, each offset by 32, and a release
    // is button 3 rather than a terminator of its own. 223 is as far as a
    // single byte reaches, and a lie about the cell is worse than silence.
    if col >= 223 || row >= 223 {
        return None;
    }
    let cb = if kind == Kind::Release {
        button::NONE
    } else {
        btn + moving
    };
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + cb,
        33 + col as u8,
        33 + row as u8,
    ])
}

/// `scrolling_history: 0` is the whole point; `Config::default()` is 10_000,
/// which measures ~30 MiB per pane once scrolled through.
fn new_term(size: Size) -> Term<VoidListener> {
    Term::new(
        Config {
            scrolling_history: 0,
            ..Config::default()
        },
        &size,
        VoidListener,
    )
}

/// A capture has no intrinsic size and its rows are separated by bare LFs,
/// which in a real terminal would not return the cursor to column 0.
fn feed_capture(
    term: &mut Term<VoidListener>,
    parser: &mut Processor,
    size: Size,
    captured: &[u8],
) {
    for (i, row) in captured.split(|&b| b == b'\n').enumerate() {
        if i >= size.rows {
            break;
        }
        if i > 0 {
            parser.advance(term, b"\r\n");
        }
        parser.advance(term, row);
    }
}

/// Render a `capture-pane -e -p` blob, retaining nothing.
///
/// The caller supplies geometry from `#{pane_width}`/`#{pane_height}`. No
/// cursor is drawn: a capture carries none, and the grid's own cursor sits
/// wherever the blob happened to end - which is not where anyone is typing.
pub fn snapshot(size: Size, captured: &[u8], sink: &mut impl FnMut(Chunk<'_>)) {
    let mut term = new_term(size);
    let mut parser: Processor = Processor::new();
    feed_capture(&mut term, &mut parser, size, captured);
    render(&term, None, sink);
}

/// `cursor` is a `(column, line)` on the visible screen, or `None` for a
/// snapshot.
fn render(
    term: &Term<VoidListener>,
    cursor: Option<(usize, usize)>,
    sink: &mut impl FnMut(Chunk<'_>),
) {
    let grid = term.grid();
    let (rows, cols) = (grid.screen_lines(), grid.columns());
    let mut run = String::with_capacity(cols * 4);

    for line in 0..rows {
        let row = &grid[Line(line as i32)];
        // Trailing default-styled blanks carry no information; dropping them
        // is most of the output size on a typical pane.
        let end = (0..cols)
            .rposition(|c| {
                let cell = &row[Column(c)];
                cell.c != ' ' || cell.bg != Color::Named(NamedColor::Background)
            })
            .map_or(0, |i| i + 1);
        // The cursor usually sits one past the last character - at a shell
        // prompt it is *always* on a trailing blank - so the row has to keep
        // enough blanks to reach it, or the block has nothing to draw on.
        let at = cursor.and_then(|(x, y)| (y == line).then(|| x.min(cols - 1)));
        let end = end.max(at.map_or(0, |c| c + 1));

        let mut style: Option<Style> = None;
        for c in 0..end {
            let cell = &row[Column(c)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut want = style_of(cell.fg, cell.bg, cell.flags);
            want.cursor = at == Some(c);
            if style != Some(want) {
                if let Some(prev) = style.filter(|_| !run.is_empty()) {
                    sink(Chunk::Text(&run, prev));
                    run.clear();
                }
                style = Some(want);
            }
            run.push(if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            });
        }
        if let Some(prev) = style.filter(|_| !run.is_empty()) {
            sink(Chunk::Text(&run, prev));
            run.clear();
        }
        if line + 1 < rows {
            sink(Chunk::LineBreak);
        }
    }
}

fn style_of(fg: Color, bg: Color, flags: Flags) -> Style {
    let (fg, bg) = if flags.contains(Flags::INVERSE) {
        (bg, fg)
    } else {
        (fg, bg)
    };
    Style {
        cursor: false,
        fg: rgb(fg, flags.contains(Flags::BOLD)),
        bg: rgb(bg, false),
        bold: flags.contains(Flags::BOLD),
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE),
        strike: flags.contains(Flags::STRIKEOUT),
    }
}

fn rgb(color: Color, bold: bool) -> Option<Rgb> {
    match color {
        Color::Spec(c) => Some(Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        }),
        Color::Indexed(i) => Some(indexed(i)),
        Color::Named(NamedColor::Foreground | NamedColor::Background) => None,
        // SGR 1 with an ANSI colour is conventionally the bright variant.
        Color::Named(n) if bold => Some(indexed(named_index(n.to_bright()))),
        Color::Named(n) => Some(indexed(named_index(n))),
    }
}

fn named_index(n: NamedColor) -> u8 {
    match n {
        NamedColor::Cursor | NamedColor::Foreground | NamedColor::BrightForeground => 7,
        NamedColor::Background => 0,
        NamedColor::DimForeground => 8,
        n if (n as usize) < 16 => n as u8,
        // Dim* occupy 259.. in declaration order, mapping back onto 0..8.
        n => (n as usize - NamedColor::DimBlack as usize) as u8,
    }
}

/// The standard xterm 256-colour palette, computed rather than tabulated.
pub fn indexed(i: u8) -> Rgb {
    const BASE16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match i {
        0..=15 => {
            let (r, g, b) = BASE16[i as usize];
            Rgb { r, g, b }
        }
        16..=231 => {
            const STEP: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let n = i as usize - 16;
            Rgb {
                r: STEP[n / 36],
                g: STEP[(n / 6) % 6],
                b: STEP[n % 6],
            }
        }
        _ => {
            let v = 8 + 10 * (i as u16 - 232);
            Rgb {
                r: v as u8,
                g: v as u8,
                b: v as u8,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Size = Size { cols: 20, rows: 3 };

    /// Flatten a render into a comparable script: `text|style-summary`, with
    /// `/` for line breaks.
    fn script(chunks: impl Fn(&mut dyn FnMut(Chunk<'_>))) -> String {
        let mut out = String::new();
        chunks(&mut |c| match c {
            Chunk::LineBreak => out.push('/'),
            Chunk::Text(t, s) => {
                out.push_str(t);
                out.push('|');
                if let Some(c) = s.fg {
                    out.push_str(&format!("fg{:02x}{:02x}{:02x}", c.r, c.g, c.b));
                }
                if let Some(c) = s.bg {
                    out.push_str(&format!("bg{:02x}{:02x}{:02x}", c.r, c.g, c.b));
                }
                for (on, tag) in [
                    (s.bold, "B"),
                    (s.italic, "I"),
                    (s.underline, "U"),
                    (s.strike, "S"),
                    (s.cursor, "C"),
                ] {
                    if on {
                        out.push_str(tag);
                    }
                }
                out.push(';');
            }
        });
        out
    }

    fn snap(size: Size, bytes: &[u8]) -> String {
        script(|sink| snapshot(size, bytes, &mut { sink }))
    }

    #[test]
    fn scrollback_is_not_preallocated() {
        // The RAM target depends on this: tmux owns history, not the emulator.
        let term = new_term(Size { cols: 80, rows: 24 });
        assert_eq!(term.grid().total_lines(), 24);
    }

    #[test]
    fn plain_text_is_one_unstyled_run() {
        assert_eq!(snap(S, b"hello"), "hello|;//");
    }

    #[test]
    fn identical_styling_coalesces_into_one_run() {
        assert_eq!(snap(S, b"\x1b[31mRED\x1b[0m."), "RED|fgcd0000;.|;//");
    }

    #[test]
    fn each_style_change_starts_a_new_run() {
        // Bold over the default foreground keeps the colour theme-inherited.
        assert_eq!(snap(S, b"a\x1b[1mb\x1b[4mc"), "a|;b|B;c|BU;//");
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        // Foreground comes from the default bg, which stays theme-inherited.
        assert_eq!(snap(S, b"\x1b[7;34mX"), "X|bg0000ee;//");
    }

    #[test]
    fn text_is_handed_over_raw_for_the_front_end_to_escape() {
        assert_eq!(snap(S, b"a<b&c>d"), "a<b&c>d|;//");
    }

    #[test]
    fn captured_rows_start_at_column_zero() {
        // A bare LF must not leave row 2 indented by row 1's width.
        assert_eq!(snap(S, b"abc\nxy"), "abc|;/xy|;/");
    }

    #[test]
    fn rows_beyond_pane_height_are_dropped() {
        assert_eq!(snap(Size { cols: 4, rows: 2 }, b"a\nb\nc\nd"), "a|;/b|;");
    }

    #[test]
    fn trailing_blanks_are_trimmed_but_coloured_blanks_survive() {
        assert_eq!(snap(S, b"hi        "), "hi|;//");
        assert_eq!(snap(S, b"\x1b[41m  \x1b[0m"), "  |bgcd0000;//");
    }

    #[test]
    fn palette_covers_cube_and_greyscale() {
        assert_eq!(indexed(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            indexed(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }

    #[test]
    fn live_pane_accumulates_across_feeds() {
        // %output arrives in fragments; state must survive between them.
        let mut live = Live::new(S);
        live.feed(b"\x1b[32mab");
        live.feed(b"cd\x1b[0m");
        assert_eq!(
            script(|sink| live.render(&mut { sink })),
            "abcd|fg00cd00; |C;//"
        );
    }

    #[test]
    fn seeded_pane_resumes_at_the_reported_cursor() {
        // Without the cursor re-injection the next %output landed at the end
        // of the capture's last row, splitting the first live line in two.
        let mut live = Live::new(S);
        live.seed(b"abc\nde", (0, 2));
        live.feed(b"xy");
        assert_eq!(
            script(|sink| live.render(&mut { sink })),
            "abc|;/de|;/xy|; |C;"
        );
    }

    #[test]
    fn seeded_pane_can_resume_mid_row() {
        // A pane whose cursor sits after a partial line must continue it.
        let mut live = Live::new(S);
        live.seed(b"abc\nde", (2, 1));
        live.feed(b"XY");
        assert_eq!(
            script(|sink| live.render(&mut { sink })),
            "abc|;/deXY|; |C;/"
        );
    }

    #[test]
    fn seeded_rows_are_not_run_together() {
        // Bare LFs in a capture must not leave row 2 indented by row 1.
        let mut live = Live::new(S);
        live.seed(b"abc\nxy", (0, 2));
        assert_eq!(script(|sink| live.render(&mut { sink })), "abc|;/xy|;/ |C;");
    }

    #[test]
    fn a_snapshot_draws_no_cursor() {
        // An unfocused pane must not look like the one receiving keys, and a
        // capture's grid cursor is wherever the blob ended anyway.
        assert!(!snap(S, b"hello").contains('C'));
    }

    #[test]
    fn the_cursor_marks_the_cell_the_next_key_lands_in() {
        let mut live = Live::new(S);
        live.feed(b"ab\x1b[1;1H");
        // Column 0 of row 0: the run splits so 'a' carries the cursor.
        assert_eq!(script(|sink| live.render(&mut { sink })), "a|C;b|;//");
    }

    #[test]
    fn a_hidden_cursor_is_not_drawn() {
        // Agents' spinners and full-screen UIs turn the cursor off; drawing
        // one anyway would point at a cell nothing is typing into.
        let mut live = Live::new(S);
        live.feed(b"hi\x1b[?25l");
        assert_eq!(script(|sink| live.render(&mut { sink })), "hi|;//");
    }

    #[test]
    fn a_cursor_past_the_last_column_stays_on_screen() {
        // At the right edge the emulator parks the cursor one past the end;
        // clamping keeps the block on the final cell instead of dropping it.
        let narrow = Size { cols: 3, rows: 1 };
        let mut live = Live::new(narrow);
        live.feed(b"abc");
        assert_eq!(script(|sink| live.render(&mut { sink })), "ab|;c|C;");
    }

    #[test]
    fn mouse_modes_follow_what_the_program_sets() {
        let mut live = Live::new(S);
        assert!(!live.mouse().wanted());
        // What crossterm's `EnableMouseCapture` writes.
        live.feed(b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h");
        let on = live.mouse();
        assert!(on.wanted() && on.motion && on.sgr);
        live.feed(b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l");
        assert_eq!(live.mouse(), Mouse::default());
    }

    #[test]
    fn sgr_reports_are_one_based_and_end_in_the_button_state() {
        let m = Mouse {
            click: true,
            sgr: true,
            ..Mouse::default()
        };
        let go = |kind| report(m, button::LEFT, 4, 9, kind).expect("wanted");
        assert_eq!(go(Kind::Press), b"\x1b[<0;5;10M");
        assert_eq!(go(Kind::Release), b"\x1b[<0;5;10m");
        // Wheels report as high button numbers, and only as a press.
        assert_eq!(
            report(m, button::WHEEL_DOWN, 0, 0, Kind::Press).expect("wanted"),
            b"\x1b[<65;1;1M"
        );
        assert!(report(m, button::WHEEL_DOWN, 0, 0, Kind::Release).is_none());
    }

    #[test]
    fn motion_is_only_reported_when_it_was_asked_for() {
        let click = Mouse {
            click: true,
            sgr: true,
            ..Mouse::default()
        };
        // 1000 alone: presses yes, dragging no, hovering no.
        assert!(report(click, button::LEFT, 0, 0, Kind::Press).is_some());
        assert!(report(click, button::LEFT, 0, 0, Kind::Motion).is_none());
        let drag = Mouse {
            drag: true,
            ..click
        };
        // A held button adds 32 to the button number.
        assert_eq!(
            report(drag, button::LEFT, 1, 1, Kind::Motion).expect("wanted"),
            b"\x1b[<32;2;2M"
        );
        // 1002 reports a drag but not a bare hover; 1003 reports both.
        assert!(report(drag, button::NONE, 1, 1, Kind::Motion).is_none());
        let hover = Mouse {
            motion: true,
            ..click
        };
        assert_eq!(
            report(hover, button::NONE, 1, 1, Kind::Motion).expect("wanted"),
            b"\x1b[<35;2;2M"
        );
        assert!(report(Mouse::default(), button::LEFT, 0, 0, Kind::Press).is_none());
    }

    #[test]
    fn the_legacy_encoding_offsets_every_field_and_gives_up_past_223() {
        let m = Mouse {
            click: true,
            ..Mouse::default()
        };
        assert_eq!(
            report(m, button::LEFT, 4, 9, Kind::Press).expect("wanted"),
            b"\x1b[M\x20\x25\x2a"
        );
        // A release is button 3, not a different terminator.
        assert_eq!(
            report(m, button::LEFT, 4, 9, Kind::Release).expect("wanted"),
            b"\x1b[M\x23\x25\x2a"
        );
        assert!(report(m, button::LEFT, 230, 0, Kind::Press).is_none());
    }
}
