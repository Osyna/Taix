use crate::AgentState;
use std::time::{Duration, Instant};

const MAX_TAIL_SIZE: usize = 4096;

pub struct Watcher {
    idle_after: Duration,
    last_output: Option<Instant>,
    tail: Vec<u8>,
}

impl Watcher {
    pub fn new(idle_after: Duration) -> Watcher {
        Watcher {
            idle_after,
            last_output: None,
            tail: Vec::new(),
        }
    }

    pub fn observe(&mut self, bytes: &[u8], now: Instant) {
        if !bytes.is_empty() {
            self.last_output = Some(now);

            self.tail.extend_from_slice(bytes);

            // Keep only the last MAX_TAIL_SIZE bytes
            if self.tail.len() > MAX_TAIL_SIZE {
                let excess = self.tail.len() - MAX_TAIL_SIZE;
                self.tail.drain(0..excess);
            }
        }
    }

    pub fn state(&self, now: Instant) -> Option<AgentState> {
        let last = self.last_output?;
        let elapsed = now.saturating_duration_since(last);

        if elapsed < self.idle_after {
            return Some(AgentState::Working);
        }

        // Silent for idle_after or more
        if looks_like_prompt(&self.tail) {
            Some(AgentState::Waiting)
        } else {
            Some(AgentState::Idle)
        }
    }

    pub fn finished(&self) -> AgentState {
        if has_error_marker(&self.tail) {
            AgentState::Failed
        } else {
            AgentState::Done
        }
    }
}

/// Strip ANSI escape sequences from bytes
fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // CSI sequence: ESC [ ... (letter or @)
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() && bytes[i] != b'@' {
                i += 1;
            }
            i += 1; // skip the final byte
        } else if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b']' {
            // OSC sequence: ESC ] ... (terminated by BEL or ESC \)
            i += 2;
            while i < bytes.len() {
                if bytes[i] == 0x07 {
                    i += 1;
                    break;
                }
                if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }

    result
}

/// Check if the tail looks like an interactive prompt
pub(crate) fn looks_like_prompt(tail: &[u8]) -> bool {
    if tail.is_empty() {
        return false;
    }

    let stripped = strip_ansi(tail);
    let text = String::from_utf8_lossy(&stripped);

    // Get the last non-blank line
    let last_line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");

    if last_line.is_empty() {
        return false;
    }

    // Trailing whitespace is the norm, not the exception: a prompt waiting
    // for input leaves the cursor one space past the question ("Continue? ",
    // "Password: ", "> "). Testing the raw line missed every one of them, so
    // an agent sitting on a question read as merely idle.
    let last_line = last_line.trim_end();
    if last_line.ends_with('?') || last_line.ends_with(':') || last_line.ends_with('>') {
        return true;
    }

    // Check for y/n affordances (case-insensitive)
    let lower = last_line.to_lowercase();
    if lower.contains("(y/n)")
        || lower.contains("[y/n]")
        || lower.contains("(yes/no)")
        || lower.contains("[yes/no]")
    {
        return true;
    }

    false
}

