//! What can occupy a tmux window: a coding agent, or nothing at all.
//!
//! A window is not "an agent with a goal". It is a shell in a project
//! directory that may already be running a harness. So the only choice the
//! user makes is *which harness*, and the default choice is "none".
//!
//! The catalogue below is deliberately larger than any one machine needs.
//! [`available`] intersects it with what is actually on `PATH`, so the menu
//! shows real options rather than a wishlist.

use crate::{Agent, AgentKind, Config};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

/// One thing that can occupy a tmux window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    /// Stable id, stored on the agent row: "terminal", "claude", "omp", ...
    pub id: String,
    /// Display name: "Terminal", "Claude Code", "Oh My Pi", ...
    pub label: String,
    /// Shell command tmux runs. Empty means a plain login shell.
    pub command: String,
    /// Bundled icon name or absolute image path. `None` = the front's
    /// generic glyph.
    pub icon: Option<String>,
    /// One of the window tints, worn by every window of this harness that
    /// has no colour of its own.
    pub color: Option<String>,
}

/// The id reserved for "just a shell, nothing launched".
pub const TERMINAL: &str = "terminal";

/// The id reserved for an embedded browser window.
pub const BROWSER: &str = "browser";

impl Harness {
    /// True for the plain-terminal entry: no harness is launched in it.
    pub fn is_terminal(&self) -> bool {
        self.id == TERMINAL
    }

    /// True for the browser entry: a WebKit window with no tmux pane.
    pub fn is_browser(&self) -> bool {
        self.id == BROWSER
    }

    fn new(id: &str, label: &str, command: &str) -> Harness {
        Harness {
            id: id.to_string(),
            label: label.to_string(),
            command: command.to_string(),
            icon: None,
            color: None,
        }
    }

    /// The catalogue entry with the user's `[agents.<id>]` changes on top.
    /// Empty label/command keep the catalogue's; that is what lets a table
    /// carry only a colour.
    fn customised(mut self, custom: Option<&AgentKind>) -> Harness {
        if let Some(c) = custom {
            if !c.label.is_empty() {
                self.label = c.label.clone();
            }
            if !c.command.is_empty() {
                self.command = clean(&c.command);
            }
            self.icon = c.icon.clone().or(self.icon);
            self.color = c.color.clone().or(self.color);
        }
        self
    }
}

/// The plain terminal, always available: it needs nothing installed.
pub fn terminal() -> Harness {
    let mut h = Harness::new(TERMINAL, "Terminal", "");
    h.icon = Some("terminal".into());
    h
}

/// The embedded browser, always available: it is built into the desktop.
pub fn browser() -> Harness {
    let mut h = Harness::new(BROWSER, "Browser", "");
    h.icon = Some("web-browser-symbolic".into());
    h
}

/// Catalogue ids that ship with a brand icon of the same name.
const BRANDED: [&str; 9] = [
    "claude",
    "codex",
    "gemini",
    "qwen",
    "opencode",
    "cursor-agent",
    "copilot",
    "amp",
    "q",
];

