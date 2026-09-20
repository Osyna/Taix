//! GDK key events -> tmux key arguments.
//!
//! A window is a real terminal, so the focused pane must receive the whole
//! keyboard: Tab for completion, Ctrl-C to interrupt, arrows for history,
//! Escape for vim and for agent TUIs. TaiX therefore claims almost nothing
//! and forwards the rest verbatim.
//!
//! Kept free of widgets and of `App` so the mapping is unit-testable without
//! a display or a tmux server.

use gtk::gdk::{Key, ModifierType};

/// One keystroke, in the three shapes a pane can be fed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    /// Send literally (`send-keys -l`), so `q` stays a `q`.
    Text(String),
    /// A tmux key name: `Enter`, `BSpace`, `C-c`, `M-b`, `F5`.
    Named(String),
    /// Exact bytes, for combinations tmux has no name for.
    ///
    /// `send-keys` types an unrecognised name as literal text, so asking it
    /// for `C-BSpace` wrote "C-BSpace" on the command line instead of
    /// deleting a word. These are spelled as the bytes a real terminal emits.
    Bytes(Vec<u8>),
}

/// The tmux name of a non-printing key, or `None` if it prints.
///
/// Checked before Unicode because the interesting ones lie about themselves:
/// Return is `\r`, Tab is `\t`, Escape is `\x1b`. tmux wants the name.
fn named(key: Key) -> Option<&'static str> {
    Some(match key {
        Key::Return | Key::KP_Enter => "Enter",
        Key::Tab | Key::KP_Tab => "Tab",
        Key::ISO_Left_Tab => "BTab",
        Key::BackSpace => "BSpace",
        Key::Escape => "Escape",
        Key::Up | Key::KP_Up => "Up",
        Key::Down | Key::KP_Down => "Down",
        Key::Left | Key::KP_Left => "Left",
        Key::Right | Key::KP_Right => "Right",
        Key::Home | Key::KP_Home => "Home",
        Key::End | Key::KP_End => "End",
        Key::Page_Up | Key::KP_Page_Up => "PPage",
        Key::Page_Down | Key::KP_Page_Down => "NPage",
        Key::Delete | Key::KP_Delete => "DC",
        Key::Insert | Key::KP_Insert => "IC",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        _ => return None,
    })
}

/// Translate one key press for the focused pane.
///
/// `None` means "nothing sensible to send" - a bare modifier, or a
/// combination no terminal emits. Never guess: a wrong byte in a shell is
/// worse than a dropped one.
pub fn translate(key: Key, modifiers: ModifierType) -> Option<Press> {
    let ctrl = modifiers.contains(ModifierType::CONTROL_MASK);
    let alt = modifiers.contains(ModifierType::ALT_MASK);
    let shift = modifiers.contains(ModifierType::SHIFT_MASK);

    // Modified keys tmux cannot name. Measured against tmux 3.7: it accepts
    // `C-Left`, `C-Delete`, `C-Space`, but types `C-BSpace` and `C-Escape`
    // out as text. These are the byte sequences xterm-family terminals send.
    match (key, ctrl, alt) {
        // Word-delete. xterm-family terminals send `^H` for Ctrl+Backspace
        // and leave the meaning to the shell, where it usually deletes one
        // character - which is not what anyone presses it for. TaiX sends
        // what readline and zsh both bind to `backward-kill-word` instead,
        // the same bytes as Alt+Backspace.
        (Key::BackSpace, true, false) | (Key::BackSpace, false, true) => {
            return Some(Press::Bytes(vec![0x1b, 0x7f]));
        }
        (Key::BackSpace, true, true) => return Some(Press::Bytes(vec![0x1b, 0x08])),
        // Ctrl+Enter is a plain carriage return on a terminal.
        (Key::Return | Key::KP_Enter, true, false) => return Some(Press::Bytes(vec![0x0d])),
        (Key::Return | Key::KP_Enter, false, true) => return Some(Press::Bytes(vec![0x1b, 0x0d])),
        // Ctrl+Escape and Ctrl+Tab are not keys a terminal receives; Tab is
        // TaiX's own pane-cycling chord.
        (Key::Escape, true, _) => return None,
        (Key::Tab | Key::KP_Tab | Key::ISO_Left_Tab, true, _) => return None,
        _ => {}
    }

    // Shift+Enter: a newline in the composer, not a submit. A bare terminal
    // has no such key - it is `ESC CR` (Meta+Return) that every agent CLI
    // binds to "insert a line", which is exactly what their own setup step
    // teaches a terminal to send. Alt+Enter above sends the same bytes, so
    // the two habits agree.
    if shift && !ctrl && !alt && matches!(key, Key::Return | Key::KP_Enter) {
        return Some(Press::Bytes(vec![0x1b, 0x0d]));
    }

    if let Some(name) = named(key) {
        let prefix = if ctrl {
            "C-"
        } else if alt {
            "M-"
        } else {
            ""
        };
        return Some(Press::Named(format!("{prefix}{name}")));
    }

    let ch = key.to_unicode()?;
    if ctrl {
        // tmux spells the control range as `C-<char>`, and names the one
        // that has no printable form.
        if ch == ' ' {
            return Some(Press::Named("C-Space".into()));
        }
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphabetic() || matches!(lower, '[' | ']' | '\\' | '^' | '_' | '@' | '?')
        {
            return Some(Press::Named(format!("C-{lower}")));
        }
        return None;
    }
    if alt {
        return Some(Press::Named(format!("M-{ch}")));
    }
    // Control characters with no name above would be sent as raw bytes.
    if (ch as u32) < 0x20 {
        return None;
    }
    Some(Press::Text(ch.to_string()))
}

