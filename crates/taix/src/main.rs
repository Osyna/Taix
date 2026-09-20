use std::process::ExitCode;
use taix_core::{Config, Store};

fn main() -> ExitCode {
    // Before anything spawns: a window opened from a launcher inherits the
    // desktop session's PATH, which has none of the directories npm, bun,
    // pyenv or cargo install agents into. Every child - the tmux server
    // included - should see what a terminal sees.
    taix_core::adopt_shell_path();

    // Multi-call dispatch: check argv[0] first for symlink behavior
    let argv0 = std::env::args()
        .next()
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
        })
        .unwrap_or_default();

    match argv0.as_str() {
        "taix-tui" => return run_tui(),
        "taix-mcp" => {
            taix_mcp::run();
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // Subcommand dispatch on argv[1]. The terminal front is the default: it
    // needs no display and no GTK, so it is the one that always works. The
    // window is asked for explicitly with `--gui`.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("--version") => {
            println!("taix {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("tui") | None => run_tui(),
        Some("mcp") => {
            taix_mcp::run();
            ExitCode::SUCCESS
        }
        Some("--gui" | "gui") => exec_gui(&args[1..]),
        Some("add") => cli_add(args.get(1)),
        Some("rm") => cli_rm(args.get(1)),
        Some("ls") => cli_ls(),
        Some("save") => cli_save(args.get(1)),
        Some("sessions") => cli_sessions(),
        // Restoring opens windows, and opening a window is shared code now,
        // so this no longer needs a display: it works over SSH and in the
        // build that never linked a UI toolkit.
        Some("restore") => cli_restore(args.get(1)),
        Some("harnesses") => cli_harnesses(),
        Some("doctor") => cli_doctor(),
        Some("jobs") => cli_jobs(&args[1..]),
        Some("-t" | "--terminal") => cli_terminal(&args[1..]),
        Some(cmd) => {
            eprintln!("error: unknown argument '{cmd}'");
            eprintln!("run 'taix --help' for usage");
            ExitCode::FAILURE
        }
    }
}

fn run_tui() -> ExitCode {
    match taix_tui::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Locate the `taix-gui` binary: a sibling of this one first, so a build tree
/// and an installed prefix both work without depending on PATH order, then
/// PATH.
fn gui_binary() -> Option<std::path::PathBuf> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("taix-gui")))
        .filter(|path| path.is_file());
    if sibling.is_some() {
        return sibling;
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("taix-gui"))
            .find(|candidate| candidate.is_file())
    })
}

/// Hand over to the `taix-gui` binary.
///
/// `taix` deliberately links no UI toolkit, because the dynamic linker maps a
/// binary's whole dependency set at startup: carrying GTK in this binary costs
/// 39.1 MB resident to show the *terminal* UI, against 1.1 MB without it.
///
/// `exec` replaces this process rather than forking, so nothing is left
/// waiting around and the window inherits the terminal and exit status.
fn exec_gui(rest: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;

    let Some(program) = gui_binary() else {
        eprintln!("error: `taix-gui` is not installed next to `taix` or on PATH");
        eprintln!("build it with: cargo build --release -p taix --features gui");
        return ExitCode::FAILURE;
    };

    // `exec` only returns on failure.
    let error = std::process::Command::new(&program).args(rest).exec();
    eprintln!("error: cannot start {}: {error}", program.display());
    ExitCode::FAILURE
}

