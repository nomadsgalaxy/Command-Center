//! Pairing (docs/pairing.md §4), both halves, matching home/pair.py byte for byte. Here's the flow:
//! - SPAKE2 on the 6-digit key, with ids `cc-frame:<frame>` and `cc-host`.
//! - The transcript T = SHA-256(canon(m1) "\n" canon(m2)).
//! - Four keys by HKDF-SHA256 (salt T, labels "cc-pair confirm F|H", "cc-pair seal F->H|H->F").
//! - HMAC-SHA256 confirmations both ways.
//! - Then the host's reply, sealed with ChaCha20-Poly1305 (a 96-bit counter nonce per direction, T as
//!   associated data), and the Frame's sealed ok.
use crate::agent::canon;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::io::{Read, Write};
use std::time::Duration;

pub const MAX: usize = 4096;
pub const STEP: Duration = Duration::from_secs(10);
pub const TRIES: u32 = 3;

/// Why a pairing didn't happen. Either the other side said why (bad-key, locked, name-taken,
/// cancelled, full, bad-name, host-changed, bad-host), or it was junk (a malformed, oversize or slow
/// message), which doesn't count as a try.
#[derive(Debug, PartialEq)]
pub enum Fail {
    Refused(String, String),
    Bad(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Fail::Refused(s, d) => write!(f, "{s} {d}"),
            Fail::Bad(s) => write!(f, "{s}"),
        }
    }
}

fn bad(s: &str) -> Fail {
    Fail::Bad(s.into())
}

/// JSON lines, at most MAX bytes each.
pub struct Line<S> {
    pub s: S,
    buf: Vec<u8>,
}

impl<S: Read + Write> Line<S> {
    pub fn new(s: S) -> Self {
        Line { s, buf: Vec::new() }
    }

    pub fn send(&mut self, v: &Value) -> Result<(), Fail> {
        let mut b = canon(v);
        b.push(b'\n');
        self.s.write_all(&b).and_then(|_| self.s.flush()).map_err(|e| Fail::Bad(e.to_string()))
    }

    /// Reads the next object. An `{"error": ..}` means the other side refused. With `fields`,
    /// nothing else can be in it, and the ones that are there have to be strings or integers.
    pub fn recv(&mut self, fields: &[&str]) -> Result<Value, Fail> {
        while !self.buf.contains(&b'\n') {
            if self.buf.len() > MAX {
                return Err(bad("too long"));
            }
            let mut chunk = [0u8; 4096];
            match self.s.read(&mut chunk) {
                Ok(0) => return Err(bad("closed")),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(_) => return Err(bad("timeout")),
            }
        }
        let i = self.buf.iter().position(|&b| b == b'\n').unwrap();
        let line: Vec<u8> = self.buf.drain(..=i).collect();
        if line.len() - 1 > MAX {
            return Err(bad("too long"));
        }
        let v: Value = serde_json::from_slice(&line[..line.len() - 1]).map_err(|_| bad("not JSON"))?;
        let o = v.as_object().ok_or(bad("not an object"))?;
        if let Some(e) = o.get("error").and_then(Value::as_str) {
            let left = o.get("tries_left").map(|t| t.to_string()).unwrap_or_default();
            return Err(Fail::Refused(e.into(), left));
        }
        if !fields.is_empty() && (o.keys().any(|k| !fields.contains(&k.as_str())) || o.values().any(|x| !(x.is_string() || x.is_i64()))) {
            return Err(bad("unexpected fields"));
        }
        Ok(v)
    }
}

fn unb64(v: &Value, size: Option<usize>) -> Result<Vec<u8>, Fail> {
    let b = B64.decode(v.as_str().ok_or(bad("not base64"))?).map_err(|_| bad("not base64"))?;
    if size.is_some_and(|n| b.len() != n) {
        return Err(bad("wrong length"));
    }
    Ok(b)
}

