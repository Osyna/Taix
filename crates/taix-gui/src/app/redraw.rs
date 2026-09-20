//! Redraw and autosize: captures, markup, and reconcile.
//!
//! `redraw` runs at frame rate for focused panes (live emulator) and
//! `SNAPSHOT_INTERVAL` for unfocused ones (tmux capture). `autosize` settles
//! viewport changes before resizing panes. `reconcile_states` derives agent
//! states and notifies on transitions.

use super::*;

impl App {
    /// Resize panes to fit their viewport: the widget's size drives the
    /// terminal, not the other way around. Settles changes before acting,
    /// so a divider drag issues one resize instead of one per pixel.
    ///
    /// Run once per frame: every pane can request this, so batching matters -
    /// one font measure and one `resize-window` per change, instead of a
    /// layout per row and a fork per pane per frame.
    pub(super) fn autosize(&mut self) {
        let now = Instant::now();
        let mut resized: Vec<(AgentId, u32, Size)> = Vec::new();
        // Every pane shares one font, so one Pango measurement serves all of
        // them; a layout per row per frame was the biggest fixed cost of an
        // idle frame.
        let Some((cw, ch)) = self.rows.first().map(|r| ui::cell_size(&r.card.body)) else {
            return;
        };

        let driven = self.web.remote();
        for row in &mut self.rows {
            let Some(window) = row.agent.window else {
                continue;
            };
            let want = if driven.contains(&row.agent.id) {
                // A browser is typing here, so its grid wins - and it may be
                // the only grid there is, since a window in a project this
                // screen is not showing has no widget to measure.
                match row.web_grid {
                    Some(size) => size,
                    None => continue,
                }
            } else {
                // The viewport, never the label: see `AgentCard::viewport`.
                let (w, h) = (row.card.viewport.width(), row.card.viewport.height());
                // Zero while unmapped; resizing to that would be destructive.
                if w <= 1 || h <= 1 {
                    continue;
                }
                // The label's CSS padding is inside the viewport, so subtract
                // it or the last column and row are always cut off.
                let inner_w = f64::from(w) - 2.0 * PANE_PAD_X;
                let inner_h = f64::from(h) - 2.0 * PANE_PAD_Y;
                Size {
                    cols: ((inner_w / cw).floor() as usize).max(20),
                    rows: ((inner_h / ch).floor() as usize).max(4),
                }
            };
            if row.applied_size == Some(want) {
                row.resize_pending_since = None;
                continue;
            }
            match row.resize_pending_since {
                None => row.resize_pending_since = Some(now),
                Some(since) if now.duration_since(since) >= RESIZE_SETTLE => {
                    row.resize_pending_since = None;
                    resized.push((row.agent.id, window, want));
                }
                Some(_) => {}
            }
        }

        for (id, window, size) in resized {
            // Record the size only once tmux accepts it. Marking it applied
            // optimistically would strand the pane at a rejected size forever,
            // since the comparison above would then see no work to do.
            if cmd::resize_window(&self.tmux, window, size.cols, size.rows).is_err() {
                continue;
            }
            if let Some(row) = self.row_mut(id) {
                row.applied_size = Some(size);
                row.dirty = true;
            }
            // The live emulator's grid is fixed at construction, so it has to
            // be rebuilt at the new size or every line wraps at the old width.
            if self.focus == Some(id) {
                self.reseed_focus();
            }
        }
    }

    pub(super) fn redraw(&mut self) {
        let now = Instant::now();
        let mut trace = self.trace_key.take();
        let tmux = &self.tmux;
        // One buffer per frame, not one per pane.
        let mut markup = std::mem::take(&mut self.markup);
        let mut painted = false;

        // O3: Batch unfocused pane captures. Collect panes needing snapshot.
        let to_capture: Vec<(usize, PaneId)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| Some(r.agent.project) == self.selected && r.dirty)
            .filter_map(|(idx, r)| {
                r.agent
                    .pane
                    .filter(|_| {
                        r.live.is_none()
                            && r.scroll == 0
                            && now.duration_since(r.last_snapshot) >= SNAPSHOT_INTERVAL
                    })
                    .map(|pane| (idx, pane))
            })
            .collect();

