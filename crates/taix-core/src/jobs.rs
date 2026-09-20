//! Scheduled jobs: when they fire, and what happens when they do.
//!
//! Three things live here because they are one feature and share a file's
//! worth of code: the schedule grammar, the executor, and the systemd user
//! timer that ticks the executor while no front is open.
//!
//! Nothing here runs inside a front. Both fronts spawn `taix jobs run-due`
//! as a child, which is the same code path the timer calls, so a job behaves
//! identically with the window open and with it shut.
//!
//! Local civil time comes from `libc::localtime_r`/`mktime` rather than a
//! date crate: the system tzdata already knows about DST, and "every weekday
//! at 09:00" means *my* 09:00.

use crate::{Action, Config, Job, JobId, Project, Store, by_id, session, spawn};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use taix_tmux::cmd;

// ---------------------------------------------------------------- schedule

#[derive(Debug)]
pub enum Schedule {
    /// Fixed interval from the previous run, in seconds. Minimum 60.
    Every(u64),
    Cron(Cron),
    /// One-shot: unix timestamp. Fires once, then the job is disabled.
    At(i64),
}

/// A 5-field cron expression, as bitmasks. Bit `n` is value `n`, so
/// `dom` uses bits 1..=31 and bit 0 is never set.
#[derive(Debug)]
pub struct Cron {
    min: u64,
    hour: u32,
    dom: u32,
    mon: u16,
    dow: u8,
    dom_restricted: bool,
    dow_restricted: bool,
}

/// Parse a schedule: `@every 15m`, one of the `@` shorthands, five cron
/// fields, or a date-time for a one-shot job.
pub fn parse(expr: &str) -> Result<Schedule, String> {
    let expr = expr.trim();
    let Some(rest) = expr.strip_prefix('@') else {
        // Try parsing as a date-time first (YYYY-MM-DD HH:MM or similar)
        if let Ok(ts) = parse_datetime(expr) {
            return Ok(Schedule::At(ts));
        }
        return cron(expr).map(Schedule::Cron);
    };
    let (word, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let five = match word {
        "every" => return every(arg.trim()),
        "hourly" => "0 * * * *",
        "daily" | "midnight" => "0 0 * * *",
        "weekly" => "0 0 * * 0",
        "monthly" => "0 0 1 * *",
        "yearly" | "annually" => "0 0 1 1 *",
        _ => return Err(format!("unknown shorthand {expr}")),
    };
    cron(five).map(Schedule::Cron)
}

fn every(arg: &str) -> Result<Schedule, String> {
    let split = arg.find(|c: char| !c.is_ascii_digit()).unwrap_or(arg.len());
    let (digits, unit) = arg.split_at(split);
    let bad = || format!("bad duration '{arg}': try 15m, 2h, 1d");
    let n: u64 = digits.parse().map_err(|_| bad())?;
    let secs = match unit.trim() {
        "" | "s" => n,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86400),
        _ => return Err(bad()),
    };
    // The ticker has minute granularity, so anything shorter would be a lie.
    if secs < 60 {
        return Err("@every needs at least 60s".to_string());
    }
    Ok(Schedule::Every(secs))
}

/// Parse a date-time string like "2026-09-21 09:00" into a unix timestamp.
fn parse_datetime(s: &str) -> Result<i64, String> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 2 {
        return Err(format!("expected 'YYYY-MM-DD HH:MM', got {s}"));
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if date_parts.len() != 3 || time_parts.len() != 2 {
        return Err(format!("expected 'YYYY-MM-DD HH:MM', got {s}"));
    }
    let year: i32 = date_parts[0]
        .parse()
        .map_err(|_| format!("bad year: {}", date_parts[0]))?;
    let month: i32 = date_parts[1]
        .parse()
        .map_err(|_| format!("bad month: {}", date_parts[1]))?;
    let day: i32 = date_parts[2]
        .parse()
        .map_err(|_| format!("bad day: {}", date_parts[2]))?;
    let hour: i32 = time_parts[0]
        .parse()
        .map_err(|_| format!("bad hour: {}", time_parts[0]))?;
    let min: i32 = time_parts[1]
        .parse()
        .map_err(|_| format!("bad minute: {}", time_parts[1]))?;

    if !(1..=12).contains(&month) {
        return Err(format!("month must be 1-12, got {month}"));
    }
    if !(1..=31).contains(&day) {
        return Err(format!("day must be 1-31, got {day}"));
    }
    if !(0..=23).contains(&hour) {
        return Err(format!("hour must be 0-23, got {hour}"));
    }
    if !(0..=59).contains(&min) {
        return Err(format!("minute must be 0-59, got {min}"));
    }

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = min;
    tm.tm_sec = 0;
    tm.tm_isdst = -1;

    unix(&mut tm).ok_or_else(|| format!("invalid date/time: {s}"))
}

