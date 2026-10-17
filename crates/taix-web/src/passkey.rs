//! Face ID unlock: a lost phone must not open your terminals.
//!
//! The pairing cookie is a bearer secret, so whoever holds the phone holds
//! it. A device that registers a passkey is therefore *locked* until the
//! platform authenticator (Face ID, a fingerprint) signs a fresh challenge,
//! and locks again after [`IDLE`] without a request. Enforcement is here,
//! on the server; the page's lock screen is only a convenience.
//!
//! A device is the `taix-dev` cookie, minted when the first challenge is
//! asked for. Nothing here trusts the browser's word about who it is: what
//! it proves is possession of the private key behind the stored public key.
//!
//! WebAuthn is a small slice of CBOR plus one ES256 check, and attestation
//! is deliberately not verified (`attestation: 'none'`): this is trust on
//! first use for a key the pairing cookie already vouched for.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Hub;
use crate::http::{Reply, Req};

/// How long a device stays unlocked without a request. Fixed: a setting
/// for it is a setting for how long a stolen phone stays open.
pub(crate) const IDLE: Duration = Duration::from_secs(300);

/// A challenge is answered by someone holding a phone up to their face;
/// two minutes is generous and anything older is a replay.
const CHALLENGE_TTL: Duration = Duration::from_secs(120);
/// Outstanding challenges. Each needs a cookie, so this only bounds a
/// paired device that asks in a loop.
const MAX_CHALLENGES: usize = 16;
const MAX_DEVICES: usize = 32;

/// The close code the socket ends with when the device locked.
pub(crate) const CLOSE_LOCKED: u16 = 4423;

const COOKIE: &str = "taix-dev";
const UP: u8 = 0x01;
const UV: u8 = 0x04;
const AT: u8 = 0x40;

/// Whether a device last active at `active` is unlocked at `now`. `None`
/// is a device that has not unlocked since the server started.
pub(crate) fn open(active: Option<Instant>, now: Instant) -> bool {
    active.is_some_and(|t| now.saturating_duration_since(t) < IDLE)
}

// ---------- CBOR ----------

