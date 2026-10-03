//! Tests pairing with the Rust Frame (cc_proto::pair::frame_pair) against the Rust host (`cc-host pair
//! --test`: key 123456, one monitor, no screen or units) over the real port. A right key pairs, and
//! the host keeps the Frame's slot, login and key (0600 in 0700). Hostile names and junk don't count
//! as tries. 3 wrong keys lock the key, and then there's a 30 s wait before another. SIGTERM cancels,
//! and the flag file and the socket go away. This was tests/pair-host and tests/pair-cross (Python,
//! against pair.py).
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use cc_proto::pair::{Fail, frame_pair};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn key() -> SigningKey {
    let mut s = [0u8; 32];
    std::fs::File::open("/dev/urandom").unwrap().read_exact(&mut s).unwrap();
    SigningKey::from_bytes(&s)
}

/// A host process that gets killed if the test fails, since it holds port 3399.
struct Host(Child);

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl std::ops::Deref for Host {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Host {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

/// Starts `cc-host pair --test` on `conf` and returns once it says it's waiting.
fn host(conf: &Path) -> (Host, BufReader<std::process::ChildStdout>) {
    let mut p = Command::new(env!("CARGO_BIN_EXE_cc-host")).args(["pair", "--test"]).env("CC_CONF", conf)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut out = BufReader::new(p.stdout.take().unwrap());
    let mut l = String::new();
    out.read_line(&mut l).unwrap();
    assert!(l.contains("state=waiting"), "{l}");
    assert!(conf.join("pairing").exists(), "the flag file (announce: pair=1)");
    (Host(p), out)
}

fn conn() -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", 3399)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

/// Sends one raw line and returns the host's answer, or "" if it just closed.
fn raw(data: &[u8]) -> String {
    let mut s = conn();
    s.write_all(data).unwrap();
    let mut buf = vec![0u8; 4096];
    let n = s.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn rest(out: &mut BufReader<std::process::ChildStdout>) -> String {
    let mut s = String::new();
    let _ = out.read_to_string(&mut s);
    s
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn pairing() {
    let conf: PathBuf = std::env::temp_dir().join(format!("cc-host-pair-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&conf);
    let sk = key();
    let pk = B64.encode(sk.verifying_key().to_bytes());

    // A right key pairs. Check what the host keeps.
    let (mut h, mut out) = host(&conf);
    let (reply, name, host_pk) = frame_pair(conn(), "123456", "frame1", &sk, None, false).expect("paired");
    assert_eq!(h.wait().unwrap().code(), Some(0));
    let said = rest(&mut out);
    assert!(said.contains("@paired frame=frame1 slot=1 monitors=1 cred=per-frame"), "{said}");
    assert!(name == "fakehost" && reply["user"] == "cc-frame1" && reply["slot"] == 1 && reply["monitors"][0]["port"] == 3410, "{reply}");
    assert!(reply["password"].as_str().is_some_and(|p| p.len() == 24) && reply["id"].as_str().is_some_and(|i| i.len() == 36), "{reply}");
    let kept: Value = serde_json::from_slice(&std::fs::read(conf.join("frames/frame1.json")).unwrap()).unwrap();
    assert_eq!(kept, reply, "the Frame's login as sent");
    assert_eq!(std::fs::read_to_string(conf.join("trusted-frames/frame1.pub")).unwrap().trim(), pk);
    assert_eq!((mode(&conf.join("frames/frame1.json")), mode(&conf.join("frames")), mode(&conf.join("trusted-frames/frame1.pub"))), (0o600, 0o700, 0o600));
    assert!(!conf.join("pairing").exists(), "the flag file went");
    assert_eq!(host_pk.len(), 32);

    // The same name with another key gets name-taken, since no one's at the host to confirm a replace.
    let (mut h, _out) = host(&conf);
    assert!(matches!(frame_pair(conn(), "123456", "frame1", &key(), None, false), Err(Fail::Refused(s, _)) if s == "name-taken"));
    // A changed host key is refused before anything is sealed.
    assert!(matches!(frame_pair(conn(), "123456", "frame2", &sk, Some([2u8; 32]), false), Err(Fail::Refused(s, _)) if s == "host-changed"));
    unsafe { libc::kill(h.id() as i32, libc::SIGTERM) };
    assert_eq!(h.wait().unwrap().code(), Some(1));

    // Hostile names and junk don't count as tries. 3 wrong keys lock it.
    let (mut h, mut out) = host(&conf);
    for bad in ["../x", "a b", &"x".repeat(33), "Upper", "", "-lead"] {
        let m = json!({"v": 1, "frame": bad, "frame_pk": pk, "spake": B64.encode([b'x'; 33])});
        let mut line = cc_proto::agent::canon(&m);
        line.push(b'\n');
        assert!(raw(&line).contains("bad-name"), "{bad:?}");
    }
    raw(&[b'x'; 5000]);
    for junk in [b"not json\n".as_slice(), b"{\"v\":1}\n", b"[1,2]\n"] {
        raw(junk);
    }
    let states: Vec<String> = (0..3).map(|_| match frame_pair(conn(), "000000", "frame9", &sk, None, false) {
        Err(Fail::Refused(s, _)) => s,
        other => format!("{other:?}"),
    }).collect();
    assert_eq!(states, ["bad-key", "bad-key", "locked"]);
    assert_eq!(h.wait().unwrap().code(), Some(1));
    assert!(rest(&mut out).contains("state=locked"));
    assert!(conf.join("pair-locked").exists() && !conf.join("pairing").exists());
    let again = Command::new(env!("CARGO_BIN_EXE_cc-host")).args(["pair", "--test"]).env("CC_CONF", &conf).output().unwrap();
    assert!(again.status.code() == Some(1) && String::from_utf8_lossy(&again.stderr).contains("wait 30 s"));
    std::fs::remove_file(conf.join("pair-locked")).unwrap();

    // SIGTERM cancels, and nothing gets written.
    let (mut h, mut out) = host(&conf);
    unsafe { libc::kill(h.id() as i32, libc::SIGTERM) };
    assert_eq!(h.wait().unwrap().code(), Some(1));
    assert!(rest(&mut out).contains("state=cancelled") && !conf.join("pairing").exists());
    assert!(!conf.join("frames/frame9.json").exists());
    let _ = std::fs::remove_dir_all(&conf);
}
