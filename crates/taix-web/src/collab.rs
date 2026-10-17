//! Working with other people and other devices: read-only share links,
//! named devices, and handing a window from one to another.
//!
//! Everything but a device's chosen name lives in memory and dies with the
//! server, which is the right lifetime for a link someone was handed: a
//! restart revokes them all. Commands are judged in one place, [`admit`],
//! which the WebSocket and `POST /cmd` both go through.
//!
//! [`admit`]: Collab::admit

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;

use crate::http::{Reply, Req};
use crate::proto::{Cmd, Holder, Snapshot};

/// How long a share link lives when the page does not say.
pub(crate) const SHARE_TTL: Duration = Duration::from_secs(3600);
/// Links alive at once. A paired device is trusted, but a loop is a bug.
const MAX_SHARES: usize = 32;
const MAX_NAME: usize = 40;
/// A claim the desktop has not yet put on its books is not stale for this
/// long, so a snapshot published between the two does not forget who asked.
const CLAIM_GRACE: Duration = Duration::from_secs(3);

/// Who is speaking: a paired browser, named by the id its page keeps (empty
/// when it did not say), or a visitor holding a share link.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Origin {
    Device(String),
    Viewer { token: String, window: i64 },
}

/// A device id the page chose for itself: short, URL-safe, or nothing.
pub(crate) fn clean_id(raw: &str) -> String {
    let ok = !raw.is_empty()
        && raw.len() <= 32
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok { raw.to_string() } else { String::new() }
}

struct Share {
    token: String,
    window: i64,
    ends: Instant,
}

/// One open WebSocket of a paired device, and how to write a frame to it.
struct Conn {
    id: u64,
    device: String,
    label: String,
    send: Arc<dyn Fn(&str) + Send + Sync>,
}

#[derive(Default)]
struct Inner {
    shares: Vec<Share>,
    /// Names the user chose, by device id. The only part kept on disk.
    names: HashMap<String, String>,
    conns: Vec<Conn>,
    /// Window, device id, when: who sent the last takeover for a window.
    holders: Vec<(i64, String, Instant)>,
    seq: u64,
    path: Option<PathBuf>,
}

impl Inner {
    /// What to call a device: its chosen name, else what its browser says
    /// it is, else nothing better than "another device".
    fn name_of(&self, device: &str) -> String {
        self.names
            .get(device)
            .cloned()
            .or_else(|| {
                self.conns
                    .iter()
                    .find(|c| c.device == device)
                    .map(|c| c.label.clone())
            })
            .unwrap_or_else(|| "another device".into())
    }

    fn holder(&self, window: i64) -> Option<&str> {
        self.holders
            .iter()
            .find(|(w, ..)| *w == window)
            .map(|(_, d, _)| d.as_str())
    }
}

#[derive(Default)]
pub(crate) struct Collab(Mutex<Inner>);

/// Constant-time, like the pairing key: a link is a password.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// "iPad", "iPhone", "Chrome on Linux": what a person calls the device
/// before they have named it.
pub(crate) fn nickname(agent: &str) -> String {
    if let Some(phone) = ["iPhone", "iPad"].into_iter().find(|p| agent.contains(p)) {
        return phone.into();
    }
    // Android before Linux: Chrome on Android says both.
    let os = [
        ("Android", "Android"),
        ("Macintosh", "Mac"),
        ("Windows", "Windows"),
        ("CrOS", "ChromeOS"),
        ("Linux", "Linux"),
    ]
    .into_iter()
    .find(|(key, _)| agent.contains(key))
    .map(|(_, name)| name);
    // Edge and Opera claim to be Chrome, and all of them claim Safari.
    let browser = [
        ("Edg", "Edge"),
        ("OPR", "Opera"),
        ("Firefox", "Firefox"),
        ("Chrome", "Chrome"),
        ("Safari", "Safari"),
    ]
    .into_iter()
    .find(|(key, _)| agent.contains(key))
    .map(|(_, name)| name);
    match (browser, os) {
        (Some(b), Some(o)) => format!("{b} on {o}"),
        (Some(b), None) => b.into(),
        (None, Some(o)) => o.into(),
        (None, None) => "Browser".into(),
    }
}

