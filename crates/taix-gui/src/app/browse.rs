//! Browser windows: a project's web views, and the ops agents drive them with.
//!
//! A browser window is an ordinary window row with no tmux pane, so it sits
//! in the sidebar, in the grid and in the web front end's list like any
//! other. What is different is its body: where a terminal window paints a
//! capture, this one holds a live `WebKitWebView`.
//!
//! The views are kept in `App::browsers` rather than in the row, because a
//! row's card is rebuilt whenever the grid is and a page an agent is halfway
//! through driving must survive that - and survive the user switching to
//! another project, which unparents the widget but keeps the process, the
//! cookies and the scroll position.

use super::*;

impl App {
    /// Build the widget a leaf of the layout tree shows. A browser window's
    /// card shows its web view where a terminal shows its screen.
    pub(super) fn card_widget(&self, id: AgentId) -> Option<gtk::Widget> {
        let row = self.row(id)?;
        #[cfg(feature = "browser")]
        let body = self
            .browsers
            .get(&id)
            .map(|browser| browser.root.clone().upcast::<gtk::Widget>());
        #[cfg(not(feature = "browser"))]
        let body: Option<gtk::Widget> = None;
        row.card.set_body_widget(body.as_ref());
        Some(row.card.root.clone().upcast())
    }

    /// The strip above a tab group: one tab per window, the active one
    /// marked. Clicking a tab focuses that window, which is also what makes
    /// it the active one - the two must not be able to disagree.
    pub(super) fn tab_strip(&self, ids: &[AgentId], active: usize) -> gtk::Widget {
        let items: Vec<ui::TabItem> = ids
            .iter()
            .filter_map(|id| self.row(*id))
            .map(|row| {
                let tint = row
                    .agent
                    .color
                    .clone()
                    .or_else(|| taix_core::by_id(&self.cfg, &row.agent.kind).color);
                ui::TabItem {
                    id: row.agent.id,
                    name: row.agent.name.clone(),
                    state: row.agent.state.as_str().to_string(),
                    tint,
                }
            })
            .collect();
        let pending = self.pending.clone();
        let pick = move |id| pending.push(Pointer::Focus(id));
        // Double-click takes a tab out of the group and gives it half the
        // pane: the same move as dropping it on this pane's right edge.
        let pending = self.pending.clone();
        let out = move |id| pending.push(Pointer::Dock(id, id, crate::tree::Side::Right));
        let pending = self.pending.clone();
        let close = move |id| pending.push(Pointer::Close(id));
        ui::tab_strip(&items, active, pick, out, close)
    }
}

/// Without WebKit there are no browser windows. An agent that asks for one
/// is told so, rather than left waiting for a desktop that will never
/// answer.
#[cfg(not(feature = "browser"))]
impl App {
    pub(super) fn ensure_browsers(&mut self) {}
    pub fn open_browser_window(&mut self) {}

    pub(super) fn drain_browser_ops(&mut self) {
        let Some(hub) = self.web.hub_owned() else {
            return;
        };
        for ask in hub.take_asks() {
            hub.answer(
                ask.id,
                serde_json::json!({
                    "ok": false,
                    "error": "this desktop was built without the browser",
                }),
            );
        }
    }

    pub(super) fn open_browser(
        &mut self,
        _project: ProjectId,
        _url: Option<&str>,
        _headless: bool,
    ) -> Result<AgentId, String> {
        Err("this build has no browser: rebuild with --features gui".into())
    }
}

#[cfg(feature = "browser")]
use serde_json::{Value, json};

#[cfg(feature = "browser")]
impl App {
    /// Give every browser window on screen a live view, and let go of the
    /// views whose windows are gone. Headless windows are not built here:
    /// they cost a web process each and are built when first driven.
    pub(super) fn ensure_browsers(&mut self) {
        let wanted: Vec<(AgentId, Option<String>)> = self
            .rows
            .iter()
            .filter(|r| {
                r.agent.kind == taix_core::BROWSER
                    && !r.agent.headless
                    && Some(r.agent.project) == self.selected
            })
            .map(|r| (r.agent.id, r.agent.url.clone()))
            .collect();
        for (id, url) in wanted {
            self.open_view(id, url.as_deref());
        }
        let live: Vec<AgentId> = self.rows.iter().map(|r| r.agent.id).collect();
        self.browsers.retain(|id, _| live.contains(id));
    }

