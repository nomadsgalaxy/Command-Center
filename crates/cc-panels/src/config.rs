//! Command Center's files in ~/.config/control-center: viewers.conf (the remote monitors),
//! home.json (saved spots), settings.json and password (plus passwords/<machine> and
//! trusted-hosts/ once a machine is paired).
use crate::geometry::Pose;
use cc_proto::conf::{self, Json};

pub fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".into())
}

pub fn config(name: &str) -> String {
    format!("{}/.config/control-center/{name}", home_dir())
}

/// One remote monitor: `<name> <user>@<host>:<port> <screen> <w>x<h> [curve=h|v|flat] [autoconnect=yes|no] ...`.
#[derive(Clone, Debug, Default)]
pub struct Viewer {
    pub name: String,
    pub user: String,
    pub host: String,
    pub port: u32,
    pub screen: i32, // the screen column (1-based): the RDP monitor, and the key older spots were saved under
    pub w: u32,
    pub h: u32,
    pub auto: bool, // autoconnect=yes (for cc-home, missing means no)
    pub machine: String, // machine= (a paired host's name, which finds its password and certificate pin), else ""
    pub label: String,   // label= (D-050), decoded: the user's name for this monitor, else ""
    pub pop: Option<(String, String)>, // for a popped-out window (popout.rs): its source monitor and uuid
    pub vnc: Option<Vnc>, // proto=vnc (vnc.rs); None is RDP, the default
    pub no_audio: bool, // audio=no: this machine's sound and microphone stay off (docs/audio.md); on by default
}

/// A VNC monitor's options (docs/vnc.md): `proto=vnc [pin=<sha256 hex>] [tls=no]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vnc {
    /// pin=: the host certificate's SHA-256, for a VNC host that isn't paired (a paired one's
    /// comes from trusted-hosts, like RDP's).
    pub pin: String,
    /// tls=no: also allow VNC's own password check and Apple's (ARD) without TLS, which macOS and
    /// TightVNC need until there's a TLS forwarder. The password then crosses the LAN weakly
    /// protected, so it's off unless you say so.
    pub insecure: bool,
}

pub fn viewers(wanted: &[String]) -> Vec<Viewer> {
    parse_viewers(&std::fs::read_to_string(config("viewers.conf")).unwrap_or_default(), wanted)
}

fn parse_viewers(text: &str, wanted: &[String]) -> Vec<Viewer> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 || f[0].starts_with('#') || (!wanted.is_empty() && !wanted.iter().any(|w| w == f[0])) {
            continue;
        }
        let Ok(screen) = f[2].parse::<i32>() else { continue };
        let (user, hostport) = f[1].split_once('@').unwrap_or(("", f[1]));
        let vnc = f[4..].contains(&"proto=vnc").then(|| Vnc {
            pin: f[4..].iter().find_map(|o| o.strip_prefix("pin=")).unwrap_or("").to_ascii_lowercase(),
            insecure: f[4..].contains(&"tls=no"),
        });
        let default_port = if vnc.is_some() { "5900" } else { "3389" };
        let (host, port) = hostport.rsplit_once(':').unwrap_or((hostport, default_port));
        let (w, h) = f[3].split_once('x').unwrap_or(("1920", "1080"));
        out.push(Viewer {
            name: f[0].into(),
            user: user.into(),
            host: host.into(),
            port: port.parse().unwrap_or(default_port.parse().unwrap_or(3389)),
            screen: screen + 1,
            w: w.parse().unwrap_or(1920),
            h: h.parse().unwrap_or(1080),
            auto: f[4..].contains(&"autoconnect=yes"),
            machine: f[4..].iter().find_map(|o| o.strip_prefix("machine=")).unwrap_or("").into(),
            label: f[4..].iter().find_map(|o| o.strip_prefix("label=")).map(decode).unwrap_or_default(),
            pop: None,
            vnc,
            no_audio: f[4..].contains(&"audio=no"),
        });
    }
    out
}

