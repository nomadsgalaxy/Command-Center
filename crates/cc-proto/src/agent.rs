//! The client half of the agent (docs/agent.md §2-3). It opens TLS 1.3 to the host and pins the
//! certificate's Ed25519 key to the one pairing kept. Then it signs the host's challenge with this
//! Frame's pairing key, and after that it's JSON lines: a request `{"v":1,"id":n,"cmd":..}`, then any
//! events (`{"event":..}`), then its answer. One thread does all the reading and writing, because an
//! SSL stream can't be shared across threads.
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey, pkcs8::DecodePrivateKey};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::{Map, Value, json};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PORT: u16 = 3399;
pub const VERSION: u64 = 1;
const AUTH: &[u8] = b"cc-agent-auth/1\n";
const LINE_MAX: usize = 16384;

#[derive(Debug)]
pub enum Error {
    Unreachable(String),
    HostChanged,  // the certificate's key isn't the pinned one, so nothing was sent
    NotPaired(String),
    Closed,
    NoAnswer,
    Bad(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Unreachable(e) => write!(f, "unreachable ({e})"),
            Error::HostChanged => write!(f, "host-changed"),
            Error::NotPaired(e) => write!(f, "not-paired ({e})"),
            Error::Closed => write!(f, "closed"),
            Error::NoAnswer => write!(f, "no answer"),
            Error::Bad(e) => write!(f, "{e}"),
        }
    }
}

/// A paired host, the way this Frame keeps it (trusted-hosts/<machine>.json).
pub struct Trusted {
    pub addr: String,
    pub host_pk: [u8; 32],
    pub frame: String,
}

/// Loads the JSON pairing wrote for `machine`. The Frame's name comes from there, or from the hostname
/// if it's missing (same as cc-home).
pub fn trusted(conf: &Path, machine: &str) -> Result<Trusted, Error> {
    let text = std::fs::read_to_string(conf.join("trusted-hosts").join(format!("{machine}.json"))).map_err(|e| Error::NotPaired(e.to_string()))?;
    let t: Value = serde_json::from_str(&text).map_err(|e| Error::Bad(e.to_string()))?;
    let pk = B64.decode(t["host_pk"].as_str().unwrap_or("")).map_err(|e| Error::NotPaired(e.to_string()))?;
    let host_pk: [u8; 32] = pk.try_into().map_err(|_| Error::NotPaired("host_pk isn't 32 bytes".into()))?;
    let frame = t["frame"].as_str().map(str::to_owned).unwrap_or_else(frame_name);
    Ok(Trusted { addr: t["addr"].as_str().unwrap_or("").to_owned(), host_pk, frame })
}

/// This Frame's name the way pairing made it: the hostname in lower case, with anything else turned
/// into '-'.
pub fn frame_name() -> String {
    let h = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_lowercase();
    let n: String = h.chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' { c } else { '-' }).collect();
    let n = n.trim_matches('-').chars().take(32).collect::<String>();
    if n.is_empty() { "frame".into() } else { n }
}

/// Reads the pairing key (frame-key: Ed25519, PKCS#8 PEM, the way pair.py writes it).
pub fn frame_key(conf: &Path) -> Result<SigningKey, Error> {
    let pem = std::fs::read_to_string(conf.join("frame-key")).map_err(|e| Error::NotPaired(e.to_string()))?;
    SigningKey::from_pkcs8_pem(&pem).map_err(|e| Error::Bad(e.to_string()))
}

/// Gets the pairing key, making it on the first pairing the way pair.py's frame_key does: PKCS#8 PEM
/// (v1, like Python writes it), 0600 in a 0700 directory. It never overwrites an existing key.
pub fn frame_key_or_new(conf: &Path) -> Result<SigningKey, Error> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = conf.join("frame-key");
    if path.exists() {
        return frame_key(conf);
    }
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut seed)).map_err(|e| Error::Bad(e.to_string()))?;
    let der = [[0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20].as_slice(), &seed].concat();
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", B64.encode(der));
    let io = |e: std::io::Error| Error::Bad(e.to_string());
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(conf).map_err(io)?;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).and_then(|mut f| f.write_all(pem.as_bytes())).map_err(io)?;
    frame_key(conf)
}

