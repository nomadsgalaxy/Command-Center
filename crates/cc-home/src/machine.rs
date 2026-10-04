//! `cc-home machine ...`, ported from home/cc-home.py's machine(). It covers the monitors in
//! viewers.conf, the machines they belong to (trusted-hosts), their agents (cc_proto::agent) and
//! pairing (cc_proto::pair). align and pair --scan use the camera, so they live in scan.rs, and
use super::{conf_dir, die, py_err, read_all, target, viewers, write_all};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use cc_proto::agent::{self, Client};
use cc_proto::conf::{self, Json, Viewer, py_float};
use serde_json::{Map, Value, json};
use std::time::Duration;

static NULL: Value = Value::Null;

pub(crate) fn hosts() -> Vec<(String, Value)> {
    conf::trusted_hosts(&conf_dir())
}

/// Same as Python's trusted_hosts().get(m, {}).
pub(crate) fn host<'a>(hosts: &'a [(String, Value)], m: &str) -> &'a Value {
    hosts.iter().find(|(k, _)| k == m).map_or(&NULL, |(_, t)| t)
}

/// A JSON value's truth, the way Python's bool() sees it.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// What Python's f"{x}" prints for something json.loads gave.
pub(crate) fn py_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => py_repr_v(v),
    }
}

fn py_repr_v(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) if n.is_f64() => py_float(n.as_f64().unwrap_or(0.0)),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr_v).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!("{{{}}}", m.iter().map(|(k, v)| format!("{}: {}", py_repr(k), py_repr_v(v))).collect::<Vec<_>>().join(", ")),
    }
}

/// Python's repr of a str.
/// ponytail: "printable" is approximated (whitespace, controls and the common format characters
/// get escaped). Python's full Unicode table also escapes unassigned code points.
fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut o = String::from(q);
    for c in s.chars() {
        let n = c as u32;
        match c {
            '\\' => o.push_str("\\\\"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c if c == q => {
                o.push('\\');
                o.push(c);
            }
            _ if n < 0x20 || (0x7f..0xa1).contains(&n) => o.push_str(&format!("\\x{n:02x}")),
            c if !c.is_ascii() && (c.is_whitespace() || c.is_control() || matches!(n, 0xad | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x206f | 0xfeff)) => {
                o.push_str(&if n <= 0xff { format!("\\x{n:02x}") } else if n <= 0xffff { format!("\\u{n:04x}") } else { format!("\\U{n:08x}") })
            }
            c => o.push(c),
        }
    }
    o.push(q);
    o
}

/// An OS error kept as a string ("Connection refused (os error 111)"), worded the way Python says it.
fn py_io(s: &str) -> String {
    if let Some((msg, n)) = s.strip_suffix(')').and_then(|t| t.rsplit_once(" (os error ")) {
        return format!("[Errno {n}] {msg}");
    }
    if s.contains("timed out") { "timed out".into() } else { s.into() }
}

/// Matches [\w<extra>]+.
fn word(s: &str, extra: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || extra.contains(c))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

fn port(v: &Viewer) -> Option<i64> {
    v.target.rsplit_once(':')?.1.parse().ok()
}

/// The monitor's port. Python stopped with a traceback on a line without one; this says so instead.
pub(crate) fn port_of(v: &Viewer) -> i64 {
    port(v).unwrap_or_else(|| die(format!("{}: {} has no port", v.name, v.target)))
}