fn print_help() {
    println!(
        "taix — orchestrator for AI coding agents on tmux\n\n\
         Usage:\n\
         \x20 taix                 run the terminal UI (default)\n\
         \x20 taix tui             the same, explicitly\n\
         \x20 taix --gui           open the GTK window\n\
         \x20 taix mcp             run the MCP stdio server\n\
         \x20 taix add <path>      register a directory as a project\n\
         \x20 taix rm <name>       stop its agents and forget the project\n\
         \x20 taix ls              list projects and windows\n\
         \x20 taix save <name>     remember the open projects and windows\n\
         \x20 taix sessions        list what has been saved\n\
         \x20 taix restore <name>  open a saved session again\n\
         \x20 taix harnesses       show which agent CLIs were detected\n\
         \x20 taix doctor          check configuration and runtime health\n\
         \x20 taix -t [-s <id>]    open a project as a plain tmux session here\n\
         \x20 taix --version\n\n\
         `-t`/`--terminal` alone lists projects with their ids; `-s`/`--session <id>`\n\
         takes an id, a name or a path and opens a tmux session with one pane per\n\
         window - same directory, same harness command - arranged like the GUI's\n\
         layout (`-l <tmux layout>` overrides it). Attaches if it is already open.\n\n\
         Config: {}\n\
         Store:  {}",
        Config::default_path().display(),
        Config::store_path().display(),
    );

    println!(
        "\nTUI keys: j/k or Tab focus a window, J/K switch project, 1-9 focus the\n\
         nth, Enter types into the focused pane (Ctrl-] leaves), c sends Ctrl-C,\n\
         n new window, t new terminal, r rename, R restart, x close, X close and\n\
         delete its worktree, p palette, / find, PgUp/PgDn scroll tmux's history,\n\
         Shift-arrows scroll one line, End back to live, z zoom, L cycle layout,\n\
         f fold, m mute, s/o save or open a session, g diff,\n\
         M merge it back, C compare with the previous pane, T transcript path,\n\
         ? keys, q quit."
    );

    // Whether the window is available is a property of what is installed
    // next to us, not of how this binary was compiled.
    if gui_binary().is_some() {
        println!(
            "\nGUI: the focused pane IS the terminal - Tab, Ctrl-C and Escape all\n\
             reach it. TaiX keeps only Ctrl+Shift: P palette, F find in the pane,\n\
             N new window, R rename, W close, K stop (offers to discard a\n\
             worktree), E fold the project, Z zoom the pane,\n\
             M mute this project, S save a session, O open one, 1-9 focus the nth\n\
             window, left/right switch project, V paste (an image becomes a file\n\
             path), C copy the selection, +/-/0 pane font, B the browser panel\n\
             (with `--features browser`). Shift-PgUp/PgDn scroll tmux's history.\n\
             Ctrl-Tab cycles the live pane; click a pane to type into it, and\n\
             right-click a project or a window for its menu - rename, colour,\n\
             restart, interrupt, compare, diff, merge back, close.\n\
             \nWindows name themselves after the agent running in them.\n\
             A `.taix.toml` at a project root carries its own defaults."
        );
    } else {
        println!(
            "\n`taix-gui` is not installed, so `taix --gui` will fail. Add it with:\n\
             \x20 cargo build --release -p taix --features gui"
        );
    }
}

