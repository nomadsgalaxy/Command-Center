//! Checks cc-home against what home/cc-home.py did (docs/rust-host.md R1). I recorded the Python
//! once in tests/fixtures/{conf,align}-cross/ before it went (these were tests/conf-cross and
//! align-cross). Same files in, so it has to give the same output, exit code, files out and
//! requests to cc-panels.
//!
//! cc-panels is faked on a socket with its own name (CC_PANELS_SOCKET), so a running one is never
//! asked. nmcli, ssh, avahi-browse and ip are faked on PATH. The machine cases run in a network
//! namespace of their own (the test re-runs itself under unshare -rn) against a real cc-host on
//! its loopback: `cc-host serve --fake` (the recordings' agent) and `cc-host pair --test` (key
//! 123456). Never a real host.
//!
//! It needs cc-host built: cargo build --release --target aarch64-unknown-linux-musl -p cc-host
//! (or CC_HOST_BIN=<path>).
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(rel: &str) -> Value {
    let p = root().join("tests/fixtures").join(rel);
    serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn temp() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("cc-home-cross-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_files(d: &Path, files: &Value) {
    for (rel, text) in files.as_object().unwrap() {
        let p = d.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text.as_str().unwrap()).unwrap();
    }
}

/// Every file under d, as relative path -> (bytes, mode).
fn walk(d: &Path) -> Vec<(String, Vec<u8>, u32)> {
    let mut out = vec![];
    let mut todo = vec![d.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for e in std::fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                todo.push(p);
            } else {
                let rel = p.strip_prefix(d).unwrap().to_string_lossy().into_owned();
                out.push((rel, std::fs::read(&p).unwrap(), std::fs::metadata(&p).unwrap().permissions().mode() & 0o777));
            }
        }
    }
    out
}

/// A fake cc-panels on an abstract socket. It keeps each request and answers with `reply`.
struct Panels {
    name: String,
    asked: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Panels {
    fn start(reply: impl Fn(&str) -> String + Send + 'static) -> Panels {
        static N: AtomicUsize = AtomicUsize::new(0);
        let name = format!("cc-home-cross-panels-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let s = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(&name).unwrap()).unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_millis(50))).unwrap();
        let (asked, stop) = (Arc::new(Mutex::new(vec![])), Arc::new(AtomicBool::new(false)));
        let (a, st) = (asked.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut buf = vec![0u8; 8192];
            while !st.load(Ordering::Relaxed) {
                let Ok((n, from)) = s.recv_from(&mut buf) else { continue };
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                a.lock().unwrap().push(text.clone());
                let _ = s.send_to_addr(reply(&text).as_bytes(), &from);
            }
        });
        Panels { name, asked, stop, thread: Some(thread) }
    }

    /// A socket name nobody serves.
    fn none() -> Panels {
        Panels { name: format!("cc-home-cross-none-{}", std::process::id()), asked: Default::default(), stop: Default::default(), thread: None }
    }

    fn done(mut self) -> Value {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
        json!(*self.asked.lock().unwrap())
    }
}

/// `s` with each run of digits after `pre` (up to `post`) swapped for N.
fn mask(s: &str, pre: &str, post: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(pre) {
        out.push_str(&rest[..i + pre.len()]);
        rest = &rest[i + pre.len()..];
        let k = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if k > 0 && rest[k..].starts_with(post) {
            out.push('N');
            rest = &rest[k..];
        }
    }
    out + rest
}

fn cc_home(home: &Path, argv: &[&str], env: &[(String, String)], stdin: &str) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_cc-home")).args(argv).env("HOME", home).envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut i = c.stdin.take().unwrap();
    let input = stdin.to_owned();
    std::thread::spawn(move || i.write_all(input.as_bytes()));
    c.wait_with_output().unwrap()
}

fn cc_host_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CC_HOST_BIN") {
        return p.into();
    }
    let t = root().join("target");
    ["aarch64-unknown-linux-musl/release", "x86_64-unknown-linux-musl/release", "release", "debug"].iter().map(|d| t.join(d).join("cc-host"))
        .find(|p| p.exists()).expect("no cc-host build: cargo build --release --target aarch64-unknown-linux-musl -p cc-host (or CC_HOST_BIN=)")
}

/// A real cc-host on this namespace's port 3399 (`serve --fake` or `pair --test`), running on its
/// config as recorded.
struct Host(std::process::Child, PathBuf);

