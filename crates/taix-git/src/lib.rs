use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub enum Error {
    NotARepo,
    Git { stderr: String },
    Io(std::io::Error),
    InvalidUtf8,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotARepo => write!(f, "not a git repository"),
            Error::Git { stderr } => write!(f, "git error: {}", stderr),
            Error::Io(e) => write!(f, "io error: {}", e),
            Error::InvalidUtf8 => write!(f, "invalid utf-8 in git output"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub is_main: bool,
}

/// What one `git status --porcelain --branch` says: the checked-out branch
/// and how far the tree has drifted from it and from its upstream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub branch: String,
    pub dirty: usize,
    pub ahead: usize,
    pub behind: usize,
}

impl Status {
    /// `feature/bar ✱3 ↑1 ↓2`: the branch, then only the counts that are
    /// not zero. What a status bar shows for a checkout.
    pub fn line(&self) -> String {
        let mut line = self.branch.clone();
        for (mark, n) in [("✱", self.dirty), ("↑", self.ahead), ("↓", self.behind)] {
            if n > 0 {
                line.push_str(&format!(" {mark}{n}"));
            }
        }
        line
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

impl DiffStat {
    /// `3 files +12 -4`, or nothing for a tree that matches its base.
    pub fn line(&self) -> Option<String> {
        (self.files > 0).then(|| {
            format!(
                "{} file{} +{} -{}",
                self.files,
                if self.files == 1 { "" } else { "s" },
                self.insertions,
                self.deletions
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub index: char,
    pub work: char,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub conflicted: bool,
    pub renamed_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub summary: String,
    pub author: String,
    pub when: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub when: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    pub index: usize,
    pub message: String,
    pub branch: String,
    pub when: i64,
}

/// Everything a git panel displays, collected in as few spawns as possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: bool,
    pub remote: Option<String>,
    pub op: Option<Op>,
    pub changes: Vec<FileChange>,
    pub branches: Vec<Branch>,
    pub log: Vec<Commit>,
    pub stashes: Vec<Stash>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    Soft,
    Mixed,
    Hard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

/// A multi-step operation git is in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl Op {
    pub fn label(self) -> &'static str {
        match self {
            Op::Merge => "Merging",
            Op::Rebase => "Rebasing",
            Op::CherryPick => "Cherry-picking",
            Op::Revert => "Reverting",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Op::Merge => "merge",
            Op::Rebase => "rebase",
            Op::CherryPick => "cherry-pick",
            Op::Revert => "revert",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Abort,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFile {
    pub status: char,
    pub path: String,
}
pub fn is_repo(path: &Path) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output();
    matches!(output, Ok(o) if o.status.success())
}

pub fn status(path: &Path) -> Result<Status> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("status")
        .arg("--porcelain=v1")
        .arg("--branch")
        .output()?;
    if !output.status.success() {
        return Err(Error::NotARepo);
    }
    let stdout = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
    Ok(parse_status(stdout))
}

/// Collect everything a git panel shows in as few spawns as possible.
///
/// `git status --porcelain=v2 --branch` replaces current_branch + status +
/// changes (one spawn instead of three). Independent reads (remote, op,
/// branches, log, stashes) run sequentially. When called with a cached Repo,
/// git_dir and remote_url cost zero spawns on subsequent calls.
pub fn panel(repo: &Repo, root: &Path) -> Result<Panel> {
    let status_output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("status")
        .arg("--porcelain=v2")
        .arg("--branch")
        .arg("--untracked-files=all")
        .output()?;

    if !status_output.status.success() {
        let stderr = String::from_utf8_lossy(&status_output.stderr)
            .trim()
            .to_string();
        return Err(Error::Git { stderr });
    }

    let status_raw = std::str::from_utf8(&status_output.stdout).map_err(|_| Error::InvalidUtf8)?;
    let status = parse_status_v2(status_raw)?;

    let remote = repo.remote_url(root).ok().flatten();
    let op = repo.in_progress(root).ok().flatten();
    let branches = repo.branches(root).unwrap_or_default();
    let log = repo.log(root, 100).unwrap_or_default();
    let stashes = repo.stash_list(root).unwrap_or_default();

    Ok(Panel {
        branch: status.branch,
        upstream: status.upstream,
        ahead: status.ahead,
        behind: status.behind,
        dirty: !status.changes.is_empty(),
        remote,
        op,
        changes: status.changes,
        branches,
        log,
        stashes,
    })
}

/// The directory a project should be rooted at.
///
/// The git top level when `path` is inside a repository, so `taix add .` from
/// a subdirectory registers the whole repo — otherwise `path` itself.
///
/// A project used to require a repository, which meant adding a plain folder
/// failed and nothing appeared in the sidebar. Git is now only needed for
/// worktree isolation.
pub fn project_root(path: &Path) -> std::io::Result<PathBuf> {
    if let Ok(repo) = Repo::open(path) {
        return Ok(repo.root);
    }
    let root = std::fs::canonicalize(path)?;
    if !root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    Ok(root)
}

pub struct Repo {
    pub root: PathBuf,
    /// `default_branch` costs up to four `git` processes and does not change
    /// while a session runs, so it is asked once per opened repository.
    base: std::sync::OnceLock<String>,
    /// The .git directory never moves while the repo exists.
    git_dir: std::sync::OnceLock<PathBuf>,
    /// A remote URL changes about once a year; a `git config` read per
    /// panel refresh is four seconds of nothing.
    remote_url: std::sync::OnceLock<Option<String>>,
}

impl Repo {
    pub fn open(path: &Path) -> Result<Repo> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("rev-parse")
            .arg("--show-toplevel")
            .output()?;

        if !output.status.success() {
            return Err(Error::NotARepo);
        }

        let root_str = std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim();
        let root = std::fs::canonicalize(root_str)?;
        Ok(Repo {
            root,
            base: std::sync::OnceLock::new(),
            git_dir: std::sync::OnceLock::new(),
            remote_url: std::sync::OnceLock::new(),
        })
    }

    /// The .git directory for this repo, cached after first lookup.
    fn git_dir_path(&self, path: &Path) -> Result<&PathBuf> {
        if let Some(dir) = self.git_dir.get() {
            return Ok(dir);
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("rev-parse")
            .arg("--git-dir")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let git_dir_str = std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim();
        let git_dir = if git_dir_str == ".git" || !git_dir_str.starts_with('/') {
            path.join(git_dir_str)
        } else {
            PathBuf::from(git_dir_str)
        };
        Ok(self.git_dir.get_or_init(|| git_dir))
    }

    /// Run git with args in the given checkout, returning stdout trimmed.
    fn git(&self, path: &Path, args: &[&str]) -> Result<String> {
        use std::ffi::OsStr;
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path);
        for arg in args {
            cmd.arg(OsStr::new(arg));
        }
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    /// The checked-out branch, including one with no commits on it yet.
    ///
    /// `git branch --show-current`, not `rev-parse --abbrev-ref HEAD`: on an
    /// unborn branch - a repository someone has just `git init`ed, which is
    /// exactly when a source-control panel is first opened - `rev-parse`
    /// fails and the panel showed a nameless checkout. This prints `master`
    /// and prints nothing at all on a detached HEAD, where there is no
    /// branch to name.
    pub fn current_branch(&self) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("branch")
            .arg("--show-current")
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    /// The branch worktrees are cut from and merged back into: `origin/HEAD`
    /// when there is a remote, else `main`, else `master`, else whatever is
    /// checked out. Remembered for the life of the `Repo`.
    pub fn default_branch(&self) -> Result<String> {
        if let Some(base) = self.base.get() {
            return Ok(base.clone());
        }
        let base = self.find_default_branch()?;
        Ok(self.base.get_or_init(|| base).clone())
    }

    fn find_default_branch(&self) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("symbolic-ref")
            .arg("refs/remotes/origin/HEAD")
            .output()?;

        if output.status.success() {
            let full_ref = std::str::from_utf8(&output.stdout)
                .map_err(|_| Error::InvalidUtf8)?
                .trim();
            if let Some(branch) = full_ref.strip_prefix("refs/remotes/origin/") {
                return Ok(branch.to_string());
            }
        }

        if self.branch_exists("main")? {
            return Ok("main".to_string());
        }
        if self.branch_exists("master")? {
            return Ok("master".to_string());
        }
        self.current_branch()
    }

    /// What a worktree window's header says: its branch, and its diff
    /// against the base when there is one. Two `git` processes, both
    /// against the worktree, never the main checkout.
    pub fn worktree_line(&self, worktree: &Path) -> Result<String> {
        let branch = self.branch_at(worktree)?;
        let base = self.default_branch()?;
        Ok(match self.diff_stat(worktree, &base)?.line() {
            Some(diff) => format!("{branch} · {diff}"),
            None => branch,
        })
    }

    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("worktree")
            .arg("list")
            .arg("--porcelain")
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        let stdout = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        Ok(parse_worktrees(stdout))
    }

    pub fn add_worktree(&self, slug: &str, base: &str) -> Result<Worktree> {
        let repo_name = self
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid repo root",
                ))
            })?;