/// The part of CBOR a `none` attestation and a COSE key use, and enough
/// more (arrays) to step over an `attStmt` of any other format.
#[derive(Debug, PartialEq)]
enum Cbor<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    Text(&'a str),
    Array(Vec<Cbor<'a>>),
    Map(Vec<(Cbor<'a>, Cbor<'a>)>),
}

impl<'a> Cbor<'a> {
    fn get<'s>(&'s self, key: &Cbor<'_>) -> Option<&'s Cbor<'a>> {
        match self {
            Cbor::Map(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn bytes(&self) -> Option<&'a [u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

/// One item and what follows it. Indefinite lengths, floats and tags are
/// refused: an authenticator has no reason to send them.
fn cbor(input: &[u8], depth: u8) -> Option<(Cbor<'_>, &[u8])> {
    if depth > 8 {
        return None;
    }
    let (&head, mut rest) = input.split_first()?;
    let (major, info) = (head >> 5, head & 31);
    let n: u64 = match info {
        0..=23 => info.into(),
        24..=27 => {
            let (raw, tail) = rest.split_at_checked(1 << (info - 24))?;
            rest = tail;
            raw.iter().fold(0, |acc, &b| acc << 8 | u64::from(b))
        }
        _ => return None,
    };
    let item = match major {
        0 => Cbor::Int(i64::try_from(n).ok()?),
        1 => Cbor::Int(-1 - i64::try_from(n).ok()?),
        2 | 3 => {
            let (data, tail) = rest.split_at_checked(usize::try_from(n).ok()?)?;
            rest = tail;
            if major == 2 {
                Cbor::Bytes(data)
            } else {
                Cbor::Text(std::str::from_utf8(data).ok()?)
            }
        }
        // Counts are not trusted for capacity: every item eats a byte, so
        // a lying count runs out of input long before it runs out of memory.
        4 => {
            let mut items = Vec::new();
            for _ in 0..n {
                let (item, tail) = cbor(rest, depth + 1)?;
                rest = tail;
                items.push(item);
            }
            Cbor::Array(items)
        }
        5 => {
            let mut pairs = Vec::new();
            for _ in 0..n {
                let (k, tail) = cbor(rest, depth + 1)?;
                let (v, tail) = cbor(tail, depth + 1)?;
                rest = tail;
                pairs.push((k, v));
            }
            Cbor::Map(pairs)
        }
        _ => return None,
    };
    Some((item, rest))
}

// ---------- authenticator data ----------

struct Auth<'a> {
    rp_hash: &'a [u8],
    flags: u8,
    count: u32,
    rest: &'a [u8],
}

fn auth_data(b: &[u8]) -> Option<Auth<'_>> {
    if b.len() < 37 {
        return None;
    }
    Some(Auth {
        rp_hash: &b[..32],
        flags: b[32],
        count: u32::from_be_bytes(b[33..37].try_into().ok()?),
        rest: &b[37..],
    })
}

/// The credential id and public key out of attested credential data
/// (aaguid, id length, id, COSE key). Only ES256 over P-256 is accepted,
/// which is the one algorithm the page asks for.
fn attested(rest: &[u8]) -> Option<(Vec<u8>, VerifyingKey)> {
    let rest = rest.get(16..)?;
    let len = usize::from(u16::from_be_bytes(rest.get(..2)?.try_into().ok()?));
    if len == 0 || len > 1023 {
        return None;
    }
    let cred = rest.get(2..2 + len)?;
    let (key, _) = cbor(rest.get(2 + len..)?, 0)?;
    let int = |k: i64| key.get(&Cbor::Int(k));
    if int(1) != Some(&Cbor::Int(2))
        || int(3) != Some(&Cbor::Int(-7))
        || int(-1) != Some(&Cbor::Int(1))
    {
        return None;
    }
    let (x, y) = (int(-2)?.bytes()?, int(-3)?.bytes()?);
    if x.len() != 32 || y.len() != 32 {
        return None;
    }
    let mut point = vec![4];
    point.extend_from_slice(x);
    point.extend_from_slice(y);
    Some((cred.to_vec(), VerifyingKey::from_sec1_bytes(&point).ok()?))
}

/// The signature over `authData || sha256(clientDataJSON)`, DER-encoded as
/// WebAuthn hands it over.
fn assertion_ok(key: &VerifyingKey, auth: &[u8], client: &[u8], der: &[u8]) -> bool {
    let mut message = auth.to_vec();
    message.extend_from_slice(&Sha256::digest(client));
    Signature::from_der(der).is_ok_and(|sig| key.verify(&message, &sig).is_ok())
}

// ---------- challenges ----------

#[derive(Default)]
struct Challenges(Vec<([u8; 32], Instant)>);

impl Challenges {
    fn push(&mut self, challenge: [u8; 32], now: Instant) {
        self.0
            .retain(|(_, at)| now.saturating_duration_since(*at) < CHALLENGE_TTL);
        if self.0.len() >= MAX_CHALLENGES {
            self.0.remove(0);
        }
        self.0.push((challenge, now));
    }

    /// Single use: a challenge that was answered, rightly or not, is gone.
    fn take(&mut self, challenge: &[u8], now: Instant) -> bool {
        let Some(i) = self.0.iter().position(|(c, _)| c == challenge) else {
            return false;
        };
        let (_, at) = self.0.remove(i);
        now.saturating_duration_since(at) < CHALLENGE_TTL
    }
}

// ---------- the store ----------

/// What survives a restart. Whether the device is unlocked does not:
/// a server that comes back up asks everyone to prove themselves again.
#[derive(Serialize, Deserialize)]
struct Record {
    id: String,
    /// The relying party id the credential was made for: the hostname.
    rp: String,
    cred: String,
    /// SEC1 uncompressed point, base64url.
    key: String,
    count: u32,
}

struct Device {
    rec: Record,
    active: Option<Instant>,
    ip: String,
}

#[derive(Default)]
struct Inner {
    path: Option<PathBuf>,
    devices: Vec<Device>,
    challenges: Challenges,
}

#[derive(Default)]
pub(crate) struct Passkeys(Mutex<Inner>);

/// The hostname part of a `Host` header, which is the WebAuthn rp id.
fn rp_id(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    }
}

pub(crate) fn origin_ok(origin: &str, host: &str) -> bool {
    origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        == Some(host)
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

impl Inner {
    /// The ceremony's shared checks: what the browser says it did, against
    /// what this server asked for.
    fn client(
        &mut self,
        json: &[u8],
        kind: &str,
        host: &str,
        now: Instant,
    ) -> Result<(), &'static str> {
        let data: ClientData = serde_json::from_slice(json).map_err(|_| "bad clientDataJSON")?;
        if data.kind != kind {
            return Err("wrong ceremony type");
        }
        let challenge = B64.decode(&data.challenge).map_err(|_| "bad challenge")?;
        if !self.challenges.take(&challenge, now) {
            return Err("unknown or expired challenge");
        }
        if !origin_ok(&data.origin, host) {
            return Err("origin does not match this host");
        }
        Ok(())
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let records: Vec<&Record> = self.devices.iter().map(|d| &d.rec).collect();
        let json = serde_json::to_vec(&records).unwrap_or_default();
        let written = {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .and_then(|mut f| f.write_all(&json))
        };
        if let Err(e) = written {
            taix_core::trace!("passkeys not saved: {e}");
        }
    }
}

impl Passkeys {
    /// Adopt the file at `path` and write there from now on.
    pub(crate) fn open_at(&self, path: PathBuf) {
        let records: Vec<Record> = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        let mut inner = self.0.lock();
        inner.devices = records
            .into_iter()
            .map(|rec| Device {
                rec,
                active: None,
                ip: String::new(),
            })
            .collect();
        inner.path = Some(path);
    }

    /// Whether this request must be refused, and the clock for the next:
    /// a device that is open stays open another [`IDLE`]. No cookie, or a
    /// device with no passkey, is never locked.
    pub(crate) fn locked(&self, dev: &str, ip: &str) -> bool {
        self.locked_at(dev, ip, Instant::now())
    }

    fn locked_at(&self, dev: &str, ip: &str, now: Instant) -> bool {
        let mut inner = self.0.lock();
        let Some(d) = inner.devices.iter_mut().find(|d| d.rec.id == dev) else {
            return false;
        };
        if !ip.is_empty() {
            d.ip = ip.to_string();
        }
        if open(d.active, now) {
            d.active = Some(now);
            return false;
        }
        true
    }

    /// Locked or not, without touching the clock: what the socket's writer
    /// asks every half second, which must not keep the device open itself.
    pub(crate) fn is_locked(&self, dev: &str) -> bool {
        let now = Instant::now();
        self.0
            .lock()
            .devices
            .iter()
            .find(|d| d.rec.id == dev)
            .is_some_and(|d| !open(d.active, now))
    }

    /// The credential id of a device with a passkey.
    fn cred(&self, dev: &str) -> Option<String> {
        let inner = self.0.lock();
        inner
            .devices
            .iter()
            .find(|d| d.rec.id == dev)
            .map(|d| d.rec.cred.clone())
    }

    fn challenge(&self, now: Instant) -> Option<[u8; 32]> {
        let mut bytes = [0u8; 32];
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .ok()?
            .read_exact(&mut bytes)
            .ok()?;
        self.0.lock().challenges.push(bytes, now);
        Some(bytes)
    }

    fn register(
        &self,
        dev: &str,
        host: &str,
        client: &[u8],
        attestation: &[u8],
        now: Instant,
    ) -> Result<(), &'static str> {
        let mut inner = self.0.lock();
        // Replacing a passkey is as good as removing it: only from a
        // device that is itself unlocked.
        if inner
            .devices
            .iter()
            .any(|d| d.rec.id == dev && !open(d.active, now))
        {
            return Err("locked");
        }
        inner.client(client, "webauthn.create", host, now)?;
        let (object, _) = cbor(attestation, 0).ok_or("bad attestationObject")?;
        let raw = object
            .get(&Cbor::Text("authData"))
            .and_then(Cbor::bytes)
            .ok_or("no authData")?;
        let auth = auth_data(raw).ok_or("short authData")?;
        let rp = rp_id(host);
        if auth.rp_hash != Sha256::digest(rp).as_slice() {
            return Err("rpIdHash does not match this host");
        }
        if auth.flags & (UP | UV | AT) != UP | UV | AT {
            return Err("user presence and verification are required");
        }
        let (cred, key) = attested(auth.rest).ok_or("not an ES256 P-256 credential")?;
        let rec = Record {
            id: dev.to_string(),
            rp: rp.to_string(),
            cred: B64.encode(cred),
            key: B64.encode(key.to_sec1_point(false).as_bytes()),
            count: auth.count,
        };
        let fresh = Device {
            rec,
            active: Some(now),
            ip: String::new(),
        };
        match inner.devices.iter_mut().find(|d| d.rec.id == dev) {
            Some(d) => *d = fresh,
            None => {
                if inner.devices.len() >= MAX_DEVICES {
                    inner.devices.remove(0);
                }
                inner.devices.push(fresh);
            }
        }
        inner.save();
        Ok(())
    }

    fn unlock(
        &self,
        dev: &str,
        host: &str,
        a: &Assertion,
        now: Instant,
    ) -> Result<(), &'static str> {
        let Assertion {
            cred,
            client,
            raw,
            sig,
        } = *a;
        let mut inner = self.0.lock();
        let Some(i) = inner.devices.iter().position(|d| d.rec.id == dev) else {
            return Err("no passkey on this device");
        };
        inner.client(client, "webauthn.get", host, now)?;
        let d = &inner.devices[i];
        if d.rec.cred != cred || d.rec.rp != rp_id(host) {
            return Err("credential does not belong to this device");
        }
        let auth = auth_data(raw).ok_or("short authData")?;
        if auth.rp_hash != Sha256::digest(&d.rec.rp).as_slice() {
            return Err("rpIdHash does not match this host");
        }
        if auth.flags & (UP | UV) != UP | UV {
            return Err("user presence and verification are required");
        }
        // Authenticators that do not count report zero forever.
        if (auth.count != 0 || d.rec.count != 0) && auth.count <= d.rec.count {
            return Err("signature counter went backwards");
        }
        let point = B64.decode(&d.rec.key).map_err(|_| "corrupt stored key")?;
        let key = VerifyingKey::from_sec1_bytes(&point).map_err(|_| "corrupt stored key")?;
        if !assertion_ok(&key, raw, client, sig) {
            return Err("bad signature");
        }
        let d = &mut inner.devices[i];
        d.rec.count = auth.count;
        d.active = Some(now);
        inner.save();
        Ok(())
    }

    fn remove(&self, dev: &str) -> bool {
        let mut inner = self.0.lock();
        let before = inner.devices.len();
        inner.devices.retain(|d| d.rec.id != dev);
        let gone = inner.devices.len() != before;
        if gone {
            inner.save();
        }
        gone
    }

    /// The desktop forgot the device at this address: its passkey goes too.
    pub(crate) fn forget_ip(&self, ip: &str) {
        let mut inner = self.0.lock();
        let before = inner.devices.len();
        inner.devices.retain(|d| d.ip != ip);
        if inner.devices.len() != before {
            inner.save();
        }
    }

    /// A new pairing key invalidates every cookie, and so every device.
    pub(crate) fn clear(&self) {
        let mut inner = self.0.lock();
        inner.devices.clear();
        inner.save();
    }
}