/// Same as Python's str.splitlines.
fn splitlines(t: &str) -> Vec<String> {
    let (mut out, mut cur, mut it) = (Vec::new(), String::new(), t.chars().peekable());
    while let Some(c) = it.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            if c == '\r' && it.peek() == Some(&'\n') {
                it.next();
            }
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Rewrites viewers.conf with change(lines), written to a temporary file and renamed into place.
/// If change returns Err, its message is printed and nothing's written. (Python read an unreadable
/// file as empty; here that stops instead, and the file is left alone.)
fn edit_conf(change: impl FnOnce(Vec<String>) -> Result<Vec<String>, String>) {
    use std::os::unix::fs::DirBuilderExt;
    let path = conf_dir().join("viewers.conf");
    let lines = match std::fs::read_to_string(&path) {
        Ok(t) => splitlines(&t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => die(format!("can't read {}: {e}", path.display())),
    };
    let lines = change(lines).unwrap_or_else(|e| die(e));
    let tmp = conf_dir().join("viewers.conf.tmp");
    let r = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(conf_dir())
        .and_then(|_| std::fs::write(&tmp, lines.join("\n") + "\n"))
        .and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = r {
        die(format!("can't write {}: {e}", path.display()));
    }
}

fn parse_opts(args: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for a in args {
        let (k, v) = a.split_once('=').unwrap_or_else(|| die("options are key=value"));
        match out.iter_mut().find(|(x, _)| x == k) {
            Some(e) => e.1 = v.into(), // like a dict: the last value wins, in the first one's place
            None => out.push((k.into(), v.into())),
        }
    }
    if out.iter().any(|(k, _)| !conf::OPTIONS.split(' ').any(|o| o == k)) {
        die(format!("unknown option (known: {})", conf::OPTIONS.replace(' ', ", ")));
    }
    out
}

/// The paired machine (its id) at this address or host name.
fn machine_at(hs: &[(String, Value)], h: &str) -> Option<String> {
    hs.iter().find(|(_, t)| truthy(&t["host_pk"]) && (t["addr"].as_str() == Some(h) || t["host"].as_str() == Some(h))).map(|(k, _)| k.clone())
}

// ---- the agent

pub(crate) enum Fail {
    NotPaired(String),
    NoAgent(String),
}

impl Fail {
    pub(crate) fn state(&self) -> &'static str {
        match self {
            Fail::NotPaired(_) => "not-paired",
            Fail::NoAgent(_) => "no-agent",
        }
    }
    pub(crate) fn msg(self) -> String {
        match self {
            Fail::NotPaired(m) | Fail::NoAgent(m) => m,
        }
    }
}

/// Connects to a paired machine's agent as this Frame (docs/agent.md). When it can't, you get
/// NotPaired or NoAgent, each saying what to do.
pub(crate) fn agent_for(m: &str) -> Result<Client, Fail> {
    if !truthy(&host(&hosts(), m)["host_pk"]) {
        return Err(Fail::NotPaired(format!("{m} isn't paired: Pair it (Machines, or cc-home machine pair)")));
    }
    let conf = conf_dir();
    let c = agent::trusted(&conf, m).and_then(|t| Client::connect(&t, &agent::frame_key_or_new(&conf)?, agent::PORT, Duration::from_secs(5)));
    c.map_err(|e| {
        let why = match e {
            agent::Error::Unreachable(s) => format!("unreachable ({})", py_io(&s)),
            agent::Error::HostChanged => "host-changed".into(),
            agent::Error::NotPaired(s) if s == "refused" => "not-paired".into(),
            agent::Error::NotPaired(s) => format!("not-paired ({s})"),
            e => format!("not-paired ({e})"),
        };
        Fail::NoAgent(format!("{m}: agent not answering ({why}): start Command Center on it (Command Center Host in its app menu, \
                               or cc-share up), or run cc-share install there"))
    })
}

pub(crate) fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// A paired machine's shared monitors, from its agent: (index, output, width, height), with the
/// size in the native pixels the stream has.
fn probe(m: &str) -> Vec<(i64, String, i64, i64)> {
    let mut cl = agent_for(m).unwrap_or_else(|e| die(e.msg()));
    let r = cl.call("monitors", Map::new(), Duration::from_secs(10), |_| {}).unwrap_or_else(|e| die(format!("{m}: {e}")));
    r["monitors"].as_array().into_iter().flatten()
        .map(|x| (x["index"].as_i64().unwrap_or(-1), py_text(&x["output"]), x["width"].as_i64().unwrap_or(0), x["height"].as_i64().unwrap_or(0))).collect()
}

/// Starts a paired monitor's server on its host before cc-panels connects, and stops it after.
/// Prints "@session <name> port=<p> ready=1", or "@session <name> state=<why>" and exits 1.
fn session(op: &str, name: &str) {
    let hs = hosts();
    let v = viewers().iter().find(|v| v.name == name).unwrap_or_else(|| die(format!("no monitor {name} (machine list)")));
    let port = port_of(v);
    if !truthy(&host(&hs, &v.machine)["host_pk"]) || port < 3410 {
        // The shared login (never paired) has its server on all the time.
        println!("@session {name} {}", if op == "start" { format!("port={port} ready=1") } else { "stopped=0".into() });
        return;
    }
    let mut cl = agent_for(&v.machine).unwrap_or_else(|e| {
        println!("@session {name} state={}", e.state());
        die(e.msg())
    });
    let index = (port - 3400).rem_euclid(10);
    let r = cl.call("session", args(json!({"op": op, "index": index})), Duration::from_secs(15), |e| {
        if e["event"] == "session" && e["index"] == index {
            println!("@session {name} state={}", py_text(&e["state"])); // "starting", so the window can show it
        }
    }).unwrap_or_else(|_| json!({"ok": false, "error": "no-answer"}));
    drop(cl);
    if !truthy(&r["ok"]) {
        println!("@session {name} state={}", py_text(&r["error"]));
        die(format!("{name}: session {op}: {} {}", py_text(&r["error"]), detail(&r)).trim());
    }
    if op == "start" {
        println!("@session {name} port={} ready=1 start_ms={}", py_text(&r["port"]), r.get("start_ms").map_or("0".into(), py_text));
    } else {
        println!("@session {name} stopped={}", truthy(&r["stopped"]) as u8);
    }
}

fn detail(r: &Value) -> String {
    if truthy(&r["detail"]) { py_text(&r["detail"]) } else { String::new() }
}

/// Pop-out (docs/remote-windows.md), for cc-panels. Lists a paired machine's windows, starts or
/// stops one window's stream, or asks the running cc-panels to pop one out.
fn window(op: &str, wh: &str, uuid: Option<&str>) {
    let (vs, hs) = (viewers(), hosts());
    let name = conf::find_viewer(wh, vs, &hs).map_or(wh.to_owned(), |v| v.name.clone());
    let v = vs.iter().find(|v| v.name == name);
    let machine = v.map(|v| v.machine.clone()).or_else(|| conf::find_machine(wh, vs, &hs)).filter(|m| !m.is_empty())
        .unwrap_or_else(|| die(format!("no monitor or machine {wh} (machine list)")));
    let braced = |u: &str| {
        let b = u.as_bytes();
        b.len() == 38 && b[0] == b'{' && b[37] == b'}'
            && b[1..37].iter().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_digit() || (b'a'..=b'f').contains(c) })
    };
    if uuid.is_some_and(|u| !braced(u)) {
        die("a window is its KWin uuid, braced: {xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}");
    }
    if op == "pop" {
        if v.is_none() {
            die("pop takes the source monitor (its panel), not a machine");
        }
        println!("{}", target().ask(&format!("pop {name} {}", uuid.unwrap_or("None")), 10.0).unwrap_or_else(|e| die(e)));
        return;
    }
    if v.is_some_and(|v| port_of(v) < 3410) {
        println!("@window {name} state=not-paired");
        die(format!("{name} is on the shared login, not paired: pair its machine to pop windows out"));
    }
    let mut cl = agent_for(&machine).unwrap_or_else(|e| {
        println!("@window {name} state={}", e.state());
        die(e.msg())
    });
    if op == "list" {
        let r = cl.call("window", args(json!({"op": "list"})), Duration::from_secs(10), |_| {}).unwrap_or_else(|e| die(format!("{name}: {e}")));
        if !truthy(&r["ok"]) {
            println!("@window {name} state={}", py_text(&r["error"]));
            die(format!("{name}: {}", py_text(&r["error"])));
        }
        for w in r["windows"].as_array().into_iter().flatten() {
            let f = |k: &str| py_text(&w[k]);
            let caption = if w.get("caption").is_some() { format!(" caption={}", f("caption")) } else { String::new() };
            println!("@window uuid={} app={} x={} y={} w={} h={}{caption}", f("uuid"), f("app"), f("x"), f("y"), f("w"), f("h"));
        }
        return;
    }
    let Some(uuid) = uuid else { die(format!("usage: machine window {op} <monitor> <uuid>")) };
    let r = cl.call("window", args(json!({"op": op, "uuid": uuid})), Duration::from_secs(15), |e| {
        if e["event"] == "window" && e["uuid"] == uuid {
            println!("@window {name} uuid={uuid} state={}", py_text(&e["state"]));
        }
    }).unwrap_or_else(|_| json!({"ok": false, "error": "no-answer"}));
    drop(cl);
    if !truthy(&r["ok"]) {
        let err = py_text(&r["error"]);
        println!("@window {name} state={}", if err == "unknown-command" { "unsupported" } else { &err });
        die(format!("{name}: window {op}: {err} {}", detail(&r)).trim());
    }
    if op == "start" {
        println!("@window {name} uuid={uuid} port={} ready=1 start_ms={}", py_text(&r["port"]), r.get("start_ms").map_or("0".into(), py_text));
    } else {
        println!("@window {name} uuid={uuid} stopped={}", truthy(&r["stopped"]) as u8);
    }
}

