//! SSH is for diagnosing only now (docs/ssh-free.md). With CC_SSH=1, probe user@host, align's
//! tag screens on a machine without an agent, and the agent commands' fallbacks still go over
//! `ssh` the way home/cc-home.py did. Without it, each of those stops and tells you what to do
//! instead. Every ssh or scp command line comes from ssh_argv(), and tests/no-ssh checks the
//! source so nothing else builds one.
use super::die;
use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub const SESSION: &str = "export XDG_RUNTIME_DIR=/run/user/$(id -u) WAYLAND_DISPLAY=wayland-0 QT_QPA_PLATFORM=wayland";
/// Where cc-share install puts cc-host.
pub const CC_HOST: &str = "~/.local/share/control-center/cc-host";

/// True when CC_SSH=1, which is what allows diagnostics over SSH.
pub fn on() -> bool {
    std::env::var("CC_SSH").as_deref() == Ok("1")
}

/// The only place an ssh or scp command line gets made. Without CC_SSH=1 the feature stops here.
pub fn ssh_argv(argv: &[&str], feature: &str, fix: &str) -> Vec<String> {
    if !on() {
        die(format!("{feature} would use SSH; that is diagnostics only (CC_SSH=1). {fix}").trim());
    }
    argv.iter().map(|a| a.to_string()).collect()
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a command with `input` on its stdin and returns its output. It's killed after `secs`,
/// and that comes back as Err (where Python raised TimeoutExpired).
pub fn run(argv: &[String], input: &str, secs: f64) -> Result<Out, String> {
    let mut c = Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("{}: {e}", argv[0]))?;
    let (mut i, o, e) = (c.stdin.take().expect("piped"), c.stdout.take().expect("piped"), c.stderr.take().expect("piped"));
    let input = input.to_owned();
    std::thread::spawn(move || i.write_all(input.as_bytes()));
    let read = |mut r: Box<dyn Read + Send>| std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = r.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    });
    let (to, te) = (read(Box::new(o)), read(Box::new(e)));
    let end = Instant::now() + Duration::from_secs_f64(secs);
    let status = loop {
        match c.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = c.kill();
                let _ = c.wait();
                return Err(format!("{} timed out after {secs} seconds", argv.join(" ")));
            }
        }
    };
    Ok(Out { code: status.code().unwrap_or(-1), stdout: to.join().unwrap_or_default(), stderr: te.join().unwrap_or_default() })
}

/// Runs a bash script on a machine over SSH. It's piped to bash because the login shell may be
/// fish. CC_SSH=1 only.
pub fn remote(host: &str, script: &str, feature: &str, fix: &str) -> Out {
    run(&ssh_argv(&["ssh", "-o", "BatchMode=yes", host, "bash -s"], feature, fix), script, 60.0).unwrap_or_else(|e| die(e))
}

/// The last `n` characters, like Python's text[-n:].
pub fn tail(s: &str, n: usize) -> &str {
    let k = s.chars().count();
    s.char_indices().nth(k.saturating_sub(n)).map_or(s, |(i, _)| &s[i..])
}

/// A machine's shared monitors, read over SSH from cc-share list and kscreen-doctor. Each one is
/// (index, output, width, height) in native pixels, swapped for a portrait monitor.
pub fn probe(user_host: &str) -> Vec<(i64, String, i64, i64)> {
    let out = remote(user_host, &format!("{SESSION}\n~/.local/bin/cc-share list; echo @@; kscreen-doctor -j"), "probe over SSH", "");
    if out.code != 0 && !out.stdout.contains("@@") {
        die(format!("can't ask {user_host}: {}", tail(out.stderr.trim(), 200)));
    }
    let (listing, screens) = out.stdout.split_once("@@").unwrap_or((&out.stdout, ""));
    let unusable = || -> ! { die(format!("{user_host}: kscreen-doctor -j said nothing usable")) };
    let doc: Value = screens.find('{').and_then(|i| serde_json::from_str(&screens[i..]).ok()).unwrap_or_else(|| unusable());
    let outputs = doc["outputs"].as_array().unwrap_or_else(|| unusable());
    let mut found = vec![];
    for line in listing.lines() {
        let Some((index, name)) = line.strip_prefix("monitor ").and_then(|r| r.split_once(": ")) else { continue };
        let name = name.split_whitespace().next().unwrap_or("");
        let (Ok(index), false) = (index.parse::<i64>(), name.is_empty() || !index.bytes().all(|c| c.is_ascii_digit())) else { continue };
        let Some(o) = outputs.iter().find(|o| o["name"] == name) else { continue };
        let num = |v: &Value| v.as_f64().filter(|x| *x != 0.0);
        let (mut w, mut h) = (num(&o["size"]["width"]), num(&o["size"]["height"]));
        if w.is_none() || h.is_none() {
            // Older kscreen has no size, so use the mode's, swapped for a quarter rotation (2, 8).
            let mode = o["modes"].as_array().into_iter().flatten().find(|m| m["id"] == o["currentModeId"]).map(|m| &m["size"]);
            (w, h) = mode.map_or((None, None), |s| (num(&s["width"]), num(&s["height"])));
            if [2, 8].contains(&o["rotation"].as_i64().unwrap_or(0)) {
                (w, h) = (h, w);
            }
        }
        if let (Some(w), Some(h)) = (w, h) {
            found.push((index, name.to_owned(), w as i64, h as i64));
        }
    }
    found
}