/// What `navigator.credentials.get` hands back, decoded.
struct Assertion<'a> {
    cred: &'a str,
    client: &'a [u8],
    raw: &'a [u8],
    sig: &'a [u8],
}

// ---------- routes ----------

/// The device cookie if it is one we could have minted, else empty.
pub(crate) fn device(req: &Req) -> &str {
    let id = req.cookies.get(COOKIE).map(String::as_str).unwrap_or("");
    if id.len() == 22
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        id
    } else {
        ""
    }
}

#[derive(Deserialize)]
struct Body {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "clientDataJSON")]
    client: String,
    #[serde(default, rename = "attestationObject")]
    attestation: String,
    #[serde(default, rename = "authenticatorData")]
    authenticator: String,
    #[serde(default)]
    signature: String,
}

/// `/passkey/*`. The caller has checked the pairing cookie and whether the
/// device is locked; these are the only authed routes a locked device may
/// use, because they are how it stops being locked.
pub(crate) fn route(req: &Req, hub: &Hub, locked: bool) -> Reply {
    let dev = device(req);
    let passkeys = &hub.passkeys;
    let decode = |s: &str| B64.decode(s).map_err(|_| Reply::fail(400, "not base64url"));
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/passkey/state") => Reply::json(
            200,
            serde_json::json!({
                "registered": passkeys.cred(dev).is_some(),
                "locked": locked,
                "cred": passkeys.cred(dev),
            })
            .to_string(),
        ),
        ("POST", "/passkey/challenge") => {
            let Some(challenge) = passkeys.challenge(Instant::now()) else {
                return Reply::fail(500, "no randomness");
            };
            // The device id is the WebAuthn user handle, so it has to exist
            // before the first credential does.
            let mut id = dev.to_string();
            let mut cookie = None;
            if id.is_empty() {
                let mut raw = [0u8; 16];
                use std::io::Read;
                let read =
                    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut raw));
                if read.is_err() {
                    return Reply::fail(500, "no randomness");
                }
                id = B64.encode(raw);
                cookie = Some(format!(
                    "{COOKIE}={id}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000"
                ));
            }
            let mut reply = Reply::json(
                200,
                serde_json::json!({
                    "challenge": B64.encode(challenge),
                    "device": id,
                    "name": crate::http::device(&req.agent),
                })
                .to_string(),
            );
            reply.headers.extend(cookie.map(|c| ("set-cookie", c)));
            reply
        }
        ("POST", "/passkey/register") => {
            let Ok(body) = serde_json::from_slice::<Body>(&req.body) else {
                return Reply::fail(400, "bad body");
            };
            let (client, attestation) = match (decode(&body.client), decode(&body.attestation)) {
                (Ok(c), Ok(a)) => (c, a),
                (Err(r), _) | (_, Err(r)) => return r,
            };
            if dev.is_empty() {
                return Reply::fail(400, "ask for a challenge first");
            }
            outcome(passkeys.register(dev, &req.host, &client, &attestation, Instant::now()))
        }
        ("POST", "/passkey/unlock") => {
            let Ok(body) = serde_json::from_slice::<Body>(&req.body) else {
                return Reply::fail(400, "bad body");
            };
            let (client, raw, sig) = match (
                decode(&body.client),
                decode(&body.authenticator),
                decode(&body.signature),
            ) {
                (Ok(c), Ok(a), Ok(s)) => (c, a, s),
                (Err(r), ..) | (_, Err(r), _) | (.., Err(r)) => return r,
            };
            let assertion = Assertion {
                cred: &body.id,
                client: &client,
                raw: &raw,
                sig: &sig,
            };
            outcome(passkeys.unlock(dev, &req.host, &assertion, Instant::now()))
        }
        ("POST", "/passkey/remove") if locked => Reply::fail(423, "locked"),
        ("POST", "/passkey/remove") => {
            passkeys.remove(dev);
            outcome(Ok(()))
        }
        _ => Reply::fail(404, "not found"),
    }
}

