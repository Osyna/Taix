# TaiX

TaiX orchestrates AI coding agents on a single tmux server. Each project can run multiple agent harnesses in separate windows, with their output captured as terminal emulator state. Three front ends share the same tmux session: a GTK desktop window, a terminal UI, and a phone/LAN web interface.

## Install and run

Build the terminal and TUI front ends:

```bash
cargo build --release
```

This produces `target/release/taix`, the CLI and TUI.

Build the GTK desktop window:

```bash
cargo build --release -p taix --features gui
```

This produces `target/release/taix-gui`, the desktop window.

Run the TUI (default):

```bash
taix
```

Run the desktop:

```bash
taix --gui
```

Config lives at `$XDG_CONFIG_HOME/taix/config.toml` (or `~/.config/taix/config.toml`). State and the database live at `$XDG_DATA_HOME/taix/` (or `~/.local/share/taix/`).

## The desktop

The desktop window shows projects in a sidebar and their agent windows as panes. The focused pane is a live terminal emulator; unfocused panes are refreshed from tmux captures. Right-click a project or window for its menu. Click a pane to focus it and type into it.

### Keyboard shortcuts

All shortcuts use `Ctrl+Shift`; the focused pane receives the rest of the keyboard.

| Key | Action |
|-----|--------|
| `Ctrl+Shift+N` | New window |
| `Ctrl+Shift+T` | Toggle files panel |
| `Ctrl+Shift+G` | Toggle git panel |
| `Ctrl+Shift+B` | Toggle browser panel (with `--features browser`) |
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+Shift+F` | Find in pane |
| `Ctrl+Shift+R` | Rename focused window |
| `Ctrl+Shift+W` | Close focused window |
| `Ctrl+Shift+K` | Stop focused window (offers to discard worktree) |
| `Ctrl+Shift+E` | Fold/unfold selected project |
| `Ctrl+Shift+Z` | Zoom focused pane |
| `Ctrl+Shift+M` | Mute/unmute selected project |
| `Ctrl+Shift+S` | Save session |
| `Ctrl+Shift+O` | Open saved session |
| `Ctrl+Shift+J` | Automation (scheduled jobs) |
| `Ctrl+Shift+V` | Paste (images become file paths) |
| `Ctrl+Shift+C` | Copy selection |
| `Ctrl+Shift++` | Increase pane font size |
| `Ctrl+Shift+-` | Decrease pane font size |
| `Ctrl+Shift+0` | Reset pane font size |
| `Ctrl+Shift+1` to `9` | Focus nth window of current project |
| `Ctrl+Shift+Left` | Previous project |
| `Ctrl+Shift+Right` | Next project |
| `Ctrl+Shift+Up` | Scroll pane history up one line |
| `Ctrl+Shift+Down` | Scroll pane history down one line |
| `Ctrl+Tab` | Cycle focus to next window |
| `Shift+Page Up` | Scroll pane history up one page |
| `Shift+Page Down` | Scroll pane history down one page |

## The web front end

Enable the web server in Settings or set `web.enabled = true` in `config.toml`. The server starts on port 4040 by default. A chip appears in the status bar showing the LAN address.

### Pairing

When a device without a pairing cookie requests a page, the desktop shows an amber pairing chip beside the web chip. Click the chip to see pending requests. Tap Pair to allow a device or Deny to refuse it. Approved devices receive a cookie valid for their IP address.

The pairing key is stored at `$XDG_DATA_HOME/taix/web.key` (mode 0600) and persists across restarts. Rotate the key from the pairing popover to invalidate every paired device.

### Tailscale

Enable Tailscale-only mode in Settings or set `web.tailscale = true` in `config.toml`. The server binds to the tailnet IPv4 address instead of all interfaces, so only devices on your tailnet can reach it.

To serve TaiX over HTTPS via `tailscale serve`, the server must already be running. Tailscale will proxy HTTPS requests to the local HTTP port.

## Scheduled jobs

Scheduled jobs run shell commands, open agent harnesses with prompts, restore saved sessions, or run git operations. Jobs fire while a TaiX window is open. To run jobs when TaiX is closed, install the systemd user timer.

### Commands

```bash
taix jobs                                  # list all jobs
taix jobs add <name> --at <when> ...       # create a job (see below)
taix jobs rm <id|name>                     # delete a job
taix jobs on <id|name>                     # enable a job
taix jobs off <id|name>                    # disable a job
taix jobs run <id|name>                    # run a job now
taix jobs log <id|name> [-n <lines>]       # show job output (default 200 lines)
taix jobs history [<id|name>] [-n <runs>]  # show run history (default all jobs, 20 runs)
taix jobs run-due                          # run due jobs (called by timer)
taix jobs install                          # install systemd user timer
taix jobs uninstall                        # remove systemd user timer
```

### Schedule syntax

Use `--at <when>` with one of:

- `@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`
- `@every 15m`, `@every 2h`, `@every 1h30m` (minutes and hours)
- Five cron fields: `min hour day month weekday`
- One-shot: `YYYY-MM-DD HH:MM` (fires once, then the job is disabled)

Cron fields accept `*`, `*/n`, `a-b`, `a,b`, and `n`. Month and weekday names are recognized:
- Months: `jan`, `feb`, ..., `dec` (or full names: `january`, `february`, ...)
- Weekdays: `sun`, `mon`, ..., `sat` (or full names: `sunday`, `monday`, ...)

Examples:

```bash
--at "@every 30m"
--at "@daily"
--at "0 9 * * mon-fri"     # 09:00 on weekdays
--at "*/15 * * * *"        # every 15 minutes
--at "0 0 1 jan,jul *"     # midnight on Jan 1 and Jul 1
--at "2026-12-25 09:00"    # one-shot: Christmas morning 2026
```

### Job actions

One of:

- `--run <command>` — run a shell command
- `--agent <harness> --ask <prompt>` — open an agent and type a prompt
- `--session <name>` — restore a saved session
- `--git <op>` — run `fetch`, `pull`, or `push`

### Options

- `--project <p>` — project id, name, or path (required for `--agent`, optional for `--git`)
- `--cwd <dir>` — working directory for `--run` (defaults to project root)
- `--timeout <secs>` — kill command after this many seconds (default 900)
- `--notify <when>` — `never`, `failure` (default), or `always`
- `--reuse` — reuse an existing window for `--agent` instead of opening a new one
- `--no-catch-up` — skip this job if it is overdue when TaiX starts

### Expansion

`{project}`, `{root}`, `{date}`, and `{time}` are expanded in `--run` commands and `--ask` prompts.

### Background timer

Jobs run only while a TaiX window is open unless the systemd user timer is installed:

```bash
taix jobs install
```

The timer wakes every minute and runs `taix jobs run-due`. To remove it:

```bash
taix jobs uninstall
```

## Config reference

All keys go in `$XDG_CONFIG_HOME/taix/config.toml` (or `~/.config/taix/config.toml`).

| Key | Default | Description |
|-----|---------|-------------|
| `tmux_socket` | `"taix"` | tmux socket name (the `-L` argument) |
| `tmux_session` | `"taix"` | tmux session name |
| `idle_after_ms` | `2000` | milliseconds before an agent is marked idle |
| `theme` | none | theme name (omit for system default) |
| `font_size` | `10.0` | pane font size in points |
| `browser_home` | `"about:blank"` | browser panel landing page |
| `editor` | none | editor command or catalogue id (`code`, `nvim`, etc.); falls back to `$VISUAL`, `$EDITOR` |
| `isolate` | `false` | give every new window its own git worktree on a branch |
| `bar` | `["where", "branch", "window", "windows", "memory"]` | bottom bar segments (valid: `where`, `branch`, `window`, `windows`, `memory`, `uptime`, `empty`) |
| `reap_idle_after_ms` | none | milliseconds before an idle agent is stopped (omit to never reap) |
| `harness_order` | `[]` | harness ids in menu order; unlisted harnesses follow in catalogue order |
| `web.enabled` | `true` | start the web server |
| `web.port` | `4040` | web server port |
| `web.tailscale` | `false` | bind to tailnet address only |

### Per-harness config

Add or override harnesses in `[agents.<id>]` tables:

```toml
[agents.claude]
label = "Claude"
command = "claude --api-key ..."
icon = "claude"
color = "blue"
hidden = false
```

All fields are optional. Omitted fields keep the catalogue defaults.

## Troubleshooting

Run the health check first:

```bash
taix doctor
```

This checks configuration, the tmux server, and installed harnesses.

TaiX keeps going when something it can live without fails: a capture that
did not answer, a `git` invocation that errored, a request it refused. Set
`TAIX_LOG=1` to have those printed to stderr instead of swallowed.

```bash
TAIX_LOG=1 taix --gui
```

### Common issues

**Port already in use:** Another process is bound to the web server port. Change `web.port` in `config.toml` or stop the conflicting process.

**tmux server gone:** The tmux session was killed outside TaiX. Close the window and reopen it to start a fresh session. TaiX will restore the layout from its state database.

**Agent not found:** The harness command is not on `PATH`. Check `taix harnesses` to see what TaiX detects. If an agent is installed but missing, ensure its directory is in the `PATH` that TaiX sees. The desktop inherits the launcher's `PATH`; run `taix --gui` from a terminal to inherit the shell's `PATH` instead.

## Desktop entry

`packaging/dev.taix.TaiX.desktop` is the launcher entry. Install it with:

```bash
install -Dm644 packaging/dev.taix.TaiX.desktop ~/.local/share/applications/dev.taix.TaiX.desktop
```