/// The bytes the Frame signs: the protocol, the challenge, the host's key and the Frame's name.
pub fn signed(challenge: &[u8], host_pk: &[u8; 32], frame: &str) -> Vec<u8> {
    [AUTH, challenge, host_pk, frame.as_bytes()].concat()
}

/// JSON the way pair.py's canon() writes it: keys sorted, no spaces (serde_json's map is already
/// sorted), and non-ASCII escaped as \uXXXX like Python's json.dumps.
pub fn canon(v: &Value) -> Vec<u8> {
    let s = serde_json::to_string(v).expect("JSON");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut b = [0u16; 2];
            for u in c.encode_utf16(&mut b) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out.into_bytes()
}

/// Pulls the Ed25519 key out of a certificate (its SubjectPublicKeyInfo is the algorithm's fixed
/// prefix and 32 bytes). ponytail: it's a byte search, which is enough for the self-signed
/// certificates hosts make.
pub fn cert_ed25519_key(der: &[u8]) -> Option<[u8; 32]> {
    const SPKI: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
    let i = der.windows(SPKI.len()).position(|w| w == SPKI)?;
    der.get(i + SPKI.len()..i + SPKI.len() + 32)?.try_into().ok()
}

#[derive(Debug)]
struct Pin {
    key: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(&self, cert: &CertificateDer, _: &[CertificateDer], _: &ServerName, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match cert_ed25519_key(cert.as_ref()) {
            Some(k) if k == self.key => Ok(ServerCertVerified::assertion()),
            _ => Err(rustls::Error::General("host-changed".into())),
        }
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.3 only".into()))
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub struct Client {
    tls: StreamOwned<ClientConnection, TcpStream>,
    buf: Vec<u8>,
    n: u64,
}

impl Client {
    /// Connects and signs in as this Frame, or says why not (on host-changed, nothing was sent to it).
    pub fn connect(t: &Trusted, key: &SigningKey, port: u16, timeout: Duration) -> Result<Client, Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| Error::Bad(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pin { key: t.host_pk, provider }))
            .with_no_client_auth();
        let at = (t.addr.as_str(), port).to_socket_addrs().map_err(|e| Error::Unreachable(e.to_string()))?.next().ok_or(Error::Unreachable("no address".into()))?;
        let tcp = TcpStream::connect_timeout(&at, timeout).map_err(|e| Error::Unreachable(e.to_string()))?;
        tcp.set_read_timeout(Some(timeout)).ok();
        tcp.set_write_timeout(Some(timeout)).ok();
        let conn = ClientConnection::new(Arc::new(config), ServerName::try_from("cc-host").expect("name")).map_err(|e| Error::Bad(e.to_string()))?;
        let mut c = Client { tls: StreamOwned::new(conn, tcp), buf: Vec::new(), n: 0 };
        let hello = match c.line() {
            Ok(v) => v,
            Err(Error::Bad(e)) if e.contains("host-changed") => return Err(Error::HostChanged),
            Err(e) => return Err(e),
        };
        let challenge = B64.decode(hello["challenge"].as_str().unwrap_or("")).map_err(|e| Error::NotPaired(e.to_string()))?;
        if challenge.len() != 32 {
            return Err(Error::NotPaired("challenge".into()));
        }
        let sig = key.sign(&signed(&challenge, &t.host_pk, &t.frame));
        c.send(&json!({"frame": t.frame, "sig": B64.encode(sig.to_bytes())}))?;
        if c.line()? != json!({"ok": true}) {
            return Err(Error::NotPaired("refused".into()));
        }
        c.tls.sock.set_read_timeout(None).ok();
        Ok(c)
    }

    fn send(&mut self, v: &Value) -> Result<(), Error> {
        let mut b = canon(v);
        b.push(b'\n');
        self.tls.write_all(&b).map_err(|e| Error::Bad(e.to_string()))?;
        self.tls.flush().map_err(|e| Error::Bad(e.to_string()))
    }

