//! `.taix.toml` at a project root: per-project defaults.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Per-project configuration from `.taix.toml` at the project root.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    /// Default harness for new windows in this project.
    pub default_harness: Option<String>,
    /// Override the global `isolate` setting for this project.
    pub isolate: Option<bool>,
    /// Environment variables exported into every window opened in this project.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Windows to open when the project is first shown.
    #[serde(default)]
    pub startup: Vec<StartupWindow>,
}

/// A window to open on project startup.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StartupWindow {
    /// Window name. `None` means auto-generated.
    pub name: Option<String>,
    /// Harness to run. `None` means a plain terminal.
    pub harness: Option<String>,
}

impl ProjectConfig {
    /// Load `.taix.toml` from a project root.
    ///
    /// Missing or malformed files return `Default` and never error — a typo in
    /// a repo file must not stop the project opening.
    pub fn load(root: &Path) -> ProjectConfig {
        let path = root.join(".taix.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return ProjectConfig::default();
        };
        toml::from_str(&content).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_file_returns_default() {
        let tmp = std::env::temp_dir().join("taix-test-missing");
        let cfg = ProjectConfig::load(&tmp);
        assert!(cfg.default_harness.is_none());
        assert!(cfg.isolate.is_none());
        assert!(cfg.env.is_empty());
        assert!(cfg.startup.is_empty());
    }

    #[test]
    fn malformed_file_returns_default() {
        let tmp = std::env::temp_dir().join("taix-test-malformed");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join(".taix.toml"), "this is not valid toml {{").unwrap();

        let cfg = ProjectConfig::load(&tmp);
        assert!(cfg.default_harness.is_none());

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn full_config_loads() {
        let tmp = std::env::temp_dir().join("taix-test-full");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(
            tmp.join(".taix.toml"),
            r#"
default_harness = "claude"
isolate = true

[env]
CUSTOM_VAR = "value"
PATH = "/custom/bin:$PATH"

[[startup]]
name = "main"
harness = "claude"

[[startup]]
harness = "codex"

[[startup]]
name = "shell"
"#,
        )
        .unwrap();

        let cfg = ProjectConfig::load(&tmp);
        assert_eq!(cfg.default_harness, Some("claude".into()));
        assert_eq!(cfg.isolate, Some(true));
        assert_eq!(cfg.env.get("CUSTOM_VAR"), Some(&"value".to_string()));
        assert_eq!(cfg.startup.len(), 3);
        assert_eq!(cfg.startup[0].name, Some("main".into()));
        assert_eq!(cfg.startup[0].harness, Some("claude".into()));
        assert_eq!(cfg.startup[1].name, None);
        assert_eq!(cfg.startup[1].harness, Some("codex".into()));
        assert_eq!(cfg.startup[2].name, Some("shell".into()));
        assert_eq!(cfg.startup[2].harness, None);

        fs::remove_dir_all(&tmp).unwrap();
    }
}
