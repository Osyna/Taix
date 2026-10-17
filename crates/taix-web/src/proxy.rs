//! The preview proxy: `/p/<port>/<path>` is `http://127.0.0.1:<port>/<path>`,
//! so a phone can open a dev server that only listens on the desktop's
//! loopback.
//!
//! A previewed page is somebody else's code running on TaiX's origin, and
//! TaiX's origin can type into terminals. So every proxied response carries
//! `Content-Security-Policy: sandbox ...` without `allow-same-origin`: the
//! page gets an opaque origin, cannot read a TaiX response, and none of its
//! requests carry the `SameSite=Lax` pairing cookie (measured in Chromium:
//! the navigation sends it, every script, stylesheet, image and `fetch` from
//! the sandboxed document does not, and no `Referer` either). The router also
//! refuses a `/ws` upgrade or a write whose `Origin` is not the request's
//! `Host` (`http::serve`).
//!
//! Without a cookie the frame's own subresources need another way in. The
//! entry, `/p/<port>/`, is cookie-authed like everything else; it answers
//! with a redirect to `/p/<port>.<device>.<mac>/`, a link only this server
//! can mint (a MAC of the pairing key, the port and the device), and relative
//! URLs inside the page inherit it. A link works for one port and only while
//! that port is listed, the pairing key is unchanged and the device is not
//! passkey-locked; it is not the pairing key and cannot be turned into it.
//!
//! Only ports a TaiX-managed pane process listens on are reachable
//! (`Hub::ports`, fed from the snapshot): this is a way to *see* what a pane
//! started, not a general port forwarder. The body streams through
//! `io::copy`, so a long-lived response (server-sent events) is delivered as
//! it is produced. A WebSocket upgrade becomes a byte tunnel, which is all
//! hot reload needs.
//!
//! ponytail: the price of the sandbox, all measured in Chromium.
//! - The previewed app has no cookies or localStorage of its own (opaque
//!   origin; `localStorage` throws).
//! - Its own server is cross-origin to it, so `fetch`, XHR and module scripts
//!   only work because the proxy adds `Access-Control-Allow-Origin: *` when
//!   the dev server sent none.
//! - A root-absolute URL (`/assets/x.js`, Vite's `/@vite/client`) cannot be
//!   told from TaiX's own paths: a sandboxed document sends no cookie and no
//!   `Referer`. The dev server must emit relative URLs (Vite `base: './'`,
//!   webpack `publicPath: 'auto'`, CRA `homepage: '.'`), as
//!   `python3 -m http.server` does. Hot reload needs the same, and a
//!   WebSocket URL under the link.
//! - A leaked link works until the pairing key changes or the port stops
//!   being listed. Request bodies are capped at the page's 4 MiB, and a
//!   chunked request body is not supported.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::http::Reply;

/// What every proxied response says about the document it carries. No
/// `allow-same-origin`, and no `allow-top-navigation`: the page may run, post
/// forms, open windows and download, and is nobody in particular.
const SANDBOX: &str = "sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads";

/// A request as it arrived: what the proxy forwards, which `Req` otherwise
/// throws away after picking out the few headers the page cares about.
#[derive(Default)]
pub struct Raw {
    /// The request target as sent, query included and still escaped.
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Raw {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_upgrade(&self) -> bool {
        self.header("upgrade")
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    }
}

/// How long a dev server may take to start answering (a cold compile), and
/// to say anything more once it has.
const BACKEND_IDLE: Duration = Duration::from_secs(300);
/// The response head: more than any server sends, less than a memory problem.
const MAX_HEAD: usize = 64 << 10;

/// A request that wants the proxy.
#[derive(Debug, PartialEq)]
pub struct Hit {
    port: u16,
    /// What follows the prefix: `/a?b`, or `` / `?b` for the bare prefix.
    tail: String,
    /// The device and MAC of a link, or `None` for the entry `/p/<port>/`.
    link: Option<(String, String)>,
}

/// Where an admitted request goes.
#[derive(Debug, PartialEq)]
pub struct Route {
    pub port: u16,
    /// `/p/<port>.<device>.<mac>`: what redirects are pointed back into.
    pub prefix: String,
    /// The path and query the dev server is asked for, starting with `/`.
    pub rest: String,
}

pub enum Gate {
    /// Not the proxy's: carry on to the router.
    Skip,
    /// Refused, or redirected to the link; sandboxed like everything here.
    Reply(Reply),
    Proxy(Route),
}

fn sandboxed(mut reply: Reply) -> Reply {
    reply
        .headers
        .push(("content-security-policy", SANDBOX.to_string()));
    reply
}

fn fail(status: u16, message: &str) -> Reply {
    sandboxed(Reply::fail(status, message))
}

fn redirect(to: String) -> Reply {
    sandboxed(Reply {
        status: 307,
        kind: "text/plain",
        body: Vec::new(),
        headers: vec![("location", to)],
    })
}

/// A `/p/` target: port, the signed device link if any, and the tail.
type Target<'a> = (u16, Option<(&'a str, &'a str)>, &'a str);