impl Host {
    fn start(kind: &str, conf: &Value) -> Host {
        let dir = temp();
        // the host's keys and what it trusts, not what the recorded host made of itself at start
        let keep = |k: &str| ["host-key", "host-id", "windows-on", "windows-captions"].contains(&k) || k.starts_with("trusted-frames/") || k.starts_with("frames/");
        write_files(&dir, &Value::Object(conf.as_object().unwrap().iter().filter(|(k, _)| keep(k)).map(|(k, v)| (k.clone(), v.clone())).collect()));
        let args: &[&str] = if kind == "agent" { &["serve", "--fake"] } else { &["pair", "--test"] };
        let mut p = Command::new(cc_host_bin()).args(args).env("CC_CONF", &dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut out = std::io::BufReader::new(p.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        assert!(line.contains("waiting") || line.contains("listening"), "cc-host {args:?}: {line}");
        std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
        Host(p, dir)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

/// The fixture keeps its long texts once, and "@text:<n>" points at texts[n].
fn expand(v: &mut Value, texts: &Value) {
    match v {
        Value::String(s) if s.starts_with("@text:") => *v = texts[s[6..].parse::<usize>().unwrap()].clone(),
        Value::Array(a) => a.iter_mut().for_each(|x| expand(x, texts)),
        Value::Object(o) => o.values_mut().for_each(|x| expand(x, texts)),
        _ => {}
    }
}

/// What a pairing writes (cc_proto::conf::write_pairing, the same as cc-home.py's write_pairing).
fn pairing_cases(cases: &Value) -> usize {
    for c in cases.as_array().unwrap() {
        let d = temp();
        write_files(&d, &c["files"]);
        let lines = match cc_proto::conf::write_pairing(&d, "203.0.113.85", "steam-frame", &c["result"], c["replace"] == true) {
            Ok(l) => json!(l),
            Err(e) => json!([format!("error {e}")]),
        };
        assert_eq!(lines, c["lines"], "{}", c["name"]);
        let got: Map<String, Value> = walk(&d).into_iter().map(|(k, b, m)| (k, json!([String::from_utf8(b).unwrap(), format!("0o{m:o}")]))).collect();
        assert_eq!(Value::Object(got), c["out"], "{}", c["name"]);
        let _ = std::fs::remove_dir_all(d);
    }
    cases.as_array().unwrap().len()
}

/// The files a run left, in the recorded form: times, a pairing's password and a new key are
/// compared by their size, since they change every run.
fn files_out(d: &Path) -> Value {
    let mut out = Map::new();
    for (k, b, m) in walk(d) {
        if k.starts_with(".cache/control-center/hibernate") {
            continue;
        }
        let v = if k.ends_with("/pair-tries.json") {
            let t: Map<String, Value> = serde_json::from_slice(&b).unwrap();
            Value::Object(t.into_iter().map(|(a, v)| (a, json!(v.as_array().unwrap().len()))).collect())
        } else if k.contains("/passwords/") || k.ends_with("/frame-key") {
            json!(b.len())
        } else {
            json!(String::from_utf8(b).unwrap())
        };
        out.insert(k, json!([v, format!("0o{m:o}")]));
    }
    Value::Object(out)
}

fn cli_case(c: &Value, bindir: &Path) {
    let name = c["name"].as_str().unwrap();
    let d = temp();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    for (rel, text) in c["files"].as_object().unwrap() {
        let p = d.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text.as_str().unwrap().replace("\"NOW\"", &now.to_string())).unwrap();
    }
    let panels = match &c["panels"] {
        Value::Null => Panels::none(),
        p => {
            let (all, refused) = (p[0].as_str().unwrap().to_owned(), p[1].clone());
            Panels::start(move |text| {
                let w: Vec<&str> = text.split_whitespace().collect();
                if w[0] == "panels" { all.clone() } else if refused.as_array().unwrap().iter().any(|r| r == w[1]) { format!("error no panel {}", w[1]) } else { "ok".into() }
            })
        }
    };
    let host = c["host"].as_str().map(|k| Host::start(k, &c["hostconf"]));
    let mut env: Vec<(String, String)> = c["env"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned())).collect();
    env.push(("PATH".into(), format!("{}:{}", bindir.display(), std::env::var("PATH").unwrap())));
    env.push(("CC_PANELS_SOCKET".into(), panels.name.clone()));
    let argv: Vec<&str> = c["argv"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
    let r = cc_home(&d, &argv, &env, c["stdin"].as_str().unwrap());
    drop(host);
    let asked = panels.done();
    let home = d.to_string_lossy();
    let stdout = mask(&String::from_utf8_lossy(&r.stdout), "start_ms=", "").replace(&*home, "~");
    let stderr = mask(&String::from_utf8_lossy(&r.stderr), "wait ", " s").replace(&*home, "~");
    let code = r.status.code().unwrap_or(-1);
    assert_eq!(stdout, c["stdout"].as_str().unwrap(), "{name} {argv:?}: stdout");
    let want_err = c["stderr"].as_str().unwrap();
    if !(want_err.starts_with("Traceback (most recent call last)") && !stderr.is_empty() && code != 0) {
        // Python stopped with a traceback and Rust says it in one line. Both fail and write nothing.
        assert_eq!(stderr, want_err, "{name} {argv:?}: stderr");
    }
    assert_eq!(code, c["exit"].as_i64().unwrap() as i32, "{name} {argv:?}: exit code");
    assert_eq!(files_out(&d), c["out"], "{name} {argv:?}: files");
    assert_eq!(asked, c["asked"], "{name} {argv:?}: asked cc-panels");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn conf_cross() {
    if std::env::var("CC_CROSS_NETNS").as_deref() != Ok("1") {
        // a network of its own (the hosts' port 3399 on a loopback nobody else has), plus a hostname
        let ok = Command::new("unshare").args(["-rnu", "env", "CC_CROSS_NETNS=1"]).arg(std::env::current_exe().unwrap())
            .args(["conf_cross", "--exact", "--nocapture", "--test-threads=1"]).status().unwrap();
        assert!(ok.success(), "conf_cross in its namespace (above)");
        return;
    }
    assert!(Command::new("ip").args(["link", "set", "lo", "up"]).status().unwrap().success());
    assert_eq!(unsafe { libc::sethostname(c"frame".as_ptr(), 5) }, 0); // this Frame's name, as recorded (a UTS namespace of its own too)
    let mut rec = fixture("conf-cross/cases.json");
    let texts = rec["texts"].take();
    expand(&mut rec, &texts);
    let n = pairing_cases(&rec["pairing"]);
    eprintln!("conf-cross: {n} pairing cases, cc-proto wrote what cc-home.py wrote");
    let bindir = temp();
    for (f, text) in rec["bin"].as_object().unwrap() {
        std::fs::write(bindir.join(f), text.as_str().unwrap()).unwrap();
        std::fs::set_permissions(bindir.join(f), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cases = rec["cli"].as_array().unwrap();
    for c in cases {
        cli_case(c, &bindir);
    }
    let _ = std::fs::remove_dir_all(bindir);
    eprintln!("conf-cross ok: {} cc-home command cases ({} against cc-host) printed, exited and wrote what cc-home.py did",
              cases.len(), cases.iter().filter(|c| !c["host"].is_null()).count());
}

/// The align's last step: a solve's output lines get placed and saved (CC_HOME_SOLVED, so no
/// camera and no hosts).
#[test]
fn align_cross() {
    let cases = fixture("align-cross/cases.json");
    for c in cases.as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let d = temp();
        write_files(&d, &c["files"]);
        let solved = d.join("solved.txt");
        let lines: Vec<&str> = c["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        std::fs::write(&solved, lines.join("\n") + "\n").unwrap();
        let panels = Panels::start(|t| if t.starts_with("panels") { "ok 0 panels\n".into() } else { "ok".into() });
        let env = [("CC_PANELS_SOCKET".to_owned(), panels.name.clone()), ("CC_HOME_SOLVED".to_owned(), solved.to_string_lossy().into_owned())];
        let r = cc_home(&d, &["machine", "align", "desk-wide", "--progress"], &env, "");
        let asked = panels.done();
        std::fs::remove_file(&solved).unwrap();
        assert_eq!(String::from_utf8_lossy(&r.stdout), c["stdout"].as_str().unwrap(), "{name}: stdout");
        assert_eq!(String::from_utf8_lossy(&r.stderr), c["stderr"].as_str().unwrap(), "{name}: stderr");
        assert_eq!(r.status.code().unwrap_or(-1) as i64, c["exit"].as_i64().unwrap(), "{name}: exit code");
        let got: Map<String, Value> = walk(&d).into_iter().map(|(k, b, _)| (k, json!(String::from_utf8(b).unwrap()))).collect();
        assert_eq!(Value::Object(got), c["out"], "{name}: files");
        assert_eq!(asked, c["asked"], "{name}: asked cc-panels");
        let _ = std::fs::remove_dir_all(d);
    }
    eprintln!("align-cross ok: {} cases placed, saved and said what cc-home.py did", cases.as_array().unwrap().len());
}
