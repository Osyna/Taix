//! A project as a plain tmux session: the same windows, split in one tmux
//! window, for a terminal with no TaiX in front of it.
//!
//! Nothing here touches the live session. The panes TaiX shows each own a
//! process, and a process cannot sit in two windows, so this *recreates* the
//! project - same directories, same harness commands, same environment, same
//! arrangement - on the default tmux socket, where `tmux ls` in any terminal
//! will find it. Same UI, not the same content.

use crate::{Agent, Config, Project, ProjectConfig, Store, by_id};

/// A project's stable public id: eight hex digits of FNV-1a over its root.
///
/// Derived, not stored, so it survives a wiped state file and is the same on
/// every machine that checks the project out at the same path. Row ids are
/// not used because they are only unique within one store.
pub fn key(project: &Project) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in project.root.as_os_str().as_encoded_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:08x}", (h >> 32) ^ (h & 0xffff_ffff))
}

/// The project a `-s` argument names: its key first, then its name, then its
/// root path.
pub fn find(store: &Store, id: &str) -> Option<Project> {
    let projects = store.projects().ok()?;
    projects
        .iter()
        .find(|p| key(p) == id)
        .or_else(|| projects.iter().find(|p| p.name == id))
        .or_else(|| projects.iter().find(|p| p.root.as_os_str() == id))
        .cloned()
}

/// The tmux layout that stands in for a GUI pane preset.
pub fn layout_for_preset(preset: &str) -> &'static str {
    match preset {
        "columns" => "even-horizontal",
        "rows" => "even-vertical",
        "main-left" => "main-vertical",
        "main-top" => "main-horizontal",
        _ => "tiled",
    }
}

