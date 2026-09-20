//! Finding a file in the project: names fuzzily, contents literally.
//!
//! One worker thread walks the tree while the panel keeps drawing. Hits are
//! left in a mutex for the panel to drain every few frames, so the first
//! ones are on screen long before the walk is finished, and a walk that is
//! taking too long can be stopped without losing what it already found.
//!
//! Two matchers, one pass. A name is scored by the same fuzzy scorer the
//! command palette uses - `src/mn` finds `src/main.rs` - and contents are
//! matched literally, because nobody greps a codebase by subsequence. A file
//! whose name matched is not read: it is already in the list, higher up than
//! any content hit would put it.
//!
//! Breadth-first, so `README.md` at the root arrives before anything under
//! `deps/`, and the list is useful in its first hundred milliseconds.
//!
//! ponytail: one walker thread. What a person feels is time to first hit,
//! and that is a few hundred microseconds in a project-sized tree; the
//! whole of `~/Projects` - 692k entries - streams its first hit in 1.2s and
//! finishes cold in 84s, which is what the stop button is for. Fanning the
//! content reads out over a pool is the upgrade if a cold monorepo ever
//! needs it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::files::Entry;

/// Contents are only read below this size. A lockfile, a minified bundle or
/// a checked-in binary is never what the search is for, and reading it
/// stalls the walk for everything that is.
const MAX_FILE: u64 = 1 << 20;
/// Hits are collected up to this many, then the walk stops. Twenty screens
/// of results is already more than anyone reads; the answer is a longer
/// needle, not a longer list.
const LIMIT: usize = 400;
/// Shorter needles match names only. Two characters appear in every file in
/// the project, and reading them all to prove it helps nobody.
const CONTENT_MIN: usize = 3;
/// Never walked. Not a gitignore parser - a name list catches the
/// directories that are pathological everywhere, and the tree beside the
/// search still shows them.
pub const SKIP: [&str; 10] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".mypy_cache",
];

/// Why a path is in the results, and what to show for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The name matched.
    Name,
    /// The contents matched, on this 1-based line.
    Content { line: usize, text: String },
}

/// One result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub entry: Entry,
    pub why: Why,
    /// Fuzzy score for a name hit; content hits share one score and sort by
    /// path.
    pub score: i32,
}

/// Best first: names above contents, better scores above worse, and paths
/// to break the tie so the order never wobbles between two polls.
pub fn order(hits: &mut [Hit]) {
    hits.sort_by(|a, b| {
        let content = |h: &Hit| matches!(h.why, Why::Content { .. });
        content(a)
            .cmp(&content(b))
            .then(b.score.cmp(&a.score))
            .then_with(|| a.entry.path.cmp(&b.entry.path))
    });
}

