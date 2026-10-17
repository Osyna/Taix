//! Memory measurement and performance monitoring.
//!
//! Runs on the slow cadence (`MEM_INTERVAL`): PSS walks `/proc` for every pane
//! process tree, CPU is derived from ticks, and the perf popover is fed here.

use super::*;

impl App {
    /// Run the slow housekeeping pass: memory, CPU, logs, reaping, and the
    /// perf monitor. Every step is work that is far too much for a 16 ms
    /// frame: reading `/proc` or forking tmux.
    pub(super) fn housekeeping(&mut self, panes: &[PaneInfo]) {
        if self.last_mem.elapsed() < MEM_INTERVAL {
            return;
        }
        self.last_mem = Instant::now();
        // PSS is the expensive read (`smaps_rollup` per process), so it is
        // skipped when nothing consumes it: the perf popover is closed,
        // memory is not in the bar, and no web client is watching. The tree
        // itself is ~30 small reads and always wanted, for the ports.
        let need_mem = self.w.perf.is_open()
            || self.cfg.bar.iter().any(|s| s == "memory")
            || self.web.clients() > 0;
        let roots: Vec<u32> = self
            .server
            .as_ref()
            .map(|(pid, _)| *pid)
            .into_iter()
            .chain(panes.iter().map(|p| p.pid))
            .collect();
        let table = taix_core::mem::ProcTable::read(&roots);
        // Before anything that needs tmux: a shell job does not, and a
        // hiccup there must not stall the scheduler.
        if self.jobs.tick() {
            self.reload();
        }
        if need_mem {
            self.measure(panes, &table);
        }
        self.flush_logs();
        self.reap_dead_panes(panes);
        self.adopt_running_harnesses(panes, &table);
        self.scan_procs(panes, table);
        self.reap_idle();
        // One `stat` per open directory, so a file an agent just wrote
        // appears without anyone pressing refresh.
        if self.files_open {
            match self.panel_tab() {
                Some(Panel::Files) => self.w.files.poll(),
                // Four `git` processes: only while the panel is the thing
                // being looked at, never behind another window.
                Some(Panel::Git) if self.w.window.is_active() => self.w.git.poll(),
                _ => {}
            }
        }
        // The monitor is a view of the reading just taken, so it repaints
        // here and nowhere else - and only while someone is looking at it.
        if self.w.perf.is_open() {
            self.refresh_perf();
        }
        if self.last_history.elapsed() >= HISTORY_INTERVAL {
            self.last_history = Instant::now();
            self.save_histories();
        }
    }

    /// Push every buffered transcript to disk. Appends are buffered so a
    /// streaming agent costs no syscall per chunk; this is where the bytes
    /// actually land.
    pub fn flush_logs(&mut self) {
        for row in &mut self.rows {
            if let Some(log) = &mut row.log {
                let _ = log.flush();
            }
        }
    }

    /// Measure memory and CPU for every pane. PSS is proportional RSS, so
    /// tmux's shared pages are split across everything that mapped them, and
    /// adding it all up is meaningful. CPU is ticks since last measure.
    ///
    /// The bar, perf monitor and web page serve these readings: all three
    /// must be fast enough that a dashboard can afford to update them, which
    /// is why nothing here touches tmux - a `capture-pane` costs what matters,
    /// not to stop reading them.
    fn measure(&mut self, panes: &[PaneInfo], table: &taix_core::mem::ProcTable) {
        let elapsed = self.measured_at.elapsed();
        self.measured_at = Instant::now();
        let own = taix_core::mem::pss_kib(std::process::id()).unwrap_or(0);
        let server = self.server.as_ref().map(|(pid, _)| *pid);
        self.panes = panes.len();
        let panes: Vec<(AgentId, Option<u32>)> = self
            .rows
            .iter()
            .map(|r| {
                let pid = r
                    .agent
                    .pane
                    .and_then(|p| panes.iter().find(|x| x.id == p).map(|x| x.pid));
                (r.agent.id, pid)
            })
            .collect();
        let roots: Vec<u32> = server
            .into_iter()
            .chain(panes.iter().filter_map(|(_, pid)| *pid))
            .collect();
        let mut each = taix_core::mem::tree_usage_each(&roots, table).into_iter();
        let server_kib = server.and_then(|_| each.next()).map_or(0, |u| u.kib);
        let mut total = 0;
        let mut project_total = 0;
        for (id, pid) in panes {
            let usage = pid.and_then(|_| each.next());
            total += usage.map_or(0, |u| u.kib);
            let selected = self.selected;
            if let Some(row) = self.row_mut(id) {
                let kib = usage.map(|u| u.kib);
                if Some(row.agent.project) == selected {
                    project_total += kib.unwrap_or(0);
                }
                // A window whose pane just appeared has no previous reading,
                // so its first interval would report the process's whole
                // life as if it had happened in two seconds.
                row.cpu = match usage {
                    Some(usage) if row.ticks > 0 => {
                        taix_core::mem::cpu_percent(usage.ticks, row.ticks, elapsed)
                    }
                    _ => 0.0,
                };
                row.ticks = usage.map_or(0, |u| u.ticks);
                row.mem_kib = kib;
                row.card.set_mem(kib);
            }
        }
        // The pane processes are children of the tmux server, so counting
        // both would double them: report the server without its panes.
        self.mem = (own, server_kib.saturating_sub(total), project_total, total);
    }

