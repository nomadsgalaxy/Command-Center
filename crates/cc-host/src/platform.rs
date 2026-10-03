//! What the agent needs from the machine it runs on. docs/rust-host.md calls for a portable core over a
//! thin per-platform layer, and this is that layer. `Linux` is KDE Plasma plus systemd user units, the
//! way cc-share sets them up. `Fake` is the conformance suite's host: one monitor, and units that
//! "start" by listening.
use serde_json::{Value, json};
use std::collections::HashSet;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub trait Platform: Send + Sync {
    /// The shared monitors, each with index, output, width, height, mm, x, y, rotation and primary.
    fn monitors(&self) -> Vec<Value>;
    /// Runs systemctl --user <args> and returns its stdout.
    fn systemctl(&self, args: &[&str]) -> String;
    /// Whether something is listening on that local port.
    fn listening(&self, port: u16) -> bool;
    /// Counts established connections to that port, only from `ip` when it's given and from non-local addresses otherwise.
    fn viewers(&self, port: u16, ip: Option<&str>) -> usize;
    /// The host's name.
    fn hostname(&self) -> String;
    /// KWin's windows (getWindowInfo's fields), the current desktop and the shared monitors' logical rects.
    fn windows(&self) -> (Vec<Value>, Option<String>, Vec<[f64; 4]>);
    /// How much memory is free for another server, in MB.
    fn memory_mb(&self) -> u64;
    /// The outputs as they are now. Any change means new screencast targets.
    fn outputs(&self) -> String;
    /// When a unit last became active, in seconds on CLOCK_MONOTONIC. 0 means it isn't known.
    fn unit_started(&self, unit: &str) -> f64;
    /// Whether krdp logged a lost screencast stream in the last few seconds.
    fn stream_failed(&self, unit: &str) -> bool;
    /// Starts a tag screen on that output (tagshow --params) and returns it for its stdin and stdout, or None.
    fn tagshow(&self, output: &str) -> Option<std::process::Child>;
    /// Asks a question on that output. The answer is "block" (Enter) or "allow" (Esc, or 20 s).
    fn ask(&self, output: &str, text: &str) -> String;
}

/// CLOCK_MONOTONIC right now, in seconds. systemd's monotonic timestamps use the same clock.
pub fn mono() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

pub struct Linux {
    pub conf: std::path::PathBuf,
}

impl Platform for Linux {
    fn monitors(&self) -> Vec<Value> {
        // The ones cc-share recorded (~/.config/control-center/shared), in krdp's order, which is by priority.
        let shared: Vec<i64> = std::fs::read_to_string(self.conf.join("shared")).unwrap_or_default().split_whitespace().filter_map(|x| x.parse().ok()).collect();
        let v: Value = serde_json::from_str(&run("kscreen-doctor", &["-j"])).unwrap_or(json!({}));
        let mut outs: Vec<&Value> = v["outputs"].as_array().map(|a| a.iter().filter(|o| o["enabled"] == json!(true)).collect()).unwrap_or_default();
        outs.sort_by_key(|o| o["priority"].as_i64().unwrap_or(0));
        shared.iter().filter_map(|&i| {
            let o = outs.get(i as usize)?;
            let (w, h) = (o["size"]["width"].as_i64()?, o["size"]["height"].as_i64()?);
            let (mut mw, mut mh) = (o["sizeMM"]["width"].as_i64().unwrap_or(0), o["sizeMM"]["height"].as_i64().unwrap_or(0));
            if (w > h) != (mw > mh) {
                std::mem::swap(&mut mw, &mut mh); // A rotated output reports its size unrotated.
            }
            Some(json!({"index": i, "output": o["name"], "width": w, "height": h, "mm": [mw, mh],
                        "x": o["pos"]["x"], "y": o["pos"]["y"], "rotation": o["rotation"], "primary": o["priority"] == json!(1)}))
        }).collect()
    }
    fn systemctl(&self, args: &[&str]) -> String {
        let mut a = vec!["--user"];
        a.extend_from_slice(args);
        run("systemctl", &a)
    }
    fn listening(&self, port: u16) -> bool {
        crate::share::listening(port)
    }
    fn viewers(&self, port: u16, ip: Option<&str>) -> usize {
        // Established on that port from that address, or from anywhere but this machine if there's no address.
        crate::share::sockets().iter().filter(|(p, s, a)| *s == 1 && *p == port && match ip {
            Some(ip) => ip.parse::<std::net::IpAddr>().is_ok_and(|x| x == *a),
            None => !a.is_loopback(),
        }).count()
    }
    fn hostname(&self) -> String {
        std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_owned()
    }
    fn windows(&self) -> (Vec<Value>, Option<String>, Vec<[f64; 4]>) {
        kwin::windows().unwrap_or_default()
            .map_or((Vec::new(), None, Vec::new()), |(w, d)| (w, d, self.shared_rects()))
    }
    fn memory_mb(&self) -> u64 {
        std::fs::read_to_string("/proc/meminfo").unwrap_or_default().lines()
            .find_map(|l| l.strip_prefix("MemAvailable:").and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()))
            .map_or(0, |kb| kb / 1024)
    }
    fn outputs(&self) -> String {
        let v: Value = serde_json::from_str(&run("kscreen-doctor", &["-j"])).unwrap_or(json!({}));
        let mut sig: Vec<String> = v["outputs"].as_array().map(|a| a.iter().filter(|o| o["enabled"] == json!(true))
            .map(|o| format!("{} {} {} {} {}", o["name"], o["currentModeId"], o["rotation"], o["pos"], o["priority"])).collect()).unwrap_or_default();
        sig.sort();
        sig.join("|")
    }
    fn unit_started(&self, unit: &str) -> f64 {
        self.systemctl(&["show", unit, "-p", "ActiveEnterTimestampMonotonic", "--value"]).trim().parse::<f64>().map_or(0.0, |us| us / 1e6)
    }
    fn stream_failed(&self, unit: &str) -> bool {
        !run("journalctl", &["--user", "-u", unit, "--since", "-6s", "-q", "-o", "cat", "-g", "target not found|Stream error"]).trim().is_empty()
    }

    fn ask(&self, output: &str, text: &str) -> String {
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let answer = run("timeout", &["30", &exe, "tagscreen", output, "--ask", text]);
        if answer.trim() == "block" { "block".into() } else { "allow".into() }
    }

    fn tagshow(&self, output: &str) -> Option<std::process::Child> {
        Command::new(std::env::current_exe().ok()?).arg("tagscreen").arg(output)
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().ok()
    }
}