pub fn ids(frame: &str) -> (Vec<u8>, Vec<u8>) {
    ([b"cc-frame:".as_slice(), frame.as_bytes()].concat(), b"cc-host".to_vec())
}

pub fn transcript(m1: &Value, m2: &Value) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(canon(m1));
    h.update(b"\n");
    h.update(canon(m2));
    h.finalize().into()
}

/// The four keys, in order: confirm F, confirm H, seal F->H, seal H->F.
pub fn keys(k: &[u8], t: &[u8; 32]) -> [[u8; 32]; 4] {
    let hk = hkdf::Hkdf::<Sha256>::new(Some(t), k);
    ["confirm F", "confirm H", "seal F->H", "seal H->F"].map(|lab| {
        let mut out = [0u8; 32];
        hk.expand(format!("cc-pair {lab}").as_bytes(), &mut out).expect("32 bytes");
        out
    })
}

fn mac(key: &[u8; 32], t: &[u8; 32]) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key size");
    m.update(t);
    m
}

/// ChaCha20-Poly1305 in one direction. The nonce is a 96-bit counter from 0, and the transcript is
/// the AD.
pub struct Seal {
    aead: ChaCha20Poly1305,
    t: [u8; 32],
    n: u64,
}

impl Seal {
    pub fn new(key: &[u8; 32], t: &[u8; 32]) -> Seal {
        Seal { aead: ChaCha20Poly1305::new(key.into()), t: *t, n: 0 }
    }
    fn nonce(&mut self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.n.to_be_bytes());
        self.n += 1;
        n
    }
    pub fn seal(&mut self, v: &Value) -> Value {
        let nonce = self.nonce();
        let ct = self.aead.encrypt((&nonce).into(), Payload { msg: &canon(v), aad: &self.t }).expect("seal");
        json!({"sealed": B64.encode(ct)})
    }
    pub fn open(&mut self, v: &Value) -> Result<Value, Fail> {
        let nonce = self.nonce();
        let ct = unb64(&v["sealed"], None)?;
        let pt = self.aead.decrypt((&nonce).into(), Payload { msg: &ct, aad: &self.t }).map_err(|_| bad("can't open"))?;
        serde_json::from_slice(&pt).map_err(|_| bad("can't open"))
    }
}

/// The Frame's half. Returns the host's reply, its name and its key. `known_host_pk` is the key we
/// paired with before at this address; if it changed, I refuse unless `replace` is set.
pub fn frame_pair<S: Read + Write>(s: S, key: &str, frame: &str, sk: &SigningKey, known_host_pk: Option<[u8; 32]>, replace: bool)
    -> Result<(Value, String, [u8; 32]), Fail> {
    let (ida, idb) = ids(frame);
    let (a, msg) = Spake2::<Ed25519Group>::start_a(&Password::new(key.as_bytes()), &Identity::new(&ida), &Identity::new(&idb));
    let mut ln = Line::new(s);
    let m1 = json!({"v": 1, "frame": frame, "frame_pk": B64.encode(sk.verifying_key().to_bytes()), "spake": B64.encode(msg)});
    ln.send(&m1)?;
    let m2 = ln.recv(&["v", "host", "host_pk", "spake"])?;
    if m2["v"] != json!(1) || !["host", "host_pk", "spake"].iter().all(|f| m2[f].is_string()) {
        return Err(bad("host's message"));
    }
    let host_pk: [u8; 32] = unb64(&m2["host_pk"], Some(32))?.try_into().unwrap();
    if known_host_pk.is_some_and(|k| k != host_pk) && !replace {
        return Err(Fail::Refused("host-changed".into(), "its key differs from the one paired before (--replace)".into()));
    }
    let k = a.finish(&unb64(&m2["spake"], None)?).map_err(|_| bad("spake"))?;
    let t = transcript(&m1, &m2);
    let [cf, ch, sfh, shf] = keys(&k, &t);
    ln.send(&json!({"confirm": B64.encode(mac(&cf, &t).finalize().into_bytes())}))?;
    let c = ln.recv(&["confirm"])?;
    if mac(&ch, &t).verify_slice(&unb64(&c["confirm"], Some(32))?).is_err() {
        return Err(Fail::Refused("bad-host".into(), "the host couldn't prove it knows the key".into()));
    }
    let (mut to_h, mut from_h) = (Seal::new(&sfh, &t), Seal::new(&shf, &t));
    let reply = from_h.open(&ln.recv(&["sealed"])?)?;
    if !reply_ok(&reply) {
        return Err(bad("host's reply"));
    }
    ln.send(&to_h.seal(&json!({"ok": true})))?;
    Ok((reply, m2["host"].as_str().unwrap_or("").to_owned(), host_pk))
}

