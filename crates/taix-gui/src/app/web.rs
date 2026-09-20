//! Web front end: snapshot building, screens, commands, and notifications.
//!
//! Runs on `WEB_INTERVAL` for publishing, `SNAPSHOT_INTERVAL` for unfocused
//! panes. Browsers watch, claim terminals, and send commands.

use super::*;

impl App {
    /// Take one terminal back for this screen. Announced, because the web
    /// side just lost it mid-sentence.
    pub(super) fn take_keyboard(&mut self, id: AgentId) {
        if self.web.reclaim(id) {
            let name = self.window_name(id);
            ui::set_status(&self.w, &format!("This computer has {name} back"));
            self.withdraw_claim(id);
            self.sidebar_dirty = true;
        }
    }

    /// Say out loud that a browser has one of this screen's terminals.
    ///
    /// Without this the desktop simply stops typing into that pane: the
    /// keys go nowhere, the bar's chip changes colour, and nobody watching
    /// the editor notices. It carries the button that ends it, which runs
    /// the same code as clicking the pane.
    pub(super) fn notify_claim(&self, id: AgentId, name: &str) {
        if self
            .row(id)
            .is_some_and(|r| self.muted.contains(&r.agent.project))
        {
            return;
        }
        let notif = gtk::gio::Notification::new(&format!("{name}: a browser is typing"));
        notif.set_body(Some(
            "A device on the network has this terminal. This screen's keys \
             do nothing in it until you take it back.",
        ));
        notif.add_button_with_target_value(
            "Take it back",
            "app.focus-agent",
            Some(&id.to_variant()),
        );
        notif.set_default_action_and_target_value("app.focus-agent", Some(&id.to_variant()));
        self.app.send_notification(Some(&claim_tag(id)), &notif);
    }

    /// The keyboard came back, so the notification that said it was gone
    /// goes too rather than sitting in the tray as a lie.
    fn withdraw_claim(&self, id: AgentId) {
        self.app.withdraw_notification(&claim_tag(id));
    }

    /// Ask about a device the bar is already showing, because nobody is
    /// looking at the bar. Mute is per project; a pairing request is about
    /// the machine, so it is never muted.
    pub(super) fn notify_pair(&self, ip: &str, agent: &str) {
        let notif = gtk::gio::Notification::new(&format!("{agent} wants to pair"));
        notif.set_body(Some(&format!(
            "From {ip}. Nobody on this network gets in until you say so."
        )));
        notif.add_button_with_target_value("Pair", "app.pair-allow", Some(&ip.to_variant()));
        notif.add_button_with_target_value("Deny", "app.pair-deny", Some(&ip.to_variant()));
        self.app.send_notification(Some(&pair_tag(ip)), &notif);
    }

    /// Withdraw the pair request notification when the request is gone.
    fn withdraw_pair(&self, ip: &str) {
        self.app.withdraw_notification(&pair_tag(ip));
    }