/// Every harness this build knows of, installed or not.
///
/// `command` is the bare interactive invocation. Anything needing an argument
/// we cannot guess (a model name, a prompt) is left out rather than shipped
/// broken — the user can add it under `[agents.*]` in `config.toml`.
///
/// Ordered roughly by how likely it is to be the one you want, since this is
/// also the menu order.
pub fn catalog() -> Vec<Harness> {
    [
        // The ones people live in.
        ("claude", "Claude Code", "claude"),
        ("codex", "Codex", "codex"),
        ("omp", "Oh My Pi", "omp"),
        ("pi", "Pi", "pi"),
        ("prime-agent", "Prime Agent", "prime-agent"),
        ("gemini", "Gemini CLI", "gemini"),
        ("qwen", "Qwen Code", "qwen"),
        ("opencode", "OpenCode", "opencode"),
        ("cursor-agent", "Cursor Agent", "cursor-agent"),
        ("copilot", "GitHub Copilot CLI", "copilot"),
        ("aider", "Aider", "aider"),
        ("amp", "Amp", "amp"),
        ("crush", "Crush", "crush"),
        ("goose", "Goose", "goose"),
        ("droid", "Factory Droid", "droid"),
        ("auggie", "Auggie", "auggie"),
        ("jules", "Jules", "jules"),
        ("forge", "Forge", "forge"),
        ("plandex", "Plandex", "plandex"),
        ("openhands", "OpenHands", "openhands"),
        ("codebuff", "Codebuff", "codebuff"),
        ("octofriend", "Octofriend", "octofriend"),
        ("cn", "Continue", "cn"),
        ("gptme", "gptme", "gptme"),
        ("interpreter", "Open Interpreter", "interpreter"),
        ("aichat", "AIChat", "aichat"),
        ("sgpt", "ShellGPT", "sgpt"),
        ("tgpt", "tgpt", "tgpt"),
        // These need a subcommand to be a session rather than a help page.
        ("q", "Amazon Q", "q chat"),
        ("llm", "LLM", "llm chat"),
    ]
    .into_iter()
    .map(|(id, label, command)| {
        let mut h = Harness::new(id, label, command);
        if BRANDED.contains(&id) {
            h.icon = Some(id.to_string());
        }
        h
    })
    .collect()
}

/// One harness as the settings dialog sees it: the merged entry plus the
/// facts that decide whether the menu offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub harness: Harness,
    /// Its program is on `PATH` (always true for the terminal).
    pub installed: bool,
    /// The user has taken it out of the menus.
    pub hidden: bool,
    /// Shipped in the catalogue, as opposed to added in the config.
    pub builtin: bool,
}

impl Entry {
    /// Would the menu list it? Installed or explicitly given a command, and
    /// not hidden. A configured command is trusted: a wrapper script need not
    /// be on `PATH` to run.
    pub fn offered(&self, cfg: &Config) -> bool {
        let configured = cfg
            .agents
            .get(&self.harness.id)
            .is_some_and(|k| !k.command.is_empty());
        !self.hidden
            && (self.installed
                || configured
                || self.harness.is_terminal()
                || self.harness.is_browser())
    }
}

/// Everything: the terminal, the catalogue and the config-only kinds, each
/// merged with its config table, in menu order. This is the list the
/// settings dialog edits; [`available`] is its offered subset.
pub fn entries(cfg: &Config) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(catalog().len() + cfg.agents.len() + 1);
    let entry = |h: Harness, builtin: bool| {
        let custom = cfg.agents.get(&h.id);
        let hidden = custom.is_some_and(|k| k.hidden);
        let h = h.customised(custom);
        let installed = h.is_terminal() || h.is_browser() || is_installed(&h.command);
        Entry {
            harness: h,
            installed,
            hidden,
            builtin,
        }
    };
    out.push(entry(terminal(), true));
    out.push(entry(browser(), true));
    for h in catalog() {
        out.push(entry(h, true));
    }
    for (id, kind) in &cfg.agents {
        if id != TERMINAL && id != BROWSER && !catalog().iter().any(|c| &c.id == id) {
            out.push(entry(Harness::new(id, &kind.label, &kind.command), false));
        }
    }
    // The terminal is pinned first; the rest follow `harness_order`, with
    // unlisted ids keeping their relative order after the listed ones.
    let rank = |id: &str| {
        if id == TERMINAL {
            return 0;
        }
        cfg.harness_order
            .iter()
            .position(|o| o == id)
            .map_or(usize::MAX, |i| i + 1)
    };
    out.sort_by_key(|e| rank(&e.harness.id));
    out
}

/// What this machine can actually start, in menu order.
///
/// [`terminal`] first, then every entry the user has not hidden whose binary
/// is on `PATH` or whose command they configured themselves.
pub fn available(cfg: &Config) -> Vec<Harness> {
    entries(cfg)
        .into_iter()
        .filter(|e| e.offered(cfg))
        .map(|e| e.harness)
        .collect()
}

