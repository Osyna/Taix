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
        ("GET", "/api/stash") => stash_list(),
        ("GET", "/api/raw") => raw(req, hub),
        ("GET", "/api/find") => find(req, hub),
        ("POST", "/api/browser") => browser(req, hub),
        ("GET", "/api/git") => git(req, hub),
        ("GET", "/api/worktree") => worktree(req, hub),
        ("GET", "/api/diff") => diff(req, hub),
        ("POST", "/api/git") => git_op(req, hub),
        ("GET", "/api/since") => since(req, hub),
        ("GET", "/api/pr") => pr(req, hub),
        ("GET", "/api/jobs") => jobs(req, hub),
        ("POST", "/api/jobs") => jobs_op(req, hub),
        ("GET", "/api/joblog") => joblog(req, hub),
        ("GET", "/api/config") => config(req, hub),
        ("POST", "/api/config") => config_op(req, hub),
        ("GET", "/api/capture") => capture(req),
        ("GET", "/api/search") => search(req),
        ("GET", "/api/transcript") => transcript(req),
        ("GET", "/api/history") => history(req, hub),
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

/// What `taix_core::stash` holds, newest first: the page's attachment tray
/// after a reload, when nothing is left in its memory.
fn stash_list() -> Reply {
    let entries: Vec<_> = taix_core::stash::list()
        .into_iter()
        .map(|(path, size, modified)| {
            let mtime = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            serde_json::json!({
                "name": path.file_name().unwrap_or_default().to_string_lossy(),
                "path": path.display().to_string(),
                "size": size,
                "mtime": mtime,
            })
        })
        .collect();
    Reply::json(200, serde_json::json!({ "entries": entries }).to_string())
}

const RAW_MAX: u64 = 32 << 20;

/// Whether `path` may be served as an image, and as which content type.
/// The extension is the whole allow-list: SVG and HTML would run script
/// from this origin. Access is a project root (`in_project`) or the stash;
/// both compare canonical paths, so `..` and symlinks cannot walk out.
/// A missing file under a refused path is a 403, not a 404, so this does
/// not reveal what exists elsewhere.
fn raw_target(
    path: &Path,
    stash: &Path,
    in_project: bool,
) -> Result<(PathBuf, &'static str), (u16, &'static str)> {
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let kind = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return Err((415, "not an image type")),
    };
    let Ok(real) = path.canonicalize() else {
        return Err(if in_project {
            (404, "not found")
        } else {
            (403, "outside every project")
        });
    };
    let in_stash = stash.canonicalize().is_ok_and(|s| real.starts_with(s));
    if !(in_project || in_stash) {
        return Err((403, "outside every project"));
    }
    Ok((real, kind))
}

/// An image's bytes, for previews of files the page has no copy of: a
/// draft restored after a reload, the tray, a project's pictures.
fn raw(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = Path::new(req.arg("path"));
    if path.as_os_str().is_empty() {
        return Reply::fail(400, "missing path");
    }
    let (real, kind) = match raw_target(path, &taix_core::stash::dir(), hub.allows(path)) {
        Ok(found) => found,
        Err((status, why)) => return Reply::fail(status, why),
    };
    match std::fs::metadata(&real) {
        Ok(m) if m.len() > RAW_MAX => return Reply::fail(413, "too large"),
        Ok(_) => {}
        Err(e) => return Reply::fail(500, &e.to_string()),
    }
    match std::fs::read(&real) {
        Ok(body) => Reply {
            status: 200,
            kind,
            body,
            headers: vec![
                ("x-content-type-options", "nosniff".into()),
                ("cache-control", "private, max-age=3600".into()),
            ],
        },
        Err(e) => Reply::fail(500, &e.to_string()),
    }
}

/// Files of `root` matching `q`, best first: basename starts with it, then
/// basename contains it, then the path contains it; shorter paths win a
/// tie, so an empty `q` lists the shallowest files.
fn rank(rels: Vec<String>, q: &str) -> Vec<String> {
    let q = q.to_lowercase();
    let mut hits: Vec<(u8, String)> = rels
        .into_iter()
        .filter_map(|rel| {
            let low = rel.to_lowercase();
            let base = low.rsplit('/').next().unwrap_or(&low);
            let tier = if base.starts_with(&q) {
                0
            } else if base.contains(&q) {
                1
            } else if low.contains(&q) {
                2
            } else {
                return None;
            };
            Some((tier, rel))
        })
        .collect();
    hits.sort_by(|a, b| (a.0, a.1.len(), &a.1).cmp(&(b.0, b.1.len(), &b.1)));
    hits.into_iter().map(|(_, rel)| rel).collect()
}

