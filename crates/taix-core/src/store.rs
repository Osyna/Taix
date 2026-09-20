//! Project and agent records, persisted as one small TOML file.
//!
//! This replaced SQLite, which was 1.3 MB of linked code — 49% of every
//! binary — backing a 16 KB database of two tables and tens of rows.
//!
//! SQLite was not chosen arbitrarily, though: `taix add`, `taix rm`, the MCP
//! server and the window are four independent processes writing the same file,
//! and its locking was load-bearing. That guarantee is preserved here rather
//! than dropped:
//!
//! * every mutation is a read-modify-write under an **exclusive `flock`** on a
//!   sidecar lock file, so a concurrent writer cannot interleave and lose a row;
//! * the lock lives in its own file that is never replaced, because locks
//!   belong to inodes — locking the data file and then renaming over it would
//!   leave a second process holding a lock on an orphaned inode;
//! * the new contents are written to a temporary file, fsynced and **renamed**
//!   over the target, so a crash mid-write leaves the previous state intact
//!   rather than a truncated file.

use crate::{AgentId, AgentState, Error, JobId, ProjectId, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub project: ProjectId,
    pub name: String,
    pub kind: String,
    // TOML has no null, so absent fields must be skipped rather than emitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub named: bool,
    pub state: AgentState,
}

fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Clone)]
pub struct NewAgent {
    pub project: ProjectId,
    pub name: String,
    pub kind: String,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
}

/// What a scheduled job does when its slot comes round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Shell,
    Agent,
    Session,
    Git,
}

/// A single run record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub at: i64,
    pub exit: Option<i32>,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// When a finished job is worth a desktop notification. Failure by default:
/// a job you asked for silently is one you only want to hear about when it
/// did not happen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Notify {
    Never,
    #[default]
    Failure,
    Always,
}

/// One scheduled job.
///
/// Flat on purpose: TOML round-trips this as a `[[jobs]]` block a human can
/// hand-edit, where a data-carrying enum nests awkwardly. [`Job::validate`]
/// enforces the invariant the flatness allows to be broken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// Cron expression or `@…` shorthand; parsed by `jobs::parse`.
    pub schedule: String,
    pub action: Action,
    /// Shell: the command line. Unused by other actions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// Agent: harness id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harness: String,
    /// Agent: what to type once the harness is ready. Empty = just open it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// Agent: send to a live window of this harness rather than opening one.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reuse: bool,
    /// Session: saved-session name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// Shell: overrides the project root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Shell: kill after this many seconds. `None` = 900.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Desktop notification policy: when to fire a notification after a run.
    #[serde(default)]
    pub notify: Notify,
    /// Whether to run a stale job on wake. When false, a job more than 120
    /// seconds overdue is stamped but not run.
    #[serde(default = "yes")]
    pub catch_up: bool,
    /// Unix seconds the job was created; the schedule baseline before a
    /// first run.
    #[serde(default)]
    pub created: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<i64>,
    /// Last 20 runs, newest last.
    #[serde(default)]
    pub runs: Vec<Run>,
}

impl Job {
    /// What the flat shape cannot express in types. Called before every run
    /// and before every save, so a half-written job fails loudly rather than
    /// silently doing nothing.
    pub fn validate(&self) -> Result<(), String> {
        match self.action {
            Action::Shell if self.command.trim().is_empty() => Err("no command to run".to_string()),
            Action::Agent if self.harness.trim().is_empty() => Err("no agent chosen".to_string()),
            Action::Agent if self.project.is_none() => {
                Err("an agent job needs a project".to_string())
            }
            Action::Session if self.session.trim().is_empty() => {
                Err("no session chosen".to_string())
            }
            Action::Git if !matches!(self.command.as_str(), "fetch" | "pull" | "push") => {
                Err(format!(
                    "git command must be fetch, pull or push, not '{}'",
                    self.command
                ))
            }
            _ => Ok(()),
        }
    }

    /// The most recent run, if any.
    pub fn last(&self) -> Option<&Run> {
        self.runs.last()
    }

    /// Did the last run fail? A non-zero exit, or no exit at all because
    /// the job could not be started.
    pub fn failing(&self) -> bool {
        self.last()
            .is_some_and(|r| r.exit.is_some_and(|c| c != 0) || r.error.is_some())
    }
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub name: String,
    pub enabled: bool,
    pub project: Option<ProjectId>,
    pub schedule: String,
    pub action: Action,
}

fn yes() -> bool {
    true
}

/// The whole persisted state. Small by construction: tens of rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Data {
    /// Schema version written by this build. Missing = 1 for existing installs.
    #[serde(default = "default_version")]
    version: u32,
    /// Monotonic id counters. Ids are never reused: a pane/window mapping
    /// keyed by a recycled id would attach to the wrong agent.
    #[serde(default)]
    next_project: ProjectId,
    #[serde(default)]
    next_agent: AgentId,
    /// Scalars must precede the arrays-of-tables: `toml::to_string_pretty`
    /// refuses to emit a value after a table.
    #[serde(default)]
    next_job: JobId,
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    agents: Vec<Agent>,
    #[serde(default)]
    jobs: Vec<Job>,
}

const CURRENT_VERSION: u32 = 1;

fn default_version() -> u32 {
    CURRENT_VERSION
}

