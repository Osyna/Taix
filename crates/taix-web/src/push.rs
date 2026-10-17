//! Web Push: the lock-screen alert for an agent that stopped to ask.
//!
//! The page cannot do this itself - iOS suspends it the moment the screen
//! goes off - so the desktop process tells the phone's push service, which
//! wakes the service worker. Three standards, each small:
//!
//! - RFC 8030 is the delivery: an HTTPS `POST` to the subscription's
//!   endpoint. That goes through `curl`, on a background thread, for the
//!   reason the rest of this crate has no HTTP client: the TLS stack would
//!   outweigh everything else here, and one alert is one short process.
//! - RFC 8291 encrypts the payload to the browser's key (`aes128gcm`).
//! - RFC 8292 (VAPID) is the proof that the sender is the server the
//!   browser subscribed to: an ES256 JWT over the endpoint's origin.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::http::{Reply, Req};

/// RFC 8292 wants a contact the push service can reach if we misbehave.
/// Apple rejects a subject without a real-looking domain, so not
/// `localhost`; `example.com` is reserved and answers nobody.
const SUBJECT: &str = "mailto:taix@example.com";

/// Longest the service may hold an undelivered alert. An hour-old "agent is
/// waiting" is not news, so there is no reason to ask for more.
const TTL: &str = "3600";

/// Subscriptions kept at once: a phone, a tablet, a few browsers. Past this
/// the endpoint is being looped, not used.
const MAX_SUBS: usize = 32;

/// One record of 4096 bytes: the 86-byte header, the 16-byte tag and the
/// padding delimiter leave this much for the message.
const MAX_PLAIN: usize = 4096 - 86 - 16 - 1;

/// One browser that asked to be told.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sub {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

/// `PushSubscription.toJSON()` as the browser produces it.
#[derive(Deserialize)]
struct Incoming {
    endpoint: String,
    keys: IncomingKeys,
}

#[derive(Deserialize)]
struct IncomingKeys {
    p256dh: String,
    auth: String,
}

impl Sub {
    /// Validate at the boundary: the endpoint becomes a URL we `POST` to
    /// and the keys become curve points, so neither is taken on trust.
    pub fn parse(body: &[u8]) -> Result<Sub, &'static str> {
        let inc: Incoming = serde_json::from_slice(body).map_err(|_| "malformed subscription")?;
        let rest = inc
            .endpoint
            .strip_prefix("https://")
            .ok_or("endpoint must be https")?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if inc.endpoint.len() > 2048
            || host.is_empty()
            || host.contains('@')
            || !inc.endpoint.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("bad endpoint");
        }
        let point = decode(&inc.keys.p256dh).ok_or("bad p256dh")?;
        if point.len() != 65 || PublicKey::from_sec1_bytes(&point).is_err() {
            return Err("bad p256dh");
        }
        if decode(&inc.keys.auth).is_none_or(|a| a.len() != 16) {
            return Err("bad auth");
        }
        Ok(Sub {
            endpoint: inc.endpoint,
            p256dh: inc.keys.p256dh,
            auth: inc.keys.auth,
        })
    }

    /// `https://host[:port]`, the `aud` of the VAPID token.
    fn origin(&self) -> &str {
        let rest = &self.endpoint[8..];
        &self.endpoint[..8 + rest.find(['/', '?', '#']).unwrap_or(rest.len())]
    }
}

fn decode(s: &str) -> Option<Vec<u8>> {
    B64.decode(s).ok()
}

/// The sender: one VAPID key and the browsers that subscribed. Nothing
/// touches disk until first use, so a hub built for a test stays off it.
pub struct Push {
    dir: PathBuf,
    key: Mutex<Option<SecretKey>>,
    subs: Mutex<Vec<Sub>>,
}

impl Push {
    pub fn new(dir: PathBuf) -> Push {
        let subs = std::fs::read(dir.join("push.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Push {
            dir,
            key: Mutex::new(None),
            subs: Mutex::new(subs),
        }
    }

    /// The VAPID key, generated on first use and kept at `vapid.key`. A
    /// key that changed would orphan every subscription, so a file that
    /// cannot be written is an error rather than a key for this run only.
    fn key(&self) -> std::io::Result<SecretKey> {
        let mut slot = self.key.lock();
        if let Some(key) = slot.as_ref() {
            return Ok(key.clone());
        }
        let path = self.dir.join("vapid.key");
        let key = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| decode(s.trim()))
            .and_then(|b| SecretKey::from_slice(&b).ok())
        {
            Some(key) => key,
            None => {
                let key = random_key()?;
                write_private(&path, B64.encode(key.to_bytes()).as_bytes())?;
                key
            }
        };
        *slot = Some(key.clone());
        Ok(key)
    }

