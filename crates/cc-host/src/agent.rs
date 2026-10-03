//! The agent (docs/agent.md), in Rust. It listens on port 3399 and picks one of two doors by the
//! first byte. A `{` goes to the pairing key screen, passed on to its 0600 socket while a key is up.
//! A 0x16 is TLS for paired Frames, which sign a single-use challenge with their pairing key. After
//! that it's versioned JSON lines. It kept the wire protocol, files and limits of home/agent.py,
//! which was the reference until the conformance suite passed against both (docs/rust-host.md R1).
//! agent.py is gone now, and tests/conformance.rs is the spec.
use crate::platform::Platform;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use cc_proto::agent::{VERSION, canon};
use cc_proto::server::{authenticate, valid_name};
use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PEEK: Duration = Duration::from_secs(2);
const LINE_MAX: usize = 16384;
const IDLE: Duration = Duration::from_secs(600);
const CAP: usize = 4;

static HUP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_hup(_: libc::c_int) {
    HUP.store(true, Relaxed);
}

pub fn urandom<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).expect("/dev/urandom");
    b
}

/// Writes a file whole that only its owner can read (0600, with its directory at 0700).
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(d) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?.write_all(data)?;
    std::fs::rename(tmp, path)
}

/// Returns the host's key from host-key (Ed25519 PKCS#8 PEM, the way pair.py makes it), making it once if it's missing.
pub fn host_key(conf: &Path) -> SigningKey {
    let path = conf.join("host-key");
    if let Ok(pem) = std::fs::read_to_string(&path)
        && let Ok(k) = SigningKey::from_pkcs8_pem(&pem)
    {
        return k;
    }
    let k = SigningKey::from_bytes(&urandom::<32>());
    write_private(&path, pkcs8_v1(&k).as_bytes()).expect("host-key");
    k
}

/// Writes PKCS#8 v1 (just the private key), the way pair.py does. Python's cryptography rejects
/// ed25519-dalek's default v2 form, which embeds the public key, and both agents read these files.
fn pkcs8_v1(k: &SigningKey) -> String {
    ed25519_dalek::pkcs8::KeypairBytes { secret_key: k.to_bytes(), public_key: None }
        .to_pkcs8_pem(Default::default())
        .expect("PKCS#8")
        .to_string()
}

/// Returns the host's id from host-id. It's a random UUID made once, never derived from the host's name or MAC (D-050).
pub fn host_id(conf: &Path) -> String {
    let path = conf.join("host-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim();
        if s.len() == 36 {
            return s.to_owned();
        }
    }
    let mut b = urandom::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let id = format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]);
    write_private(&path, id.as_bytes()).expect("host-id");
    id
}

/// Returns host-cert.pem for host-key: self-signed and valid from 2000 to 9999, since the key is the
/// pin (review M-B). An existing one, like pair.py's, is kept because Frames pin the key, not the certificate.
pub fn host_cert(conf: &Path, key: &SigningKey) -> Vec<u8> {
    let path = conf.join("host-cert.pem");
    if let Ok(pem) = std::fs::read(&path) {
        return pem;
    }
    let kp = rcgen::KeyPair::from_pem(&pkcs8_v1(key)).expect("rcgen key");
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    p.distinguished_name.push(rcgen::DnType::CommonName, "cc-host");
    p.not_before = rcgen::date_time_ymd(2000, 1, 1);
    p.not_after = rcgen::date_time_ymd(9999, 12, 31);
    let cert = p.self_signed(&kp).expect("certificate");
    write_private(&path, cert.pem().as_bytes()).expect("host-cert");
    cert.pem().into_bytes()
}

struct Conn {
    id: u64,
    close: Arc<AtomicBool>,
    out: Sender<Vec<u8>>,
}

#[derive(Default)]
struct State {
    fails: HashMap<IpAddr, (u32, Instant)>,
    conns: HashMap<String, Conn>,
    active: usize,
    next: u64,
}

pub struct Agent {
    pub conf: PathBuf,
    pub plat: Box<dyn Platform>,
    tls: Arc<ServerConfig>,
    host_pk: [u8; 32],
    state: Mutex<State>,
    pub stopping: AtomicBool,
    pub(crate) work: Mutex<crate::work::Work>,
}

