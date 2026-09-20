//! What a window showed, kept past its pane.
//!
//! tmux owns the scrollback while a pane lives; when the pane is gone - the
//! harness quit, the server was restarted, the machine rebooted - so is the
//! scrollback. So the fronts copy it out on a slow cadence and at exit, one
//! `capture-pane -e -S -` per pane into a cache file named by the agent, and
//! a relaunch `cat`s it back in before running the harness again. Cache, not
//! state: losing it costs a scrollback, never a window.

use std::path::PathBuf;

use crate::AgentId;
use taix_tmux::{PaneId, Tmux, cmd};

/// `$XDG_CACHE_HOME/taix/history/<agent id>`.
pub fn path(agent: AgentId) -> PathBuf {
    crate::config::base_dir(
        std::env::var("XDG_CACHE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        ".cache",
    )
    .join("taix/history")
    .join(agent.to_string())
}

/// Copy the pane's scrollback out. The screen's empty rows below the last
/// line are dropped, or a replay would put twenty blank lines above the
/// prompt; a pane with nothing on it yet writes nothing, so a window that
/// never spoke leaves no file behind.
pub fn save(tmux: &(impl Tmux + ?Sized), agent: AgentId, pane: PaneId) -> std::io::Result<()> {
    let mut bytes = cmd::history_styled(tmux, pane)?;
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    if end == 0 {
        return Ok(());
    }
    bytes.truncate(end);
    bytes.push(b'\n');
    let path = path(agent);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

/// The saved scrollback, if there is one.
pub fn load(agent: AgentId) -> Option<Vec<u8>> {
    std::fs::read(path(agent)).ok()
}

pub fn remove(agent: AgentId) {
    let _ = std::fs::remove_file(path(agent));
}

/// `command`, preceded by a replay of the saved scrollback when there is
/// one. tmux runs a window command through `sh -c`, so the replay is a
/// `cat` in front of an `exec`; an empty command means the shell.
pub fn replaying(agent: AgentId, command: &str) -> String {
    let file = path(agent);
    if !file.is_file() {
        return command.to_string();
    }
    let run = if command.is_empty() {
        "exec \"${SHELL:-sh}\"".to_string()
    } else {
        format!("exec {command}")
    };
    format!(
        "cat {}; {run}",
        crate::terminal::sh(&file.to_string_lossy())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_only_when_a_file_exists() {
        // ponytail: one XDG_CACHE_HOME per test process is enough here.
        let dir = std::env::temp_dir().join(format!("taix-hist-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("taix/history")).unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", &dir) };
        assert_eq!(replaying(7, "claude"), "claude");
        std::fs::write(path(7), b"old\n").unwrap();
        let cmd = replaying(7, "claude");
        let file = path(7).to_string_lossy().into_owned();
        assert_eq!(cmd, format!("cat {file}; exec claude"));
        assert!(replaying(7, "").ends_with("exec \"${SHELL:-sh}\""));
        remove(7);
        assert_eq!(replaying(7, "claude"), "claude");
        let _ = std::fs::remove_dir_all(dir);
    }
}