/// user@host for reaching a monitor's machine over SSH (align's tag viewer). A paired monitor's
/// user is its krdp login (cc-<frame>), not a real account, so for those I try its ssh= option,
/// then the login the host gave at pairing (trusted-hosts), then another monitor's on that host.
pub fn login(v: &cc_proto::conf::Viewer, conf: &std::path::Path) -> String {
    let user_host = v.target.rsplit_once(':').map_or(v.target.as_str(), |x| x.0);
    let (user, host) = user_host.rsplit_once('@').unwrap_or(("", user_host));
    if let Some(s) = v.opt("ssh").filter(|s| !s.is_empty()) {
        return s.to_owned();
    }
    if !user.starts_with("cc-") {
        return user_host.to_owned();
    }
    let t: Value = std::fs::read(conf.join("trusted-hosts").join(format!("{}.json", v.machine))).ok()
        .and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    if let Some(l) = t["login"].as_str().filter(|l| !l.is_empty()) {
        return format!("{l}@{host}");
    }
    let at = |x: &cc_proto::conf::Viewer| x.target.rsplit_once(':').map_or(x.target.clone(), |p| p.0.to_owned());
    let other = super::viewers().iter().find(|x| at(x).ends_with(&format!("@{host}")) && !x.target.starts_with("cc-")).map(at);
    other.unwrap_or_else(|| die(format!("{}: no login for {host} to align with: cc-home machine set {} ssh=<user>@{host}", v.name, v.name)))
}

/// A monitor's tag screens over SSH. It runs cc-host tagscreen <output> on that machine and sends
/// one JSON line per screen (same as the agent's: bg, tags and size). Each line gets "ok" back,
/// or "escaped" when someone at that machine pressed Esc.
pub struct Viewer {
    child: Child,
    stdin: Option<ChildStdin>,
    answers: Receiver<String>,
}

impl Viewer {
    /// Starts it. `gone(why)` gets called when it ends, either from Esc over there or because it died.
    pub fn start(host: &str, output: &str, gone: impl FnOnce() + Send + 'static) -> Result<Viewer, String> {
        let cmd = format!("bash -c '{SESSION}; exec {CC_HOST} tagscreen {output}'");
        let argv = ssh_argv(&["ssh", "-o", "BatchMode=yes", host, &cmd], "align", "");
        let mut child = Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        let (tx, answers) = channel();
        let out = child.stdout.take().expect("piped");
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if line.trim() == "escaped" {
                    break;
                }
                let _ = tx.send(line);
            }
            gone();
        });
        Ok(Viewer { stdin: child.stdin.take(), child, answers })
    }

    /// Shows one screen and waits up to 10 s for the answer. False when it's gone or hung.
    pub fn show(&mut self, params: &Value) -> bool {
        let sent = self.stdin.as_mut().is_some_and(|i| writeln!(i, "{params}").and_then(|_| i.flush()).is_ok());
        sent && self.answers.recv_timeout(Duration::from_secs(10)).is_ok()
    }

    pub fn close(mut self) {
        drop(self.stdin.take()); // EOF ends the tag screen
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end && matches!(self.child.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
    }
}

/// An output's size in mm from kscreen-doctor -j, swapped the same way its pixels are, since a
/// rotated output reports it unrotated. Matches what cc-host's agent gives.
pub fn output_mm(o: &Value) -> Value {
    let n = |v: &Value| v.as_i64().unwrap_or(0);
    let (w, h, mut mw, mut mh) = (n(&o["size"]["width"]), n(&o["size"]["height"]), n(&o["sizeMM"]["width"]), n(&o["sizeMM"]["height"]));
    if (w > h) != (mw > mh) {
        std::mem::swap(&mut mw, &mut mh);
    }
    serde_json::json!([mw, mh])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(tail("abcdé", 2), "dé");
        assert_eq!(tail("ab", 5), "ab");
        let out = run(&["cat".into()], "hi", 5.0).unwrap();
        assert_eq!((out.code, out.stdout.as_str()), (0, "hi"));
        assert!(run(&["sleep".into(), "5".into()], "", 0.2).is_err());
        let o = serde_json::json!({"size": {"width": 1440, "height": 2560}, "sizeMM": {"width": 698, "height": 393}});
        assert_eq!(output_mm(&o), serde_json::json!([393, 698]));
    }
}
