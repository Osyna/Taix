//! The live channel: one WebSocket per page, snapshots down and commands up.
//!
//! RFC 6455 is small when only one side's choices have to be handled: the
//! server never masks or fragments, and reassembles what a browser sends.
//! SHA-1 and base64 exist for the handshake's twenty bytes and nothing else,
//! so they live here rather than as two dependencies. Compression is
//! permessage-deflate (RFC 7692), which every browser offers: a screen of
//! mostly repeated rows then costs a phone's link next to nothing.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress};
use parking_lot::Mutex;
use serde::Deserialize;

use crate::collab::{self, Origin};
use crate::http::Req;
use crate::proto::Snapshot;
use crate::{Head, Hub, object, proto};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A message past this is a bug or an attack; `POST /cmd` has the same cap.
const MAX_MESSAGE: usize = 4 << 20;

/// A ping after this many idle wake-ups of the hub (500 ms each): often
/// enough that a phone's NAT and any proxy keep the connection, rare
/// enough to cost nothing.
const PING_EVERY: u32 = 30;

/// A browser that stops reading must not park the writer forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Unsent bytes the kernel may hold for one page. Past it the writer blocks
/// and the frames that would have queued behind are folded into the next
/// delta: a phone on a slow link sees the newest screen a little late, not
/// every stale screen in order seconds late.
const LOW_WATER: libc::c_int = 16 << 10;

/// Close code for a share link that expired or was revoked; the visitor's
/// page stops reconnecting on it.
const LINK_ENDED: u16 = 4404;
/// How often a link visitor's window is declared watched. The hub forgets
/// a watch after 25 s.
const VIEW_WATCH: Duration = Duration::from_secs(10);

const TEXT: u8 = 0x1;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;
/// RSV1 in a data frame's first byte: this message is compressed.
const RSV1: u8 = 0x40;

/// Shorter than this a message goes out plain: a pong or a one-row edit
/// costs more compressed than as it is.
const COMPRESS_MIN: usize = 64;

/// What a sync flush ends a deflate stream with. RFC 7692 leaves it off
/// the wire, and the receiver puts it back.
const TAIL: [u8; 4] = [0, 0, 0xff, 0xff];

/// Span styles one socket may be told about before it starts over.
const MAX_STYLES: usize = 4096;