/// `/p/8000/a?b` is `(8000, None, "/a?b")`; `/p/8000.<device>.<mac>/a` carries
/// its link. `/p/8000` and `/p/8000?b` leave the tail without its slash, for
/// the caller to redirect.
pub(crate) fn split(target: &str) -> Option<Target<'_>> {
    let rest = target.strip_prefix("/p/")?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (segment, tail) = rest.split_at(end);
    let mut parts = segment.split('.');
    let digits = parts.next()?;
    if digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port = digits.parse::<u16>().ok().filter(|p| *p != 0)?;
    let link = match (parts.next(), parts.next(), parts.next()) {
        (None, ..) => None,
        (Some(dev), Some(mac), None)
            if dev.len() <= 22
                && dev
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && mac.len() == 32
                && mac.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            Some((dev, mac))
        }
        _ => return None,
    };
    Some((port, link, tail))
}

/// Is this request for the proxy?
pub fn hit(raw: &Raw) -> Option<Hit> {
    let (port, link, tail) = split(&raw.target)?;
    Some(Hit {
        port,
        tail: tail.to_string(),
        link: link.map(|(dev, mac)| (dev.to_string(), mac.to_string())),
    })
}

/// The MAC a link carries: only this server, holding the pairing key, can
/// make one, and it names the port and device it was made for.
fn mac(key: &str, port: u16, device: &str) -> String {
    let digest = Sha256::digest(format!("taix-preview\0{key}\0{port}\0{device}"));
    digest.as_slice()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Constant time, like `Hub::check_auth`.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Decide a hit: the checks the router applies to everything - paired (by
/// cookie for the entry, by link for the rest), passkey not engaged - then
/// the allow-list. `locked` is asked about the device the request speaks for.
pub fn admit(
    hit: Hit,
    cookie_ok: bool,
    cookie_device: &str,
    key: &str,
    ports: &[u16],
    locked: impl Fn(&str) -> bool,
) -> Gate {
    let Hit { port, tail, link } = hit;
    let device = match &link {
        Some((device, tag)) => {
            if !same(tag, &mac(key, port, device)) {
                return Gate::Reply(fail(403, "not a preview link of this TaiX"));
            }
            device.as_str()
        }
        // An unpaired device gets the router's pairing flow, not a hint
        // about what is listening.
        None if !cookie_ok => return Gate::Skip,
        None => cookie_device,
    };
    if locked(device) {
        return Gate::Reply(fail(423, "locked"));
    }
    if !ports.contains(&port) {
        return Gate::Reply(fail(403, "no TaiX pane process listens on that port"));
    }
    let prefix = format!("/p/{port}.{device}.{}", mac(key, port, device));
    if link.is_none() || !tail.starts_with('/') {
        let slash = if tail.starts_with('/') { "" } else { "/" };
        return Gate::Reply(redirect(format!("{prefix}{slash}{tail}")));
    }
    Gate::Proxy(Route {
        port,
        prefix,
        rest: tail,
    })
}

/// Headers that describe one connection, not the message (RFC 9110 7.6.1).
fn hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// The names a `Connection` header says are hop-by-hop for this message.
fn connection_tokens(headers: &[(String, String)]) -> Vec<String> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("connection"))
        .flat_map(|(_, v)| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .collect()
}

/// The request line and headers sent to the dev server.
///
/// `Host` names the server, so its virtual-host and CSRF checks see the
/// address it is listening on; `Origin` follows (a sandboxed page sends
/// `null`). The pairing cookie never leaves: a dev server has no business
/// holding the key to the terminals.
fn request_head(
    method: &str,
    raw: &Raw,
    body: usize,
    port: u16,
    rest: &str,
    upgrade: bool,
) -> String {
    let mut head = format!("{method} {rest} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n");
    let listed = if upgrade {
        Vec::new()
    } else {
        connection_tokens(&raw.headers)
    };
    let mut cookies = Vec::new();
    for (name, value) in &raw.headers {
        let key = name.to_ascii_lowercase();
        match key.as_str() {
            "host" | "content-length" | "expect" | "origin" => {}
            "cookie" => cookies.extend(
                value
                    .split(';')
                    .map(str::trim)
                    .filter(|c| !c.is_empty() && !c.starts_with("taix=")),
            ),
            // A tunnel keeps exactly the two headers that ask for one.
            "connection" | "upgrade" if upgrade => {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            _ if hop(&key) || listed.contains(&key) => {}
            _ => head.push_str(&format!("{name}: {value}\r\n")),
        }
    }
    if !cookies.is_empty() {
        head.push_str(&format!("cookie: {}\r\n", cookies.join("; ")));
    }
    if raw.header("origin").is_some() {
        head.push_str(&format!("origin: http://127.0.0.1:{port}\r\n"));
    }
    if body > 0 || matches!(method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("content-length: {body}\r\n"));
    }
    if !upgrade {
        head.push_str("connection: close\r\n");
    }
    head.push_str("\r\n");
    head
}

/// Point a redirect back into the preview: `/login` and
/// `http://127.0.0.1:8000/login` both stay under the link's prefix.
fn relocate(location: &str, route: &Route) -> String {
    let (port, prefix) = (route.port, &route.prefix);
    let location = location.trim();
    if location.starts_with('/') && !location.starts_with("//") {
        return format!("{prefix}{location}");
    }
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        let origin = format!("http://{host}:{port}");
        if let Some(path) = location.strip_prefix(&origin)
            && (path.is_empty() || path.starts_with(['/', '?']))
        {
            let slash = if path.starts_with('/') { "" } else { "/" };
            return format!("{prefix}{slash}{path}");
        }
    }
    location.to_string()
}

