//! Strings both fronts print the same way. Each of these used to live in
//! every bar, header and panel that needed it, and drifted.

use std::path::{Path, PathBuf};

/// Compact uptime. Seconds are noise on a bar that repaints every 2 s, and a
/// day-long session should not read "1512m".
pub fn human_secs(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    match (secs / 86_400, h) {
        (0, 0) => format!("{m}m"),
        (0, h) => format!("{h}h{m:02}m"),
        (d, h) => format!("{d}d{:02}h", h % 24),
    }
}

/// `~` beats a repeated home directory in a column that has to ellipsize.
pub fn contract_home(path: &Path) -> String {
    let text = path.display().to_string();
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => text,
        },
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_drops_seconds_and_never_reads_in_minutes_past_an_hour() {
        assert_eq!(human_secs(0), "0m");
        assert_eq!(human_secs(59), "0m");
        assert_eq!(human_secs(90), "1m");
        assert_eq!(human_secs(3600), "1h00m");
        assert_eq!(human_secs(3600 * 25 + 60), "1d01h");
    }

    #[test]
    fn home_contracts_to_a_tilde_and_nothing_else_does() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        assert_eq!(contract_home(&home), "~");
        assert_eq!(contract_home(&home.join("src/x")), "~/src/x");
        assert_eq!(contract_home(Path::new("/opt/x")), "/opt/x");
    }
}