    /// The uncompressed public key, base64url: what the page hands
    /// `pushManager.subscribe` as `applicationServerKey`.
    pub fn public_key(&self) -> std::io::Result<String> {
        Ok(B64.encode(self.key()?.public_key().to_sec1_point(false).as_bytes()))
    }

    /// Remember a browser, replacing an earlier entry for the same endpoint.
    pub fn add(&self, sub: Sub) -> std::io::Result<()> {
        // The key has to exist before anyone is told to subscribe to it.
        self.key()?;
        let mut subs = self.subs.lock();
        subs.retain(|s| s.endpoint != sub.endpoint);
        if subs.len() >= MAX_SUBS {
            subs.remove(0);
        }
        subs.push(sub);
        self.save(&subs)
    }

    pub fn remove(&self, endpoint: &str) -> std::io::Result<()> {
        let mut subs = self.subs.lock();
        let before = subs.len();
        subs.retain(|s| s.endpoint != endpoint);
        if subs.len() == before {
            return Ok(());
        }
        self.save(&subs)
    }

    fn save(&self, subs: &[Sub]) -> std::io::Result<()> {
        write_private(
            &self.dir.join("push.json"),
            serde_json::to_string(subs)?.as_bytes(),
        )
    }

    /// Tell every subscribed browser that a window wants a human. Returns
    /// at once: the encryption and the `curl`s run on their own thread, so
    /// neither the GUI frame nor a connection waits on a push service.
    ///
    /// `waiting` is `Some(n)` when the window is asking a question and `n`
    /// windows are now waiting: the worker uses it for the app badge and
    /// for the answer buttons, which a failure has no use for.
    pub fn alert(self: &Arc<Self>, window: i64, title: &str, body: &str, waiting: Option<usize>) {
        if self.subs.lock().is_empty() {
            return;
        }
        let body: String = body.chars().take(200).collect();
        let payload = serde_json::json!({
            "title": title, "body": body, "window": window, "waiting": waiting,
        })
        .to_string()
        .into_bytes();
        let this = self.clone();
        let _ = std::thread::Builder::new()
            .name("taix-push".into())
            .spawn(move || this.deliver(&payload));
    }

    fn deliver(&self, payload: &[u8]) {
        let Ok(key) = self.key() else { return };
        let subs = self.subs.lock().clone();
        for sub in subs {
            // 404 and 410 are the push service saying this subscription is
            // gone for good (app deleted, permission revoked).
            if let Some(404 | 410) = send(&key, &sub, payload) {
                let _ = self.remove(&sub.endpoint);
            }
        }
    }
}

/// `/push/*`, behind the pairing cookie like everything else that is not
/// the pairing page.
pub fn route(req: &Req, hub: &crate::Hub) -> Reply {
    let push = hub.push();
    let ok = || Reply::json(200, "{\"ok\":true}".into());
    let io = |e: std::io::Error| Reply::fail(500, &e.to_string());
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/push/key") => match push.public_key() {
            Ok(key) => Reply::json(200, serde_json::json!({ "key": key }).to_string()),
            Err(e) => io(e),
        },
        ("POST", "/push/subscribe") => match Sub::parse(&req.body) {
            Ok(sub) => push.add(sub).map_or_else(io, |()| ok()),
            Err(e) => Reply::fail(400, e),
        },
        ("POST", "/push/unsubscribe") => {
            #[derive(Deserialize)]
            struct Gone {
                endpoint: String,
            }
            match serde_json::from_slice::<Gone>(&req.body) {
                Ok(gone) => push.remove(&gone.endpoint).map_or_else(io, |()| ok()),
                Err(_) => Reply::fail(400, "malformed request"),
            }
        }
        _ => Reply::fail(404, "not found"),
    }
}

/// POST one message to one subscription; the HTTP status, or `None` when
/// `curl` could not get one (offline, timeout, bad key).
fn send(key: &SecretKey, sub: &Sub, payload: &[u8]) -> Option<u16> {
    let ua = decode(&sub.p256dh)?;
    let auth = decode(&sub.auth)?;
    let ephemeral = random_key().ok()?;
    let salt: [u8; 16] = random().ok()?;
    let body = encrypt(&ua, &auth, payload, &ephemeral, &salt)?;
    let exp = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() + 12 * 3600;
    let jwt = vapid_jwt(&SigningKey::from(key), sub.origin(), exp);
    let public = B64.encode(key.public_key().to_sec1_point(false).as_bytes());

    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "10",
            "-X",
            "POST",
            "--data-binary",
            "@-",
        ])
        .args(["-o", "/dev/null", "-w", "%{http_code}"])
        .args(["-H", &format!("Authorization: vapid t={jwt}, k={public}")])
        .args(["-H", &format!("TTL: {TTL}")])
        .args(["-H", "Content-Encoding: aes128gcm"])
        .args(["-H", "Content-Type: application/octet-stream"])
        .args(["-H", "Urgency: high"])
        .args(["--url", &sub.endpoint])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Dropped at the end of the statement, which is what ends the body.
    child.stdin.take()?.write_all(&body).ok()?;
    let out = child.wait_with_output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// RFC 8291 §3.4: the message as one `aes128gcm` record, header included.