/// Decodes label= the way cc-home writes it (RFC 3986: every byte except A-Za-z0-9-._~ becomes
/// %XX of its UTF-8). A bad escape, or bytes that aren't UTF-8, leave the text as written.
fn decode(s: &str) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'%' {
            let Some(b) = s.get(i + 1..i + 3).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit())).and_then(|h| u8::from_str_radix(h, 16).ok()) else {
                return clean(s);
            };
            out.push(b);
            i += 3;
        } else {
            out.push(s.as_bytes()[i]);
            i += 1;
        }
    }
    clean(&String::from_utf8(out).unwrap_or_else(|_| s.into()))
}

/// A label ready to show: no control characters, since it goes into argv and a tag, and 64
/// characters at most.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// Whether `me` carries its machine's sound and microphone (docs/audio.md). One RDP session per
/// machine does, or desk-wide and desk-portrait would both play it and both claim the microphone.
/// `peers` is every monitor, as (viewer, up), where up means live and not failing to connect. It's
/// the lowest-numbered screen of `me`'s machine among the ones that are up, and `me` counts as up.
/// One line saying audio=no turns the whole machine off. Pop-out windows, VNC and Frame windows
/// never carry it.
pub fn carries_audio(me: &Viewer, peers: &[(&Viewer, bool)]) -> bool {
    let same = |v: &Viewer| v.pop.is_none() && v.vnc.is_none() && machine_of(v) == machine_of(me);
    if !same(me) || peers.iter().any(|(v, _)| same(v) && v.no_audio) || me.no_audio {
        return false;
    }
    let first = peers.iter().filter(|(v, up)| same(v) && (*up || v.name == me.name)).map(|(v, _)| (v.screen, v.name.as_str())).chain([(me.screen, me.name.as_str())]).min();
    first == Some((me.screen, me.name.as_str()))
}

/// The machine a viewer belongs to: machine=, or its host if it was never paired, same as cc-home.
pub fn machine_of(v: &Viewer) -> &str {
    if v.machine.is_empty() { &v.host } else { &v.machine }
}

/// trusted-hosts/<machine>.json, or {} when there isn't one (or the name can't be a file).
fn host_json(dir: &str, machine: &str) -> serde_json::Value {
    file_name(machine).then(|| std::fs::read_to_string(format!("{dir}trusted-hosts/{machine}.json")).ok()).flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The label of a viewer's machine (trusted-hosts' `label`), or "" when it has none.
pub fn machine_label(v: &Viewer) -> String {
    clean(host_json(&config(""), machine_of(v))["label"].as_str().unwrap_or(""))
}

/// What the user sees for a remote monitor (D-050, matching cc-home's display). That's its own
/// label if it has one, else its machine's label, with the output added when that machine has 2
/// or more lines in viewers.conf (`all`), else its name. Everything else (spots, logs, overlays)
/// keeps using the name.
pub fn display(v: &Viewer, all: &[Viewer]) -> String {
    display_in(&config(""), v, all)
}

fn display_in(dir: &str, v: &Viewer, all: &[Viewer]) -> String {
    if !v.label.is_empty() {
        return v.label.clone();
    }
    let m = machine_of(v);
    let t = host_json(dir, m);
    let label = clean(t["label"].as_str().unwrap_or(""));
    if label.is_empty() {
        return v.name.clone();
    }
    if all.iter().filter(|x| machine_of(x) == m).count() < 2 {
        return label;
    }
    let out = t["monitors"][((v.port as i64 - 3400).rem_euclid(10)).to_string()].as_str().map_or_else(|| v.name.clone(), clean);
    format!("{label} {out}")
}

// ------------------------------------------------------------------ workspaces
//
// home.json holds workspaces. Each one is a set of spots (home, scanned, presets) for one real
// place, like home or work, and it's bound to the SteamVR tracking universe (the room map) it
// was made in, so the right one gets picked by where you are. Temporary is for travelling. The
// rules are in cc_proto::conf and docs/workspaces.md:
//   {"workspace": "<active>", "workspaces": {"<name>": {"universe": "<id>", "spots": {...}, "machines": [...]}}}
// A workspace entered in a second room also has "universes": [all of its rooms] (conf::rooms).
// An older file with only "spots" becomes the workspace "default". cc-home reads it the same way.