/// Checks the host's sealed reply strictly (pair.py's check_reply). Its fields end up as file names
/// and viewers.conf lines, so anything that doesn't fit gets rejected instead of used.
pub fn reply_ok(r: &Value) -> bool {
    let s = |k: &str| r[k].as_str();
    let int = |v: &Value| v.as_i64().is_some();
    let word = |t: &str, max: usize| !t.is_empty() && t.chars().count() <= max && t.chars().all(|c| c.is_alphanumeric() || "_.-".contains(c));
    let hex = |t: &str| t.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    let uuid = |t: &str| t.len() == 36 && t.char_indices().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { hex(&c.to_string()) });
    let login = |t: &str| t.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && word(t, 32);
    let mons = r["monitors"].as_array().is_some_and(|m| (1..=10).contains(&m.len()) && m.iter().all(|m| {
        m.is_object() && int(&m["index"]) && int(&m["port"]) && int(&m["width"]) && int(&m["height"]) && m["output"].as_str().is_some_and(|o| word(o, 32))
    }));
    s("user").is_some_and(|u| crate::server::valid_name(u.strip_prefix("cc-").unwrap_or(u)))
        && s("password").is_some_and(|p| (16..=64).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
        && r["slot"].as_i64().is_some_and(|n| (1..=4).contains(&n))
        && (r["login"].is_null() || s("login").is_some_and(login))
        && (r["id"].is_null() || s("id").is_some_and(uuid))
        && s("cert_sha256").is_some_and(|c| c.len() == 64 && hex(c))
        && mons
}

/// How one connection to the host's pairing door turned out.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Paired(String, [u8; 32]),
    WrongKey, // counts as a try
    Nothing,  // junk, a bad name, a taken name or a refused reply: not a try
}