/// How the response body ends.
#[derive(Debug, PartialEq)]
enum Framing {
    None,
    Length(u64),
    Chunked,
    Eof,
}

struct Head {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
}

fn line<R: BufRead>(r: &mut R, budget: &mut usize) -> io::Result<String> {
    let mut raw = Vec::new();
    let n = (&mut *r).take(*budget as u64).read_until(b'\n', &mut raw)?;
    if n == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    *budget -= n;
    if !raw.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "response head too long",
        ));
    }
    Ok(String::from_utf8_lossy(&raw).trim_end().to_string())
}

/// The final response head: interim `1xx` ones (`100 Continue`, early
/// hints) are read past, since the browser asked for a page, not a protocol.
fn read_head<R: BufRead>(r: &mut R) -> io::Result<Head> {
    let mut budget = MAX_HEAD;
    loop {
        let status_line = line(r, &mut budget)?;
        let mut parts = status_line.splitn(3, ' ');
        let status = match (
            parts.next(),
            parts.next().and_then(|s| s.parse::<u16>().ok()),
        ) {
            (Some(version), Some(status)) if version.starts_with("HTTP/") => status,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not an HTTP response",
                ));
            }
        };
        let reason = parts.next().unwrap_or_default().to_string();
        let mut headers = Vec::new();
        loop {
            let header = line(r, &mut budget)?;
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.push((name.to_string(), value.trim().to_string()));
            }
        }
        if !(100..200).contains(&status) {
            return Ok(Head {
                status,
                reason,
                headers,
            });
        }
    }
}