        let worktree_parent = self.root.parent().ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "repo has no parent",
            ))
        })?;

        let worktree_path = worktree_parent
            .join(".taix-worktrees")
            .join(repo_name)
            .join(slug);

        // Check if worktree already exists
        let existing = self.worktrees()?;
        if let Some(wt) = existing.iter().find(|w| w.path == worktree_path) {
            return Ok(wt.clone());
        }

        let branch = format!("taix/{}", slug);

        // Create parent directories
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Check if branch exists
        let branch_exists = self.branch_exists(&branch)?;

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.root).arg("worktree").arg("add");

        if !branch_exists {
            cmd.arg("-b").arg(&branch);
        }

        cmd.arg(&worktree_path);

        if branch_exists {
            cmd.arg(&branch);
        } else {
            cmd.arg(base);
        }

        let output = cmd.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        Ok(Worktree {
            path: worktree_path,
            branch,
            is_main: false,
        })
    }

    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        // Verify it's actually a worktree
        let worktrees = self.worktrees()?;
        if !worktrees.iter().any(|w| w.path == path) {
            return Err(Error::Git {
                stderr: format!("{} is not a worktree", path.display()),
            });
        }

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.root).arg("worktree").arg("remove");

        if force {
            cmd.arg("--force");
        }

        cmd.arg(path);

        let output = cmd.output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        // Prune after successful removal
        let prune_output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("worktree")
            .arg("prune")
            .output()?;

        if !prune_output.status.success() {
            let stderr = String::from_utf8_lossy(&prune_output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        Ok(())
    }

    fn branch_exists(&self, branch: &str) -> Result<bool> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("rev-parse")
            .arg("--verify")
            .arg("--quiet")
            .arg(branch)
            .output()?;

        Ok(output.status.success())
    }

    /// The branch checked out in `path`, which may be a worktree rather than
    /// the main checkout.
    pub fn branch_at(&self, path: &Path) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("rev-parse")
            .arg("--abbrev-ref")
            .arg("HEAD")
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    /// Committed and uncommitted change against `base`, summarised.
    ///
    /// Runs `git diff --numstat <base>` from `path` to capture both committed
    /// work on the branch and uncommitted work in the tree — `<base>...HEAD`
    /// would miss uncommitted changes.
    pub fn diff_stat(&self, path: &Path, base: &str) -> Result<DiffStat> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("diff")
            .arg("--numstat")
            .arg(base)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        let stdout = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;

        let mut files = 0;
        let mut insertions = 0;
        let mut deletions = 0;

        for line in stdout.lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                files += 1;
                // Binary files show "-" for adds/dels, so a failed parse is
                // expected rather than an error.
                insertions += parts[0].parse::<usize>().unwrap_or(0);
                deletions += parts[1].parse::<usize>().unwrap_or(0);
            }
        }

        Ok(DiffStat {
            files,
            insertions,
            deletions,
        })
    }

    /// Unified diff against `base`, truncated to `max_bytes` on a line
    /// boundary so a huge diff cannot blow up the caller.
    ///
    /// Like `diff_stat`, uses `git diff <base>` to capture both committed and
    /// uncommitted changes. Returned string is `<= max_bytes`.
    pub fn diff_text(&self, path: &Path, base: &str, max_bytes: usize) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("diff")
            .arg(base)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        let full = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;

        if full.len() <= max_bytes {
            return Ok(full.to_string());
        }

        // Find the last newline before max_bytes
        let truncated = &full.as_bytes()[..max_bytes];
        if let Some(last_newline) = truncated.iter().rposition(|&b| b == b'\n') {
            // Safe: we validated full string is UTF-8, newline is a boundary
            Ok(full[..=last_newline].to_string())
        } else {
            // No newline in first max_bytes
            Ok(String::new())
        }
    }

    /// Merge the branch checked out in `worktree` into `base`, in the main
    /// checkout, without fast-forward.
    ///
    /// Errors if `worktree` is not a worktree of this repo, has uncommitted
    /// changes, has no commits beyond `base`, or if the main checkout is dirty.
    /// On merge conflict, returns `Error::Git` with git's stderr and leaves the
    /// repository as git left it.
    pub fn merge_back(&self, worktree: &Path, base: &str) -> Result<()> {
        // Verify it's a worktree of this repo
        let worktrees = self.worktrees()?;
        let wt = worktrees
            .iter()
            .find(|w| w.path == worktree)
            .ok_or_else(|| Error::Git {
                stderr: format!(
                    "{} is not a worktree of this repository",
                    worktree.display()
                ),
            })?;

        if wt.is_main {
            return Err(Error::Git {
                stderr: "cannot merge the main checkout into itself".to_string(),
            });
        }

        // Check worktree is clean
        let wt_status = status(worktree)?;
        if wt_status.dirty > 0 {
            return Err(Error::Git {
                stderr: format!(
                    "worktree {} has {} uncommitted change(s); commit first",
                    worktree.display(),
                    wt_status.dirty
                ),
            });
        }

        // Check main checkout is clean
        let main_status = status(&self.root)?;
        if main_status.dirty > 0 {
            return Err(Error::Git {
                stderr: format!(
                    "main checkout has {} uncommitted change(s); commit or stash first",
                    main_status.dirty
                ),
            });
        }

        // Check the worktree branch has commits beyond base
        let merge_base_output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("merge-base")
            .arg(base)
            .arg(&wt.branch)
            .output()?;

        if !merge_base_output.status.success() {
            let stderr = String::from_utf8_lossy(&merge_base_output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        let merge_base = std::str::from_utf8(&merge_base_output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim();

        // Get the commit hash of the worktree branch
        let wt_commit_output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("rev-parse")
            .arg(&wt.branch)
            .output()?;

        if !wt_commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&wt_commit_output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        let wt_commit = std::str::from_utf8(&wt_commit_output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim();

        if merge_base == wt_commit {
            return Err(Error::Git {
                stderr: format!("branch {} has no commits beyond {}", wt.branch, base),
            });
        }

        // Switch to base branch in main checkout
        let checkout_output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("checkout")
            .arg(base)
            .output()?;

        if !checkout_output.status.success() {
            let stderr = String::from_utf8_lossy(&checkout_output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        // Merge with --no-ff
        let merge_output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("merge")
            .arg("--no-ff")
            .arg(&wt.branch)
            .arg("-m")
            .arg(format!("Merge {}", wt.branch))
            .output()?;

        if !merge_output.status.success() {
            let stderr = String::from_utf8_lossy(&merge_output.stderr).to_string();
            return Err(Error::Git { stderr });
        }

        Ok(())
    }

    pub fn changes(&self, path: &Path) -> Result<Vec<FileChange>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("status")
            .arg("--porcelain=v1")
            .arg("-z")
            .arg("--untracked-files=all")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut changes = Vec::new();
        let mut parts = raw.split('\0').filter(|s| !s.is_empty());
        while let Some(entry) = parts.next() {
            if entry.len() < 3 {
                continue;
            }
            let index = entry.chars().nth(0).unwrap_or(' ');
            let work = entry.chars().nth(1).unwrap_or(' ');
            let file = &entry[3..];
            let renamed_from = if index == 'R' || work == 'R' {
                parts.next().map(String::from)
            } else {
                None
            };
            let conflicted = index == 'U'
                || work == 'U'
                || (index == 'A' && work == 'A')
                || (index == 'D' && work == 'D');
            let staged = index != ' ' && index != '?';
            let unstaged = work != ' ' && work != '?';
            let untracked = index == '?' && work == '?';
            changes.push(FileChange {
                path: file.to_string(),
                index,
                work,
                staged,
                unstaged,
                untracked,
                conflicted,
                renamed_from,
            });
        }
        changes.sort_by(|a, b| {
            if a.conflicted != b.conflicted {
                return b.conflicted.cmp(&a.conflicted);
            }
            if a.staged != b.staged {
                return b.staged.cmp(&a.staged);
            }
            if a.unstaged != b.unstaged {
                return b.unstaged.cmp(&a.unstaged);
            }
            if a.untracked != b.untracked {
                return b.untracked.cmp(&a.untracked);
            }
            a.path.cmp(&b.path)
        });
        Ok(changes)
    }

    pub fn stage(&self, path: &Path, files: &[String]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg("add").arg("--");
        for file in files {
            cmd.arg(file);
        }
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn stage_all(&self, path: &Path) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("add")
            .arg("-A")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn unstage(&self, path: &Path, files: &[String]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        // Try restore --staged first
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(path)
            .arg("restore")
            .arg("--staged")
            .arg("--");
        for file in files {
            cmd.arg(file);
        }
        let output = cmd.output()?;
        if output.status.success() {
            return Ok(());
        }
        // Fallback to reset when no HEAD
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(path)
            .arg("reset")
            .arg("-q")
            .arg("HEAD")
            .arg("--");
        for file in files {
            cmd.arg(file);
        }
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn discard(&self, path: &Path, files: &[String]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        for file in files {
            let file_path = path.join(file);
            if !file_path.exists() {
                continue;
            }
            // Check if tracked
            let check = Command::new("git")
                .arg("-C")
                .arg(path)
                .arg("ls-files")
                .arg("--")
                .arg(file)
                .output()?;
            let is_tracked = check.status.success()
                && !std::str::from_utf8(&check.stdout)
                    .unwrap_or("")
                    .trim()
                    .is_empty();
            if is_tracked {
                // Restore from HEAD/index
                let mut cmd = Command::new("git");
                cmd.arg("-C").arg(path).arg("restore").arg("--").arg(file);
                let output = cmd.output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    return Err(Error::Git { stderr });
                }
            } else {
                // Untracked - delete it
                if file_path.is_file() {
                    std::fs::remove_file(&file_path)?;
                } else if file_path.is_dir() {
                    std::fs::remove_dir_all(&file_path)?;
                }
            }
        }
        Ok(())
    }

    pub fn commit(&self, path: &Path, message: &str, amend: bool) -> Result<String> {
        if message.trim().is_empty() {
            return Err(Error::Git {
                stderr: "empty commit message".to_string(),
            });
        }
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg("commit").arg("-m").arg(message);
        if amend {
            cmd.arg("--amend");
        }
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        // Get the new commit hash
        let args = ["rev-parse", "--short", "HEAD"];
        self.git(path, &args)
    }

    pub fn log(&self, path: &Path, limit: usize) -> Result<Vec<Commit>> {
        let format = "%H%x1f%h%x1f%s%x1f%an%x1f%at";
        let limit_str = format!("-{}", limit);
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("log")
            .arg("-z")
            .arg(&limit_str)
            .arg(format!("--format={}", format))
            .output()?;
        // Empty repo is ok
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not have any commits")
                || stderr.contains("bad default revision")
            {
                return Ok(vec![]);
            }
            return Err(Error::Git {
                stderr: stderr.trim().to_string(),
            });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut commits = Vec::new();
        for record in raw.split('\0').filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = record.split('\x1f').collect();
            if parts.len() >= 5 {
                commits.push(Commit {
                    hash: parts[0].to_string(),
                    short: parts[1].to_string(),
                    summary: parts[2].to_string(),
                    author: parts[3].to_string(),
                    when: parts[4].parse().unwrap_or(0),
                });
            }
        }
        Ok(commits)
    }

    pub fn commits_between(&self, path: &Path, base: &str, limit: usize) -> Result<Vec<Commit>> {
        let format = "%H%x1f%h%x1f%s%x1f%an%x1f%at";
        let limit_str = format!("-{}", limit);
        let range = format!("{}..HEAD", base);
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("log")
            .arg("-z")
            .arg(&limit_str)
            .arg(format!("--format={}", format))
            .arg(&range)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut commits = Vec::new();
        for record in raw.split('\0').filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = record.split('\x1f').collect();
            if parts.len() >= 5 {
                commits.push(Commit {
                    hash: parts[0].to_string(),
                    short: parts[1].to_string(),
                    summary: parts[2].to_string(),
                    author: parts[3].to_string(),
                    when: parts[4].parse().unwrap_or(0),
                });
            }
        }
        Ok(commits)
    }

    pub fn branches(&self, path: &Path) -> Result<Vec<Branch>> {
        let format = "%(refname:short)%00%(HEAD)%00%(upstream:short)%00%(upstream:track)%00%(committerdate:unix)";
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("for-each-ref")
            .arg("refs/heads")
            .arg(format!("--format={}", format))
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut branches = Vec::new();
        for line in raw.lines() {
            let parts: Vec<&str> = line.split('\0').collect();
            if parts.len() >= 5 {
                let name = parts[0].to_string();
                let current = parts[1] == "*";
                let upstream = if parts[2].is_empty() {
                    None
                } else {
                    Some(parts[2].to_string())
                };
                let track = parts[3];
                let mut ahead = 0;
                let mut behind = 0;
                // Parse [ahead N, behind M] or [ahead N] or [behind M]
                for part in track.split(',') {
                    let part = part.trim();
                    if let Some(n) = part.strip_prefix("ahead ") {
                        ahead = n.trim_end_matches(']').parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("[ahead ") {
                        ahead = n.trim_end_matches(']').parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("behind ") {
                        behind = n.trim_end_matches(']').parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("[behind ") {
                        behind = n.trim_end_matches(']').parse().unwrap_or(0);
                    }
                }
                let when = parts[4].parse().unwrap_or(0);
                branches.push(Branch {
                    name,
                    current,
                    upstream,
                    ahead,
                    behind,
                    when,
                });
            }
        }
        branches.sort_by(|a, b| {
            if a.current != b.current {
                return b.current.cmp(&a.current);
            }
            b.when.cmp(&a.when)
        });
        Ok(branches)
    }

    pub fn checkout(&self, path: &Path, branch: &str, create: bool) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg("checkout");
        if create {
            cmd.arg("-b");
        }
        cmd.arg(branch);
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn fetch(&self, path: &Path, prune: bool) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg("fetch");
        if prune {
            cmd.arg("--prune");
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.contains("no remote")
                || stderr.contains("does not appear to be a git repository")
            {
                return Err(Error::Git {
                    stderr: "no remote configured".to_string(),
                });
            }
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn pull(&self, path: &Path) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("pull")
            .arg("--ff-only")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.contains("no remote")
                || stderr.contains("does not appear to be a git repository")
            {
                return Err(Error::Git {
                    stderr: "no remote configured".to_string(),
                });
            }
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    pub fn push(&self, path: &Path, set_upstream: bool, force: bool) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg("push");
        if set_upstream {
            cmd.arg("--set-upstream").arg("origin").arg("HEAD");
        }
        if force {
            cmd.arg("--force-with-lease");
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.contains("no remote")
                || stderr.contains("does not appear to be a git repository")
            {
                return Err(Error::Git {
                    stderr: "no remote configured".to_string(),
                });
            }
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stderr)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    pub fn stash_push(&self, path: &Path, message: &str) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(path)
            .arg("stash")
            .arg("push")
            .arg("--include-untracked");
        if !message.is_empty() {
            cmd.arg("-m").arg(message);
        }
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn stash_pop(&self, path: &Path, index: usize) -> Result<()> {
        let stash_ref = format!("stash@{{{}}}", index);
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("stash")
            .arg("pop")
            .arg(&stash_ref)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn file_diff(
        &self,
        path: &Path,
        file: &str,
        staged: bool,
        max_bytes: usize,
    ) -> Result<String> {
        // Check if file is untracked
        let check = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("ls-files")
            .arg("--")
            .arg(file)
            .output()?;
        let is_tracked = check.status.success()
            && !std::str::from_utf8(&check.stdout)
                .unwrap_or("")
                .trim()
                .is_empty();

        let output = if !is_tracked && !staged {
            // Untracked file - show diff against /dev/null
            Command::new("git")
                .arg("-C")
                .arg(path)
                .arg("diff")
                .arg("--no-index")
                .arg("/dev/null")
                .arg(file)
                .output()?
        } else {
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(path).arg("diff");
            if staged {
                cmd.arg("--cached");
            }
            cmd.arg("--").arg(file);
            cmd.output()?
        };
        // diff --no-index returns exit code 1 for differences, which is expected
        let full = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        if full.len() <= max_bytes {
            return Ok(full.to_string());
        }
        // Truncate on line boundary
        let truncated = &full.as_bytes()[..max_bytes];
        if let Some(last_newline) = truncated.iter().rposition(|&b| b == b'\n') {
            Ok(full[..=last_newline].to_string())
        } else {
            Ok(String::new())
        }
    }

    pub fn commit_diff(&self, path: &Path, hash: &str, max_bytes: usize) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("show")
            .arg(hash)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let full = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        if full.len() <= max_bytes {
            return Ok(full.to_string());
        }
        // Truncate on line boundary
        let truncated = &full.as_bytes()[..max_bytes];
        if let Some(last_newline) = truncated.iter().rposition(|&b| b == b'\n') {
            Ok(full[..=last_newline].to_string())
        } else {
            Ok(String::new())
        }
    }

    pub fn remote_url(&self, path: &Path) -> Result<Option<String>> {
        if let Some(url) = self.remote_url.get() {
            return Ok(url.clone());
        }
        // Try origin first
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("remote")
            .arg("get-url")
            .arg("origin")
            .output()?;
        if output.status.success() {
            let url = std::str::from_utf8(&output.stdout)
                .map_err(|_| Error::InvalidUtf8)?
                .trim()
                .to_string();
            if !url.is_empty() {
                self.remote_url.set(Some(url.clone())).ok();
                return Ok(Some(url));
            }
        }
        // Try first remote
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("remote")
            .output()?;
        if !output.status.success() {
            self.remote_url.set(None).ok();
            return Ok(None);
        }
        let remotes = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        if let Some(first) = remotes.lines().next() {
            let output = Command::new("git")
                .arg("-C")
                .arg(path)
                .arg("remote")
                .arg("get-url")
                .arg(first.trim())
                .output()?;
            if output.status.success() {
                let url = std::str::from_utf8(&output.stdout)
                    .map_err(|_| Error::InvalidUtf8)?
                    .trim()
                    .to_string();
                self.remote_url.set(Some(url.clone())).ok();
                return Ok(Some(url));
            }
        }
        self.remote_url.set(None).ok();
        Ok(None)
    }

    pub fn merge(&self, path: &Path, branch: &str, no_ff: bool) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg("merge").arg("--no-edit");
        if no_ff {
            cmd.arg("--no-ff");
        }
        cmd.arg(branch);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    pub fn rebase(&self, path: &Path, onto: &str) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("rebase")
            .arg(onto)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    pub fn in_progress(&self, path: &Path) -> Result<Option<Op>> {
        let git_dir = self.git_dir_path(path)?;

        if git_dir.join("MERGE_HEAD").exists() {
            return Ok(Some(Op::Merge));
        }
        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            return Ok(Some(Op::Rebase));
        }
        if git_dir.join("CHERRY_PICK_HEAD").exists() {
            return Ok(Some(Op::CherryPick));
        }
        if git_dir.join("REVERT_HEAD").exists() {
            return Ok(Some(Op::Revert));
        }
        Ok(None)
    }

    pub fn step(&self, path: &Path, step: Step) -> Result<String> {
        let op = self.in_progress(path)?;
        if op.is_none() {
            return Err(Error::Git {
                stderr: "no operation in progress".to_string(),
            });
        }

        let op = op.unwrap();
        let arg = match step {
            Step::Continue => "--continue",
            Step::Abort => "--abort",
            Step::Skip => "--skip",
        };

        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg(op.word()).arg(arg);
        // `git merge --continue` refuses every other argument, including
        // `--no-edit`, so the editor is silenced through the environment -
        // which every one of these subcommands honours.
        if step == Step::Continue {
            cmd.env("GIT_EDITOR", "true");
        }

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    pub fn resolve(&self, path: &Path, file: &str, side: Side) -> Result<()> {
        let side_arg = match side {
            Side::Ours => "--ours",
            Side::Theirs => "--theirs",
        };
        let checkout_output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("checkout")
            .arg(side_arg)
            .arg("--")
            .arg(file)
            .output()?;
        if !checkout_output.status.success() {
            let stderr = String::from_utf8_lossy(&checkout_output.stderr)
                .trim()
                .to_string();
            return Err(Error::Git { stderr });
        }

        let add_output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("add")
            .arg("--")
            .arg(file)
            .output()?;
        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr)
                .trim()
                .to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn delete_branch(&self, path: &Path, name: &str, force: bool) -> Result<()> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path).arg("branch");
        if force {
            cmd.arg("-D");
        } else {
            cmd.arg("-d");
        }
        cmd.arg(name);
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn rename_branch(&self, path: &Path, old: &str, new: &str) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("branch")
            .arg("-m")
            .arg(old)
            .arg(new)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn remote_branches(&self, path: &Path) -> Result<Vec<Branch>> {
        let format = "%09%1f%(refname:short)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(committerdate:unix)";
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("for-each-ref")
            .arg("--sort=-committerdate")
            .arg(format!("--format={}", format))
            .arg("refs/remotes")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut branches = Vec::new();
        for line in raw.lines() {
            let parts: Vec<&str> = line.split('\x1f').collect();
            if parts.len() >= 5 {
                let name = parts[1].to_string();
                if name.ends_with("/HEAD") {
                    continue;
                }
                let upstream = if parts[2].is_empty() {
                    None
                } else {
                    Some(parts[2].to_string())
                };
                let when = parts[4].parse().unwrap_or(0);
                branches.push(Branch {
                    name,
                    current: false,
                    upstream,
                    ahead: 0,
                    behind: 0,
                    when,
                });
            }
        }
        Ok(branches)
    }

    pub fn revert(&self, path: &Path, hash: &str) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("revert")
            .arg("--no-edit")
            .arg(hash)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    pub fn cherry_pick(&self, path: &Path, hash: &str) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("cherry-pick")
            .arg(hash)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim()
            .to_string())
    }

    pub fn reset(&self, path: &Path, hash: &str, mode: Reset) -> Result<()> {
        let mode_arg = match mode {
            Reset::Soft => "--soft",
            Reset::Mixed => "--mixed",
            Reset::Hard => "--hard",
        };
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("reset")
            .arg(mode_arg)
            .arg(hash)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn commit_files(&self, path: &Path, hash: &str) -> Result<Vec<CommitFile>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("show")
            .arg("--name-status")
            .arg("--format=")
            .arg("-m")
            .arg("--first-parent")
            .arg(hash)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut files = Vec::new();
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 2 {
                let status = parts[0].chars().next().unwrap_or(' ');
                let path = parts[1].to_string();
                files.push(CommitFile { status, path });
            }
        }
        Ok(files)
    }

    pub fn last_message(&self, path: &Path) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("log")
            .arg("-1")
            .arg("--format=%B")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not have any commits")
                || stderr.contains("bad default revision")
            {
                return Ok(String::new());
            }
            return Err(Error::Git {
                stderr: stderr.trim().to_string(),
            });
        }
        Ok(std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::InvalidUtf8)?
            .trim_end()
            .to_string())
    }

    pub fn stash_list(&self, path: &Path) -> Result<Vec<Stash>> {
        let format = "%gd%x1f%gs%x1f%at";
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("stash")
            .arg("list")
            .arg(format!("--format={}", format))
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut stashes = Vec::new();
        for line in raw.lines() {
            let parts: Vec<&str> = line.split('\x1f').collect();
            if parts.len() >= 3 {
                let gd = parts[0];
                let index = if let Some(stripped) = gd.strip_prefix("stash@{") {
                    stripped
                        .strip_suffix('}')
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0)
                } else {
                    0
                };
                let message = parts[1].to_string();
                let branch = if let Some(start) = message.find("on ") {
                    let after_on = &message[start + 3..];
                    if let Some(colon_pos) = after_on.find(':') {
                        after_on[..colon_pos].trim().to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                let when = parts[2].parse().unwrap_or(0);
                stashes.push(Stash {
                    index,
                    message,
                    branch,
                    when,
                });
            }
        }
        Ok(stashes)
    }

    pub fn stash_apply(&self, path: &Path, index: usize) -> Result<()> {
        let stash_ref = format!("stash@{{{}}}", index);
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("stash")
            .arg("apply")
            .arg(&stash_ref)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn stash_drop(&self, path: &Path, index: usize) -> Result<()> {
        let stash_ref = format!("stash@{{{}}}", index);
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("stash")
            .arg("drop")
            .arg(&stash_ref)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn discard_all(&self, path: &Path, untracked: bool) -> Result<()> {
        let reset_output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("reset")
            .arg("--hard")
            .arg("HEAD")
            .output()?;
        if !reset_output.status.success() {
            let stderr = String::from_utf8_lossy(&reset_output.stderr)
                .trim()
                .to_string();
            return Err(Error::Git { stderr });
        }

        if untracked {
            let clean_output = Command::new("git")
                .arg("-C")
                .arg(path)
                .arg("clean")
                .arg("-fd")
                .output()?;
            if !clean_output.status.success() {
                let stderr = String::from_utf8_lossy(&clean_output.stderr)
                    .trim()
                    .to_string();
                return Err(Error::Git { stderr });
            }
        }
        Ok(())
    }

    pub fn unstage_all(&self, path: &Path) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("reset")
            .arg("HEAD")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Git { stderr });
        }
        Ok(())
    }

    pub fn ignore(&self, path: &Path, pattern: &str) -> Result<()> {
        let gitignore_path = path.join(".gitignore");
        let existing = if gitignore_path.exists() {
            std::fs::read_to_string(&gitignore_path)?
        } else {
            String::new()
        };

        for line in existing.lines() {
            if line.trim() == pattern {
                return Ok(());
            }
        }

        let mut content = existing;
        let needs_leading_newline = !content.is_empty() && !content.ends_with('\n');
        if needs_leading_newline {
            content.push('\n');
        }
        content.push_str(pattern);
        content.push('\n');

        std::fs::write(&gitignore_path, content)?;
        Ok(())
    }

    pub fn file_log(&self, path: &Path, file: &str, limit: usize) -> Result<Vec<Commit>> {
        let format = "%H%x1f%h%x1f%s%x1f%an%x1f%at";
        let limit_str = format!("-{}", limit);
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("log")
            .arg("-z")
            .arg(&limit_str)
            .arg(format!("--format={}", format))
            .arg("--")
            .arg(file)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not have any commits")
                || stderr.contains("bad default revision")
            {
                return Ok(vec![]);
            }
            return Err(Error::Git {
                stderr: stderr.trim().to_string(),
            });
        }
        let raw = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
        let mut commits = Vec::new();
        for record in raw.split('\0').filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = record.split('\x1f').collect();
            if parts.len() >= 5 {
                commits.push(Commit {
                    hash: parts[0].to_string(),
                    short: parts[1].to_string(),
                    summary: parts[2].to_string(),
                    author: parts[3].to_string(),
                    when: parts[4].parse().unwrap_or(0),
                });
            }
        }
        Ok(commits)
    }

    pub fn commit_file_diff(
        &self,
        path: &Path,
        hash: &str,
        file: &str,
        max_bytes: usize,
    ) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("show")
            .arg(format!("{}:{}", hash, file))
            .output()?;
        if !output.status.success() {
            let output = Command::new("git")
                .arg("-C")
                .arg(path)
                .arg("diff")
                .arg(format!("{}^", hash))
                .arg(hash)
                .arg("--")
                .arg(file)
                .output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(Error::Git { stderr });
            }
            let full = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
            if full.len() <= max_bytes {
                return Ok(full.to_string());
            }
            let truncated = &full.as_bytes()[..max_bytes];
            if let Some(last_newline) = truncated.iter().rposition(|&b| b == b'\n') {
                Ok(full[..=last_newline].to_string())
            } else {
                Ok(String::new())
            }
        } else {
            let output = Command::new("git")
                .arg("-C")
                .arg(path)
                .arg("diff")
                .arg(format!("{}^", hash))
                .arg(hash)
                .arg("--")
                .arg(file)
                .output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(Error::Git { stderr });
            }
            let full = std::str::from_utf8(&output.stdout).map_err(|_| Error::InvalidUtf8)?;
            if full.len() <= max_bytes {
                return Ok(full.to_string());
            }
            let truncated = &full.as_bytes()[..max_bytes];
            if let Some(last_newline) = truncated.iter().rposition(|&b| b == b'\n') {
                Ok(full[..=last_newline].to_string())
            } else {
                Ok(String::new())
            }
        }
    }
}

