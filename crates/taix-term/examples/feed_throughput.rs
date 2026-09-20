//! Measure how fast the render path digests agent output.
//!
//! This exists to decide one thing: whether shrinking the binary with
//! `opt-level = "z"` costs throughput on the hot path. Feeding `%output` into
//! the emulator and turning the grid into styled runs is the only work TaiX
//! does per frame that scales with how chatty the agents are, so it is the
//! only place a size-over-speed setting could hurt.
//!
//! ponytail: throwaway measurement, not shipped code.

use std::time::Instant;
use taix_term::{Chunk, Live, Size};

fn colourful_output(lines: usize, cols: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines * cols * 2);
    for i in 0..lines {
        // Roughly what a chatty agent emits: SGR changes mid-line, a prefix,
        // and a wrap-length body.
        out.extend_from_slice(b"\x1b[38;5;");
        out.extend_from_slice(format!("{}", 16 + (i % 200)).as_bytes());
        out.extend_from_slice(b"m[");
        out.extend_from_slice(format!("{i:>6}").as_bytes());
        out.extend_from_slice(b"]\x1b[0m \x1b[1mstep\x1b[0m ");
        for c in 0..cols.saturating_sub(24) {
            out.push(b'a' + (c % 26) as u8);
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn main() {
    let (cols, rows) = (200usize, 50usize);
    let lines = 200_000;
    let bytes = colourful_output(lines, cols);
    let mb = bytes.len() as f64 / (1024.0 * 1024.0);

    let mut live = Live::new(Size { cols, rows });
    let start = Instant::now();
    // Chunked like real `%output` notifications rather than one giant write.
    for chunk in bytes.chunks(4096) {
        live.feed(chunk);
    }
    let feed = start.elapsed();

    let start = Instant::now();
    let frames = 600;
    let mut seen = 0usize;
    // Counted so the render cannot be optimised away entirely.
    let mut sink = |chunk: Chunk<'_>| {
        if let Chunk::Text(s, _) = chunk {
            seen += s.len();
        }
    };
    for _ in 0..frames {
        live.render(&mut sink);
    }
    let render = start.elapsed();

    println!(
        "feed    {mb:>7.1} MiB of ANSI in {:>7.1} ms  = {:>7.1} MiB/s",
        feed.as_secs_f64() * 1000.0,
        mb / feed.as_secs_f64(),
    );
    println!(
        "render  {frames} full {cols}x{rows} frames in {:>7.1} ms  = {:>7.2} ms/frame",
        render.as_secs_f64() * 1000.0,
        render.as_secs_f64() * 1000.0 / frames as f64,
    );
    println!(
        "        {} MiB of styled text produced",
        seen / (1024 * 1024)
    );
}
