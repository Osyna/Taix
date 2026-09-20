//! The GTK4 front as its own binary, so `taix` itself never links a UI
//! toolkit. Reached through `taix --gui`, which execs this.

use std::process::ExitCode;

fn main() -> ExitCode {
    // A launcher-started window inherits the desktop session's PATH, which
    // is missing every directory npm, bun, pyenv or cargo installs agents
    // into. Do this before GTK exists: it is the only single-threaded
    // moment there is.
    taix_core::adopt_shell_path();
    ExitCode::from(taix_gui::run() as u8)
}
