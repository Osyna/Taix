//! Diagnostics for the errors TaiX deliberately swallows.
//!
//! A dropped tmux reply, a refused web request and a failed capture all have
//! to be non-fatal - the window keeps running - but with nothing written
//! anywhere they are also undiagnosable. `TAIX_LOG=1` turns them into stderr
//! lines. Off, the macro costs one relaxed load.
//!
//! stderr, never stdout: `taix-mcp` speaks JSON-RPC on stdout.

use std::sync::atomic::{AtomicU8, Ordering};

/// `2` means "not looked at the environment yet".
static ON: AtomicU8 = AtomicU8::new(2);

pub fn enabled() -> bool {
    match ON.load(Ordering::Relaxed) {
        2 => {
            let on = std::env::var_os("TAIX_LOG")
                .is_some_and(|v| !v.is_empty() && v != "0" && v != "false");
            ON.store(u8::from(on), Ordering::Relaxed);
            on
        }
        v => v == 1,
    }
}

/// `trace!("capture of pane {pane} failed: {e}")`.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        if $crate::trace::enabled() {
            eprintln!("taix: {}", format_args!($($arg)*));
        }
    };
}