fn cron(expr: &str) -> Result<Cron, String> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    if f.len() != 5 {
        return Err("expected 5 fields: minute hour day month weekday".to_string());
    }
    let mut dow = field(f[4], 0, 7, "weekday")?;
    // 7 and 0 are both Sunday.
    if dow & (1 << 7) != 0 {
        dow = (dow | 1) & !(1 << 7);
    }
    Ok(Cron {
        min: field(f[0], 0, 59, "minute")?,
        hour: field(f[1], 0, 23, "hour")? as u32,
        dom: field(f[2], 1, 31, "day")? as u32,
        mon: field(f[3], 1, 12, "month")? as u16,
        dow: dow as u8,
        dom_restricted: f[2] != "*",
        dow_restricted: f[4] != "*",
    })
}

/// One comma-separated field as a bitmask. Items are `*`, `*/step`, `n`,
/// `n/step`, `a-b` or `a-b/step`. Month and weekday names are recognized.
fn field(spec: &str, lo: u64, hi: u64, name: &str) -> Result<u64, String> {
    let mut mask = 0u64;
    for item in spec.split(',') {
        let item = item.trim();
        let bad = || format!("bad {name}: {item}");
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => (r.trim(), s.trim().parse::<u64>().map_err(|_| bad())?),
            None => (item, 1),
        };
        if step == 0 {
            return Err(bad());
        }
        let (from, to) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            let from_val = parse_field_value(a.trim(), name)?;
            let to_val = parse_field_value(b.trim(), name)?;
            (from_val, to_val)
        } else {
            let n = parse_field_value(range, name)?;
            // `n/step` counts from n upwards, as cron has always done.
            if item.contains('/') { (n, hi) } else { (n, n) }
        };
        if from < lo || to > hi || from > to {
            return Err(bad());
        }
        let mut v = from;
        while v <= to {
            mask |= 1 << v;
            v += step;
        }
    }
    if mask == 0 {
        return Err(format!("bad {name}: {spec}"));
    }
    Ok(mask)
}

/// A field value: a number, or a month/weekday name written either in full
/// or as its first three letters, in any case.
fn parse_field_value(s: &str, field_name: &str) -> Result<u64, String> {
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    const DAYS: [&str; 7] = [
        "sunday",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
    ];
    let names: &[&str] = match field_name {
        "month" => &MONTHS,
        "weekday" => &DAYS,
        _ => &[],
    };
    let lower = s.to_ascii_lowercase();
    let found = names
        .iter()
        .position(|name| *name == lower || (lower.len() == 3 && name.starts_with(&lower)));
    match found {
        // Months are 1-based; weekdays count Sunday as zero.
        Some(i) if field_name == "month" => Ok(i as u64 + 1),
        Some(i) => Ok(i as u64),
        None => Err(format!("not a number or known {field_name} name: {s}")),
    }
}

impl Schedule {
    /// The first slot strictly after `after`, or `None` for an expression
    /// nothing can satisfy (`0 0 30 2 *`).
    pub fn next_after(&self, after: i64) -> Option<i64> {
        match self {
            Schedule::Every(secs) => after.checked_add(*secs as i64),
            Schedule::Cron(c) => c.next_after(after),
            Schedule::At(ts) => (*ts > after).then_some(*ts),
        }
    }

    pub fn next_runs(&self, from: i64, n: usize) -> Vec<i64> {
        let mut out = Vec::with_capacity(n);
        let mut t = from;
        for _ in 0..n {
            let Some(next) = self.next_after(t) else {
                break;
            };
            out.push(next);
            t = next;
        }
        out
    }
}

impl Cron {
    fn next_after(&self, after: i64) -> Option<i64> {
        // Start at the next whole minute: cron has no seconds.
        let mut t = after.div_euclid(60).checked_add(1)?.checked_mul(60)?;
        for _ in 0..400_000 {
            let mut tm = local(t);
            let stepped = if !self.month_ok(&tm) || !self.day_ok(&tm) {
                tm.tm_mday += 1;
                tm.tm_hour = 0;
                tm.tm_min = 0;
                true
            } else if !self.hour_ok(&tm) {
                tm.tm_hour += 1;
                tm.tm_min = 0;
                true
            } else if !self.min_ok(&tm) {
                tm.tm_min += 1;
                true
            } else {
                false
            };
            tm.tm_sec = 0;
            let resolved = unix(&mut tm)?;
            if !stepped {
                // A DST fall-back maps two civil times to one instant and
                // `mktime` answers with the first; taking it is correct, and
                // the slot it already passed is simply skipped.
                if resolved > after {
                    return Some(resolved);
                }
                t += 60;
                continue;
            }
            // Never go backwards, whatever the zone did.
            t = if resolved > t { resolved } else { t + 60 };
        }
        None
    }