    pub(super) fn refresh_web_chip(&mut self) {
        let health = self.web.health();
        let driving = self.web.remote().len();
        let pending = self.web.pending();
        let ips: String = pending
            .iter()
            .map(|r| r.ip.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let stamp = (health.life, health.clients, driving, ips);
        if self.web.painted == Some(stamp.clone()) {
            return;
        }
        self.w.web.set(&health, health.clients, driving);
        self.w.pair.set(&pending);

        // A request that is answered or has aged out takes its notification
        // with it: a tray card offering to pair a device that stopped asking
        // is a lie waiting to be clicked.
        for req in &pending {
            if !self.notified_pairs.contains(&req.ip) {
                self.notify_pair(&req.ip, &req.agent);
                self.notified_pairs.push(req.ip.clone());
            }
        }
        let gone: Vec<String> = self
            .notified_pairs
            .iter()
            .filter(|ip| !pending.iter().any(|r| r.ip == **ip))
            .cloned()
            .collect();
        for ip in gone {
            self.withdraw_pair(&ip);
            self.notified_pairs.retain(|seen| *seen != ip);
        }
    }

    /// Apply what the browsers asked for, on the same frame and through the
    /// same code as the pointer requests: a tap on a phone and a click on
    /// this screen must not be able to mean two different things.
    pub(super) fn drain_web(&mut self) {
        let cmds = match self.web.hub() {
            Some(hub) => hub.take_cmds(),
            None => return,
        };
        for cmd in cmds {
            // Input for a terminal nobody claimed is dropped in silence:
            // the page already draws that pane as one it is not driving.
            if let Some(window) = cmd.drives()
                && !self.web.holds(window)
            {
                continue;
            }
            if let Err(e) = self.apply_web(cmd) {
                ui::set_status(&self.w, &e);
            }
        }
    }

    fn apply_web(&mut self, cmd: WebCmd) -> Result<(), String> {
        match cmd {
            WebCmd::Takeover { window } => {
                if self.web.claim(window) {
                    let name = self.window_name(window);
                    ui::set_status(&self.w, &format!("A browser is driving {name}"));
                    self.notify_claim(window, &name);
                    self.sidebar_dirty = true;
                }
            }
            WebCmd::Release { window } => {
                let given_back = match window {
                    Some(id) => {
                        if self.web.reclaim(id) {
                            vec![id]
                        } else {
                            Default::default()
                        }
                    }
                    None => {
                        let held = self.web.remote();
                        self.web.reclaim_all();
                        held
                    }
                };
                if !given_back.is_empty() {
                    // The notification said the keyboard was gone; it is
                    // back, so the notification goes with it rather than
                    // sitting in the tray as a lie.
                    for id in given_back {
                        self.withdraw_claim(id);
                    }
                    self.sidebar_dirty = true;
                }
            }
            // Answered inside the web server; it never reaches this queue.
            WebCmd::Watch { .. } => {}
            WebCmd::Keys { window, text } => self.send_keys(window, &keys::Press::Text(text)),
            WebCmd::Key { window, name } => {
                // A tmux key name, not a shell word, but it still goes on a
                // command line: anything outside the shape tmux spells keys
                // in is a bug or an attempt, and neither gets sent.
                if name.len() <= 12
                    && !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    self.send_keys(window, &keys::Press::Named(name));
                }
            }
            WebCmd::Paste { window, text } => {
                // Capped, on a char boundary: a slice mid-codepoint panics.
                let mut end = text.len().min(256 * 1024);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                self.paste_text(window, &text[..end]);
            }
            WebCmd::Type { window, text } => self.send_text(window, &text),
            WebCmd::Mouse {
                window,
                button,
                col,
                row,
                motion,
            } => {
                let kind = match motion.as_str() {
                    "press" => taix_term::Kind::Press,
                    "release" => taix_term::Kind::Release,
                    _ => taix_term::Kind::Motion,
                };
                self.send_mouse(window, button, col, row, kind);
            }
            WebCmd::Scroll { window, delta } => self.scroll_pane(window, delta),
            WebCmd::ScrollBottom { window } => self.scroll_to_bottom(window),
            WebCmd::Grid { window, cols, rows } => {
                // Clamped here rather than trusted: the numbers come off a
                // page, and tmux reflows every line of scrollback to them.
                let want = Size {
                    cols: cols.clamp(20, 500),
                    rows: rows.clamp(4, 200),
                };
                if let Some(row) = self.row_mut(window) {
                    row.web_grid = Some(want);
                }
            }
            // Asked for from a browser, so it opens there: taking this
            // screen to a window someone else created is the bug.
            WebCmd::NewTerminal { project } => {
                let terminal = taix_core::terminal();
                self.spawn_elsewhere(project, &terminal).map(drop)?;
            }
            WebCmd::Spawn { project, harness } => {
                let harness = taix_core::by_id(&self.cfg, &harness);
                self.spawn_elsewhere(project, &harness).map(drop)?;
            }
            WebCmd::AddProject { path } => {
                let path = PathBuf::from(path);
                if !path.is_dir() {
                    return Err(format!("{} is not a directory", path.display()));
                }
                self.add_project(path)?;
            }
            WebCmd::RemoveProject { project } => {
                self.apply_pointer(Pointer::RemoveProject(project))?
            }
            WebCmd::RenameProject { project, name } => {
                self.apply_pointer(Pointer::RenameProject(project, name))?
            }
            WebCmd::CloseAll { project } => self.apply_pointer(Pointer::CloseAll(project))?,
            WebCmd::Close { window } => self.apply_pointer(Pointer::Close(window))?,
            WebCmd::Kill { window } => self.apply_pointer(Pointer::Kill(window))?,
            WebCmd::Interrupt { window } => self.apply_pointer(Pointer::Interrupt(window))?,
            WebCmd::Restart { window } => self.apply_pointer(Pointer::Restart(window))?,
            WebCmd::Start { window } => self.apply_pointer(Pointer::Start(window))?,
            WebCmd::Rename { window, name } => self.apply_pointer(Pointer::Rename(window, name))?,
            WebCmd::Recolor { window, color } => {
                self.apply_pointer(Pointer::Recolor(window, color))?
            }
            WebCmd::TerminalAt { path } => {
                self.apply_pointer(Pointer::TerminalAt(PathBuf::from(path)))?
            }
            WebCmd::OpenFile { path, with } => {
                self.apply_pointer(Pointer::OpenFile(PathBuf::from(path), with))?
            }
            WebCmd::Focus { window } => self.apply_pointer(Pointer::Focus(window))?,
            WebCmd::Config { patch } => {
                let mut cfg = Config::load();
                taix_web::proto::apply_patch(&mut cfg, &patch);
                cfg.save().map_err(|e| e.to_string())?;
                self.config_changed();
            }
            WebCmd::Mute { project } => {
                let name = current_name(self.project(project));
                if let Some(i) = self.muted.iter().position(|p| *p == project) {
                    self.muted.remove(i);
                    ui::set_status(&self.w, &format!("{name}: notifications on"));
                } else {
                    self.muted.push(project);
                    ui::set_status(&self.w, &format!("{name}: muted"));
                }
                self.save_layout();
            }
            WebCmd::Merge { window } => self.merge_worktree(window)?,
            WebCmd::Notice { text } => ui::set_status(&self.w, &text),
        }
        Ok(())
    }

