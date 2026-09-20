//! Per-window transcript logs, appended and rotated.

use crate::Config;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const ROTATION_THRESHOLD: u64 = 4 * 1024 * 1024; // 4 MiB

/// Per-window transcript log directory: `$XDG_DATA_HOME/taix/logs`.
pub fn dir(_cfg: &Config) -> PathBuf {
    let base = crate::config::base_dir(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        ".local/share",
    );
    base.join("taix").join("logs")
}

/// Path to a window's transcript log file.
pub fn path(cfg: &Config, project: &str, window: &str) -> PathBuf {
    let project_slug = slug(project);
    let window_slug = slug(window);
    dir(cfg)
        .join(project_slug)
        .join(format!("{window_slug}.log"))
}

/// Transcript log handle for a specific window.
///
/// Buffered on purpose. A streaming agent emits hundreds of `%output`
/// chunks a second and each one used to cost a write, a flush and an
/// `fstat` on the front's main thread - which is what made a busy window
/// stutter. The bytes are held until the buffer fills or `flush` is called
/// on the app's slow cadence, and the length is counted rather than asked
/// for.
pub struct Log {
    file: io::BufWriter<File>,
    path: PathBuf,
    len: u64,
}

impl Log {
    /// Open a log file for appending. Creates parent directories and the file.
    pub fn open(cfg: &Config, project: &str, window: &str) -> io::Result<Log> {
        let log_path = path(cfg, project, window);
        let project_dir = log_path.parent().expect("log path has parent");
        fs::create_dir_all(project_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let len = file.metadata()?.len();
        Ok(Log {
            file: io::BufWriter::new(file),
            path: log_path,
            len,
        })
    }

    /// Append bytes to the log, stripping ANSI sequences. Rotates at 4 MiB.
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let stripped = strip_ansi(bytes);
        self.file.write_all(&stripped)?;
        self.len += stripped.len() as u64;

        if self.len >= ROTATION_THRESHOLD {
            self.rotate()?;
        }
        Ok(())
    }

    /// Push what is buffered to disk. Called on a slow cadence and at exit,
    /// so `Copy transcript path` hands over a file that is up to date.
    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    /// Path to the current log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rotate: rename current to .1, .1 to .2, drop .2 if it exists.
    fn rotate(&mut self) -> io::Result<()> {
        // Everything buffered belongs to the generation being rotated out.
        self.file.flush()?;

        let gen2 = self.path.with_extension("log.2");
        let gen1 = self.path.with_extension("log.1");

        let _ = fs::remove_file(&gen2);
        if gen1.exists() {
            let _ = fs::rename(&gen1, &gen2);
        }
        fs::rename(&self.path, &gen1)?;

        self.file = io::BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.len = 0;
        Ok(())
    }
}
/// Strip ANSI escape sequences from bytes.
///
/// Handles:
/// Strip ANSI escape sequences from bytes.
///
/// Handles:
/// - CSI sequences: `ESC [ ... final` (final in `@-~`)
/// - OSC sequences: `ESC ] ... BEL` or `ESC ] ... ESC \`
/// - Two-byte escapes: `ESC <char>`
///
/// UTF-8 is passed through. Truncated sequences at buffer end are dropped.
pub fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let Some(&b) = bytes.get(i) else { break };
        if b == 0x1b {
            // ESC
            let Some(&next) = bytes.get(i + 1) else { break };
            match next {
                b'[' => {
                    // CSI
                    i += 2;
                    while let Some(&c) = bytes.get(i) {
                        i += 1;
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                }
                b']' => {
                    // OSC
                    i += 2;
                    let mut terminated = false;
                    while i < bytes.len() {
                        let Some(&c) = bytes.get(i) else { break };
                        if c == 0x07 {
                            i += 1;
                            terminated = true;
                            break;
                        }
                        if c == 0x1b
                            && let Some(&b'\\') = bytes.get(i + 1)
                        {
                            i += 2;
                            terminated = true;
                            break;
                        }
                        i += 1;
                    }
                    if !terminated {
                        break; // truncated
                    }
                }
                _ => {
                    i += 2; // two-byte escape
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    out
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_csi_sequences() {
        assert_eq!(strip_ansi(b"\x1b[31mred\x1b[0m"), b"red");
        assert_eq!(strip_ansi(b"plain"), b"plain");
        assert_eq!(strip_ansi(b"\x1b[1;32mbold green\x1b[m"), b"bold green");
    }

    #[test]
    fn strip_osc_sequences() {
        assert_eq!(strip_ansi(b"\x1b]0;title\x07text"), b"text");
        assert_eq!(strip_ansi(b"\x1b]8;;http://x\x1b\\link"), b"link");
    }

    #[test]
    fn strip_two_byte_escapes() {
        assert_eq!(strip_ansi(b"before\x1bMafter"), b"beforeafter");
    }

    #[test]
    fn truncated_sequences_dropped() {
        assert_eq!(strip_ansi(b"text\x1b["), b"text");
        assert_eq!(strip_ansi(b"text\x1b"), b"text");
        assert_eq!(strip_ansi(b"text\x1b]0;title"), b"text");
    }

    #[test]
    fn utf8_passes_through() {
        assert_eq!(strip_ansi("hello 世界".as_bytes()), "hello 世界".as_bytes());
        assert_eq!(
            strip_ansi("\x1b[32m✓\x1b[0m done".as_bytes()),
            "✓ done".as_bytes()
        );
    }

    #[test]
    fn log_append_and_rotation() {
        let _env = crate::testenv::redirect("XDG_DATA_HOME", "log-append");
        let cfg = Config::default();
        let mut log = Log::open(&cfg, "test-proj", "test-win").unwrap();

        log.append(b"line 1\n").unwrap();
        log.append(b"\x1b[31mline 2\x1b[0m\n").unwrap();
        // Buffered until asked for: what a reader of the transcript path
        // gets is what has been flushed.
        log.flush().unwrap();

        let content = fs::read_to_string(log.path()).unwrap();
        assert!(content.contains("line 1"));
        assert!(content.contains("line 2"));
        assert!(!content.contains("\x1b["));

        // Force rotation by writing enough data
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..5 {
            log.append(&chunk).unwrap();
        }

        let gen1 = log.path.with_extension("log.1");
        assert!(gen1.exists(), "rotation should create .1");

        // Current file should be smaller than threshold after rotation
        let meta = fs::metadata(log.path()).unwrap();
        assert!(meta.len() < ROTATION_THRESHOLD);
    }

    #[test]
    fn rotation_keeps_two_generations() {
        let _env = crate::testenv::redirect("XDG_DATA_HOME", "log-rotation");
        let cfg = Config::default();
        let mut log = Log::open(&cfg, "rot-test", "window").unwrap();

        let chunk = vec![b'a'; 1024 * 1024];
        for _ in 0..5 {
            log.append(&chunk).unwrap();
        }
        let gen1 = log.path.with_extension("log.1");
        assert!(gen1.exists());

        for _ in 0..5 {
            log.append(&chunk).unwrap();
        }
        let gen2 = log.path.with_extension("log.2");
        assert!(gen2.exists());
        assert!(gen1.exists());

        // Third rotation should keep only .1 and .2
        for _ in 0..5 {
            log.append(&chunk).unwrap();
        }
        assert!(gen1.exists());
        assert!(gen2.exists());
    }
}
