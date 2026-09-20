//! One scratch directory per test that reads an `XDG_*` path.
//!
//! Those paths are read from the environment, which is per-process, and
//! `cargo test` runs a crate's tests as threads of one process. A test that
//! repointed `XDG_DATA_HOME` while another was saving a session made the
//! second one fail for no reason of its own - and a test that did not repoint
//! it wrote into the real `~/.local/share/taix`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

static LOCK: Mutex<()> = Mutex::new(());

pub struct Scope {
    _guard: MutexGuard<'static, ()>,
    key: &'static str,
    previous: Option<OsString>,
    dir: PathBuf,
}

/// Hold off every other environment-sensitive test. Tests that only *read*
/// a process-global - local time, an `XDG_*` path - need this too: being the
/// only writer does not help when the reader runs concurrently.
pub fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Point `key` at an empty directory until the returned scope is dropped.
/// Holds the same lock for that whole time.
pub fn redirect(key: &'static str, tag: &str) -> Scope {
    let guard = lock();
    let dir = std::env::temp_dir().join(format!("taix-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let previous = std::env::var_os(key);
    unsafe { std::env::set_var(key, &dir) };
    Scope {
        _guard: guard,
        key,
        previous,
        dir,
    }
}

impl Scope {
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