pub fn slug(name: &str) -> String {
    let mut result = String::new();
    let mut last_was_dash = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            result.push('-');
            last_was_dash = true;
        }
    }

    let trimmed = result.trim_matches('-');
    if trimmed.is_empty() {
        "agent".to_string()
    } else {
        trimmed.to_string()
    }
}

fn parse_worktrees(porcelain: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let is_first = |idx: usize| idx == 0;

    for (record_idx, record) in porcelain.split("\n\n").enumerate() {
        if record.trim().is_empty() {
            continue;
        }

        let mut current_path = None;
        let mut current_branch = None;

        for line in record.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                current_path = Some(PathBuf::from(path));
            } else if let Some(branch_ref) = line.strip_prefix("branch ") {
                if let Some(name) = branch_ref.strip_prefix("refs/heads/") {
                    current_branch = Some(name.to_string());
                }
            } else if line == "detached" {
                current_branch = Some("(detached)".to_string());
            }
        }

        if let (Some(path), Some(branch)) = (current_path, current_branch) {
            worktrees.push(Worktree {
                path,
                branch,
                is_main: is_first(record_idx),
            });
        }
    }

    worktrees
}

fn parse_status(porcelain: &str) -> Status {
    let mut status = Status::default();
    for line in porcelain.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            // `main...origin/main [ahead 1, behind 2]`, `main`, `HEAD (no
            // branch)` for a detached head, `No commits yet on main`.
            let head = header.split(" [").next().unwrap_or(header);
            let head = head.strip_prefix("No commits yet on ").unwrap_or(head);
            status.branch = head
                .split("...")
                .next()
                .unwrap_or(head)
                .split(' ')
                .next()
                .unwrap_or_default()
                .to_string();
            if let Some(inside) = header.split_once('[').and_then(|(_, r)| r.split_once(']')) {
                for part in inside.0.split(',') {
                    let part = part.trim();
                    if let Some(n) = part.strip_prefix("ahead ") {
                        status.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("behind ") {
                        status.behind = n.parse().unwrap_or(0);
                    }
                }
            }
        } else if !line.is_empty() {
            status.dirty += 1;
        }
    }
    status
}