/// Quote a dropped or pasted path for a shell, and for an agent reading a
/// line of prose. A trailing space separates it from whatever is typed next.
pub fn path_argument(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    if raw.contains(|c: char| c.is_whitespace() || "'\"\\$`".contains(c)) {
        format!("'{}' ", raw.replace('\'', r"'\''"))
    } else {
        format!("{raw} ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: ModifierType = ModifierType::empty();
    const CTRL: ModifierType = ModifierType::CONTROL_MASK;
    const SHIFT: ModifierType = ModifierType::SHIFT_MASK;
    const ALT: ModifierType = ModifierType::ALT_MASK;

    #[test]
    fn printable_keys_are_typed_not_interpreted() {
        // The whole point: a bare `q` reaches the shell instead of quitting.
        assert_eq!(translate(Key::q, NONE), Some(Press::Text("q".into())));
        assert_eq!(translate(Key::Q, SHIFT), Some(Press::Text("Q".into())));
        assert_eq!(translate(Key::space, NONE), Some(Press::Text(" ".into())));
    }

    #[test]
    fn interrupt_and_eof_reach_the_pane() {
        assert_eq!(translate(Key::c, CTRL), Some(Press::Named("C-c".into())));
        assert_eq!(translate(Key::d, CTRL), Some(Press::Named("C-d".into())));
        // Shift must not change the control code the shell sees.
        assert_eq!(
            translate(Key::C, CTRL | SHIFT),
            Some(Press::Named("C-c".into()))
        );
    }

    #[test]
    fn editing_keys_use_tmux_names() {
        assert_eq!(
            translate(Key::Return, NONE),
            Some(Press::Named("Enter".into()))
        );
        assert_eq!(translate(Key::Tab, NONE), Some(Press::Named("Tab".into())));
        assert_eq!(
            translate(Key::BackSpace, NONE),
            Some(Press::Named("BSpace".into()))
        );
        assert_eq!(
            translate(Key::Escape, NONE),
            Some(Press::Named("Escape".into()))
        );
        assert_eq!(
            translate(Key::Page_Up, NONE),
            Some(Press::Named("PPage".into()))
        );
        assert_eq!(translate(Key::F5, NONE), Some(Press::Named("F5".into())));
    }

    #[test]
    fn shift_enter_inserts_a_line_instead_of_submitting() {
        // The composer habit: Shift+Enter must not send the agent's prompt.
        // `ESC CR` is what Alt/Option+Enter sends, which is the sequence the
        // agent CLIs bind to "new line".
        assert_eq!(
            translate(Key::Return, SHIFT),
            Some(Press::Bytes(vec![0x1b, 0x0d]))
        );
        assert_eq!(
            translate(Key::KP_Enter, SHIFT),
            Some(Press::Bytes(vec![0x1b, 0x0d]))
        );
        // Plain Enter still submits, and Ctrl+Enter is still a bare CR.
        assert_eq!(
            translate(Key::Return, NONE),
            Some(Press::Named("Enter".into()))
        );
        assert_eq!(translate(Key::Return, CTRL), Some(Press::Bytes(vec![0x0d])));
    }

    #[test]
    fn modified_keys_tmux_cannot_name_are_sent_as_bytes() {
        // The bug: `send-keys C-BSpace` typed the string "C-BSpace" into the
        // shell, because tmux has no such key name.
        // Both spellings of "kill the previous word" send the bytes every
        // shell binds to it.
        assert_eq!(
            translate(Key::BackSpace, CTRL),
            Some(Press::Bytes(vec![0x1b, 0x7f]))
        );
        assert_eq!(
            translate(Key::BackSpace, ALT),
            Some(Press::Bytes(vec![0x1b, 0x7f]))
        );
        assert_eq!(translate(Key::Return, CTRL), Some(Press::Bytes(vec![0x0d])));
        // Nothing sensible exists for these, so nothing is sent.
        assert_eq!(translate(Key::Escape, CTRL), None);
        assert_eq!(translate(Key::Tab, CTRL), None);
    }

    #[test]
    fn modified_keys_tmux_does_name_keep_using_it() {
        // Verified against tmux 3.7: these produce the right escape
        // sequences, so spelling them out as bytes would be duplication.
        assert_eq!(
            translate(Key::Left, CTRL),
            Some(Press::Named("C-Left".into()))
        );
        assert_eq!(
            translate(Key::Delete, CTRL),
            Some(Press::Named("C-DC".into()))
        );
        assert_eq!(
            translate(Key::Left, ALT),
            Some(Press::Named("M-Left".into()))
        );
    }

    #[test]
    fn meta_prefixes_words_and_named_keys() {
        assert_eq!(translate(Key::b, ALT), Some(Press::Named("M-b".into())));
        assert_eq!(
            translate(Key::Left, ALT),
            Some(Press::Named("M-Left".into()))
        );
    }

    #[test]
    fn unmappable_presses_are_dropped() {
        // A bare modifier has no character and must not send a byte.
        assert_eq!(translate(Key::Shift_L, NONE), None);
        assert_eq!(translate(Key::Control_L, CTRL), None);
        // Ctrl with a digit has no tmux name; guessing would be wrong.
        assert_eq!(translate(Key::_1, CTRL), None);
    }

    #[test]
    fn dropped_paths_survive_spaces_and_quotes() {
        use std::path::Path;
        assert_eq!(path_argument(Path::new("/tmp/a.png")), "/tmp/a.png ");
        assert_eq!(
            path_argument(Path::new("/tmp/my shot.png")),
            "'/tmp/my shot.png' "
        );
        assert_eq!(
            path_argument(Path::new("/tmp/it's.png")),
            r"'/tmp/it'\''s.png' "
        );
    }
}