fn read_home() -> serde_json::Value {
    let json: serde_json::Value =
        std::fs::read_to_string(config("home.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({}));
    migrate(json)
}

fn migrate(mut json: serde_json::Value) -> serde_json::Value {
    if !json["workspaces"].is_object() {
        // an older file's spots are the user's ("default"); with none (a fresh install) it's Temporary
        let name = if json.get("spots").is_some() { "default" } else { cc_proto::conf::TEMPORARY };
        let spots = json.get("spots").cloned().unwrap_or_else(|| serde_json::json!({}));
        json = serde_json::json!({"workspace": name, "workspaces": {name: {"spots": spots}}});
    }
    json
}

fn write_home(json: &serde_json::Value) -> std::io::Result<()> {
    write_json(&config("home.json"), json)
}

/// ~/.cache/control-center/<name>
pub fn cache(name: &str) -> String {
    format!("{}/.cache/control-center/{name}", home_dir())
}

/// Writes the whole file or nothing, so a crash mid-write never leaves half a file.
pub fn write_json(path: &str, json: &serde_json::Value) -> std::io::Result<()> {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(json)? + "\n")?;
    std::fs::rename(tmp, path)
}

/// A file the launcher wrote just before this start (cc-home autoconnect --write). None when it's
/// missing or older than five minutes, which keeps today's behaviour if a launcher stops writing it.
fn fresh(name: &str) -> Option<String> {
    let path = cache(name);
    let age = std::fs::metadata(&path).ok()?.modified().ok()?.elapsed().ok()?;
    (age < std::time::Duration::from_secs(300)).then(|| std::fs::read_to_string(&path).ok()).flatten()
}

/// The remotes to connect at start for this workspace. autoconnect.txt has "workspace<TAB>a,b" per
/// line, with "*" for a workspace it didn't know. None means connect them all (no fresh file).
pub fn autoconnect(workspace: &str) -> Option<Vec<String>> {
    pick_autoconnect(&fresh("autoconnect.txt")?, workspace)
}

fn pick_autoconnect(text: &str, workspace: &str) -> Option<Vec<String>> {
    let line = |w: &str| text.lines().find_map(|l| l.split_once('\t').filter(|(n, _)| *n == w).map(|(_, v)| v.to_string()));
    let names = line(workspace).or_else(|| line("*"))?;
    Some(names.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
}

/// The room SteamVR is tracking now (main.rs keeps it up to date). Save as binds a new workspace to it.
pub static UNIVERSE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn home_path() -> std::path::PathBuf {
    std::path::PathBuf::from(config("home.json"))
}

/// home.json, read the way cc-home reads it (the workspaces' rules live in cc_proto::conf).
pub fn workspaces() -> Result<Json, String> {
    conf::read_home(&home_path())
}

/// Changes home.json with f and writes it the way cc-home does. If f errors, nothing's written
/// and we return its error.
pub fn edit_workspaces<T>(f: impl FnOnce(&mut Json) -> Result<T, String>) -> Result<T, String> {
    let mut data = workspaces()?;
    let out = f(&mut data)?;
    conf::write_home(&home_path(), &data).map_err(|e| format!("can't save home.json: {e}"))?;
    refresh_members();
    Ok(out)
}

/// Picks the workspace for the room SteamVR is tracking now (`universe`, 0 if unknown) and makes
/// it active. conf::choose_workspace has the rules: the room's workspace, else Temporary when
/// travelling. Returns its name, and whether Temporary was just created.
pub fn choose_workspace(universe: u64) -> (String, bool) {
    if conf::room(universe) == 0 {
        eprintln!("workspaces: SteamVR has no room yet (universe {universe})");
    }
    UNIVERSE.store(conf::room(universe), std::sync::atomic::Ordering::Relaxed);
    // SteamVR can't tell which room, so let the network decide (workspace-hint.txt, cc-home workspace-for)
    let hint = fresh("workspace-hint.txt").map(|h| h.trim().to_string());
    let out = edit_workspaces(|data| Ok(conf::choose_workspace(data, universe, hint.as_deref())));
    out.unwrap_or_else(|e| {
        eprintln!("workspaces: {e}");
        refresh_members();
        (conf::TEMPORARY.into(), false)
    })
}

/// The active workspace's machines (None means all of them), as last read by refresh_members.
static MEMBERS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// Re-reads the active workspace's machines, at start and after any workspace change.
pub fn refresh_members() {
    let m = workspaces().ok().and_then(|d| conf::workspace_machines(&d, &conf::active_workspace(&d)));
    *MEMBERS.lock().unwrap() = m;
}

/// Whether remote monitor v's machine belongs to the active workspace. Only those get panels and
/// taskbar chips.
pub fn member(v: &Viewer) -> bool {
    MEMBERS.lock().unwrap().as_ref().is_none_or(|m| m.iter().any(|x| x == machine_of(v)))
}

/// The active workspace's spots.
fn spots(json: &serde_json::Value) -> &serde_json::Value {
    let active = json["workspace"].as_str().unwrap_or("default");
    &json["workspaces"][active]["spots"]
}

/// A panel's place in the active workspace's "home" spot, looked up by name (a viewer's, or a
/// window's `app:<id>`), or by screen number for older spots.
pub fn home_pose(name: &str, screen: Option<i32>) -> Option<Pose> {
    let json = read_home();
    let home = &spots(&json)["home"];
    pose_from(home.get(name).or_else(|| home.get(screen?.to_string()))?)
}

/// Where align put each panel (the active workspace's "scanned" spot), keyed like home_pose: by
/// name, else by screen number. Reads home.json once for all of them.
pub fn scanned_poses(keys: &[(&str, i32)]) -> Vec<Option<Pose>> {
    let json = read_home();
    let scanned = &spots(&json)["scanned"];
    keys.iter().map(|(name, screen)| pose_from(scanned.get(*name).or_else(|| scanned.get(screen.to_string()))?)).collect()
}

/// A pose the way spots and win-poses.json store it.
pub fn pose_from(p: &serde_json::Value) -> Option<Pose> {
    let num = |k: &str| p[k].as_f64();
    let c = p["centre"].as_array()?;
    Some(Pose {
        centre: [c.first()?.as_f64()?, c.get(1)?.as_f64()?, c.get(2)?.as_f64()?],
        yaw: num("yaw")?,
        pitch: num("pitch")?,
        roll: num("roll")?,
        width: num("width")?,
        // a top-to-bottom curve (cc-home scan's vcurve) wins, and the panel gets turned for it (kvm.rs)
        curve: num("vcurve").filter(|&v| v > 0.0).or(num("curve")).unwrap_or(0.0),
        vert: num("vcurve").is_some_and(|v| v > 0.0),
    })
}

pub fn pose_json(p: &Pose, height: f64) -> serde_json::Value {
    let r = |x: f64, d: i32| (x * 10f64.powi(d)).round() / 10f64.powi(d);
    serde_json::json!({
        "centre": p.centre.map(|c| r(c, 4)),
        "yaw": r(p.yaw, 3), "pitch": r(p.pitch, 3), "roll": r(p.roll, 3),
        "width": r(p.width, 4), "height": r(height, 4),
        "curve": if p.vert { 0.0 } else { r(p.curve, 3) }, "vcurve": if p.vert { r(p.curve, 3) } else { 0.0 },
    })
}

/// Writes one setting into settings.json and keeps the rest. It doesn't go through settings(),
/// because a typo in the file reads as {} there, and writing that back would lose every other
/// setting.
pub fn set_setting(key: &str, value: serde_json::Value) -> Result<(), String> {
    let path = config("settings.json");
    let mut s = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("settings.json: {e}")),
        Ok(t) => match serde_json::from_str::<serde_json::Value>(&t) {
            Ok(v) if v.is_object() => v,
            _ => return Err("settings.json doesn't parse".into()),
        },
    };
    s[key] = value;
    write_json(&path, &s).map_err(|e| format!("settings.json: {e}"))
}