    fn month_ok(&self, tm: &libc::tm) -> bool {
        self.mon & (1u16 << (tm.tm_mon as u16 + 1)) != 0
    }

    fn hour_ok(&self, tm: &libc::tm) -> bool {
        self.hour & (1u32 << tm.tm_hour) != 0
    }

    fn min_ok(&self, tm: &libc::tm) -> bool {
        self.min & (1u64 << tm.tm_min) != 0
    }

    /// The standard cron OR rule: with both day fields restricted, either
    /// one matching is a match.
    fn day_ok(&self, tm: &libc::tm) -> bool {
        let dom_hit = self.dom & (1u32 << tm.tm_mday) != 0;
        let dow_hit = self.dow & (1u8 << tm.tm_wday) != 0;
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom_hit || dow_hit,
            (true, false) => dom_hit,
            (false, true) => dow_hit,
            (false, false) => true,
        }
    }
}

fn local(ts: i64) -> libc::tm {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = ts as libc::time_t;
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

/// Civil local time back to unix seconds. `tm_isdst = -1` asks the C library
/// to work out whether DST applies, which is the whole reason for using it.
fn unix(tm: &mut libc::tm) -> Option<i64> {
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(tm) };
    (t != -1).then_some(t as i64)
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A timestamp as a human reads it: the weekday inside a week, a date
/// outside one.
pub fn format_local(ts: i64, now: i64) -> String {
    let tm = local(ts);
    let (h, m) = (tm.tm_hour, tm.tm_min);
    if (ts - now).abs() < 7 * 86400 {
        format!("{} {h:02}:{m:02}", DAYS[(tm.tm_wday as usize).min(6)])
    } else {
        let month = MONTHS[(tm.tm_mon as usize).min(11)];
        format!("{:02} {month} {h:02}:{m:02}", tm.tm_mday)
    }
}

// ------------------------------------------------------------------ runner

pub struct Outcome {
    pub job: JobId,
    pub name: String,
    pub ok: bool,
    /// Exit status of a shell job; `None` for the other actions.
    pub exit: Option<i32>,
    pub detail: String,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mark = if self.ok { "ok" } else { "failed" };
        write!(f, "[{}] {} — {mark}: {}", self.job, self.name, self.detail)
    }
}

/// The earliest upcoming slot across every enabled job, or `None` if
/// nothing is scheduled. Pure: the fronts call it on reload and then only
/// compare integers.
pub fn next_due_at(jobs: &[Job], now: i64) -> Option<i64> {
    jobs.iter()
        .filter(|j| j.enabled)
        .filter_map(|j| {
            parse(&j.schedule)
                .ok()?
                .next_after(j.last_run.unwrap_or(j.created))
        })
        .min()
        .map(|next| next.max(now.saturating_sub(1)))
}

/// Claim every due job and run it. Claiming stamps `last_run` under the
/// store's exclusive lock, so two runners racing on the same minute cannot
/// both take the same job.
pub fn run_due(store: &Store, cfg: &Config, now: i64) -> Vec<Outcome> {
    let due = store.claim_due_jobs(now).unwrap_or_default();
    due.iter().map(|job| run(store, cfg, job)).collect()
}

/// Force one job now, whatever its schedule says and whether or not it is
/// enabled. `last_run` still moves, so an `@every` job restarts its interval
/// and a cron job does not immediately re-fire the slot it was forced
/// through.
pub fn run_now(store: &Store, cfg: &Config, id: JobId) -> Outcome {
    let job = match store.job(id) {
        Ok(Some(job)) => job,
        Ok(None) => {
            return Outcome {
                job: id,
                name: String::new(),
                ok: false,
                exit: None,
                detail: format!("no job {id}"),
            };
        }
        Err(e) => {
            return Outcome {
                job: id,
                name: String::new(),
                ok: false,
                exit: None,
                detail: e.to_string(),
            };
        }
    };
    let mut claimed = job.clone();
    let at = now();
    claimed.last_run = Some(at);
    let _ = store.stamp_run(id, at);
    run(store, cfg, &claimed)
}

