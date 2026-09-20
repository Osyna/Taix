//! Opening a window, front-end free.
//!
//! Creating an agent is four coupled decisions - name, isolation, worktree,
//! tmux window - and getting the order wrong leaves a phantom row that can
//! never produce output. Both fronts had to make them identically, so they
//! are made once here and the fronts only decide what to draw afterwards.

use std::path::{Path, PathBuf};

use crate::{
    Agent, Config, Harness, NewAgent, Project, ProjectConfig, Store, by_id, history, next_name,
    session, terminal,
};
use taix_tmux::cmd;

/// What a successful spawn produced. The caller re-reads the store anyway;
/// this is what it needs before that happens.
pub struct Spawned {
    pub agent: Agent,
    pub window: u32,
    pub pane: u32,
}

/// One entry of a project's `[[startup]]` table, resolved to a harness id.
pub struct StartupSpec {
    pub harness: String,
    pub name: Option<String>,
}

/// Create an agent: optional git worktree, store row, tmux window, in that
/// order. Each step's failure leaves nothing dangling.
///
/// `isolate` overrides both `.taix.toml` and the global setting, which is how
/// a restored session reproduces the window it recorded rather than whatever
/// the config says today. `existing` is the project's current agents, used
/// only to pick the next free name.
pub fn window(
    store: &Store,
    cfg: &Config,
    project: &Project,
    harness: &Harness,
    existing: &[Agent],
    isolate: Option<bool>,
) -> Result<Spawned, String> {
    let name = next_name(existing, harness);
    let slug = taix_git::slug(&name);

    // A repository can carry its own defaults, so cloning it brings its agent
    // setup with it. A malformed file loads as `Default`, never an error: a
    // typo in a repo must not stop a window opening.
    let project_cfg = ProjectConfig::load(&project.root);
    let isolate = isolate.or(project_cfg.isolate).unwrap_or(cfg.isolate);
    let (cwd, branch) = if isolate {
        let repo = taix_git::Repo::open(&project.root).map_err(|e| e.to_string())?;
        let base = repo.default_branch().map_err(|e| e.to_string())?;
        let wt = repo.add_worktree(&slug, &base).map_err(|e| e.to_string())?;
        (wt.path, Some(wt.branch))
    } else {
        (project.root.clone(), None)
    };

    let new = NewAgent {
        project: project.id,
        name: name.clone(),
        kind: harness.id.clone(),
        worktree: if isolate { Some(cwd.clone()) } else { None },
        branch,
    };
    let agent = store.add_agent(&new).map_err(|e| e.to_string())?;

    let window_name = format!("{}-{}", project.name, slug);
    let env: Vec<(String, String)> = project_cfg.env.into_iter().collect();
    match cmd::new_window(
        &cfg.tmux_socket,
        &cfg.tmux_session,
        &window_name,
        &cwd,
        &resolve(&harness.command),
        &env,
    ) {
        Ok((window, pane)) => {
            let _ = store.set_tmux(agent.id, Some(window), Some(pane));
            Ok(Spawned {
                agent,
                window,
                pane,
            })
        }
        Err(e) => {
            // Do not leave a phantom agent that can never produce output.
            let _ = store.remove_agent(agent.id);
            Err(format!("tmux: {e}"))
        }
    }
}

/// Create a browser window: an agent with no tmux pane, drivable through MCP.
///
/// `existing` is the project's current agents, used to pick the next free name.
pub fn browser_window(
    store: &Store,
    _cfg: &Config,
    project: &Project,
    existing: &[Agent],
    headless: bool,
) -> Result<Agent, String> {
    let harness = crate::harness::browser();
    let name = next_name(existing, &harness);

    let new = NewAgent {
        project: project.id,
        name,
        kind: harness.id.clone(),
        worktree: None,
        branch: None,
    };
    let agent = store.add_agent(&new).map_err(|e| e.to_string())?;
    // Nothing is starting: there is no process to wait for, and a window
    // that says STARTING forever is a lie the whole UI repeats.
    store
        .set_state(agent.id, crate::AgentState::Idle)
        .map_err(|e| e.to_string())?;
    if headless {
        store
            .set_headless(agent.id, true)
            .map_err(|e| e.to_string())?;
    }
    let mut agent = agent;
    agent.state = crate::AgentState::Idle;
    agent.headless = headless;
    Ok(agent)
}

/// Open a window again for an agent whose pane is gone: same directory,
/// same harness command as the config says today, the old scrollback
/// replayed in front. The record stays what it was; only its pane is new.
pub fn relaunch(
    store: &Store,
    cfg: &Config,
    project: &Project,
    agent: &Agent,
) -> Result<(u32, u32), String> {
    let cwd = agent.worktree.as_deref().unwrap_or(&project.root);
    let command = history::replaying(agent.id, &resolve(&by_id(cfg, &agent.kind).command));
    let window_name = format!("{}-{}", project.name, taix_git::slug(&agent.name));
    let env: Vec<(String, String)> = ProjectConfig::load(&project.root).env.into_iter().collect();
    let (window, pane) = cmd::new_window(
        &cfg.tmux_socket,
        &cfg.tmux_session,
        &window_name,
        cwd,
        &command,
        &env,
    )
    .map_err(|e| format!("tmux: {e}"))?;
    store
        .set_tmux(agent.id, Some(window), Some(pane))
        .map_err(|e| e.to_string())?;
    let _ = store.set_state(agent.id, crate::AgentState::Starting);
    Ok((window, pane))
}

