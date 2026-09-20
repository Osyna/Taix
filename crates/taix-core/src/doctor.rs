use crate::{Config, Store};
use std::net::TcpListener;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

pub fn checks(cfg: &Config) -> Vec<Check> {
    vec![
        check_tmux(),
        check_tmux_server(cfg),
        check_state(cfg),
        check_config(cfg),
        check_web_key(),
        check_web_port(cfg),
        check_jobs_timer(),
        check_tailscale(),
        check_harnesses(cfg),
    ]
}

fn check_tmux() -> Check {
    let Some(path) = crate::which("tmux") else {
        return Check {
            name: "tmux".to_string(),
            ok: false,
            detail: "not installed; install tmux from your package manager".to_string(),
        };
    };
    let output = std::process::Command::new(&path).arg("-V").output();
    let version = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown version".to_string(),
    };
    Check {
        name: "tmux".to_string(),
        ok: true,
        detail: version,
    }
}

fn check_tmux_server(cfg: &Config) -> Check {
    let Some(tmux_path) = crate::which("tmux") else {
        return Check {
            name: "tmux server".to_string(),
            ok: false,
            detail: "tmux not found".to_string(),
        };
    };
    let output = std::process::Command::new(&tmux_path)
        .args(["-L", &cfg.tmux_socket, "list-windows", "-F", "#{window_id}"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let count = o.stdout.iter().filter(|&&b| b == b'\n').count();
            Check {
                name: "tmux server".to_string(),
                ok: true,
                detail: format!("running, {} windows", count),
            }
        }
        _ => Check {
            name: "tmux server".to_string(),
            ok: false,
            detail: format!("no server on socket '{}'", cfg.tmux_socket),
        },
    }
}

fn check_state(_cfg: &Config) -> Check {
    let path = Config::store_path();

    if !path.exists() {
        return Check {
            name: "state".to_string(),
            ok: false,
            detail: format!("{}: not found", path.display()),
        };
    }

    let store = match Store::open(&path) {
        Ok(s) => s,
        Err(e) => {
            return Check {
                name: "state".to_string(),
                ok: false,
                detail: format!("{}: {}", path.display(), e),
            };
        }
    };

    let projects = store.projects().unwrap_or_default().len();
    let jobs = store.jobs().unwrap_or_default().len();

    let all_agents = store
        .projects()
        .unwrap_or_default()
        .iter()
        .map(|p| store.agents(p.id).unwrap_or_default().len())
        .sum::<usize>();

    Check {
        name: "state".to_string(),
        ok: true,
        detail: format!(
            "{}: {} projects, {} agents, {} jobs",
            path.display(),
            projects,
            all_agents,
            jobs
        ),
    }
}

fn check_config(_cfg: &Config) -> Check {
    let path = Config::default_path();

    let exists = path.exists();
    let parsed = Config::load_from(&path).is_ok();

    Check {
        name: "config".to_string(),
        ok: parsed,
        detail: if parsed {
            path.display().to_string()
        } else if exists {
            format!("{}: parse error, using defaults", path.display())
        } else {
            format!("{}: not found, using defaults", path.display())
        },
    }
}

fn check_web_key() -> Check {
    let path = crate::config::base_dir(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        ".local/share",
    )
    .join("taix")
    .join("web.key");

    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => {
            return Check {
                name: "web key".to_string(),
                ok: false,
                detail: "not found; will be created on first web access".to_string(),
            };
        }
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode == 0o600 {
            Check {
                name: "web key".to_string(),
                ok: true,
                detail: format!("{}: mode 0600", path.display()),
            }
        } else {
            Check {
                name: "web key".to_string(),
                ok: false,
                detail: format!("{}: mode {:o}, expected 0600", path.display(), mode),
            }
        }
    }

    #[cfg(not(unix))]
    {
        Check {
            name: "web key".to_string(),
            ok: true,
            detail: format!("{}: exists", path.display()),
        }
    }
}

/// A bound port is the normal state while TaiX is running, so only a port
/// held by something else is a problem worth exiting non-zero for.
fn check_web_port(cfg: &Config) -> Check {
    let (ok, detail) = match TcpListener::bind(format!("127.0.0.1:{}", cfg.web.port)) {
        Ok(_) => (true, format!("port {} free", cfg.web.port)),
        Err(_) if is_taix_running() => (true, format!("port {} served by TaiX", cfg.web.port)),
        Err(_) => (
            false,
            format!("port {} held by something else", cfg.web.port),
        ),
    };
    Check {
        name: "web port".to_string(),
        ok,
        detail,
    }
}

fn is_taix_running() -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg("taix")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Not installing the timer is a supported choice, not a fault: jobs still
/// run while a TaiX window is open.
fn check_jobs_timer() -> Check {
    use crate::jobs;
    let detail = match jobs::background_status() {
        jobs::Background::Enabled => "systemd timer runs jobs every minute",
        jobs::Background::Disabled => "no timer; jobs run while a TaiX window is open",
        jobs::Background::Unavailable => "no systemd; jobs run while a TaiX window is open",
    };
    Check {
        name: "jobs timer".to_string(),
        ok: true,
        detail: detail.to_string(),
    }
}

fn check_tailscale() -> Check {
    use crate::tailscale;
    let status = tailscale::status();
    let serve = tailscale::serve_url();

    match (status, serve) {
        (None, _) => Check {
            name: "tailscale".to_string(),
            ok: false,
            detail: "not installed or not running".to_string(),
        },
        (Some(t), None) => Check {
            name: "tailscale".to_string(),
            ok: true,
            detail: format!("{}, serve: off", t.state),
        },
        (Some(t), Some(url)) => Check {
            name: "tailscale".to_string(),
            ok: true,
            detail: format!("{}, serve: {}", t.state, url),
        },
    }
}

fn check_harnesses(cfg: &Config) -> Check {
    use crate::harness;
    let available = harness::available(cfg);
    let entries = harness::entries(cfg);
    let configured = entries.len();

    Check {
        name: "harnesses".to_string(),
        ok: !available.is_empty(),
        detail: format!("{} installed of {} configured", available.len(), configured),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_file_does_not_panic() {
        let tmp = std::env::temp_dir().join(format!("taix-doctor-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        unsafe {
            std::env::set_var("XDG_DATA_HOME", &tmp);
        }

        let cfg = Config {
            agents: Default::default(),
            harness_order: vec![],
            tmux_socket: "taix-test".to_string(),
            tmux_session: "taix".to_string(),
            idle_after_ms: 2000,
            theme: None,
            font_size: 10.0,
            browser_home: "about:blank".to_string(),
            editor: None,
            isolate: false,
            bar: vec![],
            reap_idle_after_ms: None,
            web: crate::config::Web {
                enabled: true,
                port: 4040,
                tailscale: false,
            },
        };

        let result = checks(&cfg);
        assert_eq!(result.len(), 9);

        let state_check = result.iter().find(|c| c.name == "state").unwrap();
        assert!(!state_check.ok);
        assert!(state_check.detail.contains("state.toml"));

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }
}
