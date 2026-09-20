//! Named session snapshots: which projects, which windows, which harnesses.

use crate::{Config, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub name: String,
    pub projects: Vec<SnapProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapProject {
    pub name: String,
    pub root: PathBuf,
    pub windows: Vec<SnapWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapWindow {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub isolate: bool,
}

/// Session snapshot directory: `$XDG_DATA_HOME/taix/sessions`.
pub fn dir(_cfg: &Config) -> PathBuf {
    let base = crate::config::base_dir(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        ".local/share",
    );
    base.join("taix").join("sessions")
}

/// List all saved session names, sorted. Never errors.
pub fn list(cfg: &Config) -> Vec<String> {
    let d = dir(cfg);
    let Ok(entries) = fs::read_dir(&d) else {
        return vec![];
    };
    let mut names = vec![];
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && let Some(stem) = name.strip_suffix(".toml")
        {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names
}

/// Save a snapshot. Name is sanitised into a filesystem-safe slug.
pub fn save(cfg: &Config, snap: &Snapshot) -> Result<PathBuf> {
    let slug = sanitize_name(&snap.name);
    if slug.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "snapshot name is empty",
        )
        .into());
    }
    let d = dir(cfg);
    fs::create_dir_all(&d)?;
    let path = d.join(format!("{slug}.toml"));

    let content = toml::to_string_pretty(snap)?;
    let tmp = d.join(format!("{slug}.tmp"));
    fs::write(&tmp, content)?;
    #[cfg(unix)]
    {
        fs::File::open(&tmp)?.sync_all()?;
    }
    fs::rename(&tmp, &path)?;

    Ok(path)
}

/// Load a snapshot by name.
pub fn load(cfg: &Config, name: &str) -> Result<Snapshot> {
    let slug = sanitize_name(name);
    if slug.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "snapshot name is empty",
        )
        .into());
    }
    let path = dir(cfg).join(format!("{slug}.toml"));
    let content = fs::read_to_string(&path)?;
    let snap: Snapshot = toml::from_str(&content)?;
    Ok(snap)
}

/// Remove a snapshot by name.
pub fn remove(cfg: &Config, name: &str) -> Result<()> {
    let slug = sanitize_name(name);
    if slug.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "snapshot name is empty",
        )
        .into());
    }
    let d = dir(cfg);
    let path = d.join(format!("{slug}.toml"));

    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Sanitise a snapshot name into a filesystem-safe slug.
/// Removes path separators and parent directory references.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|&c| c != '/' && c != '\\' && c != '\0')
        .collect::<String>()
        .split("..")
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trip() {
        let cfg = Config::default();
        let snap = Snapshot {
            name: "test-session".into(),
            projects: vec![SnapProject {
                name: "proj1".into(),
                root: PathBuf::from("/tmp/proj1"),
                windows: vec![
                    SnapWindow {
                        name: "main".into(),
                        kind: "claude".into(),
                        color: Some("blue".into()),
                        isolate: true,
                    },
                    SnapWindow {
                        name: "aux".into(),
                        kind: "terminal".into(),
                        color: None,
                        isolate: false,
                    },
                ],
            }],
        };

        save(&cfg, &snap).unwrap();
        let loaded = load(&cfg, "test-session").unwrap();

        assert_eq!(loaded.name, snap.name);
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].windows.len(), 2);
        assert_eq!(loaded.projects[0].windows[0].color, Some("blue".into()));
        assert!(loaded.projects[0].windows[0].isolate);
        assert!(!loaded.projects[0].windows[1].isolate);

        remove(&cfg, "test-session").unwrap();
    }

    #[test]
    fn unknown_field_does_not_break_load() {
        let cfg = Config::default();
        let d = dir(&cfg);
        fs::create_dir_all(&d).unwrap();
        let path = d.join("unknown-field.toml");
        fs::write(
            &path,
            r#"
name = "test"
unknown_top_level = "ignored"
[[projects]]
name = "p"
root = "/tmp"
unknown_project_field = 42
[[projects.windows]]
name = "w"
kind = "terminal"
unknown_window_field = true
"#,
        )
        .unwrap();

        let loaded = load(&cfg, "unknown-field").unwrap();
        assert_eq!(loaded.name, "test");
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].windows.len(), 1);

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn dangerous_names_cannot_escape_directory() {
        let cfg = Config::default();
        let snap = Snapshot {
            name: "../escape".into(),
            projects: vec![],
        };

        let path = save(&cfg, &snap).unwrap();
        assert!(path.starts_with(dir(&cfg)));
        assert!(!path.to_string_lossy().contains(".."));

        let snap2 = Snapshot {
            name: "/absolute/path".into(),
            projects: vec![],
        };
        let path2 = save(&cfg, &snap2).unwrap();
        assert!(path2.starts_with(dir(&cfg)));

        remove(&cfg, "../escape").unwrap();
        remove(&cfg, "/absolute/path").unwrap();
    }

    #[test]
    fn sanitize_removes_path_components() {
        assert_eq!(sanitize_name("normal"), "normal");
        assert_eq!(sanitize_name("../parent"), "parent");
        assert_eq!(sanitize_name("/absolute"), "absolute");
        assert_eq!(sanitize_name("a/b/c"), "abc");
        assert_eq!(sanitize_name(".."), "");
        assert_eq!(sanitize_name("foo/../bar"), "foo_bar");
    }
}
