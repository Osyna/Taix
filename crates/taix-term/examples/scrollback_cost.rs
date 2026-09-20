//! Measure what an `alacritty_terminal` emulator actually costs per pane,
//! with and without scrollback.
//!
//! The plan's RAM budget rests on Alacritty issue #3650 (~5.22 KiB per
//! scrollback line, never freed). This turns that citation into a number
//! measured on this machine, for the exact configuration the prototype uses.
//! ponytail: throwaway measurement, not shipped code.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};

struct Size {
    cols: usize,
    rows: usize,
    history: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows + self.history
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages * 4)
        .unwrap_or(0)
}

/// Allocate `n` terminals, fill their scrollback, and report RSS per terminal.
fn measure(label: &str, n: usize, history: usize) {
    let before = rss_kb();
    let size = Size {
        cols: 80,
        rows: 24,
        history,
    };
    let mut terms: Vec<Term<VoidListener>> = Vec::with_capacity(n);
    let mut parser: alacritty_terminal::vte::ansi::Processor =
        alacritty_terminal::vte::ansi::Processor::new();

    for _ in 0..n {
        let config = Config {
            scrolling_history: history,
            ..Config::default()
        };
        let mut term = Term::new(config, &size, VoidListener);
        // Scrollback that is merely reserved may not be resident; scroll real
        // content through it so the measurement reflects actual use.
        for line in 0..(history + 24) {
            parser.advance(
                &mut term,
                format!("line {line} of agent output\r\n").as_bytes(),
            );
        }
        terms.push(term);
    }

    let after = rss_kb();
    let per = (after - before) as f64 / n as f64;
    println!(
        "{label:<34} history {history:>6}  {:>7.1} MiB total  {:>8.1} KiB/pane  {:>6.2} KiB/line",
        (after - before) as f64 / 1024.0,
        per,
        per / (history + 24) as f64,
    );
    drop(terms);
}

fn main() {
    println!("emulator residency, 80x24 panes, scrolled through their history\n");
    measure("prototype config (tmux owns it)", 40, 0);
    measure("modest per-pane scrollback", 40, 1_000);
    measure("plan's tmux history-limit", 10, 20_000);
}