/// Register a project without opening the window. Resolves the git top level,
/// so `taix add .` from any subdirectory registers the repository root.
fn cli_add(path: Option<&String>) -> ExitCode {
    let Some(path) = path else {
        eprintln!("usage: taix add <path>");
        return ExitCode::FAILURE;
    };
    let root = match taix_git::project_root(std::path::Path::new(path)) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    match Store::open(&Config::store_path()).and_then(|s| s.add_project(&name, &root)) {
        Ok(project) => {
            println!("added {} ({})", project.name, project.root.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Forget a project by name or path, stopping its agents' panes first.
/// Worktrees are left on disk; `taix rm` never discards unreviewed work.
fn cli_rm(target: Option<&String>) -> ExitCode {
    let Some(target) = target else {
        eprintln!("usage: taix rm <name|path>");
        return ExitCode::FAILURE;
    };
    let cfg = Config::load();
    let Some(store) = open_store() else {
        return ExitCode::FAILURE;
    };
    let projects = store.projects().unwrap_or_default();
    let found = projects
        .iter()
        .find(|p| p.name == *target || p.root.as_os_str() == std::ffi::OsStr::new(target));
    let Some(project) = found else {
        eprintln!("no project named {target}");
        return ExitCode::FAILURE;
    };
    let socket = &cfg.tmux_socket;
    let result = taix_core::purge_project(&store, project.id, |pane| {
        let _ = taix_tmux::cmd::kill_pane(socket, pane);
    });
    match result {
        Ok(()) => {
            println!("removed {}", project.name);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The store, or the reason it could not be opened - which is now a real
/// message: a state file written by a newer build is refused, and reporting
/// that as "no projects" would look like the projects had been lost.
fn open_store() -> Option<Store> {
    match Store::open(&Config::store_path()) {
        Ok(store) => Some(store),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

fn cli_ls() -> ExitCode {
    let Some(store) = open_store() else {
        return ExitCode::FAILURE;
    };
    let config = Config::load();
    let projects = store.projects().unwrap_or_default();
    if projects.is_empty() {
        println!("no projects; add one with `taix add <path>`");
        return ExitCode::SUCCESS;
    }
    for project in projects {
        println!("{} {}", project.name, project.root.display());
        for agent in store.agents(project.id).unwrap_or_default() {
            let harness = taix_core::by_id(&config, &agent.kind);
            println!(
                "  [{}] {} {} — {}",
                agent.id,
                agent.state.as_str(),
                agent.name,
                harness.label
            );
        }
    }
    ExitCode::SUCCESS
}

/// Save the current projects and windows under a name.
///
/// Reads the store, so it needs no display and no tmux: a snapshot is a
/// description of a working set, not a running one.
fn cli_save(name: Option<&String>) -> ExitCode {
    let Some(name) = name else {
        eprintln!("usage: taix save <name>");
        return ExitCode::FAILURE;
    };
    let cfg = Config::load();
    let Some(store) = open_store() else {
        return ExitCode::FAILURE;
    };
    let snap = match taix_core::spawn::snapshot(&store, name) {
        Ok(snap) => snap,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let windows: usize = snap.projects.iter().map(|p| p.windows.len()).sum();
    match taix_core::session::save(&cfg, &snap) {
        Ok(path) => {
            println!(
                "saved {} project(s), {windows} window(s) to {}",
                snap.projects.len(),
                path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cli_sessions() -> ExitCode {
    let cfg = Config::load();
    let names = taix_core::session::list(&cfg);
    if names.is_empty() {
        println!("no saved sessions — 'taix save <name>' makes one");
        return ExitCode::SUCCESS;
    }
    for name in names {
        match taix_core::session::load(&cfg, &name) {
            Ok(snap) => {
                let windows: usize = snap.projects.iter().map(|p| p.windows.len()).sum();
                println!(
                    "{name}  {} project(s), {windows} window(s)",
                    snap.projects.len()
                );
            }
            Err(_) => println!("{name}  (unreadable)"),
        }
    }
    ExitCode::SUCCESS
}

/// Reopen a saved session: register any missing project, then open its
/// windows in tmux. A project that already has windows is left alone, so
/// restoring twice does not duplicate what is already running.
fn cli_restore(name: Option<&String>) -> ExitCode {
    let Some(name) = name else {
        eprintln!("usage: taix restore <name>");
        eprintln!("run 'taix sessions' to list them");
        return ExitCode::FAILURE;
    };
    let cfg = Config::load();
    let Some(store) = open_store() else {
        return ExitCode::FAILURE;
    };
    let snap = match taix_core::session::load(&cfg, name) {
        Ok(snap) => snap,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("run 'taix sessions' to list them");
            return ExitCode::FAILURE;
        }
    };
    // The windows are opened in tmux, so the server has to exist first.
    if let Err(e) = taix_tmux::cmd::ensure_session(&cfg.tmux_socket, &cfg.tmux_session, 240, 60) {
        eprintln!("error: tmux: {e}");
        return ExitCode::FAILURE;
    }
    match taix_core::spawn::restore(&store, &cfg, &snap) {
        Ok(opened) => {
            println!("{name}: opened {opened} window(s)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Show what the `+` menu will offer here, and why.
///
/// Detection is silent by design, so this is the way to find out whether an
/// agent you installed is actually on the `PATH` TaiX sees.
fn cli_harnesses() -> ExitCode {
    let config = Config::load();
    let available = taix_core::available(&config);

    println!("offered here:");
    for harness in &available {
        let what = match () {
            _ if harness.is_browser() => "embedded web view".to_string(),
            _ if harness.command.is_empty() => "login shell".to_string(),
            _ => harness.command.clone(),
        };
        println!("  {:<22} {:<16} {what}", harness.label, harness.id);
    }

    let missing: Vec<_> = taix_core::catalog()
        .into_iter()
        .filter(|h| !available.iter().any(|a| a.id == h.id))
        .collect();
    if !missing.is_empty() {
        println!("\nknown but not on PATH ({}):", missing.len());
        let names: Vec<&str> = missing.iter().map(|h| h.label.as_str()).collect();
        println!("  {}", names.join(", "));
    }
    println!(
        "\nAdd your own under `[agents.<id>]` in {}",
        Config::default_path().display()
    );
    ExitCode::SUCCESS
}

/// Run configuration and runtime health checks. Exits non-zero when any check
/// fails, so scripts can gate on it.
fn cli_doctor() -> ExitCode {
    let cfg = Config::load();
    let checks = taix_core::doctor::checks(&cfg);

    let max_len = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);

    let mut any_failed = false;
    for check in &checks {
        let mark = if check.ok { "ok" } else { "!!" };
        any_failed |= !check.ok;
        println!(
            "{mark} {:<width$} {}",
            check.name,
            check.detail,
            width = max_len
        );
    }

    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `taix jobs …`: the scheduler's command line, and the entry point the
/// systemd timer calls.
///
/// Flags are parsed by hand, like every other subcommand here: an argument
/// parser would be a dependency for nine verbs.
fn cli_jobs(args: &[String]) -> ExitCode {
    let Some(store) = open_store() else {
        return ExitCode::FAILURE;
    };
    let cfg = Config::load();
    match args.first().map(String::as_str) {
        None => jobs_list(&store),
        Some("add") => jobs_add(&store, &args[1..]),
        Some("rm") => jobs_one(&store, args.get(1), |store, job| {
            store.remove_job(job.id).map_err(|e| e.to_string())?;
            Ok(format!("removed {}", job.name))
        }),
        Some("on") => jobs_one(&store, args.get(1), |store, job| {
            store
                .set_job_enabled(job.id, true)
                .map_err(|e| e.to_string())?;
            Ok(format!("{} is on", job.name))
        }),
        Some("off") => jobs_one(&store, args.get(1), |store, job| {
            store
                .set_job_enabled(job.id, false)
                .map_err(|e| e.to_string())?;
            Ok(format!("{} is off", job.name))
        }),
        Some("run") => jobs_one(&store, args.get(1), |store, job| {
            let outcome = taix_core::jobs::run_now(store, &cfg, job.id);
            if outcome.ok {
                Ok(outcome.to_string())
            } else {
                Err(outcome.to_string())
            }
        }),
        Some("log") => jobs_log(&store, &cfg, &args[1..]),
        Some("history") => jobs_history(&store, &args[1..]),
        Some("run-due") => jobs_run_due(&store, &cfg),
        Some("install") => match taix_core::jobs::background_install() {
            Ok(warning) => {
                println!("jobs will run every minute while you are logged in");
                if let Some(warning) = warning {
                    println!("note: {warning}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("uninstall") => match taix_core::jobs::background_uninstall() {
            Ok(()) => {
                println!("jobs now run only while a TaiX window is open");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("help" | "--help" | "-h") => {
            jobs_help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown jobs command '{other}'");
            jobs_help();
            ExitCode::FAILURE
        }
    }
}

fn jobs_help() {
    println!(
        "Usage:\n\
         \x20 taix jobs                                  list them\n\
         \x20 taix jobs add <name> --at <when> ...       see below\n\
         \x20 taix jobs rm|on|off <id|name>              forget it, enable it, disable it\n\
         \x20 taix jobs run <id|name>                    run it now, whatever the schedule\n\
         \x20 taix jobs log <id|name> [-n <lines>]       what it printed (default 200)\n\
         \x20 taix jobs history [<id|name>] [-n <runs>]  run history (default all jobs, 20 runs)\n\
         \x20 taix jobs run-due                          run whatever is due; the timer's verb\n\
         \x20 taix jobs install | uninstall              run jobs with TaiX closed\n\n\
         Adding one:\n\
         \x20 --at <when>        `@hourly`, `@daily`, `@every 15m`, cron fields, or `YYYY-MM-DD HH:MM`\n\
         \x20 --run <command>    run it in a shell\n\
         \x20 --agent <harness>  open that agent (needs --project)\n\
         \x20 --ask <prompt>     ... and type this into it\n\
         \x20 --reuse            ... in a window it already has, if there is one\n\
         \x20 --session <name>   restore a saved session\n\
         \x20 --git <op>         fetch, pull or push (project default: all projects)\n\
         \x20 --project <p>      an id, a name or a path\n\
         \x20 --cwd <dir>        where a command runs; defaults to the project root\n\
         \x20 --timeout <secs>   kill a command that outlasts this (default 900)\n\
         \x20 --notify <when>    never, failure (default), or always\n\
         \x20 --no-catch-up      skip this job if it is stale on wake\n\n\
         Cron fields take `*`, `*/n`, `a-b` and `a,b`. Month and weekday names accepted.\n\
         One-shot: `YYYY-MM-DD HH:MM` fires once and then disables the job.\n\
         `{{project}}`, `{{root}}`, `{{date}}` and `{{time}}` are expanded in a command or a prompt."
    );
}

/// Resolve `<id|name>`: an id, then an exact name, then a unique prefix.
fn find_job(jobs: &[taix_core::Job], key: &str) -> Result<taix_core::Job, String> {
    if let Ok(id) = key.parse::<taix_core::JobId>()
        && let Some(job) = jobs.iter().find(|j| j.id == id)
    {
        return Ok(job.clone());
    }
    let lower = key.to_lowercase();
    if let Some(job) = jobs.iter().find(|j| j.name.to_lowercase() == lower) {
        return Ok(job.clone());
    }
    let hits: Vec<&taix_core::Job> = jobs
        .iter()
        .filter(|j| j.name.to_lowercase().starts_with(&lower))
        .collect();
    match hits.as_slice() {
        [job] => Ok((*job).clone()),
        [] => Err(format!("no job '{key}'; `taix jobs` lists them")),
        many => Err(format!(
            "'{key}' matches {}",
            many.iter()
                .map(|j| j.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The shape every single-job verb shares: resolve, act, print.
fn jobs_one(
    store: &Store,
    key: Option<&String>,
    act: impl FnOnce(&Store, &taix_core::Job) -> Result<String, String>,
) -> ExitCode {
    let Some(key) = key else {
        eprintln!("usage: taix jobs <verb> <id|name>");
        return ExitCode::FAILURE;
    };
    let job = match find_job(&store.jobs().unwrap_or_default(), key) {
        Ok(job) => job,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match act(store, &job) {
        Ok(said) => {
            println!("{said}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn jobs_list(store: &Store) -> ExitCode {
    let jobs = store.jobs().unwrap_or_default();
    if jobs.is_empty() {
        println!("no jobs; add one with `taix jobs add <name> --at @daily --run <command>`");
        return ExitCode::SUCCESS;
    }
    let now = taix_core::jobs::now();
    for job in jobs {
        let next = match taix_core::jobs::parse(&job.schedule) {
            Err(e) => format!("bad schedule: {e}"),
            Ok(_) if !job.enabled => "off".to_string(),
            Ok(schedule) => schedule
                .next_after(job.last_run.unwrap_or(job.created))
                .map_or("no upcoming run".to_string(), |t| {
                    format!("next {}", taix_core::jobs::format_local(t.max(now), now))
                }),
        };
        // The last run already says it failed; a second marker beside it is
        // noise in a column-aligned list.
        let last = match job.last() {
            Some(run) if run.error.is_some() => {
                format!("failed: {}", run.error.as_deref().unwrap_or_default())
            }
            Some(run) => match run.exit {
                Some(0) => format!("exit 0 — {}", taix_core::jobs::format_local(run.at, now)),
                Some(code) => format!(
                    "exit {code} — {}",
                    taix_core::jobs::format_local(run.at, now)
                ),
                None => format!("ran {}", taix_core::jobs::format_local(run.at, now)),
            },
            None => "never run".to_string(),
        };
        println!(
            "[{}] {:<20} {:<18} {:<19} {last}",
            job.id, job.name, job.schedule, next
        );
    }
    ExitCode::SUCCESS
}

fn jobs_add(store: &Store, args: &[String]) -> ExitCode {
    let Some(name) = args.first().filter(|a| !a.starts_with('-')) else {
        eprintln!("usage: taix jobs add <name> --at <when> --run <command>");
        eprintln!("run 'taix jobs help' for the rest");
        return ExitCode::FAILURE;
    };
    let mut schedule = None;
    let (mut command, mut harness, mut prompt, mut session, mut git_op) =
        (None, None, None, None, None);
    let (mut project, mut cwd, mut timeout, mut notify, mut catch_up, mut reuse) =
        (None, None, None, None, true, false);
    let mut it = args[1..].iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        let taken = match arg.as_str() {
            "--at" | "--when" => value().map(|v| schedule = Some(v)),
            "--run" | "--command" => value().map(|v| command = Some(v)),
            "--agent" => value().map(|v| harness = Some(v)),
            "--ask" | "--prompt" => value().map(|v| prompt = Some(v)),
            "--session" => value().map(|v| session = Some(v)),
            "--git" => value().map(|v| git_op = Some(v)),
            "--project" => value().map(|v| project = Some(v)),
            "--cwd" => value().map(|v| cwd = Some(v)),
            "--timeout" => value().map(|v| timeout = Some(v)),
            "--notify" => value().map(|v| notify = Some(v)),
            "--no-catch-up" => {
                catch_up = false;
                Ok(())
            }
            "--reuse" => {
                reuse = true;
                Ok(())
            }
            other => Err(format!("unknown flag '{other}'")),
        };
        if let Err(e) = taken {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }

    let Some(schedule) = schedule else {
        eprintln!("error: --at <when> is required, e.g. --at @daily");
        return ExitCode::FAILURE;
    };
    let parsed = match taix_core::jobs::parse(&schedule) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let action = match (&command, &harness, &session, &git_op) {
        (Some(_), None, None, None) => taix_core::Action::Shell,
        (None, Some(_), None, None) => taix_core::Action::Agent,
        (None, None, Some(_), None) => taix_core::Action::Session,
        (None, None, None, Some(op)) => {
            command = Some(op.clone());
            taix_core::Action::Git
        }
        (None, None, None, None) => {
            eprintln!("error: one of --run, --agent, --session or --git is required");
            return ExitCode::FAILURE;
        }
        _ => {
            eprintln!("error: --run, --agent, --session and --git are alternatives");
            return ExitCode::FAILURE;
        }
    };
    let project = match project {
        None => None,
        Some(key) => match taix_core::terminal::find(store, &key) {
            Some(project) => Some(project.id),
            None => {
                eprintln!("error: no project '{key}'; `taix ls` lists them");
                return ExitCode::FAILURE;
            }
        },
    };
    let timeout = match timeout.as_deref().map(str::parse::<u64>) {
        None => None,
        Some(Ok(secs)) => Some(secs),
        Some(Err(e)) => {
            eprintln!("error: --timeout: {e}");
            return ExitCode::FAILURE;
        }
    };
    let notify = match notify.as_deref() {
        None => taix_core::Notify::default(),
        Some("never") => taix_core::Notify::Never,
        Some("failure") => taix_core::Notify::Failure,
        Some("always") => taix_core::Notify::Always,
        Some(other) => {
            eprintln!("error: --notify must be never, failure or always, not '{other}'");
            return ExitCode::FAILURE;
        }
    };

    let job = match store.add_job(&taix_core::NewJob {
        name: name.clone(),
        enabled: true,
        project,
        schedule: schedule.clone(),
        action,
    }) {
        Ok(job) => job,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut job = taix_core::Job {
        command: command.unwrap_or_default(),
        harness: harness.unwrap_or_default(),
        prompt: prompt.unwrap_or_default(),
        session: session.unwrap_or_default(),
        reuse,
        cwd: cwd.map(std::path::PathBuf::from),
        timeout_secs: timeout,
        notify,
        catch_up,
        ..job
    };
    job.name = name.clone();
    if let Err(e) = job.validate() {
        let _ = store.remove_job(job.id);
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = store.put_job(&job) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    let now = taix_core::jobs::now();
    let upcoming: Vec<String> = parsed
        .next_runs(now, 3)
        .into_iter()
        .map(|t| taix_core::jobs::format_local(t, now))
        .collect();
    println!("[{}] {name} runs {}", job.id, upcoming.join(", then "));
    if taix_core::jobs::background_status() != taix_core::jobs::Background::Enabled {
        println!("note: jobs only run while a TaiX window is open; `taix jobs install` fixes that");
    }
    ExitCode::SUCCESS
}

fn jobs_log(store: &Store, cfg: &Config, args: &[String]) -> ExitCode {
    let mut lines = 200;
    let mut key = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-n" | "--lines" => match it.next().map(|v| v.parse::<usize>()) {
                Some(Ok(n)) => lines = n,
                _ => {
                    eprintln!("error: -n needs a number");
                    return ExitCode::FAILURE;
                }
            },
            other => key = Some(other.to_string()),
        }
    }
    jobs_one(store, key.as_ref(), |_, job| {
        Ok(taix_core::jobs::log_tail(cfg, job.id, lines))
    })
}

fn jobs_history(store: &Store, args: &[String]) -> ExitCode {
    let mut max_runs = 20;
    let mut key = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-n" | "--runs" => match it.next().map(|v| v.parse::<usize>()) {
                Some(Ok(n)) => max_runs = n,
                _ => {
                    eprintln!("error: -n needs a number");
                    return ExitCode::FAILURE;
                }
            },
            other => key = Some(other.to_string()),
        }
    }

    let now = taix_core::jobs::now();
    if let Some(key) = &key {
        return jobs_one(store, Some(key), |_, job| {
            if job.runs.is_empty() {
                return Ok(format!("{} has no runs yet", job.name));
            }
            Ok(recent(job, max_runs, now, ""))
        });
    }
    let jobs = store.jobs().unwrap_or_default();
    for job in jobs.iter().filter(|j| !j.runs.is_empty()) {
        println!("[{}] {}", job.id, job.name);
        println!("{}", recent(job, max_runs, now, "  "));
    }
    ExitCode::SUCCESS
}

/// A job's last `max` runs, oldest first, one line each.
fn recent(job: &taix_core::Job, max: usize, now: i64, indent: &str) -> String {
    let start = job.runs.len().saturating_sub(max);
    job.runs[start..]
        .iter()
        .map(|run| {
            // An action that is not a process has no exit code; "ok" is
            // what happened, and "no exit" only reads as a fault.
            let outcome = match (run.exit, &run.error) {
                (_, Some(e)) => format!("failed: {e}"),
                (Some(code), None) => format!("exit {code}"),
                (None, None) => "ok".to_string(),
            };
            let dur = if run.ms < 1000 {
                format!("{}ms", run.ms)
            } else {
                format!("{:.1}s", run.ms as f64 / 1000.0)
            };
            format!(
                "{indent}{}  {dur:>8}  {outcome}",
                taix_core::jobs::format_local(run.at, now)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the timer calls. A failing job is not a failing runner: only an
/// unusable store is an error exit.
fn jobs_run_due(store: &Store, cfg: &Config) -> ExitCode {
    for outcome in taix_core::jobs::run_due(store, cfg, taix_core::jobs::now()) {
        println!("{outcome}");
    }
    ExitCode::SUCCESS
}

/// `taix -t [-s <id>] [-l <layout>]`: the project as a tmux session in this
/// terminal, nothing of TaiX left in front of it.
///
/// Execs `sh -c` with a tmux line: attach if the session exists, else build
/// it with one pane per window. Without `-s` it lists the ids, because
/// nobody remembers eight hex digits.
fn cli_terminal(args: &[String]) -> ExitCode {
    let mut session = None;
    let mut layout = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-s" | "--session" => session = it.next(),
            "-l" | "--layout" => layout = it.next(),
            other => {
                eprintln!("error: unknown argument '{other}'");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(store) = open_store() else {
        return ExitCode::FAILURE;
    };
    let Some(id) = session else {
        let projects = store.projects().unwrap_or_default();
        if projects.is_empty() {
            println!("no projects; add one with `taix add <path>`");
        }
        for p in &projects {
            println!(
                "{}  {:<24} {}",
                taix_core::terminal::key(p),
                p.name,
                p.root.display()
            );
        }
        println!("\nopen one with `taix -t -s <id>`");
        return ExitCode::SUCCESS;
    };
    let Some(project) = taix_core::terminal::find(&store, id) else {
        eprintln!("no project '{id}'; `taix -t` lists them");
        return ExitCode::FAILURE;
    };
    let config = Config::load();
    let agents = store.agents(project.id).unwrap_or_default();
    let preset = taix_core::terminal::saved_preset();
    let layout = layout
        .map(String::as_str)
        .unwrap_or_else(|| taix_core::terminal::layout_for_preset(&preset));
    let inside = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let line = taix_core::terminal::tmux_command(&config, &project, &agents, layout, inside);
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("sh").arg("-c").arg(&line).exec();
    eprintln!("cannot exec sh: {err}");
    ExitCode::FAILURE
}
