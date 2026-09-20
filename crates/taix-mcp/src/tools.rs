use serde_json::Value;
use taix_core::{AgentId, Config, Store};
use taix_tmux::cmd;

pub struct State {
    store: Store,
    config: Config,
}

impl State {
    pub fn new(store: Store, config: Config) -> Self {
        Self { store, config }
    }
}

pub fn list_projects(state: &State) -> Result<String, String> {
    let projects = state
        .store
        .projects()
        .map_err(|e| format!("store error: {}", e))?;
    let result: Vec<_> = projects
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "root": p.root.display().to_string()
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&result).unwrap())
}

pub fn list_agents(state: &State, args: &Value) -> Result<String, String> {
    let project = extract_optional_int(args, "project")?;

    let agents = if let Some(project_id) = project {
        state
            .store
            .agents(project_id)
            .map_err(|e| format!("store error: {}", e))?
    } else {
        state
            .store
            .all_agents()
            .map_err(|e| format!("store error: {}", e))?
    };

    let socket = &state.config.tmux_socket;
    let result: Vec<_> = agents
        .iter()
        .map(|a| {
            let alive = a.pane.map(|p| cmd::pane_alive(socket, p)).unwrap_or(false);
            let harness_label = taix_core::by_id(&state.config, &a.kind).label;
            serde_json::json!({
                "id": a.id,
                "project": a.project,
                "name": a.name,
                "kind": a.kind,
                "harness": harness_label,
                "state": a.state.as_str(),
                "branch": a.branch,
                "worktree": a.worktree.as_ref().map(|w| w.display().to_string()),
                "window": a.window,
                "pane": a.pane,
                "alive": alive
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&result).unwrap())
}

pub fn agent_output(state: &State, args: &Value) -> Result<String, String> {
    let agent_id = extract_int(args, "agent")?;
    let lines = extract_optional_int(args, "lines")?.unwrap_or(200);
    let lines = lines.clamp(1, 5000) as usize;

    let agent = state
        .store
        .agent(agent_id)
        .map_err(|e| format!("store error: {}", e))?
        .ok_or_else(|| format!("agent {} not found", agent_id))?;

    let pane = agent
        .pane
        .ok_or_else(|| format!("agent {} has no pane", agent_id))?;

    cmd::capture_text(&state.config.tmux_socket, pane, lines)
        .map_err(|e| format!("tmux error: {}", e))
}

pub fn send_to_agent(state: &State, args: &Value) -> Result<String, String> {
    let agent_id = extract_int(args, "agent")?;
    let text = extract_string(args, "text")?;
    let enter = extract_optional_bool(args, "enter")?.unwrap_or(true);

    let agent = state
        .store
        .agent(agent_id)
        .map_err(|e| format!("store error: {}", e))?
        .ok_or_else(|| format!("agent {} not found", agent_id))?;

    let pane = agent
        .pane
        .ok_or_else(|| format!("agent {} has no pane", agent_id))?;

    if !cmd::pane_alive(&state.config.tmux_socket, pane) {
        return Err(format!("agent {} pane is dead", agent_id));
    }

    cmd::send_text(&state.config.tmux_socket, pane, &text)
        .map_err(|e| format!("tmux error: {}", e))?;

    if enter {
        cmd::send_key(&state.config.tmux_socket, pane, "Enter")
            .map_err(|e| format!("tmux error: {}", e))?;
    }

    Ok(format!("Sent to agent {}", agent_id))
}

pub fn interrupt_agent(state: &State, args: &Value) -> Result<String, String> {
    let agent_id = extract_int(args, "agent")?;

    let agent = state
        .store
        .agent(agent_id)
        .map_err(|e| format!("store error: {}", e))?
        .ok_or_else(|| format!("agent {} not found", agent_id))?;

    let pane = agent
        .pane
        .ok_or_else(|| format!("agent {} has no pane", agent_id))?;

    if !cmd::pane_alive(&state.config.tmux_socket, pane) {
        return Err(format!("agent {} pane is dead", agent_id));
    }

    cmd::send_key(&state.config.tmux_socket, pane, "C-c")
        .map_err(|e| format!("tmux error: {}", e))?;

    Ok(format!("Interrupted agent {}", agent_id))
}

// Argument extraction helpers

fn extract_int(args: &Value, field: &str) -> Result<AgentId, String> {
    match args.get(field) {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                    Ok(f as i64)
                } else {
                    Err(format!("argument \"{}\" must be an integer", field))
                }
            } else {
                Err(format!("argument \"{}\" must be an integer", field))
            }
        }
        Some(_) => Err(format!("argument \"{}\" must be an integer", field)),
        None => Err(format!("missing required argument \"{}\"", field)),
    }
}

fn extract_string(args: &Value, field: &str) -> Result<String, String> {
    match args.get(field) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(format!("argument \"{}\" must be a string", field)),
        None => Err(format!("missing required argument \"{}\"", field)),
    }
}

fn extract_optional_int(args: &Value, field: &str) -> Result<Option<i64>, String> {
    match args.get(field) {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(Some(i))
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                    Ok(Some(f as i64))
                } else {
                    Err(format!("argument \"{}\" must be an integer", field))
                }
            } else {
                Err(format!("argument \"{}\" must be an integer", field))
            }
        }
        Some(_) => Err(format!("argument \"{}\" must be an integer", field)),
        None => Ok(None),
    }
}

fn extract_optional_bool(args: &Value, field: &str) -> Result<Option<bool>, String> {
    match args.get(field) {
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("argument \"{}\" must be a boolean", field)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_int_valid() {
        let args = json!({"agent": 42});
        assert_eq!(extract_int(&args, "agent").unwrap(), 42);
    }

    #[test]
    fn test_extract_int_integral_float() {
        let args = json!({"agent": 42.0});
        assert_eq!(extract_int(&args, "agent").unwrap(), 42);
    }

    #[test]
    fn test_extract_int_fractional_float() {
        let args = json!({"agent": 42.5});
        let err = extract_int(&args, "agent").unwrap_err();
        assert!(err.contains("must be an integer"));
    }

    #[test]
    fn test_extract_int_missing() {
        let args = json!({});
        let err = extract_int(&args, "agent").unwrap_err();
        assert!(err.contains("missing required argument \"agent\""));
    }

    #[test]
    fn test_extract_int_wrong_type() {
        let args = json!({"agent": "not a number"});
        let err = extract_int(&args, "agent").unwrap_err();
        assert!(err.contains("must be an integer"));
    }

    #[test]
    fn test_extract_optional_int_present() {
        let args = json!({"lines": 100});
        assert_eq!(extract_optional_int(&args, "lines").unwrap(), Some(100));
    }

    #[test]
    fn test_extract_optional_int_absent() {
        let args = json!({});
        assert_eq!(extract_optional_int(&args, "lines").unwrap(), None);
    }

    #[test]
    fn test_extract_optional_bool_present() {
        let args = json!({"enter": false});
        assert_eq!(extract_optional_bool(&args, "enter").unwrap(), Some(false));
    }

    #[test]
    fn test_extract_optional_bool_absent() {
        let args = json!({});
        assert_eq!(extract_optional_bool(&args, "enter").unwrap(), None);
    }
}
