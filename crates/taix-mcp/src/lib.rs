mod rpc;
mod tools;

use std::io::{self, BufRead};
use taix_core::{Config, Store};

pub fn run() {
    // CRITICAL: Nothing except protocol JSON may ever go to stdout.
    // All logging MUST go to stderr — a stray println! corrupts the stream.

    let config = Config::load();
    let store = match Store::open(&Config::store_path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to open store: {}", e);
            std::process::exit(1);
        }
    };

    let state = tools::State::new(store, config);
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("stdin read error: {}", e);
                break;
            }
        };

        if let Some(response) = rpc::handle(&state, &line) {
            // CRITICAL: Protocol output only. One JSON object per line.
            if let Err(e) = io::Write::write_all(&mut stdout, response.as_bytes()) {
                eprintln!("stdout write error: {}", e);
                break;
            }
            if let Err(e) = io::Write::write_all(&mut stdout, b"\n") {
                eprintln!("stdout write error: {}", e);
                break;
            }
            if let Err(e) = io::Write::flush(&mut stdout) {
                eprintln!("stdout flush error: {}", e);
                break;
            }
        }
    }
}