/// Drops the machine's login and cert pin, and tells the host through its agent when it answers.
/// Its monitors stay.
fn unpair(m: &str) {
    if m.chars().count() > 64 || !word(m, ".-") {
        die("usage: machine unpair <machine>");
    }
    let conf = conf_dir();
    conf::unpair_prepare(&conf, m); // if this is a retry, try the host again
    let t = host(&hosts(), m).clone();
    let frame = if truthy(&t["frame"]) { py_text(&t["frame"]) } else { std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_lowercase() };
    let step = format!("on {}: cc-share unpair {frame}", if truthy(&t["host"]) { py_text(&t["host"]) } else { m.into() });
    let mut told = false;
    match agent_for(m) {
        Ok(mut cl) => match cl.call("unpair", Map::new(), Duration::from_secs(10), |_| {}) {
            Ok(r) => {
                told = truthy(&r["ok"]);
                println!("{}", if told { "the host removed this Frame" } else { "the host refused to unpair" });
            }
            Err(e) => println!("the host didn't answer ({e})"),
        },
        Err(Fail::NoAgent(e)) => println!("{e}"),
        Err(Fail::NotPaired(_)) => {}
    }
    if truthy(&t["host_pk"]) && !told {
        println!("the host still trusts this Frame: {step}, or cc-home machine unpair {m} again when it's reachable");
    }
    // When the host wasn't told, it's kept as pending-unpair and machine list says so. Its label goes too.
    let gone = conf::unpair_finish(&conf, m, told);
    println!("{}", if gone { format!("unpaired {m}") } else { format!("{m} wasn't paired") });
}

