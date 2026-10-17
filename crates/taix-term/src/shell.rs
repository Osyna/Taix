//! Shell integration: the commands a pane has run, read off its raw output.
//!
//! A shell that speaks OSC 133 ("semantic prompts") brackets every prompt and
//! command with `ESC ] 133 ; X ST`: `A` prompt starts, `B` the command line
//! starts, `C` its output starts, `D[;exit]` it ended. Nothing else in the
//! pipeline can tell a finished command from a screenful of text, and the
//! marks cost the shell nothing, so this follows them through `%output` the
//! way [`crate::Mouse::scan`] follows mode switches - for every pane, with no
//! emulator.

use std::collections::VecDeque;

/// Commands remembered per pane. Older ones fall off the front.
pub const KEEP: usize = 200;

const MARK: &[u8] = b"\x1b]133;";
/// A mark is a letter and a few digits; a longer "mark" is not one.
const MARK_MAX: usize = 64;
/// A command line longer than this is not worth listing in full.
const TEXT_MAX: usize = 1024;

/// One command that ran in a pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The command line as the shell echoed it; empty when it could not be read.
    pub text: String,
    /// Unix milliseconds when its output started.
    pub started_ms: u64,
    pub duration_ms: u64,
    /// `None` when the shell reported no status.
    pub exit: Option<i32>,
}

/// How far through a control sequence the text between `B` and `C` is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Esc {
    #[default]
    Text,
    /// After `ESC`.
    Start,
    /// After `ESC (` and friends: one more byte belongs to the sequence.
    Skip,
    Csi,
    Osc,
}

/// The command being typed or run.
#[derive(Debug, Default)]
struct Pending {
    /// Echoed bytes since `B`, with the editor's redraws unpicked.
    line: Vec<u8>,
    /// Still collecting `line`: stops at the first newline.
    open: bool,
    /// A carriage return arrived; the next printable byte starts the line over.
    cr: bool,
    esc: Esc,
    ran: bool,
    started_ms: u64,
}

/// Mark scanner and ring of finished commands for one pane.
#[derive(Debug, Default)]
pub struct Shell {
    /// An unfinished mark (or a prefix of one) from the end of the last chunk.
    tail: Vec<u8>,
    pending: Option<Pending>,
    done: VecDeque<Command>,
}

impl Shell {
    /// Finished commands, oldest first.
    pub fn commands(&self) -> impl ExactSizeIterator<Item = &Command> {
        self.done.iter()
    }