/// settings.json (`taskbar`, `window_px_per_m`, ...), or {} when there isn't one.
pub fn settings() -> serde_json::Value {
    std::fs::read_to_string(config("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({}))
}

/// The krdp password for a viewer of `machine`: passwords/<machine> from pairing, else the
/// global one.
pub fn password(machine: &str) -> String {
    password_in(&config(""), machine)
}

fn password_in(dir: &str, machine: &str) -> String {
    let read = |n: &str| std::fs::read_to_string(format!("{dir}{n}")).ok().map(|t| t.trim_end_matches(['\n', '\r']).to_string());
    file_name(machine).then(|| read(&format!("passwords/{machine}"))).flatten().or_else(|| read("password")).unwrap_or_default()
}

/// A paired machine's certificate pin (cert_sha256 in trusted-hosts/<machine>.json, hex). None
/// when it isn't paired (no machine=, or no such file). It fails closed: "" matches no
/// certificate, and that's what we return when the file's unreadable or unusable, or the name
/// can't be a file.
pub fn pin(machine: &str) -> Option<String> {
    pin_in(&config(""), machine)
}

fn pin_in(dir: &str, machine: &str) -> Option<String> {
    if machine.is_empty() {
        return None;
    }
    if !file_name(machine) {
        return Some(String::new());
    }
    let t = match std::fs::read_to_string(format!("{dir}trusted-hosts/{machine}.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        r => r.unwrap_or_default(),
    };
    let j: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
    let hex = j["cert_sha256"].as_str().unwrap_or("");
    Some(if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) { hex.to_ascii_lowercase() } else { String::new() })
}

/// Safe as a file name in ~/.config/control-center: no path and not hidden. Unicode letters are
/// fine, since cc-home's names are Python's [\w.-].
fn file_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_alphanumeric() || "._-".contains(c))
}

