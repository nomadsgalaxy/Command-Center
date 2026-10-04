//! cc-host pair, which was home/pair.py's host_main (docs/pairing.md). It puts a 6-digit key on screen,
//! and a Frame that proves it (cc_proto::pair::host_one) gets its own slot, krdp login and ports. The
//! key lives 5 minutes. 3 wrong confirmations lock it, and then there's a 30 s wait before another.
//! Esc, a click or tap, or SIGTERM cancels it, and it's gone however it ends. When the agent holds 3399 (docs/agent.md),
//! pairing connections come through pairing.sock (0600) instead, a socket only this user can open.
//!   cc-host pair [--test]   --test: cc-home selftest-pair's host (key 123456, one monitor, no screen or units)
use crate::agent::{host_id, host_key, urandom, write_private};
use crate::platform::{Platform, mono};
use crate::screen::{Ev, Key, Screen};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use cc_proto::pair::{Outcome, host_one};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::net::TcpListener;
use crate::draw;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const TRIES: u32 = 3;
const LIFE: f64 = 300.0;
const PAUSE: f64 = 30.0;
const STEP: Duration = Duration::from_secs(10);
const SLOTS: std::ops::RangeInclusive<i64> = 1..=4; // At most 4 Frames (M4). Slot k's monitor m is on 3400 + 10k + m.

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Relaxed);
}

/// Picks uniformly from 0..n. It uses rejection sampling so there's no modulo bias.
fn below(n: u32) -> u32 {
    let lim = u32::MAX - u32::MAX % n;
    loop {
        let v = u32::from_le_bytes(urandom::<4>());
        if v < lim {
            return v % n;
        }
    }
}

/// Returns the SHA-256 of cert.pem's certificate, which is krdp's TLS certificate that the Frame pins.
fn cert_sha256(conf: &Path) -> Result<String, String> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let der = CertificateDer::from_pem_file(conf.join("cert.pem")).map_err(|e| format!("cert.pem: {e}"))?;
    Ok(Sha256::digest(&der).iter().map(|b| format!("{b:02x}")).collect())
}

/// Builds the sealed answer for a Frame that proved the key: its slot (the one it had, or the first free
/// one), a fresh password, the ports of the shared monitors and the host's id. None means every slot is taken.
fn reply(conf: &Path, plat: &dyn Platform, test: bool, frame: &str) -> Result<Value, String> {
    let frames = conf.join("frames");
    let taken: Vec<(i64, String)> = std::fs::read_dir(&frames).into_iter().flatten().flatten().filter_map(|e| {
        let name = e.file_name().into_string().ok()?;
        let f = name.strip_suffix(".json")?.to_owned();
        let v: Value = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
        Some((v["slot"].as_i64()?, f))
    }).collect();
    let slot = taken.iter().find(|(_, f)| f == frame).map(|(k, _)| *k)
        .or_else(|| SLOTS.clone().find(|k| !taken.iter().any(|(t, _)| t == k)))
        .ok_or("full")?;
    let mons: Vec<Value> = plat.monitors().iter().map(|m| {
        let i = m["index"].as_i64().unwrap_or(0);
        json!({"index": i, "output": m["output"], "width": m["width"], "height": m["height"], "port": 3400 + 10 * slot + i})
    }).collect();
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let password: String = (0..24).map(|_| ALPHABET[below(ALPHABET.len() as u32) as usize] as char).collect();
    let cert = if test { "ab".repeat(32) } else { cert_sha256(conf).map_err(|e| { eprintln!("cc-host pair: {e}"); "no-cert".to_owned() })? };
    Ok(json!({"user": format!("cc-{frame}"), "password": password, "slot": slot, "cert_sha256": cert, "monitors": mons, "id": host_id(conf)}))
}

/// A question for the person at the host. The serving thread puts it up and it's answered on the key screen.
#[derive(Default)]
struct Ask {
    question: Option<String>,
    answer: Option<mpsc::Sender<bool>>,
}

enum Door {
    Tcp(TcpListener),
    Unix(UnixListener, PathBuf),
}