fn run(store: &Store, cfg: &Config, job: &Job) -> Outcome {
    let start = Instant::now();
    let outcome = execute(store, cfg, job);
    let ms = start.elapsed().as_millis() as u64;
    let _ = store.record_run(
        job.id,
        outcome.exit,
        ms,
        (!outcome.ok).then_some(outcome.detail.as_str()),
    );
    fire_notification(job, &outcome);
    outcome
}

#[cfg(target_os = "linux")]
fn fire_notification(job: &Job, outcome: &Outcome) {
    use crate::Notify;
    let should_notify = match job.notify {
        Notify::Never => false,
        Notify::Failure => !outcome.ok,
        Notify::Always => true,
    };
    if !should_notify {
        return;
    }

    let summary = format!("TaiX: {}", job.name);
    let urgency = if outcome.ok { "normal" } else { "critical" };
    if let Err(e) = Command::new("notify-send")
        .arg(&summary)
        .arg(&outcome.detail)
        .arg("--urgency")
        .arg(urgency)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        crate::trace!("notify-send failed: {e}");
    }
}

/// them. A project that is not a repository, or a remote that refuses, is
/// reported and stepped over: one broken checkout must not silence the
/// fetch of every other.
fn git_action(store: &Store, job: &Job) -> Result<String, String> {
    let all = store.projects().map_err(|e| e.to_string())?;
    let projects: Vec<_> = match job.project {
        Some(id) => all.into_iter().filter(|p| p.id == id).collect(),
        None => all,
    };
    if projects.is_empty() {
        return Err("no project to sync".to_string());
    }

    let mut lines = Vec::new();
    let mut failed = false;
    for project in &projects {
        let result = taix_git::Repo::open(&project.root)
            .map_err(|e| e.to_string())
            .and_then(|repo| {
                match job.command.as_str() {
                    "fetch" => repo
                        .fetch(&project.root, false)
                        .map(|()| "fetched".to_string()),
                    "pull" => repo.pull(&project.root),
                    "push" => repo.push(&project.root, false, false),
                    other => Err(taix_git::Error::Git {
                        stderr: format!("unknown git operation '{other}'"),
                    }),
                }
                .map_err(|e| e.to_string())
            });
        match result {
            Ok(said) => lines.push(format!("{}: {}", project.name, said.trim())),
            Err(e) => {
                failed = true;
                lines.push(format!("{}: {}", project.name, e.trim()));
            }
        }
    }
    let report = lines.join(" · ");
    if failed { Err(report) } else { Ok(report) }
}

fn execute(store: &Store, cfg: &Config, job: &Job) -> Outcome {
    let stamp = now();
    let mut outcome = Outcome {
        job: job.id,
        name: job.name.clone(),
        ok: false,
        exit: None,
        detail: String::new(),
    };
    if let Err(e) = job.validate() {
        outcome.detail = e;
        return outcome;
    }
    let mut log = open_log(cfg, job, stamp).ok();
    let project = job.project.and_then(|id| {
        store
            .projects()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.id == id)
    });

    match job.action {
        Action::Shell => {
            let command = expand(&job.command, project.as_ref(), stamp);
            let cwd = job
                .cwd
                .clone()
                .or_else(|| project.as_ref().map(|p| p.root.clone()))
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/"));
            let (ok, exit, detail) = shell(job, &command, &cwd, log.as_ref());
            outcome.ok = ok;
            outcome.exit = exit;
            outcome.detail = detail;
        }
        Action::Agent => match agent(store, cfg, job, project.as_ref(), stamp) {
            Ok(detail) => {
                outcome.ok = true;
                outcome.detail = detail;
            }
            Err(e) => outcome.detail = e,
        },
        Action::Session => match restore(store, cfg, job) {
            Ok(detail) => {
                outcome.ok = true;
                outcome.detail = detail;
            }
            Err(e) => outcome.detail = e,
        },
        Action::Git => match git_action(store, job) {
            Ok(detail) => {
                outcome.ok = true;
                outcome.detail = detail;
            }
            Err(e) => outcome.detail = e,
        },
    }
    if let Some(file) = log.as_mut() {
        let _ = writeln!(file, "--- {}", outcome.detail);
    }
    outcome
}

/// `{project}`, `{root}`, `{date}` and `{time}`. Anything else in braces is
/// left verbatim: a shell command is full of braces that are not ours.
fn expand(text: &str, project: Option<&Project>, now: i64) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    let tm = local(now);
    let out = text
        .replace("{project}", project.map_or("", |p| p.name.as_str()))
        .replace(
            "{root}",
            &project.map_or(String::new(), |p| p.root.display().to_string()),
        );
    out.replace(
        "{date}",
        &format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        ),
    )
    .replace("{time}", &format!("{:02}:{:02}", tm.tm_hour, tm.tm_min))
}