/// A spot's zoom: for a window panel, its text size as a factor on window_px_per_m.
pub fn home_zoom(name: &str) -> Option<f64> {
    spots(&read_home())["home"][name]["zoom"].as_f64()
}

/// One panel's place into the "home" spot, keyed by name the way cc-home saves it (an older
/// entry under its screen number is removed), leaving everything else in home.json alone.
/// `zoom` is a window panel's, kept with its spot.
pub fn save_home_pose(name: &str, screen: Option<i32>, p: &Pose, height: f64, zoom: Option<f64>) -> std::io::Result<()> {
    let mut json = read_home();
    let active = json["workspace"].as_str().unwrap_or("default").to_string();
    let home = &mut json["workspaces"][&active]["spots"]["home"];
    if !home.is_object() {
        *home = serde_json::json!({});
    }
    let home = home.as_object_mut().unwrap();
    if let Some(s) = screen {
        home.remove(&s.to_string());
    }
    let mut spot = pose_json(p, height);
    if let Some(z) = zoom {
        spot["zoom"] = serde_json::json!((z * 1000.0).round() / 1000.0);
    }
    home.insert(name.into(), spot);
    write_home(&json)
}

#[cfg(test)]
mod tests {
    #[test]
    fn viewers_read_autoconnect() {
        let t = "# name  user@host:port\ndesk-wide  a@10.0.0.1:3400  2  3840x1080  curve=h  autoconnect=yes\nlaptop  a@10.0.0.2:3400  3  1920x1080  autoconnect=no\nold  a@10.0.0.3:3401  1  1440x2560\n";
        let v = super::parse_viewers(t, &[]);
        assert_eq!(v.iter().map(|v| (v.name.as_str(), v.auto)).collect::<Vec<_>>(), [("desk-wide", true), ("laptop", false), ("old", false)]);
        assert_eq!((v[0].host.as_str(), v[0].port, v[0].screen, v[0].w), ("10.0.0.1", 3400, 3, 3840));
        assert_eq!(v[0].machine, "");
        let p = super::parse_viewers("desk-dp-1  cc-frame@10.0.0.1:3410  4  2560x1440  machine=desk\n", &[]);
        assert_eq!(p[0].machine, "desk");
        assert_eq!(super::parse_viewers(t, &["old".into()]).len(), 1);
    }