impl Collab {
    /// Adopt the names stored at `path` and write there from now on.
    pub(crate) fn open_at(&self, path: PathBuf) {
        let names = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        let mut inner = self.0.lock();
        inner.names = names;
        inner.path = Some(path);
    }

    // ---------- share links ----------

    /// A new link for one window, or `None` when there are too many or the
    /// kernel would not give random bytes (`random_key` then returns zeros,
    /// and a zero token is a guessable one).
    pub(crate) fn share(&self, window: i64, ttl: Duration) -> Option<String> {
        self.share_at(window, ttl, Instant::now())
    }

    fn share_at(&self, window: i64, ttl: Duration, now: Instant) -> Option<String> {
        let token = taix_core::random_key();
        if token.bytes().all(|b| b == b'0') {
            return None;
        }
        let mut inner = self.0.lock();
        inner.shares.retain(|s| s.ends > now);
        if inner.shares.len() >= MAX_SHARES {
            return None;
        }
        inner.shares.push(Share {
            token: token.clone(),
            window,
            ends: now + ttl,
        });
        Some(token)
    }

    /// The window a link shows and how long it has left, while it is valid.
    pub(crate) fn view(&self, token: &str) -> Option<(i64, Duration)> {
        self.view_at(token, Instant::now())
    }

    fn view_at(&self, token: &str, now: Instant) -> Option<(i64, Duration)> {
        self.0
            .lock()
            .shares
            .iter()
            .find(|s| s.ends > now && same(&s.token, token))
            .map(|s| (s.window, s.ends - now))
    }

    /// Live links: token, window, seconds left. Newest last.
    pub(crate) fn shares(&self) -> Vec<(String, i64, u64)> {
        let now = Instant::now();
        let mut inner = self.0.lock();
        inner.shares.retain(|s| s.ends > now);
        inner
            .shares
            .iter()
            .map(|s| (s.token.clone(), s.window, (s.ends - now).as_secs()))
            .collect()
    }

    pub(crate) fn revoke(&self, token: &str) -> bool {
        let mut inner = self.0.lock();
        let before = inner.shares.len();
        inner.shares.retain(|s| !same(&s.token, token));
        inner.shares.len() != before
    }

    /// The pairing key changed: that was the owner saying "everyone out".
    pub(crate) fn revoke_all(&self) {
        self.0.lock().shares.clear();
    }

    // ---------- devices ----------

