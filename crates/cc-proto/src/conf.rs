//! What the Frame keeps after a pairing (docs/pairing.md §5-6, D-050). It's written byte for byte
//! the way cc-home's pair() writes it, so either one can pair and the other reads the result:
//! - trusted-hosts/<id>.json and passwords/<id>.
//! - The viewers.conf lines. A machine's line is found by (id, monitor), then by name, then a
//!   hand-added one on that host's shared login, then a removed one brought back.
//! - A pre-id host migrated to its id (files, machine=, a workspace's primary, its label).
//! - viewers.removed.
//! cc-home's tests/cross.rs checks each case against what cc-home.py wrote (tests/fixtures/conf-cross).
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

pub const PAIR_TRIES: usize = 3;
pub const PAIR_WINDOW: f64 = 600.0;

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The Frame's own limit per host (L3): at most PAIR_TRIES tries in PAIR_WINDOW seconds. Calling
/// this records the try.
pub fn pair_try(conf: &Path, addr: &str) -> Result<(), String> {
    let path = conf.join("pair-tries.json");
    let mut tries: serde_json::Map<String, Value> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let t = now();
    let recent: Vec<f64> = tries.get(addr).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).filter(|x| t - x < PAIR_WINDOW).collect()).unwrap_or_default();
    if recent.len() >= PAIR_TRIES {
        return Err(format!("{PAIR_TRIES} tries with {addr} in {} minutes: wait {} s", PAIR_WINDOW as i64 / 60, (PAIR_WINDOW - (t - recent[0])) as i64 + 1));
    }
    let mut r = recent;
    r.push(t);
    tries.insert(addr.into(), r.into());
    std::fs::write(&path, serde_json::to_string(&tries).unwrap_or_default()).map_err(|e| e.to_string())
}

/// A pairing went through, so its tries get forgotten.
pub fn pair_done(conf: &Path, addr: &str) {
    let path = conf.join("pair-tries.json");
    let mut tries: serde_json::Map<String, Value> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    tries.remove(addr);
    let _ = std::fs::write(&path, serde_json::to_string(&tries).unwrap_or_default());
}

/// A JSON string the way Python's json.dumps writes it (ensure_ascii).
pub fn py_str(s: &str) -> String {
    let mut o = String::from('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{8}' => o.push_str("\\b"),
            '\u{c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 || !c.is_ascii() => {
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    o.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn py_opt(s: Option<&str>) -> String {
    s.map_or("null".into(), py_str)
}

/// Writes a file only its owner can read (0600, folder 0700), all at once (cc-home's write_secret).
pub fn write_secret(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(d) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d)?;
    }
    let tmp = path.with_extension(format!("{}tmp", path.extension().map_or(String::new(), |e| format!("{}.", e.to_string_lossy()))));
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?.write_all(text.as_bytes())?;
    std::fs::rename(tmp, path)
}

/// Python's re.split(r"(\s+)", s): the words and the whitespace between them, alternating.
fn split_keep(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut space = false;
    for c in s.chars() {
        let w = c.is_whitespace();
        if w != space {
            out.push(String::new());
            space = w;
        }
        out.last_mut().unwrap().push(c);
    }
    if out.len() > 1 && out[0].is_empty() {
        // a leading run of spaces: Python gives ['', ' ', ...]
    }
    out
}

fn words(l: &str) -> Vec<&str> {
    l.split_whitespace().collect()
}

fn line_opts(l: &str) -> Vec<(String, String)> {
    words(l).iter().skip(4).filter_map(|o| o.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned()))).collect()
}

fn opt<'a>(opts: &'a [(String, String)], k: &str) -> Option<&'a str> {
    opts.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
}

/// cc-home's set_options for one plain option (machine=): set in place, or added after two spaces.
pub fn set_option(line: &str, k: &str, v: &str) -> String {
    let mut parts = split_keep(line.trim_end());
    let i = parts.iter().enumerate().position(|(j, t)| j >= 8 && t.starts_with(&format!("{k}=")));
    match (i, v.is_empty()) {
        (None, false) => {
            parts.push("  ".into());
            parts.push(format!("{k}={v}"));
        }
        (Some(i), false) => parts[i] = format!("{k}={v}"),
        (Some(i), true) => {
            parts.drain(i - 1..=i);
        }
        (None, true) => {}
    }
    parts.concat()
}

/// cc-home's OPTIONS: the viewers.conf options `machine set` takes, in its order. It's one string
/// because cc-home's tests/nossh.rs looks for a quoted ssh as a command line.
pub const OPTIONS: &str = "curve autoconnect machine radius ssh label";

/// urllib.parse.quote(s, safe=""): keeps RFC 3986's unreserved characters, every other byte is %XX.
pub fn quote(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn word(s: &str, extra: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || extra.contains(c))
}

/// cc-home's set_options: checks each key=val the way OPTIONS says, then sets it in place (or adds
/// it). An empty val drops the option. Err is cc-home's message, and nothing gets written then.
pub fn set_options(line: &str, opts: &[(String, String)]) -> Result<String, String> {
    let mut line = line.to_owned();
    for (k, v) in opts {
        let mut v = v.clone();
        if !v.is_empty() {
            match OPTIONS.split(' ').position(|o| o == k).unwrap_or(9) {
                5 => {
                    // any text, percent-encoded, so the line still splits on whitespace
                    if v.chars().count() > 64 || v.chars().any(|c| (c as u32) < 32) {
                        return Err(format!("{k}: at most 64 characters, no control characters"));
                    }
                    v = quote(&v);
                }
                4 => {
                    // the host's login, for align (user@host)
                    let ok = v.split_once('@').is_some_and(|(u, h)| u.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
                        && u.chars().count() <= 32 && word(u, ".-") && word(h, ".:-"));
                    if !ok {
                        return Err(format!("{k} is user@host"));
                    }
                }
                3 => {
                    let (i, f) = v.split_once('.').unwrap_or((&v, "0"));
                    let digits = |t: &str| !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit());
                    if !(digits(i) && digits(f)) || !(0.2..=20.0).contains(&v.parse::<f64>().unwrap_or(0.0)) {
                        return Err(format!("{k} is in metres, 0.2 to 20 (a 1000R monitor: 1.0)"));
                    }
                }
                2 if !word(&v, ".-") => return Err(format!("bad {k} name")),
                0 if !["h", "v", "flat"].contains(&v.as_str()) => return Err(format!("{k} is one of h, v, flat")),
                1 if !["yes", "no"].contains(&v.as_str()) => return Err(format!("{k} is one of yes, no")),
                _ => {}
            }
        }
        line = set_option(&line, k, &v);
    }
    Ok(line)
}

/// The line for the monitor `name` (cc-home's entry).
pub fn entry(lines: &[String], name: &str) -> Option<usize> {
    lines.iter().position(|l| words(l).first() == Some(&name) && words(l).len() >= 4)
}

fn index_of(l: &str) -> Option<i64> {
    let t = words(l).get(1)?.rsplit_once(':')?.1;
    t.parse::<i64>().ok().map(|p| (p - 3400) % 10)
}

/// The trusted hosts: machine -> its JSON.
pub fn trusted_hosts(conf: &Path) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = std::fs::read_dir(conf.join("trusted-hosts")).into_iter().flatten().flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let m = name.strip_suffix(".json")?.to_owned();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
        v.is_object().then_some((m, v))
    }).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Writes what a pairing brought, like cc-home's pair() after the exchange. `result` is pair.py