    #[test]
    fn viewers_read_audio_no() {
        let v = super::parse_viewers("a  u@h:3400  1  1920x1080\nb  u@h:3401  2  1920x1080  audio=no\n", &[]);
        assert_eq!((v[0].no_audio, v[1].no_audio), (false, true), "on unless audio=no");
    }

    #[test]
    fn one_session_per_machine_carries_audio() {
        let v = super::parse_viewers("wide u@h:3410 1 1920x1080 machine=desk\nport u@h:3411 2 1080x1920 machine=desk\nlap u@l:3410 1 1920x1080 machine=lap\nvnc u@m:5900 1 1920x1080 machine=desk proto=vnc\n", &[]);
        let all = |up: [bool; 4]| v.iter().zip(up).collect::<Vec<_>>();
        let who = |p: &[(&super::Viewer, bool)]| v.iter().map(|m| super::carries_audio(m, p)).collect::<Vec<_>>();
        assert_eq!(who(&all([true; 4])), [true, false, true, false], "the lowest screen of each machine, never VNC");
        assert_eq!(who(&all([false, true, true, true])), [true, true, true, false], "the first one is down: the second one takes it (and the first takes it back when it connects)");
        assert_eq!(who(&all([false; 4]))[1], true, "a lone session carries it, even if the others are down");
        let mut off = super::parse_viewers("a u@h:3410 1 1920x1080 machine=d\nb u@h:3411 2 1920x1080 machine=d audio=no\n", &[]);
        let p: Vec<_> = off.iter().map(|m| (m, true)).collect();
        assert_eq!((super::carries_audio(&off[0], &p), super::carries_audio(&off[1], &p)), (false, false), "one audio=no turns the machine off");
        off[1].no_audio = false;
        let p: Vec<_> = off.iter().map(|m| (m, true)).collect();
        assert!(super::carries_audio(&off[0], &p));
    }

    #[test]
    fn viewers_read_proto_vnc() {
        let v = super::parse_viewers("mac  me@mac.local  1  2560x1440  proto=vnc  tls=no\nsway  me@pi:5901  2  1920x1080  proto=vnc  pin=AB12\nkde  me@desk  3  1920x1080\n", &[]);
        assert_eq!((v[0].port, v[0].vnc.clone()), (5900, Some(super::Vnc { pin: String::new(), insecure: true })), "VNC's port, and tls=no");
        assert_eq!((v[1].port, v[1].vnc.clone()), (5901, Some(super::Vnc { pin: "ab12".into(), insecure: false })), "TLS unless tls=no");
        assert_eq!((v[2].port, v[2].vnc.clone()), (3389, None), "no proto= is RDP");
    }

    #[test]
    fn viewers_read_the_line_machine_add_writes() {
        // matches cc-home's f"{name:<15} {addr:<27} {screen:<7} {size}" (the Machines window's Add machine)
        let t = format!("{:<15} {:<27} {:<7} {}\n", "desk-2", "user@203.0.113.70:3401", 4, "2560x1440");
        let v = &super::parse_viewers(&t, &[])[0];
        assert_eq!((v.name.as_str(), v.user.as_str(), v.host.as_str(), v.port), ("desk-2", "user", "203.0.113.70", 3401));
        assert_eq!((v.screen, v.w, v.h, v.auto), (5, 2560, 1440, false));
    }