/// What `git status --porcelain=v2 --branch` answers in one invocation:
/// the branch, its upstream drift and every change. Asking for those
/// separately is three `git` processes.
struct StatusV2 {
    branch: String,
    upstream: Option<String>,
    ahead: usize,
    behind: usize,
    changes: Vec<FileChange>,
}

fn parse_status_v2(porcelain: &str) -> Result<StatusV2> {
    let mut branch = String::new();
    let mut upstream: Option<String> = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut changes = Vec::new();

    for line in porcelain.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = if rest == "(detached)" {
                String::new()
            } else {
                rest.to_string()
            };
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 2 {
                ahead = parts[0].trim_start_matches('+').parse().unwrap_or(0);
                behind = parts[1].trim_start_matches('-').parse().unwrap_or(0);
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            changes.push(FileChange {
                path: path.to_string(),
                index: '?',
                work: '?',
                staged: false,
                unstaged: false,
                untracked: true,
                conflicted: false,
                renamed_from: None,
            });
        } else if let Some(rest) = line.strip_prefix("1 ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 8 {
                let xy = parts[0];
                let index = xy.chars().next().unwrap_or(' ');
                let work = xy.chars().nth(1).unwrap_or(' ');
                let path = parts[7..].join(" ");
                let conflicted = index == 'U'
                    || work == 'U'
                    || (index == 'A' && work == 'A')
                    || (index == 'D' && work == 'D');
                let staged = index != '.' && index != ' ';
                let unstaged = work != '.' && work != ' ';
                changes.push(FileChange {
                    path,
                    index,
                    work,
                    staged,
                    unstaged,
                    untracked: false,
                    conflicted,
                    renamed_from: None,
                });
            }
        } else if let Some(rest) = line.strip_prefix("2 ") {
            let mut field_start = 0;
            let mut field_count = 0;
            for (i, ch) in rest.char_indices() {
                if ch == ' ' || ch == '\t' {
                    field_count += 1;
                    if field_count == 8 {
                        field_start = i + 1;
                        break;
                    }
                }
            }
            if field_count >= 8 && field_start < rest.len() {
                let path_part = &rest[field_start..];
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() >= 9 {
                    let xy = parts[0];
                    let index = xy.chars().next().unwrap_or(' ');
                    let work = xy.chars().nth(1).unwrap_or(' ');
                    if let Some((new_path, old_path)) = path_part.split_once('\t') {
                        let conflicted = index == 'U'
                            || work == 'U'
                            || (index == 'A' && work == 'A')
                            || (index == 'D' && work == 'D');
                        let staged = index != '.' && index != ' ';
                        let unstaged = work != '.' && work != ' ';
                        changes.push(FileChange {
                            path: new_path.to_string(),
                            index,
                            work,
                            staged,
                            unstaged,
                            untracked: false,
                            conflicted,
                            renamed_from: Some(old_path.to_string()),
                        });
                    }
                }
            }
        }
    }

    changes.sort_by(|a, b| {
        if a.conflicted != b.conflicted {
            return b.conflicted.cmp(&a.conflicted);
        }
        if a.staged != b.staged {
            return b.staged.cmp(&a.staged);
        }
        if a.unstaged != b.unstaged {
            return b.unstaged.cmp(&a.unstaged);
        }
        if a.untracked != b.untracked {
            return b.untracked.cmp(&a.untracked);
        }
        a.path.cmp(&b.path)
    });

    Ok(StatusV2 {
        branch,
        upstream,
        ahead,
        behind,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug() {
        assert_eq!(slug("MyAgent"), "myagent");
        assert_eq!(slug("My Agent"), "my-agent");
        assert_eq!(slug("my-agent"), "my-agent");
        assert_eq!(slug("agent_123"), "agent-123");
        assert_eq!(slug("--agent--"), "agent");
        assert_eq!(slug("!!!"), "agent");
        assert_eq!(slug(""), "agent");
        assert_eq!(slug("../../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("-leading"), "leading");
        assert_eq!(slug("trailing-"), "trailing");
        assert_eq!(slug("multiple   spaces"), "multiple-spaces");
    }

    #[test]
    fn test_parse_worktrees() {
        let porcelain = "worktree /home/user/project
HEAD abc123def456
branch refs/heads/main

worktree /home/user/project-feature
HEAD 789012fed345
branch refs/heads/feature/test

worktree /home/user/detached-work
HEAD 111222333444
detached
";

        let worktrees = parse_worktrees(porcelain);
        assert_eq!(worktrees.len(), 3);

        assert_eq!(worktrees[0].path, PathBuf::from("/home/user/project"));
        assert_eq!(worktrees[0].branch, "main");
        assert!(worktrees[0].is_main);

        assert_eq!(
            worktrees[1].path,
            PathBuf::from("/home/user/project-feature")
        );
        assert_eq!(worktrees[1].branch, "feature/test");
        assert!(!worktrees[1].is_main);

        assert_eq!(worktrees[2].path, PathBuf::from("/home/user/detached-work"));
        assert_eq!(worktrees[2].branch, "(detached)");
        assert!(!worktrees[2].is_main);
    }

    #[test]
    fn test_integration_branch_at() {
        let temp = std::env::temp_dir().join(format!("taix-git-branch-at-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let base = repo.current_branch().unwrap();

        // Main checkout
        let main_branch = repo.branch_at(&temp).unwrap();
        assert_eq!(main_branch, base);

        // Worktree has its own branch
        let wt = repo.add_worktree("feature-x", &base).unwrap();
        let wt_branch = repo.branch_at(&wt.path).unwrap();
        assert_eq!(wt_branch, "taix/feature-x");
        assert_ne!(wt_branch, main_branch);

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_diff_stat() {
        let temp = std::env::temp_dir().join(format!("taix-git-diff-stat-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let base = repo.current_branch().unwrap();
        let wt = repo.add_worktree("feature-diff", &base).unwrap();

        // No changes yet
        let stat = repo.diff_stat(&wt.path, &base).unwrap();
        assert_eq!(stat.files, 0);
        assert_eq!(stat.insertions, 0);
        assert_eq!(stat.deletions, 0);

        // Committed change
        std::fs::write(wt.path.join("new.txt"), "line1\nline2\nline3\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("add")
            .arg("new.txt")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("commit")
            .arg("-m")
            .arg("add new.txt")
            .output()
            .unwrap();

        // Uncommitted change on top — this is the key case
        std::fs::write(wt.path.join("dirty.txt"), "uncommitted\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("add")
            .arg("dirty.txt")
            .output()
            .unwrap();
        let stat = repo.diff_stat(&wt.path, &base).unwrap();
        assert_eq!(stat.files, 2);
        assert!(stat.insertions > 3); // 3 from new.txt + 1 from dirty.txt

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_diff_text() {
        let temp = std::env::temp_dir().join(format!("taix-git-diff-text-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("base.txt"), "original\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let base = repo.current_branch().unwrap();
        let wt = repo.add_worktree("feature-text", &base).unwrap();

        // Make a change
        std::fs::write(wt.path.join("base.txt"), "modified\n").unwrap();

        // Get full diff
        let full = repo.diff_text(&wt.path, &base, 100000).unwrap();
        assert!(full.contains("modified"));
        assert!(full.contains("-original"));
        assert!(full.contains("+modified"));

        // Truncate on newline boundary
        let truncated = repo.diff_text(&wt.path, &base, 50).unwrap();
        assert!(truncated.len() <= 50);
        assert!(truncated.is_empty() || truncated.ends_with('\n'));
        // Verify UTF-8
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_merge_back_success() {
        let temp = std::env::temp_dir().join(format!("taix-git-merge-ok-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let base = repo.current_branch().unwrap();
        let wt = repo.add_worktree("feature-merge", &base).unwrap();

        // Make and commit a change in the worktree
        std::fs::write(wt.path.join("feature.txt"), "new feature\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("add")
            .arg("feature.txt")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("commit")
            .arg("-m")
            .arg("add feature")
            .output()
            .unwrap();

        // Merge back
        repo.merge_back(&wt.path, &base).unwrap();

        // Verify the commit is now reachable from base
        let log_output = Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("log")
            .arg("--oneline")
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log_output.stdout);
        assert!(log.contains("add feature"));

        // Verify feature.txt exists in main
        assert!(temp.join("feature.txt").exists());

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_merge_back_dirty_worktree() {
        let temp = std::env::temp_dir().join(format!("taix-git-merge-dirty-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let base = repo.current_branch().unwrap();
        let wt = repo.add_worktree("feature-dirty", &base).unwrap();

        // Make a committed change
        std::fs::write(wt.path.join("committed.txt"), "committed\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("add")
            .arg("committed.txt")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("commit")
            .arg("-m")
            .arg("committed work")
            .output()
            .unwrap();

        // Make worktree dirty
        std::fs::write(wt.path.join("dirty.txt"), "uncommitted\n").unwrap();

        // Merge should fail
        let result = repo.merge_back(&wt.path, &base);
        assert!(result.is_err());
        if let Err(Error::Git { stderr }) = result {
            assert!(stderr.contains("uncommitted change"));
        } else {
            panic!("expected Git error");
        }

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_merge_back_nothing_to_merge() {
        let temp = std::env::temp_dir().join(format!("taix-git-merge-nothing-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let base = repo.current_branch().unwrap();
        let wt = repo.add_worktree("feature-empty", &base).unwrap();

        // No commits in worktree
        let result = repo.merge_back(&wt.path, &base);
        assert!(result.is_err());
        if let Err(Error::Git { stderr }) = result {
            assert!(stderr.contains("no commits beyond"));
        } else {
            panic!("expected Git error");
        }

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_merge_back_conflict() {
        let temp = std::env::temp_dir().join(format!("taix-git-merge-conflict-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("conflict.txt"), "original\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let base = repo.current_branch().unwrap();
        let wt = repo.add_worktree("feature-conflict", &base).unwrap();

        // Change in worktree
        std::fs::write(wt.path.join("conflict.txt"), "worktree version\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("add")
            .arg("conflict.txt")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .arg("commit")
            .arg("-m")
            .arg("worktree change")
            .output()
            .unwrap();

        // Conflicting change in main
        std::fs::write(temp.join("conflict.txt"), "main version\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg("conflict.txt")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("main change")
            .output()
            .unwrap();

        // Merge should fail with conflict
        let result = repo.merge_back(&wt.path, &base);
        assert!(result.is_err(), "merge_back should fail on conflict");
        // On conflict, the repository is left in a conflicted state
        // (the assignment says to leave it as git left it, not abort)

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn status_reads_branch_counts_and_spells_the_bar_line() {
        let both =
            parse_status("## dev...origin/dev [ahead 1, behind 5]\nA  added.rs\n D deleted.rs\n");
        assert_eq!(
            both,
            Status {
                branch: "dev".into(),
                dirty: 2,
                ahead: 1,
                behind: 5
            }
        );
        assert_eq!(both.line(), "dev ✱2 ↑1 ↓5");

        let clean = parse_status("## main...origin/main\n");
        assert_eq!(clean.line(), "main");
        assert_eq!(parse_status("## main\n M x\n?? y\n").line(), "main ✱2");
        assert_eq!(
            parse_status("## feature/x...origin/feature/x [ahead 3]\n").line(),
            "feature/x ↑3"
        );
        assert_eq!(parse_status("## HEAD (no branch)\n").branch, "HEAD");
        assert_eq!(parse_status("## No commits yet on main\n").branch, "main");
        assert_eq!(DiffStat::default().line(), None);
        assert_eq!(
            DiffStat {
                files: 1,
                insertions: 4,
                deletions: 0
            }
            .line()
            .as_deref(),
            Some("1 file +4 -0")
        );
    }

    // Integration tests with real git repos
    #[test]
    fn test_integration_repo_lifecycle() {
        let temp = std::env::temp_dir().join(format!("taix-git-test-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        // Initialize repo
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        // Set local config
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        // Create initial commit
        std::fs::write(temp.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        // Test Repo::open
        let repo = Repo::open(&temp).unwrap();
        assert_eq!(std::fs::canonicalize(&temp).unwrap(), repo.root);

        // Test current_branch
        let branch = repo.current_branch().unwrap();
        assert!(branch == "main" || branch == "master");

        // With no remote, default_branch must fall back to the checked-out
        // branch — that fallback is what agents get branched from.
        assert_eq!(repo.default_branch().unwrap(), branch);

        // Test worktrees
        let wts = repo.worktrees().unwrap();
        assert_eq!(wts.len(), 1);
        assert!(wts[0].is_main);

        // Test add_worktree
        let wt1 = repo.add_worktree("feature-1", &branch).unwrap();
        assert!(wt1.path.exists());
        assert_eq!(wt1.branch, "taix/feature-1");
        assert!(!wt1.is_main);

        // Test idempotency
        let wt1_again = repo.add_worktree("feature-1", &branch).unwrap();
        assert_eq!(wt1.path, wt1_again.path);
        assert_eq!(wt1.branch, wt1_again.branch);

        // Status on a clean worktree, then a dirty one; the worktree line
        // reads the same tree through the cached base.
        assert_eq!(status(&wt1.path).unwrap().dirty, 0);
        std::fs::write(wt1.path.join("test.txt"), "dirty").unwrap();
        assert_eq!(status(&wt1.path).unwrap().dirty, 1);
        assert_eq!(repo.worktree_line(&wt1.path).unwrap(), wt1.branch);

        // Test remove_worktree
        repo.remove_worktree(&wt1.path, true).unwrap();
        assert!(!wt1.path.exists());

        // Cleanup
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_not_a_repo() {
        let temp = std::env::temp_dir().join(format!("taix-git-notrepo-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        let result = Repo::open(&temp);
        assert!(matches!(result, Err(Error::NotARepo)));

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_remove_nonexistent_worktree() {
        let temp = std::env::temp_dir().join(format!("taix-git-remove-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let fake_path = temp.join("fake-worktree");

        let result = repo.remove_worktree(&fake_path, false);
        assert!(result.is_err());

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_changes_with_rename() {
        let temp = std::env::temp_dir().join(format!("taix-git-changes-rename-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("old.txt"), "content\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        // Rename a file
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("mv")
            .arg("old.txt")
            .arg("new.txt")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let changes = repo.changes(&temp).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "new.txt");
        assert_eq!(changes[0].renamed_from, Some("old.txt".to_string()));
        assert!(changes[0].staged);

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_parse_status_v2_edge_cases() {
        let porcelain = r#"# branch.oid abc123
# branch.head (detached)
1 MM N... 100644 100644 100644 hash1 hash2 staged-unstaged.txt
2 R. N... 100644 100644 100644 hash3 hash4 R100 new.txt	old.txt
? untracked.txt
"#;
        let status = parse_status_v2(porcelain).unwrap();
        let changes = status.changes;

        assert_eq!(status.branch, "");
        assert_eq!(status.upstream, None);
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
        assert_eq!(changes.len(), 3);

        let rename = changes.iter().find(|c| c.path == "new.txt").unwrap();
        assert_eq!(rename.renamed_from, Some("old.txt".to_string()));
        assert!(rename.staged);
        assert_eq!(rename.index, 'R');

        let staged_unstaged = changes
            .iter()
            .find(|c| c.path == "staged-unstaged.txt")
            .unwrap();
        assert!(staged_unstaged.staged);
        assert!(staged_unstaged.unstaged);
        assert_eq!(staged_unstaged.index, 'M');
        assert_eq!(staged_unstaged.work, 'M');

        let untracked = changes.iter().find(|c| c.path == "untracked.txt").unwrap();
        assert!(untracked.untracked);
        assert!(!untracked.staged);
        assert!(!untracked.unstaged);
    }

    #[test]
    fn test_integration_stage_unstage() {
        let temp = std::env::temp_dir().join(format!("taix-git-stage-unstage-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("file.txt"), "content\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        // Modify file
        std::fs::write(temp.join("file.txt"), "modified\n").unwrap();

        let repo = Repo::open(&temp).unwrap();
        let before = repo.changes(&temp).unwrap();
        assert_eq!(before.len(), 1);
        assert!(before[0].unstaged);
        assert!(!before[0].staged);

        // Stage it
        repo.stage(&temp, &["file.txt".to_string()]).unwrap();
        let staged = repo.changes(&temp).unwrap();
        assert_eq!(staged.len(), 1);
        assert!(staged[0].staged);
        assert!(!staged[0].unstaged);

        // Unstage it
        repo.unstage(&temp, &["file.txt".to_string()]).unwrap();
        let after = repo.changes(&temp).unwrap();
        assert_eq!(after, before);

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_commit_on_empty_repo() {
        let temp = std::env::temp_dir().join(format!("taix-git-commit-empty-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("file.txt"), "content\n").unwrap();
        let repo = Repo::open(&temp).unwrap();
        repo.stage_all(&temp).unwrap();

        let hash = repo.commit(&temp, "initial commit", false).unwrap();
        assert!(!hash.is_empty());

        let log = repo.log(&temp, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].summary, "initial commit");

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_log_empty_repo() {
        let temp = std::env::temp_dir().join(format!("taix-git-log-empty-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let log = repo.log(&temp, 10).unwrap();
        assert_eq!(log.len(), 0);

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_branches_with_upstream() {
        let temp = std::env::temp_dir().join(format!("taix-git-branches-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("file.txt"), "content\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        // Create a second branch
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("-b")
            .arg("feature")
            .output()
            .unwrap();
        std::fs::write(temp.join("feature.txt"), "feature\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("feature")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let branches = repo.branches(&temp).unwrap();
        assert!(branches.len() >= 2);

        // Current branch should be first
        assert!(branches[0].current);
        assert_eq!(branches[0].name, "feature");

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_integration_discard() {
        let temp = std::env::temp_dir().join(format!("taix-git-discard-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();

        std::fs::write(temp.join("tracked.txt"), "content\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        // Create untracked files
        std::fs::write(temp.join("untracked1.txt"), "untracked1\n").unwrap();
        std::fs::write(temp.join("untracked2.txt"), "untracked2\n").unwrap();

        let repo = Repo::open(&temp).unwrap();

        // Discard only untracked1
        repo.discard(&temp, &["untracked1.txt".to_string()])
            .unwrap();

        assert!(!temp.join("untracked1.txt").exists());
        assert!(temp.join("untracked2.txt").exists());

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_is_repo() {
        let temp = std::env::temp_dir().join(format!("taix-git-is-repo-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();

        assert!(!is_repo(&temp));

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        assert!(is_repo(&temp));

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_merge() {
        let temp = std::env::temp_dir().join(format!("taix-git-merge-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("base.txt"), "base\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        // Fast-forward merge
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("-b")
            .arg("feature")
            .output()
            .unwrap();
        std::fs::write(temp.join("feature.txt"), "feature\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("feature")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("main")
            .output()
            .unwrap();
        let result = repo.merge(&temp, "feature", false);
        assert!(result.is_ok());

        // No-ff merge
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("-b")
            .arg("feature2")
            .output()
            .unwrap();
        std::fs::write(temp.join("feature2.txt"), "feature2\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("feature2")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("main")
            .output()
            .unwrap();
        let result = repo.merge(&temp, "feature2", true);
        assert!(result.is_ok());

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_in_progress_and_step_abort() {
        let temp = std::env::temp_dir().join(format!("taix-git-in-progress-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .arg("-b")
            .arg("main")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("conflict.txt"), "base\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        // Create conflicting branches
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("-b")
            .arg("branch1")
            .output()
            .unwrap();
        std::fs::write(temp.join("conflict.txt"), "branch1\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("branch1")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("main")
            .output()
            .unwrap();
        std::fs::write(temp.join("conflict.txt"), "main\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("main")
            .output()
            .unwrap();

        // Nothing in progress initially
        assert_eq!(repo.in_progress(&temp).unwrap(), None);

        // A conflicting merge stops, and an abort puts it back.
        let _ = repo.merge(&temp, "branch1", false);
        assert_eq!(repo.in_progress(&temp).unwrap(), Some(Op::Merge));
        repo.step(&temp, Step::Abort).unwrap();
        assert_eq!(repo.in_progress(&temp).unwrap(), None);

        // The same conflict, resolved and continued, lands a merge commit.
        // This is the step that breaks if `--continue` is handed any other
        // argument: git refuses `git merge --continue --no-edit` outright.
        let _ = repo.merge(&temp, "branch1", false);
        std::fs::write(temp.join("conflict.txt"), "settled\n").unwrap();
        repo.stage(&temp, &["conflict.txt".to_string()]).unwrap();
        repo.step(&temp, Step::Continue).unwrap();
        assert_eq!(repo.in_progress(&temp).unwrap(), None);
        let log = repo.log(&temp, 1).unwrap();
        assert!(
            log[0].summary.contains("Merge"),
            "merge commit missing: {}",
            log[0].summary
        );

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_resolve() {
        let temp = std::env::temp_dir().join(format!("taix-git-resolve-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .arg("-b")
            .arg("main")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("conflict.txt"), "base\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("-b")
            .arg("feature")
            .output()
            .unwrap();
        std::fs::write(temp.join("conflict.txt"), "theirs\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("feature")
            .output()
            .unwrap();

        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("main")
            .output()
            .unwrap();
        std::fs::write(temp.join("conflict.txt"), "ours\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("main")
            .output()
            .unwrap();

        let _ = repo.merge(&temp, "feature", false);

        // Resolve with ours (current branch = main)
        let result = repo.resolve(&temp, "conflict.txt", Side::Ours);
        assert!(result.is_ok());
        let content = std::fs::read_to_string(temp.join("conflict.txt")).unwrap();
        assert_eq!(content, "ours\n");

        // Resolve with theirs on another attempt
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("merge")
            .arg("--abort")
            .output()
            .unwrap();
        let _ = repo.merge(&temp, "feature", false);
        let result = repo.resolve(&temp, "conflict.txt", Side::Theirs);
        assert!(result.is_ok());
        let content = std::fs::read_to_string(temp.join("conflict.txt")).unwrap();
        assert_eq!(content, "theirs\n");

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_delete_branch() {
        let temp = std::env::temp_dir().join(format!("taix-git-delete-branch-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .arg("-b")
            .arg("main")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("base.txt"), "base\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        // Create and merge a branch
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("-b")
            .arg("merged")
            .output()
            .unwrap();
        std::fs::write(temp.join("merged.txt"), "merged\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("merged")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("main")
            .output()
            .unwrap();
        repo.merge(&temp, "merged", false).unwrap();

        // Delete merged branch without force
        let result = repo.delete_branch(&temp, "merged", false);
        assert!(result.is_ok());

        // Create unmerged branch
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("-b")
            .arg("unmerged")
            .output()
            .unwrap();
        std::fs::write(temp.join("unmerged.txt"), "unmerged\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("unmerged")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("checkout")
            .arg("main")
            .output()
            .unwrap();

        // Delete unmerged without force should fail
        let result = repo.delete_branch(&temp, "unmerged", false);
        assert!(result.is_err());

        // Delete unmerged with force should succeed
        let result = repo.delete_branch(&temp, "unmerged", true);
        assert!(result.is_ok());

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_stash_list_and_ops() {
        let temp = std::env::temp_dir().join(format!("taix-git-stash-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("base.txt"), "base\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        // Create changes and stash
        std::fs::write(temp.join("base.txt"), "modified\n").unwrap();
        repo.stash_push(&temp, "test stash").unwrap();

        let stashes = repo.stash_list(&temp).unwrap();
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].index, 0);
        assert!(stashes[0].message.contains("test stash"));

        // Apply stash
        repo.stash_apply(&temp, 0).unwrap();
        let content = std::fs::read_to_string(temp.join("base.txt")).unwrap();
        assert_eq!(content, "modified\n");

        // Drop stash
        repo.stash_drop(&temp, 0).unwrap();
        let stashes = repo.stash_list(&temp).unwrap();
        assert_eq!(stashes.len(), 0);

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_reset() {
        let temp = std::env::temp_dir().join(format!("taix-git-reset-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("file.txt"), "v1\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("v1")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();
        let v1_hash = Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("rev-parse")
            .arg("HEAD")
            .output()
            .unwrap();
        let v1 = String::from_utf8_lossy(&v1_hash.stdout).trim().to_string();

        std::fs::write(temp.join("file.txt"), "v2\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("v2")
            .output()
            .unwrap();

        // Soft reset keeps changes staged
        repo.reset(&temp, &v1, Reset::Soft).unwrap();
        let output = Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("diff")
            .arg("--cached")
            .arg("--name-only")
            .output()
            .unwrap();
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(staged.contains("file.txt"));

        // Hard reset discards changes
        repo.reset(&temp, &v1, Reset::Hard).unwrap();
        let content = std::fs::read_to_string(temp.join("file.txt")).unwrap();
        assert_eq!(content, "v1\n");

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_commit_files() {
        let temp = std::env::temp_dir().join(format!("taix-git-commit-files-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("a.txt"), "a\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        std::fs::write(temp.join("a.txt"), "modified\n").unwrap();
        std::fs::write(temp.join("b.txt"), "added\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("changes")
            .output()
            .unwrap();

        let hash = Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("rev-parse")
            .arg("HEAD")
            .output()
            .unwrap();
        let hash_str = String::from_utf8_lossy(&hash.stdout).trim().to_string();

        let files = repo.commit_files(&temp, &hash_str).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.path == "a.txt" && f.status == 'M'));
        assert!(files.iter().any(|f| f.path == "b.txt" && f.status == 'A'));

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_file_log() {
        let temp = std::env::temp_dir().join(format!("taix-git-file-log-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("tracked.txt"), "v1\n").unwrap();
        std::fs::write(temp.join("other.txt"), "other\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        std::fs::write(temp.join("tracked.txt"), "v2\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("update tracked")
            .output()
            .unwrap();

        std::fs::write(temp.join("other.txt"), "other v2\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("update other")
            .output()
            .unwrap();

        let commits = repo.file_log(&temp, "tracked.txt", 10).unwrap();
        assert_eq!(commits.len(), 2);
        assert!(commits[0].summary.contains("update tracked"));
        assert!(commits[1].summary.contains("initial"));

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_discard_all() {
        let temp = std::env::temp_dir().join(format!("taix-git-discard-all-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .output()
            .unwrap();
        std::fs::write(temp.join("tracked.txt"), "original\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("add")
            .arg(".")
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("commit")
            .arg("-m")
            .arg("initial")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        // Modify tracked and add untracked
        std::fs::write(temp.join("tracked.txt"), "modified\n").unwrap();
        std::fs::write(temp.join("untracked.txt"), "untracked\n").unwrap();

        // Discard without untracked
        repo.discard_all(&temp, false).unwrap();
        let content = std::fs::read_to_string(temp.join("tracked.txt")).unwrap();
        assert_eq!(content, "original\n");
        assert!(temp.join("untracked.txt").exists());

        // Modify again and discard with untracked
        std::fs::write(temp.join("tracked.txt"), "modified again\n").unwrap();
        repo.discard_all(&temp, true).unwrap();
        let content = std::fs::read_to_string(temp.join("tracked.txt")).unwrap();
        assert_eq!(content, "original\n");
        assert!(!temp.join("untracked.txt").exists());

        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_ignore() {
        let temp = std::env::temp_dir().join(format!("taix-git-ignore-{}", rand_suffix()));
        std::fs::create_dir_all(&temp).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&temp)
            .arg("init")
            .output()
            .unwrap();

        let repo = Repo::open(&temp).unwrap();

        // Add first pattern
        repo.ignore(&temp, "*.log").unwrap();
        let content = std::fs::read_to_string(temp.join(".gitignore")).unwrap();
        assert_eq!(content, "*.log\n");

        // Add second pattern
        repo.ignore(&temp, "*.tmp").unwrap();
        let content = std::fs::read_to_string(temp.join(".gitignore")).unwrap();
        assert_eq!(content, "*.log\n*.tmp\n");

        // Re-add existing pattern (no-op)
        repo.ignore(&temp, "*.log").unwrap();
        let content = std::fs::read_to_string(temp.join(".gitignore")).unwrap();
        assert_eq!(content, "*.log\n*.tmp\n");

        std::fs::remove_dir_all(&temp).unwrap();
    }

    fn rand_suffix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{:x}", nanos)
    }
}
