use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Extra or overriding harnesses. Empty by default: the built-in
    /// catalogue in `harness.rs` supplies the known ones and `PATH` decides
    /// which are offered, so listing them here is only for adding your own
    /// or changing a command, label, icon or colour.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, AgentKind>,
    /// Harness ids in the order the menu should list them. Ids not named
    /// here follow, in catalogue order; the terminal is always first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harness_order: Vec<String>,
    #[serde(default = "default_tmux_socket")]
    pub tmux_socket: String,
    #[serde(default = "default_tmux_session")]
    pub tmux_session: String,
    #[serde(default = "default_idle_after_ms")]
    pub idle_after_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Pane font size in points. Zoomed at runtime; this is the start value.
    #[serde(default = "default_font_size")]
    pub font_size: f64,
    /// Landing page for the embedded browser panel. `about:blank` - the
    /// default - is the GUI's own themed start page, not a white rectangle.
    #[serde(default = "default_browser_home")]
    pub browser_home: String,
    /// What "open" in the files panel uses: a catalogue id (`code`, `zed`,
    /// `nvim`, ...) or a command (`emacsclient -t`). Unset means the first
    /// of `$VISUAL`, `$EDITOR` and the catalogue that is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    /// Give every new window its own git worktree on a branch.
    ///
    /// Off by default: a window is usually just a terminal in the project,
    /// and a project no longer has to be a git repository at all. Turning it
    /// on restores what the old new-agent dialog's switch did, without
    /// asking per window.
    #[serde(default)]
    pub isolate: bool,
    /// Bottom-bar segments, in order.
    ///
    /// Valid segment ids: `where`, `branch`, `windows`, `memory`, `uptime`, `empty`.
    /// Unknown ids are ignored by the GUI.
    #[serde(default = "default_bar")]
    pub bar: Vec<String>,
    /// Stop an agent left idle this long. `None` = never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reap_idle_after_ms: Option<u64>,
    /// The LAN web front end. A nested table, so it must stay last: a TOML
    /// table ends the top level, and a scalar written after it would be
    /// read back as one of its keys.
    #[serde(default)]
    pub web: Web,
}

/// One `[agents.<id>]` table: a harness of the user's own, or the parts of
/// a catalogued one they changed. Every field is optional so that recolouring
/// Claude does not require restating its command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentKind {
    /// Menu label; empty keeps the catalogue's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Shell command; empty keeps the catalogue's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// A bundled icon name (`claude`, `robot`, ...) or an absolute path to
    /// an image file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// One of the eight window tints; windows of this harness wear it unless
    /// recoloured individually.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Keep it out of the menus without forgetting the customisation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
}

/// `[web]`: serve the same UI to the other devices in the house.
///
/// On by default. The point of the feature is that the phone in your pocket
/// already works; a switch you have to find first is a switch nobody finds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Web {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_web_port")]
    pub port: u16,
    /// Bind the front end to this machine's tailnet address instead of
    /// every interface, so only devices on the tailnet can reach it.
    #[serde(default)]
    pub tailscale: bool,
}

impl Default for Web {
    fn default() -> Self {
        Web {
            enabled: true,
            port: default_web_port(),
            tailscale: false,
        }
    }
}

fn yes() -> bool {
    true
}

fn default_web_port() -> u16 {
    4040
}

impl AgentKind {
    /// A kind that changes nothing: the settings dialog drops these rather
    /// than writing empty tables.
    pub fn is_empty(&self) -> bool {
        *self == AgentKind::default()
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_tmux_socket() -> String {
    "taix".to_string()
}

fn default_tmux_session() -> String {
    "taix".to_string()
}

fn default_idle_after_ms() -> u64 {
    2000
}

fn default_font_size() -> f64 {
    10.0
}

fn default_browser_home() -> String {
    "about:blank".to_string()
}

fn default_bar() -> Vec<String> {
    vec![
        "where".to_string(),
        "branch".to_string(),
        // What you are typing into, spelled out: the bar is the only place
        // that says which window has the keyboard and what shape it is.
        "window".to_string(),
        "windows".to_string(),
        "memory".to_string(),
    ]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            agents: BTreeMap::new(),
            harness_order: Vec::new(),
            tmux_socket: default_tmux_socket(),
            tmux_session: default_tmux_session(),
            idle_after_ms: default_idle_after_ms(),
            theme: None,
            font_size: default_font_size(),
            browser_home: default_browser_home(),
            editor: None,
            isolate: false,
            bar: default_bar(),
            reap_idle_after_ms: None,
            web: Web::default(),
        }
    }
}

/// The config is read on every web request and once a frame by the GUI,
/// and it changes when someone saves it. Keyed on the file's mtime, so an
/// edit from anywhere is picked up on the next read and nothing else costs
/// a parse.
static CACHE: Mutex<Option<(Option<SystemTime>, Config)>> = Mutex::new(None);

impl Config {
    pub fn load() -> Config {
        let path = Self::default_path();
        let mtime = std::fs::symlink_metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((stamp, config)) = cache.as_ref()
            && mtime == *stamp
        {
            return config.clone();
        }
        let config = match Self::load_from(&path) {
            Ok(config) => config,
            // No file at all is the default state, not a problem worth a line
            // on every run. A file that exists and will not parse is: the
            // settings the user wrote are being ignored.
            Err(_) if mtime.is_none() => Config::default(),
            Err(e) => {
                eprintln!("warning: ignoring {}: {e}", path.display());
                Config::default()
            }
        };
        *cache = Some((mtime, config.clone()));

        config
    }