/// Files under `root` for the @-mention picker. Git knows which are worth
/// listing (tracked or untracked, minus ignored); outside a repository the
/// tree is walked instead, bounded and skipping what is never wanted.
fn find(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let root = Path::new(req.arg("path"));
    if root.as_os_str().is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(root) {
        return Reply::fail(403, "outside every project");
    }
    let rels: Vec<String> = match run_limited(
        root,
        "git",
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        3,
    ) {
        Some(out) => out
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        None => walk_files(root),
    };
    // `--cached` still names files deleted from disk; a mention of one is a dead path.
    let entries: Vec<_> = rank(rels, req.arg("q"))
        .into_iter()
        .filter(|rel| root.join(rel).is_file())
        .take(30)
        .map(|rel| serde_json::json!({ "path": root.join(&rel).display().to_string(), "rel": rel }))
        .collect();
    Reply::json(200, serde_json::json!({ "entries": entries }).to_string())
}

/// Relative paths of regular files under `root`, at most 20000 entries seen.
fn walk_files(root: &Path) -> Vec<String> {
    let (mut files, mut seen) = (Vec::new(), 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            seen += 1;
            if seen > 20_000 {
                return files;
            }
            let (Ok(kind), name) = (
                entry.file_type(),
                entry.file_name().to_string_lossy().into_owned(),
            ) else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && name != "node_modules" && name != "target" {
                    stack.push(entry.path());
                }
            } else if kind.is_file()
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                files.push(rel.display().to_string());
            }
        }
    }
    files
}

/// Ask the desktop to do something in a browser window; the GUI answers
/// asynchronously. Used by the MCP server to drive WebKitGTK.
fn browser(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let body: serde_json::Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => return Reply::fail(400, &e.to_string()),
    };

    match hub.ask(body, std::time::Duration::from_secs(30)) {
        Ok(result) => Reply::json(200, serde_json::to_string(&result).unwrap_or_default()),
        Err(e) => Reply::json(
            200,
            serde_json::json!({ "ok": false, "error": e }).to_string(),
        ),
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

/// Run `program args` in `dir`, killed after `secs`. Returns stdout when it exits 0.
fn run_limited(dir: &Path, program: &str, args: &[&str], secs: u64) -> Option<String> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // Drain on a thread: a full pipe would otherwise stall the child past the deadline.
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut out, &mut b).ok()?;
        Some(String::from_utf8_lossy(&b).into_owned())
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(secs);
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let s = reader.join().ok()??;
    ok.then_some(s)
}

/// "Changes since you last looked": commits in `mark..HEAD` plus the
/// uncommitted diff. The mark itself lives in the browser.
fn since(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = req.arg("path");
    if path.is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(Path::new(path)) {
        return Reply::fail(403, "outside every project");
    }
    let dir = Path::new(path);
    let mark = req.arg("mark");
    // A sha only: anything else would be read by git as an option or a range.
    if !(4..=64).contains(&mark.len()) || !mark.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Reply::fail(400, "bad mark");
    }
    let Some(head) = run_limited(dir, "git", &["rev-parse", "HEAD"], 10) else {
        return Reply::fail(500, "not a git repository or no commits");
    };
    let range = format!("{mark}..HEAD");
    // None: the mark is gone (rebased away, other clone) - say so, do not guess.
    let commits = run_limited(
        dir,
        "git",
        &[
            "log",
            "-200",
            "--format=%H%x1f%h%x1f%s%x1f%an%x1f%at",
            range.as_str(),
            "--",
        ],
        10,
    )
    .map(|text| {
        text.lines()
            .filter_map(|l| {
                let mut f = l.split('\x1f');
                Some(serde_json::json!({
                    "hash": f.next()?,
                    "short": f.next()?,
                    "subject": f.next()?,
                    "author": f.next()?,
                    "when": f.next()?.parse::<i64>().ok()?,
                }))
            })
            .collect::<Vec<_>>()
    });
    const MAX_DIFF: usize = 512 * 1024;
    let mut diff = run_limited(dir, "git", &["diff", "HEAD", "--"], 10).unwrap_or_default();
    let truncated = diff.len() > MAX_DIFF;
    if truncated {
        // The cut lands after a newline, an ASCII byte, so it is a char boundary.
        let cut = diff.as_bytes()[..MAX_DIFF]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1);
        diff.truncate(cut);
    }
    let json = serde_json::json!({
        "head": head.trim(),
        "commits": commits,
        "diff": diff,
        "truncated": truncated,
    });
    Reply::json(200, json.to_string())
}

