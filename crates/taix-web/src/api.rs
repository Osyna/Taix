use crate::http::{Reply, Req};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Instant, SystemTime};

/// A repository read is four `git` processes and about 5 ms, and the page
/// polls it. `.git/index` moving is what a commit or a stage looks like
/// from outside; the timer catches everything else.
const GIT_TTL: std::time::Duration = std::time::Duration::from_secs(2);

struct CachedGit {
    checked: Instant,
    index_mtime: Option<SystemTime>,
    json: String,
}

static GIT_CACHE: LazyLock<Mutex<HashMap<PathBuf, CachedGit>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static REPO_CACHE: LazyLock<Mutex<HashMap<PathBuf, Arc<taix_git::Repo>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn invalidate_git_cache(path: &Path) {
    if let Ok(mut cache) = GIT_CACHE.lock() {
        cache.remove(path);
    }
}

fn git_index_mtime(repo_root: &Path) -> Option<SystemTime> {
    std::fs::symlink_metadata(repo_root.join(".git/index"))
        .and_then(|m| m.modified())
        .ok()
}

pub(crate) fn route(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/files") => files(req, hub),
        ("GET", "/api/file") => file(req, hub),
        ("POST", "/api/file") => file_op(req, hub),
        ("POST", "/api/upload") => upload(req),
        ("GET", "/api/git") => git(req, hub),
        ("GET", "/api/worktree") => worktree(req, hub),
        ("GET", "/api/diff") => diff(req, hub),
        ("POST", "/api/git") => git_op(req, hub),
        ("GET", "/api/jobs") => jobs(req, hub),
        ("POST", "/api/jobs") => jobs_op(req, hub),
        ("GET", "/api/joblog") => joblog(req, hub),
        ("GET", "/api/config") => config(req, hub),
        ("POST", "/api/config") => config_op(req, hub),
        ("GET", "/api/capture") => capture(req),
        ("GET", "/api/transcript") => transcript(req),
        _ => Reply::fail(404, "not found"),
    }
}

fn files(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = req.arg("path");
    if path.is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(Path::new(path)) {
        return Reply::fail(403, "outside every project");
    }

    let dir = Path::new(path);
    let parent = dir.parent().map(|p| p.display().to_string());

    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    let mut entries = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let hidden = name.starts_with('.');
        let entry_path = entry.path();
        let size = metadata.len();
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        entries.push(serde_json::json!({
            "name": name,
            "path": entry_path.display().to_string(),
            "dir": metadata.is_dir(),
            "size": size,
            "mtime": mtime,
            "hidden": hidden,
        }));

        if entries.len() >= 2000 {
            break;
        }
    }

    entries.sort_by(|a, b| {
        let a_dir = a["dir"].as_bool().unwrap_or(false);
        let b_dir = b["dir"].as_bool().unwrap_or(false);
        if a_dir != b_dir {
            return b_dir.cmp(&a_dir);
        }
        let a_name = a["name"].as_str().unwrap_or("");
        let b_name = b["name"].as_str().unwrap_or("");
        a_name.to_lowercase().cmp(&b_name.to_lowercase())
    });

    let json = serde_json::json!({
        "path": path,
        "parent": parent,
        "entries": entries,
    });

    Reply::json(200, serde_json::to_string(&json).unwrap())
}

fn file(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = req.arg("path");
    if path.is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(Path::new(path)) {
        return Reply::fail(403, "outside every project");
    }

    let content = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    let max_size = 256 * 1024;
    let truncated = content.len() > max_size;
    let slice = if truncated {
        &content[..max_size]
    } else {
        &content
    };

    let text = match std::str::from_utf8(slice) {
        Ok(s) => s.to_string(),
        Err(_) => return Reply::fail(500, "not a text file"),
    };

    let json = serde_json::json!({
        "path": path,
        "text": text,
        "truncated": truncated,
    });

    Reply::json(200, serde_json::to_string(&json).unwrap())
}