    /// A paired page opened its socket. `send` writes one text frame to it.
    pub(crate) fn join(
        &self,
        device: &str,
        label: &str,
        send: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> u64 {
        let mut inner = self.0.lock();
        inner.seq += 1;
        let id = inner.seq;
        inner.conns.push(Conn {
            id,
            device: device.into(),
            label: label.into(),
            send,
        });
        id
    }

    pub(crate) fn leave(&self, conn: u64) {
        self.0.lock().conns.retain(|c| c.id != conn);
    }

    /// A device's name as the user set it, or `fallback`.
    pub(crate) fn name(&self, device: &str, fallback: &str) -> String {
        self.0
            .lock()
            .names
            .get(device)
            .cloned()
            .unwrap_or_else(|| fallback.into())
    }

    /// Set a device's name; empty goes back to the browser's own.
    pub(crate) fn rename(&self, device: &str, name: &str) {
        let name: String = name.trim().chars().take(MAX_NAME).collect();
        let mut inner = self.0.lock();
        if name.is_empty() {
            inner.names.remove(device);
        } else {
            inner.names.insert(device.into(), name);
        }
        if let Some(path) = &inner.path {
            let json = serde_json::to_vec(&inner.names).unwrap_or_default();
            let _ = std::fs::write(path, json);
        }
    }

    /// Connected devices, one entry each: id and name.
    pub(crate) fn devices(&self) -> Vec<(String, String)> {
        let inner = self.0.lock();
        let mut seen: Vec<&str> = Vec::new();
        for conn in &inner.conns {
            if !seen.contains(&conn.device.as_str()) {
                seen.push(&conn.device);
            }
        }
        seen.into_iter()
            .map(|id| (id.to_string(), inner.name_of(id)))
            .collect()
    }

    /// Tell every page of `to` that `from` sent it a window. How many
    /// sockets heard it; zero means the device has gone.
    pub(crate) fn hand_off(&self, from: &str, to: &str, window: i64, title: &str) -> usize {
        let (frame, targets) = {
            let inner = self.0.lock();
            let title: String = title.chars().take(80).collect();
            let frame = json!({ "notice": {
                "from": inner.name_of(from), "window": window, "name": title
            }})
            .to_string();
            let targets: Vec<_> = inner
                .conns
                .iter()
                .filter(|c| c.device == to)
                .map(|c| c.send.clone())
                .collect();
            (frame, targets)
        };
        // Written with the lock released: a phone that stopped reading
        // blocks its writer for the socket's timeout.
        for send in &targets {
            send(&frame);
        }
        targets.len()
    }

    // ---------- commands ----------

    /// The one place a browser's command is judged before it joins the
    /// queue the desktop drains. Returns what to queue: nothing, the
    /// command, or - for "give everything back" - one release per window
    /// that device actually holds.
    pub(crate) fn admit(&self, cmd: Cmd, from: &Origin) -> Vec<Cmd> {
        self.admit_at(cmd, from, Instant::now())
    }

    fn admit_at(&self, cmd: Cmd, from: &Origin, now: Instant) -> Vec<Cmd> {
        let mut inner = self.0.lock();
        let dev = match from {
            // A link can only ask to have its one window kept fresh. Typing,
            // claiming, scrolling, closing - every other command is refused
            // here, whatever the page said.
            Origin::Viewer { token, window } => {
                let live = inner
                    .shares
                    .iter()
                    .any(|s| s.ends > now && s.window == *window && same(&s.token, token));
                return match cmd {
                    Cmd::Watch { .. } if live => vec![Cmd::Watch {
                        client: format!("share:{token}"),
                        windows: vec![*window],
                    }],
                    _ => Vec::new(),
                };
            }
            Origin::Device(dev) if dev.is_empty() => return vec![cmd],
            Origin::Device(dev) => dev,
        };
        match cmd {
            Cmd::Takeover { window } => {
                inner.holders.retain(|(w, ..)| *w != window);
                inner.holders.push((window, dev.clone(), now));
                vec![cmd]
            }
            Cmd::Release {
                window: Some(window),
            } => {
                if inner.holder(window).is_some_and(|h| h != dev.as_str()) {
                    return Vec::new();
                }
                inner.holders.retain(|(w, ..)| *w != window);
                vec![cmd]
            }
            // The page closing hands back what it holds, not what some
            // other browser holds. Nobody on record: pass it through, it may
            // be the leftovers of a server restart.
            Cmd::Release { window: None } => {
                if inner.holders.is_empty() {
                    return vec![cmd];
                }
                let mine: Vec<i64> = inner
                    .holders
                    .iter()
                    .filter(|(_, d, _)| d == dev)
                    .map(|(w, ..)| *w)
                    .collect();
                inner.holders.retain(|(_, d, _)| d != dev);
                mine.into_iter()
                    .map(|w| Cmd::Release { window: Some(w) })
                    .collect()
            }
            cmd => match cmd.drives() {
                // Another device holds this terminal: its keys do not count.
                Some(w) if inner.holder(w).is_some_and(|h| h != dev.as_str()) => Vec::new(),
                _ => vec![cmd],
            },
        }
    }

    /// Who holds each of the windows the desktop says a browser holds.
    pub(crate) fn holders(&self, remote: &[i64]) -> Vec<Holder> {
        let now = Instant::now();
        let mut inner = self.0.lock();
        inner
            .holders
            .retain(|(w, _, at)| remote.contains(w) || now.duration_since(*at) < CLAIM_GRACE);
        inner
            .holders
            .iter()
            .filter(|(w, ..)| remote.contains(w))
            .map(|(w, d, _)| Holder {
                window: *w,
                device: d.clone(),
                name: inner.name_of(d),
            })
            .collect()
    }
}

/// What a link visitor is told about its one window, out of a published
/// snapshot. Only this crosses to the visitor: no paths, no attach command,
/// no other windows.
pub(crate) fn viewer_frame(snap: &Snapshot, window: i64, ends: u64) -> String {
    let pane = snap.panes.iter().find(|p| p.id == window);
    let win = snap
        .projects
        .iter()
        .flat_map(|p| &p.windows)
        .find(|w| w.id == window);
    json!({
        "name": win.map(|w| w.name.as_str()),
        "state": win.map(|w| w.state.as_str()),
        "alive": win.is_some_and(|w| w.alive),
        "ends": ends,
        "cols": pane.map(|p| p.cols),
        "rows": pane.map(|p| p.rows),
        "html": pane.map(|p| p.screen.to_html()),
    })
    .to_string()
}

// ---------- routes (paired devices only) ----------

/// `/collab/*`. The caller has checked the pairing cookie and the passkey
/// lock. `?d=` is the asking page's device id.
pub(crate) fn route(req: &Req, hub: &crate::Hub) -> Reply {
    let me = clean_id(req.arg("d"));
    let collab = &hub.collab;
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/collab/share") => {
            #[derive(Deserialize)]
            struct Body {
                window: i64,
                #[serde(default)]
                minutes: Option<u64>,
            }
            let Ok(body) = serde_json::from_slice::<Body>(&req.body) else {
                return Reply::fail(400, "expected {window}");
            };
            let ttl = body
                .minutes
                .map_or(SHARE_TTL, |m| Duration::from_secs(m.clamp(1, 1440) * 60));
            match collab.share(body.window, ttl) {
                Some(token) => Reply::json(
                    200,
                    json!({ "token": token, "secs": ttl.as_secs() }).to_string(),
                ),
                None => Reply::fail(400, "too many links are live; revoke one first"),
            }
        }
        ("GET", "/collab/shares") => {
            let shares: Vec<_> = collab
                .shares()
                .into_iter()
                .map(|(token, window, secs)| json!({ "token": token, "window": window, "secs": secs }))
                .collect();
            Reply::json(200, json!({ "shares": shares }).to_string())
        }
        ("POST", "/collab/revoke") => {
            #[derive(Deserialize)]
            struct Body {
                token: String,
            }
            match serde_json::from_slice::<Body>(&req.body) {
                Ok(body) => Reply::json(
                    200,
                    json!({ "revoked": collab.revoke(&body.token) }).to_string(),
                ),
                Err(_) => Reply::fail(400, "expected {token}"),
            }
        }
        ("GET", "/collab/devices") => {
            let name = collab.name(&me, &nickname(&req.agent));
            let others: Vec<_> = collab
                .devices()
                .into_iter()
                .filter(|(id, _)| *id != me)
                .map(|(id, name)| json!({ "id": id, "name": name }))
                .collect();
            Reply::json(
                200,
                json!({ "me": { "id": me, "name": name }, "devices": others }).to_string(),
            )
        }
        ("POST", "/collab/name") => {
            #[derive(Deserialize)]
            struct Body {
                name: String,
            }
            match serde_json::from_slice::<Body>(&req.body) {
                Ok(_) if me.is_empty() => Reply::fail(400, "this page has no device id"),
                Ok(body) => {
                    collab.rename(&me, &body.name);
                    Reply::json(
                        200,
                        json!({ "name": collab.name(&me, &nickname(&req.agent)) }).to_string(),
                    )
                }
                Err(_) => Reply::fail(400, "expected {name}"),
            }
        }
        ("POST", "/collab/send") => {
            #[derive(Deserialize)]
            struct Body {
                to: String,
                window: i64,
                #[serde(default)]
                name: String,
            }
            match serde_json::from_slice::<Body>(&req.body) {
                Ok(_) if me.is_empty() => Reply::fail(400, "this page has no device id"),
                Ok(body) => match collab.hand_off(&me, &body.to, body.window, &body.name) {
                    0 => Reply::fail(404, "that device is no longer connected"),
                    n => Reply::json(200, json!({ "sent": n }).to_string()),
                },
                Err(_) => Reply::fail(400, "expected {to, window}"),
            }
        }
        _ => Reply::fail(404, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(window: i64) -> Cmd {
        Cmd::Keys {
            window,
            text: "rm -rf".into(),
        }
    }

    fn viewer(token: &str, window: i64) -> Origin {
        Origin::Viewer {
            token: token.into(),
            window,
        }
    }

    #[test]
    fn a_link_is_valid_until_it_expires_or_is_revoked() {
        let c = Collab::default();
        let now = Instant::now();
        let ttl = Duration::from_secs(60);
        let token = c.share_at(7, ttl, now).unwrap();

        assert_eq!(c.view_at(&token, now).map(|v| v.0), Some(7));
        assert_eq!(
            c.view_at(&token, now + ttl - Duration::from_secs(1))
                .map(|v| v.0),
            Some(7)
        );
        assert!(c.view_at(&token, now + ttl).is_none());
        assert!(c.view_at("not-a-token", now).is_none());
        assert!(c.view_at("", now).is_none());

        let other = c.share_at(8, ttl, now).unwrap();
        assert_ne!(token, other);
        assert!(c.revoke(&token));
        assert!(c.view_at(&token, now).is_none());
        assert_eq!(c.view_at(&other, now).map(|v| v.0), Some(8));
        c.revoke_all();
        assert!(c.view_at(&other, now).is_none());
    }

    #[test]
    fn a_viewer_may_only_ask_to_watch_its_own_window() {
        let c = Collab::default();
        let now = Instant::now();
        let token = c.share_at(3, Duration::from_secs(60), now).unwrap();
        let v = viewer(&token, 3);

        for cmd in [
            keys(3),
            Cmd::Takeover { window: 3 },
            Cmd::Release { window: None },
            Cmd::Scroll {
                window: 3,
                delta: 5,
            },
            Cmd::Close { window: 3 },
            Cmd::Kill { window: 3 },
            Cmd::OpenFile {
                path: "/etc/passwd".into(),
                with: None,
            },
            Cmd::Config {
                patch: Default::default(),
            },
        ] {
            assert!(c.admit_at(cmd.clone(), &v, now).is_empty(), "{cmd:?}");
        }

        // Asking for more windows than the link covers gets exactly one.
        let cmds = c.admit_at(
            Cmd::Watch {
                client: "x".into(),
                windows: vec![1, 3, 9],
            },
            &v,
            now,
        );
        assert_eq!(
            cmds,
            [Cmd::Watch {
                client: format!("share:{token}"),
                windows: vec![3]
            }]
        );

        // A token that does not match this window, a revoked one, an expired one.
        assert!(
            c.admit_at(
                Cmd::Watch {
                    client: "x".into(),
                    windows: vec![4]
                },
                &viewer(&token, 4),
                now
            )
            .is_empty()
        );
        let late = now + Duration::from_secs(61);
        assert!(
            c.admit_at(
                Cmd::Watch {
                    client: "x".into(),
                    windows: vec![3]
                },
                &v,
                late
            )
            .is_empty()
        );
        c.revoke(&token);
        assert!(
            c.admit_at(
                Cmd::Watch {
                    client: "x".into(),
                    windows: vec![3]
                },
                &v,
                now
            )
            .is_empty()
        );
    }

    #[test]
    fn only_the_device_that_claimed_a_window_may_drive_or_release_it() {
        let c = Collab::default();
        let now = Instant::now();
        let (a, b) = (Origin::Device("aaa".into()), Origin::Device("bbb".into()));

        assert_eq!(c.admit_at(Cmd::Takeover { window: 5 }, &a, now).len(), 1);
        assert_eq!(c.admit_at(keys(5), &a, now), [keys(5)]);
        assert!(c.admit_at(keys(5), &b, now).is_empty());
        assert!(
            c.admit_at(Cmd::Release { window: Some(5) }, &b, now)
                .is_empty()
        );
        // A window nobody claimed through this server is not b's to refuse.
        assert_eq!(c.admit_at(keys(6), &b, now).len(), 1);

        // b closing its page gives back nothing of a's; a's gives back 5.
        assert!(
            c.admit_at(Cmd::Release { window: None }, &b, now)
                .is_empty()
        );
        assert_eq!(
            c.admit_at(Cmd::Release { window: None }, &a, now),
            [Cmd::Release { window: Some(5) }]
        );
        assert_eq!(c.admit_at(keys(5), &b, now).len(), 1);
    }

    #[test]
    fn the_snapshot_names_who_holds_what_the_desktop_says_is_held() {
        let c = Collab::default();
        let a = Origin::Device("aaa".into());
        c.rename("aaa", "  My iPad  ");
        c.admit(Cmd::Takeover { window: 5 }, &a);
        c.admit(Cmd::Takeover { window: 6 }, &a);

        let held = c.holders(&[5]);
        assert_eq!(held.len(), 1);
        assert_eq!((held[0].window, held[0].name.as_str()), (5, "My iPad"));
        // 6 is not held on the desktop, but the claim is too young to drop.
        assert_eq!(c.holders(&[5, 6]).len(), 2);
    }

    #[test]
    fn a_hand_off_reaches_only_the_chosen_device() {
        let c = Collab::default();
        let heard = Arc::new(Mutex::new(Vec::<(&'static str, String)>::new()));
        let ear = |who: &'static str| {
            let heard = heard.clone();
            Arc::new(move |text: &str| heard.lock().push((who, text.to_string())))
                as Arc<dyn Fn(&str) + Send + Sync>
        };
        c.join("aaa", "iPad", ear("a"));
        let b = c.join("bbb", "Chrome on Linux", ear("b"));
        c.join("ccc", "iPhone", ear("c"));
        assert_eq!(c.devices().len(), 3);

        assert_eq!(c.hand_off("aaa", "bbb", 9, "build"), 1);
        let heard_now = heard.lock().clone();
        assert_eq!(heard_now.len(), 1);
        assert_eq!(heard_now[0].0, "b");
        let notice: serde_json::Value = serde_json::from_str(&heard_now[0].1).unwrap();
        assert_eq!(notice["notice"]["from"], "iPad");
        assert_eq!(notice["notice"]["window"], 9);
        assert_eq!(notice["notice"]["name"], "build");

        c.leave(b);
        assert_eq!(c.hand_off("aaa", "bbb", 9, "build"), 0);
    }

    #[test]
    fn devices_are_called_what_their_browser_is() {
        let ua = |s: &str| nickname(s);
        assert_eq!(
            ua("Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit Safari"),
            "iPad"
        );
        assert_eq!(
            ua("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0) CriOS Safari"),
            "iPhone"
        );
        assert_eq!(
            ua("Mozilla/5.0 (X11; Linux x86_64) Chrome/120 Safari/537"),
            "Chrome on Linux"
        );
        assert_eq!(
            ua("Mozilla/5.0 (Linux; Android 14) Chrome/120 Mobile Safari"),
            "Chrome on Android"
        );
        assert_eq!(
            ua("Mozilla/5.0 (Windows NT 10.0) Chrome/120 Safari Edg/120"),
            "Edge on Windows"
        );
        assert_eq!(ua(""), "Browser");
    }

    #[test]
    fn a_viewer_frame_carries_one_window_and_nothing_else() {
        let mut snap: Snapshot = serde_json::from_str(
            r#"{"lock":{"remote":[]},"host":"h","session":{"uptime":0,"shell":"sh","tmux":""},
            "projects":[{"id":1,"name":"p","path":"/home/me/secret","folded":false,"windows":[
              {"id":3,"name":"build","kind":"terminal","icon":"t","state":"working","since":0,"alive":true,"attention":false,"cpu":0.0,"tmux":"tmux attach"},
              {"id":4,"name":"other","kind":"terminal","icon":"t","state":"idle","since":0,"alive":true,"attention":false,"cpu":0.0}]}],
            "panes":[{"id":3,"cols":80,"rows":24,"scroll":0,"mouse":false,"alt":false},
                     {"id":4,"cols":80,"rows":24,"scroll":0,"mouse":false,"alt":false}],
            "bar":[],"harnesses":[],"palette":[],
            "health":{"life":"live","port":1,"addr":"","url":"","clients":0,"error":""}}"#,
        )
        .unwrap();
        for (pane, text) in snap.panes.iter_mut().zip(["hi", "SECRET"]) {
            pane.screen = crate::html::screen(|sink| {
                sink(taix_term::Chunk::Text(text, taix_term::Style::default()))
            });
        }
        let out = viewer_frame(&snap, 3, 99);
        assert!(out.contains("build") && out.contains("hi"));
        for leak in ["SECRET", "other", "/home/me", "tmux attach"] {
            assert!(!out.contains(leak), "{leak} leaked: {out}");
        }
    }
}