    /// Fill the perf monitor: one row per project, in sidebar order, so a
    /// project is found in the same place in both.
    ///
    /// Takes `&self` and no readings: everything here was measured on the
    /// memory cadence, which is what makes it safe to call from a hover
    /// handler that may fire while something else holds a borrow.
    pub fn refresh_perf(&self) {
        let rows: Vec<crate::perf::Row> = self
            .projects
            .iter()
            .map(|project| {
                let windows: Vec<&Row> = self
                    .rows
                    .iter()
                    .filter(|r| r.agent.project == project.id)
                    .collect();
                let kinds: Vec<String> = windows
                    .iter()
                    .map(|r| taix_core::by_id(&self.cfg, &r.agent.kind).label)
                    .collect();
                let labels: Vec<&str> = kinds.iter().map(String::as_str).collect();
                crate::perf::Row {
                    name: project.name.clone(),
                    detail: crate::perf::detail(
                        windows.len(),
                        // Panes, not attached emulators: a project that is
                        // not on screen has no emulators by design, and
                        // reporting it as "nothing running" would be a lie.
                        windows.iter().filter(|r| r.agent.pane.is_some()).count(),
                        windows
                            .iter()
                            .filter(|r| r.agent.state.needs_attention())
                            .count(),
                        &labels,
                    ),
                    selected: self.selected == Some(project.id),
                    kib: windows.iter().filter_map(|r| r.mem_kib).sum(),
                    cpu: windows.iter().map(|r| r.cpu).sum(),
                }
            })
            .collect();
        self.w.perf.set(
            &rows,
            &crate::perf::Totals {
                ui_kib: self.mem.0,
                tmux_kib: self.mem.1,
                panes_kib: rows.iter().map(|r| r.kib).sum(),
                cpu: rows.iter().map(|r| r.cpu).sum(),
                panes: self.panes,
                version: self.server.as_ref().map(|(_, v)| v.clone()),
                socket: self.cfg.tmux_socket.clone(),
                uptime: self.launched.elapsed(),
            },
        );
    }

    /// Which windows serve which ports: every pane header, sidebar row and
    /// project, the bar's segment, and the process monitor if it is open.
    fn scan_procs(&mut self, panes: &[PaneInfo], table: taix_core::mem::ProcTable) {
        let pane_pids: Vec<u32> = panes.iter().map(|p| p.pid).collect();
        // Pane descendants only: on a kernel without `children` the table is
        // the whole machine, and its fds are not ours to walk.
        self.ports.update(&table.descendants(&pane_pids));
        for row in &mut self.rows {
            row.pid = row
                .agent
                .pane
                .and_then(|pane| panes.iter().find(|p| p.id == pane))
                .map(|p| p.pid);
            let tree = row
                .pid
                .map_or_else(Vec::new, |pid| table.descendants(&[pid]));
            let addrs: Vec<std::net::SocketAddr> = tree
                .iter()
                .flat_map(|&pid| self.ports.of(pid).iter().copied())
                .collect();
            row.procs = (
                taix_core::procs::ports(&addrs),
                tree.len().saturating_sub(1),
            );
        }
        let mut served: Vec<u16> = self
            .rows
            .iter()
            .flat_map(|r| r.procs.0.iter().copied())
            .collect();
        served.sort_unstable();
        served.dedup();
        self.jobs.ports(&served);
        self.table = table;
        self.paint_procs();
        if let Some(scope) = self.w.procs.open_scope() {
            self.refresh_procs(scope);
        }
    }

    /// Put the last reading on every anchor: pane headers, sidebar rows and
    /// projects, and the bar.
    pub(super) fn paint_procs(&self) {
        let merge = |rows: &mut dyn Iterator<Item = &Row>| {
            let (mut ports, mut procs) = (Vec::new(), 0);
            for row in rows {
                ports.extend_from_slice(&row.procs.0);
                procs += row.procs.1;
            }
            ports.sort_unstable();
            ports.dedup();
            (ports, procs)
        };
        for row in &self.rows {
            row.card.set_procs(&row.procs.0, row.procs.1);
        }
        for card in &self.sidebar {
            let (ports, procs) =
                merge(&mut self.rows.iter().filter(|r| r.agent.project == card.project));
            crate::procs::chip(&card.procs, &ports, procs);
            for chip in &card.windows {
                if let Some(row) = self.rows.iter().find(|r| r.agent.id == chip.id) {
                    crate::procs::chip(&chip.procs, &row.procs.0, row.procs.1);
                }
            }
        }
        let (ports, procs) = merge(&mut self.rows.iter());
        self.w.procs.set_summary(&ports, procs);
    }

    /// Fill the process monitor from the last reading. `&self` for the same
    /// reason as `refresh_perf`: it runs from a hover handler.
    pub fn refresh_procs(&self, scope: crate::procs::Scope) {
        use crate::procs::Scope;
        let windows: Vec<crate::procs::Window> = self
            .projects
            .iter()
            .flat_map(|project| {
                self.rows
                    .iter()
                    .filter(move |r| r.agent.project == project.id)
                    .map(move |r| (project, r))
            })
            .filter(|(_, r)| match scope {
                Scope::All => true,
                Scope::Project(id) => r.agent.project == id,
                Scope::Window(id) => r.agent.id == id,
            })
            .filter_map(|(project, r)| {
                let procs = taix_core::procs::tree(r.pid?, &self.table)
                    .into_iter()
                    .map(|p| {
                        let addrs = self.ports.of(p.pid).to_vec();
                        (p, addrs)
                    })
                    .collect();
                Some(crate::procs::Window {
                    id: r.agent.id,
                    name: r.agent.name.clone(),
                    project: project.name.clone(),
                    selected: self.selected == Some(project.id),
                    procs,
                })
            })
            .collect();
        self.w.procs.set(scope, &windows);
    }
}
