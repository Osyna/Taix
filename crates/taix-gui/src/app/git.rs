//! Git worker and worktree operations.
//!
//! Runs on `GIT_INTERVAL`: branch status for the bar, worktree lines for each
//! window, diff dialogs, and merge-back. The worker thread keeps git reads off
//! the main thread.

use super::*;

impl App {
    /// Read the bar's branch status and a line per worktree window on a
    /// worker thread: each is a `git` process, and paying for several of
    /// them on the GTK thread is a visible stutter every four seconds.
    ///
    /// One request is in flight at a time; a tick that lands while the
    /// worker is out is skipped rather than queued, so a slow repository
    /// cannot stack up forks.
    pub(super) fn refresh_git(&mut self) {
        if self.git_worker_busy.get() {
            return;
        }
        let bar_root = self
            .selected
            .and_then(|id| self.project(id))
            .map(|p| p.root.clone());
        let rows: Vec<(AgentId, PathBuf, PathBuf)> = self
            .rows
            .iter()
            .filter_map(|r| {
                let worktree = r.agent.worktree.clone()?;
                let root = self.project(r.agent.project)?.root.clone();
                Some((r.agent.id, root, worktree))
            })
            .collect();
        if bar_root.is_none() && rows.is_empty() {
            return;
        }
        self.git_worker_busy.set(true);
        glib::spawn_future_local(async move {
            let done = gio::spawn_blocking(move || {
                let bar_git =
                    bar_root.and_then(|root| taix_git::status(&root).ok().map(|s| s.line()));
                // Every window of one project shares its repository, and
                // opening one is a `git` process: open per root, not per row.
                let mut repos: HashMap<PathBuf, Option<taix_git::Repo>> = HashMap::new();
                let worktree_git: Vec<(AgentId, Option<String>)> = rows
                    .into_iter()
                    .map(|(id, root, worktree)| {
                        let repo = repos
                            .entry(root.clone())
                            .or_insert_with(|| taix_git::Repo::open(&root).ok());
                        let text = repo
                            .as_ref()
                            .and_then(|repo| repo.worktree_line(&worktree).ok());
                        (id, text)
                    })
                    .collect();
                (bar_git, worktree_git)
            })
            .await;
            // A worker that died still has to hand something back, or the
            // busy flag stays set and git never refreshes again.
            let result = match done {
                Ok(result) => result,
                Err(_) => {
                    taix_core::trace!("git worker died");
                    (None, Vec::new())
                }
            };
            PENDING_GIT_RESULT.with(|cell| *cell.borrow_mut() = Some(result));
            crate::app::wake();
        });
    }

    /// Take whatever the worker left, on the next tick.
    pub(super) fn apply_git_result(&mut self) {
        let Some((bar_git, worktree_git)) =
            PENDING_GIT_RESULT.with(|cell| cell.borrow_mut().take())
        else {
            return;
        };
        self.git_worker_busy.set(false);
        self.bar_git = bar_git;
        for (id, text) in worktree_git {
            if let Some(row) = self.row_mut(id)
                && row.git != text
            {
                row.git = text.clone();
                row.card.set_git(text.as_deref());
            }
        }
    }

    pub(super) fn open_diff(&mut self, id: AgentId) -> Result<(), String> {
        let Some((root, worktree, name)) = self.worktree_of(id) else {
            return Err("no worktree for this window".into());
        };
        let repo = taix_git::Repo::open(&root).map_err(|e| e.to_string())?;
        let base = repo.default_branch().map_err(|e| e.to_string())?;
        let diff = repo
            .diff_text(&worktree, &base, DIFF_CAP)
            .map_err(|e| e.to_string())?;
        if diff.trim().is_empty() {
            ui::toast(&self.w, &format!("{name}: no changes against {base}"));
            return Ok(());
        }
        ui::show_text(
            &self.w.window,
            &format!("{name} against {base}"),
            &unified_markup(&diff),
            false,
        );
        Ok(())
    }

    pub(super) fn merge_worktree(&mut self, id: AgentId) -> Result<(), String> {
        let Some((root, worktree, name)) = self.worktree_of(id) else {
            return Err("no worktree for this window".into());
        };
        let repo = taix_git::Repo::open(&root).map_err(|e| e.to_string())?;
        let base = repo.default_branch().map_err(|e| e.to_string())?;
        match repo.merge_back(&worktree, &base) {
            Ok(()) => ui::toast(&self.w, &format!("{name} merged into {base}")),
            // Refusals are the common case and they are informative: a dirty
            // worktree means "commit first", not "this failed".
            Err(e) => ui::show_text(
                &self.w.window,
                &format!("{name}: not merged"),
                &pango::escape(&e.to_string()),
                true,
            ),
        }
        Ok(())
    }

    fn worktree_of(&self, id: AgentId) -> Option<(PathBuf, PathBuf, String)> {
        let row = self.row(id)?;
        let worktree = row.agent.worktree.clone()?;
        let root = self.project(row.agent.project)?.root.clone();
        Some((root, worktree, row.agent.name.clone()))
    }

    /// Diff one window's visible output against the live pane's. Running four
    /// agents on one task is only useful if their answers can be compared.
    pub(super) fn compare_with_live(&mut self, id: AgentId) -> Result<(), String> {
        // The live pane, unless this *is* it - then the one focused before it.
        let other = match self.focus {
            Some(focus) if focus != id => Some(focus),
            _ => self.previous.filter(|p| *p != id),
        };
        let Some(live) = other.filter(|p| self.row(*p).is_some()) else {
            return Err("no other pane to compare with".into());
        };
        let text = |agent: AgentId| -> (String, String) {
            let row = self.row(agent);
            let name = row.map(|r| r.agent.name.clone()).unwrap_or_default();
            let body = row
                .and_then(|r| r.agent.pane)
                .and_then(|pane| cmd::capture_text(&self.tmux, pane, 0).ok())
                .unwrap_or_default();
            (name, body)
        };
        let (left_name, left) = text(id);
        let (right_name, right) = text(live);
        let pair = crate::compare::diff_markup(&left, &right);
        crate::compare::open(&self.w.window, &left_name, &right_name, &pair);
        Ok(())
    }

    pub(super) fn copy_transcript(&mut self, id: AgentId) -> Result<(), String> {
        let path = self
            .row(id)
            .and_then(|r| r.log.as_ref().map(|l| l.path().to_path_buf()))
            .ok_or("no transcript for this window")?;
        if !path.exists() {
            return Err("no transcript file".into());
        }
        let display = path.display().to_string();
        self.w.window.clipboard().set_text(&pango::escape(&display));
        ui::toast(&self.w, &format!("copied {display}"));
        Ok(())
    }
}