/// Pairs with a host that's showing a key (docs/pairing.md). Runs the exchange on its port 3399
/// (pair.py frame's half, cc_proto::pair), then writes what it brought (conf::write_pairing). The
/// key itself never leaves this Frame. Err says why not, after printing its @pair line.
pub(crate) fn pair(addr: &str, key: &str, replace: bool) -> Result<(), String> {
    let k = key.trim();
    let key_ok = match k.len() {
        6 => digits(k),
        7 => digits(&k[..3]) && k.as_bytes()[3] == b' ' && digits(&k[4..]),
        _ => false,
    };
    if addr.chars().count() > 253 || !word(addr, ".:-") || !key_ok {
        return Err("usage: machine pair <addr> <6-digit key> [--replace]".into());
    }
    let key = k.replace(' ', "");
    let conf = conf_dir();
    {
        use std::os::unix::fs::DirBuilderExt; // for a fresh Frame (Python's pair stopped there with a traceback)
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&conf);
    }
    if let Err(e) = conf::pair_try(&conf, addr) {
        if e.starts_with(&format!("{} tries with ", conf::PAIR_TRIES)) {
            println!("@pair {addr} state=too-many");
        }
        return Err(e);
    }
    let frame = agent::frame_name();
    // The key paired before at this address. A changed one is refused unless we're replacing it.
    let known = conf::trusted_hosts(&conf).iter().filter(|(_, t)| t["addr"].as_str() == Some(addr) && truthy(&t["host_pk"]))
        .filter_map(|(_, t)| B64.decode(t["host_pk"].as_str()?).ok()).map(|b| b.try_into().unwrap_or([0u8; 32])).last();
    let sk = agent::frame_key_or_new(&conf).map_err(|e| format!("pairing: this Frame's key: {e}"))?;
    println!("@pair {addr} state=waiting");
    let s = std::net::ToSocketAddrs::to_socket_addrs(&(addr, agent::PORT)).into_iter().flatten()
        .find_map(|a| std::net::TcpStream::connect_timeout(&a, cc_proto::pair::STEP).ok());
    let Some(s) = s else {
        println!("@pair {addr} state=unreachable");
        return Err(format!("pairing: unreachable (nothing answered on {addr}:{}: is its key up, and its firewall open? cc-share pair --firewall on it)", agent::PORT));
    };
    s.set_read_timeout(Some(cc_proto::pair::STEP)).ok();
    s.set_write_timeout(Some(cc_proto::pair::STEP)).ok();
    let (reply, host, host_pk) = match cc_proto::pair::frame_pair(s, &key, &frame, &sk, known, replace) {
        Ok(x) => x,
        Err(cc_proto::pair::Fail::Refused(st, d)) => {
            println!("@pair {addr} state={st}");
            let d = if st == "host-changed" { "its key differs from the one paired before (cc-home pair --replace)".into() } else { d };
            return Err(format!("pairing: {}", format!("{st} {d}").trim()));
        }
        Err(cc_proto::pair::Fail::Bad(e)) => {
            println!("@pair {addr} state=failed");
            return Err(format!("pairing: {}", py_io(&e)));
        }
    };
    println!("@pair {addr} state=ok");
    let result = json!({"host": host, "host_pk": B64.encode(host_pk), "addr": addr, "reply": reply});
    match conf::write_pairing(&conf, addr, &frame, &result, replace) {
        Err(e) if e == "host-changed" => {
            println!("@pair {addr} state=host-changed");
            Err(format!("pairing: {addr} is another host now (its id changed): machine pair ... --replace to trust it"))
        }
        Err(e) => Err(e),
        Ok(said) => {
            conf::pair_done(&conf, addr);
            said.iter().for_each(|l| println!("{l}"));
            Ok(())
        }
    }
}