/// The host's half of one connection.
/// - `trusted(frame)`: the key paired before under that name.
/// - `confirm_replace(frame)`: asks the person at the host whether a new key can take that name.
/// - `reply(frame, frame_pk)`: the sealed answer (the Frame's login and ports), or a refusal ("full").
/// - `tries_left`: how many tries the key has left after this one, if this one was wrong (0 means
///   locked).
pub fn host_one<S: Read + Write>(s: S, key: &str, host_name: &str, sk: &SigningKey, tries_left: u32,
                                 trusted: &dyn Fn(&str) -> Option<[u8; 32]>, confirm_replace: &dyn Fn(&str) -> bool,
                                 reply: &dyn Fn(&str, &[u8; 32]) -> Result<Value, String>) -> Result<Outcome, Fail> {
    let mut ln = Line::new(s);
    let m1 = ln.recv(&["v", "frame", "frame_pk", "spake"])?;
    if m1["v"] != json!(1) || !["frame", "frame_pk", "spake"].iter().all(|f| m1[f].is_string()) {
        return Err(bad("first message"));
    }
    let frame = m1["frame"].as_str().unwrap();
    if !crate::server::valid_name(frame) {
        ln.send(&json!({"error": "bad-name"}))?;
        return Ok(Outcome::Nothing);
    }
    let frame_pk: [u8; 32] = unb64(&m1["frame_pk"], Some(32))?.try_into().unwrap();
    VerifyingKey::from_bytes(&frame_pk).map_err(|_| bad("frame key"))?;
    if trusted(frame).is_some_and(|k| k != frame_pk) && !confirm_replace(frame) {
        ln.send(&json!({"error": "name-taken"}))?;
        return Ok(Outcome::Nothing);
    }
    let (ida, idb) = ids(frame);
    let (b, msg) = Spake2::<Ed25519Group>::start_b(&Password::new(key.as_bytes()), &Identity::new(&ida), &Identity::new(&idb));
    let m2 = json!({"v": 1, "host": host_name, "host_pk": B64.encode(sk.verifying_key().to_bytes()), "spake": B64.encode(msg)});
    ln.send(&m2)?;
    let k = b.finish(&unb64(&m1["spake"], None)?).map_err(|_| bad("spake"))?;
    let t = transcript(&m1, &m2);
    let [cf, ch, sfh, shf] = keys(&k, &t);
    let c = ln.recv(&["confirm"])?;
    if mac(&cf, &t).verify_slice(&unb64(&c["confirm"], Some(32)).unwrap_or_default()).is_err() {
        if tries_left <= 1 {
            ln.send(&json!({"error": "locked"}))?;
        } else {
            ln.send(&json!({"error": "bad-key", "tries_left": tries_left - 1}))?;
        }
        return Ok(Outcome::WrongKey);
    }
    ln.send(&json!({"confirm": B64.encode(mac(&ch, &t).finalize().into_bytes())}))?;
    let (mut to_f, mut from_f) = (Seal::new(&shf, &t), Seal::new(&sfh, &t));
    let answer = match reply(frame, &frame_pk) {
        Ok(a) => a,
        Err(e) => {
            ln.send(&json!({"error": e}))?;
            return Ok(Outcome::Nothing);
        }
    };
    ln.send(&to_f.seal(&answer))?;
    if from_f.open(&ln.recv(&["sealed"])?)?["ok"] != json!(true) {
        return Err(bad("no ok"));
    }
    Ok(Outcome::Paired(frame.to_owned(), frame_pk))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_seal_match_python() {
        // golden values from home/pair.py: keys(b"K"*32, sha256(b"T")) and a sealed {"ok": true}
        let t: [u8; 32] = Sha256::digest(b"T").into();
        let k = keys(&[b'K'; 32], &t);
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let golden = include_str!("../tests/pair-golden.txt");
        let mut lines = golden.lines();
        for key in k {
            assert_eq!(hex(&key), lines.next().unwrap());
        }
        let mut s = Seal::new(&k[2], &t);
        assert_eq!(s.seal(&json!({"ok": true}))["sealed"].as_str().unwrap(), lines.next().unwrap());
        assert_eq!(hex(&mac(&k[0], &t).finalize().into_bytes()), lines.next().unwrap());
    }

    #[test]
    fn a_reply_is_checked_as_pair_py_does() {
        let ok = json!({"user": "cc-steam-frame", "password": "A".repeat(24), "slot": 1, "cert_sha256": "ab".repeat(32), "id": "0123abcd-0000-4000-8000-00000000beef",
                        "monitors": [{"index": 0, "output": "eDP-1", "width": 1920, "height": 1080, "port": 3410}]});
        assert!(reply_ok(&ok));
        for (k, v) in [("user", json!("cc-x y")), ("password", json!("short")), ("slot", json!(5)), ("id", json!("../../x")), ("login", json!("Root")),
                       ("cert_sha256", json!("AB".repeat(32))), ("monitors", json!([])), ("monitors", json!([{"index": 0, "output": "a b", "width": 1, "height": 1, "port": 3410}]))] {
            let mut r = ok.clone();
            r[k] = v;
            assert!(!reply_ok(&r), "{k}");
        }
    }
}