///
/// `ephemeral` and `salt` are parameters, not drawn in here, so the RFC's
/// own example can be replayed byte for byte.
fn encrypt(
    ua_public: &[u8],
    auth: &[u8],
    plaintext: &[u8],
    ephemeral: &SecretKey,
    salt: &[u8; 16],
) -> Option<Vec<u8>> {
    if plaintext.len() > MAX_PLAIN {
        return None;
    }
    let ua = PublicKey::from_sec1_bytes(ua_public).ok()?;
    let as_public = ephemeral.public_key().to_sec1_point(false);
    let secret = ephemeral.diffie_hellman(&ua);

    // Mix the browser's auth secret and both public keys into the secret.
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public.as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), secret.raw_secret_bytes())
        .expand(&info, &mut ikm)
        .ok()?;

    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .ok()?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).ok()?;

    // 0x02 closes the only, and so last, record.
    let mut record = plaintext.to_vec();
    record.push(2);
    let sealed = Aes128Gcm::new_from_slice(&cek)
        .ok()?
        .encrypt(&nonce.into(), record.as_slice())
        .ok()?;

    let mut out = salt.to_vec();
    out.extend_from_slice(&4096u32.to_be_bytes());
    out.push(as_public.as_bytes().len() as u8);
    out.extend_from_slice(as_public.as_bytes());
    out.extend_from_slice(&sealed);
    Some(out)
}