impl Data {
    /// Agents and jobs are kept in id order at rest, so every accessor is
    /// a clone rather than a clone and a sort. Projects are deliberately
    /// left in the order the user put them in.
    fn sort(&mut self) {
        self.agents.sort_by_key(|a| a.id);
        self.jobs.sort_by_key(|j| j.id);
    }
}

enum Backend {
    File {
        path: PathBuf,
        lock: PathBuf,
    },
    /// Test-only backend with the same semantics and no filesystem.
    Memory(Mutex<Data>),
}

pub struct Store {
    backend: Backend,
}

/// Holds an `flock` for as long as it is alive.
struct Guard(File);

impl Guard {
    fn acquire(path: &Path, exclusive: bool) -> Result<Guard> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if exclusive {
            file.lock()?;
        } else {
            file.lock_shared()?;
        }
        Ok(Guard(file))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Closing the descriptor would release the lock anyway; being explicit
        // keeps the release ordered before the file is dropped.
        let _ = self.0.unlock();
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = path.with_extension("lock");
        // Fail now if the directory is unwritable, rather than on first write.
        drop(Guard::acquire(&lock, false)?);
        // And read once, so a file this build cannot understand is refused
        // here - where a front end reports it - instead of degrading every
        // later read to "no projects" and looking like data loss.
        Self::read_file(path)?;
        Ok(Store {
            backend: Backend::File {
                path: path.to_path_buf(),
                lock,
            },
        })
    }

    pub fn open_memory() -> Result<Store> {
        Ok(Store {
            backend: Backend::Memory(Mutex::new(Data::default())),
        })
    }

    /// A stamp that changes whenever another process rewrites the file:
    /// every save is a fresh file renamed into place, so its mtime and
    /// length move together. One `stat`, where loading the store to compare
    /// it costs a lock, a read and a TOML parse - which the fronts used to
    /// pay twice a second. `None` for a store with no file.
    pub fn stamp(&self) -> Option<(std::time::SystemTime, u64)> {
        let Backend::File { path, .. } = &self.backend else {
            return None;
        };
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    fn load(path: &Path) -> Result<Data> {
        Ok(Self::read_file(path)?.0)
    }
    /// The store and the exact text it came from, so a mutation can tell
    /// whether it changed anything without serialising twice.
    fn read_file(path: &Path) -> Result<(Data, String)> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // A store that has never been written is an empty store.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let mut data: Data = toml::from_str(&text)?;
        if data.version > CURRENT_VERSION {
            return Err(Error::Generic(format!(
                "{}: version {} is newer than this build ({}); upgrade taix",
                path.display(),
                data.version,
                CURRENT_VERSION
            )));
        }
        data.version = CURRENT_VERSION;
        data.sort();
        Ok((data, text))
    }

    fn save(path: &Path, text: &str) -> Result<()> {
        // Same directory, so the rename is atomic within one filesystem.
        let tmp = path.with_extension(format!("new.{}", std::process::id()));
        {
            let mut file = File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read a consistent snapshot.
    fn read<T>(&self, f: impl FnOnce(&Data) -> T) -> Result<T> {
        match &self.backend {
            Backend::Memory(m) => Ok(f(&m.lock().unwrap_or_else(PoisonError::into_inner))),
            Backend::File { path, lock } => {
                let _guard = Guard::acquire(lock, false)?;
                Ok(f(&Self::load(path)?))
            }
        }
    }

    /// Read-modify-write under an exclusive lock. On `Err` nothing is written,
    /// so a rejected mutation cannot leave a half-applied state behind. A
    /// mutation that changed nothing writes nothing either: a save is a
    /// write, an `fsync` and a rename, and the fronts settle state on a
    /// cadence where "nothing changed" is the common answer.
    fn mutate<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        match &self.backend {
            Backend::Memory(m) => {
                let mut data = m.lock().unwrap_or_else(PoisonError::into_inner);
                let out = f(&mut data)?;
                data.sort();
                Ok(out)
            }
            Backend::File { path, lock } => {
                let _guard = Guard::acquire(lock, true)?;
                let (mut data, text) = Self::read_file(path)?;
                let out = f(&mut data)?;
                data.sort();
                let next = toml::to_string_pretty(&data)?;
                if next != text {
                    Self::save(path, &next)?;
                }
                Ok(out)
            }
        }
    }

    /// Projects in the order the user put them in: `add_project` appends, so
    /// a new one lands at the bottom, and `move_project` is the only thing
    /// that changes it. Sorting by name here instead would mean renaming a
    /// project silently moved it.
    pub fn projects(&self) -> Result<Vec<Project>> {
        self.read(|d| d.projects.clone())
    }

    /// Put `id` at `index`, counted among the *other* projects - which is
    /// what a drop between two rows means.
    pub fn move_project(&self, id: ProjectId, index: usize) -> Result<()> {
        self.mutate(|d| {
            let Some(from) = d.projects.iter().position(|p| p.id == id) else {
                return Ok(());
            };
            let project = d.projects.remove(from);
            let at = index.min(d.projects.len());
            d.projects.insert(at, project);
            Ok(())
        })
    }

    pub fn add_project(&self, name: &str, root: &Path) -> Result<Project> {
        // A non-UTF-8 root cannot round-trip through TOML, and rejecting it
        // here is better than writing a file we can never read back.
        if root.to_str().is_none() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not valid UTF-8",
            )));
        }
        self.mutate(|d| {
            if d.projects.iter().any(|p| p.root == root) {
                return Err(Error::Duplicate);
            }
            d.next_project += 1;
            let project = Project {
                id: d.next_project,
                name: name.to_string(),
                root: root.to_path_buf(),
            };
            d.projects.push(project.clone());
            Ok(project)
        })
    }

    pub fn remove_project(&self, id: ProjectId) -> Result<()> {
        self.mutate(|d| {
            d.projects.retain(|p| p.id != id);
            // What the SQL schema did with ON DELETE CASCADE.
            d.agents.retain(|a| a.project != id);
            Ok(())
        })
    }

    pub fn agents(&self, project: ProjectId) -> Result<Vec<Agent>> {
        self.read(|d| {
            d.agents
                .iter()
                .filter(|a| a.project == project)
                .cloned()
                .collect()
        })
    }

    pub fn all_agents(&self) -> Result<Vec<Agent>> {
        self.read(|d| d.agents.clone())
    }

    pub fn agent(&self, id: AgentId) -> Result<Option<Agent>> {
        self.read(|d| d.agents.iter().find(|a| a.id == id).cloned())
    }

    /// Every agent, with the ones whose pane is gone settled first - in one
    /// write, where the fronts used to make one per agent. `alive` answers
    /// for a pane id.
    ///
    /// A plain terminal with no live shell is a closed tab, not a result
    /// worth keeping: `Ctrl-D` should make the window go away, the way it
    /// does in every terminal. An agent's record stays, because how it
    /// finished is the thing you wanted to see, and a worktree must not be
    /// forgotten silently. `Starting` is spared: its pane is assigned a
    /// moment after the row exists. Whatever stays loses its pane, and one
    /// that was still Working is Done: leaving it claiming Working is the
    /// difference between a dashboard and a liar.
    pub fn reconcile(&self, alive: impl Fn(u32) -> bool) -> Result<Vec<Agent>> {
        let terminal = crate::terminal().id;
        self.mutate(|d| {
            d.agents.retain(|a| {
                a.pane.is_some_and(&alive)
                    || a.kind != terminal
                    || a.worktree.is_some()
                    || a.named
                    || a.state == AgentState::Starting
            });
            for a in &mut d.agents {
                // No pane yet is not a dead pane: `spawn` assigns it a
                // moment after the row exists, and a reload from another
                // process can land in between.
                let Some(pane) = a.pane else { continue };
                if alive(pane) {
                    continue;
                }
                a.pane = None;
                a.window = None;
                if matches!(a.state, AgentState::Starting | AgentState::Working) {
                    a.state = AgentState::Done;
                }
            }
            Ok(d.agents.clone())
        })
    }

    pub fn add_agent(&self, new: &NewAgent) -> Result<Agent> {
        self.mutate(|d| {
            d.next_agent += 1;
            let agent = Agent {
                id: d.next_agent,
                project: new.project,
                name: new.name.clone(),
                kind: new.kind.clone(),
                worktree: new.worktree.clone(),
                branch: new.branch.clone(),
                window: None,
                pane: None,
                color: None,
                named: false,
                state: AgentState::Starting,
            };
            d.agents.push(agent.clone());
            Ok(agent)
        })
    }

    pub fn set_state(&self, id: AgentId, state: AgentState) -> Result<()> {
        self.mutate(|d| {
            if let Some(agent) = d.agents.iter_mut().find(|a| a.id == id) {
                agent.state = state;
            }
            Ok(())
        })
    }

    pub fn set_tmux(&self, id: AgentId, window: Option<u32>, pane: Option<u32>) -> Result<()> {
        self.mutate(|d| {
            if let Some(agent) = d.agents.iter_mut().find(|a| a.id == id) {
                agent.window = window;
                agent.pane = pane;
            }
            Ok(())
        })
    }

    pub fn remove_agent(&self, id: AgentId) -> Result<()> {
        self.mutate(|d| {
            d.agents.retain(|a| a.id != id);
            Ok(())
        })
    }

    pub fn rename_project(&self, id: ProjectId, name: &str) -> Result<()> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "name cannot be empty",
            )));
        }
        self.mutate(|d| {
            if let Some(project) = d.projects.iter_mut().find(|p| p.id == id) {
                project.name = trimmed.to_string();
            }
            Ok(())
        })
    }

    pub fn rename_agent(&self, id: AgentId, name: &str) -> Result<()> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "name cannot be empty",
            )));
        }
        self.mutate(|d| {
            if let Some(agent) = d.agents.iter_mut().find(|a| a.id == id) {
                agent.name = trimmed.to_string();
                agent.named = true;
            }
            Ok(())
        })
    }

    pub fn set_harness(&self, id: AgentId, kind: &str, name: &str) -> Result<()> {
        self.mutate(|d| {
            if let Some(agent) = d.agents.iter_mut().find(|a| a.id == id) {
                agent.kind = kind.to_string();
                agent.name = name.to_string();
            }
            Ok(())
        })
    }

    pub fn set_color(&self, id: AgentId, color: Option<&str>) -> Result<()> {
        self.mutate(|d| {
            if let Some(agent) = d.agents.iter_mut().find(|a| a.id == id) {
                agent.color = color.map(|s| s.to_string());
            }
            Ok(())
        })
    }

    pub fn jobs(&self) -> Result<Vec<Job>> {
        self.read(|d| d.jobs.clone())
    }

    pub fn job(&self, id: JobId) -> Result<Option<Job>> {
        self.read(|d| d.jobs.iter().find(|j| j.id == id).cloned())
    }

    pub fn add_job(&self, new: &NewJob) -> Result<Job> {
        self.mutate(|d| {
            d.next_job += 1;
            let job = Job {
                id: d.next_job,
                name: new.name.clone(),
                enabled: new.enabled,
                project: new.project,
                schedule: new.schedule.clone(),
                action: new.action,
                command: String::new(),
                harness: String::new(),
                prompt: String::new(),
                reuse: false,
                session: String::new(),
                cwd: None,
                timeout_secs: None,
                notify: Notify::default(),
                catch_up: true,
                created: crate::jobs::now(),
                last_run: None,
                runs: Vec::new(),
            };
            d.jobs.push(job.clone());
            Ok(job)
        })
    }

    /// Save an edited definition. Run state (`created`, `last_run`, `runs`)
    /// is deliberately not copied: a dialog saving an edit must not clobber
    /// a run a background runner recorded a moment earlier. An unknown id is
    /// a no-op.
    pub fn put_job(&self, job: &Job) -> Result<()> {
        self.mutate(|d| {
            if let Some(stored) = d.jobs.iter_mut().find(|j| j.id == job.id) {
                stored.name = job.name.clone();
                stored.enabled = job.enabled;
                stored.project = job.project;
                stored.schedule = job.schedule.clone();
                stored.action = job.action;
                stored.command = job.command.clone();
                stored.harness = job.harness.clone();
                stored.prompt = job.prompt.clone();
                stored.reuse = job.reuse;
                stored.session = job.session.clone();
                stored.cwd = job.cwd.clone();
                stored.timeout_secs = job.timeout_secs;
                stored.notify = job.notify;
                stored.catch_up = job.catch_up;
            }
            Ok(())
        })
    }

    pub fn remove_job(&self, id: JobId) -> Result<()> {
        self.mutate(|d| {
            d.jobs.retain(|j| j.id != id);
            Ok(())
        })
    }

    pub fn set_job_enabled(&self, id: JobId, on: bool) -> Result<()> {
        self.mutate(|d| {
            if let Some(job) = d.jobs.iter_mut().find(|j| j.id == id) {
                job.enabled = on;
            }
            Ok(())
        })
    }

    /// Take every job that is due, stamping `last_run` in the same
    /// exclusive-locked write that selects them. A systemd tick and a front
    /// tick racing on the same slot cannot both win.
    pub fn claim_due_jobs(&self, now: i64) -> Result<Vec<Job>> {
        self.mutate(|d| {
            let mut due = Vec::new();
            for job in &mut d.jobs {
                if !job.enabled {
                    continue;
                }
                let Ok(schedule) = crate::jobs::parse(&job.schedule) else {
                    continue;
                };
                let base = job.last_run.unwrap_or(job.created);
                let Some(next) = schedule.next_after(base) else {
                    continue;
                };
                if next > now {
                    continue;
                }
                let stale = now - next > 120;
                job.last_run = Some(now);
                // One-shot jobs are disabled after they fire
                if matches!(schedule, crate::jobs::Schedule::At(_)) {
                    job.enabled = false;
                }
                if !stale || job.catch_up {
                    due.push(job.clone());
                }
            }
            Ok(due)
        })
    }

    pub fn record_run(
        &self,
        id: JobId,
        exit: Option<i32>,
        ms: u64,
        error: Option<&str>,
    ) -> Result<()> {
        self.mutate(|d| {
            if let Some(job) = d.jobs.iter_mut().find(|j| j.id == id) {
                let run = Run {
                    at: job.last_run.unwrap_or(crate::jobs::now()),
                    exit,
                    ms,
                    error: error.map(|s| s.to_string()),
                };
                job.runs.push(run);
                if job.runs.len() > 20 {
                    job.runs.remove(0);
                }
            }
            Ok(())
        })
    }

    /// Move a job's schedule baseline without claiming it: what a forced
    /// run does, so the interval restarts from the forced run.
    pub fn stamp_run(&self, id: JobId, at: i64) -> Result<()> {
        self.mutate(|d| {
            if let Some(job) = d.jobs.iter_mut().find(|j| j.id == id) {
                job.last_run = Some(at);
            }
            Ok(())
        })
    }
}