impl Agent {
    pub fn new(conf: PathBuf, plat: Box<dyn Platform>) -> Arc<Agent> {
        let key = host_key(&conf);
        host_id(&conf);
        let cert = host_cert(&conf, &key);
        let certs: Vec<CertificateDer> = CertificateDer::pem_slice_iter(&cert).collect::<Result<_, _>>().expect("host-cert.pem");
        let der = key.to_pkcs8_der().expect("PKCS#8");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3")
            .with_no_client_auth()
            .with_single_cert(certs, PrivateKeyDer::Pkcs8(der.as_bytes().to_vec().into()))
            .expect("the host's certificate and key");
        tls.send_tls13_tickets = 0; // No resumption and no early data (review M-A).
        let agent = Arc::new(Agent { conf, plat, tls: Arc::new(tls), host_pk: key.verifying_key().to_bytes(), state: Mutex::default(),
                                     stopping: AtomicBool::new(false), work: Mutex::default() });
        agent.adopt();
        agent
    }

    /// Sends an event (escaped, blocked, starting) to that Frame's connection, if it has one.
    pub fn event(&self, frame: &str, v: Value) {
        if let Some(c) = self.state.lock().unwrap().conns.get(frame) {
            let mut b = canon(&v);
            b.push(b'\n');
            let _ = c.out.send(b);
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.conf.join(p)
    }

    fn trusted(&self, frame: &str) -> bool {
        self.path(&format!("trusted-frames/{frame}.pub")).exists()
    }

    fn locked(&self) -> bool {
        self.path("agent-locked").exists()
    }

    /// Accepts connections until stopped. It allows at most CAP at once and closes any from an address that's backing off.
    pub fn serve(self: &Arc<Self>, lis: TcpListener) {
        unsafe {
            libc::signal(libc::SIGHUP, on_hup as *const () as libc::sighandler_t);
        }
        let me = self.clone();
        std::thread::spawn(move || me.tick());
        lis.set_nonblocking(true).ok();
        while !self.stopping.load(Relaxed) {
            let (conn, at) = match lis.accept() {
                Ok(c) => c,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(_) => break,
            };
            conn.set_nonblocking(false).ok();
            let ip = at.ip();
            {
                let mut st = self.state.lock().unwrap();
                let backing_off = st.fails.get(&ip).is_some_and(|(_, until)| *until > Instant::now());
                if backing_off || st.active >= CAP {
                    continue; // Dropping it closes it.
                }
                st.active += 1;
            }
            let me = self.clone();
            std::thread::spawn(move || {
                if !me.handle(conn, ip) {
                    me.failed(ip);
                }
                me.state.lock().unwrap().active -= 1;
            });
        }
    }

    fn failed(&self, ip: IpAddr) {
        let mut st = self.state.lock().unwrap();
        let n = st.fails.get(&ip).map_or(0, |f| f.0);
        st.fails.insert(ip, (n + 1, Instant::now() + Duration::from_secs_f64(60f64.min(2f64.powi(n as i32)))));
    }

    /// Handles one connection. Returns false if it counts as a failure: junk, a bad handshake or a bad login.
    fn handle(self: &Arc<Self>, conn: TcpStream, ip: IpAddr) -> bool {
        conn.set_read_timeout(Some(PEEK)).ok();
        let mut first = [0u8; 1];
        match conn.peek(&mut first) {
            Ok(1) if first[0] == b'{' => {
                self.pairing_door(conn);
                true
            }
            Ok(1) if first[0] == 0x16 => self.agent_door(conn, ip),
            _ => false,
        }
    }

    /// While a key is on screen, this copies bytes both ways to the key screen's socket. Otherwise it just closes.
    fn pairing_door(&self, conn: TcpStream) {
        if !self.path("pairing").exists() {
            return;
        }
        let Ok(inner) = UnixStream::connect(self.path("pairing.sock")) else { return };
        let step = Some(Duration::from_secs(10));
        conn.set_read_timeout(step).ok();
        inner.set_read_timeout(step).ok();
        let (mut a, mut b) = (conn.try_clone().expect("clone"), inner.try_clone().expect("clone"));
        let t = std::thread::spawn(move || {
            let _ = std::io::copy(&mut b, &mut a);
            let _ = a.shutdown(Shutdown::Both);
        });
        let (mut c, mut d) = (conn, inner);
        let _ = std::io::copy(&mut c, &mut d);
        let _ = d.shutdown(Shutdown::Both);
        let _ = t.join();
    }

    fn agent_door(self: &Arc<Self>, tcp: TcpStream, ip: IpAddr) -> bool {
        let Ok(conn) = ServerConnection::new(self.tls.clone()) else { return false };
        let mut tls = StreamOwned::new(conn, tcp);
        tls.sock.set_read_timeout(Some(PEEK)).ok();
        while tls.conn.is_handshaking() {
            if tls.conn.complete_io(&mut tls.sock).is_err() {
                return false;
            }
        }
        let Some((frame, rest)) = authenticate(&mut tls, &self.host_pk, &self.path("trusted-frames"), urandom::<32>()) else {
            return false;
        };
        self.state.lock().unwrap().fails.remove(&ip); // A paired Frame got in, so its address's backoff ends.
        let _ = std::fs::write(self.path(&format!("frames/{frame}.last")), ip.to_string());
        let (out_tx, out_rx) = channel::<Vec<u8>>();
        let close = Arc::new(AtomicBool::new(false));
        let id = {
            let mut st = self.state.lock().unwrap();
            st.next += 1;
            let id = st.next;
            if let Some(old) = st.conns.insert(frame.clone(), Conn { id, close: close.clone(), out: out_tx.clone() }) {
                old.close.store(true, Relaxed); // The newest connection wins (M-F).
            }
            id
        };
        println!("agent: {frame} connected");
        let (line_tx, line_rx) = channel::<Option<Vec<u8>>>();
        let me = self.clone();
        let (f2, ip2, out2) = (frame.clone(), ip.to_string(), out_tx);
        let commands = std::thread::spawn(move || me.commands(&f2, &ip2, line_rx, out2));
        io_loop(tls, rest, out_rx, line_tx, close);
        let _ = commands.join();
        let mut st = self.state.lock().unwrap();
        if st.conns.get(&frame).is_some_and(|c| c.id == id) {
            st.conns.remove(&frame);
        }
        println!("agent: {frame} gone");
        true
    }

    fn commands(self: &Arc<Self>, frame: &str, ip: &str, lines: Receiver<Option<Vec<u8>>>, out: Sender<Vec<u8>>) {
        let send = |v: Value| {
            let mut b = canon(&v);
            b.push(b'\n');
            let _ = out.send(b);
        };
        while let Ok(Some(line)) = lines.recv() {
            let Ok(req) = serde_json::from_slice::<Value>(&line) else { return };
            let (Some(id), Some(cmd)) = (req.get("id").and_then(Value::as_i64), req.get("cmd").and_then(Value::as_str)) else { return };
            if req.get("v") != Some(&json!(VERSION)) {
                return;
            }
            if !self.trusted(frame) {
                return; // Revoked. Each command checks again (review N6).
            }
            if self.locked() {
                send(json!({"id": id, "ok": false, "error": "locked"}));
                continue;
            }
            println!("agent: {frame} {cmd}"); // Never log the payload.
            if (cmd == "session" || cmd == "window") && req["op"] == json!("start") {
                // A slow start shouldn't hold up this Frame's other commands.
                let (me, f, i, out, cmd) = (self.clone(), frame.to_owned(), ip.to_owned(), out.clone(), cmd.to_owned());
                std::thread::spawn(move || {
                    let mut a = if cmd == "session" { me.session_start(&f, &i, &req["index"]) } else { me.window_start(&f, &i, &req["uuid"]) };
                    a["id"] = json!(id);
                    let mut b = canon(&a);
                    b.push(b'\n');
                    let _ = out.send(b);
                });
                continue;
            }
            let mut answer = match self.command(frame, ip, cmd, &req) {
                Ok(v) => v,
                Err(e) => json!({"ok": false, "error": e}),
            };
            answer.as_object_mut().expect("an object").insert("id".into(), json!(id));
            send(answer);
            if cmd == "unpair" && answer_ok(&req) {
                std::thread::sleep(Duration::from_millis(200));
                return;
            }
        }
    }

    fn command(self: &Arc<Self>, frame: &str, _ip: &str, cmd: &str, req: &Value) -> Result<Value, String> {
        match cmd {
            "version" => Ok(json!({"ok": true, "version": VERSION, "host_id": host_id(&self.conf), "host": self.plat.hostname(),
                                   "login": std::env::var("USER").unwrap_or_default(), "protocols": ["rdp"]})),
            "monitors" => Ok(json!({"ok": true, "monitors": self.plat.monitors()})),
            "status" => {
                let (tags, blocked) = self.tags_state(frame);
                Ok(json!({"ok": true, "tags": tags, "blocked": blocked, "sessions": self.sessions_of(frame)}))
            }
            "session" if req["op"] == json!("stop") => Ok(self.session_stop(frame, &req["index"])),
            "window" if req["op"] == json!("list") => self.window_list(frame),
            "window" if req["op"] == json!("stop") => Ok(self.window_stop(frame, &req["uuid"])),
            "tags" => self.tags(frame, req),
            "unpair" => {
                for f in [format!("frames/{frame}.json"), format!("trusted-frames/{frame}.pub"), format!("frames/{frame}.last")] {
                    let _ = std::fs::remove_file(self.path(&f));
                }
                self.plat.systemctl(&["stop", &format!("control-center-frame@{frame}-*"), &format!("control-center-window@{frame}-*")]);
                self.forget_frame(frame);
                Ok(json!({"ok": true}))
            }
            _ => {
                let _ = req;
                Err("unknown-command".into())
            }
        }
    }

    /// Runs every 200 ms. A SIGHUP (from cc-share unpair or lock) cuts off Frames that aren't trusted
    /// anymore, or all of them while it's locked.
    fn tick(self: Arc<Self>) {
        let (mut idle_at, mut watch_at, mut tags_at) = (Instant::now(), Instant::now(), Instant::now());
        while !self.stopping.load(Relaxed) {
            if HUP.swap(false, Relaxed) {
                let locked = self.locked();
                for (frame, c) in &self.state.lock().unwrap().conns {
                    if locked || !self.trusted(frame) {
                        c.close.store(true, Relaxed);
                    }
                }
                if locked {
                    self.tags_hide(None);
                } else {
                    self.unblock_all();
                }
            }
            if tags_at.elapsed() >= Duration::from_secs(1) {
                tags_at = Instant::now();
                self.tags_expire();
            }
            if watch_at.elapsed() >= Duration::from_secs(5) {
                watch_at = Instant::now();
                self.watch();
            }
            if idle_at.elapsed() >= Duration::from_secs(30) {
                idle_at = Instant::now();
                self.idle_stop();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn answer_ok(_: &Value) -> bool {
    true
}

/// Does one TLS connection's reads and writes on one thread, since a TLS stream isn't shared across
/// threads. Queued writes go out, and incoming lines go to the command thread (None when it ends).
fn io_loop(mut tls: StreamOwned<ServerConnection, TcpStream>, mut buf: Vec<u8>, out: Receiver<Vec<u8>>, lines: Sender<Option<Vec<u8>>>, close: Arc<AtomicBool>) {
    let mut last = Instant::now();
    tls.sock.set_read_timeout(Some(Duration::from_millis(50))).ok();
    let split = |buf: &mut Vec<u8>| -> bool {
        while let Some(i) = buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=i).collect();
            line.pop();
            if line.len() > LINE_MAX || lines.send(Some(line)).is_err() {
                return false;
            }
        }
        buf.len() <= LINE_MAX
    };
    if split(&mut buf) {
        'run: while !close.load(Relaxed) {
            while let Ok(b) = out.try_recv() {
                if tls.write_all(&b).and_then(|_| tls.flush()).is_err() {
                    break 'run;
                }
            }
            let mut chunk = [0u8; 8192];
            match tls.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    last = Instant::now();
                    buf.extend_from_slice(&chunk[..n]);
                    if !split(&mut buf) {
                        break;
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if last.elapsed() > IDLE {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }
    let _ = lines.send(None);
    tls.conn.send_close_notify();
    let _ = tls.flush();
    let _ = tls.sock.shutdown(Shutdown::Both);
}

#[allow(dead_code)]
fn _names(n: &str) -> bool {
    valid_name(n)
}

#[allow(dead_code)]
fn _map(_: Map<String, Value>) {}

#[allow(dead_code)]
fn _b64(b: &[u8]) -> String {
    B64.encode(b)
}