/// RFC 8292 §2: the ES256 token the push service checks against the `k`
/// the browser subscribed with.
fn vapid_jwt(key: &SigningKey, aud: &str, exp: u64) -> String {
    let head = B64.encode(r#"{"typ":"JWT","alg":"ES256"}"#);
    let claims =
        B64.encode(serde_json::json!({ "aud": aud, "exp": exp, "sub": SUBJECT }).to_string());
    let signed = format!("{head}.{claims}");
    let sig: Signature = key.sign(signed.as_bytes());
    format!("{signed}.{}", B64.encode(sig.to_bytes()))
}

/// Bytes from the kernel's pool, the way `taix_core::random_key` takes them.
fn random<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// A fresh P-256 scalar. Zero and values past the group order are not keys,
/// so the (astronomically rare) rejection simply draws again.
fn random_key() -> std::io::Result<SecretKey> {
    loop {
        if let Ok(key) = SecretKey::from_slice(&random::<32>()?) {
            return Ok(key);
        }
    }
}

/// Both files hold capabilities - the signing key, and URLs anyone holding
/// can push to - so they are private to the user.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;
    use std::os::unix::fs::PermissionsExt;

    fn b(s: &str) -> Vec<u8> {
        decode(s).unwrap()
    }

    /// RFC 8291 Appendix A, whole: the published inputs must produce the
    /// published message.
    #[test]
    fn rfc8291_appendix_a() {
        let plain = b("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
        assert_eq!(plain, b"When I grow up, I want to be a watermelon");
        let eph = SecretKey::from_slice(&b("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let ua_public = b(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        );
        let auth = b("BTBZMqHH6r4Tts7J_aSIgg");
        let salt: [u8; 16] = b("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let got = encrypt(&ua_public, &auth, &plain, &eph, &salt).unwrap();
        // The header and the ciphertext are published as separate base64
        // runs: 86 bytes is not a multiple of three, so they cannot be joined
        // as text.
        let mut want = b(concat!(
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIg",
            "Dll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8",
        ));
        assert_eq!(want.len(), 86);
        want.extend(b(
            "8pfeW0KbunFT06SuDKoJH9Ql87S1QUrdirN6GcG7sFz1y1sqLgVi1VhjVkHsUoEsbI_0LpXMuGvnzQ",
        ));
        assert_eq!(got, want);
    }

    /// The same key pair seen from the browser's side: the receiver derives
    /// the same secret, so this also pins which end is which.
    #[test]
    fn rfc8291_receiver_derives_the_same_secret() {
        let eph = SecretKey::from_slice(&b("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let ua = SecretKey::from_slice(&b("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94")).unwrap();
        let ours = eph.diffie_hellman(&ua.public_key());
        let theirs = ua.diffie_hellman(&eph.public_key());
        assert_eq!(ours.raw_secret_bytes(), theirs.raw_secret_bytes());
        assert_eq!(
            B64.encode(ours.raw_secret_bytes()),
            "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"
        );
    }

    #[test]
    fn oversize_message_is_refused_not_truncated() {
        let eph = random_key().unwrap();
        let ua = random_key().unwrap().public_key().to_sec1_point(false);
        let salt = [0u8; 16];
        assert!(encrypt(ua.as_bytes(), &[0; 16], &vec![b'x'; MAX_PLAIN], &eph, &salt).is_some());
        assert!(
            encrypt(
                ua.as_bytes(),
                &[0; 16],
                &vec![b'x'; MAX_PLAIN + 1],
                &eph,
                &salt
            )
            .is_none()
        );
    }

    #[test]
    fn jwt_verifies_against_the_published_key() {
        let dir = std::env::temp_dir().join(format!("taix-push-jwt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let push = Push::new(dir.clone());
        let public = b(&push.public_key().unwrap());
        assert_eq!(public.len(), 65);
        assert_eq!(public[0], 4);

        let jwt = vapid_jwt(
            &SigningKey::from(&push.key().unwrap()),
            "https://push.example",
            1234,
        );
        let (signed, sig) = jwt.rsplit_once('.').unwrap();
        let sig = Signature::from_slice(&b(sig)).unwrap();
        VerifyingKey::from_sec1_bytes(&public)
            .unwrap()
            .verify(signed.as_bytes(), &sig)
            .unwrap();

        let claims: serde_json::Value =
            serde_json::from_slice(&b(signed.split('.').nth(1).unwrap())).unwrap();
        assert_eq!(claims["aud"], "https://push.example");
        assert_eq!(claims["exp"], 1234);
        assert_eq!(claims["sub"], SUBJECT);

        // Same key on the next start, or every subscription is orphaned.
        assert_eq!(
            Push::new(dir.clone()).public_key().unwrap(),
            push.public_key().unwrap()
        );
        let mode = std::fs::metadata(dir.join("vapid.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn sub_json(endpoint: &str, p256dh: &str, auth: &str) -> Vec<u8> {
        serde_json::json!({
            "endpoint": endpoint,
            "expirationTime": null,
            "keys": { "p256dh": p256dh, "auth": auth },
        })
        .to_string()
        .into_bytes()
    }

    const P256DH: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

    #[test]
    fn subscription_validation() {
        let good = Sub::parse(&sub_json(
            "https://fcm.googleapis.com/fcm/send/abc:def",
            P256DH,
            AUTH,
        ))
        .unwrap();
        assert_eq!(good.origin(), "https://fcm.googleapis.com");
        let ported = Sub::parse(&sub_json("https://push.example:8443?x=1", P256DH, AUTH)).unwrap();
        assert_eq!(ported.origin(), "https://push.example:8443");

        let bad = |endpoint: &str, p256dh: &str, auth: &str| {
            Sub::parse(&sub_json(endpoint, p256dh, auth)).unwrap_err()
        };
        assert_eq!(
            bad("http://push.example/x", P256DH, AUTH),
            "endpoint must be https"
        );
        assert_eq!(bad("https://", P256DH, AUTH), "bad endpoint");
        assert_eq!(
            bad("https://user@push.example/x", P256DH, AUTH),
            "bad endpoint"
        );
        assert_eq!(
            bad("https://push.example/a b", P256DH, AUTH),
            "bad endpoint"
        );
        assert_eq!(bad("https://push.example/x", "AAAA", AUTH), "bad p256dh");
        // Right length, not a point on the curve.
        assert_eq!(
            bad("https://push.example/x", &B64.encode([4u8; 65]), AUTH),
            "bad p256dh"
        );
        assert_eq!(bad("https://push.example/x", P256DH, "AAAA"), "bad auth");
        assert_eq!(
            bad("https://push.example/x", P256DH, "not base64!"),
            "bad auth"
        );
        assert_eq!(Sub::parse(b"{}").unwrap_err(), "malformed subscription");
    }

    #[test]
    fn subscriptions_are_keyed_by_endpoint_and_persist() {
        let dir = std::env::temp_dir().join(format!("taix-push-subs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let push = Push::new(dir.clone());
        let one = Sub::parse(&sub_json("https://push.example/1", P256DH, AUTH)).unwrap();
        let two = Sub::parse(&sub_json("https://push.example/2", P256DH, AUTH)).unwrap();
        push.add(one.clone()).unwrap();
        push.add(two.clone()).unwrap();
        push.add(one.clone()).unwrap();
        assert_eq!(Push::new(dir.clone()).subs.lock().len(), 2);
        push.remove("https://push.example/1").unwrap();
        assert_eq!(*Push::new(dir.clone()).subs.lock(), vec![two]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