/// Kill every tmux pane belonging to `project`, then drop its records.
///
/// Worktrees are deliberately left on disk: removing a project must never
/// discard work the user has not reviewed.
pub fn purge_project(
    store: &Store,
    project: ProjectId,
    mut kill_pane: impl FnMut(u32),
) -> Result<(), String> {
    for agent in store.agents(project).map_err(|e| e.to_string())? {
        if let Some(pane) = agent.pane {
            kill_pane(pane);
        }
    }
    store.remove_project(project).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempdir::Dir, Store) {
        let dir = tempdir::Dir::new();
        let store = Store::open(&dir.path().join("state.toml")).unwrap();
        (dir, store)
    }

    /// Minimal scratch directory; the store is the only thing under test.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Dir {
                let path = std::env::temp_dir().join(format!(
                    "taix-store-test-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Dir(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn cascade_delete_removes_agents() {
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();

        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "agent1".into(),
                kind: "claude".into(),
                worktree: None,
                branch: None,
            })
            .unwrap();

        assert_eq!(store.agents(proj.id).unwrap().len(), 1);

        store.remove_project(proj.id).unwrap();

        assert!(store.agent(agent.id).unwrap().is_none());
    }

    #[test]
    fn duplicate_project_root_rejected() {
        let store = Store::open_memory().unwrap();
        store.add_project("proj1", Path::new("/tmp/test")).unwrap();

        let err = store
            .add_project("proj2", Path::new("/tmp/test"))
            .unwrap_err();
        assert!(matches!(err, Error::Duplicate));
    }

    #[test]
    fn agent_round_trip_with_state_and_tmux() {
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();

        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "agent1".into(),
                kind: "claude".into(),
                worktree: Some(PathBuf::from("/tmp/wt")),
                branch: Some("main".into()),
            })
            .unwrap();

        assert_eq!(agent.state, AgentState::Starting);
        assert_eq!(agent.window, None);
        assert_eq!(agent.pane, None);

        store.set_state(agent.id, AgentState::Working).unwrap();
        store.set_tmux(agent.id, Some(1), Some(2)).unwrap();

        let retrieved = store.agent(agent.id).unwrap().unwrap();
        assert_eq!(retrieved.state, AgentState::Working);
        assert_eq!(retrieved.window, Some(1));
        assert_eq!(retrieved.pane, Some(2));

        store.set_tmux(agent.id, None, None).unwrap();
        let retrieved = store.agent(agent.id).unwrap().unwrap();
        assert_eq!(retrieved.window, None);
        assert_eq!(retrieved.pane, None);
    }

    #[test]
    fn reconcile_settles_dead_panes_by_the_fronts_rules() {
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();
        let terminal = crate::terminal().id;
        let add = |kind: &str, worktree: Option<&str>| {
            store
                .add_agent(&NewAgent {
                    project: proj.id,
                    name: kind.to_string(),
                    kind: kind.to_string(),
                    worktree: worktree.map(PathBuf::from),
                    branch: None,
                })
                .unwrap()
                .id
        };
        // A closed plain terminal is a closed tab: gone.
        let closed_shell = add(&terminal, None);
        store.set_tmux(closed_shell, Some(1), Some(1)).unwrap();
        store.set_state(closed_shell, AgentState::Idle).unwrap();
        // A named one, or one with a worktree, is kept, pane cleared.
        let named_shell = add(&terminal, None);
        store.set_tmux(named_shell, Some(2), Some(2)).unwrap();
        store.set_state(named_shell, AgentState::Idle).unwrap();
        store.rename_agent(named_shell, "mine").unwrap();
        let wt_shell = add(&terminal, Some("/tmp/wt"));
        store.set_tmux(wt_shell, Some(3), Some(3)).unwrap();
        store.set_state(wt_shell, AgentState::Working).unwrap();
        // A starting terminal has no pane yet and must not be swept.
        let starting = add(&terminal, None);
        // An agent that died Working is Done; one that is alive is untouched.
        let dead_agent = add("claude", None);
        store.set_tmux(dead_agent, Some(4), Some(4)).unwrap();
        store.set_state(dead_agent, AgentState::Working).unwrap();
        let live_agent = add("claude", None);
        store.set_tmux(live_agent, Some(5), Some(5)).unwrap();
        store.set_state(live_agent, AgentState::Working).unwrap();

        let agents = store.reconcile(|pane| pane == 5).unwrap();
        let by_id = |id| agents.iter().find(|a| a.id == id);
        assert!(by_id(closed_shell).is_none());
        let named = by_id(named_shell).unwrap();
        assert_eq!(
            (named.pane, named.window, named.state),
            (None, None, AgentState::Idle)
        );
        assert_eq!(by_id(wt_shell).unwrap().state, AgentState::Done);
        assert_eq!(by_id(starting).unwrap().state, AgentState::Starting);
        assert_eq!(by_id(dead_agent).unwrap().state, AgentState::Done);
        let live = by_id(live_agent).unwrap();
        assert_eq!((live.pane, live.state), (Some(5), AgentState::Working));
        assert_eq!(agents.len(), 5);
        // The store agrees with what was returned.
        assert_eq!(store.all_agents().unwrap(), agents);
    }

    #[test]
    fn a_mutation_that_changes_nothing_does_not_rewrite_the_file() {
        let (dir, store) = temp_store();
        let path = dir.path().join("state.toml");
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();
        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "a".into(),
                kind: "claude".into(),
                worktree: None,
                branch: None,
            })
            .unwrap();
        store.set_state(agent.id, AgentState::Working).unwrap();
        let before = store.stamp().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Same state again, and a reconcile that finds every pane alive.
        store.set_state(agent.id, AgentState::Working).unwrap();
        store.reconcile(|_| true).unwrap();
        assert_eq!(store.stamp().unwrap(), before, "{}", path.display());
        store.set_state(agent.id, AgentState::Done).unwrap();
        assert_ne!(store.stamp().unwrap(), before);
    }

    #[test]
    fn unknown_state_degrades_to_idle() {
        let (dir, store) = temp_store();
        let path = dir.path().join("state.toml");
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();
        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "agent1".into(),
                kind: "claude".into(),
                worktree: None,
                branch: None,
            })
            .unwrap();

        // A state string this build does not know must not fail the whole load.
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("state = \"starting\"", "state = \"teleporting\"");
        std::fs::write(&path, text).unwrap();

        let retrieved = store.agent(agent.id).unwrap().unwrap();
        assert_eq!(retrieved.state, AgentState::Idle);
    }

    #[test]
    fn state_survives_reopening_the_file() {
        // The whole point of a persistent store, and the one thing the
        // in-memory backend cannot demonstrate.
        let dir = tempdir::Dir::new();
        let path = dir.path().join("state.toml");
        let id = {
            let store = Store::open(&path).unwrap();
            let proj = store.add_project("proj", Path::new("/tmp/p")).unwrap();
            let agent = store
                .add_agent(&NewAgent {
                    project: proj.id,
                    name: "a".into(),
                    kind: "claude".into(),
                    worktree: Some(PathBuf::from("/tmp/wt")),
                    branch: Some("taix/a".into()),
                })
                .unwrap();
            store.set_state(agent.id, AgentState::Waiting).unwrap();
            store.set_tmux(agent.id, Some(7), Some(9)).unwrap();
            agent.id
        };

        let reopened = Store::open(&path).unwrap();
        let agent = reopened.agent(id).unwrap().unwrap();
        assert_eq!(agent.state, AgentState::Waiting);
        assert_eq!(agent.window, Some(7));
        assert_eq!(agent.pane, Some(9));
        assert_eq!(agent.worktree, Some(PathBuf::from("/tmp/wt")));
        assert_eq!(agent.branch, Some("taix/a".into()));
        assert_eq!(reopened.projects().unwrap().len(), 1);
    }

    #[test]
    fn ids_are_never_reused_after_removal() {
        // A recycled id would let a stale window/pane mapping attach to a new
        // agent. SQLite's rowid could reuse; the counter must not.
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("p", Path::new("/tmp/p")).unwrap();
        let mk = |n: &str| NewAgent {
            project: proj.id,
            name: n.into(),
            kind: "claude".into(),
            worktree: None,
            branch: None,
        };

        let first = store.add_agent(&mk("a")).unwrap();
        store.remove_agent(first.id).unwrap();
        let second = store.add_agent(&mk("b")).unwrap();

        assert_ne!(first.id, second.id);
    }

    #[test]
    fn concurrent_writers_do_not_lose_rows() {
        // This is the guarantee SQLite was carrying: `taix add`, `taix rm`,
        // the MCP server and the window all write this file. Two independent
        // handles interleaving read-modify-write must not clobber each other.
        let dir = tempdir::Dir::new();
        let path = dir.path().join("state.toml");
        let seed = Store::open(&path).unwrap();
        let proj = seed.add_project("p", Path::new("/tmp/p")).unwrap();

        let threads: Vec<_> = (0..4)
            .map(|t| {
                let path = path.clone();
                let project = proj.id;
                std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    for i in 0..10 {
                        store
                            .add_agent(&NewAgent {
                                project,
                                name: format!("t{t}-{i}"),
                                kind: "claude".into(),
                                worktree: None,
                                branch: None,
                            })
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let agents = seed.all_agents().unwrap();
        assert_eq!(agents.len(), 40, "a writer clobbered another's rows");
        let mut ids: Vec<_> = agents.iter().map(|a| a.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), 40, "duplicate ids handed out");
    }

    #[test]
    fn a_leftover_temp_file_is_not_mistaken_for_state() {
        // A crash between write and rename leaves the temp file behind; the
        // previous state must still load.
        let dir = tempdir::Dir::new();
        let path = dir.path().join("state.toml");
        let store = Store::open(&path).unwrap();
        store.add_project("keep", Path::new("/tmp/keep")).unwrap();

        std::fs::write(path.with_extension("new.999"), "garbage = [[[").unwrap();

        let projects = Store::open(&path).unwrap().projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "keep");
    }

    #[test]
    fn projects_keep_their_order_and_a_move_survives_a_reopen() {
        let dir = tempdir::Dir::new();
        let path = dir.path().join("state.toml");
        let store = Store::open(&path).unwrap();
        for name in ["one", "two", "three"] {
            store
                .add_project(name, &Path::new("/tmp").join(name))
                .unwrap();
        }
        let names = |s: &Store| -> Vec<String> {
            s.projects().unwrap().into_iter().map(|p| p.name).collect()
        };
        // A new project is the last one, not the alphabetically next one.
        assert_eq!(names(&store), ["one", "two", "three"]);

        let third = store.projects().unwrap()[2].id;
        store.move_project(third, 0).unwrap();
        assert_eq!(names(&store), ["three", "one", "two"]);

        // Dropped on the bottom half of the last card: past the end clamps.
        store.move_project(third, 99).unwrap();
        assert_eq!(names(&store), ["one", "two", "three"]);
        assert_eq!(names(&Store::open(&path).unwrap()), ["one", "two", "three"]);
    }

    #[test]
    fn agent_color_and_named_serde() {
        // Fields must deserialize when absent, skip when false/None
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();
        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "agent1".into(),
                kind: "claude".into(),
                worktree: None,
                branch: None,
            })
            .unwrap();

        // Fresh agent has no color, named=false
        let retrieved = store.agent(agent.id).unwrap().unwrap();
        assert_eq!(retrieved.color, None);
        assert!(!retrieved.named);

        // Serialize and check the TOML doesn't emit color or named
        let data = store.read(|d| d.clone()).unwrap();
        let toml = toml::to_string_pretty(&data).unwrap();
        assert!(!toml.contains("color"), "None color should be skipped");
        assert!(!toml.contains("named"), "false named should be skipped");

        // Deserialize from old TOML missing those fields
        let old_toml = r#"
            next_project = 1
            next_agent = 1
            [[projects]]
            id = 1
            name = "test"
            root = "/tmp/test"
            [[agents]]
            id = 1
            project = 1
            name = "agent1"
            kind = "claude"
            state = "idle"
        "#;
        let parsed: Data = toml::from_str(old_toml).unwrap();
        assert_eq!(parsed.agents[0].color, None);
        assert!(!parsed.agents[0].named);
    }

    #[test]
    fn rename_agent_sets_named() {
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();
        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "Auto Name".into(),
                kind: "claude".into(),
                worktree: None,
                branch: None,
            })
            .unwrap();

        // Initially not named
        assert!(!store.agent(agent.id).unwrap().unwrap().named);

        // Rename sets named
        store.rename_agent(agent.id, "My Custom Name").unwrap();
        let retrieved = store.agent(agent.id).unwrap().unwrap();
        assert_eq!(retrieved.name, "My Custom Name");
        assert!(retrieved.named);

        // Rejects empty/whitespace
        assert!(store.rename_agent(agent.id, "").is_err());
        assert!(store.rename_agent(agent.id, "   ").is_err());
        // Name unchanged after rejection
        assert_eq!(
            store.agent(agent.id).unwrap().unwrap().name,
            "My Custom Name"
        );
    }

    #[test]
    fn rename_project_trims_and_rejects_empty() {
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();

        store.rename_project(proj.id, "  New Name  ").unwrap();
        let retrieved = store
            .projects()
            .unwrap()
            .into_iter()
            .find(|p| p.id == proj.id)
            .unwrap();
        assert_eq!(retrieved.name, "New Name");

        assert!(store.rename_project(proj.id, "").is_err());
        assert!(store.rename_project(proj.id, "   ").is_err());
    }

    #[test]
    fn set_harness_changes_kind_and_name_not_named() {
        let store = Store::open_memory().unwrap();
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();
        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "Claude Code".into(),
                kind: "claude".into(),
                worktree: None,
                branch: None,
            })
            .unwrap();

        // set_harness changes kind and name but not named
        store.set_harness(agent.id, "omp", "Oh My Pi").unwrap();
        let retrieved = store.agent(agent.id).unwrap().unwrap();
        assert_eq!(retrieved.kind, "omp");
        assert_eq!(retrieved.name, "Oh My Pi");
        assert!(!retrieved.named);
    }

    #[test]
    fn set_color_round_trips() {
        let (dir, store) = temp_store();
        let path = dir.path().join("state.toml");
        let proj = store.add_project("test", Path::new("/tmp/test")).unwrap();
        let agent = store
            .add_agent(&NewAgent {
                project: proj.id,
                name: "agent".into(),
                kind: "claude".into(),
                worktree: None,
                branch: None,
            })
            .unwrap();

        // Set color
        store.set_color(agent.id, Some("red")).unwrap();
        assert_eq!(
            store.agent(agent.id).unwrap().unwrap().color,
            Some("red".to_string())
        );

        // Clear color
        store.set_color(agent.id, None).unwrap();
        assert_eq!(store.agent(agent.id).unwrap().unwrap().color, None);

        // Reopen and check None is still None
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.agent(agent.id).unwrap().unwrap().color, None);
    }

    fn a_job(store: &Store, schedule: &str) -> Job {
        store
            .add_job(&NewJob {
                name: "nightly".into(),
                enabled: true,
                project: None,
                schedule: schedule.into(),
                action: Action::Shell,
            })
            .unwrap()
    }

    #[test]
    fn a_due_job_is_claimed_exactly_once() {
        let store = Store::open_memory().unwrap();
        let job = a_job(&store, "@every 60");
        let due = job.created + 120;

        let first = store.claim_due_jobs(due).unwrap();
        assert_eq!(first.len(), 1, "the slot passed, so it is due");
        assert_eq!(first[0].last_run, Some(due), "claiming stamps the run");

        // A second runner on the same tick - a systemd timer against an
        // open front - must find nothing left to take.
        assert!(store.claim_due_jobs(due).unwrap().is_empty());

        // A disabled job is never claimed, however overdue.
        store.set_job_enabled(job.id, false).unwrap();
        assert!(store.claim_due_jobs(due + 10_000).unwrap().is_empty());
    }

    #[test]
    fn an_unparseable_schedule_is_skipped_not_claimed() {
        let store = Store::open_memory().unwrap();
        let job = a_job(&store, "every other tuesday");
        assert!(
            store
                .claim_due_jobs(job.created + 86400)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.job(job.id).unwrap().unwrap().last_run, None);
    }

    #[test]
    fn saving_an_edit_keeps_the_run_history() {
        let store = Store::open_memory().unwrap();
        let job = a_job(&store, "@every 60");
        store.claim_due_jobs(job.created + 120).unwrap();
        store
            .record_run(job.id, Some(3), 1200, Some("exit 3"))
            .unwrap();

        // What the dialog writes back: the definition it is holding, with
        // no idea a run happened a moment ago.
        let mut edit = job.clone();
        edit.name = "renamed".into();
        edit.schedule = "@hourly".into();
        edit.command = "echo hi".into();
        store.put_job(&edit).unwrap();

        let stored = store.job(job.id).unwrap().unwrap();
        assert_eq!(stored.name, "renamed");
        assert_eq!(stored.schedule, "@hourly");
        assert_eq!(stored.command, "echo hi");
        assert_eq!(stored.last_run, Some(job.created + 120));
        assert_eq!(stored.runs.len(), 1);
        assert_eq!(stored.runs[0].exit, Some(3));
        assert_eq!(stored.runs[0].error.as_deref(), Some("exit 3"));
    }

    #[test]
    fn jobs_round_trip_through_toml() {
        let (dir, store) = temp_store();
        let job = a_job(&store, "0 9 * * 1-5");
        let mut edit = job.clone();
        edit.command = "make test".into();
        edit.cwd = Some(PathBuf::from("/tmp"));
        edit.timeout_secs = Some(30);
        store.put_job(&edit).unwrap();
        store.record_run(job.id, Some(0), 500, None).unwrap();

        let reopened = Store::open(&dir.path().join("state.toml")).unwrap();
        let stored = reopened.job(job.id).unwrap().unwrap();
        assert_eq!(stored.action, Action::Shell);
        assert_eq!(stored.command, "make test");
        assert_eq!(stored.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(stored.runs.len(), 1);
        assert_eq!(stored.runs[0].exit, Some(0));
        assert!(stored.enabled);
    }

    #[test]
    fn record_run_caps_at_20() {
        let store = Store::open_memory().unwrap();
        let job = a_job(&store, "@every 60");
        store.claim_due_jobs(job.created + 120).unwrap();

        for i in 0..25 {
            store.record_run(job.id, Some(i), 100, None).unwrap();
        }

        let stored = store.job(job.id).unwrap().unwrap();
        assert_eq!(stored.runs.len(), 20, "capped at 20");
        assert_eq!(stored.runs[0].exit, Some(5), "oldest is #5");
        assert_eq!(stored.runs[19].exit, Some(24), "newest is #24");
    }

    #[test]
    fn catch_up_false_skips_stale_jobs() {
        let store = Store::open_memory().unwrap();
        let mut job_no_catchup = a_job(&store, "@every 60");
        job_no_catchup.catch_up = false;
        store.put_job(&job_no_catchup).unwrap();

        let job_with_catchup = a_job(&store, "@every 60");

        // One hour stale
        let stale_time = job_no_catchup.created + 3600;

        let claimed = store.claim_due_jobs(stale_time).unwrap();
        let claimed_ids: Vec<_> = claimed.iter().map(|j| j.id).collect();

        assert!(
            !claimed_ids.contains(&job_no_catchup.id),
            "catch_up=false job is stamped but not returned"
        );
        assert!(
            claimed_ids.contains(&job_with_catchup.id),
            "catch_up=true job is returned"
        );

        // Both should have been stamped
        assert_eq!(
            store.job(job_no_catchup.id).unwrap().unwrap().last_run,
            Some(stale_time)
        );
        assert_eq!(
            store.job(job_with_catchup.id).unwrap().unwrap().last_run,
            Some(stale_time)
        );
    }

    #[test]
    fn git_action_validates_command() {
        let store = Store::open_memory().unwrap();
        let mut job = store
            .add_job(&NewJob {
                name: "sync".into(),
                enabled: true,
                project: None,
                schedule: "@daily".into(),
                action: Action::Git,
            })
            .unwrap();

        job.command = "pull".into();
        assert!(job.validate().is_ok(), "pull is valid");

        job.command = "fetch".into();
        assert!(job.validate().is_ok(), "fetch is valid");

        job.command = "push".into();
        assert!(job.validate().is_ok(), "push is valid");

        job.command = "rebase".into();
        assert!(job.validate().is_err(), "rebase is invalid");

        job.command = "".into();
        assert!(job.validate().is_err(), "empty is invalid");
    }

    #[test]
    fn runs_round_trip_through_toml() {
        let (dir, store) = temp_store();
        let job = a_job(&store, "@hourly");
        store.claim_due_jobs(job.created + 3600).unwrap();
        store.record_run(job.id, Some(0), 1234, None).unwrap();
        store
            .record_run(job.id, Some(1), 5678, Some("failed"))
            .unwrap();

        let reopened = Store::open(&dir.path().join("state.toml")).unwrap();
        let stored = reopened.job(job.id).unwrap().unwrap();
        assert_eq!(stored.runs.len(), 2);
        assert_eq!(stored.runs[0].exit, Some(0));
        assert_eq!(stored.runs[0].ms, 1234);
        assert_eq!(stored.runs[0].error, None);
        assert_eq!(stored.runs[1].exit, Some(1));
        assert_eq!(stored.runs[1].ms, 5678);
        assert_eq!(stored.runs[1].error.as_deref(), Some("failed"));
    }

    #[test]
    fn a_newer_version_is_refused_and_left_alone() {
        // The failure this prevents: an older build silently reading the
        // file as empty, then writing its empty self over a newer one.
        let (dir, _store) = temp_store();
        let path = dir.path().join("state.toml");
        let future = "version = 2\nnext_project = 7\n";
        std::fs::write(&path, future).unwrap();
        let Err(e) = Store::open(&path) else {
            panic!("a version this build cannot read must not open");
        };
        let msg = e.to_string();
        assert!(msg.contains("version 2"), "message: {msg}");
        assert!(msg.contains("newer"), "message: {msg}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), future);
    }

    #[test]
    fn a_file_without_a_version_still_loads() {
        let (dir, _store) = temp_store();
        let path = dir.path().join("state.toml");
        std::fs::write(&path, "next_project = 1\n").unwrap();
        assert!(Store::open(&path).unwrap().projects().is_ok());
    }
}