/// Answer the upgrade, then serve the page until either side hangs up.
///
/// Snapshots go out on this thread, parked on the hub like every other
/// stream; commands come in on a second one, so a large frame being
/// written never holds up a keystroke.
///
/// `origin` says who is on the other end. A paired device gets what its
/// page (`client`, the id its watch commands carry) has not seen yet, can
/// be sent notices, and may command; a link visitor gets frames cut down to
/// its one window and a hub that refuses its commands. `dev` is the passkey
/// device cookie, `label` what the browser calls itself.
#[allow(clippy::too_many_arguments)] // all plain request facts; a struct would only rename them
pub(crate) fn session<R: Read + Send + 'static>(
    mut reader: R,
    mut w: TcpStream,
    req: &Req,
    dev: &str,
    client: &str,
    origin: Origin,
    label: &str,
    hub: &Arc<Hub>,
) {
    let deflate = req
        .raw
        .header("sec-websocket-extensions")
        .and_then(negotiate);
    let extension = match deflate {
        None => "",
        Some(false) => "sec-websocket-extensions: permessage-deflate\r\n",
        Some(true) => {
            "sec-websocket-extensions: permessage-deflate; server_no_context_takeover\r\n"
        }
    };
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         upgrade: websocket\r\n\
         connection: Upgrade\r\n\
         sec-websocket-accept: {}\r\n{extension}\r\n",
        accept(&req.ws_key)
    );
    if w.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = w.set_write_timeout(Some(WRITE_TIMEOUT));
    low_water(&w);
    let writer = Arc::new(Mutex::new(Out {
        w,
        deflate: deflate.map(Deflate::new),
    }));
    let closed = Arc::new(AtomicBool::new(false));
    let dev = dev.to_string();
    let up = {
        let (writer, closed, hub, dev) = (writer.clone(), closed.clone(), hub.clone(), dev.clone());
        let origin = origin.clone();
        std::thread::spawn(move || {
            commands(&mut reader, &writer, &dev, &origin, &hub, deflate.is_some());
            closed.store(true, Ordering::Relaxed);
            let _ = writer.lock().w.shutdown(Shutdown::Both);
        })
    };
    hub.joined();
    // A paired device can be handed things: it is on the list from here.
    let conn = match &origin {
        Origin::Device(id) if !id.is_empty() => {
            let out = writer.clone();
            let send = Arc::new(move |text: &str| {
                let _ = out.lock().text(text);
            });
            Some(hub.collab.join(id, label, send))
        }
        _ => None,
    };
    // A link visitor: when its link ends, and what it was last sent.
    let mut ends = 0;
    let mut last = String::new();
    let mut page = Page::default();
    let mut watched: Option<Instant> = None;
    let (mut seen, mut idle) = (0, 0);
    while let Some(wake) = hub.wait_frame(seen) {
        if closed.load(Ordering::Relaxed) {
            break;
        }
        // Idle past the limit with the socket still open: end it here.
        if hub.passkeys.is_locked(&dev) {
            locked_close(&writer);
            break;
        }
        if let Origin::Viewer { token, .. } = &origin {
            // Revoked or expired: say so, and hang up. Checked on every
            // wake, idle ones included, so the end is prompt.
            let Some((_, left)) = hub.collab.view(token) else {
                let mut payload = LINK_ENDED.to_be_bytes().to_vec();
                payload.extend_from_slice(b"link ended");
                let _ = writer.lock().control(CLOSE, &payload);
                break;
            };
            if ends == 0 {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                ends = (now + left).as_secs();
            }
            // The desktop only captures windows somebody says they are
            // looking at, and the visitor's page cannot say it itself.
            if watched.is_none_or(|at| at.elapsed() >= VIEW_WATCH) {
                watched = Some(Instant::now());
                hub.command(
                    proto::Cmd::Watch {
                        client: String::new(),
                        windows: Vec::new(),
                    },
                    &origin,
                );
            }
        }
        let sent = if wake.rev == seen {
            idle += 1;
            if idle < PING_EVERY {
                continue;
            }
            idle = 0;
            writer.lock().control(PING, b"")
        } else {
            idle = 0;
            seen = wake.rev;
            let Some(snap) = &wake.snap else { continue };
            let text = match &origin {
                // Unchanged for this visitor: most frames are about
                // somebody else's window.
                Origin::Viewer { window, .. } => {
                    let text = collab::viewer_frame(snap, *window, ends);
                    if text == last {
                        continue;
                    }
                    last = text.clone();
                    text
                }
                Origin::Device(_) => match page.update(&wake.head, snap, &hub.watching(client)) {
                    Some(text) => text,
                    None => continue,
                },
            };
            writer.lock().text(&text)
        };
        if sent.is_err() {
            break;
        }
    }
    if let Some(conn) = conn {
        hub.collab.leave(conn);
    }
    hub.left();
    let _ = writer.lock().w.shutdown(Shutdown::Both);
    let _ = up.join();
}

/// What one page holds: the head sections it was last sent, the span
/// styles it has been told, and the rows of each pane it watches. Lives
/// with the socket, so a page that reconnects starts from nothing and is
/// sent every section, style and whole screen again.
#[derive(Default)]
pub(crate) struct Page {
    head: Arc<Head>,
    /// Span attributes to the index this page knows them by. Plain text is
    /// 0 and has no entry.
    styles: HashMap<String, usize>,
    panes: HashMap<i64, Held>,
}

struct Held {
    /// `cols`, `rows`, `scroll`, `mouse`, `alt`.
    meta: (usize, usize, usize, bool, bool),
    /// Each row as the JSON the page was sent.
    lines: Vec<String>,
}