    /// Follow the marks in one `%output` chunk. State persists across
    /// chunks, so a mark split in two is still found. Returns whether a
    /// command finished.
    ///
    /// ponytail: the command text is whatever the shell echoed between `B`
    /// and the first newline, with escapes dropped, `\b` as delete and `\r`
    /// as "start over". That is exact for bash and zsh typing, tab
    /// completion and history recall; a command edited mid-line with cursor
    /// motion, or one that wraps a multi-line prompt, can read wrong. Reading
    /// the screen over tmux when `D` arrives would fix it at a fork per
    /// command.
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) -> bool {
        // Nearly every chunk has no marks: borrow it as is.
        let joined;
        let mut buf = if self.tail.is_empty() {
            bytes
        } else {
            let mut v = std::mem::take(&mut self.tail);
            v.extend_from_slice(bytes);
            joined = v;
            &joined[..]
        };
        let mut finished = false;
        loop {
            let Some(at) = find_mark(buf) else {
                // Hold back a mark's beginning cut off by the chunk edge.
                let keep = partial(buf);
                self.text(&buf[..buf.len() - keep]);
                self.tail.extend_from_slice(&buf[buf.len() - keep..]);
                return finished;
            };
            self.text(&buf[..at]);
            let body = &buf[at + MARK.len()..];
            let Some((end, skip)) = terminator(body) else {
                // Unterminated: wait for the rest, unless it is too long to be one.
                if body.len() < MARK_MAX {
                    self.tail.extend_from_slice(&buf[at..]);
                    return finished;
                }
                buf = body;
                continue;
            };
            finished |= self.mark(&body[..end], now_ms);
            buf = &body[end + skip..];
        }
    }

    fn mark(&mut self, payload: &[u8], now_ms: u64) -> bool {
        match payload.first() {
            Some(b'A') => {
                // A new prompt with the last command never closed (a shell
                // that skips `D`): it ran, so list it. One that never ran
                // (a line abandoned with ^C) is dropped by `finish`.
                self.finish(None, now_ms)
            }
            Some(b'B') => {
                self.pending = Some(Pending {
                    open: true,
                    started_ms: now_ms,
                    ..Pending::default()
                });
                false
            }
            Some(b'C') => {
                // fish has no `B`, so `C` can be the first mark of a command.
                let p = self.pending.get_or_insert_with(Pending::default);
                p.open = false;
                p.ran = true;
                p.started_ms = now_ms;
                // fish and kitty send the command itself with the mark.
                if let Some(url) = payload
                    .split(|b| *b == b';')
                    .find_map(|o| o.strip_prefix(b"cmdline_url="))
                {
                    p.line = unescape(url);
                }
                false
            }
            Some(b'D') => {
                // `D;0`, `D;130;aid=1`, or a bare `D`.
                let exit = payload
                    .strip_prefix(b"D;")
                    .and_then(|r| r.split(|b| *b == b';').next())
                    .and_then(|n| std::str::from_utf8(n).ok())
                    .and_then(|n| n.parse().ok());
                self.finish(exit, now_ms)
            }
            _ => false,
        }
    }

    fn finish(&mut self, exit: Option<i32>, now_ms: u64) -> bool {
        let Some(p) = self.pending.take() else {
            return false;
        };
        let text = String::from_utf8_lossy(&p.line).trim().to_string();
        // No `C` means nothing ran: an empty Enter, or a line given up on.
        if !p.ran {
            return false;
        }
        if self.done.len() == KEEP {
            self.done.pop_front();
        }
        self.done.push_back(Command {
            text,
            started_ms: p.started_ms,
            duration_ms: now_ms.saturating_sub(p.started_ms),
            exit,
        });
        true
    }

    /// Echoed bytes between `B` and the newline: unpick the line editor.
    fn text(&mut self, bytes: &[u8]) {
        let Some(p) = self.pending.as_mut().filter(|p| p.open) else {
            return;
        };
        for &b in bytes {
            match p.esc {
                Esc::Start => {
                    p.esc = match b {
                        b'[' => Esc::Csi,
                        b']' => Esc::Osc,
                        b'(' | b')' | b'*' | b'+' | b'#' => Esc::Skip,
                        _ => Esc::Text,
                    }
                }
                Esc::Skip => p.esc = Esc::Text,
                Esc::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        p.esc = Esc::Text;
                    }
                }
                Esc::Osc => match b {
                    0x07 => p.esc = Esc::Text,
                    0x1b => p.esc = Esc::Start,
                    _ => {}
                },
                Esc::Text => match b {
                    0x1b => p.esc = Esc::Start,
                    b'\r' => p.cr = true,
                    b'\n' => {
                        p.open = false;
                        return;
                    }
                    0x08 | 0x7f => {
                        while let Some(last) = p.line.pop() {
                            if last & 0xc0 != 0x80 {
                                break;
                            }
                        }
                    }
                    b if b >= 0x20 => {
                        if std::mem::take(&mut p.cr) {
                            p.line.clear();
                        }
                        if p.line.len() < TEXT_MAX {
                            p.line.push(b);
                        }
                    }
                    _ => {}
                },
            }
        }
    }
}

