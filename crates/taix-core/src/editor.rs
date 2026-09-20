//! What "open" means for a file: the editor it goes to, and whether that
//! editor is a desktop application to launch or a terminal program that
//! needs a window of its own.
//!
//! Nothing here launches anything. The fronts do that, because launching a
//! desktop app is a GIO call and the CLI must not link GIO. This module
//! answers "which editor" - from `config.toml`, else from `$VISUAL` and
//! `$EDITOR`, else from what is on `PATH` - and "does it need a terminal".

use crate::{Config, which};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    /// Catalogue id, or the command itself for one the catalogue does not
    /// know. This is what `editor = "..."` in the config holds.
    pub id: String,
    pub label: String,
    /// The command the file's path is appended to.
    pub command: String,
    /// Runs in a terminal: opened in a TaiX window rather than launched.
    pub terminal: bool,
}

/// Desktop editors first: "open" means a desktop app to most people, and a
/// terminal editor that is only ever on the machine as someone's dependency
/// should not win by being installed. `$VISUAL` and `$EDITOR` beat the whole
/// list - that is the user having said which.
const CATALOG: &[(&str, &str, &str, bool)] = &[
    ("code", "VS Code", "code", false),
    ("codium", "VSCodium", "codium", false),
    ("zed", "Zed", "zed", false),
    ("cursor", "Cursor", "cursor", false),
    ("windsurf", "Windsurf", "windsurf", false),
    ("subl", "Sublime Text", "subl", false),
    ("kate", "Kate", "kate", false),
    (
        "gnome-text-editor",
        "Text Editor",
        "gnome-text-editor",
        false,
    ),
    ("nvim", "Neovim", "nvim", true),
    ("vim", "Vim", "vim", true),
    ("hx", "Helix", "hx", true),
    ("emacs", "Emacs", "emacs -nw", true),
    ("micro", "Micro", "micro", true),
    ("nano", "Nano", "nano", true),
    ("kak", "Kakoune", "kak", true),
];

fn entry((id, label, command, terminal): &(&str, &str, &str, bool)) -> Editor {
    Editor {
        id: id.to_string(),
        label: label.to_string(),
        command: command.to_string(),
        terminal: *terminal,
    }
}

/// The catalogue entry with this id.
pub fn by_id(id: &str) -> Option<Editor> {
    CATALOG.iter().find(|e| e.0 == id).map(entry)
}

/// An editor spelled as a command: `nvim`, `/usr/bin/code -w`,
/// `emacsclient -t`. A program the catalogue knows keeps its label and its
/// terminal flag and the user's own flags; an unknown one is assumed to be a
/// terminal program, which is what `$EDITOR` has always meant.
pub fn from_command(command: &str) -> Option<Editor> {
    let command = command.trim();
    let program = command.split_whitespace().next()?;
    let base = program.rsplit('/').next().unwrap_or(program);
    Some(
        match CATALOG.iter().find(|e| e.2.split(' ').next() == Some(base)) {
            Some(known) => Editor {
                command: command.to_string(),
                ..entry(known)
            },
            None => Editor {
                id: command.to_string(),
                label: base.to_string(),
                command: command.to_string(),
                terminal: true,
            },
        },
    )
}

/// What `$VISUAL`, then `$EDITOR`, name - if it is actually installed.
fn from_env(visual: Option<&str>, editor: Option<&str>) -> Option<Editor> {
    [visual, editor]
        .into_iter()
        .flatten()
        .filter_map(from_command)
        .find(|e| installed_command(&e.command))
}

fn installed_command(command: &str) -> bool {
    command
        .split_whitespace()
        .next()
        .is_some_and(|program| which(program).is_some())
}

/// Every editor this machine can open a file with, in offer order: the
/// environment's choice first, then the catalogue entries on `PATH`.
pub fn installed() -> Vec<Editor> {
    let env = from_env(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
    );
    let known: Vec<Editor> = CATALOG
        .iter()
        .map(entry)
        .filter(|e| env.as_ref().is_none_or(|have| have.id != e.id))
        .filter(|e| installed_command(&e.command))
        .collect();
    env.into_iter().chain(known).collect()
}

/// The editor "open" uses: the config's, else the first installed one.
///
/// The config value is an id or a command, tried in that order, so `"zed"`
/// and `"emacsclient -t"` both work and neither needs a second field.
pub fn resolve(cfg: &Config) -> Option<Editor> {
    match cfg.editor.as_deref() {
        Some(chosen) => by_id(chosen).or_else(|| from_command(chosen)),
        None => installed().into_iter().next(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_program_keeps_its_label_and_the_users_flags() {
        let e = from_command("/usr/local/bin/code --wait").unwrap();
        assert_eq!(
            (e.id.as_str(), e.label.as_str(), e.terminal),
            ("code", "VS Code", false)
        );
        assert_eq!(e.command, "/usr/local/bin/code --wait");
        let e = from_command("emacs -nw").unwrap();
        assert!(e.terminal);
    }

    #[test]
    fn an_unknown_program_is_a_terminal_editor_named_after_itself() {
        let e = from_command("  emacsclient -t ").unwrap();
        assert_eq!(e.label, "emacsclient");
        assert_eq!(e.id, "emacsclient -t");
        assert!(e.terminal);
        assert_eq!(from_command("   "), None);
    }

    #[test]
    fn the_config_wins_over_the_environment_and_accepts_a_command() {
        let mut cfg = Config {
            editor: Some("hx".into()),
            ..Config::default()
        };
        assert_eq!(resolve(&cfg).unwrap().label, "Helix");
        cfg.editor = Some("my-editor --flag".into());
        let e = resolve(&cfg).unwrap();
        assert_eq!((e.label.as_str(), e.terminal), ("my-editor", true));
    }

    #[test]
    fn the_environment_only_counts_when_its_program_exists() {
        // `sh` is on every PATH; the other name is on none.
        let e = from_env(Some("no-such-editor-anywhere"), Some("sh -c")).unwrap();
        assert_eq!(e.command, "sh -c");
        assert_eq!(from_env(None, Some("no-such-editor-anywhere")), None);
    }
}