fn shell(
    job: &Job,
    command: &str,
    cwd: &std::path::Path,
    log: Option<&File>,
) -> (bool, Option<i32>, String) {
    let mut spawn = Command::new("sh");
    spawn.arg("-c").arg(command).current_dir(cwd);
    // `sh -c` reads no rc file; the PATH it inherits is the one `main`
    // adopted, or the one the unit bakes in.
    spawn.stdin(Stdio::null());
    if let Some((out, err)) = log.and_then(|f| Some((f.try_clone().ok()?, f.try_clone().ok()?))) {
        spawn.stdout(out).stderr(err);
    }
    let mut child = match spawn.spawn() {
        Ok(child) => child,
        Err(e) => {
            return (
                false,
                None,
                format!("cannot run it in {}: {e}", cwd.display()),
            );
        }
    };
    // Without a ceiling one hung command leaks a process on every tick.
    let limit = job.timeout_secs.unwrap_or(900);
    let deadline = Instant::now() + Duration::from_secs(limit);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let detail = match status.code() {
                    Some(0) => "exit 0".to_string(),
                    Some(code) => format!("exit {code}"),
                    None => "killed by a signal".to_string(),
                };
                return (status.success(), status.code(), detail);
            }
            Ok(None) => {}
            Err(e) => return (false, None, format!("cannot wait for it: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return (false, None, format!("timed out after {limit}s"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// How long to wait for a freshly opened harness to show a prompt. Typing
/// before it appears loses the text.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

fn agent(
    store: &Store,
    cfg: &Config,
    job: &Job,
    project: Option<&Project>,
    stamp: i64,
) -> Result<String, String> {
    let project = project.ok_or("its project is gone")?;
    cmd::ensure_session(&cfg.tmux_socket, &cfg.tmux_session, 240, 60)
        .map_err(|e| format!("tmux: {e}"))?;
    let agents = store.agents(project.id).map_err(|e| e.to_string())?;

    let live = job.reuse.then(|| {
        agents.iter().find(|a| {
            a.kind == job.harness
                && a.pane
                    .is_some_and(|pane| cmd::pane_alive(&cfg.tmux_socket, pane))
        })
    });
    let (pane, mut detail) = match live.flatten() {
        Some(agent) => (
            agent.pane.unwrap_or_default(),
            format!("{} in {}", agent.name, project.name),
        ),
        None => {
            let harness = by_id(cfg, &job.harness);
            let spawned = spawn::window(store, cfg, project, &harness, &agents, None)?;
            let mut detail = format!(
                "opened {} in {} (window {})",
                harness.label, project.name, spawned.window
            );
            if !wait_ready(cfg, spawned.pane) {
                detail.push_str(" (sent before the prompt appeared)");
            }
            (spawned.pane, detail)
        }
    };

    let prompt = expand(&job.prompt, Some(project), stamp);
    if prompt.is_empty() {
        return Ok(detail);
    }
    cmd::send_text(&cfg.tmux_socket, pane, &prompt).map_err(|e| format!("tmux: {e}"))?;
    cmd::send_key(&cfg.tmux_socket, pane, "Enter").map_err(|e| format!("tmux: {e}"))?;
    detail.push_str(&format!(", sent {} chars", prompt.chars().count()));
    Ok(detail)
}

fn wait_ready(cfg: &Config, pane: u32) -> bool {
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if cmd::capture_text(&cfg.tmux_socket, pane, 40)
            .is_ok_and(|text| crate::watcher::looks_like_prompt(text.as_bytes()))
        {
            return true;
        }
    }
    false
}

fn restore(store: &Store, cfg: &Config, job: &Job) -> Result<String, String> {
    let snap = session::load(cfg, &job.session).map_err(|e| e.to_string())?;
    cmd::ensure_session(&cfg.tmux_socket, &cfg.tmux_session, 240, 60)
        .map_err(|e| format!("tmux: {e}"))?;
    let opened = spawn::restore(store, cfg, &snap)?;
    Ok(format!("restored {opened} window(s)"))
}

// --------------------------------------------------------------------- log

const LOG_ROTATE: u64 = 1024 * 1024;

pub fn log_path(cfg: &Config, id: JobId) -> PathBuf {
    crate::log::dir(cfg).join("jobs").join(format!("{id}.log"))
}

fn open_log(cfg: &Config, job: &Job, now: i64) -> std::io::Result<File> {
    let path = log_path(cfg, job.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_ROTATE) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "\n=== {} — {} ===", format_local(now, now), job.name)?;
    Ok(file)
}