impl Page {
    /// The message that brings this page up to `snap`, or `None` when
    /// nothing it shows has moved: `{"head": sections, "styles": new,
    /// "panes": [delta]}`, each only when there is something to say. A
    /// pane is a whole screen (`lines`) the first time, then `n` rows of
    /// which `edit` lists the ones that changed, after scrolling the held
    /// rows up by `shift` when that says it shorter. Panes outside `watch`
    /// are never sent; a spinner costs one row, not a screen.
    pub(crate) fn update(
        &mut self,
        head: &Arc<Head>,
        snap: &Snapshot,
        watch: &[i64],
    ) -> Option<String> {
        // A program painting true colour would grow the table for as long
        // as the page stays open: start over, with whole screens.
        if self.styles.len() > MAX_STYLES {
            self.styles.clear();
            self.panes.clear();
        }
        self.panes
            .retain(|id, _| watch.contains(id) && snap.panes.iter().any(|p| p.id == *id));
        let mut fresh = Vec::new();
        let mut panes = String::new();
        for pane in snap.panes.iter().filter(|p| watch.contains(&p.id)) {
            let styles: Vec<usize> = pane
                .screen
                .attrs
                .iter()
                .map(|attr| self.style(attr, &mut fresh))
                .collect();
            let lines: Vec<String> = pane
                .screen
                .rows
                .iter()
                .map(|row| encode_row(row, &styles))
                .collect();
            let meta = (pane.cols, pane.rows, pane.scroll, pane.mouse, pane.alt);
            let mut delta = format!(
                "{{\"id\":{},\"cols\":{},\"rows\":{},\"scroll\":{},\"mouse\":{},\"alt\":{}",
                pane.id, pane.cols, pane.rows, pane.scroll, pane.mouse, pane.alt
            );
            match self.panes.get_mut(&pane.id) {
                Some(held) => {
                    let (shift, edit) = diff(&held.lines, &lines);
                    if shift == 0
                        && edit.is_empty()
                        && held.lines.len() == lines.len()
                        && held.meta == meta
                    {
                        continue;
                    }
                    let _ = write!(delta, ",\"n\":{}", lines.len());
                    if shift > 0 {
                        let _ = write!(delta, ",\"shift\":{shift}");
                    }
                    if !edit.is_empty() {
                        delta.push_str(",\"edit\":[");
                        for (k, &i) in edit.iter().enumerate() {
                            if k > 0 {
                                delta.push(',');
                            }
                            let _ = write!(delta, "[{i},{}]", lines[i]);
                        }
                        delta.push(']');
                    }
                    *held = Held { meta, lines };
                }
                None => {
                    let _ = write!(delta, ",\"lines\":[{}]", lines.join(","));
                    self.panes.insert(pane.id, Held { meta, lines });
                }
            }
            delta.push('}');
            if !panes.is_empty() {
                panes.push(',');
            }
            panes.push_str(&delta);
        }
        // The sections that differ from what this page holds. One the
        // snapshot has stopped carrying is `null`: the page lays sections
        // over what it has.
        let sections = if Arc::ptr_eq(&self.head, head) {
            None
        } else {
            let moved: Vec<(&str, &str)> = head
                .iter()
                .filter(|(name, json)| self.head.get(*name) != Some(*json))
                .map(|(name, json)| (name.as_str(), json.as_str()))
                .chain(
                    self.head
                        .keys()
                        .filter(|name| !head.contains_key(*name))
                        .map(|name| (name.as_str(), "null")),
                )
                .collect();
            let sections = (!moved.is_empty()).then(|| object(moved.into_iter()));
            self.head = head.clone();
            sections
        };
        if sections.is_none() && fresh.is_empty() && panes.is_empty() {
            return None;
        }
        let mut text = String::from("{");
        if let Some(sections) = sections {
            text.push_str("\"head\":");
            text.push_str(&sections);
            text.push(',');
        }
        if !fresh.is_empty() {
            text.push_str("\"styles\":");
            text.push_str(&serde_json::to_string(&fresh).ok()?);
            text.push(',');
        }
        text.push_str("\"panes\":[");
        text.push_str(&panes);
        text.push_str("]}");
        Some(text)
    }