impl Linux {
    /// The shared monitors' logical rectangles in KWin's coordinates: position, and size divided by scale.
    fn shared_rects(&self) -> Vec<[f64; 4]> {
        let names: Vec<String> = self.monitors().iter().filter_map(|m| m["output"].as_str().map(str::to_owned)).collect();
        let v: Value = serde_json::from_str(&run("kscreen-doctor", &["-j"])).unwrap_or(json!({}));
        v["outputs"].as_array().map(|a| a.iter().filter(|o| o["enabled"] == json!(true) && names.iter().any(|n| o["name"] == json!(n)))
            .map(|o| {
                let scale = o["scale"].as_f64().unwrap_or(1.0).max(0.1);
                [o["pos"]["x"].as_f64().unwrap_or(0.0), o["pos"]["y"].as_f64().unwrap_or(0.0),
                 o["size"]["width"].as_f64().unwrap_or(0.0) / scale, o["size"]["height"].as_f64().unwrap_or(0.0) / scale]
            }).collect()).unwrap_or_default()
    }
}

/// KWin over D-Bus, through zbus with no Python or gi. It gets the WindowsRunner's window ids (each is
/// listed twice, so they're deduped), getWindowInfo for each one, and the current virtual desktop.
mod kwin {
    use serde_json::{Map, Value, json};
    use std::collections::{BTreeSet, HashMap};
    use zbus::zvariant::OwnedValue;