        let pane_ids: Vec<PaneId> = to_capture.iter().map(|(_, p)| *p).collect();
        let seeds = if !pane_ids.is_empty() {
            cmd::seed_many(tmux, &pane_ids)
        } else {
            Vec::new()
        };

        // Mark captured panes as snapshotted.
        for ((idx, _), seed) in to_capture.iter().zip(&seeds) {
            if seed.is_some() {
                self.rows[*idx].last_snapshot = now;
            }
        }

        // Single pass over rows for rendering.
        for (row_idx, row) in self.rows.iter_mut().enumerate() {
            if Some(row.agent.project) != self.selected {
                continue;
            }
            if !row.dirty {
                continue;
            }
            // A selection lives in the label's text; replacing the markup
            // throws it away, which made selecting anything in a busy pane
            // impossible. Hold the frame until the selection is released.
            // tmux keeps the output either way, so nothing is lost.
            if row.card.body.selection_bounds().is_some() {
                continue;
            }
            let Some(pane) = row.agent.pane else {
                // No pane: the last thing it showed, behind the start mark.
                // Fed through the emulator at the pane's last known grid so
                // colours and wrapping read as they did.
                row.dirty = false;
                markup.clear();
                if let Some(saved) = taix_core::history::load(row.agent.id) {
                    let size = row.applied_size.unwrap_or(Size {
                        cols: 120,
                        rows: 40,
                    });
                    let tail = tail_lines(&saved, size.rows);
                    taix_term::snapshot(size, tail, &mut |chunk| {
                        pango::push(&mut markup, chunk, None)
                    });
                }
                // O1: Skip set_markup if unchanged.
                if markup != row.last_markup {
                    row.card.body.set_markup(&markup);
                    row.last_markup.clear();
                    row.last_markup.push_str(&markup);
                }
                continue;
            };
            markup.clear();
            // Scrolled back: the emulator only holds the live screen, so the
            // view comes from tmux's history instead. `scroll` lines above
            // the bottom means the window `-scroll ..= rows-1-scroll` in
            // tmux's own coordinates, where 0 is the top visible row.
            if row.scroll > 0 {
                row.dirty = false;
                let size = pane_size(tmux, pane);
                let up = row.scroll as i32;
                let captured = cmd::capture_range(tmux, pane, -up, size.rows as i32 - 1 - up, true);
                taix_term::snapshot(size, &captured, &mut |chunk| {
                    pango::push(&mut markup, chunk, None)
                });
                // O1: Skip set_markup if unchanged.
                if markup != row.last_markup {
                    row.card.body.set_markup(&markup);
                    row.last_markup.clear();
                    row.last_markup.push_str(&markup);
                }
                continue;
            }
            match &row.live {
                Some(live) => {
                    row.dirty = false;
                    let block = pango::hex(row.card.body.color());
                    // The pane background as ink under the block, read from
                    // the installed palette so a user `gtk.css` or a pywal
                    // import recolours the cursor along with everything else.
                    // `lookup_color` is deprecated and unreplaced: GTK4 has
                    // no other way to read a colour back from CSS.
                    #[allow(deprecated)]
                    let ink = row
                        .card
                        .body
                        .style_context()
                        .lookup_color("taix_pane_bg")
                        .map(pango::hex)
                        .unwrap_or_else(|| "#000000".into());
                    live.render(&mut |chunk| pango::push(&mut markup, chunk, Some((&block, &ink))));
                    // `TAIX_TRACE=1`: key press to the paint that carries its
                    // echo, the one number typing feel reduces to.
                    if let Some(pressed) = trace.take() {
                        eprintln!(
                            "[trace] key->paint {:.1} ms",
                            pressed.elapsed().as_secs_f64() * 1e3
                        );
                    }
                }
                None => {
                    // O3: Batched capture via seed_many: find this pane's seed.
                    row.dirty = false;
                    if let Some(capture_idx) =
                        to_capture.iter().position(|(idx, _)| *idx == row_idx)
                    {
                        if let Some(Some(seed)) = seeds.get(capture_idx) {
                            let size = Size {
                                cols: seed.cols,
                                rows: seed.rows,
                            };
                            taix_term::snapshot(size, &seed.screen, &mut |chunk| {
                                pango::push(&mut markup, chunk, None)
                            });
                        } else {
                            taix_core::trace!("capture of pane {} failed", pane);
                        }
                    }
                }
            }
            // O1: Skip set_markup if unchanged.
            if markup != row.last_markup {
                row.card.body.set_markup(&markup);
                row.last_markup.clear();
                row.last_markup.push_str(&markup);
            }
            // Only the live pane's own paints throttle its wake-ups: an
            // unfocused snapshot landing just before a keystroke's echo must
            // not push that echo to the next timer tick.
            painted |= row.live.is_some();
        }
        if painted {
            self.last_paint = now;
        }
        self.markup = markup;
        self.trace_key = trace;
    }

    /// Push derived states into the store and notify on the ones a human
    /// must act on. Only transitions notify, so a long Waiting does not spam.
    pub(super) fn reconcile_states(&mut self, panes: &[PaneInfo]) {
        let now = Instant::now();
        let mut updates: Vec<(AgentId, AgentState)> = Vec::new();
        let mut alerts: Vec<(AgentId, String, String, AgentState)> = Vec::new();

        for row in &mut self.rows {
            let derived = match row.agent.pane {
                Some(pane) if panes.iter().any(|p| p.id == pane) => row.watcher.state(now),
                Some(_) => Some(row.watcher.finished()),
                None => None,
            };
            // A plain terminal sitting at a shell prompt is *technically*
            // waiting for input - that is what a prompt is - so deriving
            // "waiting" from one flooded the bar and raised a notification
            // every time the prompt redrew. A prompt is a terminal's resting
            // state, so collapse it here, at the single place state is
            // derived, instead of filtering it everywhere state is read.
            let derived = derived.map(|state| {
                if state.needs_attention() && row.agent.kind == taix_core::terminal().id {
                    AgentState::Idle
                } else {
                    state
                }
            });
            let Some(state) = derived else { continue };
            if state == row.agent.state {
                continue;
            }
            row.agent.state = state;
            row.card.update(
                &row.agent,
                &self.cfg,
                tint(&self.cfg, &row.agent).as_deref(),
            );
            updates.push((row.agent.id, state));
            if state.needs_attention() && row.notified_state != Some(state) {
                alerts.push((
                    row.agent.id,
                    row.agent.name.clone(),
                    row.agent.kind.clone(),
                    state,
                ));
                row.notified_state = Some(state);
            } else if !state.needs_attention() {
                row.notified_state = None;
            }
        }

        if !updates.is_empty() {
            for (id, state) in updates {
                let _ = self.store.set_state(id, state);
            }
            // Our own write, so take the stamp with it. Without this the
            // next reconcile pass reads the file as "somebody else changed
            // it" and does a full reload - which demotes the focused pane
            // and re-seeds it from a capture. A busy window changes state
            // every few hundred milliseconds, so the pane visibly blinked
            // for as long as it was working.
            self.restamp();
            // A state change repaints the sidebar dots and the bar counts.
            self.refresh_sidebar();
            self.refresh_bar();
        }
        for (id, name, kind, state) in alerts {
            self.notify(id, &name, &kind, state);
        }
    }

    /// Raise a notification a human can act on.
    ///
    /// A dashboard you have to watch is worth less than one that pages you,
    /// so the notification carries the button that gets you there - and a
    /// muted project raises nothing at all.
    fn notify(&self, id: AgentId, name: &str, kind: &str, state: AgentState) {
        if self
            .row(id)
            .is_some_and(|r| self.muted.contains(&r.agent.project))
        {
            return;
        }
        let notif = gtk::gio::Notification::new(&match state {
            AgentState::Waiting => format!("{name} needs input"),
            AgentState::Failed => format!("{name} failed"),
            other => format!("{name} is {}", other.as_str()),
        });
        let harness = taix_core::by_id(&self.cfg, kind);
        notif.set_body(Some(&harness.label));
        notif.set_priority(match state {
            AgentState::Failed => gtk::gio::NotificationPriority::High,
            _ => gtk::gio::NotificationPriority::Normal,
        });
        notif.add_button_with_target_value("Focus", "app.focus-agent", Some(&id.to_variant()));
        notif.set_default_action_and_target_value("app.focus-agent", Some(&id.to_variant()));
        self.app
            .send_notification(Some(&format!("taix-agent-{name}")), &notif);
    }
}