/// The last `lines` lines of a job's log, for the UI.
pub fn log_tail(cfg: &Config, id: JobId, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(log_path(cfg, id)) else {
        return "nothing logged yet".to_string();
    };
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

// ------------------------------------------------------------- out of band

/// The `taix` binary: this exe if it is `taix`, else a sibling, else PATH.
pub fn taix_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    if let Some(exe) = exe
        .as_ref()
        .filter(|e| e.file_name().is_some_and(|n| n == "taix"))
    {
        return Some(exe.clone());
    }
    let sibling = exe
        .as_deref()
        .and_then(|e| e.parent())
        .map(|dir| dir.join("taix"))
        .filter(|path| path.is_file());
    sibling.or_else(|| crate::which("taix"))
}

pub fn spawn_runner() -> std::io::Result<Child> {
    detached(&["jobs".to_string(), "run-due".to_string()])
}

pub fn spawn_run_now(id: JobId) -> std::io::Result<Child> {
    detached(&["jobs".to_string(), "run".to_string(), id.to_string()])
}

fn detached(args: &[String]) -> std::io::Result<Child> {
    let bin = taix_binary().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "`taix` is not next to this binary or on PATH",
        )
    })?;
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// The front-side half of the scheduler, identical in both fronts: at most
/// one runner child, owned and reaped here so nothing zombies and the UI
/// thread never calls `wait`.
///
/// The gate is a single integer comparison per housekeeping pass; the
/// calendar walk only happens on [`Ticker::rearm`].
#[derive(Default)]
pub struct Ticker {
    next: Option<i64>,
    child: Option<Child>,
    request: Option<JobId>,
}

impl Ticker {
    /// Recompute the gate. Called whenever the store is reloaded.
    pub fn rearm(&mut self, jobs: &[Job], now: i64) {
        self.next = next_due_at(jobs, now);
    }

    /// Run this job on the next pass, whatever its schedule says.
    pub fn request(&mut self, id: JobId) {
        self.request = Some(id);
    }

    /// One housekeeping pass. `true` means a runner just finished and the
    /// caller should reload to pick up `last_run` and `last_exit`.
    pub fn tick(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            if matches!(child.try_wait(), Ok(None)) {
                return false;
            }
            self.child = None;
            return true;
        }
        if let Some(id) = self.request.take() {
            self.child = spawn_run_now(id).ok();
        } else if self.next.is_some_and(|t| t <= now()) {
            match spawn_runner() {
                Ok(child) => {
                    self.child = Some(child);
                    self.next = None;
                }
                // Do not retry in a tight loop when `taix` cannot be found.
                Err(_) => self.next = Some(now() + 60),
            }
        }
        false
    }
}

// ------------------------------------------------------------- systemd

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Enabled,
    Disabled,
    Unavailable,
}

const SERVICE: &str = "taix-jobs.service";
const TIMER: &str = "taix-jobs.timer";

#[cfg(target_os = "linux")]
fn unit_dir() -> PathBuf {
    crate::config::base_dir(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        ".config",
    )
    .join("systemd")
    .join("user")
}

/// The answer changes only when someone installs or uninstalls the timer,
/// and the web front end asks for it on every poll. The TTL is for a
/// `systemctl enable` typed in a terminal, which nothing here can see.
#[cfg(target_os = "linux")]
static STATUS: Mutex<Option<(Instant, Background)>> = Mutex::new(None);
#[cfg(target_os = "linux")]
const STATUS_TTL: Duration = Duration::from_secs(30);

#[cfg(target_os = "linux")]
fn invalidate_background_status() {
    *STATUS.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

#[cfg(target_os = "linux")]
pub fn background_status() -> Background {
    let mut cached = STATUS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, status)) = *cached
        && at.elapsed() < STATUS_TTL
    {
        return status;
    }
    let status = read_background_status();
    *cached = Some((Instant::now(), status));
    status
}

/// Asking `systemctl is-enabled` costs a process, and the GUI used to ask
/// ninety times a second. An enabled user timer is a symlink in
/// `timers.target.wants`, which is one `lstat` to check.
#[cfg(target_os = "linux")]
fn read_background_status() -> Background {
    if crate::which("systemctl").is_none() {
        return Background::Unavailable;
    }
    let dir = unit_dir();
    if std::fs::symlink_metadata(dir.join(TIMER)).is_err() {
        return Background::Disabled;
    }
    if std::fs::symlink_metadata(dir.join("timers.target.wants").join(TIMER)).is_ok() {
        Background::Enabled
    } else {
        Background::Disabled
    }
}