trait ReadWrite: Read + Write + Send {}
impl<T: Read + Write + Send> ReadWrite for T {}

impl Door {
    /// Returns the next connection, if one is waiting. The listener is non-blocking but the stream isn't.
    fn accept(&self) -> Option<Box<dyn ReadWrite>> {
        match self {
            Door::Tcp(l) => l.accept().ok().and_then(|(s, _)| {
                s.set_nonblocking(false).ok()?;
                s.set_read_timeout(Some(STEP)).ok()?;
                s.set_write_timeout(Some(STEP)).ok()?;
                Some(Box::new(s) as Box<dyn ReadWrite>)
            }),
            Door::Unix(l, _) => l.accept().ok().and_then(|(s, _)| {
                s.set_nonblocking(false).ok()?;
                s.set_read_timeout(Some(STEP)).ok()?;
                s.set_write_timeout(Some(STEP)).ok()?;
                Some(Box::new(s) as Box<dyn ReadWrite>)
            }),
        }
    }
}

type Paired = (String, [u8; 32], Value);

/// Takes connections one at a time until a Frame pairs, the key is used up, or it expires or gets cancelled.
#[allow(clippy::too_many_arguments)]
fn serve(door: &Door, conf: &Path, plat: &dyn Platform, test: bool, key: &str, name: &str, cancelled: &AtomicBool, ask: &Mutex<Ask>) -> (&'static str, Option<Paired>) {
    let sk = host_key(conf);
    let end = mono() + LIFE;
    let trusted = |frame: &str| -> Option<[u8; 32]> {
        let s = std::fs::read_to_string(conf.join("trusted-frames").join(format!("{frame}.pub"))).ok()?;
        B64.decode(s.trim()).ok()?.try_into().ok()
    };
    let confirm_replace = |frame: &str| -> bool {
        if test {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        *ask.lock().unwrap() = Ask { question: Some(format!("A Frame named {frame} was paired before with another key. Replace it? Enter: yes, Esc or tap: no")), answer: Some(tx) };
        let yes = rx.recv_timeout(Duration::from_secs(60)).unwrap_or(false);
        *ask.lock().unwrap() = Ask::default();
        yes
    };
    let sealed: Mutex<Option<Value>> = Mutex::new(None);
    let reply = |frame: &str, _: &[u8; 32]| -> Result<Value, String> {
        let r = reply(conf, plat, test, frame)?;
        *sealed.lock().unwrap() = Some(r.clone());
        Ok(r)
    };
    let mut tries = TRIES;
    loop {
        if cancelled.load(Relaxed) || TERM.load(Relaxed) {
            return ("cancelled", None);
        }
        if mono() > end {
            return ("expired", None);
        }
        let Some(mut s) = door.accept() else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        match host_one(&mut *s, key, name, &sk, tries, &trusted, &confirm_replace, &reply) {
            Ok(Outcome::Paired(f, pk)) => return ("ok", Some((f, pk, sealed.lock().unwrap().take().unwrap_or_default()))),
            Ok(Outcome::WrongKey) => {
                tries -= 1; // A wrong key is the only thing that counts as a try.
                if tries == 0 {
                    return ("locked", None);
                }
            }
            _ => {} // Junk, a bad or taken name, or a refusal doesn't count as a try (M1).
        }
        if cancelled.load(Relaxed) || TERM.load(Relaxed) {
            let _ = s.write_all(b"{\"error\": \"cancelled\"}\n");
        }
    }
}

pub fn main(conf: &Path, plat: Box<dyn Platform + Send + Sync>, test: bool) -> i32 {
    let locked = conf.join("pair-locked");
    if let Some(age) = std::fs::metadata(&locked).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok())
        && age.as_secs_f64() < PAUSE
    {
        eprintln!("a key was just locked out: wait {PAUSE:.0} s");
        return 1;
    }
    let key = if test { "123456".to_owned() } else { format!("{:06}", below(1_000_000)) };
    let name = plat.hostname();
    let mut screen = if test {
        None
    } else {
        match Screen::open(None) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("cc-host pair: the key can't be shown: {e}");
                return 1;
            }
        }
    };
    let door = match TcpListener::bind(("0.0.0.0", cc_proto::agent::PORT)) {
        Ok(l) => Door::Tcp(l),
        Err(_) => {
            // The agent holds 3399 and passes pairing connections on to this socket.
            let path = conf.join("pairing.sock");
            let _ = std::fs::remove_file(&path);
            let old = unsafe { libc::umask(0o177) };
            let l = UnixListener::bind(&path);
            unsafe { libc::umask(old) };
            match l {
                Ok(l) => Door::Unix(l, path),
                Err(e) => {
                    eprintln!("cc-host pair: pairing.sock: {e}");
                    return 1;
                }
            }
        }
    };
    let nb = match &door {
        Door::Tcp(l) => l.set_nonblocking(true),
        Door::Unix(l, _) => l.set_nonblocking(true),
    };
    if let Err(e) = nb {
        eprintln!("cc-host pair: {e}");
        return 1;
    }
    let flag = conf.join("pairing");
    if !conf.exists() {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(conf); // A fresh host's first pairing.
    }
    let _ = std::fs::write(&flag, b""); // For announce: pair=1
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t) };
    println!("@pair state=waiting");

    let cancelled = Arc::new(AtomicBool::new(false));
    let ask = Arc::new(Mutex::new(Ask::default()));
    let end = mono() + LIFE;
    let addr = cc_proto::lan::route_source(); // for the address tags (docs/pairing.md 3)
    let (state, paired) = std::thread::scope(|sc| {
        let worker = sc.spawn(|| serve(&door, conf, plat.as_ref(), test, &key, &name, &cancelled, &ask));
        let mut drawn: Option<(Option<String>, u64, (usize, usize))> = None;
        while !worker.is_finished() {
            let Some(s) = screen.as_mut() else {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            let question = ask.lock().unwrap().question.clone();
            let now = (question.clone(), (end - mono()).max(0.0) as u64, s.size());
            if drawn.as_ref() != Some(&now) {
                let (w, h) = now.2;
                if s.show(&draw::key_screen(&name, &key, addr, now.1, question.as_deref(), w, h)).is_err() {
                    cancelled.store(true, Relaxed);
                }
                drawn = Some(now);
            }
            for ev in s.wait(100) {
                let answer = |yes| ask.lock().unwrap().answer.take().map(|a| a.send(yes));
                match ev {
                    Ev::Key(Key::Enter) => {
                        answer(true);
                    }
                    Ev::Key(_) | Ev::Tap => {
                        if answer(false).is_none() {
                            cancelled.store(true, Relaxed); // Esc or a tap gets rid of the key right away.
                        }
                    }
                    Ev::Resized => drawn = None,
                    Ev::Closed => {
                        answer(false);
                        cancelled.store(true, Relaxed);
                    }
                }
            }
        }
        worker.join().unwrap_or(("cancelled", None))
    });
    drop(screen);
    drop(key); // It's destroyed however it ends: success, lockout, expiry or Esc.
    if let Door::Unix(_, path) = &door {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_file(&flag);
    println!("@pair state={state}");
    if state == "locked" {
        let _ = std::fs::write(&locked, b"");
    }
    let Some((frame, pk, r)) = paired else { return 1 };
    let saved = write_private(&conf.join("frames").join(format!("{frame}.json")), r.to_string().as_bytes())
        .and_then(|_| write_private(&conf.join("trusted-frames").join(format!("{frame}.pub")), B64.encode(pk).as_bytes()));
    if let Err(e) = saved {
        eprintln!("cc-host pair: {frame}'s files: {e}");
        return 1;
    }
    let mons = r["monitors"].as_array().cloned().unwrap_or_default();
    if !test {
        // This Frame's own krdp servers start when it connects (the agent's sessions, docs/agent.md
        // 5a). Restarting a running one makes it take the re-paired Frame's new password.
        for m in &mons {
            plat.systemctl(&["try-restart", &format!("control-center-frame@{frame}-{}.service", m["index"])]);
        }
    }
    println!("@paired frame={frame} slot={} monitors={} cred=per-frame", r["slot"], mons.len());
    0
}