/// frame's line, {"host", "host_pk", "addr", "reply"}. Returns the progress lines it would print
/// ("@pair <addr> kept=...", "@paired ..."), or why it didn't ("host-changed").
pub fn write_pairing(conf: &Path, addr: &str, frame: &str, result: &Value, replace: bool) -> Result<Vec<String>, String> {
    let host = result["host"].as_str().unwrap_or("");
    let r = &result["reply"];
    let monitors = r["monitors"].as_array().cloned().unwrap_or_default();
    let host_ok = !host.is_empty() && host.len() <= 64 && host.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '-');
    let mons_ok = monitors.iter().all(|m| {
        let (p, w, h) = (m["port"].as_i64().unwrap_or(0), m["width"].as_i64().unwrap_or(0), m["height"].as_i64().unwrap_or(0));
        (3410..=3449).contains(&p) && 0 < w && w < 20000 && 0 < h && h < 20000
    });
    if !host_ok || !mons_ok {
        return Err("pairing: the host's name or monitors aren't usable".into());
    }
    let mut said = Vec::new();
    let id = r["id"].as_str();
    let mid = id.unwrap_or(host).to_owned();
    let host_pk = result["host_pk"].as_str().unwrap_or("");
    let known = trusted_hosts(conf);
    let same: Vec<&(String, Value)> = known.iter().filter(|(_, t)| t["addr"].as_str() == Some(addr) || t["host_pk"].as_str() == Some(host_pk)).collect();
    let changed = same.iter().any(|(_, t)| t["id"].as_str().is_some_and(|i| i != mid));
    if changed && !replace {
        return Err("host-changed".into());
    }
    let olds: BTreeSet<String> = same.iter().filter(|(k, t)| *k != mid && (t["id"].as_str().is_none() || replace)).map(|(k, _)| k.clone()).collect();
    let label = known.iter().find(|(k, _)| *k == mid).and_then(|(_, t)| t["label"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| same.iter().find_map(|(_, t)| t["label"].as_str().filter(|s| !s.is_empty())));
    let mons_json = monitors.iter().map(|m| format!("{}: {}", py_str(&m["index"].as_i64().unwrap_or(0).to_string()), py_str(m["output"].as_str().unwrap_or("")))).collect::<Vec<_>>().join(", ");
    let th = format!("{{\"id\": {}, \"host\": {}, \"addr\": {}, \"host_pk\": {}, \"frame\": {}, \"cert_sha256\": {}, \"label\": {}, \"monitors\": {{{mons_json}}}}}",
                     py_opt(id), py_str(host), py_str(addr), py_str(host_pk), py_str(frame), py_str(r["cert_sha256"].as_str().unwrap_or("")), py_opt(label));
    write_secret(&conf.join("trusted-hosts").join(format!("{mid}.json")), &th).map_err(|e| e.to_string())?;
    write_secret(&conf.join("passwords").join(&mid), r["password"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
    if !olds.is_empty() {
        // a workspace anchored on the old name moves to the id (the file the way cc-panels and cc-home write it: indent 2)
        if let Ok(text) = std::fs::read_to_string(conf.join("home.json")) {
            let mut t = text.clone();
            for o in &olds {
                t = t.replace(&format!("\"primary\": {}", py_str(o)), &format!("\"primary\": {}", py_str(&mid)));
            }
            if t != text {
                let _ = std::fs::write(conf.join("home.json"), t);
            }
        }
    }
    for o in &olds {
        let _ = std::fs::remove_file(conf.join("trusted-hosts").join(format!("{o}.json")));
        let _ = std::fs::remove_file(conf.join("passwords").join(o));
    }
    let removed_path = conf.join("viewers.removed");
    let removed: Vec<String> = std::fs::read_to_string(&removed_path).unwrap_or_default().lines().filter(|l| words(l).len() >= 4).map(str::to_owned).collect();
    let mut restored: Vec<String> = Vec::new();
    let conf_path = conf.join("viewers.conf");
    let mut lines: Vec<String> = std::fs::read_to_string(&conf_path).unwrap_or_default().lines().map(str::to_owned).collect();
    let mut screens: Vec<i64> = lines.iter().filter(|l| words(l).len() >= 4 && !l.starts_with('#')).filter_map(|l| words(l)[2].parse().ok()).collect();
    for l in lines.iter_mut() {
        if opt(&line_opts(l), "machine").is_some_and(|m| olds.contains(m)) {
            *l = set_option(l, "machine", &mid);
        }
    }
    let user = r["user"].as_str().unwrap_or("");
    for m in &monitors {
        let (index, output) = (m["index"].as_i64().unwrap_or(0), m["output"].as_str().unwrap_or(""));
        let name = format!("{host}-{output}").to_lowercase();
        let target = format!("{user}@{addr}:{}", m["port"].as_i64().unwrap_or(0));
        let size = format!("{}x{}", m["width"].as_i64().unwrap_or(0), m["height"].as_i64().unwrap_or(0));
        let mut i = lines.iter().position(|l| words(l).len() >= 4 && !l.starts_with('#') && opt(&line_opts(l), "machine") == Some(mid.as_str())
                                                 && words(l)[1].rsplit_once(':').is_some_and(|(_, p)| p.parse::<i64>().is_ok()) && index_of(l) == Some(index));
        if i.is_none() {
            i = entry(&lines, &name);
        }
        let shared_target = format!("@{addr}:{}", 3400 + index);
        let same_l = |l: &str| words(l).len() >= 4 && !l.starts_with('#') && words(l)[1].ends_with(&shared_target)
                             && words(l)[1].len() > shared_target.len() && !words(l)[1][..words(l)[1].len() - shared_target.len()].chars().any(|c| c.is_whitespace() || c == '@');
        let mut old = if i.is_some() { None } else { lines.iter().position(|l| same_l(l)) };
        if i.is_none() && old.is_none() {
            let ours = |l: &str| opt(&line_opts(l), "machine").is_some_and(|x| x == mid || olds.contains(x))
                                && words(l)[1].rsplit_once(':').is_some_and(|(_, p)| p.parse::<i64>().is_ok()) && index_of(l) == Some(index);
            let gone: Vec<&String> = removed.iter().filter(|l| (same_l(l) || ours(l)) && entry(&lines, words(l)[0]).is_none()).collect();
            if let Some(last) = gone.last() {
                let mut back = split_keep(last);
                if let Ok(n) = back[4].parse::<i64>() {
                    if screens.contains(&n) {
                        let next = screens.iter().max().copied().unwrap_or(0) + 1;
                        screens.push(next);
                        back[4] = next.to_string();
                    } else {
                        screens.push(n);
                    }
                }
                lines.push(back.concat());
                old = Some(lines.len() - 1);
                restored.push((*last).clone());
            }
        }
        if let Some(o) = old {
            let mut p = split_keep(&lines[o]);
            let was = p[2].rsplit_once(':').map_or(p[2].clone(), |x| x.0.to_owned());
            p[2] = target.clone();
            lines[o] = set_option(&p.concat(), "machine", &mid);
            said.push(format!("@pair {addr} kept={} (was {was}:{})", p[0], 3400 + index));
        } else if let Some(i) = i {
            let mut p = split_keep(&lines[i]);
            p[2] = target.clone();
            p[6] = size.clone();
            lines[i] = set_option(&p.concat(), "machine", &mid);
        } else {
            let next = screens.iter().max().copied().unwrap_or(0) + 1;
            screens.push(next);
            lines.push(set_option(&format!("{name:<15} {target:<27} {next:<7} {size}"), "machine", &mid));
        }
    }
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(conf);
    }
    let tmp = conf.join("viewers.conf.tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &conf_path).map_err(|e| e.to_string())?;
    if !restored.is_empty() {
        let left: String = removed.iter().filter(|l| !restored.contains(l)).map(|l| format!("{l}\n")).collect();
        let _ = std::fs::write(&removed_path, left);
    }
    said.push(format!("@paired {host} id={mid} monitors={} cred=per-frame", monitors.len()));
    Ok(said)
}

/// Before unpairing: if an unpair is still pending (the host wasn't told last time), try it again,
/// so its trusted-hosts entry comes back for the agent call (like cc-home's machine unpair).
pub fn unpair_prepare(conf: &Path, machine: &str) {
    let (th, pend) = (conf.join("trusted-hosts").join(format!("{machine}.json")), conf.join("pending-unpair").join(format!("{machine}.json")));
    if pend.exists() && !th.exists() {
        let _ = std::fs::rename(&pend, &th);
    }
}

/// After the host was (or wasn't) told: this Frame forgets the machine's login and pin. If the host
/// wasn't told, the entry stays as pending-unpair (S9: "the host still trusts this Frame"). Returns
/// whether there was anything to forget.
pub fn unpair_finish(conf: &Path, machine: &str, told: bool) -> bool {
    let th = conf.join("trusted-hosts").join(format!("{machine}.json"));
    let pend = conf.join("pending-unpair").join(format!("{machine}.json"));
    let text = std::fs::read_to_string(&th).ok();
    let had_pin = text.as_deref().and_then(|t| serde_json::from_str::<Value>(t).ok()).is_some_and(|v| v["host_pk"].is_string());
    if let (Some(t), false, true) = (&text, told, had_pin) {
        let _ = write_secret(&pend, t);
    }
    let mut gone = false;
    for f in [th, conf.join("passwords").join(machine)] {
        gone |= std::fs::remove_file(f).is_ok();
    }
    if told {
        let _ = std::fs::remove_file(&pend);
    }
    gone
}

/// JSON the way Python's json module keeps it: objects in file order (a key set again stays where it
/// was), and numbers written back the way Python writes them. cc-home's json.dump(indent=2) rewrites
/// home.json byte for byte, so either side can write it and a diff shows only what changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(String), // Python's text for it: an int's digits, a float's repr
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

static NULL: Json = Json::Null;

/// Python's repr of a float (what json.dump uses): shortest round trip, with an exponent below 1e-4
/// and from 1e16 up.
pub fn py_float(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let e = format!("{x:e}");
    let (m, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let f = format!("{x}");
        if f.contains('.') { f } else { f + ".0" }
    } else {
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Python's round(x, n): correctly rounded, ties to even, same as formatting.
pub fn py_round(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

impl Json {
    pub fn float(x: f64) -> Json {
        Json::Num(py_float(x))
    }
    pub fn obj() -> Json {
        Json::Obj(Vec::new())
    }
    /// json.loads (None if it isn't JSON). It's lenient where Python is strict (control characters,
    /// "1."), and a lone surrogate becomes U+FFFD.
    pub fn parse(s: &str) -> Option<Json> {
        let mut p = Parser { s: s.as_bytes(), i: 0 };
        let v = p.value()?;
        p.ws();
        (p.i == p.s.len()).then_some(v)
    }
    /// json.dump(indent=2) (ensure_ascii), with no newline at the end.
    pub fn dump(&self) -> String {
        let mut o = String::new();
        self.dump_to(&mut o, 0);
        o
    }
    /// json.dumps (default separators, ensure_ascii), all on one line.
    pub fn dumps(&self) -> String {
        match self {
            Json::Arr(a) => format!("[{}]", a.iter().map(Json::dumps).collect::<Vec<_>>().join(", ")),
            Json::Obj(m) => format!("{{{}}}", m.iter().map(|(k, v)| format!("{}: {}", py_str(k), v.dumps())).collect::<Vec<_>>().join(", ")),
            _ => self.dump(),
        }
    }
    /// Converts serde_json's value to this (its objects stay in serde's order).
    pub fn from_serde(v: &Value) -> Json {
        match v {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Number(n) if n.is_f64() => Json::float(n.as_f64().unwrap_or(0.0)),
            Value::Number(n) => Json::Num(n.to_string()),
            Value::String(s) => Json::Str(s.clone()),
            Value::Array(a) => Json::Arr(a.iter().map(Json::from_serde).collect()),
            Value::Object(m) => Json::Obj(m.iter().map(|(k, v)| (k.clone(), Json::from_serde(v))).collect()),
        }
    }
    fn dump_to(&self, o: &mut String, level: usize) {
        let (open, close, n) = match self {
            Json::Null => return o.push_str("null"),
            Json::Bool(b) => return o.push_str(if *b { "true" } else { "false" }),
            Json::Num(t) => return o.push_str(t),
            Json::Str(s) => return o.push_str(&py_str(s)),
            Json::Arr(a) => ('[', ']', a.len()),
            Json::Obj(m) => ('{', '}', m.len()),
        };
        o.push(open);
        for i in 0..n {
            o.push_str(if i > 0 { ",\n" } else { "\n" });
            o.push_str(&"  ".repeat(level + 1));
            match self {
                Json::Arr(a) => a[i].dump_to(o, level + 1),
                Json::Obj(m) => {
                    o.push_str(&py_str(&m[i].0));
                    o.push_str(": ");
                    m[i].1.dump_to(o, level + 1);
                }
                _ => {}
            }
        }
        if n > 0 {
            o.push('\n');
            o.push_str(&"  ".repeat(level));
        }
        o.push(close);
    }
    pub fn get(&self, k: &str) -> Option<&Json> {
        self.items().iter().find(|(a, _)| a == k).map(|(_, v)| v)
    }
    /// get, or null, so it chains (data.at("workspaces").at(name)).
    pub fn at(&self, k: &str) -> &Json {
        self.get(k).unwrap_or(&NULL)
    }
    pub fn get_mut(&mut self, k: &str) -> Option<&mut Json> {
        self.items_mut().iter_mut().find(|(a, _)| a == k).map(|(_, v)| v)
    }
    /// d[k] = v: in place when k is there, otherwise at the end. A non-object becomes one.
    pub fn set(&mut self, k: &str, v: Json) {
        match self.get_mut(k) {
            Some(x) => *x = v,
            None => self.items_mut().push((k.into(), v)),
        }
    }
    /// d.setdefault(k, v)
    pub fn setdefault(&mut self, k: &str, v: Json) -> &mut Json {
        if self.get(k).is_none() {
            self.set(k, v);
        }
        self.get_mut(k).unwrap()
    }
    /// d.pop(k, None)
    pub fn remove(&mut self, k: &str) -> Option<Json> {
        let m = self.items_mut();
        m.iter().position(|(a, _)| a == k).map(|i| m.remove(i).1)
    }
    /// An object's entries (nothing for anything else).
    pub fn items(&self) -> &[(String, Json)] {
        match self {
            Json::Obj(m) => m,
            _ => &[],
        }
    }
    pub fn items_mut(&mut self) -> &mut Vec<(String, Json)> {
        if !matches!(self, Json::Obj(_)) {
            *self = Json::obj();
        }
        match self {
            Json::Obj(m) => m,
            _ => unreachable!(),
        }
    }
    pub fn list(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => &[],
        }
    }
    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(t) => t.parse().ok(),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    /// Python's bool(): empty, zero and null are false.
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Num(_) => self.num() != Some(0.0),
            Json::Str(s) => !s.is_empty(),
            Json::Arr(a) => !a.is_empty(),
            Json::Obj(m) => !m.is_empty(),
        }
    }
    /// How Python's f"{x}" shows a str, number, bool or None.
    pub fn text(&self) -> String {
        match self {
            Json::Str(s) => s.clone(),
            Json::Null => "None".into(),
            Json::Bool(b) => if *b { "True" } else { "False" }.into(),
            _ => self.dump(),
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| b" \t\n\r".contains(c)) {
            self.i += 1;
        }
    }
    fn eat(&mut self, t: &str) -> bool {
        let hit = self.s[self.i..].starts_with(t.as_bytes());
        if hit {
            self.i += t.len();
        }
        hit
    }
    fn value(&mut self) -> Option<Json> {
        self.ws();
        let (open, close) = match *self.s.get(self.i)? {
            b'"' => return self.string().map(Json::Str),
            b'{' => (b'{', "}"),
            b'[' => (b'[', "]"),
            _ => {
                for (t, v) in [("null", Json::Null), ("true", Json::Bool(true)), ("false", Json::Bool(false)), ("NaN", Json::Num("NaN".into())),
                               ("Infinity", Json::Num("Infinity".into())), ("-Infinity", Json::Num("-Infinity".into()))] {
                    if self.eat(t) {
                        return Some(v);
                    }
                }
                return self.number();
            }
        };
        self.i += 1;
        let mut out = if open == b'{' { Json::obj() } else { Json::Arr(Vec::new()) };
        self.ws();
        if self.eat(close) {
            return Some(out);
        }
        loop {
            if let Json::Arr(a) = &mut out {
                a.push(self.value()?);
            } else {
                self.ws();
                let k = self.string()?;
                self.ws();
                if !self.eat(":") {
                    return None;
                }
                let v = self.value()?;
                out.set(&k, v);
            }
            self.ws();
            if self.eat(close) {
                return Some(out);
            }
            if !self.eat(",") {
                return None;
            }
        }
    }
    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c)) {
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        if t.contains(['.', 'e', 'E']) {
            t.parse::<f64>().ok().map(Json::float)
        } else {
            t.parse::<i128>().ok().map(|n| Json::Num(n.to_string()))
        }
    }
    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok().filter(|_| h.bytes().all(|c| c.is_ascii_hexdigit()))
    }
    fn string(&mut self) -> Option<String> {
        if !self.eat("\"") {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' | b'\\' | b'/' => e as char,
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.s[self.i..].starts_with(b"\\u") {
                                let back = self.i;
                                self.i += 2;
                                match self.hex4()? {
                                    lo @ 0xdc00..0xe000 => u = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00),
                                    _ => self.i = back,
                                }
                            }
                            char::from_u32(u).unwrap_or('\u{fffd}')
                        }
                        _ => return None,
                    };
                    out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