#[derive(Default)]
struct Shared {
    found: Mutex<Vec<Hit>>,
    /// Entries looked at, for the progress line.
    scanned: AtomicUsize,
    /// Hits produced, including the ones already drained.
    hits: AtomicUsize,
    done: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn push(&self, hit: Hit) {
        let mut found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
        found.push(hit);
        // Both counters move under the lock, so "400 hits" and the stop that
        // follows it cannot disagree.
        if self.hits.fetch_add(1, Ordering::Relaxed) + 1 >= LIMIT {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// A running search. Dropping it stops the thread.
pub struct Search {
    shared: Arc<Shared>,
}

impl Search {
    /// Start walking `root` for `query`.
    pub fn start(root: PathBuf, query: &str, hidden: bool) -> Search {
        let shared = Arc::new(Shared::default());
        let needle = Needle::new(query);
        let worker = shared.clone();
        // Detached: nothing waits for it. It stops when `stop` is set, which
        // `Drop` does, or when the tree is walked.
        std::thread::spawn(move || {
            walk(&root, &needle, hidden, &worker);
            worker.done.store(true, Ordering::Release);
        });
        Search { shared }
    }

    /// The hits found since the last call.
    pub fn take(&self) -> Vec<Hit> {
        std::mem::take(
            &mut *self
                .shared
                .found
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Entries looked at so far, and whether the walk has finished.
    pub fn progress(&self) -> (usize, bool) {
        (
            self.shared.scanned.load(Ordering::Relaxed),
            self.shared.done.load(Ordering::Acquire),
        )
    }

    /// Give up on the rest of the tree; what was found stays.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The needle, prepared once for the whole walk.
struct Needle {
    text: String,
    /// ASCII-folded bytes, for the subsequence gate and the content scan.
    lower: Vec<u8>,
    /// Smartcase: an all-lowercase needle ignores case, as in the pane find.
    exact: bool,
    /// Long enough to be worth reading files for.
    content: bool,
    /// A needle of pure ASCII can use the byte gate; anything else goes
    /// straight to the scorer, which folds properly.
    ascii: bool,
}

impl Needle {
    fn new(text: &str) -> Needle {
        Needle {
            text: text.to_string(),
            lower: text.to_ascii_lowercase().into_bytes(),
            exact: text.chars().any(char::is_uppercase),
            content: text.chars().count() >= CONTENT_MIN,
            ascii: text.is_ascii(),
        }
    }

    /// The fuzzy score of a name, or `None` when it does not match.
    ///
    /// The gate exists because [`crate::palette::score`] allocates four
    /// vectors per call and a walk asks it a hundred thousand times; nearly
    /// every name fails on the first missing character.
    fn score(&self, name: &str) -> Option<i32> {
        if self.ascii && !gate(&self.lower, name) {
            return None;
        }
        crate::palette::score(&self.text, name)
    }
}

/// Is `needle` a subsequence of `hay`, folding ASCII?
fn gate(needle: &[u8], hay: &str) -> bool {
    let mut want = needle.iter();
    let mut next = want.next();
    for byte in hay.bytes() {
        match next {
            Some(c) if byte.to_ascii_lowercase() == *c => next = want.next(),
            Some(_) => {}
            None => return true,
        }
    }
    next.is_none()
}

/// The first offset in `hay` where the needle occurs, or `None`.
///
/// ASCII folding rather than Unicode: the offset has to index the untouched
/// text so the line around it can be shown, and only ASCII folds without
/// moving the bytes after it.
fn find(hay: &str, needle: &Needle) -> Option<usize> {
    if needle.exact {
        return hay.find(&needle.text);
    }
    let hay = hay.as_bytes();
    let want = needle.lower.as_slice();
    let (first, len) = (*want.first()?, want.len());
    let mut at = 0;
    while at + len <= hay.len() {
        let step = hay[at..=hay.len() - len]
            .iter()
            .position(|b| b.to_ascii_lowercase() == first)?;
        at += step;
        if hay[at..at + len].eq_ignore_ascii_case(want) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// The line number and the text of the line holding `at`.
fn line_at(text: &str, at: usize) -> (usize, String) {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let line = text[..at].bytes().filter(|b| *b == b'\n').count() + 1;
    // A minified line is one match and eight thousand columns; the row shows
    // a readable slice of it.
    (line, text[start..end].trim().chars().take(160).collect())
}

/// Search one file's contents.
fn grep(path: &Path, needle: &Needle) -> Option<Why> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 || len > MAX_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    // A NUL in the head is what `grep` calls binary, and it is right.
    if bytes.iter().take(4096).any(|b| *b == 0) {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let at = find(text, needle)?;
    let (line, text) = line_at(text, at);
    Some(Why::Content { line, text })
}

/// Walk `root` breadth-first, pushing hits as they are found.
fn walk(root: &Path, needle: &Needle, hidden: bool, shared: &Shared) {
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(listing) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in listing.flatten() {
            if shared.stopped() {
                return;
            }
            shared.scanned.fetch_add(1, Ordering::Relaxed);
            let name = entry.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                continue;
            }
            // From the directory listing, so a symlink is a symlink: the
            // walk never follows one, and never loops.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let dir = kind.is_dir();
            if dir && SKIP.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if dir {
                queue.push_back(path.clone());
            }
            let make = |why, score| Hit {
                entry: Entry {
                    name: name.clone(),
                    path: path.clone(),
                    dir,
                    mtime: None,
                },
                why,
                score,
            };
            match needle.score(&name) {
                Some(score) => shared.push(make(Why::Name, score)),
                // Only files nobody named: a name hit already outranks
                // whatever its contents would have said.
                None if needle.content && kind.is_file() => {
                    if let Some(why) = grep(&path, needle) {
                        shared.push(make(why, 0));
                    }
                }
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// One project, in the shape that breaks a search: a heavy directory, a
    /// dotfile, a binary.
    fn tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("taix-search-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("node_modules/left-pad")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {\n    hello();\n}\n").unwrap();
        fs::write(dir.join("src/hello_world.rs"), "nothing to see\n").unwrap();
        fs::write(dir.join("README.md"), "# Title\nsay HELLO there\n").unwrap();
        fs::write(dir.join(".env"), "SECRET=hello\n").unwrap();
        fs::write(dir.join("node_modules/left-pad/index.js"), "hello\n").unwrap();
        fs::write(dir.join("blob.bin"), [b'h', b'i', 0, b'x']).unwrap();
        dir
    }

    /// Drain until the walk says it is finished.
    fn run(root: &Path, query: &str, hidden: bool) -> Vec<Hit> {
        let search = Search::start(root.to_path_buf(), query, hidden);
        let mut hits = Vec::new();
        loop {
            hits.extend(search.take());
            if search.progress().1 {
                hits.extend(search.take());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        order(&mut hits);
        hits
    }

    fn names(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|h| h.entry.name.clone()).collect()
    }

    #[test]
    fn a_name_outranks_a_body_and_bodies_sort_by_path() {
        let dir = tree("mixed");
        let hits = run(&dir, "hello", true);
        assert_eq!(
            names(&hits),
            ["hello_world.rs", ".env", "README.md", "main.rs"]
        );
        assert_eq!(hits[0].why, Why::Name);
        assert_eq!(
            hits[2].why,
            Why::Content {
                line: 2,
                text: "say HELLO there".to_string(),
            }
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_name_matches_out_of_order_the_body_never_does() {
        let dir = tree("fuzzy");
        // "hlwd" is a subsequence of the name and appears in no file.
        assert_eq!(names(&run(&dir, "hlwd", true)), ["hello_world.rs"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_short_needle_stays_out_of_the_files() {
        let dir = tree("short");
        // "se" is in two files and in no name: too short to open anything.
        assert!(run(&dir, "se", true).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn heavy_directories_dotfiles_and_binaries_are_left_alone() {
        let dir = tree("skips");
        assert!(!names(&run(&dir, "hello", true)).contains(&"index.js".to_string()));
        assert!(!names(&run(&dir, "hello", false)).contains(&".env".to_string()));
        // "hix" is in blob.bin, but a NUL says it is not text.
        assert!(run(&dir, "hix", true).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_uppercase_needle_is_taken_literally_in_bodies() {
        let dir = tree("case");
        // The name still matches either way; only `README.md` says HELLO.
        assert_eq!(
            names(&run(&dir, "HELLO", true)),
            ["hello_world.rs", "README.md"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stopping_ends_the_walk() {
        let dir = tree("stop");
        let search = Search::start(dir.clone(), "hello", true);
        search.stop();
        while !search.progress().1 {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_offset_of_a_match_indexes_the_untouched_text() {
        let needle = Needle::new("hello");
        let text = "über HELLO\nnext";
        let at = find(text, &needle).unwrap();
        assert_eq!(&text[at..at + 5], "HELLO");
        assert_eq!(line_at(text, at), (1, "über HELLO".to_string()));
    }

    #[test]
    fn the_gate_agrees_with_the_scorer() {
        for (needle, name) in [("mn", "main.rs"), ("MR", "main.rs"), ("xyz", "main.rs")] {
            let gated = Needle::new(needle).score(name);
            assert_eq!(gated, crate::palette::score(needle, name), "{needle}");
        }
    }
}
