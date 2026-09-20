//! Gzips the embedded assets at build time.
//!
//! The front end ships inside the binary, so there is nothing on disk to
//! put behind a web server that would compress it. Doing it here costs
//! nothing at run time and turns a 210 KB first load into about 55 KB.
//! A build machine without `gzip` writes empty copies and the server
//! serves the plain bytes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    walk(Path::new("assets"), &out);
}

fn walk(dir: &Path, out: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let Ok(relative) = path.strip_prefix("assets") else {
            continue;
        };
        let target = out.join(format!("{}.gz", relative.display()));
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let zipped = fs::read(&path).map(gzip).unwrap_or_default();
        let _ = fs::write(&target, zipped);
    }
}

fn gzip(source: Vec<u8>) -> Vec<u8> {
    let child = Command::new("gzip")
        .args(["-9", "-n", "-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return Vec::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&source);
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => out.stdout,
        _ => Vec::new(),
    }
}