    fn line(&mut self) -> Result<Value, Error> {
        loop {
            if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                return serde_json::from_slice(&line[..line.len() - 1]).map_err(|e| Error::Bad(e.to_string()));
            }
            if self.buf.len() > LINE_MAX {
                return Err(Error::Bad("line too long".into()));
            }
            let mut chunk = [0u8; 4096];
            match self.tls.read(&mut chunk) {
                Ok(0) => return Err(Error::Closed),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err(Error::NoAnswer),
                Err(e) => return Err(Error::Bad(e.to_string())),
            }
        }
    }

    /// Runs one command and returns its answer (`ok` true or false, with `error`). Any events that
    /// come in meanwhile go to `event`.
    pub fn call(&mut self, cmd: &str, args: Map<String, Value>, timeout: Duration, mut event: impl FnMut(&Value)) -> Result<Value, Error> {
        self.n += 1;
        let mut req = args;
        req.insert("v".into(), json!(VERSION));
        req.insert("id".into(), json!(self.n));
        req.insert("cmd".into(), json!(cmd));
        self.send(&Value::Object(req))?;
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::NoAnswer);
            }
            self.tls.sock.set_read_timeout(Some(left)).ok();
            let v = self.line()?;
            if v.get("event").is_some() {
                event(&v);
            } else if v["id"] == json!(self.n) {
                return Ok(v);
            }
        }
    }

    /// Between commands, this reads the events that came in meanwhile until `wait` passes with nothing
    /// more. Python's client queued them as they arrived, and cc-home's align watches for a tag
    /// screen's Esc this way. Err means the connection went.
    pub fn poll(&mut self, wait: Duration, mut event: impl FnMut(&Value)) -> Result<(), Error> {
        self.tls.sock.set_read_timeout(Some(wait)).ok();
        loop {
            match self.line() {
                Ok(v) if v.get("event").is_some() => event(&v),
                Ok(_) => {} // a late answer to a command we already gave up on
                Err(Error::NoAnswer) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}

impl Client {
    /// Like `poll`, but it returns once `wait` has passed even if events keep coming (a stream's never go quiet).
    pub fn listen(&mut self, wait: Duration, mut event: impl FnMut(&Value)) -> Result<(), Error> {
        let end = Instant::now() + wait;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            self.tls.sock.set_read_timeout(Some(left)).ok();
            match self.line() {
                Ok(v) if v.get("event").is_some() => event(&v),
                Ok(_) => {}
                Err(Error::NoAnswer) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}

/// One command on a fresh connection, for callers that only want the answer.
pub fn call_once(conf: &Path, machine: &str, cmd: &str, args: Map<String, Value>) -> Result<Value, Error> {
    let t = trusted(conf, machine)?;
    let mut c = Client::connect(&t, &frame_key(conf)?, PORT, Duration::from_secs(5))?;
    c.call(cmd, args, Duration::from_secs(15), |_| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_matches_python() {
        // json.dumps({"b": 1, "a": "é", "c": [1, {"z": 0, "y": None}]}, sort_keys=True, separators=(",", ":"))
        let v = json!({"b": 1, "a": "é", "c": [1, {"z": 0, "y": null}]});
        assert_eq!(String::from_utf8(canon(&v)).unwrap(), r#"{"a":"\u00e9","b":1,"c":[1,{"y":null,"z":0}]}"#);
    }

    #[test]
    fn signed_bytes_are_the_protocol() {
        let s = signed(&[7u8; 32], &[9u8; 32], "steam-frame");
        assert!(s.starts_with(b"cc-agent-auth/1\n") && s.ends_with(b"steam-frame") && s.len() == 16 + 32 + 32 + 11);
    }

    #[test]
    fn a_new_frame_key_is_made_once() {
        let d = std::env::temp_dir().join(format!("cc-key-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let k = frame_key_or_new(&d).unwrap();
        assert_eq!(frame_key_or_new(&d).unwrap().to_bytes(), k.to_bytes(), "kept");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.join("frame-key")).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&d);
    }
}