/// A harness command with its program resolved to an absolute path.
///
/// tmux **overwrites** `PATH` for every pane with the one its *server* was
/// started with - `new-window -e PATH=...` and `set-environment -g PATH`
/// are both ignored (verified against tmux 3.7c). So a server started by a
/// desktop session outlives the fix and would keep reporting exit 127 for
/// an agent installed by npm. Handing tmux a full path sidesteps the
/// server's `PATH` entirely; only the first word is touched, so
/// `q chat` and `emacsclient -t` survive.
fn resolve(command: &str) -> String {
    let Some(program) = command.split_whitespace().next() else {
        return command.to_string();
    };
    match crate::harness::which(program) {
        Some(full) if !program.contains('/') => {
            format!("{}{}", full.display(), &command[program.len()..])
        }
        _ => command.to_string(),
    }
}

/// The windows a project's `.taix.toml` asks for, in file order.
///
/// An entry with no harness means the project's default, and failing that a
/// plain terminal - so a `[[startup]]` block with nothing but a name still
/// opens something.
pub fn startup_windows(root: &Path) -> Vec<StartupSpec> {
    let pc = ProjectConfig::load(root);
    pc.startup
        .iter()
        .map(|w| StartupSpec {
            harness: w
                .harness
                .clone()
                .or_else(|| pc.default_harness.clone())
                .unwrap_or_else(|| terminal().id),
            name: w.name.clone(),
        })
        .collect()
}

/// Snapshot every registered project and its open windows.
///
/// Built from the store rather than from a front's own list, so both fronts
/// save the same thing and a session saved from one restores in the other.
pub fn snapshot(store: &Store, name: &str) -> Result<session::Snapshot, String> {
    let projects = store.projects().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(projects.len());
    for project in &projects {
        let agents = store.agents(project.id).map_err(|e| e.to_string())?;
        out.push(session::SnapProject {
            name: project.name.clone(),
            root: project.root.clone(),
            windows: agents
                .iter()
                .map(|a| session::SnapWindow {
                    name: a.name.clone(),
                    kind: a.kind.clone(),
                    color: a.color.clone(),
                    isolate: a.worktree.is_some(),
                })
                .collect(),
        });
    }
    Ok(session::Snapshot {
        name: name.to_string(),
        projects: out,
    })
}

/// Reopen a saved session; returns how many windows were opened.
///
/// Projects that are already registered are reused, and a project that
/// already has windows is left alone: restore must not duplicate what is in
/// front of you.
pub fn restore(store: &Store, cfg: &Config, snap: &session::Snapshot) -> Result<usize, String> {
    let mut opened = 0usize;
    for snap_project in &snap.projects {
        let Some(project) = ensure_project(store, &snap_project.root)? else {
            continue;
        };
        let existing = store.agents(project.id).map_err(|e| e.to_string())?;
        if !existing.is_empty() {
            continue;
        }
        let mut agents = existing;
        for saved in &snap_project.windows {
            let harness = by_id(cfg, &saved.kind);
            let spawned = window(store, cfg, &project, &harness, &agents, Some(saved.isolate))?;
            if saved.name != harness.label {
                let _ = store.rename_agent(spawned.agent.id, &saved.name);
            }
            if let Some(color) = &saved.color {
                let _ = store.set_color(spawned.agent.id, Some(color));
            }
            // Feed the next name generator, or every window would be called
            // the same thing and tmux would get five "Terminal" windows.
            agents.push(spawned.agent);
            opened += 1;
        }
    }
    Ok(opened)
}

/// The project registered at `root`, adding it if it is not known yet.
fn ensure_project(store: &Store, root: &Path) -> Result<Option<Project>, String> {
    let projects = store.projects().map_err(|e| e.to_string())?;
    if let Some(found) = projects.into_iter().find(|p| p.root == root) {
        return Ok(Some(found));
    }
    let resolved: PathBuf = taix_git::project_root(root).map_err(|e| e.to_string())?;
    let name = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| resolved.display().to_string());
    match store.add_project(&name, &resolved) {
        Ok(project) => Ok(Some(project)),
        // Registered under a different spelling of the same path between the
        // listing and the insert; the caller's next pass will see it.
        Err(_) => Ok(store
            .projects()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| p.root == resolved)),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn the_program_is_resolved_and_its_arguments_are_not() {
        // `sh` is on every PATH this can run on.
        let line = resolve("sh -c 'echo hi'");
        let (program, rest) = line.split_once(' ').expect("arguments survive");
        assert!(program.starts_with('/'), "{program} is not absolute");
        assert!(program.ends_with("/sh"), "{program} is not sh");
        assert_eq!(rest, "-c 'echo hi'");
    }

    #[test]
    fn what_cannot_be_found_is_left_exactly_as_it_was() {
        // A harness that is not installed still has to reach tmux verbatim:
        // its "command not found" is the message the user needs to see.
        assert_eq!(
            resolve("taix-no-such-agent chat"),
            "taix-no-such-agent chat"
        );
        assert_eq!(
            resolve("/opt/agent/bin/run --tui"),
            "/opt/agent/bin/run --tui"
        );
        assert_eq!(resolve(""), "");
    }
}