/// The `PATH` the unit runs with: what the installed unit says, else what
/// installing right now would bake in.
#[cfg(target_os = "linux")]
pub fn background_path() -> String {
    std::fs::read_to_string(unit_dir().join(SERVICE))
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("Environment=PATH=").map(str::to_string))
        })
        .unwrap_or_else(|| crate::harness::shell_path().to_string())
}

/// Install and start the minute ticker. `Ok(Some(warning))` means it is
/// running but will not survive logout.
#[cfg(target_os = "linux")]
pub fn background_install() -> Result<Option<String>, String> {
    let taix = taix_binary();
    if crate::which("systemctl").is_none() {
        let path = taix.unwrap_or_else(|| PathBuf::from("taix"));
        return Err(format!(
            "no systemctl; add this crontab line instead: * * * * * {} jobs run-due >/dev/null 2>&1",
            path.display()
        ));
    }
    let dir = unit_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let service_body = format!(
        "[Unit]\n\
         Description=TaiX scheduled jobs\n\n\
         [Service]\n\
         Type=oneshot\n\
         Environment=PATH={}\n\
         ExecStart={} jobs run-due\n",
        crate::harness::shell_path(),
        taix.unwrap_or_else(|| PathBuf::from("taix")).display()
    );
    let timer_body = "[Unit]\n\
         Description=TaiX scheduled jobs ticker\n\n\
         [Timer]\n\
         OnCalendar=*:0/1\n\n\
         [Install]\n\
         WantedBy=timers.target\n"
        .to_string();
    write_unit(&dir.join(SERVICE), &service_body)?;
    write_unit(&dir.join(TIMER), &timer_body)?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", TIMER])?;
    systemctl(&["start", TIMER])?;
    invalidate_background_status();
    if lingering() {
        Ok(None)
    } else {
        Ok(Some(
            "running, but will stop on logout; run `loginctl enable-linger` to keep it".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
pub fn background_uninstall() -> Result<(), String> {
    systemctl(&["stop", TIMER]).ok();
    systemctl(&["disable", TIMER]).ok();
    let dir = unit_dir();
    std::fs::remove_file(dir.join(SERVICE)).ok();
    std::fs::remove_file(dir.join(TIMER)).ok();
    systemctl(&["daemon-reload"]).ok();
    invalidate_background_status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_unit(path: &std::path::Path, body: &str) -> Result<(), String> {
    std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Does this user's session survive logout? Unknown counts as yes: a
/// warning nobody can act on is noise.
#[cfg(target_os = "linux")]
fn lingering() -> bool {
    Command::new("loginctl")
        .args(["show-user", "--property=Linger"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .is_some_and(|text| text.trim() == "Linger=yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NewJob;

    fn next(expr: &str, after: i64) -> i64 {
        parse(expr).unwrap().next_after(after).unwrap()
    }

    #[test]
    fn every_counts_from_the_last_run() {
        assert_eq!(next("@every 900", 1000), 1900);
        assert_eq!(next("@every 15m", 1000), 1900);
        assert_eq!(next("@every 2h", 0), 7200);
        assert_eq!(next("@every 1d", 0), 86400);
    }

    #[test]
    fn rejects_what_it_cannot_run() {
        assert!(parse("* * * *").is_err());
        assert!(parse("60 * * * *").is_err());
        assert!(parse("* 24 * * *").is_err());
        assert!(parse("@every 30s").is_err());
        assert!(parse("@sometimes").is_err());
        assert!(parse("*/0 * * * *").is_err());
        assert!(parse("0 0 30 2 *").unwrap().next_after(0).is_none());
    }

    /// `TZ` is process-wide and this test moves it. Every other test that
    /// converts a local time has to take the same lock: being the only
    /// writer does not help a reader running in another thread.
    #[test]
    fn civil_time_rules() {
        let _env = crate::testenv::lock();
        unsafe extern "C" {
            fn tzset();
        }
        let set_tz = |zone: &str| unsafe {
            std::env::set_var("TZ", zone);
            tzset();
        };

        set_tz("UTC");
        // Minute patterns are offset-free — every real UTC offset is a
        // whole number of minutes — but `next_after` still walks local
        // civil time, so they belong under the pinned zone.
        for base in [0, 1, 59, 60, 1_700_000_000, 1_700_000_299] {
            let next = next("*/5 * * * *", base);
            assert_eq!(next % 300, 0, "{base} -> {next}");
            assert!(next > base && next <= base + 300, "{base} -> {next}");
        }
        assert_eq!(next("* * * * *", 1_700_000_000), 1_700_000_040);

        // 2024-01-01 00:00:00 UTC was a Monday.
        let monday = 1_704_067_200;
        assert_eq!(next("0 9 * * *", monday), monday + 9 * 3600);
        // Friday 09:00 -> Monday 09:00, skipping the weekend.
        let friday_nine = monday + 4 * 86400 + 9 * 3600;
        assert_eq!(next("0 9 * * 1-5", friday_nine), friday_nine + 3 * 86400);
        // 7 is Sunday too.
        assert_eq!(next("0 0 * * 7", monday), monday + 6 * 86400);
        // Day-of-month and weekday both restricted: either one matches.
        let both = next("0 0 3 * 5", monday);
        assert_eq!(both, monday + 2 * 86400, "the 3rd comes before Friday");
        assert_eq!(next("0 0 3 * 5", both), monday + 4 * 86400, "then Friday");
        // Month is AND-ed with the day rule.
        assert_eq!(next("0 0 1 3 *", monday), 1_709_251_200);
        assert_eq!(format_local(monday + 9 * 3600, monday), "Mon 09:00");
        assert_eq!(format_local(monday + 40 * 86400, monday), "10 Feb 00:00");

        // A daily job at a civil time the clock skips over must still fire
        // once that day and then move on, rather than spinning.
        set_tz("Europe/Berlin");
        let sunday = 1_711_843_200; // 2024-03-31 00:00 UTC; 02:00 CET never happens
        let first = next("30 2 * * *", sunday);
        let second = next("30 2 * * *", first);
        assert!(first > sunday && second > first);
        assert!(second - first <= 2 * 86400, "{first} -> {second}");

        // Local 09:00 is a moving target: 08:00 UTC in winter, 07:00 in
        // summer. Getting this wrong is what a UTC-only scheduler does.
        assert_eq!(next("0 9 * * *", 1_711_843_200) % 86400, 7 * 3600);
        assert_eq!(next("0 9 * * *", 1_704_067_200) % 86400, 8 * 3600);
        set_tz("UTC");
    }

    #[test]
    fn expansion_leaves_unknown_braces_alone() {
        let project = Project {
            id: 1,
            name: "osyna".to_string(),
            root: PathBuf::from("/tmp/osyna"),
        };
        let out = expand("cd {root} && echo {project} ${OTHER}", Some(&project), 0);
        assert_eq!(out, "cd /tmp/osyna && echo osyna ${OTHER}");
    }

    #[test]
    fn month_and_weekday_names() {
        // `0 9 * * mon-fri` is what people write. Masks, not timestamps: the
        // civil-time walk has its own test, and asserting a wall-clock
        // instant here would make this depend on the process's timezone -
        // which another test in this file changes underneath it.
        assert_eq!(cron("0 9 * * mon-fri").unwrap().dow, 0b0011_1110);
        assert_eq!(cron("0 0 1 jan,jul *").unwrap().mon, (1 << 1) | (1 << 7));
        assert_eq!(cron("0 0 1 january *").unwrap().mon, 1 << 1);
        assert_eq!(cron("0 9 * * SUNDAY").unwrap().dow, 1);
        assert!(cron("0 9 * * funday").is_err());
    }

    #[test]
    fn one_shot_fires_once() {
        let _env = crate::testenv::lock();
        let store = Store::open_memory().unwrap();
        let proj = store
            .add_project("test", std::path::Path::new("/tmp/test"))
            .unwrap();
        let future = now() + 60; // 60 seconds in future to ensure we're past created time
        // Parse the future timestamp into YYYY-MM-DD HH:MM format
        let tm = local(future);
        let date_str = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        );

        // Verify the schedule parses correctly
        let parsed = parse(&date_str).unwrap();
        match parsed {
            Schedule::At(ts) => {
                assert!(
                    ts >= future - 60 && ts <= future + 60,
                    "parsed timestamp {} should be near {}",
                    ts,
                    future
                );
            }
            _ => panic!("Expected At variant, got {:?}", parsed),
        }

        let job = store
            .add_job(&NewJob {
                name: "once".into(),
                enabled: true,
                project: Some(proj.id),
                schedule: date_str,
                action: crate::Action::Shell,
            })
            .unwrap();

        // First claim: job fires
        let due = store.claim_due_jobs(future + 1).unwrap();
        assert_eq!(due.len(), 1, "one-shot job should fire once");
        assert_eq!(due[0].id, job.id);

        // Job is now disabled
        let retrieved = store.job(job.id).unwrap().unwrap();
        assert!(
            !retrieved.enabled,
            "one-shot job should be disabled after firing"
        );

        // Second claim: nothing fires
        let due2 = store.claim_due_jobs(future + 100).unwrap();
        assert_eq!(due2.len(), 0, "disabled job should not fire again");
    }
}