    pub fn load_from(path: &std::path::Path) -> Result<Config> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Write the whole config to its default path. Comments in a hand-written
    /// file are lost; the settings dialog says so.
    pub fn save(&self) -> Result<()> {
        let path = Self::default_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        // mtime granularity could hide a save within the same second of
        // the last read, so seed the cache rather than invalidating it.
        let mtime = std::fs::symlink_metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some((mtime, self.clone()));
        Ok(())
    }

    pub fn default_path() -> PathBuf {
        base_dir(
            std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
            ".config",
        )
        .join("taix")
        .join("config.toml")
    }

    pub fn store_path() -> PathBuf {
        base_dir(
            std::env::var("XDG_DATA_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
            ".local/share",
        )
        .join("taix")
        .join("state.toml")
    }
}

/// Resolve an XDG base directory, preferring `xdg`, then `$HOME/<relative>`.
///
/// The last resort MUST be absolute. It used to be the bare relative path
/// `.local/share`, which meant a process started without `HOME` silently
/// created its database inside whatever the current directory happened to be —
/// observed dropping a `taix.db` into a source checkout.
pub(crate) fn base_dir(xdg: Option<&str>, home: Option<&str>, relative: &str) -> PathBuf {
    if let Some(dir) = xdg.filter(|s| !s.is_empty()) {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return path;
        }
    }
    if let Some(home) = home.filter(|s| !s.is_empty()) {
        let path = PathBuf::from(home);
        if path.is_absolute() {
            return path.join(relative);
        }
    }
    std::env::temp_dir().join("taix-state").join(relative)
}

/// The web server's pairing key: 24 hex chars, generated once and kept at
/// `$XDG_DATA_HOME/taix/web.key` (mode 0600) so a paired phone stays
/// paired across restarts.
pub fn web_key() -> std::io::Result<String> {
    use std::fs;
    let path = web_key_path();
    if let Ok(key) = fs::read_to_string(&path) {
        let key = key.trim();
        if key.len() == 24 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(key.to_string());
        }
    }
    let key = random_key();
    write_web_key(&path, &key)?;
    Ok(key)
}

pub fn set_web_key(key: &str) -> std::io::Result<()> {
    write_web_key(&web_key_path(), key)
}

fn web_key_path() -> std::path::PathBuf {
    use std::fs;
    let dir = base_dir(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        ".local/share",
    )
    .join("taix");
    let _ = fs::create_dir_all(&dir);
    dir.join("web.key")
}

fn write_web_key(path: &std::path::Path, key: &str) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(key.as_bytes())
}

/// A fresh 24-hex-char key from the kernel's pool.
pub fn random_key() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 12];
    // /dev/urandom cannot fail to open on Linux short of fd exhaustion, and
    // a key file that cannot be written is the caller's case to handle.
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_never_lands_in_the_current_directory() {
        // The regression: no XDG var and no HOME used to yield the relative
        // `.local/share`, so TaiX wrote its database into the cwd.
        let fallback = base_dir(None, None, ".local/share");
        assert!(fallback.is_absolute(), "{fallback:?}");
        // A relative XDG var or HOME is equally unusable and must be refused.
        assert!(base_dir(Some("relative/dir"), None, ".config").is_absolute());
        assert!(base_dir(None, Some("also/relative"), ".config").is_absolute());
    }

    #[test]
    fn xdg_wins_over_home_and_empty_values_are_skipped() {
        assert_eq!(
            base_dir(Some("/xdg"), Some("/home/u"), ".config"),
            PathBuf::from("/xdg")
        );
        assert_eq!(
            base_dir(Some(""), Some("/home/u"), ".config"),
            PathBuf::from("/home/u/.config")
        );
        assert_eq!(
            base_dir(None, Some("/home/u"), ".local/share"),
            PathBuf::from("/home/u/.local/share")
        );
    }

    #[test]
    fn partial_config_merges_over_defaults() {
        let partial_toml = r#"
            tmux_socket = "custom"
        "#;

        let config: Config = toml::from_str(partial_toml).unwrap();

        assert_eq!(config.tmux_socket, "custom");
        // Defaults should still be present
        assert_eq!(config.tmux_session, "taix");
        assert_eq!(config.idle_after_ms, 2000);
        // Harnesses are no longer configured by default: the catalogue plus
        // PATH decides what is offered, so an empty map is correct here.
        assert!(config.agents.is_empty());
        assert!(!config.isolate, "isolation must be opt-in");
    }

    #[test]
    fn a_configured_harness_is_parsed() {
        let config: Config = toml::from_str(
            r#"
            isolate = true
            [agents.mine]
            label = "My Wrapper"
            command = "/opt/mine/run.sh --flag"
        "#,
        )
        .unwrap();

        assert!(config.isolate);
        let mine = config.agents.get("mine").expect("configured harness");
        assert_eq!(mine.label, "My Wrapper");
        // Used verbatim: there is no {task}/{name} substitution any more.
        assert_eq!(mine.command, "/opt/mine/run.sh --flag");
    }

    #[test]
    fn a_saved_config_reads_back_and_omits_the_unset() {
        let mut cfg = Config {
            harness_order: vec!["omp".into(), "claude".into()],
            ..Default::default()
        };
        cfg.agents.insert(
            "claude".into(),
            AgentKind {
                color: Some("teal".into()),
                hidden: true,
                ..Default::default()
            },
        );
        let text = toml::to_string_pretty(&cfg).unwrap();
        // A partial override stays partial on disk: no empty label/command.
        assert!(!text.contains("label = \"\""), "{text}");
        assert!(!text.contains("theme"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.harness_order, cfg.harness_order);
        assert_eq!(back.agents, cfg.agents);
    }
}