    fn to_json(v: &zbus::zvariant::Value) -> Value {
        use zbus::zvariant::Value as V;
        match v {
            V::Str(s) => json!(s.as_str()),
            V::Bool(b) => json!(b),
            V::F64(f) => json!(f),
            V::I32(i) => json!(i),
            V::U32(i) => json!(i),
            V::I64(i) => json!(i),
            V::U64(i) => json!(i),
            V::Array(a) => Value::Array(a.iter().map(to_json).collect()),
            V::Value(inner) => to_json(inner),
            _ => Value::Null,
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn windows() -> zbus::Result<Option<(Vec<Value>, Option<String>)>> {
        let bus = zbus::blocking::Connection::session()?;
        let m: Vec<(String, String, String, i32, f64, HashMap<String, OwnedValue>)> =
            bus.call_method(Some("org.kde.KWin"), "/WindowsRunner", Some("org.kde.krunner1"), "Match", &("",))?.body().deserialize()?;
        let ids: BTreeSet<String> = m.into_iter().filter_map(|r| r.0.strip_prefix("0_").filter(|u| u.starts_with('{')).map(str::to_owned)).collect();
        let mut out = Vec::new();
        for id in ids {
            let Ok(reply) = bus.call_method(Some("org.kde.KWin"), "/KWin", Some("org.kde.KWin"), "getWindowInfo", &(id.as_str(),)) else { continue };
            let info: HashMap<String, OwnedValue> = reply.body().deserialize()?;
            if !info.is_empty() {
                out.push(Value::Object(info.iter().map(|(k, v)| (k.clone(), to_json(v))).collect::<Map<_, _>>()));
            }
        }
        let desktop: Option<String> = bus.call_method(Some("org.kde.KWin"), "/VirtualDesktopManager", Some("org.freedesktop.DBus.Properties"), "Get",
                                                      &("org.kde.KWin.VirtualDesktopManager", "current"))
            .ok().and_then(|r| r.body().deserialize::<OwnedValue>().ok()).and_then(|v| match to_json(&v) { Value::String(s) => Some(s), _ => None });
        Ok(Some((out, desktop)))
    }
}

/// The conformance suite's host. It has one 1920x1080 monitor, a unit "starts" when its port starts
/// listening 0.3 s later (slot 1's ports), and nobody watches.
#[derive(Default)]
pub struct Fake {
    up: Arc<Mutex<HashSet<u16>>>,
}

impl Platform for Fake {
    fn monitors(&self) -> Vec<Value> {
        vec![json!({"index": 0, "output": "eDP-1", "width": 1920, "height": 1080, "mm": [344, 194], "x": 0, "y": 0, "rotation": 1, "primary": true})]
    }
    fn systemctl(&self, args: &[&str]) -> String {
        let port = |unit: &str| -> Option<u16> {
            let i = unit.split('@').nth(1)?;
            if unit.contains("window@") { Some(3405 + 10 + i.split('-').nth_back(5)?.parse::<u16>().ok()?) } else { Some(3410 + i.trim_end_matches(".service").rsplit('-').next()?.parse::<u16>().ok()?) }
        };
        match (args.first(), args.get(1)) {
            (Some(&"start"), Some(u)) | (Some(&"restart"), Some(u)) if !u.contains('*') => {
                if let Some(p) = port(u) {
                    let up = self.up.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(300));
                        up.lock().unwrap().insert(p);
                    });
                }
            }
            (Some(&"stop"), _) => {
                for u in &args[1..] {
                    if let Some(p) = port(u) {
                        self.up.lock().unwrap().remove(&p);
                    }
                }
            }
            _ => {}
        }
        String::new()
    }
    fn listening(&self, port: u16) -> bool {
        self.up.lock().unwrap().contains(&port)
    }
    fn viewers(&self, _: u16, _: Option<&str>) -> usize {
        0
    }
    fn hostname(&self) -> String {
        "fakehost".into()
    }
    fn windows(&self) -> (Vec<Value>, Option<String>, Vec<[f64; 4]>) {
        (fake_windows(), Some("D1".into()), vec![[0.0, 0.0, 1920.0, 1080.0]])
    }
    fn memory_mb(&self) -> u64 {
        4096
    }
    fn outputs(&self) -> String {
        "fake".into()
    }
    fn unit_started(&self, _: &str) -> f64 {
        0.0
    }
    fn stream_failed(&self, _: &str) -> bool {
        false
    }
    fn ask(&self, _: &str, _: &str) -> String {
        "allow".into()
    }

    fn tagshow(&self, _: &str) -> Option<std::process::Child> {
        Command::new("cat").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().ok()
    }
}

/// The conformance suite's windows, the same ones the Python fake host had: one poppable window, plus
/// one of each kind the scope keeps out.
pub fn fake_windows() -> Vec<Value> {
    let w = |n: u32, extra: Value| {
        let mut v = json!({"uuid": format!("{{{n:08x}-0000-4000-8000-000000000000}}"), "type": 0, "x": 100.0, "y": 100.0,
                           "width": 800.0, "height": 600.0, "desktops": ["D1"], "desktopFile": "org.kde.dolphin", "caption": "secret"});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    vec![w(1, json!({})), w(2, json!({"excludeFromCapture": true})), w(3, json!({"minimized": true})),
         w(4, json!({"desktops": ["D2"]})), w(5, json!({"x": 5000.0})), w(6, json!({"type": 2})),
         w(7, json!({"x": 1000.0, "width": 400.0, "desktopFile": "org.kde.kate"})), w(8, json!({"x": 1500.0, "width": 300.0, "desktopFile": "org.kde.konsole"}))]
}
