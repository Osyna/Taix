//! Enough HTTP/1.1 to serve one page, one WebSocket and a POST.
//!
//! Hand-written because the whole contract is: parse a request line and a
//! `Content-Length`, write a header block, keep the socket alive. A
//! framework would be a dependency tree, a runtime and an async colour
//! change through a crate that otherwise has none.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use crate::collab::{self, Origin};
use crate::{Hub, proto};

/// Assets are embedded, not read from disk: the GUI is one binary that a
/// user may have copied anywhere, and a web front end that 404s because the
/// working directory moved is worse than no web front end.
///
/// Request path, content type, source, and the gzip copy `build.rs` made
/// of it - empty when the build machine had no `gzip`.
macro_rules! assets {
    ($(($path:expr, $kind:expr, $file:expr)),* $(,)?) => {
        &[$((
            $path,
            $kind,
            include_str!(concat!("../assets/", $file)),
            include_bytes!(concat!(env!("OUT_DIR"), "/", $file, ".gz")),
        )),*]
    };
}

type Asset = (&'static str, &'static str, &'static str, &'static [u8]);

const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";

const ASSETS: &[Asset] = assets![
    ("/", HTML, "index.html"),
    ("/style.css", "text/css; charset=utf-8", "style.css"),
    (
        "/manifest.webmanifest",
        "application/manifest+json",
        "manifest.webmanifest"
    ),
    ("/sw.js", JS, "sw.js"),
    ("/js/store.js", JS, "js/store.js"),
    ("/js/app.js", JS, "js/app.js"),
    ("/js/side.js", JS, "js/side.js"),
    ("/js/term.js", JS, "js/term.js"),
    ("/js/compose.js", JS, "js/compose.js"),
    ("/js/panels.js", JS, "js/panels.js"),
    ("/js/history.js", JS, "js/history.js"),
    ("/js/lock.js", JS, "js/lock.js"),
    ("/js/hosts.js", JS, "js/hosts.js"),
    ("/js/preview.js", JS, "js/preview.js"),
    ("/viewer.html", HTML, "viewer.html"),
    ("/js/viewer.js", JS, "js/viewer.js"),
    ("/js/collab.js", JS, "js/collab.js"),
    ("/js/age.js", JS, "js/age.js"),
    ("/js/hints.js", JS, "js/hints.js"),
];

/// One PNG under `assets/`, served at the same path.
macro_rules! png {
    ($file:literal) => {
        (
            concat!("/", $file),
            "image/png",
            include_bytes!(concat!("../assets/", $file)),
        )
    };
}

/// The app's own icons and launch screens: binary, so not in `ASSETS`, and
/// public - the manifest and the home screen ask for them without the cookie.
const IMAGES: &[(&str, &str, &[u8])] = &[
    (
        "/icon.svg",
        "image/svg+xml",
        include_bytes!("../assets/icon.svg"),
    ),
    (
        "/icon-192.png",
        "image/png",
        include_bytes!("../assets/icon-192.png"),
    ),
    (
        "/icon-512.png",
        "image/png",
        include_bytes!("../assets/icon-512.png"),
    ),
    (
        "/icon-maskable.png",
        "image/png",
        include_bytes!("../assets/icon-maskable.png"),
    ),
    (
        "/apple-touch-icon.png",
        "image/png",
        include_bytes!("../assets/apple-touch-icon.png"),
    ),
    // One launch screen per iPhone screen size, and per iPad size and
    // orientation, named by physical pixels; `index.html` picks between
    // them by media query.
    png!("splash/750x1334.png"),
    png!("splash/828x1792.png"),
    png!("splash/1125x2436.png"),
    png!("splash/1170x2532.png"),
    png!("splash/1179x2556.png"),
    png!("splash/1206x2622.png"),
    png!("splash/1242x2688.png"),
    png!("splash/1284x2778.png"),
    png!("splash/1290x2796.png"),
    png!("splash/1320x2868.png"),
    png!("splash/1488x2266.png"),
    png!("splash/2266x1488.png"),
    png!("splash/1620x2160.png"),
    png!("splash/2160x1620.png"),
    png!("splash/1640x2360.png"),
    png!("splash/2360x1640.png"),
    png!("splash/1668x2388.png"),
    png!("splash/2388x1668.png"),
    png!("splash/1668x2420.png"),
    png!("splash/2420x1668.png"),
    png!("splash/2048x2732.png"),
    png!("splash/2732x2048.png"),
    png!("splash/2064x2752.png"),
    png!("splash/2752x2064.png"),
];