    #[test]
    fn passwords_and_pins_by_machine() {
        let dir = std::env::temp_dir().join(format!("cc-panels-test-{}/", std::process::id()));
        let d = dir.to_str().unwrap();
        std::fs::create_dir_all(dir.join("passwords")).unwrap();
        std::fs::create_dir_all(dir.join("trusted-hosts")).unwrap();
        assert_eq!(super::password_in(d, "desk"), "", "none at all");
        std::fs::write(dir.join("password"), "global\n").unwrap();
        std::fs::write(dir.join("passwords/desk"), "per-frame\n").unwrap();
        assert_eq!(super::password_in(d, "desk"), "per-frame");
        assert_eq!(super::password_in(d, "laptop"), "global", "not paired: the global one");
        assert_eq!(super::password_in(d, ""), "global", "no machine=");
        assert_eq!(super::password_in(d, "../password"), "global", "not a file name");
        let hex = "AB".repeat(32);
        std::fs::write(dir.join("trusted-hosts/desk.json"), format!(r#"{{"addr": "10.0.0.1", "host_pk": "x", "cert_sha256": "{hex}"}}"#)).unwrap();
        std::fs::write(dir.join("trusted-hosts/bad.json"), r#"{"cert_sha256": "abc"}"#).unwrap();
        assert_eq!(super::pin_in(d, "desk"), Some("ab".repeat(32)));
        assert_eq!(super::pin_in(d, "bad"), Some(String::new()), "unusable: pinned to nothing");
        assert_eq!(super::pin_in(d, "laptop"), None, "not paired: today's");
        assert_eq!(super::pin_in(d, ""), None);
        assert_eq!(super::pin_in(d, "../desk"), Some(String::new()), "not a file name: refused");
        assert_eq!(super::pin_in(d, ".hidden"), Some(String::new()));
        std::fs::create_dir_all(dir.join("trusted-hosts/odd.json")).unwrap(); // a read error, not ENOENT
        assert_eq!(super::pin_in(d, "odd"), Some(String::new()), "unreadable: refused, not unpaired");
        std::fs::write(dir.join("trusted-hosts/café.json"), format!(r#"{{"cert_sha256": "{hex}"}}"#)).unwrap();
        assert_eq!(super::pin_in(d, "café"), Some("ab".repeat(32)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn labels_decode_percent_escapes_or_stay_as_written() {
        let label = |opt: &str| super::parse_viewers(&format!("d a@h:3400 0 1920x1080 {opt}\n"), &[])[0].label.clone();
        assert_eq!(label("label=Desk%20left"), "Desk left");
        assert_eq!(label("label=Caf%C3%A9%2C%20d%C3%A9j%C3%A0"), "Café, déjà", "UTF-8 bytes, a comma");
        assert_eq!(label("label=a-b._~"), "a-b._~", "unreserved: as is");
        assert_eq!(label(""), "", "none");
        assert_eq!(label("label=100%"), "100%", "a bad escape: as written");
        assert_eq!(label("label=%zz1"), "%zz1");
        assert_eq!(label("label=%+1x"), "%+1x", "not hex digits");
        assert_eq!(label("label=%FF%FE"), "%FF%FE", "not UTF-8: as written");
        assert_eq!(label("label=a%00b%0Ac"), "abc", "no control characters");
        assert_eq!(label(&format!("label={}", "%C3%A9".repeat(70))).chars().count(), 64, "64 characters at most");
    }

    #[test]
    fn display_is_the_monitors_label_the_machines_or_the_name() {
        let dir = std::env::temp_dir().join(format!("cc-panels-display-{}/", std::process::id()));
        let d = dir.to_str().unwrap();
        std::fs::create_dir_all(dir.join("trusted-hosts")).unwrap();
        std::fs::write(dir.join("trusted-hosts/id1.json"), r#"{"id": "id1", "host": "desk", "label": "Desk", "monitors": {"0": "DP-1", "1": "HDMI-A-1"}}"#).unwrap();
        std::fs::write(dir.join("trusted-hosts/id2.json"), r#"{"id": "id2", "host": "laptop", "label": "Work laptop"}"#).unwrap();
        std::fs::write(dir.join("trusted-hosts/id3.json"), r#"{"id": "id3", "label": null}"#).unwrap();
        std::fs::write(dir.join("trusted-hosts/bad.json"), "{not json").unwrap();
        std::fs::write(dir.join("trusted-hosts/10.0.0.9.json"), r#"{"label": "Old box"}"#).unwrap(); // renaming a machine that was never paired
        let all = super::parse_viewers(
            "desk-wide a@h:3400 0 3840x1080 machine=id1\n\
             desk-side a@h:3411 1 1440x2560 machine=id1 label=Side%20one\n\
             desk-3 a@h:3402 2 1920x1080 machine=id1\n\
             laptop a@l:3400 3 1920x1080 machine=id2\n\
             plain a@p:3400 4 1920x1080 machine=id3\n\
             broken a@b:3400 5 1920x1080 machine=bad\n\
             sneaky a@b:3400 6 1920x1080 machine=../id2\n\
             old a@10.0.0.9:3400 7 1920x1080\n",
            &[],
        );
        let show: Vec<_> = all.iter().map(|v| super::display_in(d, v, &all)).collect();
        assert_eq!(show[0], "Desk DP-1", "the machine's, and its output: 2+ monitors");
        assert_eq!(show[1], "Side one", "its own label first");
        assert_eq!(show[2], "Desk desk-3", "no output for it: the name");
        assert_eq!(show[3], "Work laptop", "one monitor: the machine's alone");
        assert_eq!(show[4], "plain", "no label: the name");
        assert_eq!(show[5], "broken", "a file that won't parse: the name");
        assert_eq!(show[6], "sneaky", "not a file name: not read");
        assert_eq!(show[7], "Old box", "never paired: by its host");
        assert_eq!(super::display_in(d, &all[1], &all[1..2]), "Side one");
        assert_eq!(super::display_in(d, &all[0], &all[..1]), "Desk", "only it in viewers.conf: no output");
        let gone = super::Viewer { machine: "nobody".into(), name: "x".into(), ..Default::default() };
        assert_eq!(super::display_in(d, &gone, &[]), "x", "no file");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn autoconnect_picks_the_workspace_line_or_the_default() {
        let t = "home\tdesk-wide,desk-portrait\nwork\twork-laptop\noffice\t\n*\tdesk-wide\n";
        assert_eq!(super::pick_autoconnect(t, "home").unwrap(), ["desk-wide", "desk-portrait"]);
        assert_eq!(super::pick_autoconnect(t, "office").unwrap(), Vec::<String>::new()); // none, since it's not on a known network
        assert_eq!(super::pick_autoconnect(t, "workspace-3").unwrap(), ["desk-wide"]);
        assert!(super::pick_autoconnect("home\ta\n", "elsewhere").is_none()); // no line and no default, so all of them
    }

    #[test]
    fn vcurve_spot_reads_as_a_vertical_curve_and_writes_back() {
        let v = serde_json::json!({"centre": [0.6, 1.3, -0.7], "yaw": -44.5, "pitch": 2.4, "roll": -0.9,
                                   "width": 0.393, "height": 0.698, "curve": 0, "vcurve": 1.09});
        let p = super::pose_from(&v).unwrap();
        assert!(p.vert && (p.curve - 1.09).abs() < 1e-9);
        let back = super::pose_json(&p, 0.698);
        assert_eq!((back["curve"].as_f64(), back["vcurve"].as_f64()), (Some(0.0), Some(1.09)));
        let flat = super::pose_from(&serde_json::json!({"centre": [0, 1, -1], "yaw": 0, "pitch": 0, "roll": 0,
                                                        "width": 1.2, "curve": 1.04})).unwrap();
        assert!(!flat.vert && (flat.curve - 1.04).abs() < 1e-9);
    }

    #[test]
    fn old_spots_become_the_default_workspace() {
        let old = serde_json::json!({"spots": {"home": {"desk-wide": {"width": 3.2}}}});
        let new = super::migrate(old);
        assert_eq!(new["workspace"], "default");
        assert_eq!(new["workspaces"]["default"]["spots"]["home"]["desk-wide"]["width"], 3.2);
        assert_eq!(super::migrate(new.clone()), new); // and stays as it is
    }
}