/// Drop `{task}` and `{name}` from a configured command.
///
/// Those were substituted back when a window carried a goal. A config written
/// then would now hand the agent a literal `{task}` argument, which is worse
/// than ignoring it.
fn clean(command: &str) -> String {
    command
        .split_whitespace()
        .filter(|word| !matches!(*word, "{task}" | "{name}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Look a harness up by id.
///
/// Falls back to the terminal entry, so a row stored when a harness was
/// installed still renders after it is uninstalled instead of vanishing.
pub fn by_id(cfg: &Config, id: &str) -> Harness {
    let custom = cfg.agents.get(id);
    if id == TERMINAL {
        return terminal().customised(custom);
    }
    if id == BROWSER {
        return browser().customised(custom);
    }
    if let Some(found) = catalog().into_iter().find(|h| h.id == id) {
        return found.customised(custom);
    }
    match custom {
        Some(kind) => Harness::new(id, &kind.label, &kind.command).customised(custom),
        None => terminal(),
    }
}

/// A generated name, unique within the project: "Claude Code", then
/// "Claude Code 2".
///
/// The user does not name windows — there is nothing to name them after, since
/// a window has no goal. `existing` should be the project's agents.
pub fn next_name(existing: &[Agent], harness: &Harness) -> String {
    let base = &harness.label;
    if !existing.iter().any(|a| a.name == *base) {
        return base.clone();
    }
    // Start at 2: the unsuffixed name is the first one.
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !existing.iter().any(|a| a.name == *candidate))
        .unwrap_or_else(|| base.clone())
}

/// Is the command's program on `PATH` as an executable file?
///
/// Deliberately not `command -v`: that resolves shell builtins and keywords,
/// which reported `continue` as an installed agent on the author's machine.
fn is_installed(command: &str) -> bool {
    let Some(program) = command.split_whitespace().next() else {
        return false;
    };
    which(program).is_some()
}

/// The `PATH` a terminal on this machine has, which is rarely the one this
/// process was started with: a desktop session knows nothing of
/// `~/.npm-global/bin`, `~/.bun/bin` or `~/.cargo/bin`, so an agent
/// installed by npm reads as "not installed" in a GUI launched from a
/// launcher while its name works in every shell.
///
/// A tmux pane runs the login *interactive* shell, which is exactly where
/// rc files add those directories - so ask that shell once and cache the
/// answer. Its stdin is closed, so an rc file that reads input gets EOF
/// instead of hanging us, and our own `PATH` is kept as a tail in case the
/// session has something the shell does not.
pub fn shell_path() -> &'static str {
    &SHELL_PATH
}

/// Run the rest of this process - and everything it starts - with the
/// shell's `PATH`. Call once from `main`, before any thread exists.
///
/// This is what makes a tmux server *TaiX* starts hand its panes a usable
/// `PATH`, what lets a shell job call an agent, and what gets baked into
/// the systemd unit. Started from a terminal, the inherited `PATH` is
/// already the shell's, so nothing is asked and nothing is paid: only a
/// launcher-started window spends the one shell startup.
pub fn adopt_shell_path() {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return;
    }
    let path = shell_path();
    if std::env::var("PATH").is_ok_and(|own| own == path) {
        return;
    }
    // SAFETY: documented as main-only, before threads. Nothing in this
    // process reads `PATH` concurrently yet.
    unsafe { std::env::set_var("PATH", path) };
}

static SHELL_PATH: LazyLock<String> = LazyLock::new(|| {
    let own = std::env::var("PATH").unwrap_or_default();
    let Ok(shell) = std::env::var("SHELL") else {
        return own;
    };
    let asked = Command::new(shell)
        .args(["-lic", r#"printf %s "$PATH""#])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        // An rc file that prints on startup prints before we do.
        .and_then(|text| text.lines().next_back().map(str::to_string))
        .unwrap_or_default();
    if asked.is_empty() {
        return own;
    }
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&asked).collect();
    for dir in std::env::split_paths(&own) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs)
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or(own)
});