/// home.json the way cc-home's read_all reads it: {"workspace": active, "workspaces": {name: {"spots",
/// "universe", "primary", "known_networks", "machines"}}}. An older file with only "spots" becomes
/// the workspace "default", and a missing or unreadable one (a fresh install) becomes an empty
/// Temporary. Err means it's JSON but not an object. Python stopped there too, so nothing overwrites it.
pub fn read_home(path: &Path) -> Result<Json, String> {
    let data = std::fs::read_to_string(path).ok().and_then(|t| Json::parse(&t)).unwrap_or_else(Json::obj);
    if !matches!(data, Json::Obj(_)) {
        return Err(format!("{} isn't a JSON object", path.display()));
    }
    if matches!(data.get("workspaces"), Some(Json::Obj(_))) {
        return Ok(data);
    }
    let name = if data.get("spots").is_some() { "default" } else { TEMPORARY };
    let spots = data.get("spots").cloned().unwrap_or_else(Json::obj);
    let mut out = Json::obj();
    out.set("workspace", Json::Str(name.into()));
    out.setdefault("workspaces", Json::obj()).setdefault(name, Json::obj()).set("spots", spots);
    Ok(out)
}

/// cc-home's write_all: makes the folder (0700) on a fresh Frame, then renames a temporary file over.
pub fn write_home(path: &Path, data: &Json) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(d) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, data.dump())?;
    std::fs::rename(&tmp, path)
}