/// A photo or a file from a phone, landed where a harness can read it.
///
/// Agents take paths, not attachments, so this is the same operation as
/// pasting a screenshot onto a pane on the desktop and shares its home
/// (`taix_core::stash`): write the bytes, answer with the path, and let
/// the composer paste it into the line. The body is the file itself rather
/// than a multipart form - there is one field, and parsing MIME
/// boundaries to discover that would be a parser to maintain for no
/// information.
fn upload(req: &Req) -> Reply {
    if req.body.is_empty() {
        return Reply::fail(400, "nothing uploaded");
    }
    match taix_core::stash::stash(&req.body, req.arg("name")) {
        Ok(path) => Reply::json(
            200,
            serde_json::json!({
                "path": path.display().to_string(),
                "name": path.file_name().unwrap_or_default().to_string_lossy(),
                "size": req.body.len(),
            })
            .to_string(),
        ),
        Err(e) => Reply::fail(500, &e.to_string()),
    }
}
#[derive(Deserialize)]
struct FileOp {
    op: String,
    path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

fn file_op(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let op: FileOp = match serde_json::from_str(std::str::from_utf8(&req.body).unwrap_or("")) {
        Ok(o) => o,
        Err(e) => return Reply::fail(400, &e.to_string()),
    };

    let base = Path::new(&op.path);
    if !hub.allows(base) {
        return Reply::fail(403, "outside every project");
    }

    let result_path = match op.op.as_str() {
        "mkdir" => {
            let Some(name) = op.name else {
                return Reply::fail(400, "mkdir needs name");
            };
            let path = base.join(&name);
            if !hub.allows(&path) {
                return Reply::fail(403, "outside every project");
            }
            if let Err(e) = std::fs::create_dir(&path) {
                return Reply::fail(500, &e.to_string());
            }
            path
        }
        "create" => {
            let Some(name) = op.name else {
                return Reply::fail(400, "create needs name");
            };
            let path = base.join(&name);
            if !hub.allows(&path) {
                return Reply::fail(403, "outside every project");
            }
            if let Err(e) = std::fs::File::create(&path) {
                return Reply::fail(500, &e.to_string());
            }
            path
        }
        "rename" => {
            let Some(to_str) = op.to else {
                return Reply::fail(400, "rename needs to");
            };
            let to_path = if to_str.starts_with('/') {
                PathBuf::from(&to_str)
            } else {
                base.parent().unwrap_or(base).join(&to_str)
            };
            if !hub.allows(&to_path) {
                return Reply::fail(403, "outside every project");
            }
            if let Err(e) = std::fs::rename(base, &to_path) {
                return Reply::fail(500, &e.to_string());
            }
            to_path
        }
        "delete" => {
            let metadata = match std::fs::metadata(base) {
                Ok(m) => m,
                Err(e) => return Reply::fail(500, &e.to_string()),
            };
            if metadata.is_dir() {
                if let Err(e) = std::fs::remove_dir_all(base) {
                    return Reply::fail(500, &e.to_string());
                }
            } else {
                if let Err(e) = std::fs::remove_file(base) {
                    return Reply::fail(500, &e.to_string());
                }
            }
            base.to_path_buf()
        }
        _ => return Reply::fail(400, "unknown file op"),
    };

    let json = serde_json::json!({
        "path": result_path.display().to_string(),
    });
    Reply::json(200, serde_json::to_string(&json).unwrap())
}

fn git(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = req.arg("path");
    if path.is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(Path::new(path)) {
        return Reply::fail(403, "outside every project");
    }

    let path = Path::new(path);
    let now = Instant::now();
    let current_mtime = git_index_mtime(path);

    // ponytail: one lock for every repo, split it if that ever contends
    if let Ok(cache) = GIT_CACHE.lock()
        && let Some(entry) = cache.get(path)
        && now.duration_since(entry.checked) < GIT_TTL
        && entry.index_mtime == current_mtime
    {
        return Reply::json(200, entry.json.clone());
    }

    let repo = match REPO_CACHE.lock() {
        Ok(mut cache) => {
            if let Some(r) = cache.get(path) {
                r.clone()
            } else {
                match taix_git::Repo::open(path) {
                    Ok(r) => {
                        let arc = Arc::new(r);
                        cache.insert(path.to_path_buf(), arc.clone());
                        arc
                    }
                    Err(e) => return Reply::fail(500, &e.to_string()),
                }
            }
        }
        Err(_) => match taix_git::Repo::open(path) {
            Ok(r) => Arc::new(r),
            Err(e) => return Reply::fail(500, &e.to_string()),
        },
    };

    let panel = match taix_git::panel(&repo, path) {
        Ok(p) => p,
        Err(e) => {
            taix_core::trace!("panel failed: {}", e);
            return Reply::fail(500, &e.to_string());
        }
    };

    let files: Vec<_> = panel
        .changes
        .iter()
        .map(|f| {
            let label = if f.conflicted {
                "U".to_string()
            } else if f.untracked {
                "?".to_string()
            } else if f.staged {
                match f.index {
                    ' ' | '\0' => "•".to_string(),
                    c => c.to_string(),
                }
            } else {
                match f.work {
                    ' ' | '\0' => "•".to_string(),
                    c => c.to_string(),
                }
            };
            serde_json::json!({
                "path": f.path,
                "label": label,
                "staged": f.staged,
                "conflict": f.conflicted,
            })
        })
        .collect();

    let branches_json: Vec<_> = panel
        .branches
        .iter()
        .map(|b| {
            serde_json::json!({
                "name": b.name,
                "head": b.current,
                "upstream": b.upstream,
                "track": format_track(b.ahead, b.behind),
                "when": b.when,
            })
        })
        .collect();

    let log_json: Vec<_> = panel
        .log
        .iter()
        .map(|c| {
            serde_json::json!({
                "hash": c.hash,
                "short": c.short,
                "subject": c.summary,
                "author": c.author,
                "when": c.when,
            })
        })
        .collect();

    let stashes_json: Vec<_> = panel
        .stashes
        .iter()
        .map(|s| {
            serde_json::json!({
                "index": s.index,
                "note": s.message,
                "when": s.when,
            })
        })
        .collect();

    let json = serde_json::json!({
        "root": path.display().to_string(),
        "branch": panel.branch,
        "upstream": panel.upstream,
        "ahead": panel.ahead,
        "behind": panel.behind,
        "dirty": panel.changes.len(),
        "op": panel.op.map(op_name),
        "remote": panel.remote,
        "files": files,
        "branches": branches_json,
        "log": log_json,
        "stashes": stashes_json,
    });

    let json_str = serde_json::to_string(&json).unwrap();

    if let Ok(mut cache) = GIT_CACHE.lock() {
        cache.insert(
            path.to_path_buf(),
            CachedGit {
                checked: now,
                index_mtime: current_mtime,
                json: json_str.clone(),
            },
        );
    }

    Reply::json(200, json_str)
}

fn diff(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = req.arg("path");
    if path.is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(Path::new(path)) {
        return Reply::fail(403, "outside every project");
    }

    let path = Path::new(path);
    let repo = match taix_git::Repo::open(path) {
        Ok(r) => r,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    let commit = req.arg("commit");
    let text = if !commit.is_empty() {
        match repo.commit_diff(path, commit, 512 * 1024) {
            Ok(t) => t,
            Err(e) => return Reply::fail(500, &e.to_string()),
        }
    } else {
        let file = req.arg("file");
        if file.is_empty() {
            return Reply::fail(400, "missing file or commit");
        }
        let staged = req.arg("staged") == "1";
        match repo.file_diff(path, file, staged, 512 * 1024) {
            Ok(t) => t,
            Err(e) => return Reply::fail(500, &e.to_string()),
        }
    };

    let json = serde_json::json!({ "text": text });
    Reply::json(200, serde_json::to_string(&json).unwrap())
}

fn worktree(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = req.arg("path");
    if path.is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(Path::new(path)) {
        return Reply::fail(403, "outside every project");
    }

    let path = Path::new(path);
    let repo = match taix_git::Repo::open(path) {
        Ok(r) => r,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    let base = if req.arg("base").is_empty() {
        match repo.default_branch() {
            Ok(b) => b,
            Err(e) => return Reply::fail(500, &e.to_string()),
        }
    } else {
        req.arg("base").to_string()
    };

    let stat = match repo.diff_stat(path, &base) {
        Ok(s) => s,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    // ponytail: no cache - base branch can vary, existing cache doesn't accommodate per-base keys
    const MAX_DIFF: usize = 512 * 1024;
    let output = match std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("diff")
        .arg(&base)
        .output()
    {
        Ok(o) => o,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    if !output.status.success() {
        return Reply::fail(500, &String::from_utf8_lossy(&output.stderr));
    }

    let full = match std::str::from_utf8(&output.stdout) {
        Ok(s) => s,
        Err(_) => return Reply::fail(500, "invalid UTF-8"),
    };

    let (diff, truncated) = if full.len() > MAX_DIFF {
        let bytes = &full.as_bytes()[..MAX_DIFF];
        let last_newline = bytes.iter().rposition(|&b| b == b'\n');
        let text = if let Some(pos) = last_newline {
            &full[..=pos]
        } else {
            ""
        };
        (text.to_string(), true)
    } else {
        (full.to_string(), false)
    };

    let json = serde_json::json!({
        "base": base,
        "diff": diff,
        "files": stat.files,
        "truncated": truncated,
    });
    Reply::json(200, serde_json::to_string(&json).unwrap())
}

#[derive(Deserialize)]
struct GitOp {
    path: String,
    op: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    amend: bool,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    create: bool,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    untracked: bool,
    #[serde(default)]
    no_ff: bool,
    #[serde(default)]
    onto: String,
    #[serde(default)]
    step: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    side: String,
    #[serde(default)]
    prune: bool,
    #[serde(default)]
    set_upstream: bool,
    #[serde(default)]
    index: usize,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
}

fn git_op(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let op: GitOp = match serde_json::from_str(std::str::from_utf8(&req.body).unwrap_or("")) {
        Ok(o) => o,
        Err(e) => return Reply::fail(400, &e.to_string()),
    };

    if !hub.allows(Path::new(&op.path)) {
        return Reply::fail(403, "outside every project");
    }

    // Named before the repository is opened: an op nobody implements is a
    // bad request whatever the directory turns out to be, and answering
    // "not a git repository" to it sends the caller after the wrong bug.
    const OPS: [&str; 25] = [
        "stage",
        "unstage",
        "discard",
        "stage-all",
        "unstage-all",
        "discard-all",
        "commit",
        "checkout",
        "delete-branch",
        "rename-branch",
        "merge",
        "rebase",
        "step",
        "resolve",
        "fetch",
        "pull",
        "push",
        "stash-push",
        "stash-pop",
        "stash-apply",
        "stash-drop",
        "cherry-pick",
        "revert",
        "reset",
        "ignore",
    ];
    if !OPS.contains(&op.op.as_str()) {
        return Reply::fail(400, "unknown git op");
    }

    let path = Path::new(&op.path);
    let repo = match taix_git::Repo::open(path) {
        Ok(r) => r,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    let out = match op.op.as_str() {
        "stage" => repo
            .stage(path, &op.files)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "unstage" => repo
            .unstage(path, &op.files)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "discard" => repo
            .discard(path, &op.files)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "stage-all" => repo
            .stage_all(path)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "unstage-all" => repo
            .unstage_all(path)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "discard-all" => repo
            .discard_all(path, op.untracked)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "commit" => repo
            .commit(path, &op.message, op.amend)
            .map_err(|e| e.to_string()),
        "checkout" => repo
            .checkout(path, &op.branch, op.create)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "delete-branch" => repo
            .delete_branch(path, &op.branch, op.force)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "rename-branch" => repo
            .rename_branch(path, &op.from, &op.to)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "merge" => repo
            .merge(path, &op.branch, op.no_ff)
            .map_err(|e| e.to_string()),
        "rebase" => repo.rebase(path, &op.onto).map_err(|e| e.to_string()),
        "step" => {
            let step = match op.step.as_str() {
                "continue" => taix_git::Step::Continue,
                "skip" => taix_git::Step::Skip,
                "abort" => taix_git::Step::Abort,
                _ => return Reply::fail(400, "invalid step"),
            };
            repo.step(path, step).map_err(|e| e.to_string())
        }
        "resolve" => {
            let side = match op.side.as_str() {
                "ours" => taix_git::Side::Ours,
                "theirs" => taix_git::Side::Theirs,
                _ => return Reply::fail(400, "invalid side"),
            };
            repo.resolve(path, &op.file, side)
                .map(|_| String::new())
                .map_err(|e| e.to_string())
        }
        "fetch" => repo
            .fetch(path, op.prune)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "pull" => repo.pull(path).map_err(|e| e.to_string()),
        "push" => repo
            .push(path, op.set_upstream, op.force)
            .map_err(|e| e.to_string()),
        "stash-push" => repo
            .stash_push(path, &op.message)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "stash-pop" => repo
            .stash_pop(path, op.index)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "stash-apply" => repo
            .stash_apply(path, op.index)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "stash-drop" => repo
            .stash_drop(path, op.index)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        "cherry-pick" => repo.cherry_pick(path, &op.hash).map_err(|e| e.to_string()),
        "revert" => repo.revert(path, &op.hash).map_err(|e| e.to_string()),
        "reset" => {
            let mode = match op.mode.as_str() {
                "soft" => taix_git::Reset::Soft,
                "mixed" => taix_git::Reset::Mixed,
                "hard" => taix_git::Reset::Hard,
                _ => return Reply::fail(400, "invalid reset mode"),
            };
            repo.reset(path, &op.hash, mode)
                .map(|_| String::new())
                .map_err(|e| e.to_string())
        }
        "ignore" => repo
            .ignore(path, &op.pattern)
            .map(|_| String::new())
            .map_err(|e| e.to_string()),
        _ => return Reply::fail(400, "unknown git op"),
    };

    match out {
        Ok(output) => {
            invalidate_git_cache(path);
            let json = serde_json::json!({ "ok": true, "out": output });
            Reply::json(200, serde_json::to_string(&json).unwrap())
        }
        Err(e) => Reply::fail(500, &e),
    }
}

fn jobs(_req: &Req, _hub: &Arc<crate::Hub>) -> Reply {
    let store = match taix_core::Store::open(&taix_core::Config::store_path()) {
        Ok(s) => s,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    let all_jobs = match store.jobs() {
        Ok(j) => j,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };
    let now = taix_core::jobs::now();
    let background = taix_core::jobs::background_status();

    let background_str = match background {
        taix_core::jobs::Background::Enabled => "enabled",
        taix_core::jobs::Background::Disabled => "disabled",
        taix_core::jobs::Background::Unavailable => "unavailable",
    };

    let jobs_json: Vec<_> = all_jobs
        .iter()
        .map(|j| {
            let schedule_parsed = taix_core::jobs::parse(&j.schedule).ok();
            let next = schedule_parsed.and_then(|s| {
                let base = j.last_run.unwrap_or(now);
                s.next_after(base)
            });
            let last = j.runs.last();
            let failing = last.map(|r| r.error.is_some()).unwrap_or(false);

            serde_json::json!({
                "id": j.id,
                "name": j.name,
                "enabled": j.enabled,
                "schedule": j.schedule,
                "action": action_name(j.action),
                "command": j.command,
                "harness": j.harness,
                "project": j.project,
                "session": j.session,
                "notify": notify_name(j.notify),
                "catch_up": j.catch_up,
                "next": next,
                "failing": failing,
                "last": last.map(|r| serde_json::json!({
                    "at": r.at,
                    "exit": r.exit,
                    "ms": r.ms,
                    "error": r.error,
                })),
            })
        })
        .collect();

    // collect the 30 most recent runs across all jobs
    let mut all_runs = Vec::new();
    for job in &all_jobs {
        for run in &job.runs {
            all_runs.push(serde_json::json!({
                "id": run.at,
                "job": job.id,
                "started": run.at,
                "ok": run.exit == Some(0),
                "summary": if let Some(err) = &run.error {
                    err.clone()
                } else if let Some(exit) = run.exit {
                    if exit == 0 {
                        format!("success ({}ms)", run.ms)
                    } else {
                        format!("exit {exit}")
                    }
                } else {
                    "no exit".to_string()
                },
            }));
        }
    }
    all_runs.sort_by(|a, b| {
        let a_id = a["id"].as_i64().unwrap_or(0);
        let b_id = b["id"].as_i64().unwrap_or(0);
        b_id.cmp(&a_id)
    });
    all_runs.truncate(30);

    let json = serde_json::json!({
        "background": background_str,
        "jobs": jobs_json,
        "runs": all_runs,
    });

    Reply::json(200, serde_json::to_string(&json).unwrap())
}
#[derive(Deserialize)]
#[serde(untagged)]
enum JobsOp {
    Simple {
        op: String,
        id: i64,
    },
    CreateUpdate {
        op: String,
        #[serde(default)]
        id: Option<i64>,
        name: String,
        schedule: String,
        action: String,
        enabled: bool,
        notify: String,
        #[serde(default)]
        project: Option<i64>,
        #[serde(default)]
        harness: Option<String>,
        #[serde(default)]
        command: Option<String>,
    },
}

fn jobs_op(req: &Req, _hub: &Arc<crate::Hub>) -> Reply {
    let op: JobsOp = match serde_json::from_str(std::str::from_utf8(&req.body).unwrap_or("")) {
        Ok(o) => o,
        Err(e) => return Reply::fail(400, &e.to_string()),
    };

    let store = match taix_core::Store::open(&taix_core::Config::store_path()) {
        Ok(s) => s,
        Err(e) => return Reply::fail(500, &e.to_string()),
    };

    match op {
        JobsOp::Simple { op, id } => match op.as_str() {
            "run" => {
                if let Err(e) = taix_core::jobs::spawn_run_now(id) {
                    return Reply::fail(500, &e.to_string());
                }
            }
            "enable" => {
                if let Err(e) = store.set_job_enabled(id, true) {
                    return Reply::fail(500, &e.to_string());
                }
            }
            "disable" => {
                if let Err(e) = store.set_job_enabled(id, false) {
                    return Reply::fail(500, &e.to_string());
                }
            }
            "delete" => {
                if let Err(e) = store.remove_job(id) {
                    return Reply::fail(500, &e.to_string());
                }
            }
            _ => return Reply::fail(400, "unknown jobs op"),
        },
        JobsOp::CreateUpdate {
            op,
            id,
            name,
            schedule,
            action,
            enabled,
            notify,
            project,
            harness,
            command,
        } => {
            // validate schedule
            if let Err(e) = taix_core::jobs::parse(&schedule) {
                return Reply::fail(400, &format!("bad schedule: {}", e));
            }

            let action_enum = match action.as_str() {
                "shell" => taix_core::Action::Shell,
                "agent" => taix_core::Action::Agent,
                "session" => taix_core::Action::Session,
                "git" => taix_core::Action::Git,
                _ => return Reply::fail(400, "unknown action"),
            };

            let notify_enum = match notify.as_str() {
                "never" => taix_core::Notify::Never,
                "failure" => taix_core::Notify::Failure,
                "always" => taix_core::Notify::Always,
                _ => return Reply::fail(400, "unknown notify"),
            };

            match op.as_str() {
                "create" => {
                    let new_job = taix_core::NewJob {
                        name,
                        enabled,
                        project,
                        schedule,
                        action: action_enum,
                    };
                    let mut job = match store.add_job(&new_job) {
                        Ok(j) => j,
                        Err(e) => return Reply::fail(500, &e.to_string()),
                    };
                    job.notify = notify_enum;
                    job.harness = harness.unwrap_or_default();
                    job.command = command.unwrap_or_default();
                    if let Err(e) = store.put_job(&job) {
                        return Reply::fail(500, &e.to_string());
                    }
                }
                "update" => {
                    let Some(job_id) = id else {
                        return Reply::fail(400, "update needs id");
                    };
                    let all_jobs = match store.jobs() {
                        Ok(j) => j,
                        Err(e) => return Reply::fail(500, &e.to_string()),
                    };
                    let mut job = match all_jobs.into_iter().find(|j| j.id == job_id) {
                        Some(j) => j,
                        None => return Reply::fail(404, "job not found"),
                    };
                    job.name = name;
                    job.enabled = enabled;
                    job.schedule = schedule;
                    job.action = action_enum;
                    job.notify = notify_enum;
                    job.project = project;
                    job.harness = harness.unwrap_or_default();
                    job.command = command.unwrap_or_default();
                    if let Err(e) = store.put_job(&job) {
                        return Reply::fail(500, &e.to_string());
                    }
                }
                _ => return Reply::fail(400, "unknown jobs op"),
            }
        }
    }

    let json = serde_json::json!({ "ok": true });
    Reply::json(200, serde_json::to_string(&json).unwrap())
}

fn joblog(req: &Req, _hub: &Arc<crate::Hub>) -> Reply {
    let id_str = req.arg("id");
    if id_str.is_empty() {
        return Reply::fail(400, "missing id");
    }
    let id: i64 = match id_str.parse() {
        Ok(i) => i,
        Err(_) => return Reply::fail(400, "invalid id"),
    };

    let cfg = taix_core::Config::load();
    let text = taix_core::jobs::log_tail(&cfg, id, 200);

    let json = serde_json::json!({ "text": text });
    Reply::json(200, serde_json::to_string(&json).unwrap())
}

fn config(_req: &Req, _hub: &Arc<crate::Hub>) -> Reply {
    let cfg = taix_core::Config::load();

    let json = serde_json::json!({
        "theme": cfg.theme,
        "font_size": cfg.font_size,
        "isolate": cfg.isolate,
        "editor": cfg.editor,
        "browser_home": cfg.browser_home,
        "idle_after_ms": cfg.idle_after_ms,
        "reap_idle_after_ms": cfg.reap_idle_after_ms,
        "web": {
            "enabled": true,
            "port": _hub.health().port,
        },
    });

    Reply::json(200, serde_json::to_string(&json).unwrap())
}
fn config_op(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let patch: std::collections::HashMap<String, serde_json::Value> =
        match serde_json::from_str(std::str::from_utf8(&req.body).unwrap_or("")) {
            Ok(p) => p,
            Err(e) => return Reply::fail(400, &e.to_string()),
        };

    let mut cfg = taix_core::Config::load();
    crate::proto::apply_patch(&mut cfg, &patch);

    if let Err(e) = cfg.save() {
        return Reply::fail(500, &e.to_string());
    }

    // queue the same command for the GUI to apply live
    hub.push_cmd(crate::proto::Cmd::Config {
        patch: patch.clone(),
    });

    let json = serde_json::json!({
        "theme": cfg.theme,
        "font_size": cfg.font_size,
        "isolate": cfg.isolate,
        "editor": cfg.editor,
        "browser_home": cfg.browser_home,
        "idle_after_ms": cfg.idle_after_ms,
        "reap_idle_after_ms": cfg.reap_idle_after_ms,
        "web": {
            "enabled": cfg.web.enabled,
            "port": cfg.web.port,
        },
    });

    Reply::json(200, serde_json::to_string(&json).unwrap())
}

fn format_track(ahead: usize, behind: usize) -> String {
    match (ahead, behind) {
        (0, 0) => String::new(),
        (a, 0) => format!("↑{a}"),
        (0, b) => format!("↓{b}"),
        (a, b) => format!("↑{a} ↓{b}"),
    }
}

fn op_name(op: taix_git::Op) -> &'static str {
    match op {
        taix_git::Op::Merge => "merge",
        taix_git::Op::Rebase => "rebase",
        taix_git::Op::CherryPick => "cherry-pick",
        taix_git::Op::Revert => "revert",
    }
}

fn action_name(action: taix_core::Action) -> &'static str {
    match action {
        taix_core::Action::Shell => "shell",
        taix_core::Action::Agent => "agent",
        taix_core::Action::Session => "session",
        taix_core::Action::Git => "git",
    }
}

fn notify_name(notify: taix_core::Notify) -> &'static str {
    match notify {
        taix_core::Notify::Never => "never",
        taix_core::Notify::Failure => "failure",
        taix_core::Notify::Always => "always",
    }
}

/// The agent a `window=` argument names, with its project. The store is
/// read from disk: these run on a web thread, and the GUI's copy is not
/// shareable across it.
fn agent_of(req: &Req) -> Result<(taix_core::Project, taix_core::Agent), Reply> {
    let id: i64 = req
        .arg("window")
        .parse()
        .map_err(|_| Reply::fail(400, "invalid window id"))?;
    let store = taix_core::Store::open(&taix_core::Config::store_path())
        .map_err(|e| Reply::fail(500, &e.to_string()))?;
    let projects = store
        .projects()
        .map_err(|e| Reply::fail(500, &e.to_string()))?;
    for project in projects {
        let agents = store
            .agents(project.id)
            .map_err(|e| Reply::fail(500, &e.to_string()))?;
        if let Some(agent) = agents.into_iter().find(|a| a.id == id) {
            return Ok((project, agent));
        }
    }
    Err(Reply::fail(404, "no such window"))
}

/// The pane as tmux holds it: the visible screen, or the last `lines` of
/// scrollback and screen.
fn capture(req: &Req) -> Reply {
    let (_, agent) = match agent_of(req) {
        Ok(found) => found,
        Err(reply) => return reply,
    };
    let Some(pane) = agent.pane else {
        return Reply::fail(404, "window has no pane");
    };
    let lines: usize = req.arg("lines").parse().unwrap_or(0).min(5000);
    let socket = taix_core::Config::load().tmux_socket;
    let target = format!("%{pane}");
    let start = format!("-{lines}");
    let mut args = vec!["-L", &socket, "capture-pane", "-p", "-J"];
    if lines > 0 {
        args.extend(["-S", &start, "-E", "-"]);
    }
    args.extend(["-t", &target]);
    match std::process::Command::new("tmux").args(&args).output() {
        Ok(out) if out.status.success() => Reply::json(
            200,
            serde_json::json!({ "text": String::from_utf8_lossy(&out.stdout) }).to_string(),
        ),
        Ok(out) => Reply::fail(500, String::from_utf8_lossy(&out.stderr).trim()),
        Err(e) => Reply::fail(500, &e.to_string()),
    }
}

/// The tail of the window's transcript log. Outlives the pane: a killed
/// agent's last words are still on disk.
fn transcript(req: &Req) -> Reply {
    let (project, agent) = match agent_of(req) {
        Ok(found) => found,
        Err(reply) => return reply,
    };
    let lines: usize = req.arg("lines").parse().unwrap_or(200).clamp(1, 5000);
    let path = taix_core::log::path(&taix_core::Config::load(), &project.name, &agent.name);
    let content = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Reply::fail(404, "this window has no transcript yet");
        }
        Err(e) => return Reply::fail(500, &e.to_string()),
    };
    // Rotation caps the file at 4 MiB, so reading it whole is the cheap
    // path; a tail-seek would be code for a case that cannot happen.
    let all: Vec<&str> = content.lines().collect();
    let tail = &all[all.len().saturating_sub(lines)..];
    Reply::json(
        200,
        serde_json::json!({ "text": tail.join("\n"), "path": path.display().to_string() })
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, path: &str, query: &[(&str, &str)], body: &str) -> Req {
        Req {
            method: method.to_string(),
            path: path.to_string(),
            query: query
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
            cookies: Default::default(),
            accept_encoding: String::new(),
            if_none_match: String::new(),
            peer: String::new(),
            agent: String::new(),
        }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("taix-api-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_path_outside_every_project_is_refused() {
        let hub = std::sync::Arc::new(crate::Hub::detached());
        let root = dir("allowed");
        hub.set_roots(vec![root.clone()]);
        let reply = route(&req("GET", "/api/files", &[("path", "/etc")], ""), &hub);
        assert_eq!(reply.status, 403);
        let inside = route(
            &req(
                "GET",
                "/api/files",
                &[("path", &root.display().to_string())],
                "",
            ),
            &hub,
        );
        assert_eq!(inside.status, 200);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_listing_puts_directories_first_then_sorts_by_name() {
        let hub = std::sync::Arc::new(crate::Hub::detached());
        let root = dir("listing");
        hub.set_roots(vec![root.clone()]);
        for name in ["b.txt", "A.txt"] {
            std::fs::write(root.join(name), "x").unwrap();
        }
        for name in ["zeta", "Alpha"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let reply = route(
            &req(
                "GET",
                "/api/files",
                &[("path", &root.display().to_string())],
                "",
            ),
            &hub,
        );
        let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        let names: Vec<&str> = body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Alpha", "zeta", "A.txt", "b.txt"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unknown_endpoint_is_a_404_and_an_unknown_git_op_a_400() {
        let hub = std::sync::Arc::new(crate::Hub::detached());
        let root = dir("ops");
        hub.set_roots(vec![root.clone()]);
        assert_eq!(route(&req("GET", "/api/nope", &[], ""), &hub).status, 404);
        let body = format!(r#"{{"path":"{}","op":"launch-missiles"}}"#, root.display());
        assert_eq!(
            route(&req("POST", "/api/git", &[], &body), &hub).status,
            400
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn file_op_refuses_paths_escaping_project_roots() {
        let hub = std::sync::Arc::new(crate::Hub::detached());
        let root = dir("file-escape");
        hub.set_roots(vec![root.clone()]);

        // .. path should be refused
        let dotdot_body = serde_json::json!({
            "op": "mkdir",
            "path": root.display().to_string(),
            "name": "../escape"
        })
        .to_string();
        let reply = route(&req("POST", "/api/file", &[], &dotdot_body), &hub);
        assert_eq!(reply.status, 403);

        // absolute path outside every root should be refused
        let abs_body = serde_json::json!({
            "op": "create",
            "path": "/etc",
            "name": "bad"
        })
        .to_string();
        let reply = route(&req("POST", "/api/file", &[], &abs_body), &hub);
        assert_eq!(reply.status, 403);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn job_create_rejects_bad_schedule() {
        let hub = std::sync::Arc::new(crate::Hub::detached());

        // invalid cron should fail with 400
        let bad_cron_body = serde_json::json!({
            "op": "create",
            "name": "test",
            "schedule": "invalid cron",
            "action": "shell",
            "enabled": true,
            "notify": "never",
            "command": "echo test"
        })
        .to_string();
        let reply = route(&req("POST", "/api/jobs", &[], &bad_cron_body), &hub);
        assert_eq!(reply.status, 400);
        assert!(String::from_utf8_lossy(&reply.body).contains("schedule"));
    }

    #[test]
    fn worktree_endpoint_validates_path() {
        let hub = std::sync::Arc::new(crate::Hub::detached());
        let root = dir("worktree");
        hub.set_roots(vec![root.clone()]);

        // missing path should fail with 400
        let reply = route(&req("GET", "/api/worktree", &[], ""), &hub);
        assert_eq!(reply.status, 400);

        // path outside every project should fail with 403
        let reply = route(&req("GET", "/api/worktree", &[("path", "/etc")], ""), &hub);
        assert_eq!(reply.status, 403);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