/// Resolve `program` against `PATH`, requiring an executable regular file.
///
/// This process's `PATH`, which [`adopt_shell_path`] has already made the
/// shell's.
pub fn which(program: &str) -> Option<PathBuf> {
    // An explicit path is not a PATH lookup.
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The harness running under each pane process, in the given order.
///
/// Walks the table the caller already read. `None` where the pane is
/// running only a shell, or nothing recognisable.
pub fn running_harnesses(
    cfg: &Config,
    pane_pids: &[u32],
    table: &crate::mem::ProcTable,
) -> Vec<Option<Harness>> {
    let harnesses = available(cfg);
    pane_pids
        .iter()
        .map(|&pane_pid| {
            let pids = table.descendants(&[pane_pid]);
            let cmdlines: Vec<String> = pids
                .iter()
                .filter_map(|&pid| crate::mem::cmdline(pid))
                .collect();
            match_harness(&harnesses, &cmdlines)
        })
        .collect()
}

/// Match harnesses against command lines, returning the best match.
///
/// Prefers the deepest descendant (later in cmdlines) and the longest matching
/// command basename. Never returns the terminal entry.
fn match_harness(harnesses: &[Harness], cmdlines: &[String]) -> Option<Harness> {
    // Filter out terminal entry
    let harnesses: Vec<&Harness> = harnesses.iter().filter(|h| !h.is_terminal()).collect();

    let mut best: Option<(&Harness, usize, usize)> = None; // (harness, cmdline_idx, basename_len)

    for (cmd_idx, cmdline) in cmdlines.iter().enumerate() {
        for harness in &harnesses {
            if harness.command.is_empty() {
                continue;
            }
            // Extract first word of harness command
            let first_word = harness.command.split_whitespace().next().unwrap_or("");
            if first_word.is_empty() {
                continue;
            }
            // Skip shell variable references like $SHELL
            if first_word.starts_with('$') {
                continue;
            }
            // Extract basename
            let basename = Path::new(first_word)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if basename.is_empty() {
                continue;
            }

            // Check if basename matches in cmdline as a whole word or path segment
            if matches_in_cmdline(cmdline, basename) {
                let basename_len = basename.len();
                // Prefer deeper (later) cmdlines, then longer basenames
                if let Some((_, prev_idx, prev_len)) = best {
                    if cmd_idx > prev_idx || (cmd_idx == prev_idx && basename_len > prev_len) {
                        best = Some((harness, cmd_idx, basename_len));
                    }
                } else {
                    best = Some((harness, cmd_idx, basename_len));
                }
            }
        }
    }

    best.map(|(h, _, _)| h.clone())
}

/// Does basename appear in cmdline as a whole word or path segment?
fn matches_in_cmdline(cmdline: &str, basename: &str) -> bool {
    cmdline.split_whitespace().any(|word| {
        // Check as exact word match
        if word == basename {
            return true;
        }
        // Check as any path component
        Path::new(word)
            .components()
            .any(|comp| comp.as_os_str().to_str() == Some(basename))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentState;

    fn agent(name: &str) -> Agent {
        Agent {
            id: 1,
            project: 1,
            name: name.to_string(),
            kind: "claude".into(),
            worktree: None,
            branch: None,
            window: None,
            pane: None,
            url: None,
            headless: false,
            color: None,
            named: false,
            state: AgentState::Idle,
        }
    }

    #[test]
    fn generated_names_do_not_collide() {
        let claude = Harness::new("claude", "Claude Code", "claude");
        assert_eq!(next_name(&[], &claude), "Claude Code");

        let one = vec![agent("Claude Code")];
        assert_eq!(next_name(&one, &claude), "Claude Code 2");

        let two = vec![agent("Claude Code"), agent("Claude Code 2")];
        assert_eq!(next_name(&two, &claude), "Claude Code 3");

        // A gap is reused rather than skipped past.
        let gap = vec![agent("Claude Code"), agent("Claude Code 3")];
        assert_eq!(next_name(&gap, &claude), "Claude Code 2");
    }

    #[test]
    fn terminal_is_always_offered_and_launches_nothing() {
        let cfg = Config::default();
        let list = available(&cfg);
        assert_eq!(list[0], terminal(), "terminal must be the default choice");
        assert!(list[0].is_terminal());
        assert!(
            list[0].command.is_empty(),
            "a terminal window must not launch a harness"
        );
    }

    #[test]
    fn shell_keywords_are_not_mistaken_for_installed_agents() {
        // `command -v continue` succeeds on any POSIX shell because it is a
        // keyword. That is what made a bogus entry appear in the menu.
        assert!(!is_installed("continue"));
        assert!(!is_installed("if"));
        // A real binary every system has, to prove the check is not just false.
        assert!(is_installed("sh"), "PATH lookup should find /bin/sh");
    }

    #[test]
    fn a_directory_on_path_is_not_an_executable() {
        // `which` must require a file: PATH entries can shadow a name with a
        // directory, and spawning that fails at exec time instead of here.
        assert_eq!(which("/tmp"), None);
        assert!(which("sh").is_some());
    }

    #[test]
    fn config_overrides_a_catalogued_command_but_keeps_the_id() {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "claude".into(),
            crate::AgentKind {
                label: "Claude (danger)".into(),
                command: "claude --dangerously-skip-permissions".into(),
                ..Default::default()
            },
        );
        let found = available(&cfg)
            .into_iter()
            .find(|h| h.id == "claude")
            .expect("configured harness is offered even if PATH lookup fails");
        assert_eq!(found.command, "claude --dangerously-skip-permissions");
        assert_eq!(found.label, "Claude (danger)");
        // Exactly once: overriding must not also append a duplicate.
        assert_eq!(
            available(&cfg).iter().filter(|h| h.id == "claude").count(),
            1
        );
    }

    #[test]
    fn stale_task_placeholders_are_dropped_from_configured_commands() {
        // A config written when windows carried a goal would otherwise hand
        // the agent a literal "{task}" argument.
        let mut cfg = Config::default();
        cfg.agents.insert(
            "claude".into(),
            crate::AgentKind {
                label: "Claude Code".into(),
                command: "claude {task}".into(),
                ..Default::default()
            },
        );
        assert_eq!(by_id(&cfg, "claude").command, "claude");
        let offered = available(&cfg)
            .into_iter()
            .find(|x| x.id == "claude")
            .unwrap();
        assert_eq!(offered.command, "claude");
    }

    #[test]
    fn a_config_only_kind_is_offered() {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "mine".into(),
            crate::AgentKind {
                label: "My Wrapper".into(),
                command: "/opt/mine/run.sh".into(),
                ..Default::default()
            },
        );
        assert!(available(&cfg).iter().any(|h| h.id == "mine"));
    }

    #[test]
    fn an_uninstalled_harness_still_resolves_for_display() {
        let cfg = Config::default();
        // Stored rows outlive installs; a missing harness must not panic or
        // erase the row, it just reads as a terminal.
        let gone = by_id(&cfg, "definitely-not-installed");
        assert!(gone.is_terminal());
        // A known id resolves to its real label whether installed or not.
        assert_eq!(by_id(&cfg, "codex").label, "Codex");
    }

    #[test]
    fn catalogue_ids_and_labels_are_unique() {
        let all = catalog();
        let mut ids: Vec<&str> = all.iter().map(|h| h.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate harness id in the catalogue");
        assert!(
            !all.iter().any(|h| h.id == TERMINAL),
            "the terminal entry is added separately, not catalogued"
        );
        assert!(
            all.iter().all(|h| !h.command.is_empty()),
            "only the terminal entry may have an empty command"
        );
    }

    #[test]
    fn a_partial_override_keeps_the_catalogue_label_and_command() {
        // Recolouring Claude must not blank its command or rename it.
        let mut cfg = Config::default();
        cfg.agents.insert(
            "claude".into(),
            crate::AgentKind {
                color: Some("purple".into()),
                icon: Some("/tmp/x.png".into()),
                ..Default::default()
            },
        );
        let h = by_id(&cfg, "claude");
        assert_eq!(h.label, "Claude Code");
        assert_eq!(h.command, "claude");
        assert_eq!(h.color.as_deref(), Some("purple"));
        assert_eq!(h.icon.as_deref(), Some("/tmp/x.png"));
        // The terminal takes a colour too.
        cfg.agents.insert(
            TERMINAL.into(),
            crate::AgentKind {
                color: Some("teal".into()),
                ..Default::default()
            },
        );
        assert_eq!(by_id(&cfg, TERMINAL).color.as_deref(), Some("teal"));
        assert!(by_id(&cfg, TERMINAL).command.is_empty());
    }

    #[test]
    fn hidden_and_ordered_entries() {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "mine".into(),
            crate::AgentKind {
                label: "Mine".into(),
                command: "/opt/mine".into(),
                ..Default::default()
            },
        );
        cfg.agents.insert(
            "other".into(),
            crate::AgentKind {
                label: "Other".into(),
                command: "/opt/other".into(),
                hidden: true,
                ..Default::default()
            },
        );
        cfg.harness_order = vec!["mine".into(), "claude".into()];
        let all = entries(&cfg);
        let ids: Vec<&str> = all.iter().map(|e| e.harness.id.as_str()).collect();
        assert_eq!(&ids[..3], &[TERMINAL, "mine", "claude"], "{ids:?}");
        // Hidden entries are still listed for the settings dialog...
        let other = all.iter().find(|e| e.harness.id == "other").unwrap();
        assert!(other.hidden && !other.builtin);
        // ...but never offered.
        let offered = available(&cfg);
        assert!(offered.iter().any(|h| h.id == "mine"));
        assert!(!offered.iter().any(|h| h.id == "other"));
        assert!(offered[0].is_terminal());
    }

    #[test]
    fn match_harness_node_bundle() {
        // Node-launched prime-agent bundle
        let harnesses = vec![
            Harness::new("prime-agent", "Prime Agent", "prime-agent"),
            Harness::new("claude", "Claude Code", "claude"),
        ];
        let cmdlines =
            vec!["node /home/u/.npm-global/lib/node_modules/prime-agent/dist/cli.js".to_string()];
        let matched = super::match_harness(&harnesses, &cmdlines);
        assert_eq!(matched.as_ref().map(|h| h.id.as_str()), Some("prime-agent"));
    }

    #[test]
    fn match_harness_bare_shell_matches_nothing() {
        let harnesses = vec![
            Harness::new("claude", "Claude Code", "claude"),
            Harness::new("zsh", "Zsh", "zsh"),
        ];
        let cmdlines = vec!["/bin/zsh".to_string()];
        let matched = super::match_harness(&harnesses, &cmdlines);
        assert_eq!(matched.as_ref().map(|h| h.id.as_str()), Some("zsh"));
    }

    #[test]
    fn match_harness_terminal_never_returned() {
        let harnesses = vec![terminal(), Harness::new("claude", "Claude Code", "claude")];
        let cmdlines = vec!["claude".to_string()];
        let matched = super::match_harness(&harnesses, &cmdlines);
        assert_eq!(matched.as_ref().map(|h| h.id.as_str()), Some("claude"));

        // Even when only terminal matches
        let cmdlines2 = vec!["/bin/sh".to_string()];
        let matched2 = super::match_harness(&harnesses, &cmdlines2);
        assert_eq!(matched2, None);
    }

    #[test]
    fn match_harness_directory_does_not_match() {
        // /home/u/opencodex/ should not match opencode
        let harnesses = vec![Harness::new("opencode", "OpenCode", "opencode")];
        let cmdlines = vec!["node /home/u/opencodex/x.js".to_string()];
        let matched = super::match_harness(&harnesses, &cmdlines);
        assert_eq!(matched, None);
    }

    #[test]
    fn match_harness_longest_basename_wins() {
        // prime-agent beats prime
        let harnesses = vec![
            Harness::new("prime", "Prime", "prime"),
            Harness::new("prime-agent", "Prime Agent", "prime-agent"),
        ];
        let cmdlines = vec!["prime-agent".to_string()];
        let matched = super::match_harness(&harnesses, &cmdlines);
        assert_eq!(matched.as_ref().map(|h| h.id.as_str()), Some("prime-agent"));
    }

    #[test]
    fn match_harness_deeper_descendant_wins() {
        // Deeper claude wins over parent shell
        let harnesses = vec![
            Harness::new("zsh", "Zsh", "zsh"),
            Harness::new("claude", "Claude Code", "claude"),
        ];
        // Shell first, then claude as child
        let cmdlines = vec!["/bin/zsh".to_string(), "claude".to_string()];
        let matched = super::match_harness(&harnesses, &cmdlines);
        assert_eq!(matched.as_ref().map(|h| h.id.as_str()), Some("claude"));
    }
}