// ---- discover (mDNS)

/// Python's shlex.split: words, quotes and backslashes the way a POSIX shell reads them. None when
/// the quotes don't balance.
fn shlex(s: &str) -> Option<Vec<String>> {
    let (mut out, mut cur, mut have, mut it) = (Vec::new(), String::new(), false, s.chars().peekable());
    while let Some(c) = it.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            '\\' => {
                cur.push(it.next()?);
                have = true;
            }
            '\'' => {
                have = true;
                loop {
                    match it.next()? {
                        '\'' => break,
                        c => cur.push(c),
                    }
                }
            }
            '"' => {
                have = true;
                loop {
                    match it.next()? {
                        '"' => break,
                        '\\' if matches!(it.peek(), Some('\\' | '"')) => cur.push(it.next()?),
                        '\\' if it.peek().is_none() => return None,
                        c => cur.push(c),
                    }
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    Some(out)
}

/// A command's stdout, or the error Python's subprocess would give (TimeoutExpired's text on a
/// timeout).
fn run_timeout(argv: &[&str], secs: f64) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|e| format!("{}: '{}'", py_err(&e), argv[0]))?;
    let mut out = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed().as_secs_f64() < secs => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let list: Vec<String> = argv.iter().map(|a| format!("'{a}'")).collect();
                return Err(format!("Command '[{}]' timed out after {} seconds", list.join(", "), py_float(secs)));
            }
        }
    }
    Ok(String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned())
}

pub(crate) struct Found {
    key: String, // avahi's service name
    rank: i32,
    pub(crate) name: String,
    pub(crate) addr: String,
    port: String,
    txt: Vec<(String, String)>,
}

