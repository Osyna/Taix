//! Memory use via PSS (proportional set size), not RSS.
//!
//! PSS divides shared memory proportionally across every process mapping it, so
//! measuring the TaiX UI and the tmux server (both mapping libc, gtk, vte,
//! ...) adds each shared page once rather than double-counting it.

use std::collections::HashSet;
use std::fs;
use std::sync::LazyLock;

/// PSS of one process in KiB, or None if it is gone or unreadable.
#[cfg(target_os = "linux")]
pub fn pss_kib(pid: u32) -> Option<u64> {
    // smaps_rollup is faster than summing the full smaps
    if let Some(pss) = fs::read_to_string(format!("/proc/{}/smaps_rollup", pid))
        .ok()
        .and_then(|content| parse_pss(&content))
    {
        return Some(pss);
    }
    // ponytail: 4096 constant; sysconf(_SC_PAGESIZE) is overkill for Linux
    fs::read_to_string(format!("/proc/{}/statm", pid))
        .ok()
        .and_then(|s| parse_statm(&s))
}

/// Command line of a process, arguments joined by spaces. None if gone.
#[cfg(target_os = "linux")]
pub fn cmdline(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
    if raw.is_empty() {
        return None;
    }
    // Kernel threads have an empty cmdline; processes have NUL-separated args.
    let parts: Vec<&[u8]> = raw.split(|&b| b == 0).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    Some(
        parts
            .iter()
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// PSS of every listed process plus all their descendants, in KiB.
pub fn tree_pss_kib(roots: &[u32], table: &ProcTable) -> u64 {
    sum_pss(&table.descendants(roots))
}

/// What one process tree costs: memory now, CPU over its whole life.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// PSS in KiB.
    pub kib: u64,
    /// Cumulative user + system CPU time in USER_HZ ticks. Only differences
    /// between two readings mean anything.
    pub ticks: u64,
}

/// `/proc` reports CPU time in USER_HZ, which is 100 on every Linux target
/// TaiX builds for regardless of the kernel's own tick rate. Reading it
/// properly is `sysconf(_SC_CLK_TCK)`, i.e. linking libc for one constant.
pub const USER_HZ: u64 = 100;

/// One figure per listed process tree. Trees that overlap are counted in
/// each.
pub fn tree_usage_each(roots: &[u32], table: &ProcTable) -> Vec<Usage> {
    roots
        .iter()
        .map(|&root| {
            let pids = table.descendants(std::slice::from_ref(&root));
            Usage {
                kib: sum_pss(&pids),
                ticks: pids.iter().map(|&pid| table.ticks(pid)).sum(),
            }
        })
        .collect()
}

/// CPU used since a previous reading, as a percentage of one core.
///
/// Saturating, because a tree is not a fixed set of processes: a pane that
/// replaced its child reads *lower* than it did a moment ago, and that is a
/// zero, not an underflow.
pub fn cpu_percent(ticks: u64, previous: u64, elapsed: std::time::Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return 0.0;
    }
    ticks.saturating_sub(previous) as f64 / USER_HZ as f64 / secs * 100.0
}

fn sum_pss(pids: &[u32]) -> u64 {
    pids.iter().filter_map(|&pid| pss_kib(pid)).sum()
}