const PAIR_PAGE: &str = r#"<!doctype html>
<html><head><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1">
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{background:#0b0f12;color:#e0e0e0;font:16px sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh}
form{background:#1a1f24;padding:2rem;border-radius:8px;width:90%;max-width:400px}
h1{margin-bottom:0.5rem;font-size:1.5rem}
p{margin-bottom:1.5rem;color:#a0a0a0}
.info{margin-bottom:1rem;color:#6a9fb5;font-size:0.9rem}
input{width:100%;padding:0.75rem;border:1px solid #444;border-radius:4px;background:#0b0f12;color:#e0e0e0;font:1rem monospace;margin-bottom:1rem}
input:focus{outline:none;border-color:#4a90e2}
button{width:100%;padding:0.75rem;border:none;border-radius:4px;background:#4a90e2;color:white;font:1rem sans-serif;cursor:pointer}
button:hover{background:#357abd}
</style></head><body>
<form method=post action=/pair>
<h1>Pair with TaiX</h1>
<div class=info id=info></div>
<p id=msg>Asked the desktop. Waiting for someone to say yes.</p>
<input name=k autocomplete=off autocapitalize=off spellcheck=false inputmode=text autofocus>
<button>Connect</button>
</form>
<script>
let timer;
function poll(){
fetch('/pair/state',{cache:'no-store'}).then(r=>r.json()).then(d=>{
document.getElementById('info').textContent=d.device+' · '+d.ip;
if(d.state==='paired')location.reload();
else if(d.state==='denied'){
clearInterval(timer);
document.getElementById('msg').textContent='The desktop said no. Ask the person at the keyboard.';
}
}).catch(()=>{});
}
poll();
timer=setInterval(poll,2000);
</script>
</body></html>"#;

/// A request nobody is streaming: a phone that goes to sleep mid-request
/// must not park a thread for the life of the process.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the entire request gets, including headers and body. A
/// keep-alive connection gets a fresh deadline per request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A hostile device sending one header line every 9 seconds must not park a
/// thread indefinitely.
const MAX_HEADERS: usize = 64;

/// Total bytes across all header lines. 16 KiB is far beyond what any page
/// on this LAN sends, and accepting more is a DoS.
const MAX_HEADER_BYTES: usize = 16 << 10;

/// Read the body in chunks instead of allocating the full claimed length up
/// front: a lying `content-length` costs nothing until bytes arrive.
const BODY_CHUNK: usize = 64 << 10;

/// FNV-1a over the asset, so a browser that already has this build's copy
/// is told `304` instead of being sent it again. A rebuild changes the
/// bytes and so changes the tag; nothing has to be versioned by hand.
fn etag(bytes: &[u8]) -> String {
    let mut hash = 2166136261u32;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    format!("\"{hash:08x}\"")
}

fn asset_etag(index: usize) -> &'static str {
    static TAGS: LazyLock<Vec<String>> =
        LazyLock::new(|| ASSETS.iter().map(|a| etag(a.2.as_bytes())).collect());
    TAGS.get(index).map(String::as_str).unwrap_or("")
}

/// A hostile client on the LAN must not be able to park a thread each.
static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
const MAX_CONNECTIONS: usize = 64;

pub struct Req {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub body: Vec<u8>,
    pub cookies: HashMap<String, String>,
    pub accept_encoding: String,
    pub if_none_match: String,
    pub peer: String,
    pub agent: String,
    /// `Sec-WebSocket-Key`, on an upgrade request only.
    pub ws_key: String,
    /// `Host`, which is the WebAuthn relying party and what an origin is
    /// checked against.
    pub host: String,
    /// The request as sent, for the preview proxy to forward.
    pub raw: crate::proxy::Raw,
}

impl Req {
    pub fn arg(&self, key: &str) -> &str {
        self.query.get(key).map(String::as_str).unwrap_or_default()
    }
}

/// What an endpoint came back with. Bodies are small and already in memory
/// - a directory listing, a diff - so there is no streaming case to serve.
pub struct Reply {
    pub status: u16,
    pub kind: &'static str,
    pub body: Vec<u8>,
    /// Beyond content-type and length: the pairing cookie, a redirect.
    pub headers: Vec<(&'static str, String)>,
}

impl Reply {
    pub fn json(status: u16, body: String) -> Reply {
        Reply {
            status,
            kind: "application/json; charset=utf-8",
            body: body.into_bytes(),
            headers: Vec::new(),
        }
    }

    /// An error the page can show verbatim. Always JSON, so the client has
    /// one shape to handle.
    pub fn fail(status: u16, message: &str) -> Reply {
        Reply::json(status, serde_json::json!({ "error": message }).to_string())
    }

    /// Pairing succeeded: hand over the cookie and land on the page.
    fn paired(key: &str) -> Reply {
        Reply {
            status: 302,
            kind: "text/plain",
            body: Vec::new(),
            headers: vec![
                (
                    "set-cookie",
                    format!("taix={key}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000"),
                ),
                ("location", "/".into()),
            ],
        }
    }
}

/// The pairing cookie's value, or empty.
fn cookie(req: &Req) -> &str {
    req.cookies.get("taix").map(String::as_str).unwrap_or("")
}

/// An asset by index into `ASSETS`. The bytes are revalidated, never held
/// blind: the page links its scripts by plain path, so a browser that
/// cached one for a year would keep running the previous build's code
/// after an update. An `etag` costs one conditional request and sends
/// nothing when the build has not changed.
fn asset(index: usize, req: &Req) -> Reply {
    let (_, kind, plain, gz) = ASSETS[index];
    let tag = asset_etag(index);
    let mut headers = vec![
        ("cache-control", "no-cache".to_string()),
        ("etag", tag.to_string()),
    ];
    if req.if_none_match == tag {
        return Reply {
            status: 304,
            kind,
            body: Vec::new(),
            headers,
        };
    }
    let zipped = !gz.is_empty() && req.accept_encoding.contains("gzip");
    if zipped {
        headers.push(("content-encoding", "gzip".to_string()));
    }
    Reply {
        status: 200,
        kind,
        body: if zipped {
            gz.to_vec()
        } else {
            plain.as_bytes().to_vec()
        },
        headers,
    }
}

/// The app's page with the head of the picture inlined, so the first paint
/// does not wait for the socket to open and answer. Different on every
/// load, so it has no `etag`; gzipped here, the prebuilt copy having no
/// head in it.
fn boot(req: &Req, head: &str) -> Reply {
    let index = ASSETS.iter().position(|a| a.0 == "/").unwrap_or_default();
    let (_, kind, plain, _) = ASSETS[index];
    // In a script block `</` ends it early and `<!` can open a comment:
    // JSON spells either character another way, so none of it is markup.
    let head = head.replace("</", "<\\/").replace("<!", "<\\u0021");
    let page = plain.replacen(
        "<!--boot-->",
        &format!("<script id=\"boot\" type=\"application/json\">{head}</script>"),
        1,
    );
    let zipped = req
        .accept_encoding
        .contains("gzip")
        .then(|| gzip(page.as_bytes()))
        .flatten();
    let mut headers = Vec::new();
    if zipped.is_some() {
        headers.push(("content-encoding", "gzip".to_string()));
    }
    Reply {
        status: 200,
        kind,
        body: zipped.unwrap_or_else(|| page.into_bytes()),
        headers,
    }
}

fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(bytes).ok()?;
    gz.finish().ok()
}

/// Platform and browser out of a user-agent, for the line the desktop
/// shows beside the address. Edge and Opera are checked before Safari
/// because every one of them claims to be Safari.
pub(crate) fn device(agent: &str) -> String {
    let os = [
        "iPhone",
        "iPad",
        "Android",
        "Macintosh",
        "Windows",
        "CrOS",
        "Linux",
    ]
    .iter()
    .find(|&&s| agent.contains(s))
    .copied();
    let browser = ["Edg", "OPR", "Firefox", "Chrome", "Safari"]
        .iter()
        .find(|&&s| agent.contains(s))
        .map(|&s| match s {
            "Edg" => "Edge",
            "OPR" => "Opera",
            _ => s,
        });
    match (os, browser) {
        (Some(o), Some(b)) => format!("{o} · {b}"),
        (Some(o), None) => o.to_string(),
        (None, Some(b)) => b.to_string(),
        _ => "unknown device".to_string(),
    }
}

pub fn serve(stream: TcpStream, hub: &Arc<Hub>) {
    struct Slot;
    impl Drop for Slot {
        fn drop(&mut self) {
            CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        }
    }
    if CONNECTIONS.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
        CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        return;
    }
    let _slot = Slot;
    let peer = stream
        .peer_addr()
        .ok()
        .map(|addr| match addr.ip() {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => ip.to_string(),
        })
        .unwrap_or_default();
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    while let Some(req) = parse(&mut reader, &peer) {
        // A share link's live page: no cookie, only the token. Before the
        // paired `/ws` below, and it never reaches the pairing check.
        if req.method == "GET"
            && let Some(token) = req
                .path
                .strip_prefix("/s/")
                .and_then(|p| p.strip_suffix("/ws"))
        {
            let Some((window, _)) = hub.collab.view(token) else {
                let _ = write_reply(&mut writer, &Reply::fail(404, "this link has ended"));
                return;
            };
            if req.ws_key.is_empty() {
                let _ = write_reply(
                    &mut writer,
                    &Reply::fail(400, "expected a websocket upgrade"),
                );
                return;
            }
            let _ = reader.get_ref().set_read_timeout(None);
            let origin = Origin::Viewer {
                token: token.to_string(),
                window,
            };
            crate::ws::session(reader, writer, &req, "", "", origin, "", hub);
            return;
        }
        if req.method == "GET" && req.path == "/ws" {
            if foreign_origin(&req) {
                let _ = write_reply(&mut writer, &Reply::fail(403, "cross-origin"));
                return;
            }
            if !hub.check_auth(cookie(&req)) {
                let _ = write_reply(&mut writer, &Reply::fail(401, "unpaired"));
                return;
            }
            if req.ws_key.is_empty() {
                let _ = write_reply(
                    &mut writer,
                    &Reply::fail(400, "expected a websocket upgrade"),
                );
                return;
            }
            let dev = crate::passkey::device(&req);
            if hub.passkeys.locked(dev, &req.peer) {
                let _ = write_reply(&mut writer, &Reply::fail(423, "locked"));
                return;
            }
            hub.record_device(&req.peer, &device(&req.agent));
            // The socket is meant to be silent for minutes at a time; the
            // idle timeout is for a request that never arrives.
            let _ = reader.get_ref().set_read_timeout(None);
            let origin = Origin::Device(collab::clean_id(req.arg("d")));
            let client = collab::clean_id(req.arg("c"));
            let label = collab::nickname(&req.agent);
            crate::ws::session(reader, writer, &req, dev, &client, origin, &label, hub);
            return;
        }
        if let Some(hit) = crate::proxy::hit(&req.raw) {
            use crate::proxy::{self, Gate};
            let cookie_ok = hub.check_auth(cookie(&req));
            let gate = proxy::admit(
                hit,
                cookie_ok,
                crate::passkey::device(&req),
                &hub.key(),
                &hub.ports(),
                |dev| hub.passkeys.locked(dev, &req.peer),
            );
            let reply = match gate {
                Gate::Skip => None,
                Gate::Reply(reply) => Some(reply),
                Gate::Proxy(route) => {
                    if cookie_ok {
                        hub.record_device(&req.peer, &device(&req.agent));
                    }
                    if req.raw.is_upgrade() {
                        // Like the page's own socket: silent for minutes.
                        let _ = reader.get_ref().set_read_timeout(None);
                        if let Err(reply) =
                            proxy::tunnel(&req.method, &req.raw, &route, &mut reader, &writer)
                        {
                            let _ = write_reply(&mut writer, &reply);
                        }
                        return;
                    }
                    match proxy::forward(&req.method, &req.raw, &req.body, &route, &mut writer) {
                        Ok(true) => continue,
                        Ok(false) => return,
                        Err(reply) => Some(reply),
                    }
                }
            };
            if let Some(reply) = reply {
                if write_reply(&mut writer, &reply).is_err() {
                    return;
                }
                continue;
            }
        }
        if foreign_origin(&req) && !matches!(req.method.as_str(), "GET" | "HEAD") {
            if write_reply(&mut writer, &Reply::fail(403, "cross-origin")).is_err() {
                return;
            }
            continue;
        }
        let reply = route(&req, hub);
        if write_reply(&mut writer, &reply).is_err() {
            return;
        }
    }
}

/// A browser names the page a request came from in `Origin`. One that is not
/// this host's is somebody else's page - a previewed dev server (whose
/// `Origin` is `null`: the proxy sandboxes it), another site - and may not
/// write, or open the socket that does. A client that sends none (the CLI,
/// a navigation) is not a page.
fn foreign_origin(req: &Req) -> bool {
    req.raw
        .header("origin")
        .is_some_and(|origin| !crate::passkey::origin_ok(origin, &req.host))
}

/// Everything but the pairing handshake needs the cookie. An unpaired GET
/// gets the pairing page whatever it asked for, so a bookmark to `/#w12`
/// still lands somewhere useful; anything else gets a JSON 401.
fn route(req: &Req, hub: &Arc<Hub>) -> Reply {
    let pair_page = || Reply {
        status: 200,
        kind: "text/html; charset=utf-8",
        body: PAIR_PAGE.as_bytes().to_vec(),
        // Answers any path for an unpaired device: the service worker must
        // not keep it as the offline copy of `/` or of a script.
        headers: vec![("x-taix-pair", "1".into())],
    };
    let authed = hub.check_auth(cookie(req));
    if authed {
        hub.record_device(&req.peer, &device(&req.agent));
    }
    let locked = authed && hub.passkeys.locked(crate::passkey::device(req), &req.peer);
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") if hub.check_auth(req.arg("k")) => Reply::paired(&hub.key()),
        ("POST", "/pair") => {
            let body = String::from_utf8_lossy(&req.body);
            let key = body
                .split('&')
                .find_map(|pair| pair.strip_prefix("k="))
                .map(decode)
                .unwrap_or_default();
            if hub.check_auth(key.trim()) {
                Reply::paired(&hub.key())
            } else {
                pair_page()
            }
        }
        ("GET", "/pair/state") => {
            let state = hub.pair_state(&req.peer);
            let label = device(&req.agent);
            Reply::json(
                200,
                format!(
                    r#"{{"state":"{}","device":"{}","ip":"{}"}}"#,
                    state,
                    label.replace('\\', "\\\\").replace('"', "\\\""),
                    req.peer.replace('\\', "\\\\").replace('"', "\\\"")
                ),
            )
        }
        // What makes the page installable, before the cookie check: a
        // browser fetches the manifest without credentials, and got the
        // pairing page instead - so TaiX was never installable at all.
        // None of it says anything about the session.
        ("GET", path @ ("/manifest.webmanifest" | "/sw.js")) => {
            let i = ASSETS.iter().position(|a| a.0 == path).unwrap_or_default();
            let mut reply = asset(i, req);
            if path == "/sw.js" {
                reply.headers.push(("service-worker-allowed", "/".into()));
            }
            reply
        }
        ("GET", path) if let Some((_, kind, bytes)) = IMAGES.iter().find(|i| i.0 == path) => {
            Reply {
                status: 200,
                kind,
                body: bytes.to_vec(),
                // Changes with a rebuild at most, and a launcher asks often.
                headers: vec![("cache-control", "max-age=86400".into())],
            }
        }
        // A share link: the token is the credential, and nothing here sets
        // the pairing cookie. The only route that answers before the check.
        ("GET", path) if path.starts_with("/s/") => visit(req, hub),
        _ if !authed => {
            if req.method == "GET" {
                if hub.approved(&req.peer) {
                    Reply::paired(&hub.key())
                } else {
                    hub.pair_request(&req.peer, &device(&req.agent));
                    pair_page()
                }
            } else {
                Reply::fail(401, "unpaired")
            }
        }
        (_, path) if path.starts_with("/passkey/") => crate::passkey::route(req, hub, locked),
        // A locked device may still load the page, so it can draw its lock
        // screen; anything that touches a session waits for the passkey.
        (method, path)
            if locked
                && !(method == "GET"
                    && (path == "/health"
                        || path.starts_with("/icons/")
                        || ASSETS.iter().any(|a| a.0 == path))) =>
        {
            Reply::fail(423, "locked")
        }
        ("GET", "/health") => Reply::json(
            200,
            serde_json::to_string(&hub.health()).unwrap_or_default(),
        ),
        ("POST", "/cmd") => match serde_json::from_slice::<proto::Cmd>(&req.body) {
            Ok(cmd) => {
                hub.command(cmd, &Origin::Device(collab::clean_id(req.arg("d"))));
                Reply::json(200, "{\"ok\":true}".into())
            }
            Err(e) => Reply::fail(400, &e.to_string()),
        },
        ("GET", "/") if !locked && let Some(head) = hub.head_json() => boot(req, &head),
        ("GET", path) if let Some(i) = ASSETS.iter().position(|a| a.0 == path) => asset(i, req),
        ("GET", path) if let Some(name) = path.strip_prefix("/icons/") => {
            let name = name.trim_end_matches(".svg");
            match hub.icons().iter().find(|(id, _)| *id == name) {
                Some((_, svg)) => Reply {
                    status: 200,
                    kind: "image/svg+xml",
                    body: svg.as_bytes().to_vec(),
                    headers: Vec::new(),
                },
                None => Reply::fail(404, "no such icon"),
            }
        }
        ("GET", path) | ("POST", path) if path.starts_with("/api/") => crate::api::route(req, hub),
        (_, path) if path.starts_with("/collab/") => collab::route(req, hub),
        ("GET", path) | ("POST", path) if path.starts_with("/push/") => {
            crate::push::route(req, hub)
        }
        _ => Reply::fail(404, "not found"),
    }
}

const LINK_GONE: &str = "<!doctype html><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width,initial-scale=1\">\
<body style=\"background:#0b0f12;color:#a8b1b8;font:16px sans-serif;display:grid;place-items:center;height:100vh\">\
<p>This link has expired or was revoked.</p>";

/// `/s/<token>[/...]`: the read-only page for one shared window and the two
/// assets it needs. The token is the whole credential, and this never sets
/// the pairing cookie - a visitor gets a page, not a place in the house.
/// The live part is the WebSocket at `/s/<token>/ws`, answered in `serve`.
fn visit(req: &Req, hub: &Hub) -> Reply {
    let rest = req.path.strip_prefix("/s/").unwrap_or_default();
    let (token, tail) = rest.split_once('/').unwrap_or((rest, ""));
    if hub.collab.view(token).is_none() {
        return Reply {
            status: 404,
            kind: HTML,
            body: LINK_GONE.as_bytes().to_vec(),
            headers: Vec::new(),
        };
    }
    // Relative asset URLs in the page resolve under the token only with the
    // trailing slash.
    if !rest.contains('/') {
        return Reply {
            status: 302,
            kind: "text/plain",
            body: Vec::new(),
            headers: vec![("location", format!("/s/{token}/"))],
        };
    }
    let name = match tail {
        "" => "/viewer.html",
        "style.css" => "/style.css",
        "js/viewer.js" => "/js/viewer.js",
        _ => return Reply::fail(404, "not found"),
    };
    match ASSETS.iter().position(|a| a.0 == name) {
        Some(i) => asset(i, req),
        None => Reply::fail(404, "not found"),
    }
}

fn write_reply(w: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\n",
        reply.status,
        reason(reply.status),
        reply.kind,
        reply.body.len()
    );
    let mut has_cache = false;
    for (name, value) in &reply.headers {
        if name.eq_ignore_ascii_case("cache-control") {
            has_cache = true;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    if !has_cache {
        head.push_str("cache-control: no-store\r\n");
    }
    head.push_str("connection: keep-alive\r\n\r\n");
    w.write_all(head.as_bytes())?;
    if reply.status != 304 {
        w.write_all(&reply.body)?;
    }
    w.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        423 => "Locked",
        307 => "Temporary Redirect",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "OK",
    }
}

/// Read one request. `None` ends the connection: a closed socket, a
/// timeout, or anything we will not answer.
fn parse<R: Read>(reader: &mut BufReader<R>, peer: &str) -> Option<Req> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    if Instant::now() > deadline {
        taix_core::trace!("request timeout {}", peer);
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut length = 0usize;
    let mut cookies = HashMap::new();
    let mut accept_encoding = String::new();
    let mut if_none_match = String::new();
    let mut agent = String::new();
    let mut ws_key = String::new();
    let mut host = String::new();
    let mut raw_headers = Vec::new();
    let mut header_count = 0;
    let mut header_bytes = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        if Instant::now() > deadline {
            taix_core::trace!("request timeout {}", peer);
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        header_count += 1;
        header_bytes += header.len();
        if header_count > MAX_HEADERS {
            taix_core::trace!("header count exceeded {} {}", peer, header_count);
            return None;
        }
        if header_bytes > MAX_HEADER_BYTES {
            taix_core::trace!("header bytes exceeded {} {}", peer, header_bytes);
            return None;
        }
        if let Some((name, value)) = header.split_once(':') {
            raw_headers.push((name.to_string(), value.trim().to_string()));
            let name_lower = name.to_ascii_lowercase();
            if name_lower == "content-length" {
                length = value.trim().parse().ok()?;
            } else if name_lower == "cookie" {
                for pair in value.split(';') {
                    if let Some((k, v)) = pair.trim().split_once('=') {
                        cookies.insert(k.to_string(), v.to_string());
                    }
                }
            } else if name_lower == "accept-encoding" {
                accept_encoding = value.trim().to_string();
            } else if name_lower == "if-none-match" {
                if_none_match = value.trim().to_string();
            } else if name_lower == "user-agent" {
                agent = value.trim().to_string();
            } else if name_lower == "host" {
                host = value.trim().to_string();
            } else if name_lower == "sec-websocket-key" {
                ws_key = value.trim().to_string();
            }
        }
    }
    // A body big enough to matter is a bug or an attack, and neither
    // deserves an allocation - except on the one route whose body *is* the
    // payload: a phone camera's JPEG is routinely past 4 MiB.
    let cap = if target.starts_with("/api/upload") {
        32 << 20
    } else {
        4 << 20
    };
    if length > cap {
        taix_core::trace!("body length exceeded {} {} bytes", peer, length);
        return None;
    }
    let mut body = Vec::with_capacity(length.min(BODY_CHUNK));
    let mut read = 0;
    while read < length {
        if Instant::now() > deadline {
            taix_core::trace!("request timeout {}", peer);
            return None;
        }
        let chunk_size = (length - read).min(BODY_CHUNK);
        let start = body.len();
        body.resize(start + chunk_size, 0);
        if reader
            .read_exact(&mut body[start..start + chunk_size])
            .is_err()
        {
            return None;
        }
        read += chunk_size;
    }
    let (path, raw_query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    Some(Req {
        method,
        path: decode(path),
        query: raw_query
            .split('&')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let (key, value) = part.split_once('=').unwrap_or((part, ""));
                (decode(key), decode(value))
            })
            .collect(),
        body,
        cookies,
        accept_encoding,
        if_none_match,
        peer: peer.to_string(),
        agent,
        ws_key,
        host,
        raw: crate::proxy::Raw {
            target,
            headers: raw_headers,
        },
    })
}

/// Percent-decoding, plus `+` for a space in a query string. Invalid
/// escapes are kept verbatim rather than dropped: a path is about to be
/// compared against the filesystem, and silently mangling it would turn a
/// typo into the wrong file.
fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_escapes_and_plus_become_bytes() {
        assert_eq!(decode("/api/files"), "/api/files");
        assert_eq!(decode("a%20b+c"), "a b c");
        assert_eq!(decode("%2Fhome%2Fuser%2Fsrc"), "/home/user/src");
        // A stray `%` is data, not an error.
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }

    fn req(method: &str, path: &str, query: &[(&str, &str)], cookie: &str) -> Req {
        let pair = |(k, v): &(&str, &str)| (k.to_string(), v.to_string());
        Req {
            method: method.into(),
            path: path.into(),
            query: query.iter().map(pair).collect(),
            body: Vec::new(),
            cookies: cookie
                .split_once('=')
                .map(|kv| pair(&kv))
                .into_iter()
                .collect(),
            accept_encoding: String::new(),
            if_none_match: String::new(),
            peer: String::new(),
            agent: String::new(),
            ws_key: String::new(),
            raw: Default::default(),
            host: String::new(),
        }
    }

    #[test]
    fn the_page_carries_the_head_inline_and_the_head_cannot_end_the_script() {
        let head = r#"{"host":"</script><!--<script>","n":1}"#;
        let mut accepts_gzip = req("GET", "/", &[], "");
        accepts_gzip.accept_encoding = "gzip".into();
        for (request, zipped) in [(req("GET", "/", &[], ""), false), (accepts_gzip, true)] {
            let reply = boot(&request, head);
            assert_eq!(reply.status, 200);
            let encoded = reply
                .headers
                .iter()
                .any(|(name, _)| *name == "content-encoding");
            assert_eq!(encoded, zipped);
            let body = if zipped {
                let mut out = String::new();
                flate2::read::GzDecoder::new(&reply.body[..])
                    .read_to_string(&mut out)
                    .unwrap();
                out
            } else {
                String::from_utf8(reply.body).unwrap()
            };
            assert!(body.contains("<html") && !body.contains("<!--boot-->"));
            let open = r#"<script id="boot" type="application/json">"#;
            let start = body.find(open).expect("the marker is replaced") + open.len();
            let inner = &body[start..start + body[start..].find("</script>").unwrap()];
            assert!(!inner.contains("</") && !inner.contains("<!"), "{inner}");
            let parsed: serde_json::Value = serde_json::from_str(inner).unwrap();
            assert_eq!(parsed["host"], "</script><!--<script>");
        }
    }

    #[test]
    fn unpaired_requests_stop_at_the_pairing_page() {
        let hub = Arc::new(Hub::detached());
        let key = hub.key().to_string();
        let unpaired = route(&req("GET", "/", &[], ""), &hub);
        assert_eq!(unpaired.status, 200);
        assert!(String::from_utf8_lossy(&unpaired.body).contains("Pair with TaiX"));
        // The service worker keys on this to never store it as the app.
        assert!(
            unpaired
                .headers
                .iter()
                .any(|(name, _)| *name == "x-taix-pair")
        );
        // Browsers fetch these without the cookie; the pairing page in their
        // place makes the app uninstallable.
        for path in [
            "/manifest.webmanifest",
            "/sw.js",
            "/icon-192.png",
            "/splash/750x1334.png",
        ] {
            let reply = route(&req("GET", path, &[], ""), &hub);
            assert!(
                !reply.headers.iter().any(|(name, _)| *name == "x-taix-pair"),
                "{path}"
            );
        }
        assert_eq!(route(&req("POST", "/cmd", &[], ""), &hub).status, 401);
        assert_eq!(
            route(&req("GET", "/js/app.js", &[], "taix=nope"), &hub).status,
            200
        );
        assert!(
            String::from_utf8_lossy(&route(&req("GET", "/js/app.js", &[], "taix=nope"), &hub).body)
                .contains("Pair with TaiX")
        );
        // A key one byte off is not "almost right".
        let mut off = key.clone();
        off.replace_range(0..1, if key.starts_with('0') { "1" } else { "0" });
        assert_eq!(
            route(&req("GET", "/", &[("k", &off)], ""), &hub).status,
            200
        );
        // The link from the desktop sets the cookie; the cookie then opens the app.
        let paired = route(&req("GET", "/", &[("k", &key)], ""), &hub);
        assert_eq!(paired.status, 302);
        assert!(
            paired
                .headers
                .iter()
                .any(|(n, v)| *n == "set-cookie" && v.starts_with(&format!("taix={key};")))
        );
        let app = route(&req("GET", "/", &[], &format!("taix={key}")), &hub);
        assert!(String::from_utf8_lossy(&app.body).contains("<html"));
        assert!(!String::from_utf8_lossy(&app.body).contains("Pair with TaiX"));
    }

    /// A device with a passkey that has not unlocked gets the shell and the
    /// passkey routes, and a 423 for everything that touches a session.
    #[test]
    fn locked_device_reaches_only_the_shell_and_passkey_routes() {
        let hub = Arc::new(Hub::detached());
        let id = "AAAAAAAAAAAAAAAAAAAAAA";
        let file =
            std::env::temp_dir().join(format!("taix-passkeys-gate-{}.json", std::process::id()));
        std::fs::write(
            &file,
            format!(r#"[{{"id":"{id}","rp":"localhost","cred":"c","key":"k","count":0}}]"#),
        )
        .unwrap();
        hub.passkeys.open_at(file.clone());
        let _ = std::fs::remove_file(&file);
        let ask = |method, path| {
            let mut r = req(method, path, &[], &format!("taix={}", hub.key()));
            r.cookies.insert("taix-dev".into(), id.into());
            route(&r, &hub).status
        };
        for (method, path) in [
            ("POST", "/cmd"),
            ("GET", "/api/files"),
            ("POST", "/api/file"),
            ("POST", "/passkey/remove"),
        ] {
            assert_eq!(ask(method, path), 423, "{path}");
        }
        for (method, path) in [
            ("GET", "/"),
            ("GET", "/js/lock.js"),
            ("GET", "/style.css"),
            ("GET", "/health"),
            ("GET", "/passkey/state"),
        ] {
            assert_eq!(ask(method, path), 200, "{path}");
        }
        // Another device on the same pairing key is not this device.
        let other = req("POST", "/cmd", &[], &format!("taix={}", hub.key()));
        assert_ne!(route(&other, &hub).status, 423);
    }

    /// A request as the listener builds it for a device with no cookie.
    fn ask(peer: &str, agent: &str) -> Req {
        let mut r = req("GET", "/", &[], "");
        r.peer = peer.to_string();
        r.agent = agent.to_string();
        r
    }

    const IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

    #[test]
    fn an_unpaired_get_asks_the_desktop() {
        let hub = Arc::new(Hub::detached());
        let reply = route(&ask("10.0.0.9", IPHONE), &hub);
        assert_eq!(reply.status, 200);
        assert!(String::from_utf8_lossy(&reply.body).contains("Pair with TaiX"));
        let pending = hub.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].ip, "10.0.0.9");
        assert_eq!(pending[0].agent, "iPhone · Safari");
    }

    #[test]
    fn allowing_one_address_pairs_only_that_one() {
        let hub = Arc::new(Hub::detached());
        let key = hub.key().to_string();
        let phone = ask("10.0.0.9", IPHONE);
        route(&phone, &hub);
        assert!(hub.approve("10.0.0.9"));
        let reply = route(&phone, &hub);
        assert_eq!(reply.status, 302);
        assert!(
            reply
                .headers
                .iter()
                .any(|(n, v)| *n == "set-cookie" && v.starts_with(&format!("taix={key};")))
        );
        // Approval is a page load, not a session: without the cookie the
        // endpoints stay shut even for an allowed address.
        let mut post = phone;
        post.method = "POST".into();
        post.path = "/cmd".into();
        assert_eq!(route(&post, &hub).status, 401);

        let other = route(&ask("10.0.0.8", IPHONE), &hub);
        assert_eq!(other.status, 200);
        assert!(String::from_utf8_lossy(&other.body).contains("Pair with TaiX"));
    }

    #[test]
    fn a_denied_address_stops_asking() {
        let hub = Arc::new(Hub::detached());
        let phone = ask("10.0.0.9", IPHONE);
        route(&phone, &hub);
        assert!(hub.deny("10.0.0.9"));
        assert!(hub.pending().is_empty());
        route(&phone, &hub);
        assert!(hub.pending().is_empty());
    }

    #[test]
    fn excessive_headers_are_refused() {
        use std::io::Cursor;

        let mut req = b"GET / HTTP/1.1\r\n".to_vec();
        // 65 headers exceeds MAX_HEADERS (64)
        for i in 0..65 {
            req.extend_from_slice(format!("X-Custom-{}: value\r\n", i).as_bytes());
        }
        req.extend_from_slice(b"\r\n");

        let cursor = Cursor::new(req);
        let mut reader = BufReader::new(cursor);
        let result = parse(&mut reader, "127.0.0.1");
        assert!(
            result.is_none(),
            "should reject request with too many headers"
        );
    }

    #[test]
    fn excessive_header_bytes_are_refused() {
        use std::io::Cursor;

        let mut req = b"GET / HTTP/1.1\r\n".to_vec();
        // One header with 17KB value exceeds MAX_HEADER_BYTES (16 KiB)
        let big_value = "x".repeat(17 * 1024);
        req.extend_from_slice(format!("X-Big: {}\r\n", big_value).as_bytes());
        req.extend_from_slice(b"\r\n");

        let cursor = Cursor::new(req);
        let mut reader = BufReader::new(cursor);
        let result = parse(&mut reader, "127.0.0.1");
        assert!(
            result.is_none(),
            "should reject request with too many header bytes"
        );
    }
    #[test]
    fn cookie_authenticated_requests_record_the_device() {
        let hub = Arc::new(Hub::detached());
        let key = hub.key();
        let mut phone = ask("10.0.0.9", IPHONE);
        phone.cookies.insert("taix".into(), key.clone());
        route(&phone, &hub);
        let paired = hub.paired();
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].ip, "10.0.0.9");
        assert_eq!(paired[0].agent, "iPhone · Safari");
        route(&phone, &hub);
        assert_eq!(hub.paired().len(), 1);
    }

    #[test]
    fn rotating_the_key_invalidates_cookies_and_clears_paired() {
        let hub = Arc::new(Hub::detached());
        let key = hub.key();
        let mut phone = ask("10.0.0.9", IPHONE);
        phone.cookies.insert("taix".into(), key.clone());
        route(&phone, &hub);
        assert_eq!(hub.paired().len(), 1);
        hub.rotate_key("newkey000000000000");
        assert!(hub.paired().is_empty());
        assert_eq!(route(&phone, &hub).status, 200);
        assert!(String::from_utf8_lossy(&route(&phone, &hub).body).contains("Pair with TaiX"));
    }

    #[test]
    fn pair_state_reports_waiting_denied_paired() {
        let hub = Arc::new(Hub::detached());
        let key = hub.key();
        let mut phone = ask("10.0.0.9", IPHONE);
        route(&phone, &hub);
        phone.path = "/pair/state".into();
        let reply = route(&phone, &hub);
        assert!(String::from_utf8_lossy(&reply.body).contains(r#""state":"waiting""#));
        hub.deny("10.0.0.9");
        let reply = route(&phone, &hub);
        assert!(String::from_utf8_lossy(&reply.body).contains(r#""state":"denied""#));
        hub.approve("10.0.0.9");
        phone.path = "/".into();
        route(&phone, &hub);
        phone.path = "/pair/state".into();
        phone.cookies.insert("taix".into(), key.clone());
        let reply = route(&phone, &hub);
        assert!(String::from_utf8_lossy(&reply.body).contains(r#""state":"paired""#));
    }

    #[test]
    fn a_share_link_opens_one_page_without_pairing_the_visitor() {
        let hub = Arc::new(Hub::detached());
        let token = hub.collab.share(3, Duration::from_secs(60)).unwrap();
        let visit = |path: &str| {
            let mut r = req("GET", path, &[], "");
            r.peer = "10.0.0.9".into();
            route(&r, &hub)
        };

        let page = visit(&format!("/s/{token}/"));
        assert_eq!(page.status, 200);
        assert!(String::from_utf8_lossy(&page.body).contains("Read-only"));
        assert!(page.headers.iter().all(|(n, _)| *n != "set-cookie"));
        assert_eq!(visit(&format!("/s/{token}/style.css")).status, 200);
        assert_eq!(visit(&format!("/s/{token}")).status, 302);
        // Nothing else of the house is reachable on a link: not a paired
        // asset, and the API still answers with the pairing page.
        assert_eq!(visit(&format!("/s/{token}/js/store.js")).status, 404);
        assert!(String::from_utf8_lossy(&visit("/api/files").body).contains("Pair with TaiX"));

        assert_eq!(visit("/s/0000000000000000/").status, 404);
        assert!(hub.collab.revoke(&token));
        let gone = visit(&format!("/s/{token}/"));
        assert_eq!(gone.status, 404);
        assert!(gone.headers.iter().all(|(n, _)| *n != "set-cookie"));
    }
}