/// How long a `gh pr view` answer (or its absence) is reused. gh talks to
/// the network, so the page's polling must not reach it.
const PR_TTL: std::time::Duration = std::time::Duration::from_secs(90);

type PrCache = HashMap<(PathBuf, String), (Instant, String)>;
static PR_CACHE: LazyLock<Mutex<PrCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The current branch's pull request: `{number,state,title,url,checks}` or
/// `{}` when `gh` is missing, the project is no GitHub repo, or no PR exists.
fn pr(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let path = req.arg("path");
    if path.is_empty() {
        return Reply::fail(400, "missing path");
    }
    if !hub.allows(Path::new(path)) {
        return Reply::fail(403, "outside every project");
    }
    // The branch only keys the cache: a checkout must not show the old branch's PR.
    let key = (PathBuf::from(path), req.arg("branch").to_string());
    if let Ok(cache) = PR_CACHE.lock()
        && let Some((at, json)) = cache.get(&key)
        && at.elapsed() < PR_TTL
    {
        return Reply::json(200, json.clone());
    }
    let json = run_limited(
        Path::new(path),
        "gh",
        &[
            "pr",
            "view",
            "--json",
            "number,state,title,url,statusCheckRollup",
        ],
        15,
    )
    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    .filter(|v| v.get("number").is_some())
    .map(|v| {
        let (mut pass, mut fail, mut pending) = (0, 0, 0);
        for c in v["statusCheckRollup"].as_array().into_iter().flatten() {
            // CheckRun carries status+conclusion, StatusContext carries state.
            let s = c["conclusion"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| c["state"].as_str())
                .unwrap_or("PENDING");
            match s {
                "SUCCESS" | "NEUTRAL" | "SKIPPED" => pass += 1,
                "PENDING" | "EXPECTED" | "QUEUED" | "IN_PROGRESS" => pending += 1,
                _ => fail += 1,
            }
        }
        serde_json::json!({
            "number": v["number"],
            "state": v["state"],
            "title": v["title"],
            "url": v["url"],
            "checks": { "pass": pass, "fail": fail, "pending": pending },
        })
        .to_string()
    })
    .unwrap_or_else(|| "{}".to_string());
    if let Ok(mut cache) = PR_CACHE.lock() {
        cache.insert(key, (Instant::now(), json.clone()));
    }
    Reply::json(200, json)
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
                "ok": run.ok(),
                "name": job.name,
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
    // `at` picks one run's output; without it, the job's rolling log.
    let text = match req.arg("at").parse::<i64>() {
        Ok(at) => taix_core::jobs::run_output(&cfg, id, at)
            .unwrap_or_else(|| "that run's output is no longer kept".to_string()),
        Err(_) => taix_core::jobs::log_tail(&cfg, id, 200),
    };

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
    let lines: usize = req.arg("lines").parse().unwrap_or(0).min(5000);
    let start = format!("-{lines}");
    match pane_text(req, (lines > 0).then_some(start.as_str())) {
        Ok(text) => Reply::json(200, serde_json::json!({ "text": text }).to_string()),
        Err(reply) => reply,
    }
}

/// The window's pane through `capture-pane`, from line `start` (`-` is the
/// oldest line tmux still holds) to the bottom, or just the visible screen.
fn pane_text(req: &Req, start: Option<&str>) -> Result<String, Reply> {
    let (_, agent) = agent_of(req)?;
    let Some(pane) = agent.pane else {
        return Err(Reply::fail(404, "window has no pane"));
    };
    let socket = taix_core::Config::load().tmux_socket;
    let target = format!("%{pane}");
    let mut args = vec!["-L", &socket, "capture-pane", "-p", "-J"];
    if let Some(start) = start {
        args.extend(["-S", start, "-E", "-"]);
    }
    args.extend(["-t", &target]);
    match std::process::Command::new("tmux").args(&args).output() {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err(Reply::fail(
            500,
            String::from_utf8_lossy(&out.stderr).trim(),
        )),
        Err(e) => Err(Reply::fail(500, &e.to_string())),
    }
}

/// Lines of the pane's whole scrollback that hold `q`, ignoring case.
fn search(req: &Req) -> Reply {
    let needle = req.arg("q").to_lowercase();
    if needle.is_empty() {
        return Reply::fail(400, "missing q");
    }
    match pane_text(req, Some("-")) {
        Ok(text) => {
            let (total, hits) = grep_lines(&text, &needle);
            let matches: Vec<_> = hits
                .into_iter()
                .map(|(line, text)| serde_json::json!({ "line": line, "text": text }))
                .collect();
            Reply::json(
                200,
                serde_json::json!({ "matches": matches, "total": total, "lines": text.lines().count() })
                    .to_string(),
            )
        }
        Err(reply) => reply,
    }
}

