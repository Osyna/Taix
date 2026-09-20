mod config;
#[cfg(not(target_os = "linux"))]
compile_error!("taix reads /proc and installs systemd user units; only Linux is supported");

pub mod diff;
pub mod doctor;
pub mod editor;
mod harness;
pub mod history;
pub mod jobs;
pub mod log;
pub mod mem;
mod project_config;
pub mod session;
pub mod spawn;
pub mod stash;
mod store;
pub mod tailscale;
pub mod terminal;
#[cfg(test)]
mod testenv;
pub mod text;
pub mod trace;
mod watcher;

pub use config::{AgentKind, Config, random_key, set_web_key, web_key};
pub use harness::{
    Entry, Harness, TERMINAL, adopt_shell_path, available, by_id, catalog, entries, next_name,
    running_harnesses, shell_path, terminal, which,
};
pub use project_config::ProjectConfig;
pub use store::{Action, Agent, Job, NewAgent, NewJob, Notify, Project, Run, Store, purge_project};
pub use watcher::Watcher;

pub type ProjectId = i64;
pub type AgentId = i64;
pub type JobId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Starting,
    Working,
    Waiting,
    Idle,
    Done,
    Failed,
}

impl AgentState {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentState::Starting => "starting",
            AgentState::Working => "working",
            AgentState::Waiting => "waiting",
            AgentState::Idle => "idle",
            AgentState::Done => "done",
            AgentState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<AgentState> {
        match s {
            "starting" => Some(AgentState::Starting),
            "working" => Some(AgentState::Working),
            "waiting" => Some(AgentState::Waiting),
            "idle" => Some(AgentState::Idle),
            "done" => Some(AgentState::Done),
            "failed" => Some(AgentState::Failed),
            _ => None,
        }
    }

    pub fn needs_attention(self) -> bool {
        matches!(self, AgentState::Waiting | AgentState::Failed)
    }
}

impl serde::Serialize for AgentState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for AgentState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        // A state written by a newer build must not fail the whole load; an
        // unrecognised agent reads as idle and the watcher re-derives it.
        let raw = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Ok(AgentState::parse(&raw).unwrap_or(AgentState::Idle))
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Config(toml::de::Error),
    Encode(toml::ser::Error),
    Duplicate,
    Generic(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {}", e),
            Error::Config(e) => write!(f, "config error: {}", e),
            Error::Encode(e) => write!(f, "cannot write state: {}", e),
            Error::Duplicate => write!(f, "duplicate project root"),
            Error::Generic(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Config(e) => Some(e),
            Error::Encode(e) => Some(e),
            Error::Duplicate | Error::Generic(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::Config(e)
    }
}

impl From<toml::ser::Error> for Error {
    fn from(e: toml::ser::Error) -> Self {
        Error::Encode(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
