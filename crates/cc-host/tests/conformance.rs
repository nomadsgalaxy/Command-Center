//! The agent's spec, tested black-box over a real port (docs/agent.md, docs/rust-host.md R1). It runs
//! `cc-host serve --fake` on a private port and config, then goes through every hostile login case,
//! the doors, the limits and the commands. This was tests/agent-conformance (Python, run against both
//! agents). The Python agent is gone, but the cases are the same. It's one test because the cases
//! share one server and run in order. Each one starts clean, with an earlier refusal's backoff waited out.
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey, pkcs8::DecodePrivateKey};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PORT: u16 = 34991;

/// Accepts any certificate and keeps it, since the raw cases look at it themselves.
#[derive(Debug)]
struct Any(Mutex<Option<Vec<u8>>>, Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for Any {
    fn verify_server_cert(&self, cert: &CertificateDer, _: &[CertificateDer], _: &ServerName, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        *self.0.lock().unwrap() = Some(cert.to_vec());
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.3 only".into()))
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.1.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.1.signature_verification_algorithms.supported_schemes()
    }
}

/// A TLS connection to the agent that speaks in lines.
struct Conn {
    tls: StreamOwned<ClientConnection, TcpStream>,
    buf: Vec<u8>,
    cert: Vec<u8>,
    version: Option<rustls::ProtocolVersion>,
    n: u64,
}

impl Conn {
    /// Connects over TLS and reads the challenge line. Returns (connection, challenge).
    fn raw() -> std::io::Result<(Conn, Vec<u8>)> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let seen = Arc::new(Any(Mutex::new(None), provider.clone()));
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13]).unwrap()
            .dangerous().with_custom_certificate_verifier(seen.clone()).with_no_client_auth();
        let tcp = TcpStream::connect(("127.0.0.1", PORT))?;
        tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
        let conn = ClientConnection::new(Arc::new(cfg), ServerName::try_from("cc-host").unwrap()).map_err(std::io::Error::other)?;
        let mut c = Conn { tls: StreamOwned::new(conn, tcp), buf: vec![], cert: vec![], version: None, n: 0 };
        let hello = c.line().ok_or_else(|| std::io::Error::other("no challenge"))?;
        c.cert = seen.0.lock().unwrap().clone().unwrap_or_default();
        c.version = c.tls.conn.protocol_version();
        let ch = B64.decode(hello["challenge"].as_str().unwrap_or("")).map_err(std::io::Error::other)?;
        Ok((c, ch))
    }

    /// Signs in as `frame` with `sk`, to a host whose key is `host_pk`. None means it was refused.
    fn login(frame: &str, sk: &SigningKey, host_pk: &[u8; 32]) -> Option<Conn> {
        let (mut c, ch) = Conn::raw().ok()?;
        if cc_proto::agent::cert_ed25519_key(&c.cert)? != *host_pk {
            return None;
        }
        c.send(&json!({"frame": frame, "sig": B64.encode(sk.sign(&cc_proto::agent::signed(&ch, host_pk, frame)).to_bytes())}));
        (c.line()? == json!({"ok": true})).then_some(c)
    }

    fn send_raw(&mut self, b: &[u8]) {
        let _ = self.tls.write_all(b).and_then(|_| self.tls.flush());
    }

    fn send(&mut self, v: &Value) {
        let mut b = cc_proto::agent::canon(v);
        b.push(b'\n');
        self.send_raw(&b);
    }

    /// The next line. None means the end: closed, reset, or nothing within the read timeout.
    fn line(&mut self) -> Option<Value> {
        loop {
            if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let l: Vec<u8> = self.buf.drain(..=i).collect();
                return serde_json::from_slice(&l[..l.len() - 1]).ok();
            }
            let mut chunk = [0u8; 4096];
            match self.tls.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn request(&mut self, cmd: &str, args: Value) -> u64 {
        self.n += 1;
        let mut r = args.as_object().cloned().unwrap_or_default();
        r.insert("v".into(), json!(1));
        r.insert("id".into(), json!(self.n));
        r.insert("cmd".into(), json!(cmd));
        self.send(&Value::Object(r));
        self.n
    }

    /// A command's answer, skipping events.
    fn call(&mut self, cmd: &str, args: Value) -> Value {
        let id = self.request(cmd, args);
        self.answer(id)
    }

    fn answer(&mut self, id: u64) -> Value {
        self.tls.sock.set_read_timeout(Some(Duration::from_secs(20))).ok();
        loop {
            let v = self.line().unwrap_or_else(|| panic!("no answer to {id}"));
            if v["id"] == json!(id) && v.get("event").is_none() {
                return v;
            }
        }
    }

    /// Whether the agent closed it, meaning a read sees the end within `wait`.
    fn closed(&mut self, wait: Duration) -> bool {
        self.tls.sock.set_read_timeout(Some(wait)).ok();
        let mut chunk = [0u8; 64];
        match self.tls.read(&mut chunk) {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
        }
    }
}