    /// The view for one browser window, built if this is its first showing.
    /// The page it lands on is the one the window was last on, so a restart
    /// puts every browser window back where it was.
    fn open_view(&mut self, id: AgentId, url: Option<&str>) {
        if self.browsers.contains_key(&id) {
            return;
        }
        let home = url.unwrap_or(&self.cfg.browser_home).to_string();
        let browser = crate::browser::Browser::new(&home);
        // Record where the page went, so the window reopens there. Parked
        // like every other intent: this fires from inside WebKit's own
        // callback, where `App` is already borrowed.
        let pending = self.pending.clone();
        browser.on_uri_changed(move |uri| pending.push(Pointer::BrowserUrl(id, uri)));
        self.browsers.insert(id, browser);
    }

    /// Open a browser window in a project, or hand back the one that is
    /// already there. `Ctrl-B` and the agents' `browser_tabs new` both land
    /// here, so there is one answer to "where does the page open".
    pub(super) fn open_browser(
        &mut self,
        project: ProjectId,
        url: Option<&str>,
        headless: bool,
    ) -> Result<AgentId, String> {
        // A project added from the CLI a second ago is not in this screen's
        // list until it re-reads the store; asking an agent to try again
        // later would be the wrong answer to a question with a right one.
        if self.project(project).is_none() {
            self.reload();
        }
        let proj = self.project(project).ok_or("unknown project")?.clone();
        let existing: Vec<Agent> = self
            .rows
            .iter()
            .filter(|r| r.agent.project == project)
            .map(|r| r.agent.clone())
            .collect();
        let agent =
            taix_core::spawn::browser_window(&self.store, &self.cfg, &proj, &existing, headless)
                .map_err(|e| e.to_string())?;
        if let Some(url) = url {
            let _ = self.store.set_url(agent.id, Some(url.to_string()));
        }
        self.reload();
        if !headless {
            self.select_project(project);
            self.set_focus(agent.id);
        }
        if let Some(url) = url {
            self.open_view(agent.id, Some(url));
            if let Some(browser) = self.browsers.get(&agent.id) {
                browser.load(url);
            }
        }
        Ok(agent.id)
    }

    /// What `Ctrl-B` means now that the panel is gone: focus this project's
    /// browser window, and open one when it has none.
    pub fn open_browser_window(&mut self) {
        let Some(project) = self.selected else { return };
        let existing = self
            .rows
            .iter()
            .find(|r| {
                r.agent.project == project
                    && r.agent.kind == taix_core::BROWSER
                    && !r.agent.headless
            })
            .map(|r| r.agent.id);
        match existing {
            Some(id) => self.set_focus(id),
            None => {
                if let Err(e) = self.open_browser(project, None, false) {
                    ui::set_status(&self.w, &e);
                }
            }
        }
    }

    /// The page moved: remember it, so the window reopens where it was.
    /// Written through the store rather than held in memory, because the
    /// next process is the one that needs to know.
    pub(super) fn browser_url(&mut self, id: AgentId, url: String) {
        if self
            .row(id)
            .is_some_and(|r| r.agent.url.as_deref() == Some(url.as_str()))
        {
            return;
        }
        if self.store.set_url(id, Some(url.clone())).is_ok()
            && let Some(row) = self.row_mut(id)
        {
            row.agent.url = Some(url);
        }
        // Our own write: see `restamp`. A page that navigates every few
        // seconds would otherwise reload the whole model that often.
        self.restamp();
        self.sidebar_dirty = true;
    }

    /// Answer the browser ops parked by the MCP server. Each one names a
    /// window, or means "wherever a browser window already is"; the answer
    /// comes back from the page itself, frames later, so the hub is told
    /// out of the reply callback rather than from here.
    pub(super) fn drain_browser_ops(&mut self) {
        let Some(hub) = self.web.hub_owned() else {
            return;
        };
        for ask in hub.take_asks() {
            let answer = match self.resolve_tab(&ask.body) {
                Ok(None) => {
                    // A whole-session question: no page is involved.
                    Some(self.browser_tabs(&ask.body))
                }
                Ok(Some(id)) => {
                    let hub = hub.clone();
                    let ask_id = ask.id;
                    match self.browsers.get(&id) {
                        Some(browser) => {
                            browser.dispatch(&ask.body, move |mut answer| {
                                if let Some(obj) = answer.as_object_mut() {
                                    obj.insert("tab".into(), json!(id));
                                }
                                hub.answer(ask_id, answer);
                            });
                            None
                        }
                        None => Some(json!({
                            "ok": false,
                            "error": format!("window {id} has no page open"),
                        })),
                    }
                }
                Err(e) => Some(json!({ "ok": false, "error": e })),
            };
            if let Some(answer) = answer {
                hub.answer(ask.id, answer);
            }
        }
    }

