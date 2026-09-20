//! Bytes that arrived without a path, given one.
//!
//! Agents take an image or a document as a path on a command line, so the
//! only thing a front has to do with a pasted screenshot, a dropped file or
//! a photo uploaded from a phone is put it on disk and name it. All three
//! land here, so a path typed by the desktop and a path typed by the web
//! front mean the same thing. The cache directory is the honest home for
//! it: losing these on a reboot costs nothing, and nothing a user pasted
//! belongs in a git repository they did not choose to put it in.

use std::path::{Path, PathBuf};

/// `$XDG_CACHE_HOME/taix/files`.
pub fn dir() -> PathBuf {
    crate::config::base_dir(
        std::env::var("XDG_CACHE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        ".cache",
    )
    .join("taix/files")
}

/// Write `bytes` under `name`, or beside it when that name is taken, and
/// answer with the path to type.
pub fn stash(bytes: &[u8], name: &str) -> std::io::Result<PathBuf> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let path = free_path(&dir, &safe_name(name));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// A name from somewhere else - a phone's camera roll, a `Content-Type`
/// guess - reduced to what a filesystem and a shell both take verbatim.
/// Anything else becomes `_`: this name is about to be pasted onto a
/// command line, so a quote or a `;` in it is not a naming question.
fn safe_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let name: String = base
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .take(64)
        .collect();
    let name = name.trim_start_matches(['.', '-']);
    if name.is_empty() {
        "file".to_string()
    } else {
        name.to_string()
    }
}

/// The name asked for, or the next free one beside it. A camera roll is
/// full of `IMG_0001.jpg`, and silently overwriting yesterday's upload
/// because it happened to share a name would lose data.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}{ext}")))
        .find(|path| !path.exists())
        .expect("an unbounded range always has a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostile_name_becomes_one_harmless_word() {
        assert_eq!(safe_name("photo.jpg"), "photo.jpg");
        // A path is not a name: only the last segment can be one, which is
        // also what makes `..` harmless.
        assert_eq!(safe_name("/etc/../passwd"), "passwd");
        assert_eq!(safe_name("notes;rm -rf.txt"), "notes_rm_-rf.txt");
        assert_eq!(safe_name("'; drop table;"), "___drop_table_");
        // Nothing usable left, and a dotfile is not a usable name either.
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("..."), "file");
    }

    #[test]
    fn a_taken_name_gets_a_neighbour_not_an_overwrite() {
        let dir = std::env::temp_dir().join(format!("taix-stash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(free_path(&dir, "shot.png"), dir.join("shot.png"));
        std::fs::write(dir.join("shot.png"), b"a").unwrap();
        assert_eq!(free_path(&dir, "shot.png"), dir.join("shot-2.png"));
        std::fs::write(dir.join("shot-2.png"), b"b").unwrap();
        assert_eq!(free_path(&dir, "shot.png"), dir.join("shot-3.png"));
        // A name with no extension keeps its whole self as the stem.
        std::fs::write(dir.join("README"), b"c").unwrap();
        assert_eq!(free_path(&dir, "README"), dir.join("README-2"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