    /// The index this page knows a span's attributes by. A new one is
    /// pushed on `fresh`, to go out with the rows that use it.
    fn style<'a>(&mut self, attr: &'a str, fresh: &mut Vec<(usize, &'a str)>) -> usize {
        if attr.is_empty() {
            return 0;
        }
        if let Some(&i) = self.styles.get(attr) {
            return i;
        }
        let i = self.styles.len() + 1;
        self.styles.insert(attr.to_string(), i);
        fresh.push((i, attr));
        i
    }
}

/// One row as JSON: a plain run is its text, any other `[style, text]`,
/// the style renumbered as the page knows it (`styles` maps a screen's own
/// indices).
fn encode_row(row: &[(usize, String)], styles: &[usize]) -> String {
    let mut out = Vec::with_capacity(2 + row.iter().map(|(_, text)| text.len() + 8).sum::<usize>());
    out.push(b'[');
    for (i, (style, text)) in row.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        match styles[*style] {
            0 => {
                let _ = serde_json::to_writer(&mut out, text);
            }
            style => {
                let _ = write!(out, "[{style},");
                let _ = serde_json::to_writer(&mut out, text);
                out.push(b']');
            }
        }
    }
    out.push(b']');
    String::from_utf8_lossy(&out).into_owned()
}

/// The rows of `new` a page holding `old` has to be told, and how far to
/// scroll `old` up first. The page pads what it holds with empty rows, so
/// those need no telling. Scrolling is tried when more than one row moved:
/// a screen that scrolled k lines is `old` shifted by k, and saying so
/// beats resending the lines that all moved together.
fn diff(old: &[String], new: &[String]) -> (usize, Vec<usize>) {
    fn stale<'a>(
        old: &'a [String],
        new: &'a [String],
        shift: usize,
    ) -> impl Iterator<Item = usize> + 'a {
        (0..new.len()).filter(move |&i| old.get(i + shift).map_or("[]", String::as_str) != new[i])
    }
    // `[i,row],` per edit; `"shift":k,` once.
    let bytes = |shift| {
        stale(old, new, shift)
            .map(|i| new[i].len() + 7)
            .sum::<usize>()
    };
    let mut best = (0, bytes(0));
    if stale(old, new, 0).nth(1).is_some() {
        for shift in 1..=new.len() / 2 {
            let cost = bytes(shift) + 12;
            if cost < best.1 {
                best = (shift, cost);
            }
        }
    }
    (best.0, stale(old, new, best.0).collect())
}