/// One remote monitor in viewers.conf, the way cc-home's viewers() reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewer {
    pub name: String,
    pub target: String,              // user@host:port
    pub screen: i64,                 // 1-based (the file's column + 1)
    pub pixels: (i64, i64),
    pub opts: Vec<(String, String)>, // key=value after the size; a repeated key: the last
    pub machine: String,             // machine=, else the host
    pub label: Option<String>,       // label=, %-decoded
}

impl Viewer {
    pub fn opt(&self, k: &str) -> Option<&str> {
        self.opts.iter().rev().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

/// urllib.parse.unquote: %XX as UTF-8 bytes (a bad sequence becomes U+FFFD).
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).filter(|h| b[i] == b'%' && h.iter().all(u8::is_ascii_hexdigit));
        match hex {
            Some(h) => {
                out.push(u8::from_str_radix(std::str::from_utf8(h).unwrap_or("0"), 16).unwrap_or(0));
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// viewers.conf's monitors in file order (a repeated name replaces the earlier one in its place).
/// A line whose screen or size isn't a number gets skipped (cc-home's Python stops with a traceback).
pub fn viewers(conf: &Path) -> Vec<Viewer> {
    let mut out: Vec<Viewer> = Vec::new();
    for l in std::fs::read_to_string(conf.join("viewers.conf")).unwrap_or_default().lines() {
        let p = words(l);
        if p.len() < 4 || p[0].starts_with('#') {
            continue;
        }
        let size: Vec<Option<i64>> = p[3].split('x').map(|n| n.parse().ok()).collect();
        let (Ok(screen), [Some(w), Some(h)]) = (p[2].parse::<i64>(), size.as_slice()) else { continue };
        let opts: Vec<(String, String)> = p[4..].iter().filter_map(|o| o.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
        let mut v = Viewer { name: p[0].into(), target: p[1].into(), screen: screen + 1, pixels: (*w, *h), opts, machine: String::new(), label: None };
        v.machine = v.opt("machine").filter(|m| !m.is_empty()).map(str::to_owned)
            .unwrap_or_else(|| p[1].rsplit('@').next().unwrap_or("").rsplit_once(':').map_or(p[1].rsplit('@').next().unwrap_or(""), |x| x.0).to_owned());
        v.label = Some(unquote(v.opt("label").unwrap_or(""))).filter(|s| !s.is_empty());
        match out.iter_mut().find(|x| x.name == v.name) {
            Some(x) => *x = v,
            None => out.push(v),
        }
    }
    out
}

/// What the user sees for a monitor (D-050, cc-home's display): its own label if it has one, else its
/// machine's label (plus the output when that machine has more than one monitor), else its name.
pub fn display(v: &Viewer, hosts: &[(String, Value)], all: &[Viewer]) -> String {
    if let Some(l) = &v.label {
        return l.clone();
    }
    let t = hosts.iter().find(|(k, _)| *k == v.machine).map(|(_, t)| t);
    let Some(label) = t.and_then(|t| t["label"].as_str()).filter(|s| !s.is_empty()) else { return v.name.clone() };
    if all.iter().filter(|x| x.machine == v.machine).count() < 2 {
        return label.into();
    }
    let index = (v.target.rsplit_once(':').and_then(|x| x.1.parse::<i64>().ok()).unwrap_or(0) - 3400).rem_euclid(10);
    let output = t.and_then(|t| t["monitors"][index.to_string()].as_str()).unwrap_or(&v.name);
    format!("{label} {output}")
}

/// A monitor by its name, or by what the user sees (any case), as long as just one matches.
pub fn find_viewer<'a>(x: &str, vs: &'a [Viewer], hosts: &[(String, Value)]) -> Option<&'a Viewer> {
    if let Some(v) = vs.iter().find(|v| v.name == x) {
        return Some(v);
    }
    let hit: Vec<&Viewer> = vs.iter().filter(|v| display(v, hosts, vs).to_lowercase() == x.to_lowercase()).collect();
    (hit.len() == 1).then(|| hit[0])
}

/// A machine (its id, or a pre-id host's name), found by id, host name, label or one of its monitors.
pub fn find_machine(x: &str, vs: &[Viewer], hosts: &[(String, Value)]) -> Option<String> {
    if vs.iter().any(|v| v.machine == x) || hosts.iter().any(|(k, _)| k == x) {
        return Some(x.into());
    }
    if let Some(v) = find_viewer(x, vs, hosts) {
        return Some(v.machine.clone());
    }
    let x = x.to_lowercase();
    let field = |t: &Value, k: &str| t[k].as_str().unwrap_or("").to_lowercase();
    let hit: Vec<&String> = hosts.iter().filter(|(_, t)| field(t, "label") == x || field(t, "host") == x).map(|(k, _)| k).collect();
    (hit.len() == 1).then(|| hit[0].clone())
}

// ---- workspaces (docs/workspaces.md)
//
// A workspace is one place's layout (spots) and machines, kept in home.json's "workspaces". The
// dedicated ones are named by the user (or were made before Temporary existed: "default" and
// "workspace-N" stay as they are), and they're bound to the room (SteamVR universe) they were made in.
// Temporary (key TEMPORARY) is the travel one, and it's never bound to a room:
// - Until the first dedicated workspace exists, everything saves there (a fresh install starts in it).
// - A room no dedicated workspace is bound to (travelling) uses it, instead of making a
//   workspace-N for every new room. It keeps its own layout from trip to trip (moving panels saves
//   there), and if it's missing it gets made from the active one's layout.
// - Loading a saved workspace abroad (load_workspace) puts that layout in Temporary, moved as one
//   rigid piece in front of you. The saved one, its spots and its room never get touched, so its
//   own room brings it back the way it was.
// A workspace's machines are its "machines" list (machine ids: viewers.conf's machine=, else the
// host). No list means every machine, so existing workspaces and Temporary behave like before.

/// The travel workspace's key in home.json (the UI calls it "Temporary").
pub const TEMPORARY: &str = "temporary";

/// The UI's name for workspace `name`.
pub fn workspace_title(name: &str) -> &str {
    if name == TEMPORARY { "Temporary" } else { name }
}

/// The room SteamVR tracks. 0 means none, and so do the Frame's stand-ins while it has lost its world
/// ("Switch to the dummy chaperone universe", 424242; 525252 in its worlds list).
pub fn room(universe: u64) -> u64 {
    if matches!(universe, 424242 | 525252) { 0 } else { universe }
}

pub fn active_workspace(data: &Json) -> String {
    data.at("workspace").str().unwrap_or("default").to_owned()
}

fn has_workspace(data: &Json, n: &str) -> bool {
    matches!(data.at("workspaces").get(n), Some(Json::Obj(_)))
}

/// A new Temporary from workspace `from`'s layout and machines (empty if there's none).
fn make_temporary(data: &mut Json, from: &str) {
    let src = data.at("workspaces").at(from).clone();
    let mut t = Json::obj();
    t.set("spots", src.get("spots").cloned().unwrap_or_else(Json::obj));
    if let Some(m) = src.get("machines") {
        t.set("machines", m.clone());
    }
    data.setdefault("workspaces", Json::obj()).set(TEMPORARY, t);
}

/// Picks the workspace for the room SteamVR tracks (`universe`; `hint` is the network's, from
/// cc-home workspace-for) and makes it active:
/// 1. A dedicated workspace bound to this room: the active one if it is, else the first.
/// 2. No room known (0, or a dummy universe): the hint's, else the active one.
/// 3. A new room while the active one is dedicated and bound to none (`cc-home workspace new`):
///    that one, bound to this room.
/// 4. Otherwise (travelling, or no dedicated workspace yet): Temporary, made from the active one's
///    layout if it's missing.
/// Returns its name and whether Temporary was just made.
pub fn choose_workspace(data: &mut Json, universe: u64, hint: Option<&str>) -> (String, bool) {
    let (universe, active) = (room(universe), active_workspace(data));
    let id = universe.to_string();
    let ws = data.at("workspaces");
    let here = |n: &str| universe != 0 && n != TEMPORARY && ws.at(n).at("universe").str() == Some(&id);
    let found = if here(&active) { Some(active.clone()) } else { ws.items().iter().map(|(n, _)| n.clone()).find(|n| here(n)) };
    let (name, made) = match found {
        Some(n) => (n, false),
        None if universe == 0 => {
            let pick = hint.filter(|h| has_workspace(data, h)).map(str::to_owned);
            match pick.or_else(|| has_workspace(data, &active).then(|| active.clone())) {
                Some(n) => (n, false),
                None => (TEMPORARY.into(), !has_workspace(data, TEMPORARY)),
            }
        }
        None if active != TEMPORARY && has_workspace(data, &active) && !ws.at(&active).at("universe").truthy() => {
            data.setdefault("workspaces", Json::obj()).setdefault(&active, Json::obj()).set("universe", Json::Str(id));
            (active.clone(), false)
        }
        None => (TEMPORARY.into(), !has_workspace(data, TEMPORARY)),
    };
    if made {
        make_temporary(data, &active);
    }
    data.set("workspace", Json::Str(name.clone()));
    (name, made)
}

/// Whether a new (or renamed) dedicated workspace can take this name.
pub fn check_workspace_name(data: &Json, name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name != name.trim() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        return Err("a workspace's name: 1 to 40 characters, no spaces at its ends".into());
    }
    if name.eq_ignore_ascii_case(TEMPORARY) {
        return Err("Temporary is the travel workspace's name".into());
    }
    if has_workspace(data, name) {
        return Err(format!("workspace {name} exists"));
    }
    Ok(())
}

/// Makes a new dedicated workspace from the active one's layout and machines, bound to this room
/// (`universe`; with none, it gets bound in the first new room it starts in, rule 3), and makes it active.
pub fn save_workspace_as(data: &mut Json, name: &str, universe: u64) -> Result<(), String> {
    check_workspace_name(data, name)?;
    let src = data.at("workspaces").at(&active_workspace(data)).clone();
    let mut w = Json::obj();
    w.set("spots", src.get("spots").cloned().unwrap_or_else(Json::obj));
    if let Some(m) = src.get("machines") {
        w.set("machines", m.clone());
    }
    if room(universe) != 0 {
        w.set("universe", Json::Str(room(universe).to_string()));
    }
    data.setdefault("workspaces", Json::obj()).set(name, w);
    data.set("workspace", Json::Str(name.into()));
    Ok(())
}

/// Makes workspace `name` the active one.
pub fn use_workspace(data: &mut Json, name: &str) -> Result<(), String> {
    if !has_workspace(data, name) {
        return Err(format!("no workspace {name} (cc-home workspace)"));
    }
    data.set("workspace", Json::Str(name.into()));
    Ok(())
}

/// Renames a dedicated workspace. Temporary can't be renamed, save it as one instead.
pub fn rename_workspace(data: &mut Json, old: &str, new: &str) -> Result<(), String> {
    if !has_workspace(data, old) || has_workspace(data, new) {
        return Err(format!("can't rename {old} to {new}"));
    }
    if old == TEMPORARY {
        return Err("Temporary keeps its name: save it as a workspace instead".into());
    }
    check_workspace_name(data, new)?;
    let ws = data.setdefault("workspaces", Json::obj());
    let w = ws.remove(old).unwrap_or_else(Json::obj);
    ws.set(new, w); // (last, as cc-home always wrote it)
    if active_workspace(data) == old {
        data.set("workspace", Json::Str(new.into()));
    }
    Ok(())
}

/// Deletes a workspace. Never the active one, so switch first.
pub fn forget_workspace(data: &mut Json, name: &str) -> Result<(), String> {
    if name == active_workspace(data) || !has_workspace(data, name) {
        return Err(format!("can't forget {} (it's active, or there's no such workspace)", workspace_title(name)));
    }
    data.setdefault("workspaces", Json::obj()).remove(name);
    Ok(())
}

/// Workspace `name`'s machines. None means all of them.
pub fn workspace_machines(data: &Json, name: &str) -> Option<Vec<String>> {
    let m = data.at("workspaces").at(name).get("machines")?;
    Some(m.list().iter().filter_map(|x| x.str().map(str::to_owned)).collect())
}

/// Whether `machine` is one of workspace `name`'s.
pub fn is_member(data: &Json, name: &str, machine: &str) -> bool {
    workspace_machines(data, name).is_none_or(|m| m.iter().any(|x| x == machine))
}

/// Puts `machine` into (`on`) or takes it out of workspace `name`. `all` is every machine right now:
/// the list the first edit of an "all of them" workspace starts from.
pub fn set_member(data: &mut Json, name: &str, machine: &str, on: bool, all: &[String]) -> Result<(), String> {
    if !has_workspace(data, name) {
        return Err(format!("no workspace {name}"));
    }
    let mut list = workspace_machines(data, name).unwrap_or_else(|| all.to_vec());
    list.dedup();
    list.retain(|m| m != machine);
    if on {
        list.push(machine.into());
    }
    let w = data.setdefault("workspaces", Json::obj()).setdefault(name, Json::obj());
    w.set("machines", Json::Arr(list.into_iter().map(Json::Str).collect()));
    Ok(())
}

/// How far ahead of you a loaded layout's anchor goes, horizontally (calibration knob).
pub const LOAD_AHEAD: f64 = 1.0;

fn centre_of(p: &Json) -> Option<[f64; 3]> {
    match p.at("centre").list() {
        [a, b, c] => Some([a.num()?, b.num()?, c.num()?]),
        _ => None,
    }
}

/// `pose` moved as one rigid piece with `from` (centre, yaw) onto `to`: turned about the vertical
/// through from's centre by the yaw change, then from's centre moved onto to's. Pitch, roll, size and
/// curve stay the same, since gravity is the same in every room. This is cc-home reanchor's move.
pub fn rigid(pose: &Json, from: ([f64; 3], f64), to: ([f64; 3], f64)) -> Json {
    let Some(p) = centre_of(pose) else { return pose.clone() };
    let turn = (to.1 - from.1).to_radians();
    let (c, s) = (turn.cos(), turn.sin());
    let (x, y, z) = (p[0] - from.0[0], p[1] - from.0[1], p[2] - from.0[2]);
    let at = [to.0[0] + x * c + z * s, to.0[1] + y, to.0[2] - x * s + z * c];
    let mut out = pose.clone();
    out.set("centre", Json::Arr(at.iter().map(|v| Json::float(py_round(*v, 4))).collect()));
    let yaw = (pose.at("yaw").num().unwrap_or(0.0) + to.1 - from.1 + 180.0).rem_euclid(360.0) - 180.0;
    out.set("yaw", Json::float(py_round(yaw, 3)));
    out
}

/// The anchor of a layout (spots' "home"): the first of `anchor_keys` (its primary machine's
/// monitors) that's placed there, else the middle of everything placed and their mean heading.
pub fn layout_anchor(home: &Json, anchor_keys: &[String]) -> Option<([f64; 3], f64)> {
    let pose = |p: &Json| Some((centre_of(p)?, p.at("yaw").num()?));
    if let Some(a) = anchor_keys.iter().find_map(|k| home.get(k).and_then(pose)) {
        return Some(a);
    }
    let all: Vec<([f64; 3], f64)> = home.items().iter().filter_map(|(_, p)| pose(p)).collect();
    if all.is_empty() {
        return None;
    }
    let n = all.len() as f64;
    let c = [0, 1, 2].map(|i| all.iter().map(|a| a.0[i]).sum::<f64>() / n);
    let (s, co) = all.iter().fold((0.0, 0.0), |(s, c), a| (s + a.1.to_radians().sin(), c + a.1.to_radians().cos()));
    Some((c, s.atan2(co).to_degrees()))
}

/// Loads workspace `name`'s layout where you are (travelling). Its spots, except "scanned" and
/// "before-align" (those are its room's real monitors), get moved as one rigid piece so its anchor
/// (layout_anchor) stands LOAD_AHEAD in front of you, at its own height above the floor, facing you.
/// `head` is the head position and `head_yaw` its facing, in cc-panels' yaw (0 looks down -z). The
/// result goes into Temporary along with its machines, and Temporary becomes active.
/// `name` itself never changes. Returns how many panels its home layout placed.
pub fn load_workspace(data: &mut Json, name: &str, head: [f64; 3], head_yaw: f64, anchor_keys: &[String]) -> Result<usize, String> {
    if !has_workspace(data, name) {
        return Err(format!("no workspace {name} (cc-home workspace)"));
    }
    let src = data.at("workspaces").at(name).clone();
    let spots = src.at("spots");
    let from = layout_anchor(spots.at("home"), anchor_keys).ok_or_else(|| format!("{} has no layout to load", workspace_title(name)))?;
    let y = head_yaw.to_radians();
    let to = ([head[0] - y.sin() * LOAD_AHEAD, from.0[1], head[2] - y.cos() * LOAD_AHEAD], head_yaw);
    let mut moved = Json::obj();
    for (spot, poses) in spots.items().iter().filter(|(k, _)| k != "scanned" && k != "before-align") {
        moved.set(spot, Json::Obj(poses.items().iter().map(|(k, p)| (k.clone(), rigid(p, from, to))).collect()));
    }
    let n = moved.at("home").items().len();
    let mut t = Json::obj();
    t.set("spots", moved);
    if let Some(m) = src.get("machines") {
        t.set("machines", m.clone());
    }
    t.set("loaded", Json::Str(name.into()));
    data.setdefault("workspaces", Json::obj()).set(TEMPORARY, t);
    data.set("workspace", Json::Str(TEMPORARY.into()));
    Ok(n)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_option_as_cc_home() {
        assert_eq!(set_option("a   u@h:3400   1   1920x1080", "machine", "m1"), "a   u@h:3400   1   1920x1080  machine=m1");
        assert_eq!(set_option("a   u@h:3400   1   1920x1080  machine=m0  curve=h", "machine", "m1"), "a   u@h:3400   1   1920x1080  machine=m1  curve=h");
        assert_eq!(py_str("é\"x\n"), "\"\\u00e9\\\"x\\n\"");
    }

    #[test]
    fn unpair_keeps_a_pending_entry_until_the_host_is_told() {
        let d = std::env::temp_dir().join(format!("cc-conf-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        write_secret(&d.join("trusted-hosts/m.json"), r#"{"id": "m", "addr": "10.0.0.2", "host_pk": "PK"}"#).unwrap();
        write_secret(&d.join("passwords/m"), "pw").unwrap();
        assert!(unpair_finish(&d, "m", false));
        assert!(d.join("pending-unpair/m.json").exists() && !d.join("trusted-hosts/m.json").exists() && !d.join("passwords/m").exists());
        unpair_prepare(&d, "m"); // a retry: the entry back for the agent call
        assert!(d.join("trusted-hosts/m.json").exists() && !d.join("pending-unpair/m.json").exists());
        assert!(unpair_finish(&d, "m", true));
        assert!(!d.join("pending-unpair/m.json").exists() && !d.join("trusted-hosts/m.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn json_as_python_writes_it() {
        for (x, py) in [(1e16, "1e+16"), (1e15, "1000000000000000.0"), (1e-5, "1e-05"), (0.0001, "0.0001"), (-0.0, "-0.0"), (0.1 + 0.2, "0.30000000000000004"), (1.5e300, "1.5e+300")] {
            assert_eq!(py_float(x), py);
        }
        assert_eq!(py_round(2.675, 2), 2.67);
        let t = "{\"b\": [], \"a\": {\"x\": 1, \"\u{e9}\": [1.0, null, true, \"\\ud83d\\ude00\"]}, \"b\": {}}";
        let j = Json::parse(t).unwrap();
        assert_eq!(j.dump(), "{\n  \"b\": {},\n  \"a\": {\n    \"x\": 1,\n    \"\\u00e9\": [\n      1.0,\n      null,\n      true,\n      \"\\ud83d\\ude00\"\n    ]\n  }\n}");
        assert_eq!(Json::parse("[1, 2"), None);
    }

    #[test]
    fn set_options_as_cc_home() {
        let o = |kv: &[&str]| kv.iter().map(|a| a.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).unwrap()).collect::<Vec<_>>();
        let l = "a   u@h:3410   1   1920x1080  curve=h";
        assert_eq!(set_options(l, &o(&["label=Desk é/1", "curve="])).unwrap(), "a   u@h:3410   1   1920x1080  label=Desk%20%C3%A9%2F1");
        assert_eq!(set_options(l, &o(&["radius=1.5", "ssh=user@10.0.0.2", "machine=m.1"])).unwrap(),
                   "a   u@h:3410   1   1920x1080  curve=h  radius=1.5  ssh=user@10.0.0.2  machine=m.1");
        for (kv, e) in [("curve=x", "curve is one of h, v, flat"), ("autoconnect=1", "autoconnect is one of yes, no"), ("radius=0.1", "radius is in metres, 0.2 to 20 (a 1000R monitor: 1.0)"),
                        ("radius=1.", "radius is in metres, 0.2 to 20 (a 1000R monitor: 1.0)"), ("ssh=Root@h", "ssh is user@host"), ("machine=a b", "bad machine name"),
                        ("label=a\tb", "label: at most 64 characters, no control characters")] {
            assert_eq!(set_options(l, &o(&[kv])).unwrap_err(), e);
        }
        assert_eq!(set_options(l, &o(&[&format!("label={}", "x".repeat(65))])).unwrap_err(), "label: at most 64 characters, no control characters");
    }

    #[test]
    fn dumps_as_python() {
        let j = Json::parse("{\"b\": [1, 2.50, {}], \"a\": \"\u{e9}\", \"c\": []}").unwrap();
        assert_eq!(j.dumps(), "{\"b\": [1, 2.5, {}], \"a\": \"\\u00e9\", \"c\": []}");
        assert_eq!(Json::from_serde(&serde_json::json!({"x": [1, 1.0, null, true]})).dumps(), "{\"x\": [1, 1.0, null, true]}");
    }

    #[test]
    fn viewers_as_cc_home_reads_them() {
        let d = std::env::temp_dir().join(format!("cc-conf-viewers-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("viewers.conf"), "# name user@host:port screen size\na  u@10.0.0.2:3411  1  1920x1080  label=Desk%20%C3%A9  curve=v\nb u@10.0.0.2:3410 2 bad\nc  u@h:3400 0 800x600 machine=  autoconnect=yes\n").unwrap();
        let v = viewers(&d);
        assert_eq!(v.iter().map(|x| (x.name.as_str(), x.screen, x.machine.as_str())).collect::<Vec<_>>(), [("a", 2, "10.0.0.2"), ("c", 1, "h")]);
        assert_eq!(v[0].label.as_deref(), Some("Desk é"));
        assert_eq!(v[1].opt("autoconnect"), Some("yes"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn home(text: &str) -> Json {
        Json::parse(text).unwrap()
    }

    /// My setup: two dedicated workspaces bound to rooms, workspace-2 active.
    const TWO: &str = r#"{"workspace": "workspace-2", "workspaces": {
        "home": {"spots": {"home": {"desk-wide": {"centre": [0.0, 1.2, -1.0], "yaw": 0.0, "pitch": 0.0, "roll": 0.0, "width": 1.2}}}, "universe": "111"},
        "workspace-2": {"known_networks": ["Home"], "spots": {"home": {"desk-wide": {"centre": [1.0, 1.3, -2.0], "yaw": 30.0, "pitch": -5.0, "roll": 0.0, "width": 1.5}}}, "universe": "222"}}}"#;

    #[test]
    fn a_fresh_install_starts_in_temporary_and_stays_there() {
        let d = std::env::temp_dir().join(format!("cc-ws-fresh-{}", std::process::id()));
        let mut data = read_home(&d.join("home.json")).unwrap();
        assert_eq!(active_workspace(&data), TEMPORARY);
        assert_eq!(choose_workspace(&mut data, 111, None), (TEMPORARY.into(), false));
        assert_eq!(choose_workspace(&mut data, 222, None), (TEMPORARY.into(), false), "another room: still Temporary, never bound");
        assert!(!data.at("workspaces").at(TEMPORARY).at("universe").truthy());
        assert_eq!(data.at("workspaces").items().len(), 1, "no workspace-N");
        // an older file with only spots is the user's: "default", dedicated
        let old = d.join("old.json");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&old, r#"{"spots": {"home": {}}}"#).unwrap();
        let mut data = read_home(&old).unwrap();
        assert_eq!(choose_workspace(&mut data, 111, None), ("default".into(), false));
        assert_eq!(data.at("workspaces").at("default").at("universe").str(), Some("111"), "bound in its first room, as before");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_known_room_picks_its_workspace_unchanged() {
        let mut data = home(TWO);
        let before = data.at("workspaces").dumps();
        assert_eq!(choose_workspace(&mut data, 111, None), ("home".into(), false));
        assert_eq!(choose_workspace(&mut data, 222, Some("home")), ("workspace-2".into(), false), "the room wins over the network");
        assert_eq!(data.at("workspaces").dumps(), before, "nothing in them changed");
    }

    #[test]
    fn an_unknown_room_uses_temporary_not_a_new_workspace() {
        let mut data = home(TWO);
        assert_eq!(choose_workspace(&mut data, 333, None), (TEMPORARY.into(), true));
        let t = data.at("workspaces").at(TEMPORARY);
        assert_eq!(t.at("spots").dumps(), home(TWO).at("workspaces").at("workspace-2").at("spots").dumps(), "made from the last-used layout");
        assert!(!t.at("universe").truthy() && data.at("workspaces").get("workspace-3").is_none());
        // from then on it keeps its own layout: another unknown room, and back
        data.setdefault("workspaces", Json::obj()).setdefault(TEMPORARY, Json::obj()).set("spots", home(r#"{"home": {}}"#));
        assert_eq!(choose_workspace(&mut data, 444, None), (TEMPORARY.into(), false));
        assert_eq!(data.at("workspaces").at(TEMPORARY).at("spots").dumps(), r#"{"home": {}}"#);
        assert_eq!(choose_workspace(&mut data, 111, None), ("home".into(), false), "home again: its own");
        assert_eq!(choose_workspace(&mut data, 555, None), (TEMPORARY.into(), false));
        assert_eq!(data.at("workspaces").at(TEMPORARY).at("spots").dumps(), r#"{"home": {}}"#, "kept, not copied again");
    }

    #[test]
    fn a_dedicated_unbound_workspace_is_bound_in_its_first_room() {
        let mut data = home(TWO);
        save_workspace_as(&mut data, "office", 0).unwrap(); // cc-home workspace new, cc-panels not running
        assert_eq!(choose_workspace(&mut data, 333, None), ("office".into(), false));
        assert_eq!(data.at("workspaces").at("office").at("universe").str(), Some("333"));
        save_workspace_as(&mut data, "desk", 444).unwrap();
        assert_eq!(data.at("workspaces").at("desk").at("universe").str(), Some("444"), "Save as binds to this room");
    }

    #[test]
    fn no_room_or_the_dummy_universe_keeps_the_active_one_or_the_networks() {
        for u in [0, 424242, 525252] {
            let mut data = home(TWO);
            assert_eq!(choose_workspace(&mut data, u, None), ("workspace-2".into(), false));
            assert_eq!(choose_workspace(&mut data, u, Some("home")), ("home".into(), false), "the network's hint");
            assert_eq!(choose_workspace(&mut data, u, Some("nowhere")), ("home".into(), false), "an unknown hint: the active one");
            assert!(data.at("workspaces").get(TEMPORARY).is_none(), "no room: nothing made");
        }
    }

    #[test]
    fn names_rename_and_forget() {
        let mut data = home(TWO);
        assert!(save_workspace_as(&mut data, "Temporary", 0).is_err() && save_workspace_as(&mut data, "home", 0).is_err());
        assert!(save_workspace_as(&mut data, " x", 0).is_err() && save_workspace_as(&mut data, "", 0).is_err());
        rename_workspace(&mut data, "workspace-2", "flat").unwrap();
        assert_eq!(active_workspace(&data), "flat");
        assert_eq!(data.at("workspaces").items().iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["home", "flat"]);
        assert!(forget_workspace(&mut data, "flat").is_err(), "the active one");
        choose_workspace(&mut data, 999, None);
        assert!(rename_workspace(&mut data, TEMPORARY, "x").is_err());
        forget_workspace(&mut data, "flat").unwrap();
        assert!(use_workspace(&mut data, "flat").is_err());
        use_workspace(&mut data, "home").unwrap();
        forget_workspace(&mut data, TEMPORARY).unwrap();
    }

    #[test]
    fn membership_starts_as_all_machines() {
        let mut data = home(TWO);
        assert!(workspace_machines(&data, "home").is_none() && is_member(&data, "home", "anything"));
        let all = ["desk".to_string(), "laptop".to_string()];
        set_member(&mut data, "home", "laptop", false, &all).unwrap();
        assert_eq!(workspace_machines(&data, "home").unwrap(), ["desk"]);
        assert!(!is_member(&data, "home", "laptop") && is_member(&data, "workspace-2", "laptop"), "per workspace");
        set_member(&mut data, "home", "laptop", true, &all).unwrap();
        set_member(&mut data, "home", "laptop", true, &all).unwrap();
        assert_eq!(workspace_machines(&data, "home").unwrap(), ["desk", "laptop"]);
        save_workspace_as(&mut data, "copy", 0).unwrap_or(()); // from the active one (workspace-2: all)
        assert!(workspace_machines(&data, "copy").is_none());
    }

    #[test]
    fn a_rigid_move_keeps_the_layout() {
        let p = |c: [f64; 3], yaw: f64| home(&format!(r#"{{"centre": [{}, {}, {}], "yaw": {yaw}, "pitch": -7.0, "roll": 2.0, "width": 0.8, "curve": 1.5}}"#, c[0], c[1], c[2]));
        let (a, b) = (p([0.0, 1.2, -1.0], 0.0), p([0.9, 1.4, -1.3], -40.0));
        let from = ([0.0, 1.2, -1.0], 0.0);
        let to = ([5.0, 1.2, 3.0], 90.0);
        let (a2, b2) = (rigid(&a, from, to), rigid(&b, from, to));
        let c = |j: &Json| centre_of(j).unwrap();
        assert!(c(&a2).iter().zip([5.0, 1.2, 3.0]).all(|(x, y)| (x - y).abs() < 1e-4), "{a2:?}");
        let dist = |u: [f64; 3], v: [f64; 3]| (0..3).map(|i| (u[i] - v[i]).powi(2)).sum::<f64>().sqrt();
        assert!((dist(c(&a), c(&b)) - dist(c(&a2), c(&b2))).abs() < 1e-3, "distances kept");
        assert!((b2.at("yaw").num().unwrap() - 50.0).abs() < 1e-9, "turned as much as the anchor");
        assert_eq!((b2.at("pitch").num(), b2.at("roll").num(), b2.at("width").num(), b2.at("curve").num()), (Some(-7.0), Some(2.0), Some(0.8), Some(1.5)));
        assert!((c(&b2)[1] - 1.4).abs() < 1e-9, "heights kept");
        // yaw 90 looks down -x: b, 0.9 m right of the anchor and 0.3 m further, goes 0.9 m right of it (-z) and 0.3 m further (-x)
        assert!(dist(c(&b2), [4.7, 1.4, 2.1]) < 1e-3, "{b2:?}");
    }

    #[test]
    fn loading_abroad_puts_the_layout_ahead_and_leaves_home_alone() {
        let mut data = home(TWO);
        choose_workspace(&mut data, 333, None); // travelling
        set_member(&mut data, "home", "desk", true, &[]).unwrap();
        let home_before = data.at("workspaces").at("home").dumps();
        let n = load_workspace(&mut data, "home", [10.0, 1.7, 10.0], 90.0, &[]).unwrap();
        assert_eq!((n, active_workspace(&data)), (1, TEMPORARY.to_string()));
        assert_eq!(data.at("workspaces").at("home").dumps(), home_before, "home untouched, room binding too");
        let t = data.at("workspaces").at(TEMPORARY);
        let p = t.at("spots").at("home").at("desk-wide");
        // facing -x, 1 m ahead of the head, at its own height, facing you
        assert!(centre_of(p).unwrap().iter().zip([9.0, 1.2, 10.0]).all(|(a, b)| (a - b).abs() < 1e-4), "{p:?}");
        assert_eq!(p.at("yaw").num(), Some(90.0));
        assert_eq!(workspace_machines(&data, TEMPORARY).unwrap(), ["desk"], "its machines come with it");
        assert_eq!(t.at("loaded").str(), Some("home"));
        // home's own room brings it back as it was
        assert_eq!(choose_workspace(&mut data, 111, None), ("home".into(), false));
        assert_eq!(data.at("workspaces").at("home").dumps(), home_before);
        // the primary's monitor is the anchor when there is one; scanned stays behind
        let mut data = home(r#"{"workspace": "w", "workspaces": {"w": {"spots": {
            "home": {"a": {"centre": [0.0, 1.0, -1.0], "yaw": 0.0}, "b": {"centre": [1.0, 1.5, -1.0], "yaw": 20.0}},
            "scanned": {"a": {"centre": [0.0, 1.0, -1.0], "yaw": 0.0}}}}}}"#);
        load_workspace(&mut data, "w", [0.0, 1.6, 0.0], 0.0, &["b".into()]).unwrap();
        let s = data.at("workspaces").at(TEMPORARY).at("spots");
        assert!(centre_of(s.at("home").at("b")).unwrap().iter().zip([0.0, 1.5, -1.0]).all(|(a, b)| (a - b).abs() < 1e-4));
        assert_eq!(s.at("home").at("b").at("yaw").num(), Some(0.0));
        assert!(s.get("scanned").is_none());
        assert!(load_workspace(&mut data, "nowhere", [0.0; 3], 0.0, &[]).is_err());
    }
}