/// Check if the tail contains error markers
fn has_error_marker(tail: &[u8]) -> bool {
    if tail.is_empty() {
        return false;
    }

    let stripped = strip_ansi(tail);
    let text = String::from_utf8_lossy(&stripped);

    // Get the last non-blank line
    let last_line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");

    let lower = last_line.to_lowercase();
    lower.contains("error")
        || lower.contains("panic")
        || lower.contains("fatal")
        || lower.contains("traceback")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_vs_log_line_classification() {
        // Should be prompts
        assert!(looks_like_prompt(b"Enter value:"));
        assert!(looks_like_prompt(b"Continue? (y/n)"));
        assert!(looks_like_prompt(b"Proceed [Y/n]"));
        assert!(looks_like_prompt(b"Delete file? (yes/no)"));
        assert!(looks_like_prompt(b"user@host>"));
        assert!(looks_like_prompt(b"What would you like?"));

        // Should NOT be prompts (normal log lines)
        assert!(!looks_like_prompt(b"Compiling foo v0.1.0"));
        assert!(!looks_like_prompt(b"Finished in 2.3s"));
        assert!(!looks_like_prompt(b"Downloaded package"));
        assert!(!looks_like_prompt(b""));
        assert!(!looks_like_prompt(b"   \n  \n"));

        // A prompt actually waiting for you leaves the cursor after a space,
        // which is how agents and shells both write them. Checking the raw
        // line missed all of these, so a window sitting on a question showed
        // as idle and raised no notification.
        assert!(looks_like_prompt(b"Continue? "));
        assert!(looks_like_prompt(b"Password: "));
        assert!(looks_like_prompt(b"> "));
        assert!(looks_like_prompt(b"Apply this patch? \r\n"));
        // Trailing space must not turn a log line into a question.
        assert!(!looks_like_prompt(b"Compiling foo v0.1.0 "));
    }

    #[test]
    fn working_to_idle_transition() {
        let mut watcher = Watcher::new(Duration::from_millis(100));
        let start = Instant::now();

        watcher.observe(b"output", start);
        assert_eq!(watcher.state(start), Some(AgentState::Working));

        // Still working just before threshold
        let almost_idle = start + Duration::from_millis(99);
        assert_eq!(watcher.state(almost_idle), Some(AgentState::Working));

        // Idle after threshold (no prompt)
        let past_idle = start + Duration::from_millis(100);
        assert_eq!(watcher.state(past_idle), Some(AgentState::Idle));
    }

    #[test]
    fn waiting_state_with_prompt() {
        let mut watcher = Watcher::new(Duration::from_millis(100));
        let start = Instant::now();

        watcher.observe(b"Continue? (y/n)", start);

        // Working immediately
        assert_eq!(watcher.state(start), Some(AgentState::Working));

        // Waiting after idle_after
        let past_idle = start + Duration::from_millis(100);
        assert_eq!(watcher.state(past_idle), Some(AgentState::Waiting));
    }

    #[test]
    fn finished_done_vs_failed() {
        let mut watcher = Watcher::new(Duration::from_millis(100));
        watcher.observe(b"All tests passed\n", Instant::now());
        assert_eq!(watcher.finished(), AgentState::Done);

        let mut watcher = Watcher::new(Duration::from_millis(100));
        watcher.observe(b"Error: file not found\n", Instant::now());
        assert_eq!(watcher.finished(), AgentState::Failed);

        let mut watcher = Watcher::new(Duration::from_millis(100));
        watcher.observe(
            b"thread 'main' panicked at 'assertion failed'\n",
            Instant::now(),
        );
        assert_eq!(watcher.finished(), AgentState::Failed);

        let mut watcher = Watcher::new(Duration::from_millis(100));
        watcher.observe(b"fatal: repository not found\n", Instant::now());
        assert_eq!(watcher.finished(), AgentState::Failed);
    }

    #[test]
    fn prompt_detection_sees_through_ansi_styling() {
        // Agents colour their prompts; the escape bytes must not hide the `?`
        // and must not make a plain log line look like a question.
        assert!(looks_like_prompt(b"\x1b[1mContinue?\x1b[0m"));
        assert!(looks_like_prompt(b"\x1b[33m[y/N]\x1b[0m"));
        assert!(!looks_like_prompt(
            b"\x1b[32m   Compiling\x1b[0m foo v0.1.0"
        ));
    }

    #[test]
    fn bounded_tail() {
        let mut watcher = Watcher::new(Duration::from_millis(100));
        let now = Instant::now();

        // Write more than MAX_TAIL_SIZE
        let big_chunk = vec![b'x'; MAX_TAIL_SIZE + 1000];
        watcher.observe(&big_chunk, now);

        // Should be capped
        assert!(watcher.tail.len() <= MAX_TAIL_SIZE);
    }

    #[test]
    fn no_observation_returns_none() {
        let watcher = Watcher::new(Duration::from_millis(100));
        assert_eq!(watcher.state(Instant::now()), None);
    }
}