/// The preset the GUI last saved, read from its layout file. A flat
/// `key=value` file the GUI owns; only the one key is wanted here, and a
/// missing file means the default.
pub fn saved_preset() -> String {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".local/share")));
    data.and_then(|d| std::fs::read_to_string(d.join("taix/layout")).ok())
        .and_then(|text| {
            text.lines().find_map(|l| {
                l.strip_prefix("preset=")
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| "balanced".to_string())
}

/// The shell line that opens the project as a tmux session, attaching to it
/// if it already exists.
///
/// One tmux window named after the project, one pane per TaiX window in the
/// window's directory (its worktree when it has one) running its harness
/// with the project's `.taix.toml` environment, then the layout. Pane
/// titles carry the window names, so `set -g pane-border-status top` shows
/// them.
///
/// `inside_tmux`: attaching from within tmux is refused as nesting, so the
/// session is built detached at the client's size and switched to instead.
pub fn tmux_command(
    cfg: &Config,
    project: &Project,
    agents: &[Agent],
    layout: &str,
    inside_tmux: bool,
) -> String {
    // `=` is tmux's "this exact session, not a prefix match"; `sh` quotes it
    // because zsh would otherwise read it as a command path.
    let name = format!("taix-{}", taix_git::slug(&project.name));
    let session = sh(&name);
    let exact = sh(&format!("={name}"));
    let env: String = ProjectConfig::load(&project.root)
        .env
        .iter()
        .map(|(k, v)| format!(" -e {}", sh(&format!("{k}={v}"))))
        .collect();
    let mut cmd = if inside_tmux {
        format!(
            "tmux switch-client -t {exact} 2>/dev/null || {{ tmux new-session -d -s {session} \
             -x \"$(tput cols)\" -y \"$(tput lines)\" -n {n}",
            n = sh(&project.name)
        )
    } else {
        format!(
            "tmux attach -t {exact} 2>/dev/null || tmux new-session -s {session} -n {n}",
            n = sh(&project.name)
        )
    };
    // Every later command names the session: from inside another tmux the
    // "current" session is the one the shell sits in, and the panes would be
    // split there.
    let at = format!(" -t {}", sh(&format!("={name}:")));
    let mut first = true;
    for agent in agents {
        let cwd = agent.worktree.as_deref().unwrap_or(&project.root);
        let harness = by_id(cfg, &agent.kind);
        if !first {
            cmd.push_str(" \\; split-window");
            cmd.push_str(&at);
        }
        cmd.push_str(&env);
        cmd.push_str(" -c ");
        cmd.push_str(&sh(&cwd.to_string_lossy()));
        if !harness.command.is_empty() {
            cmd.push(' ');
            cmd.push_str(&sh(&harness.command));
        }
        cmd.push_str(" \\; select-pane");
        cmd.push_str(&at);
        cmd.push_str(" -T ");
        cmd.push_str(&sh(&agent.name));
        first = false;
    }
    if agents.is_empty() {
        cmd.push_str(&env);
        cmd.push_str(" -c ");
        cmd.push_str(&sh(&project.root.to_string_lossy()));
    }
    if agents.len() > 1 {
        cmd.push_str(" \\; select-layout");
        cmd.push_str(&at);
        cmd.push(' ');
        cmd.push_str(layout);
    }
    cmd.push_str(" \\; select-pane -t ");
    cmd.push_str(&sh(&format!("={name}:{{start}}.{{top-left}}")));
    if inside_tmux {
        cmd.push_str(&format!(" && tmux switch-client -t {exact}; }}"));
    }
    cmd
}

/// The short form: `taix -t -s <key>` builds the same line from the store.
pub fn taix_command(project: &Project) -> String {
    format!("taix -t -s {}", key(project))
}

/// The shell line that puts a terminal on one *live* TaiX window, alone.
///
/// Unlike [`tmux_command`] this touches the real session: a grouped session
/// on TaiX's own socket shares the windows but has its own current window,
/// so an ssh client lands on `@window` while the desktop keeps looking at
/// whatever it was looking at. `destroy-unattached` throws the group member
/// away on detach; the windows stay, they belong to the main session too.
pub fn attach_command(cfg: &Config, window: u32) -> String {
    format!(
        "tmux -L {sock} new-session -t {exact} \\; select-window -t @{window} \\; set destroy-unattached on",
        sock = sh(&cfg.tmux_socket),
        exact = sh(&format!("={}", cfg.tmux_session)),
    )
}

/// Single-quote for POSIX sh. Nothing is left for the shell to interpret.
///
/// `=` and `~` are safe inside a word and dangerous at the front of one:
/// zsh expands `=foo` to the path of the command `foo` (its `EQUALS`
/// option, on by default) and `~foo` to a home directory. tmux's own
/// "exact session" prefix is `=`, so every target it produced died with
/// `zsh: taix-assets not found` when pasted into a zsh - and worked in
/// bash, which has neither expansion.
pub fn sh(s: &str) -> String {
    let plain = |b: &u8| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(b);
    if !s.is_empty() && !s.starts_with(['=', '~']) && s.bytes().all(|b| plain(&b)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentState;
    use std::path::PathBuf;

    fn project(root: &str) -> Project {
        Project {
            id: 7,
            name: "osyna website".into(),
            root: PathBuf::from(root),
        }
    }

    fn agent(name: &str, kind: &str, worktree: Option<&str>) -> Agent {
        Agent {
            id: 1,
            project: 7,
            name: name.into(),
            kind: kind.into(),
            worktree: worktree.map(PathBuf::from),
            branch: None,
            window: None,
            pane: None,
            color: None,
            named: false,
            state: AgentState::Idle,
        }
    }

    #[test]
    fn key_is_stable_and_path_derived() {
        let a = project("/home/u/proj");
        let b = project("/home/u/proj");
        let c = project("/home/u/other");
        assert_eq!(key(&a), key(&b));
        assert_ne!(key(&a), key(&c));
        assert_eq!(key(&a).len(), 8);
        assert!(key(&a).bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn command_recreates_every_window_and_quotes_what_needs_it() {
        let cfg = Config::default();
        let p = project("/home/u/my proj");
        let agents = vec![
            agent("Claude Code", "claude", None),
            agent("Terminal", "terminal", Some("/home/u/my proj/.wt/x")),
        ];
        let cmd = tmux_command(&cfg, &p, &agents, "main-vertical", false);
        assert!(cmd.starts_with("tmux attach -t '=taix-osyna-website' 2>/dev/null || tmux new-session -s taix-osyna-website -n 'osyna website' -c '/home/u/my proj' claude \\; select-pane -t '=taix-osyna-website:' -T 'Claude Code'"), "{cmd}");
        assert!(
            cmd.contains("\\; split-window -t '=taix-osyna-website:' -c '/home/u/my proj/.wt/x' \\; select-pane -t '=taix-osyna-website:' -T Terminal"),
            "{cmd}"
        );
        assert!(
            cmd.ends_with("\\; select-layout -t '=taix-osyna-website:' main-vertical \\; select-pane -t '=taix-osyna-website:{start}.{top-left}'"),
            "{cmd}"
        );
        // A terminal window launches nothing, so no command argument.
        assert!(!cmd.contains("x ''"), "{cmd}");
    }

    #[test]
    fn inside_tmux_builds_detached_and_switches() {
        let cfg = Config::default();
        let p = project("/p");
        let cmd = tmux_command(&cfg, &p, &[agent("T", "terminal", None)], "tiled", true);
        assert!(cmd.starts_with("tmux switch-client -t '=taix-osyna-website' 2>/dev/null || { tmux new-session -d -s taix-osyna-website -x "), "{cmd}");
        assert!(cmd.ends_with("\\; select-pane -t '=taix-osyna-website:{start}.{top-left}' && tmux switch-client -t '=taix-osyna-website'; }"), "{cmd}");
    }

    #[test]
    fn one_window_gets_no_layout_and_none_gets_a_shell() {
        let cfg = Config::default();
        let p = project("/p");
        let one = tmux_command(&cfg, &p, &[agent("T", "terminal", None)], "tiled", false);
        assert!(!one.contains("select-layout"), "{one}");
        let none = tmux_command(&cfg, &p, &[], "tiled", false);
        assert!(
            none.contains("-n 'osyna website' -c /p \\; select-pane -t '=taix-osyna-website:{start}.{top-left}'"),
            "{none}"
        );
    }

    #[test]
    fn shell_quoting_survives_an_apostrophe() {
        assert_eq!(sh("it's"), "'it'\\''s'");
        assert_eq!(sh("plain-1.0"), "plain-1.0");
        assert_eq!(sh(""), "''");
    }

    #[test]
    fn a_leading_equals_is_quoted_because_zsh_expands_it() {
        // Unquoted `=taix-assets` is a command path to zsh, and the whole
        // pasted line died with "zsh: taix-assets not found".
        assert_eq!(sh("=taix-assets"), "'=taix-assets'");
        assert_eq!(sh("~/proj"), "'~/proj'");
        // Only at the front: `-e FOO=bar` must stay one bare word.
        assert_eq!(sh("FOO=bar"), "FOO=bar");
    }

    #[test]
    fn every_generated_word_is_safe_in_zsh() {
        // The line is pasted into whatever shell the user runs, so no word
        // may start with a character zsh expands.
        let cfg = Config::default();
        let p = project("/home/u/my proj");
        let agents = vec![agent("Claude Code", "claude", None)];
        for line in [
            tmux_command(&cfg, &p, &agents, "tiled", false),
            tmux_command(&cfg, &p, &agents, "tiled", true),
            tmux_command(&cfg, &p, &[], "tiled", false),
        ] {
            for word in line.split_whitespace() {
                assert!(
                    !word.starts_with('=') && !word.starts_with('~'),
                    "{word} in {line}"
                );
            }
        }
    }

    #[test]
    fn presets_map_onto_tmux_layouts() {
        assert_eq!(layout_for_preset("columns"), "even-horizontal");
        assert_eq!(layout_for_preset("balanced"), "tiled");
        assert_eq!(layout_for_preset("nonsense"), "tiled");
    }
}