    /// Which panes the web page needs a screen for: what this screen shows,
    /// plus whatever a browser says it is looking at, plus anything a
    /// browser is driving.
    ///
    /// The last two are the whole point: a laptop on the LAN can sit on a
    /// project the desktop is not showing, and those rows have no widget
    /// here, so nothing else would ever capture them.
    fn web_screens(&self) -> Vec<AgentId> {
        let mut ids = self.visible();
        for id in self.web.watched().into_iter().chain(self.web.remote()) {
            if !ids.contains(&id) && self.rows.iter().any(|r| r.agent.id == id) {
                ids.push(id);
            }
        }
        ids
    }

    /// Rebuild the screens the web page draws, at the same cadences the
    /// desktop redraws at: the focused pane from its live emulator, the
    /// rest from a capture on the snapshot interval.
    ///
    /// Only ever called with a browser connected, which is what keeps the
    /// whole feature free for a user who never opens it.
    pub(super) fn refresh_web_screens(&mut self) {
        let now = Instant::now();
        let ids = self.web_screens();
        let tmux = &self.tmux;
        for id in ids {
            let Some(row) = self.rows.iter_mut().find(|r| r.agent.id == id) else {
                continue;
            };
            let Some(pane) = row.agent.pane else {
                row.web_html.clear();
                continue;
            };
            if row.scroll > 0 {
                let size = pane_size(tmux, pane);
                let up = row.scroll as i32;
                let captured = cmd::capture_range(tmux, pane, -up, size.rows as i32 - 1 - up, true);
                row.web_html = taix_web::html::screen(|sink| {
                    taix_term::snapshot(size, &captured, &mut |chunk| sink(chunk))
                });
                row.web_size = size;
                continue;
            }
            match &row.live {
                Some(live) => {
                    row.web_html = taix_web::html::screen(|sink| {
                        live.render(&mut |chunk| sink(chunk));
                    });
                    // The emulator's own grid, which is the widget's, not
                    // tmux's idea of it a frame ago.
                    row.web_size = live.size();
                }
                None => {
                    if now.duration_since(row.web_snap) < SNAPSHOT_INTERVAL {
                        continue;
                    }
                    row.web_snap = now;
                    let Some(seed) = cmd::seed(tmux, pane) else {
                        continue;
                    };
                    let size = Size {
                        cols: seed.cols,
                        rows: seed.rows,
                    };
                    row.web_html = taix_web::html::screen(|sink| {
                        taix_term::snapshot(size, &seed.screen, &mut |chunk| sink(chunk))
                    });
                    // A pane with no widget here is whatever size tmux has
                    // it at: the page draws that, not this screen's layout.
                    row.web_size = size;
                }
            }
        }
    }

    /// Publish the picture the web page draws from. The hub drops it if
    /// nothing changed, so an idle session sends nothing at all.
    pub(super) fn publish_web(&mut self) {
        if self.web.clients() == 0 {
            return;
        }
        if self.last_web_publish.elapsed() < WEB_INTERVAL {
            return;
        }
        self.last_web_publish = Instant::now();
        if self
            .web_slow
            .at
            .is_none_or(|at| at.elapsed() >= SLOW_INTERVAL)
        {
            self.web_slow = WebSlow {
                at: Some(Instant::now()),
                harnesses: self.harness_views(),
                palette: self.web_palette(),
            };
        }
        self.refresh_web_screens();
        let snapshot = self.web_snapshot();
        if let Some(hub) = self.web.hub() {
            hub.publish(snapshot);
            hub.set_roots(self.projects.iter().map(|p| p.root.clone()).collect());
        }
    }

    fn harness_views(&self) -> Vec<taix_web::proto::HarnessView> {
        taix_core::available(&self.cfg)
            .into_iter()
            .filter(|h| !h.is_terminal())
            .map(|h| taix_web::proto::HarnessView {
                id: h.id.clone(),
                label: h.label.clone(),
                icon: h.icon.unwrap_or_else(|| crate::icons::FALLBACK.to_string()),
                color: h.color,
            })
            .collect()
    }