#[cfg(target_os = "linux")]
fn low_water(w: &TcpStream) {
    use std::os::fd::AsRawFd;
    // SAFETY: a live socket, an int option, its size.
    unsafe {
        libc::setsockopt(
            w.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_NOTSENT_LOWAT,
            (&LOW_WATER as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn low_water(_: &TcpStream) {}

/// Tell the page it locked, so it shows the lock screen instead of
/// reconnecting into a refusal.
fn locked_close(writer: &Mutex<Out>) {
    let mut payload = crate::passkey::CLOSE_LOCKED.to_be_bytes().to_vec();
    payload.extend_from_slice(b"locked");
    let _ = writer.lock().control(CLOSE, &payload);
}

/// A page timing its round trip: `{"kind":"ping","t":n}`. Answered by the
/// thread that read it, so it never waits for the next frame.
#[derive(Deserialize)]
struct Ping<'a> {
    kind: &'a str,
    t: Option<serde_json::Number>,
}

/// Read the page's messages until it closes: each is one `Cmd`, the same
/// JSON `POST /cmd` takes, or a ping. `packed` says the browser agreed to
/// compress.
fn commands(
    r: &mut impl Read,
    writer: &Mutex<Out>,
    dev: &str,
    origin: &Origin,
    hub: &Hub,
    packed: bool,
) {
    let mut message = Vec::new();
    let mut inflate = packed.then(|| Decompress::new(false));
    // The message being read started with RSV1; fragments after the first
    // do not repeat it.
    let mut deflated = false;
    while let Some((fin, op, rsv1, payload)) = read_frame(r) {
        // Every frame is activity, and the one that finds the device locked
        // is the last: the page learns why from the close code.
        if hub.passkeys.locked(dev, "") {
            locked_close(writer);
            return;
        }
        match op {
            // Text, binary, continuation: one message, possibly in parts.
            0x0..=0x2 => {
                if op != 0 {
                    deflated = rsv1;
                }
                message.extend_from_slice(&payload);
                if message.len() > MAX_MESSAGE {
                    return;
                }
                if fin {
                    let body = if deflated {
                        match inflate.as_mut().and_then(|d| unpack(d, &message)) {
                            Some(body) => body,
                            None => return,
                        }
                    } else {
                        std::mem::take(&mut message)
                    };
                    message.clear();
                    if let Ok(Ping {
                        kind: "ping",
                        t: Some(t),
                    }) = serde_json::from_slice(&body)
                    {
                        let pong = format!("{{\"panes\":[],\"pong\":{t}}}");
                        if writer.lock().text(&pong).is_err() {
                            return;
                        }
                    } else if let Ok(cmd) = serde_json::from_slice::<proto::Cmd>(&body) {
                        hub.command(cmd, origin);
                    }
                }
            }
            CLOSE => {
                let _ = writer
                    .lock()
                    .control(CLOSE, &payload[..payload.len().min(2)]);
                return;
            }
            PING if writer.lock().control(PONG, &payload).is_err() => return,
            _ => {}
        }
    }
}

/// One frame from a browser: FIN, opcode, RSV1 (this message is
/// compressed), unmasked payload. `None` for a closed socket or anything
/// RFC 6455 says a server must refuse - an unmasked client frame, or one
/// larger than we would ever accept.
fn read_frame(r: &mut impl Read) -> Option<(bool, u8, bool, Vec<u8>)> {
    let mut head = [0; 2];
    r.read_exact(&mut head).ok()?;
    let (fin, rsv1, op) = (head[0] & 0x80 != 0, head[0] & RSV1 != 0, head[0] & 0x0f);
    if head[1] & 0x80 == 0 {
        return None;
    }
    let len = match head[1] & 0x7f {
        126 => {
            let mut n = [0; 2];
            r.read_exact(&mut n).ok()?;
            u16::from_be_bytes(n) as usize
        }
        127 => {
            let mut n = [0; 8];
            r.read_exact(&mut n).ok()?;
            usize::try_from(u64::from_be_bytes(n)).ok()?
        }
        n => n as usize,
    };
    if len > MAX_MESSAGE {
        return None;
    }
    let mut mask = [0; 4];
    r.read_exact(&mut mask).ok()?;
    let mut payload = vec![0; len];
    r.read_exact(&mut payload).ok()?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
    Some((fin, op, rsv1, payload))
}

/// A whole, unmasked server frame.
fn encode(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | op);
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= u16::MAX as usize => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// The socket, and the compressor its text messages share when the browser
/// agreed to permessage-deflate. One lock covers both: a message written
/// by the reader thread (a pong) joins the compressed stream in the order
/// it reaches the wire, which a stream with context takeover requires.
struct Out {
    w: TcpStream,
    deflate: Option<Deflate>,
}

impl Out {
    /// A text message, compressed when that is on and it is long enough
    /// to gain. A failed compressor is an error: the browser has not seen
    /// what it swallowed, so the stream cannot go on.
    fn text(&mut self, text: &str) -> std::io::Result<()> {
        let frame = match &mut self.deflate {
            Some(deflate) if text.len() >= COMPRESS_MIN => {
                let mut frame = encode(TEXT, &deflate.pack(text.as_bytes())?);
                frame[0] |= RSV1;
                frame
            }
            _ => encode(TEXT, text.as_bytes()),
        };
        self.w.write_all(&frame)
    }

    /// A control frame, which is never compressed.
    fn control(&mut self, op: u8, payload: &[u8]) -> std::io::Result<()> {
        self.w.write_all(&encode(op, payload))
    }
}

/// The sending half of permessage-deflate: one raw deflate stream for the
/// life of the socket, each message ended by a sync flush whose empty
/// block (`00 00 ff ff`) stays off the wire (RFC 7692 section 7.2.1).
struct Deflate {
    c: Compress,
    /// `server_no_context_takeover`: forget the history after each message.
    reset: bool,
}

impl Deflate {
    fn new(reset: bool) -> Deflate {
        Deflate {
            c: Compress::new(Compression::default(), false),
            reset,
        }
    }

    fn pack(&mut self, data: &[u8]) -> std::io::Result<Vec<u8>> {
        let c = &mut self.c;
        let mut out = Vec::with_capacity(data.len() / 2 + 64);
        let start = c.total_in();
        // `compress_vec` fills the spare capacity and never grows the Vec:
        // the flush is complete once everything is in and there was room
        // left over.
        loop {
            let used = (c.total_in() - start) as usize;
            if out.len() == out.capacity() {
                out.reserve(out.capacity());
            }
            c.compress_vec(&data[used..], &mut out, FlushCompress::Sync)
                .map_err(std::io::Error::other)?;
            if (c.total_in() - start) as usize == data.len() && out.len() < out.capacity() {
                break;
            }
        }
        if self.reset {
            c.reset();
        }
        if !out.ends_with(&TAIL) {
            return Err(std::io::Error::other("deflate did not end on a sync flush"));
        }
        out.truncate(out.len() - TAIL.len());
        Ok(out)
    }
}

/// One compressed message from the browser, inflated: the receiving half
/// keeps its history across messages like the sender's. `None` for data
/// that is not deflate, or that inflates past [`MAX_MESSAGE`].
fn unpack(d: &mut Decompress, data: &[u8]) -> Option<Vec<u8>> {
    let mut input = Vec::with_capacity(data.len() + TAIL.len());
    input.extend_from_slice(data);
    input.extend_from_slice(&TAIL);
    let mut out = Vec::with_capacity(data.len() * 4 + 64);
    let start = d.total_in();
    loop {
        let (read, wrote) = (d.total_in(), d.total_out());
        if out.len() == out.capacity() {
            if out.len() > MAX_MESSAGE {
                return None;
            }
            out.reserve(out.capacity());
        }
        d.decompress_vec(
            &input[(read - start) as usize..],
            &mut out,
            FlushDecompress::Sync,
        )
        .ok()?;
        if (d.total_in() - start) as usize == input.len() && out.len() < out.capacity() {
            return Some(out);
        }
        if (d.total_in(), d.total_out()) == (read, wrote) {
            return None;
        }
    }
}

/// Whether to compress, from a client's `Sec-WebSocket-Extensions` offers:
/// `Some(reset)` takes the first `permessage-deflate` offer this can meet,
/// `reset` being whether it asked for `server_no_context_takeover`. A
/// window limit on our side is declined, as is any parameter this does not
/// know: the compressor's window is fixed. The client's own limits need no
/// answer, since inflating with the full window decodes a smaller one.
fn negotiate(offers: &str) -> Option<bool> {
    offers.split(',').find_map(|offer| {
        let mut parts = offer.split(';').map(str::trim);
        if !parts.next()?.eq_ignore_ascii_case("permessage-deflate") {
            return None;
        }
        let mut reset = false;
        for part in parts {
            let name = part.split('=').next().unwrap_or_default().trim();
            match name.to_ascii_lowercase().as_str() {
                "server_no_context_takeover" => reset = true,
                "client_no_context_takeover" | "client_max_window_bits" => {}
                _ => return None,
            }
        }
        Some(reset)
    })
}

/// `Sec-WebSocket-Accept` for a client's key.
fn accept(key: &str) -> String {
    base64(&sha1(format!("{}{GUID}", key.trim()).as_bytes()))
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0; 20];
    for (bytes, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *bytes = word.to_be_bytes();
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                TABLE[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handshake_answers_the_rfc_example() {
        // RFC 6455 section 1.3.
        assert_eq!(
            accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
    }

    #[test]
    fn a_masked_browser_frame_reads_back_and_an_unmasked_one_is_refused() {
        let text = br#"{"kind":"key","window":3,"name":"Enter"}"#;
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut frame = vec![0x81, 0x80 | text.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(text.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let (fin, op, rsv1, payload) = read_frame(&mut frame.as_slice()).unwrap();
        assert!(fin && !rsv1);
        assert_eq!(op, TEXT);
        assert_eq!(payload, text);

        assert!(read_frame(&mut [0x81u8, 0x02, b'h', b'i'].as_slice()).is_none());
    }

    #[test]
    fn server_frames_pick_the_length_encoding_by_size() {
        assert_eq!(encode(TEXT, b"hi"), [0x81, 2, b'h', b'i']);
        let mid = encode(TEXT, &[0; 300]);
        assert_eq!(&mid[..4], [0x81, 126, 1, 44]);
        let big = encode(TEXT, &vec![0; 70_000]);
        assert_eq!(&big[..2], [0x81, 127]);
        assert_eq!(big.len(), 70_000 + 10);
    }

    /// Both directions against flate2 used the way a browser's zlib is: a
    /// raw stream that keeps its history, sync-flushed per message, with the
    /// empty block off the wire.
    #[test]
    fn messages_survive_deflate_both_ways_and_the_second_reuses_the_first() {
        let rows = r#"{"panes":[{"id":3,"lines":[["$ cargo test"],[1,"   Compiling taix-web"]]}]}"#;
        let mut ours = Deflate::new(false);
        let mut theirs = Decompress::new(false);
        let mut sizes = Vec::new();
        for _ in 0..2 {
            let wire = ours.pack(rows.as_bytes()).unwrap();
            assert!(!wire.ends_with(&TAIL), "the flush block stays off the wire");
            let mut input = wire.clone();
            input.extend_from_slice(&TAIL);
            let mut out = Vec::with_capacity(rows.len() * 2);
            theirs
                .decompress_vec(&input, &mut out, FlushDecompress::Sync)
                .unwrap();
            assert_eq!(out, rows.as_bytes());
            sizes.push(wire.len());
        }
        assert!(sizes[1] < sizes[0], "history is kept: {sizes:?}");

        // The other way: the browser's compressed commands.
        let mut browser = Compress::new(Compression::default(), false);
        let mut ours = Decompress::new(false);
        for text in [
            r#"{"kind":"keys","window":3,"text":"ls"}"#,
            r#"{"kind":"key","window":3,"name":"Enter"}"#,
        ] {
            let mut wire = Vec::with_capacity(256);
            browser
                .compress_vec(text.as_bytes(), &mut wire, FlushCompress::Sync)
                .unwrap();
            wire.truncate(wire.len() - TAIL.len());
            assert_eq!(unpack(&mut ours, &wire).unwrap(), text.as_bytes());
        }
        // Not deflate at all is refused, not guessed at.
        assert!(unpack(&mut Decompress::new(false), b"\xff\xff\xff not deflate").is_none());
    }

    #[test]
    fn the_extension_offer_is_taken_when_it_can_be_met() {
        // What Chrome, Firefox and Safari send.
        assert_eq!(
            negotiate("permessage-deflate; client_max_window_bits"),
            Some(false)
        );
        assert_eq!(
            negotiate("permessage-deflate; server_no_context_takeover; client_max_window_bits=12"),
            Some(true)
        );
        // The next offer is tried when the first cannot be met.
        assert_eq!(
            negotiate("permessage-deflate; server_max_window_bits=10, permessage-deflate"),
            Some(false)
        );
        assert_eq!(
            negotiate("permessage-deflate; server_max_window_bits=10"),
            None
        );
        assert_eq!(negotiate("permessage-deflate; surprise"), None);
        assert_eq!(negotiate("x-webkit-deflate-frame"), None);
    }

    #[test]
    fn a_ping_is_recognised_by_its_kind_and_echoes_whatever_number_it_carries() {
        fn ping(text: &str) -> Option<Ping<'_>> {
            serde_json::from_str(text).ok()
        }
        let sent = ping(r#"{"kind":"ping","t":1234.5}"#).unwrap();
        assert_eq!(sent.kind, "ping");
        assert_eq!(sent.t.unwrap().to_string(), "1234.5");
        // A command is not one: no timestamp, or not a ping.
        assert!(ping(r#"{"kind":"key","window":3,"name":"Enter"}"#).is_some_and(|p| p.t.is_none()));
    }

    /// The whole path on a real socket: the offer is accepted in the 101,
    /// the first screen arrives compressed (RSV1) and inflates to the
    /// message, and a ping - plain or compressed - is answered on the spot.
    #[test]
    fn a_socket_negotiates_deflate_sends_compressed_frames_and_answers_pings() {
        use std::net::TcpListener;
        let hub = Arc::new(Hub::detached());
        hub.publish(crate::tests::snap(vec![crate::tests::pane(
            1,
            "$ cargo test\ntest result: ok",
        )]));
        hub.push_cmd(proto::Cmd::Watch {
            client: "cli".into(),
            windows: vec![1],
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = {
            let hub = hub.clone();
            std::thread::spawn(move || crate::http::serve(listener.accept().unwrap().0, &hub))
        };
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            c,
            "GET /ws?c=cli HTTP/1.1\r\nHost: localhost\r\nCookie: taix={}\r\n\
             Upgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits\r\n\r\n",
            hub.key()
        )
        .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            c.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap().to_lowercase();
        assert!(head.starts_with("http/1.1 101"), "{head}");
        assert!(
            head.contains("sec-websocket-extensions: permessage-deflate\r\n"),
            "{head}"
        );

        // An unmasked server frame: first byte, payload.
        let read = |c: &mut TcpStream| {
            let mut two = [0; 2];
            c.read_exact(&mut two).unwrap();
            let len = match two[1] {
                126 => {
                    let mut n = [0; 2];
                    c.read_exact(&mut n).unwrap();
                    u16::from_be_bytes(n) as usize
                }
                n => n as usize,
            };
            let mut payload = vec![0; len];
            c.read_exact(&mut payload).unwrap();
            (two[0], payload)
        };
        let mut inflate = Decompress::new(false);
        let (first, payload) = read(&mut c);
        assert_eq!(first, 0x80 | RSV1 | TEXT, "the first screen is compressed");
        let screen: serde_json::Value =
            serde_json::from_slice(&unpack(&mut inflate, &payload).unwrap()).unwrap();
        assert_eq!(screen["head"]["host"], "test");
        assert_eq!(
            screen["panes"][0]["lines"],
            serde_json::json!([["$ cargo test"], ["test result: ok"]])
        );

        // A masked client frame, compressed or not.
        let send = |c: &mut TcpStream, first: u8, payload: &[u8]| {
            let mask = [1u8, 2, 3, 4];
            let mut frame = vec![first, 0x80 | payload.len() as u8];
            frame.extend_from_slice(&mask);
            frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            c.write_all(&frame).unwrap();
        };
        send(&mut c, 0x80 | TEXT, br#"{"kind":"ping","t":41.5}"#);
        let (first, payload) = read(&mut c);
        assert_eq!(first, 0x80 | TEXT, "a pong is too short to compress");
        assert_eq!(payload, br#"{"panes":[],"pong":41.5}"#);

        let mut browser = Compress::new(Compression::default(), false);
        let mut packed = Vec::with_capacity(256);
        browser
            .compress_vec(
                br#"{"kind":"ping","t":42}"#,
                &mut packed,
                FlushCompress::Sync,
            )
            .unwrap();
        packed.truncate(packed.len() - TAIL.len());
        send(&mut c, 0x80 | RSV1 | TEXT, &packed);
        assert_eq!(read(&mut c).1, br#"{"panes":[],"pong":42}"#);

        drop(c);
        server.join().unwrap();
    }
}