const MATCHES: usize = 500;

/// How many lines of `text` hold the lowercase `needle`, and the last
/// `MATCHES` of them as (1-based line number, line cut to 400 characters).
fn grep_lines(text: &str, needle: &str) -> (usize, Vec<(usize, String)>) {
    let mut hits: Vec<(usize, String)> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(needle))
        .map(|(i, line)| (i + 1, line.trim_end().chars().take(400).collect()))
        .collect();
    let total = hits.len();
    hits.drain(..total.saturating_sub(MATCHES));
    (total, hits)
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

/// The commands shells have run in these windows, newest first: one window
/// (`window=`), or all of them, narrowed to those whose text holds `q`
/// (ignoring case). Only a shell that sends OSC 133 marks has any.
fn history(req: &Req, hub: &Arc<crate::Hub>) -> Reply {
    let window: Option<i64> = req.arg("window").parse().ok();
    let needle = req.arg("q").to_lowercase();
    let mut found: Vec<(i64, taix_term::Command)> = hub
        .history
        .lock()
        .iter()
        .filter(|(id, _)| window.is_none_or(|w| w == *id))
        .flat_map(|(id, cmds)| cmds.iter().map(|c| (*id, c.clone())))
        .filter(|(_, c)| needle.is_empty() || c.text.to_lowercase().contains(&needle))
        .collect();
    found.sort_by_key(|(_, c)| std::cmp::Reverse(c.started_ms));
    found.truncate(500);
    let commands: Vec<_> = found
        .iter()
        .map(|(id, c)| {
            serde_json::json!({
                "window": id, "text": c.text, "at": c.started_ms,
                "ms": c.duration_ms, "exit": c.exit,
            })
        })
        .collect();
    Reply::json(200, serde_json::json!({ "commands": commands }).to_string())
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
            ws_key: String::new(),
            host: String::new(),
            raw: Default::default(),
        }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("taix-api-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_scrollback_search_ignores_case_numbers_lines_and_keeps_the_newest() {
        assert_eq!(
            grep_lines("one\nFoo bar  \nbaz\nfoo", "foo"),
            (2, vec![(2, "Foo bar".to_string()), (4, "foo".to_string())])
        );
        let many = "x\n".repeat(MATCHES + 3);
        let (total, hits) = grep_lines(&many, "x");
        assert_eq!((total, hits.len(), hits[0].0), (MATCHES + 3, MATCHES, 4));
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

    #[test]
    fn ranking_puts_basename_prefix_before_contains_before_path_and_short_first() {
        let all = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ranked = rank(
            all(&[
                "src/deep/dir/Main.rs",
                "docs/main-notes.md",
                "src/domain.rs",
                "main/readme.txt",
                "main.rs",
                "unrelated.rs",
            ]),
            "MAIN",
        );
        assert_eq!(
            ranked,
            [
                "main.rs",
                "docs/main-notes.md",
                "src/deep/dir/Main.rs",
                "src/domain.rs",
                "main/readme.txt"
            ]
        );
        // No query: everything matches, shortest path first.
        assert_eq!(rank(all(&["a/b/c.rs", "z.rs", "y/x.rs"]), "")[0], "z.rs");
    }

    #[test]
    fn a_raw_read_needs_an_image_extension_and_a_path_in_the_stash_or_a_project() {
        let base = dir("raw");
        let stash = base.join("stash");
        std::fs::create_dir(&stash).unwrap();
        std::fs::write(stash.join("a.PNG"), "x").unwrap();
        std::fs::write(stash.join("a.svg"), "x").unwrap();
        std::fs::write(base.join("x.png"), "x").unwrap();

        let ok = raw_target(&stash.join("a.PNG"), &stash, false).unwrap();
        assert_eq!(ok.1, "image/png");
        assert_eq!(
            raw_target(&stash.join("a.svg"), &stash, false)
                .unwrap_err()
                .0,
            415
        );
        // `..` out of the stash lands on a real file that is not the stash's.
        let escape = stash.join("../x.png");
        assert_eq!(raw_target(&escape, &stash, false).unwrap_err().0, 403);
        // The same file is fine once a project root covers it.
        assert!(raw_target(&escape, &stash, true).is_ok());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