impl Found {
    pub(crate) fn txt(&self, k: &str) -> Option<&str> {
        self.txt.iter().rev().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

/// Command Center hosts announcing themselves over mDNS (_controlcenter._tcp, opt-in per host).
/// When a host shows up more than once, I take its address on the default route's interface (IPv4).
pub(crate) fn discover(wait: f64) -> Vec<Found> {
    try_discover(wait).unwrap_or_else(|e| die(e))
}

/// discover, but says why it couldn't browse instead of dying (a pairing scan can read the address off the host's screen instead).
pub(crate) fn try_discover(wait: f64) -> Result<Vec<Found>, String> {
    let out = run_timeout(&["avahi-browse", "-rpt", "_controlcenter._tcp"], wait + 5.0).map_err(|e| format!("can't browse the network (avahi-browse): {e}"))?;
    let route = std::process::Command::new("ip").args(["-4", "route", "get", "1.1.1.1"]).stdin(std::process::Stdio::null()).output();
    let route = route.map(|r| String::from_utf8_lossy(&r.stdout).into_owned()).unwrap_or_default();
    let route: Vec<&str> = route.split_whitespace().collect();
    let lan = route.iter().position(|w| *w == "dev").and_then(|i| route.get(i + 1)).copied();
    let mut hosts: Vec<Found> = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.splitn(10, ';').collect();
        if f.len() < 10 || f[0] != "=" || f[7].starts_with("127.") {
            continue;
        }
        let txt = shlex(f[9]).unwrap_or_else(|| die("can't read avahi-browse's answer: No closing quotation")); // (a ValueError in Python)
        let txt: Vec<(String, String)> = txt.iter().filter_map(|t| t.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
        let rank = (f[2] == "IPv4") as i32 * 2 + (Some(f[1]) == lan) as i32;
        let mut h = Found { key: f[3].into(), rank, name: f[3].into(), addr: f[7].into(), port: f[8].into(), txt };
        h.name = h.txt("host").unwrap_or(f[3]).to_owned();
        match hosts.iter_mut().find(|x| x.key == h.key) {
            Some(x) if rank > x.rank => *x = h,
            Some(_) => {}
            None => hosts.push(h),
        }
    }
    Ok(hosts)
}

fn discover_cmd(rest: &[String]) {
    let wait = match rest.iter().position(|a| a == "--wait") {
        Some(i) => rest.get(i + 1).and_then(|w| w.parse::<f64>().ok()).unwrap_or_else(|| die("--wait takes seconds")),
        None => 3.0,
    };
    let vs = viewers();
    for h in discover(wait) {
        let known: Vec<&str> = vs.iter().filter(|x| {
            let t = x.target.rsplit('@').next().unwrap_or("");
            let t = t.rsplit_once(':').map_or(t, |p| p.0);
            t == h.addr || t == h.name
        }).map(|x| x.name.as_str()).collect();
        let mut keys: Vec<&str> = h.txt.iter().map(|(k, _)| k.as_str()).filter(|k| k.strip_prefix('m').is_some_and(digits)).collect();
        keys.sort();
        keys.dedup();
        let mons: Vec<String> = keys.iter().map(|k| format!("{k}={}", h.txt(k).unwrap_or(""))).collect();
        let known = if known.is_empty() { "-".into() } else { known.join(",") };
        println!("{}", format!("@host name={} addr={} port={} monitors={} {} pair={} known={known}", h.name, h.addr, h.port,
                               h.txt("monitors").unwrap_or("0"), mons.join(" "), h.txt("pair").unwrap_or("0")).replace("  ", " "));
    }
}

// ---- the command

fn said(label: &str) -> String {
    if label.is_empty() { "label cleared".into() } else { format!("shown as {}", py_repr(label)) }
}

pub fn main(argv: &[&str]) {
    let cmd = argv.first().copied().unwrap_or("list");
    let mut rest: Vec<String> = argv.iter().skip(1).map(|s| s.to_string()).collect();
    let n = rest.len();
    let s = |i: usize| rest.get(i).map_or("", String::as_str);
    let ok = match cmd {
        "discover" | "list" => true,
        "pair" if rest.iter().any(|a| a == "--scan") => true,
        "rename" => n >= 1,
        "probe" | "remove" | "connect" | "unpair" | "align" => n == 1,
        "add" | "set" | "pair" => n >= 2,
        "window" => n >= 2 && ["list", "start", "stop", "pop"].contains(&s(0)),
        "session" => n == 2 && ["start", "stop"].contains(&s(0)),
        _ => false,
    };
    if !ok {
        die(super::machine_usage());
    }
    let (vs, hs) = (viewers(), hosts());
    if ["set", "remove", "connect"].contains(&cmd) {
        if let Some(v) = conf::find_viewer(&rest[0], vs, &hs) {
            rest[0] = v.name.clone(); // also find a monitor by the name the user sees
        }
    }
    if (cmd == "unpair" || cmd == "align") && conf::find_viewer(&rest[0], vs, &hs).is_none() {
        if let Some(m) = conf::find_machine(&rest[0], vs, &hs) {
            rest[0] = m;
        }
    } else if cmd == "align" {
        if let Some(v) = conf::find_viewer(&rest[0], vs, &hs) {
            rest[0] = v.name.clone();
        }
    }
    match cmd {
        "discover" => discover_cmd(&rest),
        "list" if rest.iter().any(|a| a == "--json") => {
            // for other clients (cc-panels has its own display rule)
            let out: Vec<Json> = vs.iter().map(|x| {
                let t = host(&hs, &x.machine);
                let index = (port_of(x) - 3400).rem_euclid(10);
                let pending = conf_dir().join("pending-unpair").join(format!("{}.json", x.machine)).exists();
                Json::Obj(vec![
                    ("name".into(), Json::Str(x.name.clone())),
                    ("label".into(), x.label.clone().map_or(Json::Null, Json::Str)),
                    ("display".into(), Json::Str(conf::display(x, &hs, vs))),
                    ("machine".into(), Json::Str(x.machine.clone())),
                    ("machine_label".into(), Json::from_serde(&t["label"])),
                    ("host".into(), Json::from_serde(&t["host"])),
                    ("output".into(), Json::from_serde(&t["monitors"][index.to_string()])),
                    ("target".into(), Json::Str(x.target.clone())),
                    ("pixels".into(), Json::Arr(vec![Json::Num(x.pixels.0.to_string()), Json::Num(x.pixels.1.to_string())])),
                    ("host_trusts_after_unpair".into(), Json::Bool(pending)),
                ])
            }).collect();
            println!("{}", Json::Arr(out).dumps());
        }
        "list" => {
            // the monitors (viewers.conf lines), grouped by the machine they belong to
            let mut order: Vec<&str> = Vec::new();
            for v in vs {
                if !order.contains(&v.machine.as_str()) {
                    order.push(&v.machine);
                }
            }
            for m in order {
                let t = host(&hs, m);
                let label = if truthy(&t["label"]) { format!("  ({})", py_text(&t["label"])) } else { String::new() };
                let h = if truthy(&t["host"]) && t["host"].as_str() != Some(m) { format!("  host {}", py_text(&t["host"])) } else { String::new() };
                println!("{m}{label}{h}");
                for x in vs.iter().filter(|x| x.machine == m) {
                    println!("  {} \"{}\": {} {}x{} curve={} autoconnect={}", x.name, conf::display(x, &hs, vs), x.target, x.pixels.0, x.pixels.1,
                             x.opt("curve").filter(|c| !c.is_empty()).unwrap_or("flat"), x.opt("autoconnect").unwrap_or("no"));
                }
            }
        }
        "rename" => {
            // what the user sees: a machine's label, or one monitor's with --monitor
            let label = rest[1..].iter().filter(|a| *a != "--monitor").cloned().collect::<Vec<_>>().join(" ");
            if rest.iter().any(|a| a == "--monitor") {
                let name = conf::find_viewer(&rest[0], vs, &hs).unwrap_or_else(|| die(format!("no monitor {} (machine list)", rest[0]))).name.clone();
                edit_conf(|ls| ls.into_iter().map(|l| if l.split_whitespace().next() == Some(&name) {
                    conf::set_options(&l, &[("label".into(), label.clone())])
                } else {
                    Ok(l)
                }).collect());
                println!("{name}: {}", said(&label));
            } else {
                let m = conf::find_machine(&rest[0], vs, &hs).unwrap_or_else(|| die(format!("no machine {} (machine list)", rest[0])));
                let path = conf_dir().join("trusted-hosts").join(format!("{m}.json"));
                let mut t = match (hs.iter().any(|(k, _)| *k == m), std::fs::read_to_string(&path).ok().and_then(|s| Json::parse(&s))) {
                    (true, Some(o @ Json::Obj(_))) => o,
                    _ => Json::obj(),
                };
                if label.chars().count() > 64 || label.chars().any(|c| (c as u32) < 32) {
                    die("a label: at most 64 characters, no control characters");
                }
                t.set("label", if label.is_empty() { Json::Null } else { Json::Str(label.clone()) });
                // a machine that was never paired gets a file with just its label
                conf::write_secret(&path, &t.dumps()).unwrap_or_else(|e| die(format!("can't write {}: {e}", path.display())));
                println!("{m}: {}", said(&label));
            }
        }
        "probe" => {
            // A machine's monitors, from its agent.
            let m = conf::find_machine(&rest[0], vs, &hs).or_else(|| machine_at(&hs, &rest[0]))
                .unwrap_or_else(|| die(format!("no paired machine {}: Pair it first", rest[0])));
            let found = probe(&m);
            for (index, output, w, h) in found {
                println!("monitor {index}: {output} {w}x{h}");
            }
        }
        "add" => {
            let sized = n > 2 && rest[2].split_once('x').is_some_and(|(w, h)| digits(w) && digits(h));
            let opts = parse_opts(&rest[if sized { 3 } else { 2 }..]);
            let (name, addr) = (&rest[0], &rest[1]);
            let addr_ok = addr.split_once('@').and_then(|(u, r)| Some((u, r.rsplit_once(':')?)))
                .is_some_and(|(u, (h, p))| word(u, ".-") && word(h, ".:-") && digits(p));
            if !word(name, ".-") || !addr_ok {
                die("usage: machine add <name> <user@host:port> [WxH] [curve=..] [autoconnect=..]");
            }
            let size = if sized {
                rest[2].clone()
            } else {
                // Ask its agent for the real size, since the panel's size and mouse scale come from it.
                let (h, p) = addr.rsplit_once(':').unwrap_or_default();
                let m = machine_at(&hs, h.rsplit_once('@').map_or(h, |x| x.1));
                let Some(m) = m else { die("give a size (WxH), or Pair it (Pair reads the size)") };
                let found = probe(&m);
                let index = (p.parse::<i64>().unwrap_or_else(|_| die("usage: machine add <name> <user@host:port>")) - 3400).rem_euclid(10);
                found.iter().find(|x| x.0 == index).map(|x| format!("{}x{}", x.2, x.3))
                    .unwrap_or_else(|| die(format!("no monitor {index} on {h}, or give WxH")))
            };
            edit_conf(|mut lines| {
                if conf::entry(&lines, name).is_some() {
                    return Err(format!("{name} exists (machine set)"));
                }
                let next = lines.iter().filter_map(|l| {
                    let w: Vec<&str> = l.split_whitespace().collect();
                    (w.len() >= 4 && !l.starts_with('#') && digits(w[2])).then(|| w[2].parse::<i64>().ok()).flatten()
                }).max().unwrap_or(0) + 1;
                lines.push(conf::set_options(&format!("{name:<15} {addr:<27} {next:<7} {size}"), &opts)?);
                Ok(lines)
            });
        }
        "set" => {
            let opts = parse_opts(&rest[1..]);
            edit_conf(|mut lines| {
                let i = conf::entry(&lines, &rest[0]).ok_or(format!("no machine {} (machine list)", rest[0]))?;
                lines[i] = conf::set_options(&lines[i], &opts)?;
                Ok(lines)
            });
        }
        "remove" => {
            let name = &rest[0];
            let mut data = read_all();
            let gone = vs.iter().find(|v| v.name == *name).map(|v| v.machine.clone()).filter(|m| !m.is_empty());
            if let Some(gone) = gone.filter(|g| !vs.iter().any(|v| v.machine == *g && v.name != *name)) {
                // That was the machine's last monitor, so workspaces anchored on it lose their primary.
                if let Some(ws) = data.get_mut("workspaces") {
                    for (_, w) in ws.items_mut().iter_mut() {
                        if w.at("primary").str() == Some(&gone) {
                            w.remove("primary");
                        }
                    }
                }
                write_all(&data);
            }
            edit_conf(|mut lines| {
                let i = conf::entry(&lines, name).ok_or(format!("no machine {name} (machine list)"))?;
                // Remember it, so pairing its machine later brings it back, spots and all.
                use std::io::Write;
                let path = conf_dir().join("viewers.removed");
                std::fs::OpenOptions::new().append(true).create(true).open(&path)
                    .and_then(|mut f| f.write_all(format!("{}\n", lines[i].trim_end()).as_bytes()))
                    .map_err(|e| format!("can't write {}: {e}", path.display()))?;
                lines.remove(i);
                Ok(lines)
            });
            println!("its saved spots stay in home.json (cc-home forget removes a whole spot); pairing its machine brings it back");
        }
        "connect" => {
            // connect one that autoconnect left out, right now (cc-panels' viewer connect)
            println!("{}", target().ask(&format!("viewer connect {}", rest[0]), 10.0).unwrap_or_else(|e| die(e)));
        }
        "window" => window(&rest[0], &rest[1], rest.get(2).map(String::as_str)),
        "session" => {
            let name = conf::find_viewer(&rest[1], vs, &hs).map_or(rest[1].clone(), |v| v.name.clone());
            session(&rest[0], &name)
        }
        "pair" if rest.iter().any(|a| a == "--scan") => {
            let addr = rest.iter().find(|a| !a.starts_with("--")).cloned();
            crate::scan::pair_scan(addr.as_deref(), rest.iter().any(|a| a == "--replace"));
        }
        "pair" => {
            let replace = rest.iter().any(|a| a == "--replace");
            let mut key = rest[1..].iter().filter(|a| *a != "--replace").cloned().collect::<Vec<_>>().join(" ");
            if key == "-" {
                // read it from stdin, not the command line, because ps shows command lines
                key.clear();
                let _ = std::io::stdin().read_line(&mut key);
            }
            pair(&rest[0], &key, replace).unwrap_or_else(|e| die(e));
        }
        "unpair" => unpair(&rest[0]),
        _ => {
            // one monitor, or every monitor on a machine
            let names: Vec<String> = if vs.iter().any(|v| v.name == rest[0]) {
                vec![rest[0].clone()]
            } else {
                vs.iter().filter(|v| v.machine == rest[0]).map(|v| v.name.clone()).collect()
            };
            if names.is_empty() {
                die(format!("no monitor or machine {} (machine list)", rest[0]));
            }
            crate::scan::scan(&names);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_texts() {
        assert_eq!(py_repr("Desk"), "'Desk'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a'b\"c\\\u{7f}é\u{a0}"), "'a\\'b\"c\\\\\\x7fé\\xa0'");
        assert_eq!(py_text(&json!([1.0, "x", null, {"a": true}])), "[1.0, 'x', None, {'a': True}]");
        assert_eq!(py_io("Connection refused (os error 111)"), "[Errno 111] Connection refused");
        assert_eq!(splitlines("a\r\nb\n\nc"), ["a", "b", "", "c"]);
        assert_eq!(shlex(r#""host=a b" 'm0=x' pair=1 "q=\"\\x""#).unwrap(), ["host=a b", "m0=x", "pair=1", "q=\"\\x"]);
        assert_eq!(shlex("\"open"), None);
    }
}