/// "82 MB", "1.2 GB", "940 KB" - for a status bar, not for arithmetic.
pub fn human_kib(kib: u64) -> String {
    if kib < 1024 {
        format!("{} KB", kib)
    } else if kib < 1024 * 1024 {
        let mb = kib as f64 / 1024.0;
        if mb < 10.0 {
            format!("{:.1} MB", mb)
        } else {
            format!("{} MB", mb.round() as u64)
        }
    } else {
        let gb = kib as f64 / (1024.0 * 1024.0);
        if gb < 10.0 {
            format!("{:.1} GB", gb)
        } else {
            format!("{} GB", gb.round() as u64)
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_pss(smaps_rollup: &str) -> Option<u64> {
    smaps_rollup
        .lines()
        .find(|line| line.starts_with("Pss:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|num| num.parse().ok())
}

#[cfg(target_os = "linux")]
fn parse_statm(statm: &str) -> Option<u64> {
    let rss: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(rss * 4)
}

/// Every process in the trees rooted at `roots`: its parent, so trees can
/// be walked, and its CPU ticks, so they need not be read a second time. A
/// housekeeping pass reads this once and hands it to everything that walks
/// a tree - memory, CPU, harness detection.
///
/// Walking down from the roots through `/proc/<pid>/task/<tid>/children`
/// reads ~30 files on a normal session; stat-ing all of `/proc` to build a
/// whole-machine parent map read 528 and cost 6.6 ms every pass.
#[derive(Debug, Default)]
pub struct ProcTable(Vec<Proc>);

#[derive(Debug, Clone, Copy)]
struct Proc {
    pid: u32,
    ppid: u32,
    ticks: u64,
}

/// `children` needs `CONFIG_PROC_CHILDREN`. Where the kernel does not have
/// it the file is simply absent, and a downward walk would see every tree
/// as a lone root, so fall back to the whole-machine pass there.
static CHILDREN: LazyLock<bool> = LazyLock::new(|| {
    std::fs::metadata("/proc/self/task")
        .and_then(|_| std::fs::read_dir(format!("/proc/self/task/{}", std::process::id())))
        .and_then(|mut d| {
            d.find_map(|e| e.ok()?.path().join("children").metadata().ok())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        })
        .is_ok()
});

impl ProcTable {
    /// Build a table by reading `/proc`. Entries whose parent is not in the
    /// tree are kept: the table has to know about processes that belong to a
    /// pane but live outside the window (disown, double-fork).
    #[cfg(target_os = "linux")]
    pub fn read(roots: &[u32]) -> Self {
        if !*CHILDREN {
            return Self::read_whole_machine();
        }
        let mut table = ProcTable::default();
        let mut queue: Vec<u32> = roots.to_vec();
        let mut seen = HashSet::new();
        while let Some(pid) = queue.pop() {
            if !seen.insert(pid) {
                continue;
            }
            let Some(ppid) = fs::read_to_string(format!("/proc/{}/stat", pid))
                .ok()
                .and_then(|s| parse_ppid(&s))
            else {
                continue;
            };
            let ticks = fs::read_to_string(format!("/proc/{}/stat", pid))
                .ok()
                .and_then(|s| parse_ticks(&s))
                .unwrap_or(0);
            table.0.push(Proc { pid, ppid, ticks });
            queue.extend(children(pid));
        }
        table
    }

    #[cfg(target_os = "linux")]
    fn read_whole_machine() -> Self {
        let mut table = ProcTable::default();
        let Ok(entries) = fs::read_dir("/proc") else {
            return table;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            let Some(stat) = fs::read_to_string(format!("/proc/{}/stat", pid)).ok() else {
                continue;
            };
            let Some(ppid) = parse_ppid(&stat) else {
                continue;
            };
            let ticks = parse_ticks(&stat).unwrap_or(0);
            table.0.push(Proc { pid, ppid, ticks });
        }
        table
    }

    pub fn descendants(&self, roots: &[u32]) -> Vec<u32> {
        let mut out = Vec::new();
        let mut queue: Vec<u32> = roots.to_vec();
        let mut seen = HashSet::new();
        while let Some(pid) = queue.pop() {
            if !seen.insert(pid) {
                continue;
            }
            out.push(pid);
            for proc in &self.0 {
                if proc.ppid == pid {
                    queue.push(proc.pid);
                }
            }
        }
        out
    }

    pub fn ticks(&self, pid: u32) -> u64 {
        self.0.iter().find(|p| p.pid == pid).map_or(0, |p| p.ticks)
    }

    #[cfg(test)]
    fn from_parents(pairs: &[(u32, u32)]) -> Self {
        let mut table = ProcTable::default();
        for &(pid, ppid) in pairs {
            table.0.push(Proc {
                pid,
                ppid,
                ticks: 0,
            });
        }
        table
    }
}

fn parse_ppid(stat_line: &str) -> Option<u32> {
    // field 2 is (comm), which may contain spaces and ')'; split on last ')'
    let close_paren = stat_line.rfind(')')?;
    let rest = &stat_line[close_paren + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// utime + stime out of a `/proc/pid/stat` line: fields 14 and 15, counted
/// past `(comm)` the same way `parse_ppid` counts past it.
fn parse_ticks(stat_line: &str) -> Option<u64> {
    let rest = &stat_line[stat_line.rfind(')')? + 1..];
    // `rest` starts at field 3, so field 14 is the 12th word here.
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// The pids the kernel lists as children of `pid`, across all its threads.
#[cfg(target_os = "linux")]
fn children(pid: u32) -> Vec<u32> {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .filter_map(|task| fs::read_to_string(task.ok()?.path().join("children")).ok())
        .flat_map(|list| {
            list.split_whitespace()
                .filter_map(|pid| pid.parse().ok())
                .collect::<Vec<u32>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pss() {
        let sample = "\
Rss:                1234 kB
Pss:                5678 kB
Pss_Anon:           3456 kB
Shared_Clean:       2000 kB
";
        assert_eq!(parse_pss(sample), Some(5678));
    }

    #[test]
    fn test_parse_pss_missing() {
        assert_eq!(parse_pss("Rss: 100 kB\n"), None);
    }

    #[test]
    fn test_parse_ppid_normal() {
        let stat = "1234 (bash) S 42 1234 1234 0 -1 4194304";
        assert_eq!(parse_ppid(stat), Some(42));
    }

    #[test]
    fn test_parse_ppid_weird_comm() {
        let stat = "1234 (weird ) name) S 42 1234 1234 0 -1 4194304";
        assert_eq!(parse_ppid(stat), Some(42));
    }

    #[test]
    fn ticks_are_user_plus_system_even_with_a_bracket_in_comm() {
        // pid comm state ppid pgrp session tty tpgid flags minflt cminflt
        // majflt cmajflt utime stime ...
        let stat = "9 (od) name) S 1 9 9 0 -1 4194304 100 0 0 0 70 30 5 5";
        assert_eq!(parse_ticks(stat), Some(100));
        assert_eq!(parse_ticks("9 (sh) S 1 9 9 0 -1 0 0 0 0 0"), None);
    }

    #[test]
    fn cpu_percent_is_of_one_core_and_never_negative() {
        let two_s = std::time::Duration::from_secs(2);
        // A core saturated for both seconds: 200 ticks at USER_HZ 100.
        assert_eq!(cpu_percent(1200, 1000, two_s), 100.0);
        assert_eq!(cpu_percent(1050, 1000, two_s), 25.0);
        // Both edges of a tree whose processes were replaced.
        assert_eq!(cpu_percent(10, 1000, two_s), 0.0);
        assert_eq!(cpu_percent(1200, 1000, std::time::Duration::ZERO), 0.0);
    }

    #[test]
    fn test_descendants_simple() {
        let table = ProcTable::from_parents(&[(2, 1), (3, 2), (4, 1), (5, 99)]);
        let result = table.descendants(&[1]);
        assert!(result.contains(&1));
        assert!(result.contains(&2));
        assert!(result.contains(&3));
        assert!(result.contains(&4));
        assert!(!result.contains(&5));
        assert!(!result.contains(&99));
    }

    #[test]
    fn test_descendants_multiple_roots() {
        let table = ProcTable::from_parents(&[(3, 1), (4, 2), (5, 3)]);
        let result = table.descendants(&[1, 2]);
        // 5 is a child of 3, which is a child of 1
        // each pid counted once even though 1 and 2 are both roots
        assert!(result.contains(&1));
        assert!(result.contains(&2));
        assert!(result.contains(&3));
        assert!(result.contains(&4));
        assert!(result.contains(&5));
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_descendants_cycle() {
        // corrupt/racy /proc might have cycles
        let table = ProcTable::from_parents(&[(2, 1), (3, 2), (1, 3)]); // cycle: 1->2->3->1
        let result = table.descendants(&[1]);
        // must not hang; visited set prevents infinite loop
        assert!(result.contains(&1));
        assert!(result.contains(&2));
        assert!(result.contains(&3));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_human_kib_boundaries() {
        assert_eq!(human_kib(0), "0 KB");
        assert_eq!(human_kib(1023), "1023 KB");
        assert_eq!(human_kib(1024), "1.0 MB");
        assert_eq!(human_kib(9625), "9.4 MB"); // 9625 / 1024 = 9.4
        assert_eq!(human_kib(83968), "82 MB"); // 83968 / 1024 = 82
        assert_eq!(human_kib(1048576), "1.0 GB"); // 1024 * 1024
    }
}