fn outcome(result: Result<(), &'static str>) -> Reply {
    match result {
        Ok(()) => Reply::json(200, "{\"ok\":true}".into()),
        Err("locked") => Reply::fail(423, "locked"),
        Err(why) => Reply::fail(403, why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{SigningKey, signature::Signer};

    const HOST: &str = "localhost:4040";

    fn key() -> SigningKey {
        SigningKey::from_slice(&[7u8; 32]).unwrap()
    }

    fn head(major: u8, n: usize) -> Vec<u8> {
        match n {
            0..=23 => vec![major << 5 | n as u8],
            _ => vec![major << 5 | 24, n as u8],
        }
    }

    fn bytes(b: &[u8]) -> Vec<u8> {
        [head(2, b.len()), b.to_vec()].concat()
    }

    fn text(s: &str) -> Vec<u8> {
        [head(3, s.len()), s.as_bytes().to_vec()].concat()
    }

    fn int(n: i64) -> Vec<u8> {
        if n >= 0 {
            head(0, n as usize)
        } else {
            head(1, (-1 - n) as usize)
        }
    }

    fn map(pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
        let mut out = head(5, pairs.len());
        for (k, v) in pairs {
            out.extend(k);
            out.extend(v);
        }
        out
    }

    fn rp_hash() -> Vec<u8> {
        Sha256::digest("localhost").to_vec()
    }

    fn cose(sk: &SigningKey) -> Vec<u8> {
        let point = sk.verifying_key().to_sec1_point(false);
        map(vec![
            (int(1), int(2)),
            (int(3), int(-7)),
            (int(-1), int(1)),
            (int(-2), bytes(point.x().unwrap())),
            (int(-3), bytes(point.y().unwrap())),
        ])
    }

    /// A `none` attestationObject as an authenticator would send it.
    fn attestation(sk: &SigningKey, flags: u8, cred: &[u8]) -> Vec<u8> {
        let mut auth = rp_hash();
        auth.push(flags);
        auth.extend(0u32.to_be_bytes());
        auth.extend([0u8; 16]);
        auth.extend((cred.len() as u16).to_be_bytes());
        auth.extend(cred);
        auth.extend(cose(sk));
        map(vec![
            (text("fmt"), text("none")),
            (text("attStmt"), map(vec![])),
            (text("authData"), bytes(&auth)),
        ])
    }

    fn client(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
        serde_json::json!({ "type": kind, "challenge": B64.encode(challenge), "origin": origin })
            .to_string()
            .into_bytes()
    }

    fn assertion(sk: &SigningKey, flags: u8, count: u32, client: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut auth = rp_hash();
        auth.push(flags);
        auth.extend(count.to_be_bytes());
        let mut message = auth.clone();
        message.extend_from_slice(&Sha256::digest(client));
        let sig: Signature = sk.sign(&message);
        (auth, sig.to_der().as_bytes().to_vec())
    }

    #[test]
    fn cbor_reads_a_none_attestation() {
        let sk = key();
        let raw = attestation(&sk, UP | UV | AT, b"cred-1");
        let (object, rest) = cbor(&raw, 0).unwrap();
        assert!(rest.is_empty());
        assert_eq!(object.get(&Cbor::Text("fmt")), Some(&Cbor::Text("none")));
        let auth = auth_data(
            object
                .get(&Cbor::Text("authData"))
                .unwrap()
                .bytes()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(auth.flags, UP | UV | AT);
        let (cred, key) = attested(auth.rest).unwrap();
        assert_eq!(cred, b"cred-1");
        assert_eq!(&key, sk.verifying_key());
    }

    #[test]
    fn cbor_refuses_what_it_cannot_bound() {
        assert!(cbor(&[0x5f], 0).is_none(), "indefinite length");
        assert!(cbor(&[0x44, 1, 2], 0).is_none(), "truncated bytes");
        assert!(cbor(&[0xa1, 0x01], 0).is_none(), "map missing its value");
        assert!(
            cbor(&[0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], 0).is_none(),
            "huge count"
        );
        assert!(cbor(&[0x81; 20], 0).is_none(), "nesting past the limit");
        assert_eq!(cbor(&[0x38, 0x63], 0).unwrap().0, Cbor::Int(-100));
    }

    #[test]
    fn es256_assertion_verifies_and_rejects_tampering() {
        let sk = key();
        let json = client("webauthn.get", b"c", "http://localhost:4040");
        let (auth, sig) = assertion(&sk, UP | UV, 1, &json);
        assert!(assertion_ok(sk.verifying_key(), &auth, &json, &sig));
        let mut other = json.clone();
        other.push(b' ');
        assert!(!assertion_ok(sk.verifying_key(), &auth, &other, &sig));
        let stranger = SigningKey::from_slice(&[9u8; 32]).unwrap();
        assert!(!assertion_ok(stranger.verifying_key(), &auth, &json, &sig));
        assert!(!assertion_ok(
            sk.verifying_key(),
            &auth,
            &json,
            &sig[..sig.len() - 1]
        ));
    }

    #[test]
    fn challenge_is_single_use_and_expires() {
        let (mut c, now) = (Challenges::default(), Instant::now());
        c.push([1; 32], now);
        assert!(c.take(&[1; 32], now));
        assert!(!c.take(&[1; 32], now), "second answer");
        c.push([2; 32], now);
        assert!(!c.take(&[2; 32], now + CHALLENGE_TTL), "stale answer");
        assert!(!c.take(&[2; 32], now), "an expired one is consumed too");
        c.push([3; 32], now);
        assert!(!c.take(&[4; 32], now), "never issued");
    }

    #[test]
    fn lock_gate_decides_locked_unlocked_expired() {
        let now = Instant::now();
        assert!(!open(None, now), "never unlocked");
        assert!(open(Some(now), now + IDLE - Duration::from_secs(1)));
        assert!(!open(Some(now), now + IDLE), "idle timeout");
    }

    #[test]
    fn register_then_lock_then_unlock() {
        let (pk, sk, now) = (Passkeys::default(), key(), Instant::now());
        let origin = "http://localhost:4040";
        let challenge = pk.challenge(now).unwrap();
        let att = attestation(&sk, UP | UV | AT, b"cred-1");
        let create = client("webauthn.create", &challenge, origin);
        assert_eq!(pk.register("dev", HOST, &create, &att, now), Ok(()));
        assert_eq!(
            pk.register("dev", HOST, &create, &att, now),
            Err("unknown or expired challenge")
        );
        assert!(!pk.locked_at("dev", "10.0.0.2", now + Duration::from_secs(10)));
        assert!(!pk.locked_at("other", "", now), "no passkey, no lock");

        // Idle past the limit: locked, and activity does not revive it.
        let later = now + Duration::from_secs(10) + IDLE;
        assert!(pk.locked_at("dev", "", later));
        assert!(pk.locked_at("dev", "", later + Duration::from_secs(1)));

        let challenge = pk.challenge(later).unwrap();
        let json = client("webauthn.get", &challenge, origin);
        let (auth, sig) = assertion(&sk, UP | UV, 1, &json);
        let cred = B64.encode("cred-1");
        let unlock = |json: &[u8], auth: &[u8], sig: &[u8], cred: &str| {
            let a = Assertion {
                cred,
                client: json,
                raw: auth,
                sig,
            };
            pk.unlock("dev", HOST, &a, later)
        };
        assert_eq!(
            unlock(&json, &auth, &sig, "bogus"),
            Err("credential does not belong to this device")
        );
        // The refused attempt spent the challenge: a retry needs a new one.
        assert_eq!(
            unlock(&json, &auth, &sig, &cred),
            Err("unknown or expired challenge")
        );

        let challenge = pk.challenge(later).unwrap();
        let json = client("webauthn.get", &challenge, origin);
        let (auth, sig) = assertion(&sk, UP | UV, 1, &json);
        assert_eq!(unlock(&json, &auth, &sig, &cred), Ok(()));
        assert!(!pk.locked_at("dev", "", later + Duration::from_secs(1)));

        // A counter that did not advance is a cloned authenticator.
        let challenge = pk.challenge(later).unwrap();
        let json = client("webauthn.get", &challenge, origin);
        let (auth, sig) = assertion(&sk, UP | UV, 1, &json);
        assert_eq!(
            unlock(&json, &auth, &sig, &cred),
            Err("signature counter went backwards")
        );
    }

    #[test]
    fn ceremony_rejects_wrong_origin_type_flags_and_host() {
        let (pk, sk, now) = (Passkeys::default(), key(), Instant::now());
        let att = |flags| attestation(&sk, flags, b"cred-1");
        let go = |kind: &str, origin: &str, flags: u8, host: &str| {
            let challenge = pk.challenge(now).unwrap();
            pk.register(
                "dev",
                host,
                &client(kind, &challenge, origin),
                &att(flags),
                now,
            )
        };
        let good = "http://localhost:4040";
        assert_eq!(
            go("webauthn.get", good, UP | UV | AT, HOST),
            Err("wrong ceremony type")
        );
        assert_eq!(
            go(
                "webauthn.create",
                "https://evil.example",
                UP | UV | AT,
                HOST
            ),
            Err("origin does not match this host")
        );
        assert_eq!(
            go("webauthn.create", good, UP | AT, HOST),
            Err("user presence and verification are required")
        );
        assert_eq!(
            go(
                "webauthn.create",
                "http://phone.lan",
                UP | UV | AT,
                "phone.lan"
            ),
            Err("rpIdHash does not match this host")
        );
        assert_eq!(go("webauthn.create", good, UP | UV | AT, HOST), Ok(()));
    }

    #[test]
    fn a_locked_device_cannot_replace_its_passkey() {
        let (pk, sk, now) = (Passkeys::default(), key(), Instant::now());
        let origin = "http://localhost:4040";
        let enrol = |at| {
            let challenge = pk.challenge(at).unwrap();
            let json = client("webauthn.create", &challenge, origin);
            pk.register(
                "dev",
                HOST,
                &json,
                &attestation(&sk, UP | UV | AT, b"cred-1"),
                at,
            )
        };
        assert_eq!(enrol(now), Ok(()));
        assert_eq!(enrol(now + IDLE), Err("locked"));
        assert_eq!(enrol(now + Duration::from_secs(1)), Ok(()));
    }

    #[test]
    fn forgetting_an_address_drops_its_passkey() {
        let (pk, sk, now) = (Passkeys::default(), key(), Instant::now());
        let challenge = pk.challenge(now).unwrap();
        let json = client("webauthn.create", &challenge, "http://localhost:4040");
        pk.register(
            "dev",
            HOST,
            &json,
            &attestation(&sk, UP | UV | AT, b"cred-1"),
            now,
        )
        .unwrap();
        assert!(!pk.locked_at("dev", "10.0.0.2", now));
        pk.forget_ip("10.0.0.3");
        assert!(pk.cred("dev").is_some());
        pk.forget_ip("10.0.0.2");
        assert!(pk.cred("dev").is_none());
    }

    #[test]
    fn rp_id_drops_the_port_only() {
        assert_eq!(rp_id("localhost:4040"), "localhost");
        assert_eq!(rp_id("m.tail1234.ts.net"), "m.tail1234.ts.net");
        assert!(origin_ok("https://m.ts.net", "m.ts.net"));
        assert!(!origin_ok("https://m.ts.net.evil.com", "m.ts.net"));
    }
}