/// The head we send the browser, and how to carry the body after it.
///
/// The sandbox goes on as a header of its own: a policy the dev server sent
/// keeps applying beside it (browsers enforce every
/// `Content-Security-Policy` header), and neither can loosen the other.
fn response_head(head: &Head, method: &str, route: &Route) -> (String, Framing) {
    let listed = connection_tokens(&head.headers);
    let find = |name: &str| {
        head.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let framing = if method == "HEAD" || matches!(head.status, 204 | 304) {
        Framing::None
    } else if find("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        Framing::Chunked
    } else if let Some(n) = find("content-length").and_then(|v| v.parse().ok()) {
        Framing::Length(n)
    } else {
        Framing::Eof
    };
    let mut out = format!("HTTP/1.1 {} {}\r\n", head.status, head.reason);
    for (name, value) in &head.headers {
        let key = name.to_ascii_lowercase();
        match key.as_str() {
            // The preview lives in a frame of the TaiX page; a dev server's
            // own refusal to be framed would only blank it.
            "x-frame-options" => {}
            // Never let a page on a dev server replace the pairing cookie.
            "set-cookie" if value.starts_with("taix=") => {}
            "location" => out.push_str(&format!("{name}: {}\r\n", relocate(value, route))),
            _ if hop(&key) || listed.contains(&key) => {}
            _ => out.push_str(&format!("{name}: {value}\r\n")),
        }
    }
    out.push_str(&format!("content-security-policy: {SANDBOX}\r\n"));
    // The sandboxed page's origin is opaque, so its own server is cross-origin
    // to it: `fetch`, XHR and module scripts need this to read anything. It
    // is the dev server's content going to its own page, and no cookie rides.
    if find("access-control-allow-origin").is_none() {
        out.push_str("access-control-allow-origin: *\r\n");
    }
    out.push_str(match framing {
        Framing::Chunked => "transfer-encoding: chunked\r\nconnection: close\r\n",
        Framing::Eof => "connection: close\r\n",
        _ => "connection: keep-alive\r\n",
    });
    out.push_str("\r\n");
    (out, framing)
}

/// Loopback, v4 first: a dev server started on `localhost` is on `::1` alone
/// as often as not.
fn connect(port: u16) -> Result<TcpStream, Reply> {
    let wait = Duration::from_secs(3);
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let stream = TcpStream::connect_timeout(&v4, wait)
        .or_else(|_| TcpStream::connect_timeout(&v6, wait))
        .map_err(|e| fail(502, &format!("nothing answered on port {port}: {e}")))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// Ask the dev server and stream its answer to `client`.
///
/// `Err` is a reply nothing has been written for yet; `Ok` says whether the
/// client connection can carry another request. It cannot after a body of
/// unknown or cut-short length.
pub fn forward(
    method: &str,
    raw: &Raw,
    body: &[u8],
    route: &Route,
    client: &mut impl Write,
) -> Result<bool, Reply> {
    let port = route.port;
    let bad = |e: io::Error| fail(502, &format!("port {port}: {e}"));
    let mut back = connect(port)?;
    let _ = back.set_read_timeout(Some(BACKEND_IDLE));
    let _ = back.set_write_timeout(Some(Duration::from_secs(30)));
    back.write_all(request_head(method, raw, body.len(), port, &route.rest, false).as_bytes())
        .and_then(|()| back.write_all(body))
        .map_err(bad)?;
    let mut back = BufReader::new(back);
    let head = read_head(&mut back).map_err(bad)?;
    let (text, framing) = response_head(&head, method, route);
    if client.write_all(text.as_bytes()).is_err() {
        return Ok(false);
    }
    Ok(match framing {
        Framing::None => true,
        Framing::Length(n) => io::copy(&mut (&mut back).take(n), client).is_ok_and(|c| c == n),
        Framing::Chunked | Framing::Eof => {
            let _ = io::copy(&mut back, client);
            false
        }
    })
}

/// A WebSocket upgrade: the dev server's handshake answer and every frame
/// after it travel as raw bytes, in both directions, until either side hangs
/// up. Returns once both have.
pub fn tunnel(
    method: &str,
    raw: &Raw,
    route: &Route,
    from: &mut impl Read,
    to: &TcpStream,
) -> Result<(), Reply> {
    let port = route.port;
    let back = connect(port)?;
    (&back)
        .write_all(request_head(method, raw, 0, port, &route.rest, true).as_bytes())
        .map_err(|e| fail(502, &format!("port {port}: {e}")))?;
    let hang_up = || {
        let _ = back.shutdown(Shutdown::Both);
        let _ = to.shutdown(Shutdown::Both);
    };
    std::thread::scope(|s| {
        s.spawn(|| {
            let _ = io::copy(&mut &back, &mut &*to);
            hang_up();
        });
        let _ = io::copy(from, &mut &back);
        hang_up();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "test0000000000000000";

    fn raw(target: &str, headers: &[(&str, &str)]) -> Raw {
        Raw {
            target: target.to_string(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn route(port: u16, rest: &str) -> Route {
        Route {
            port,
            prefix: format!("/p/{port}.dev.{}", mac(KEY, port, "dev")),
            rest: rest.to_string(),
        }
    }

    /// A header value, case-insensitively, for every line of that name.
    fn csp_lines(head: &str) -> Vec<&str> {
        head.lines()
            .filter_map(|l| l.strip_prefix("content-security-policy: "))
            .collect()
    }

    #[test]
    fn prefix_splits_into_port_link_and_path() {
        let link = format!("{:0>32}", "ab");
        assert_eq!(split("/p/8000/a/b?c=1"), Some((8000, None, "/a/b?c=1")));
        assert_eq!(split("/p/8000/"), Some((8000, None, "/")));
        assert_eq!(split("/p/8000"), Some((8000, None, "")));
        assert_eq!(split("/p/8000?x=1"), Some((8000, None, "?x=1")));
        let target = format!("/p/8000.Dev_-1.{link}/x");
        assert_eq!(
            split(&target),
            Some((8000, Some(("Dev_-1", link.as_str())), "/x"))
        );
        assert_eq!(
            split(&format!("/p/8000..{link}/")),
            Some((8000, Some(("", link.as_str())), "/"))
        );
        for bad in [
            "/p/",
            "/p//a",
            "/p/0/",
            "/p/65536/",
            "/p/80a/",
            "/p/-1/",
            "/p/123456/",
            "/px/80/",
            "/api/p/80/",
            "/p/8000.dev/",
            "/p/8000.dev.short/",
            "/p/8000.dev.zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz/",
            "/p/8000.d/v.0000000000000000000000000000000a/",
            "/p/8000.dev.00000000000000000000000000000000.x/",
        ] {
            assert_eq!(split(bad), None, "{bad}");
        }
    }

    #[test]
    fn hit_is_only_the_prefix() {
        let stray = raw("/src/main.js", &[("Referer", "http://h:1/p/5173/")]);
        assert_eq!(
            hit(&stray),
            None,
            "no Referer fallback: it cannot be authed"
        );
        let found = hit(&raw("/p/8000/x", &[])).unwrap();
        assert_eq!((found.port, found.tail.as_str()), (8000, "/x"));
    }

    fn gate(target: &str, cookie: bool, locked: &[&str], ports: &[u16]) -> Gate {
        let hit = hit(&raw(target, &[])).unwrap();
        admit(hit, cookie, "dev", KEY, ports, |d| locked.contains(&d))
    }

    fn reply(gate: Gate) -> Reply {
        match gate {
            Gate::Reply(r) => r,
            Gate::Skip => panic!("skipped"),
            Gate::Proxy(r) => panic!("proxied {r:?}"),
        }
    }

    fn sandboxed_header(reply: &Reply) -> bool {
        reply
            .headers
            .iter()
            .any(|(k, v)| *k == "content-security-policy" && v == SANDBOX)
    }

    #[test]
    fn the_entry_is_cookie_authed_and_redirects_to_a_link() {
        let link = format!("/p/8000.dev.{}", mac(KEY, 8000, "dev"));
        let to = |target| {
            let r = reply(gate(target, true, &[], &[8000]));
            assert_eq!(r.status, 307);
            assert!(sandboxed_header(&r), "redirects are sandboxed too");
            r.headers
                .iter()
                .find(|(k, _)| *k == "location")
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(to("/p/8000/a?b=1"), format!("{link}/a?b=1"));
        assert_eq!(to("/p/8000"), format!("{link}/"));
        assert_eq!(to("/p/8000?b=1"), format!("{link}/?b=1"));
        assert!(matches!(gate("/p/8000/a", false, &[], &[8000]), Gate::Skip));
        let locked = reply(gate("/p/8000/a", true, &["dev"], &[8000]));
        assert_eq!(locked.status, 423);
        let unlisted = reply(gate("/p/8000/a", true, &[], &[3000]));
        assert_eq!(unlisted.status, 403);
        assert!(sandboxed_header(&locked) && sandboxed_header(&unlisted));
    }

    #[test]
    fn a_link_needs_no_cookie_but_binds_port_device_and_key() {
        let target = |port, dev: &str, path: &str| {
            format!("/p/{port}.{dev}.{}{path}", mac(KEY, 8000, "dev"))
        };
        let proxied = |g: Gate| match g {
            Gate::Proxy(r) => Some(r),
            _ => None,
        };
        let ok = proxied(gate(&target(8000, "dev", "/a?b"), false, &[], &[8000])).unwrap();
        assert_eq!((ok.port, ok.rest.as_str()), (8000, "/a?b"));
        assert!(ok.prefix.starts_with("/p/8000.dev."));
        // The MAC of one port does not open another, nor another device's.
        assert_eq!(
            reply(gate(&target(9000, "dev", "/"), false, &[], &[9000])).status,
            403
        );
        assert_eq!(
            reply(gate(&target(8000, "other", "/"), false, &[], &[8000])).status,
            403
        );
        let other_key = hit(&raw(&target(8000, "dev", "/"), &[])).unwrap();
        let g = admit(other_key, true, "dev", "another key", &[8000], |_| false);
        assert_eq!(reply(g).status, 403);
        // Still subject to the lock and the allow-list.
        assert_eq!(
            reply(gate(&target(8000, "dev", "/"), false, &["dev"], &[8000])).status,
            423
        );
        assert_eq!(
            reply(gate(&target(8000, "dev", "/"), false, &[], &[])).status,
            403
        );
        // A link with no slash after it gets one.
        let bare = reply(gate(&target(8000, "dev", ""), false, &[], &[8000]));
        assert_eq!(bare.status, 307);
        assert!(sandboxed_header(&bare));
    }

    #[test]
    fn request_drops_hop_headers_the_pairing_cookie_and_rewrites_host() {
        let r = raw(
            "/p/8000/x",
            &[
                ("Host", "phone.lan:4040"),
                ("Connection", "keep-alive, X-Secret"),
                ("X-Secret", "1"),
                ("Keep-Alive", "timeout=5"),
                ("Transfer-Encoding", "chunked"),
                ("Cookie", "taix=KEY; theme=dark"),
                ("Origin", "null"),
                ("Accept", "text/html"),
                ("Content-Length", "99"),
                ("Expect", "100-continue"),
            ],
        );
        let head = request_head("POST", &r, 3, 8000, "/x", false);
        assert!(
            head.starts_with("POST /x HTTP/1.1\r\nhost: 127.0.0.1:8000\r\n"),
            "{head}"
        );
        assert!(head.contains("Accept: text/html\r\n"));
        assert!(head.contains("cookie: theme=dark\r\n"));
        assert!(head.contains("origin: http://127.0.0.1:8000\r\n"));
        assert!(head.contains("content-length: 3\r\n"));
        assert!(head.ends_with("connection: close\r\n\r\n"));
        for gone in [
            "phone.lan",
            "X-Secret",
            "Keep-Alive",
            "Transfer-Encoding",
            "taix=",
            "99",
            "Expect",
            "null",
        ] {
            assert!(!head.contains(gone), "{gone} in {head}");
        }
    }

    #[test]
    fn a_tunnel_keeps_the_upgrade_headers() {
        let r = raw(
            "/p/8000/",
            &[
                ("Connection", "Upgrade"),
                ("Upgrade", "websocket"),
                ("Sec-WebSocket-Key", "abc"),
                ("Sec-WebSocket-Protocol", "vite-hmr"),
            ],
        );
        assert!(r.is_upgrade());
        let head = request_head("GET", &r, 0, 8000, "/", true);
        for kept in [
            "Connection: Upgrade",
            "Upgrade: websocket",
            "Sec-WebSocket-Key: abc",
            "Sec-WebSocket-Protocol: vite-hmr",
        ] {
            assert!(head.contains(kept), "{kept} in {head}");
        }
        assert!(!head.contains("connection: close"));
    }

    #[test]
    fn redirects_stay_inside_the_link() {
        let r = route(8000, "/");
        let p = &r.prefix;
        assert_eq!(relocate("/login?next=/", &r), format!("{p}/login?next=/"));
        assert_eq!(relocate("http://127.0.0.1:8000/a", &r), format!("{p}/a"));
        assert_eq!(relocate("http://localhost:8000", &r), format!("{p}/"));
        assert_eq!(
            relocate("http://localhost:8000?x=1", &r),
            format!("{p}/?x=1")
        );
        assert_eq!(
            relocate("http://localhost:80001/a", &r),
            "http://localhost:80001/a"
        );
        assert_eq!(relocate("//cdn.example/x", &r), "//cdn.example/x");
        assert_eq!(relocate("https://example.com/", &r), "https://example.com/");
    }

    #[test]
    fn the_sandbox_is_its_own_header_beside_the_dev_servers_policy() {
        let head = Head {
            status: 200,
            reason: "OK".into(),
            headers: vec![
                (
                    "Content-Security-Policy".into(),
                    "default-src 'self'".into(),
                ),
                ("Content-Length".into(), "0".into()),
            ],
        };
        let (text, _) = response_head(&head, "GET", &route(8000, "/"));
        assert!(text.contains("Content-Security-Policy: default-src 'self'\r\n"));
        assert_eq!(csp_lines(&text), [SANDBOX], "{text}");
        assert_eq!(text.matches("access-control-allow-origin: *").count(), 1);
        let own = Head {
            status: 200,
            reason: "OK".into(),
            headers: vec![("Access-Control-Allow-Origin".into(), "http://x".into())],
        };
        let (text, _) = response_head(&own, "GET", &route(8000, "/"));
        assert!(text.contains("Access-Control-Allow-Origin: http://x\r\n") && !text.contains("*"));
        assert!(
            !SANDBOX.contains("allow-same-origin") && !SANDBOX.contains("allow-top-navigation")
        );
    }

    /// One dev server answer through `forward`, over real loopback sockets.
    fn exchange(answer: &'static [u8], method: &str) -> (String, bool) {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let backend = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            s.write_all(answer).unwrap();
            String::from_utf8_lossy(&seen).to_string()
        });
        let mut out = Vec::new();
        let keep = forward(
            method,
            &raw("/p/1/x", &[("Cookie", "taix=K")]),
            b"",
            &route(port, "/x"),
            &mut out,
        )
        .unwrap_or_else(|r| panic!("forward failed: {}", r.status));
        let seen = backend.join().unwrap();
        assert!(
            seen.starts_with(&format!(
                "{method} /x HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n"
            )),
            "{seen}"
        );
        assert!(!seen.contains("taix="));
        (String::from_utf8(out).unwrap(), keep)
    }

    #[test]
    fn a_counted_body_streams_through_and_keeps_the_connection() {
        let (out, keep) = exchange(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\nX-Frame-Options: DENY\r\nLocation: /next\r\n\r\nhello",
            "GET",
        );
        assert!(keep);
        assert!(out.starts_with("HTTP/1.1 200 OK\r\n"), "{out}");
        assert!(out.ends_with("\r\n\r\nhello"), "{out}");
        assert!(out.contains("Content-Length: 5\r\n") && out.contains("Location: /p/"));
        assert!(out.contains("connection: keep-alive") && !out.contains("Connection: close"));
        assert!(!out.contains("X-Frame-Options"));
        assert_eq!(csp_lines(&out), [SANDBOX], "{out}");
    }

    #[test]
    fn a_chunked_body_is_passed_chunked_and_closes() {
        let (out, keep) = exchange(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
            "GET",
        );
        assert!(!keep);
        assert!(
            out.contains("transfer-encoding: chunked\r\nconnection: close\r\n"),
            "{out}"
        );
        assert!(out.ends_with("\r\n\r\n5\r\nhello\r\n0\r\n\r\n"), "{out}");
        assert_eq!(csp_lines(&out), [SANDBOX], "{out}");
    }

    #[test]
    fn a_short_body_and_a_head_request_are_told_apart() {
        let (_, keep) = exchange(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhi", "GET");
        assert!(!keep, "cut short: the connection cannot be reused");
        let (out, keep) = exchange(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n", "HEAD");
        assert!(
            keep && out.ends_with("\r\n\r\n") && !out.contains("hello"),
            "{out}"
        );
    }

    #[test]
    fn interim_responses_are_read_past() {
        let (out, _) = exchange(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n",
            "GET",
        );
        assert!(out.starts_with("HTTP/1.1 204 No Content\r\n"), "{out}");
    }

    /// The whole path a browser takes: the real listener loop, router and
    /// proxy, a dev server behind it. The entry takes the cookie and hands
    /// back a link; the link works with no cookie at all, as a sandboxed
    /// frame's requests do, and every answer says so. Then the lock, the
    /// allow-list, a foreign `Origin`, and a WebSocket upgrade.
    #[test]
    fn served_requests_reach_only_listed_ports_and_upgrades_tunnel() {
        use std::net::TcpListener;
        use std::sync::Arc;

        let dev = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = dev.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in dev.incoming() {
                let mut s = stream.unwrap();
                let mut seen = Vec::new();
                let mut buf = [0u8; 1024];
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut buf).unwrap();
                    seen.extend_from_slice(&buf[..n]);
                }
                if String::from_utf8_lossy(&seen).contains("Upgrade: websocket") {
                    s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n")
                        .unwrap();
                    // An echo: whatever comes through the tunnel goes back.
                    let _ = io::copy(&mut s.try_clone().unwrap(), &mut s);
                } else {
                    s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .unwrap();
                }
            }
        });

        let hub = Arc::new(crate::Hub::detached());
        let key = hub.key();
        *hub.ports.lock() = vec![port];
        let front = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let front_addr = front.local_addr().unwrap();
        let served = hub.clone();
        std::thread::spawn(move || {
            for stream in front.incoming() {
                let (stream, hub) = (stream.unwrap(), served.clone());
                std::thread::spawn(move || crate::http::serve(stream, &hub));
            }
        });
        let ask = |request: String| {
            let mut s = TcpStream::connect(front_addr).unwrap();
            s.write_all(request.as_bytes()).unwrap();
            s.shutdown(Shutdown::Write).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        let get = |path: String, cookie: &str| {
            ask(format!(
                "GET {path} HTTP/1.1\r\nHost: h\r\nCookie: {cookie}\r\n\r\n"
            ))
        };
        let header = |out: &str, name: &str| {
            out.lines()
                .find_map(|l| l.strip_prefix(name))
                .map(str::to_string)
        };
        let cookie = format!("taix={key}");

        // The entry: cookie in, link out, sandboxed.
        let entry = get(format!("/p/{port}/x?q=1"), &cookie);
        assert!(entry.starts_with("HTTP/1.1 307"), "{entry}");
        assert_eq!(
            header(&entry, "content-security-policy: ").as_deref(),
            Some(SANDBOX)
        );
        let link = header(&entry, "location: ").unwrap();
        assert!(
            link.starts_with(&format!("/p/{port}..")) && link.ends_with("/x?q=1"),
            "{link}"
        );

        // The link: no cookie, the dev server's answer, sandboxed.
        let page = get(link.clone(), "");
        assert!(
            page.starts_with("HTTP/1.1 200 OK") && page.ends_with("\r\n\r\nok"),
            "{page}"
        );
        assert_eq!(
            header(&page, "content-security-policy: ").as_deref(),
            Some(SANDBOX)
        );
        // A forged link does not work, cookie or not.
        let forged = link.replace(&format!("/p/{port}."), &format!("/p/{port}.x"));
        assert!(get(forged, &cookie).starts_with("HTTP/1.1 403"));

        let unlisted = get(format!("/p/{}/x", port.wrapping_add(1).max(1)), &cookie);
        assert!(unlisted.starts_with("HTTP/1.1 403"), "{unlisted}");
        assert_eq!(
            header(&unlisted, "content-security-policy: ").as_deref(),
            Some(SANDBOX)
        );
        let stranger = get(format!("/p/{port}/x"), "taix=wrong");
        assert!(
            !stranger.ends_with("ok") && !stranger.starts_with("HTTP/1.1 3"),
            "{stranger}"
        );

        // A write or an upgrade from another origin is refused outright.
        let post = |origin: &str| {
            ask(format!(
                "POST /cmd HTTP/1.1\r\nHost: h\r\nCookie: {cookie}\r\nOrigin: {origin}\r\nContent-Length: 2\r\n\r\n{{}}"
            ))
        };
        for origin in ["null", "http://evil.example", "http://h.evil.example"] {
            assert!(post(origin).starts_with("HTTP/1.1 403"), "{origin}");
        }
        assert!(!post("http://h").starts_with("HTTP/1.1 403"));
        let upgrade = |origin: &str| {
            ask(format!(
                "GET /ws HTTP/1.1\r\nHost: h\r\nCookie: {cookie}\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: abc\r\n\r\n"
            ))
        };
        assert!(upgrade("null").starts_with("HTTP/1.1 403"));

        // The tunnel: bytes written after the handshake come back echoed.
        let mut s = TcpStream::connect(front_addr).unwrap();
        s.write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: h\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
                link.strip_suffix("x?q=1").unwrap()
            )
            .as_bytes(),
        )
        .unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.starts_with("HTTP/1.1 101"), "{status}");
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header == "\r\n" {
                break;
            }
        }
        s.write_all(b"ping").unwrap();
        let mut echoed = [0u8; 4];
        reader.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");
    }
}