/// A percent-encoded `cmdline_url` value.
fn unescape(s: &[u8]) -> Vec<u8> {
    let hex = |i: usize| s.get(i).and_then(|b| (*b as char).to_digit(16));
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%'
            && let (Some(h), Some(l)) = (hex(i + 1), hex(i + 2))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// Where the next `ESC ] 133 ;` starts.
fn find_mark(buf: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = buf[from..].iter().position(|b| *b == 0x1b) {
        let at = from + i;
        if buf[at..].starts_with(MARK) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// How many bytes at the end of `buf` could be the start of a mark.
fn partial(buf: &[u8]) -> usize {
    (1..MARK.len())
        .rev()
        .find(|&n| buf.len() >= n && buf[buf.len() - n..] == MARK[..n])
        .unwrap_or(0)
}

/// The end of a mark's payload: its offset and the terminator's length (`BEL`
/// is one byte, `ESC \` is two).
fn terminator(body: &[u8]) -> Option<(usize, usize)> {
    let limit = body.len().min(MARK_MAX);
    (0..limit).find_map(|i| match body[i] {
        0x07 => Some((i, 1)),
        0x1b if body.get(i + 1) == Some(&b'\\') => Some((i, 2)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(s: &Shell) -> Vec<(String, Option<i32>, u64)> {
        s.commands()
            .map(|c| (c.text.clone(), c.exit, c.duration_ms))
            .collect()
    }

    #[test]
    fn a_command_with_bel_and_st_terminators() {
        let mut s = Shell::default();
        assert!(!s.feed(
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls -l\r\n\x1b]133;C\x07",
            1000
        ));
        assert!(s.feed(b"total 0\r\n\x1b]133;D;0\x1b\\", 1250));
        assert!(!s.feed(
            b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\false\r\n\x1b]133;C\x1b\\",
            2000
        ));
        assert!(s.feed(b"\x1b]133;D;1\x07", 2010));
        assert_eq!(
            cmds(&s),
            [
                ("ls -l".into(), Some(0), 250),
                ("false".into(), Some(1), 10)
            ]
        );
        assert_eq!(s.commands().next().unwrap().started_ms, 1000);
    }

    #[test]
    fn d_without_an_exit_code() {
        let mut s = Shell::default();
        s.feed(b"\x1b]133;B\x07pwd\r\n\x1b]133;C\x07/\r\n", 0);
        assert!(s.feed(b"\x1b]133;D\x07", 5));
        assert_eq!(cmds(&s), [("pwd".into(), None, 5)]);
        // Extra options after the code are not part of it.
        s.feed(
            b"\x1b]133;B\x07x\r\n\x1b]133;C\x07\x1b]133;D;127;aid=3\x07",
            9,
        );
        assert_eq!(s.commands().last().unwrap().exit, Some(127));
    }

    #[test]
    fn marks_split_across_chunks() {
        let stream: &[u8] = b"out\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x1b\\\x1b]133;A\x07";
        // Every possible split point, including inside the introducer, the
        // payload and the two-byte terminator.
        for cut in 0..stream.len() {
            let mut s = Shell::default();
            let first = s.feed(&stream[..cut], 0);
            let second = s.feed(&stream[cut..], 10);
            assert!(first || second, "cut at {cut}");
            assert_eq!(s.commands().len(), 1, "cut at {cut}");
            assert_eq!(s.commands().next().unwrap().text, "echo hi", "cut at {cut}");
            assert_eq!(s.commands().next().unwrap().exit, Some(0), "cut at {cut}");
        }
        // One byte at a time.
        let mut s = Shell::default();
        for b in stream {
            s.feed(&[*b], 0);
        }
        assert_eq!(s.commands().len(), 1);
    }

    #[test]
    fn the_line_editor_is_unpicked() {
        let mut s = Shell::default();
        // Typed "lx", backspaced, "s"; the editor then redraws the line and an
        // escape sequence colours part of it.
        s.feed(b"\x1b]133;B\x07lx\x08\x1b[Ks", 0);
        s.feed(
            b"\r\x1b[1mls\x1b[0m -a\r\n\x1b]133;C\x07\x1b]133;D;0\x07",
            1,
        );
        assert_eq!(cmds(&s), [("ls -a".into(), Some(0), 0)]);
    }

    #[test]
    fn an_empty_enter_is_not_a_command() {
        let mut s = Shell::default();
        s.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;D;0\x07", 0);
        assert_eq!(s.commands().len(), 0);
    }

    #[test]
    fn the_ring_is_bounded() {
        let mut s = Shell::default();
        for i in 0..KEEP + 5 {
            s.feed(
                format!("\x1b]133;B\x07c{i}\r\n\x1b]133;C\x07\x1b]133;D;0\x07").as_bytes(),
                0,
            );
        }
        assert_eq!(s.commands().len(), KEEP);
        assert_eq!(s.commands().next().unwrap().text, "c5");
    }

    #[test]
    fn fish_sends_the_command_with_c_and_has_no_b() {
        let mut s = Shell::default();
        s.feed(b"\x1b]133;A\x1b\\> ", 0);
        s.feed(b"\x1b]133;C;cmdline_url=echo%20hi%3B%20ls\x1b\\hi\r\n", 100);
        assert!(s.feed(b"\x1b]133;D;2\x1b\\", 160));
        assert_eq!(cmds(&s), [("echo hi; ls".into(), Some(2), 60)]);
    }

    #[test]
    fn plain_output_is_untouched() {
        let mut s = Shell::default();
        assert!(!s.feed(b"\x1b[31mred\x1b[0m \x1b]0;title\x07 \x1b", 0));
        assert!(!s.feed(b"]0;x\x07done", 0));
        assert_eq!(s.commands().len(), 0);
    }
}