    fn web_snapshot(&self) -> taix_web::proto::Snapshot {
        use taix_web::proto as p;
        let projects = self
            .projects
            .iter()
            .map(|project| p::ProjectView {
                id: project.id,
                name: project.name.clone(),
                path: project.root.to_string_lossy().to_string(),
                folded: self.folded.contains(&project.id),
                windows: self
                    .rows
                    .iter()
                    .filter(|row| row.agent.project == project.id)
                    .map(|row| p::WindowView {
                        id: row.agent.id,
                        name: row.agent.name.clone(),
                        kind: row.agent.kind.clone(),
                        icon: taix_core::by_id(&self.cfg, &row.agent.kind)
                            .icon
                            .unwrap_or_else(|| crate::icons::FALLBACK.to_string()),
                        tint: tint(&self.cfg, &row.agent),
                        state: row.agent.state.as_str().to_string(),
                        alive: row.agent.pane.is_some(),
                        attention: row.agent.state.needs_attention(),
                        branch: row.git.clone(),
                        mem_kib: row.mem_kib,
                        cpu: row.cpu as f32,
                        tmux: row
                            .agent
                            .window
                            .map(|w| taix_core::terminal::attach_command(&self.cfg, w)),
                    })
                    .collect(),
            })
            .collect();
        let panes = self
            .web_screens()
            .into_iter()
            .filter_map(|id| self.row(id))
            .filter(|row| !row.web_html.is_empty())
            .map(|row| p::PaneView {
                id: row.agent.id,
                cols: row.web_size.cols,
                rows: row.web_size.rows,
                scroll: row.scroll,
                mouse: row.card.mouse.get().wanted(),
                html: Some(row.web_html.clone()),
            })
            .collect();
        let uptime = self.launched.elapsed().as_secs();
        let shell = std::env::var("SHELL")
            .ok()
            .and_then(|s| {
                std::path::Path::new(&s)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "sh".to_string());
        let tmux_version = self
            .server
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        p::Snapshot {
            rev: 0,
            lock: p::LockView {
                remote: self.web.remote(),
            },
            host: hostname().to_string(),
            session: p::SessionView {
                uptime,
                shell,
                tmux: tmux_version,
            },
            projects,
            selected: self.selected,
            focus: self.focus,
            zoomed: self.zoomed,
            pane_ids: None,
            panes,
            bar: self.web_bar(),
            harnesses: self.web_slow.harnesses.clone(),
            status: self.w.status.text().to_string(),
            palette: self.web_slow.palette.clone(),
            health: self.web.health(),
        }
    }

    /// The bar, as facts rather than as a layout: the page arranges them
    /// itself, because a phone has room for two of them and this screen has
    /// room for five.
    fn web_bar(&self) -> Vec<taix_web::proto::BarItem> {
        use taix_web::proto::BarItem;
        let project = self.selected.and_then(|id| self.project(id));
        let live = self.rows.iter().filter(|r| r.live.is_some()).count();
        let attention = self
            .rows
            .iter()
            .filter(|r| r.agent.state.needs_attention())
            .count();
        let mut items = vec![BarItem {
            id: "where".to_string(),
            text: match project {
                Some(project) => format!("{} {}", project.name, tilde(&project.root)),
                None => "TaiX".to_string(),
            },
        }];
        if let Some(branch) = &self.bar_git {
            items.push(BarItem {
                id: "branch".to_string(),
                text: branch.clone(),
            });
        }
        items.push(BarItem {
            id: "windows".to_string(),
            text: format!(
                "{} windows · {live} live · {attention} attention",
                self.visible().len()
            ),
        });
        items.push(BarItem {
            id: "memory".to_string(),
            text: format!(
                "ui {} · tmux {} · total {}",
                mib(self.mem.0),
                mib(self.mem.1),
                mib(self.mem.0 + self.mem.1 + self.mem.3)
            ),
        });
        items
    }

    /// The eight window tints as the installed palette resolves them, so a
    /// user on Catppuccin sees Catppuccin in the browser too.
    ///
    /// `lookup_color` is deprecated and unreplaced: GTK4 offers no other
    /// way to read a colour back out of CSS.
    fn web_palette(&self) -> Vec<taix_web::proto::BarItem> {
        use gtk::prelude::*;
        #[allow(deprecated)]
        let context = self.w.window.style_context();
        [
            "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink",
        ]
        .into_iter()
        .filter_map(|name| {
            #[allow(deprecated)]
            let rgba = context.lookup_color(&format!("taix_tint_{name}"))?;
            Some(taix_web::proto::BarItem {
                id: name.to_string(),
                text: pango::hex(rgba),
            })
        })
        .collect()
    }
}