    /// Which window an op is about. `None` means the op is about the set of
    /// windows rather than about a page.
    fn resolve_tab(&mut self, op: &Value) -> Result<Option<AgentId>, String> {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or_default();
        if kind == "tabs" {
            return Ok(None);
        }
        if let Some(id) = op.get("tab").and_then(Value::as_i64) {
            let row = self.row(id).ok_or(format!("no window {id}"))?;
            if row.agent.kind != taix_core::BROWSER {
                return Err(format!("window {id} is not a browser window"));
            }
            self.wake_view(id);
            return Ok(Some(id));
        }
        let project = op
            .get("project")
            .and_then(Value::as_i64)
            .or(self.selected)
            .ok_or("no project is open")?;
        let existing = self
            .rows
            .iter()
            .find(|r| r.agent.project == project && r.agent.kind == taix_core::BROWSER)
            .map(|r| r.agent.id);
        let id = match existing {
            Some(id) => id,
            // An agent that says "go to this URL" with no window open means
            // to open one: making it ask twice buys nothing.
            None => self.open_browser(project, None, false)?,
        };
        self.wake_view(id);
        Ok(Some(id))
    }

    /// Build the view for a window that is being driven but has never been
    /// shown - a headless window, or one belonging to a project nobody is
    /// looking at.
    fn wake_view(&mut self, id: AgentId) {
        let url = self.row(id).and_then(|r| r.agent.url.clone());
        self.open_view(id, url.as_deref());
    }

    /// `tabs`: list, open, show or close the browser windows.
    fn browser_tabs(&mut self, op: &Value) -> Value {
        let action = op.get("action").and_then(Value::as_str).unwrap_or("list");
        let tab = op.get("tab").and_then(Value::as_i64);
        let url = op.get("url").and_then(Value::as_str);
        match action {
            "list" => json!({ "ok": true, "tabs": self.browser_list() }),
            "new" => {
                let project = op.get("project").and_then(Value::as_i64).or(self.selected);
                let headless = op
                    .get("headless")
                    .and_then(Value::as_bool)
                    .unwrap_or_default();
                match project {
                    Some(project) => match self.open_browser(project, url, headless) {
                        Ok(id) => json!({ "ok": true, "tab": id, "tabs": self.browser_list() }),
                        Err(e) => json!({ "ok": false, "error": e }),
                    },
                    None => json!({ "ok": false, "error": "no project is open" }),
                }
            }
            "select" => match tab {
                Some(id) if self.row(id).is_some() => {
                    let project = self.row(id).map(|r| r.agent.project);
                    if let Some(project) = project {
                        self.select_project(project);
                    }
                    self.set_focus(id);
                    json!({ "ok": true, "tab": id })
                }
                _ => json!({ "ok": false, "error": "select needs the id of a browser window" }),
            },
            "close" => match tab {
                Some(id) if self.row(id).is_some() => match self.kill_agent(id, false) {
                    Ok(()) => json!({ "ok": true, "tab": id }),
                    Err(e) => json!({ "ok": false, "error": e }),
                },
                _ => json!({ "ok": false, "error": "close needs the id of a browser window" }),
            },
            other => json!({ "ok": false, "error": format!("unknown tabs action: {other}") }),
        }
    }

    fn browser_list(&self) -> Vec<Value> {
        self.rows
            .iter()
            .filter(|r| r.agent.kind == taix_core::BROWSER)
            .map(|r| {
                let live = self.browsers.get(&r.agent.id);
                json!({
                    "id": r.agent.id,
                    "project": r.agent.project,
                    "name": r.agent.name,
                    "url": live.and_then(|b| b.uri()).or_else(|| r.agent.url.clone()),
                    "title": live.and_then(|b| b.title()),
                    "headless": r.agent.headless,
                    "active": self.focus == Some(r.agent.id),
                })
            })
            .collect()
    }
}