/// Whether a plain TCP connection ended, meaning the agent closed it (or reset it).
fn shut(s: &mut TcpStream) -> bool {
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut b = [0u8; 10];
    matches!(s.read(&mut b), Ok(0) | Err(_))
}

struct Suite {
    dir: PathBuf,
    server: Child,
    frame: SigningKey,
    host_pk: [u8; 32],
    passed: Vec<&'static str>,
}

impl Drop for Suite {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn key() -> SigningKey {
    let mut s = [0u8; 32];
    std::fs::File::open("/dev/urandom").unwrap().read_exact(&mut s).unwrap();
    SigningKey::from_bytes(&s)
}

fn write(p: &Path, data: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

fn uid(n: u32) -> String {
    format!("{{{n:08x}-0000-4000-8000-000000000000}}")
}

impl Suite {
    fn start() -> Suite {
        let dir = std::env::temp_dir().join(format!("cc-host-conformance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let frame = key();
        write(&dir.join("trusted-frames/steam-frame.pub"), &B64.encode(frame.verifying_key().to_bytes()));
        write(&dir.join("frames/steam-frame.json"), r#"{"slot": 1}"#);
        let mut server = Command::new(env!("CARGO_BIN_EXE_cc-host")).args(["serve", "--fake", "--port", &PORT.to_string()])
            .env("CC_CONF", &dir).stdout(Stdio::piped()).spawn().expect("cc-host serve");
        let mut out = server.stdout.take().unwrap();
        let mut first = [0u8; 64];
        let n = out.read(&mut first).unwrap_or(0);
        assert!(String::from_utf8_lossy(&first[..n]).contains("listening"), "the agent didn't start");
        std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink())); // Drain its log.
        let pem = std::fs::read_to_string(dir.join("host-key")).unwrap();
        let host_pk = SigningKey::from_pkcs8_pem(&pem).unwrap().verifying_key().to_bytes();
        Suite { dir, server, frame, host_pk, passed: vec![] }
    }

    fn client(&self) -> Option<Conn> {
        Conn::login("steam-frame", &self.frame, &self.host_pk)
    }

    /// After a refusal, this waits out the address's backoff (it doubles per refusal) and then does a
    /// real login, which ends it because the agent clears a paired Frame's address.
    fn fresh(&self) {
        let end = Instant::now() + Duration::from_secs(90);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(500));
            if self.client().is_some() {
                return;
            }
        }
        panic!("still backing off after 90 s");
    }

    fn case(&mut self, name: &'static str, f: impl FnOnce(&Suite)) {
        self.fresh();
        f(self);
        self.passed.push(name);
    }

    fn settings(&self, v: Option<Value>) {
        let p = self.dir.join("settings.json");
        match v {
            Some(v) => write(&p, &v.to_string()),
            None => {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn hup(&self) {
        unsafe { libc::kill(self.server.id() as i32, libc::SIGHUP) };
    }
}

#[test]
fn agent_conformance() {
    let mut s = Suite::start();
    let stranger = key();

    s.case("TLS 1.3, the host's pinned key", |s| {
        let (c, _) = Conn::raw().unwrap();
        assert_eq!(c.version, Some(rustls::ProtocolVersion::TLSv1_3));
        assert_eq!(cc_proto::agent::cert_ed25519_key(&c.cert), Some(s.host_pk));
    });
    s.case("an unknown name and a wrong key look the same", |s| {
        assert!(Conn::login("nobody", &stranger, &s.host_pk).is_none());
        s.fresh();
        assert!(Conn::login("steam-frame", &stranger, &s.host_pk).is_none());
        s.fresh();
    });
    let sign = |s: &Suite, ch: &[u8], host: &[u8; 32], name: &str| json!({"frame": "steam-frame", "sig": B64.encode(s.frame.sign(&cc_proto::agent::signed(ch, host, name)).to_bytes())});
    s.case("a signature for another host (a relay) is refused", |s| {
        let (mut c, ch) = Conn::raw().unwrap();
        c.send(&sign(s, &ch, &[1u8; 32], "steam-frame"));
        assert!(c.line().is_none());
        s.fresh();
    });
    s.case("another connection's challenge is refused", |s| {
        let (_c, ch) = Conn::raw().unwrap();
        let (mut c2, ch2) = Conn::raw().unwrap();
        c2.send(&sign(s, &ch, &s.host_pk, "steam-frame"));
        assert!(ch != ch2 && c2.line().is_none());
        s.fresh();
    });
    s.case("a signature over another name is refused", |s| {
        let (mut c, ch) = Conn::raw().unwrap();
        c.send(&sign(s, &ch, &s.host_pk, "other"));
        assert!(c.line().is_none());
        s.fresh();
    });
    s.case("an oversize login closes", |s| {
        let (mut c, _) = Conn::raw().unwrap();
        c.send_raw(&[b"{".as_slice(), &[b' '; 400], b"\n"].concat());
        assert!(c.line().is_none());
        s.fresh();
    });
    s.case("a silent login closes within 5 s", |s| {
        let t0 = Instant::now();
        let (mut c, _) = Conn::raw().unwrap();
        assert!(c.line().is_none() && t0.elapsed() < Duration::from_millis(7500));
        s.fresh();
    });
    s.case("a wrong first byte closes without an answer", |s| {
        let mut t = TcpStream::connect(("127.0.0.1", PORT)).unwrap();
        t.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        assert!(shut(&mut t));
        s.fresh();
    });
    s.case("a silent connection closes within 2 s", |s| {
        let t0 = Instant::now();
        let mut t = TcpStream::connect(("127.0.0.1", PORT)).unwrap();
        assert!(shut(&mut t) && t0.elapsed() < Duration::from_secs(4));
        s.fresh();
    });
    s.case("the pairing door is closed with no key on screen", |s| {
        let mut t = TcpStream::connect(("127.0.0.1", PORT)).unwrap();
        t.write_all(b"{\"v\":1}\n").unwrap();
        assert!(shut(&mut t));
        s.fresh();
    });
    s.case("a refusal backs off the address (backoff)", |s| {
        let mut t = TcpStream::connect(("127.0.0.1", PORT)).unwrap();
        t.write_all(b"x").unwrap();
        shut(&mut t);
        std::thread::sleep(Duration::from_millis(200));
        assert!(s.client().is_none());
        s.fresh();
    });
    s.case("version, monitors, an unknown command", |s| {
        let mut c = s.client().unwrap();
        let v = c.call("version", json!({}));
        assert!(v["ok"] == json!(true) && v["version"] == json!(1) && v["host_id"].as_str().map(str::len) == Some(36) && v["protocols"] == json!(["rdp"]), "{v}");
        let m = c.call("monitors", json!({}));
        assert!(m["monitors"][0]["output"] == "eDP-1" && m["monitors"][0]["width"] == 1920, "{m}");
        assert_eq!(c.call("nonsense", json!({})), json!({"id": 3, "ok": false, "error": "unknown-command"}));
        // The fake host has no Deck controller, so it doesn't offer the IMU stream.
        assert_eq!(v["features"], json!([]), "{v}");
        assert_eq!(c.call("imu", json!({"op": "start"}))["error"], "no-imu");
        assert_eq!(c.call("imu", json!({"op": "start", "hz": 1}))["error"], "bad-rate");
        assert_eq!(c.call("imu", json!({"op": "stop"}))["stopped"], json!(false), "stopping what isn't running is fine");
        assert_eq!(c.call("imu", json!({"op": "sideways"}))["error"], "bad-op");
    });
    s.case("an oversize line closes the connection", |s| {
        let mut c = s.client().unwrap();
        c.send_raw(&[b"{".as_slice(), &[b' '; 16500], b"\n"].concat());
        assert!(c.closed(Duration::from_secs(2)));
    });
    s.case("newest wins", |s| {
        let mut a = s.client().unwrap();
        let mut b = s.client().unwrap();
        assert!(a.closed(Duration::from_millis(800)));
        assert!(!b.closed(Duration::from_millis(200)) && b.call("version", json!({}))["ok"] == json!(true));
    });
    s.case("a revoked Frame is cut off and refused at once", |s| {
        let mut b = s.client().unwrap();
        let pub_ = s.dir.join("trusted-frames/steam-frame.pub");
        let saved = std::fs::read_to_string(&pub_).unwrap();
        std::fs::remove_file(&pub_).unwrap();
        s.hup();
        assert!(b.closed(Duration::from_millis(800)), "kept its connection");
        assert!(s.client().is_none());
        write(&pub_, &saved);
    });
    s.case("at most 4 connections at once", |s| {
        let held: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(("127.0.0.1", PORT)).unwrap()).collect();
        std::thread::sleep(Duration::from_millis(300));
        let mut extra = TcpStream::connect(("127.0.0.1", PORT)).unwrap();
        assert!(shut(&mut extra));
        drop(held);
        s.fresh();
    });
    s.case("sessions: start, the same start twice at once, status, stop", |s| {
        let mut c = s.client().unwrap();
        let a = c.request("session", json!({"op": "start", "index": 0}));
        let b = c.request("session", json!({"op": "start", "index": 0}));
        let v = c.request("version", json!({}));
        // Answers come in any order. The version mustn't wait for the starts, since a starting session holds nothing else up.
        let mut got = std::collections::HashMap::new();
        let mut order = vec![];
        c.tls.sock.set_read_timeout(Some(Duration::from_secs(20))).ok();
        while got.len() < 3 {
            let l = c.line().expect("answers");
            if let Some(id) = l["id"].as_u64() && l.get("event").is_none() {
                order.push(id);
                got.insert(id, l);
            }
        }
        assert_eq!(order[0], v, "a starting session held the Frame's other commands: {order:?}");
        for id in [a, b] {
            let r = &got[&id];
            assert!(r["ok"] == json!(true) && r["port"] == json!(3410) && r["ready"] == json!(true), "{r}");
            assert!(r["start_ms"].is_u64() && r["port"].is_u64(), "integers: {r}");
        }
        let st = c.call("status", json!({}));
        assert!(st["sessions"].as_array().map(Vec::len) == Some(1) && st["sessions"][0]["index"] == 0 && st["sessions"][0]["port"] == 3410, "{st}");
        assert_eq!(c.call("session", json!({"op": "start", "index": 7}))["error"], "no-such-monitor");
        assert_eq!(c.call("session", json!({"op": "stop", "index": 0}))["stopped"], json!(true));
        assert_eq!(c.call("status", json!({}))["sessions"], json!([]));
    });
    s.case("windows: off without the host's opt-in; the scope; captions opt-in; bad uuids", |s| {
        let mut c = s.client().unwrap();
        assert_eq!(c.call("window", json!({"op": "list"}))["error"], "windows-off");
        write(&s.dir.join("windows-on"), "");
        let listed = c.call("window", json!({"op": "list"}))["windows"].clone();
        let uuids: Vec<&str> = listed.as_array().unwrap().iter().filter_map(|w| w["uuid"].as_str()).collect();
        assert_eq!(uuids, [uid(1), uid(7), uid(8)]);
        assert_eq!(listed[0], json!({"uuid": uid(1), "app": "org.kde.dolphin", "x": 100, "y": 100, "w": 800, "h": 600}));
        assert!(["x", "y", "w", "h"].iter().all(|k| listed[0][k].is_u64()), "geometry as integers");
        write(&s.dir.join("windows-captions"), "");
        assert_eq!(c.call("window", json!({"op": "list"}))["windows"][0]["caption"], "secret");
        std::fs::remove_file(s.dir.join("windows-captions")).unwrap();
        for n in [2, 3, 4, 5, 6] {
            assert_eq!(c.call("window", json!({"op": "start", "uuid": uid(n)}))["error"], "not-shared", "{n}");
        }
        for bad in [json!("3f1c2b7e-5a4d-4c1e-9b8a-6d2e1f0a9c3b"), json!("{../../etc}"), json!(format!("{{{}}}", "a".repeat(36))), json!(7)] {
            assert_eq!(c.call("window", json!({"op": "start", "uuid": bad}))["error"], "bad-uuid", "{bad}");
        }
    });
    s.case("windows: start, the same again, 2 per Frame, stop, the session cap, the rate limit", |s| {
        let mut c = s.client().unwrap();
        let r = c.call("window", json!({"op": "start", "uuid": uid(1)}));
        assert!(r["ok"] == json!(true) && r["port"] == 3415, "{r}");
        assert_eq!(c.call("window", json!({"op": "start", "uuid": uid(1)}))["port"], 3415);
        assert_eq!(c.call("window", json!({"op": "start", "uuid": uid(7)}))["port"], 3416);
        assert_eq!(c.call("window", json!({"op": "start", "uuid": uid(8)}))["error"], "busy", "a third pop-out");
        assert!(c.call("status", json!({}))["sessions"].as_array().unwrap().iter().any(|x| x["uuid"] == json!(uid(1))));
        assert_eq!(c.call("window", json!({"op": "stop", "uuid": uid(7)}))["stopped"], json!(true));
        s.settings(Some(json!({"sessions_max": 1})));
        assert_eq!(c.call("window", json!({"op": "start", "uuid": uid(8)}))["error"], "busy", "past sessions_max");
        s.settings(Some(json!({"window_asks_per_min": 1})));
        std::thread::sleep(Duration::from_millis(100));
        let (r1, r2) = (c.call("window", json!({"op": "list"})), c.call("window", json!({"op": "list"})));
        assert!(r1["error"] == "rate-limited" || r2["error"] == "rate-limited", "{r1} {r2}");
        s.settings(None);
        c.call("window", json!({"op": "stop", "uuid": uid(1)}));
        std::fs::remove_file(s.dir.join("windows-on")).unwrap();
    });
    let good = json!({"bg": "white", "tags": [[0, 100, 200, 240], [1, 1500, 200, 240]]});
    s.case("tag screens: drawn from checked parameters, hidden", |s| {
        let mut c = s.client().unwrap();
        let many: Vec<Value> = (0..65).map(|_| json!([0, 0, 0, 12])).collect();
        for (bad, why) in [(json!({"bg": "white", "tags": many}), "too-many-tags"), (json!({"bg": "white", "tags": [[0, 1800, 200, 240]]}), "tag-outside"),
                           (json!({"bg": "white", "tags": [[0, 100, 200, 240], [1, 200, 300, 240]]}), "tags-overlap"), (json!({"bg": "white", "tags": [[0, 100, 10, 240]]}), "tag-in-banner"),
                           (json!({"bg": "white", "tags": [[250, 100, 200, 240]]}), "tag-outside"), (json!({"bg": "red", "tags": []}), "bad-params"),
                           (json!({"bg": "white", "tags": [], "image": "x"}), "bad-params")] {
            assert_eq!(c.call("tags", json!({"op": "show", "index": 0, "params": bad}))["error"], why);
        }
        assert_eq!(c.call("tags", json!({"op": "show", "index": 0, "params": good}))["ok"], json!(true));
        assert_eq!(c.call("status", json!({}))["tags"], json!([0]));
        assert!(c.call("tags", json!({"op": "hide"}))["ok"] == json!(true) && c.call("status", json!({}))["tags"] == json!([]));
        assert_eq!(c.call("tags", json!({"op": "show", "index": 3, "params": good}))["error"], "no-such-monitor");
    });
    s.case("tag cap: a scan's many shows count once, a hide stops the clock (3 scans in a row fit)", |s| {
        let mut c = s.client().unwrap();
        s.settings(Some(json!({"tags_cap_min": 0.5}))); // 30 s. The 3 scans take ~8, but summing overlapping shows (or time after a hide) would pass 30.
        for _ in 0..3 {
            for _ in 0..10 {
                let r = c.call("tags", json!({"op": "show", "index": 0, "params": good}));
                assert_eq!(r["ok"], json!(true), "{r}");
                std::thread::sleep(Duration::from_millis(150));
            }
            assert_eq!(c.call("tags", json!({"op": "hide"}))["ok"], json!(true));
        }
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(c.call("tags", json!({"op": "show", "index": 0, "params": good}))["ok"], json!(true));
        c.call("tags", json!({"op": "hide"}));
        s.settings(None);
    });
    println!("agent conformance ok: {} cases: {}", s.passed.len(), s.passed.join("; "));
    assert_eq!(s.passed.len(), 21);
}
