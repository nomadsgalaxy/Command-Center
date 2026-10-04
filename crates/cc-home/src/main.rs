//! cc-home handles Command Center's spots, workspaces, machines, networks and the camera's align
//! (see USAGE). I ported it from home/cc-home.py, which is gone now. The output, exit codes and
//! files it writes match the Python byte for byte, as recorded in tests/fixtures/, and
//! tests/cross.rs checks that.
//!
//! Where things live:
//! - machine.rs: the monitors, their agents and pairing
//! - scan.rs: align, refit, calibrate and pairing by camera (cc-scan)
//! - hibernate.rs: the Desktop's open apps across a close
//! - session.rs: the Desktop's launch (desktop, panels, rest, session, box). These were shell
//!   scripts; their paths are now links to this binary, and it dispatches on argv[0].
//! - install.rs: the rest of install.sh
//!
//! It's a static musl binary so the launcher can run it on the Frame's host, and cc-panels can
//! run it in cc-box.
mod hibernate;
mod install;
mod machine;
mod scan;
mod session;

use cc_proto::conf::{self, Json, Viewer, py_round};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::PathBuf;
use std::sync::OnceLock;

pub fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".into())
}

pub fn cache(name: &str) -> String {
    format!("{}/.cache/control-center/{name}", home_dir())
}

fn conf_dir() -> PathBuf {
    PathBuf::from(home_dir()).join(".config/control-center")
}

/// Same as Python's SystemExit(msg): the message on stderr, then exit 1.
fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

/// home/cc-home.py's docstring. Printed with exit 2 when there's no command or an unknown one.
const USAGE: &str = r#"Command Center spots: saved places for the panels (cc-panels), in
the room (SteamVR's standing space), so they come back to the same real-world spot whatever
way you face. "home" is the layout cc-panels starts with and Right Ctrl + Home recalls. The
"scanned" spot puts each remote monitor's screen onto the real monitor it shows, at its real
size, measured by camera (or by touching its corners with a Frame controller). Spots are on
demand: panels stay free to move.

  cc-home save [spot]                     save where every screen is now (default: home)
  cc-home apply [spot]                    put the screens back there (default: home); another spot
                                          then becomes home, the old home kept as "previous" (undo:
                                          cc-home apply previous)
  cc-home scan [name ...] [--progress]    find the remote monitors in the room by camera (room
                                          view on, look at them): sets "scanned", curve included;
                                          --progress adds "@event monitor key=value" lines per step
                                          (prepare setup calibrate size capture solving placed
                                          skipped failed done)
  cc-home calibrate <name> [left|right]   touch the corners of the monitor <name> (from
                                          viewers.conf); saves it in "scanned" and puts it there
  cc-home list                            the saved spots (of the active workspace)
  cc-home forget <spot>                   delete a spot
  cc-home workspace [use|new|save-as|load|rename|forget ...|primary <machine>|-|known-network add|remove <name>|
                   machines <name> [add|remove <machine>]]   workspaces: each place's spots (home, work) and
                                          machines, picked by the room SteamVR tracks when cc-panels starts;
                                          a room none is bound to uses "temporary" (travelling): load <name>
                                          puts that workspace's layout in front of you there (docs/workspaces.md)
  cc-home workspace enter <name> [monitor ...]   back at that place, but SteamVR made a new room: look at
                                          one of its monitors (corner tags), and every spot moves with it
                                          and this room is added to it
  cc-home machine [list | discover [--wait S] | probe <user@host> | add <name> <user@host:port> [WxH] [key=val ...] |
                  set <name> key=val ... |
                  remove <name> | connect <name> | align <name> |
                  pair <addr> <key|-> [--replace] | pair --scan [addr] [--replace] | unpair <machine> |
                  rename <machine> <label> | rename <monitor> <label> --monitor | list --json |
                  session start|stop <monitor> | imu <machine> [seconds] | window list <machine> |
                  window start|stop|pop <monitor> <uuid>]    the remote monitors in viewers.conf (comments, columns
                                          and unknown options kept); options: curve=h|v|flat,
                                          radius=<m> (pins the curve's radius: 1.0 for 1000R),
                                          autoconnect=yes|no (absent = no); key= drops an option;
                                          align = scan it (what the config window's Align runs);
                                          pair = the key on the host's screen (cc-share pair) gets this
                                          Frame its own login: every shared monitor is added (docs/pairing.md);
                                          --scan reads the key off the host's screen with the camera, from the
                                          one host showing a key (or addr); rename: what you see (empty
                                          clears it); names, labels and machine ids all work as <name>;
                                          imu = a Steam Deck's orientation and rates, live, to wave it and watch
                                          (Ctrl-C ends it; docs/deck-tracking.md)
  cc-home network                         the networks the headset is on now (wifi name, wired connection name)
                                          and which workspaces know them
  cc-home autoconnect --write             for the launcher, just before cc-panels starts: writes
                                          ~/.cache/control-center/autoconnect.txt and workspace-hint.txt
  cc-home autoconnect [--all] [name ...]  the monitors to connect at Desktop launch (names, one per line):
                                          only on a known network of the workspace (workspace known-network
                                          add); only monitors marked autoconnect=yes.
                                          --all: "workspace<TAB>monitor,monitor" for every workspace
  cc-home workspace-for [name ...]        the workspace the current network points to (when SteamVR can't
                                          tell the place): the active one if it knows it, else the only one
  cc-home reanchor <monitor>              the room moved (SteamVR re-anchored its tracking): move every
                                          spot the way <monitor> moved between its last two scans
  cc-home refit                           fit the VR mirror camera again from a tag board shown 1 m ahead
                                          (look at it from three spots); kept only if it fits well
  cc-home selftest                        check the geometry

Saved in ~/.config/control-center/home.json, per workspace. If SteamVR's room origin moves (room setup
redone), save or calibrate again."#;

/// Just the machine part, for `cc-home machine` with a subcommand it doesn't know (stderr, exit 1).
fn machine_usage() -> &'static str {
    let (a, b) = (USAGE.find("  cc-home machine").unwrap_or(0), USAGE.find("  cc-home selftest").unwrap_or(USAGE.len()));
    &USAGE[a..b]
}

fn usage() -> ! {
    println!("{USAGE}");
    std::process::exit(2)
}

/// A known panel's corners put through pose_from_corners have to give the panel back, the refit
/// board has to stand ahead of the head, and a room move has to move a pose along with it.
fn selftest() {
    let panel_axes = |yaw: f64, pitch: f64, roll: f64| -> (V3, V3) {
        let (y, p) = (yaw * DEG, pitch * DEG);
        let z = [y.sin() * p.cos(), -p.sin(), y.cos() * p.cos()];
        let x0 = normalize([z[2], 0.0, -z[0]]);
        let y0 = cross(z, x0);
        let (c, s) = ((roll * DEG).cos(), (roll * DEG).sin());
        ([0, 1, 2].map(|i| x0[i] * c + y0[i] * s), [0, 1, 2].map(|i| y0[i] * c - x0[i] * s))
    };
    for (yaw, pitch, roll) in [(0.0, 0.0, 0.0), (35.0, -10.0, 0.0), (-120.0, 25.0, 7.0), (90.0, 0.0, -90.0)] {
        let (x, y) = panel_axes(yaw, pitch, roll);
        let (c, w, h) = ([0.3, 1.2, -0.8], 0.6, 0.34);
        let tl: V3 = [0, 1, 2].map(|i| c[i] - x[i] * w / 2.0 + y[i] * h / 2.0);
        let tr: V3 = [0, 1, 2].map(|i| tl[i] + x[i] * w);
        let bl: V3 = [0, 1, 2].map(|i| tl[i] - y[i] * h);
        let p = scan::pose_from_corners(tl, tr, bl);
        assert!(centre(&p).unwrap().iter().zip(c).all(|(a, b)| (a - b).abs() < 1e-3), "{p:?}");
        assert!((num(&p, "width") - w).abs() < 1e-3 && (num(&p, "height") - h).abs() < 1e-3, "{p:?}");
        assert!((num(&p, "corner_deg") - 90.0).abs() < 0.1, "{p:?}");
        let (x2, _) = panel_axes(num(&p, "yaw"), num(&p, "pitch"), num(&p, "roll"));
        assert!(x2.iter().zip(x).all(|(a, b)| (a - b).abs() < 1e-2), "{p:?} {yaw}");
    }
    let (c, x, _, z) = scan::board_pose([[1.0, 0.0, 0.0, 0.2], [0.0, 1.0, 0.0, 1.6], [0.0, 0.0, 1.0, -0.3]], 1.0); // looking down -z
    assert!(c.iter().zip([0.2, 1.6, -1.3]).all(|(a, b)| (a - b).abs() < 1e-9) && x == [1.0, 0.0, 0.0] && z == [0.0, 0.0, 1.0], "{c:?} {x:?} {z:?}");
    let pose = |c: [f64; 3], yaw: f64, pitch: f64| Json::parse(&format!("{{\"centre\": [{}, {}, {}], \"yaw\": {yaw}, \"pitch\": {pitch}, \"roll\": 0}}", c[0], c[1], c[2])).unwrap();
    let (a, b) = (pose([0.0, 1.5, -1.0], 0.0, 0.0), pose([0.1, 1.5, -0.9], 90.0, 0.0));
    let p = moved(&pose([1.0, 1.2, -1.0], 10.0, 5.0), &a, &b);
    assert!(centre(&p).unwrap().iter().zip([0.1, 1.2, -1.9]).all(|(u, v)| (u - v).abs() < 1e-3) && num(&p, "yaw") == 100.0 && num(&p, "pitch") == 5.0, "{p:?}");
    assert!((room_move(&a, &b).1 - 90.0).abs() < 1e-9 && (room_move(&b, &a).1 - 90.0).abs() < 1e-9);
    println!("selftest ok");
}

fn viewers() -> &'static [Viewer] {
    static V: OnceLock<Vec<Viewer>> = OnceLock::new();
    V.get_or_init(|| conf::viewers(&conf_dir()))
}

// ---- home.json

fn read_all() -> Json {
    conf::read_home(&conf_dir().join("home.json")).unwrap_or_else(|e| die(e))
}

fn write_all(data: &Json) {
    if let Err(e) = conf::write_home(&conf_dir().join("home.json"), data) {
        die(format!("can't write home.json: {e}"));
    }
}

fn active(data: &Json) -> String {
    data.at("workspace").str().unwrap_or("default").to_owned()
}

/// The active workspace's spots.
fn load() -> Json {
    let data = read_all();
    data.at("workspaces").at(&active(&data)).get("spots").cloned().unwrap_or_else(Json::obj)
}

fn store(spots: Json) {
    let mut data = read_all();
    let a = active(&data);
    data.setdefault("workspaces", Json::obj()).setdefault(&a, Json::obj()).set("spots", spots);
    write_all(&data);
}

/// Spots are keyed by viewer name. Older ones used the viewer's screen number (viewers.conf) instead.
fn key_name(key: &str) -> String {
    match key.parse::<i64>() {
        Ok(n) if key.bytes().all(|c| c.is_ascii_digit()) => {
            viewers().iter().find(|v| v.screen == n).map_or(format!("screen {n}"), |v| v.name.clone())
        }
        _ => key.into(),
    }
}

/// Save under the viewer name, dropping an older screen-number entry for the same screen.
fn store_pose(spots: &mut Json, spot: &str, name: &str, pose: Json) {
    let poses = spots.setdefault(spot, Json::obj());
    if let Some(v) = viewers().iter().find(|v| v.name == name) {
        poses.remove(&v.screen.to_string());
    }
    poses.set(name, pose);
}

// ---- pose math, same as cc-home.py's (Python's sum(), degrees() and round(), so the same digits come out)

type V3 = [f64; 3];

/// Python 3.12+'s sum() of floats, which is Neumaier's compensated sum.
fn psum(xs: V3) -> f64 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for x in xs {
        let t = s + x;
        c += if s.abs() >= x.abs() { (s - t) + x } else { (x - t) + s };
        s = t;
    }
    if c != 0.0 && c.is_finite() { s + c } else { s }
}

fn dot(a: V3, b: V3) -> f64 {
    psum([a[0] * b[0], a[1] * b[1], a[2] * b[2]])
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(v: V3) -> f64 {
    match dot(v, v).sqrt() {
        0.0 => 1.0,
        n => n,
    }
}

fn normalize(v: V3) -> V3 {
    let n = norm(v);
    v.map(|c| c / n)
}

const DEG: f64 = std::f64::consts::PI / 180.0;

fn degrees(x: f64) -> f64 {
    x / DEG
}

/// cc-panels' place arguments for a panel with this centre, right axis x and front z.
fn pose_from_axes(centre: V3, x: V3, z: V3, width: f64, height: f64) -> Json {
    let f = z.map(|c| -c); // the direction you look to see the front
    let yaw = degrees(f64::atan2(-f[0], -f[2]));
    let pitch = degrees((f[1] / norm(f)).clamp(-1.0, 1.0).asin());
    let x0 = normalize([z[2], 0.0, -z[0]]); // roll as PanelPose defines it
    let y0 = cross(z, x0);
    let roll = degrees(f64::atan2(dot(x, y0), dot(x, x0)));
    let mut p = Json::obj();
    p.set("centre", Json::Arr(centre.iter().map(|c| Json::float(py_round(*c, 4))).collect()));
    for (k, v, n) in [("yaw", yaw, 3), ("pitch", pitch, 3), ("roll", roll, 3), ("width", width, 4), ("height", height, 4)] {
        p.set(k, Json::float(py_round(v, n)));
    }
    p
}

fn centre(pose: &Json) -> Option<V3> {
    match pose.at("centre").list() {
        [a, b, c] => Some([a.num()?, b.num()?, c.num()?]),
        _ => None,
    }
}

fn num(pose: &Json, k: &str) -> f64 {
    pose.at(k).num().unwrap_or(0.0)
}

/// The upright rigid move that takes pose a to pose b: how far its centre went (mm) and how far
/// it turned (deg).
fn room_move(a: &Json, b: &Json) -> (f64, f64) {
    let (ca, cb) = (centre(a).unwrap_or_default(), centre(b).unwrap_or_default());
    let d = [cb[0] - ca[0], cb[1] - ca[1], cb[2] - ca[2]];
    ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 1000.0, ((num(b, "yaw") - num(a, "yaw") + 180.0).rem_euclid(360.0) - 180.0).abs())
}

/// Moves pose the same way a went to b: turned about vertical by b's yaw change around a's centre,
/// then shifted so a's centre lands on b's. A relocalization keeps gravity, so pitch and roll
/// stay as they are.
fn moved(pose: &Json, a: &Json, b: &Json) -> Json {
    let turn = (num(b, "yaw") - num(a, "yaw")) * DEG;
    let (c, s) = (turn.cos(), turn.sin());
    let (p, ca, cb) = (centre(pose).unwrap_or_default(), centre(a).unwrap_or_default(), centre(b).unwrap_or_default());
    let (x, y, z) = (p[0] - ca[0], p[1] - ca[1], p[2] - ca[2]);
    let mut out = pose.clone();
    let at = [cb[0] + x * c + z * s, cb[1] + y, cb[2] - x * s + z * c];
    out.set("centre", Json::Arr(at.iter().map(|v| Json::float(py_round(*v, 4))).collect()));
    out.set("yaw", Json::float(py_round(num(pose, "yaw") + num(b, "yaw") - num(a, "yaw"), 3)));
    out
}

// ---- cc-panels' control socket (@controlcenter: we send a datagram, it replies to our own address)

/// A socket error worded the way Python says it ("timed out", "[Errno 111] Connection refused").
fn py_err(e: &std::io::Error) -> String {
    match (e.kind(), e.raw_os_error()) {
        (std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut, _) => "timed out".into(),
        (_, Some(n)) => format!("[Errno {n}] {}", e.to_string().trim_end_matches(&format!(" (os error {n})"))),
        _ => e.to_string(),
    }
}

/// @controlcenter, unless CC_PANELS_SOCKET names another one (that's how tests/cross.rs reaches its
/// fake cc-panels).
fn panels_name() -> Vec<u8> {
    std::env::var("CC_PANELS_SOCKET").unwrap_or_else(|_| "controlcenter".into()).into_bytes()
}

struct Panels(UnixDatagram);

impl Panels {
    fn ask(&self, text: &str, timeout: f64) -> Result<String, String> {
        let to = SocketAddr::from_abstract_name(panels_name()).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 65536];
        let n = self.0.set_read_timeout(Some(std::time::Duration::from_secs_f64(timeout)))
            .and_then(|_| self.0.send_to_addr(text.as_bytes(), &to))
            .and_then(|_| self.0.recv(&mut buf))
            .map_err(|e| format!("cc-panels didn't answer ({})", py_err(&e)))?;
        let reply = String::from_utf8_lossy(&buf[..n]).into_owned();
        if reply.starts_with("ok") { Ok(reply) } else { Err(reply) }
    }
}

/// cc-panels' control socket when it's running, else None.
fn panels_socket() -> Option<Panels> {
    let me = SocketAddr::from_abstract_name(format!("cc-home-{}", std::process::id()).as_bytes()).ok()?;
    let p = Panels(UnixDatagram::bind_addr(&me).ok()?);
    p.ask("panels", 1.0).ok().map(|_| p)
}

fn target() -> Panels {
    panels_socket().unwrap_or_else(|| die("cc-panels isn't running (start it, or click Desktop in VR)"))
}

fn place(sock: &Panels, key: &str, pose: &Json) -> Result<(), String> {
    let name = key_name(key);
    let c = centre(pose).ok_or("no place saved")?;
    let mut height = if pose.at("height").truthy() { format!(" {:.4}", num(pose, "height")) } else { String::new() };
    if pose.at("vcurve").truthy() {
        height = format!(" {:.4} {:.3}", num(pose, "height"), num(pose, "vcurve"));
    }
    sock.ask(&format!("place {name} {:.4} {:.4} {:.4} {:.3} {:.3} {:.3} {:.4} {:.3}{height}", c[0], c[1], c[2],
                      num(pose, "yaw"), num(pose, "pitch"), num(pose, "roll"), num(pose, "width"), num(pose, "curve")), 10.0)
        .map(|_| ())
}

/// Where every panel is now, keyed by viewer name.
fn current_poses(sock: &Panels) -> Json {
    let reply = sock.ask("panels", 10.0).unwrap_or_else(|e| die(e));
    let mut poses = Json::obj();
    for line in reply.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let Some(v) = f.get(1..).and_then(|v| v.iter().map(|x| x.parse::<f64>().ok()).collect::<Option<Vec<f64>>>()) else { continue };
        if v.len() < 12 {
            continue;
        }
        let mut p = pose_from_axes([v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]], v[9], v[10]);
        p.set("curve", Json::float(py_round(v[11], 3)));
        if v.len() > 12 && v[12] > 0.0 {
            p.set("vcurve", Json::float(py_round(v[12], 3))); // a top-to-bottom curve
        }
        poses.set(f[0], p);
    }
    poses
}

// ---- the commands

fn save(spot: &str) {
    let sock = target();
    let mut spots = load();
    let poses = current_poses(&sock);
    for (name, pose) in poses.items() {
        store_pose(&mut spots, spot, name, pose.clone());
    }
    store(spots);
    println!("saved {} panel(s) as {spot}", poses.items().len());
}

/// What was placed becomes home, since cc-panels starts from home, and the rest of home stays. The
/// old home is kept as the spot "previous", so cc-home apply previous gives one level of undo.
fn make_home(spots: &mut Json, placed: &Json) {
    let old = spots.get("home").cloned().unwrap_or_else(Json::obj);
    spots.set("previous", old);
    for (name, pose) in placed.items() {
        store_pose(spots, "home", name, pose.clone());
    }
}

fn apply(spot: &str) {
    let mut spots = load();
    let poses = spots.at(spot).clone();
    if !poses.truthy() {
        die(format!("no spot {spot} (cc-home list)"));
    }
    let (sock, mut placed) = (target(), Json::obj());
    for (key, pose) in poses.items() {
        match place(&sock, key, pose) {
            Ok(()) => placed.set(&key_name(key), pose.clone()),
            Err(e) => println!("{}: {e}", key_name(key)),
        }
    }
    let n = placed.items().len();
    if spot != "home" && n > 0 {
        make_home(&mut spots, &placed);
        store(spots);
    }
    println!("{spot}: {n} placed on our panels{}", if spot != "home" && n > 0 { ", now home (cc-home apply previous undoes)" } else { "" });
}

/// Moves every spot in the workspace the way <name> moved between its last two scans. That's for
/// when the room moved because SteamVR relocalized. The monitors placed in that align stay put,
/// the old home is kept as "previous", and then everything is placed again.
fn reanchor(name: &str) {
    let mut spots = load();
    let (a, b) = (spots.at("before-align").at(name).clone(), spots.at("scanned").at(name).clone());
    if !(a.truthy() && b.truthy()) {
        die(format!("{name} has no earlier scan to compare (align it first)"));
    }
    let fresh: Vec<String> = spots.at("before-align").items().iter().map(|(k, _)| k.clone()).collect(); // placed by that align, so already right
    let home = spots.get("home").cloned().unwrap_or_else(Json::obj);
    spots.set("previous", home);
    for (spot, poses) in spots.items_mut().iter_mut() {
        if spot == "previous" || spot == "before-align" {
            continue;
        }
        if let Json::Obj(m) = poses {
            for (key, pose) in m.iter_mut() {
                if !fresh.contains(&key_name(key)) && pose.get("centre").is_some() && pose.get("yaw").is_some() {
                    *pose = moved(pose, &a, &b);
                }
            }
        }
    }
    spots.remove("before-align"); // done, so a second reanchor doesn't move everything twice
    let home = spots.at("home").clone();
    store(spots);
    let (mm, deg) = room_move(&a, &b);
    let placed = panels_socket().map_or(0, |sock| home.items().iter().filter(|(k, p)| place(&sock, k, p).is_ok()).count());
    println!("moved everything {mm:.0} mm and {deg:.1} deg with {name}; {placed} placed now; cc-home apply previous undoes it");
}

fn list() {
    let spots = load();
    let mut all: Vec<&(String, Json)> = spots.items().iter().collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    for (spot, poses) in all {
        let mut ps: Vec<&(String, Json)> = poses.items().iter().collect();
        ps.sort_by(|a, b| a.0.cmp(&b.0));
        let each: Vec<String> = ps.iter().map(|(k, p)| format!("{} ({:.0} cm)", key_name(k), num(p, "width") * 100.0)).collect();
        println!("{spot}: {}", each.join(", "));
    }
}

fn forget(spot: &str) {
    let mut spots = load();
    match spots.remove(spot) {
        None | Some(Json::Null) => die(format!("no spot {spot}")),
        Some(_) => store(spots),
    }
}

const WORKSPACE_USAGE: &str = "cc-home workspace [use <name> | new <name> | save-as <name> | load <name> | enter <name> [monitor ...] | rename <old> <new> | forget <name> | machines <name> [add|remove <machine>]]";

/// cc-panels' room (its SteamVR universe) when it's running, else 0 (none).
fn panels_universe() -> u64 {
    panels_socket().and_then(|s| s.ask("universe", 2.0).ok()).and_then(|r| r.split_whitespace().nth(1)?.parse().ok()).unwrap_or(0)
}

/// Where the head is and which way it faces, from cc-panels (its yaw: 0 looks down -z).
fn head_pose(sock: &Panels) -> ([f64; 3], f64) {
    let r = sock.ask("head", 2.0).unwrap_or_else(|e| die(e));
    let n: Vec<f64> = r.split_whitespace().skip(1).take(12).filter_map(|v| v.parse().ok()).collect();
    if n.len() < 12 {
        die(format!("cc-panels' head: {r}"));
    }
    ([n[3], n[7], n[11]], degrees(f64::atan2(n[2], n[10])))
}

/// Tells cc-panels, if it's running, to take the active workspace again: its machines and its
/// spots, live.
fn panels_reload() {
    if let Some(sock) = panels_socket()
        && let Err(e) = sock.ask("workspace reload", 10.0)
    {
        println!("cc-panels: {e}");
    }
}

fn workspace(args: &[&str]) {
    let mut data = read_all();
    let act = active(&data);
    let cmd = args.first().copied().unwrap_or("");
    let mut live = false; // whether cc-panels needs to take the active workspace again
    match (cmd, args.len()) {
        ("enter", 2..) => return scan::enter(args[1], &args[2..]), // it writes home.json itself, after the scan
        ("use", 2) => {
            conf::use_workspace(&mut data, args[1]).unwrap_or_else(|e| die(e));
            live = true;
        }
        // A new dedicated workspace from the active one's layout and machines. new leaves it
        // unbound (it gets bound in the next new room it starts in); save-as binds it to
        // cc-panels' room.
        ("new" | "save-as", 2) => {
            let room = if cmd == "save-as" { panels_universe() } else { 0 };
            conf::save_workspace_as(&mut data, args[1], room).unwrap_or_else(|e| die(e));
        }
        // Travelling: its layout goes in front of you, in Temporary, and the workspace itself stays as it is.
        ("load", 2) => {
            let sock = target();
            let (head, yaw) = head_pose(&sock);
            let primary = data.at("workspaces").at(args[1]).at("primary").str().unwrap_or("").to_owned();
            let anchors: Vec<String> = viewers().iter().filter(|v| !primary.is_empty() && v.machine == primary).map(|v| v.name.clone()).collect();
            let n = conf::load_workspace(&mut data, args[1], head, yaw, &anchors).unwrap_or_else(|e| die(e));
            println!("loaded {} here: {n} panel(s), in Temporary ({} itself unchanged)", args[1], args[1]);
            live = true;
        }
        ("rename", 3) => conf::rename_workspace(&mut data, args[1], args[2]).unwrap_or_else(|e| die(e)),
        ("forget", 2) => conf::forget_workspace(&mut data, args[1]).unwrap_or_else(|e| die(e)),
        ("machines", 2) => {
            if data.at("workspaces").get(args[1]).is_none() {
                die(format!("no workspace {}", args[1]));
            }
            let mut ms: Vec<&str> = viewers().iter().map(|v| v.machine.as_str()).collect();
            ms.dedup();
            let all = conf::workspace_machines(&data, args[1]).is_none();
            let hosts = conf::trusted_hosts(&conf_dir());
            for m in ms {
                let label = hosts.iter().find(|(k, _)| k == m).and_then(|(_, t)| t["label"].as_str()).filter(|l| !l.is_empty()).map_or(String::new(), |l| format!(" ({l})"));
                println!("{} {m}{label}", if conf::is_member(&data, args[1], m) { '*' } else { ' ' });
            }
            println!("{}", if all { "(no list: every machine)" } else { "(* in it)" });
            return;
        }
        ("machines", 4) if args[2] == "add" || args[2] == "remove" => {
            let hosts = conf::trusted_hosts(&conf_dir());
            let m = conf::find_machine(args[3], viewers(), &hosts).unwrap_or_else(|| die(format!("no machine {} (cc-home machine list)", args[3])));
            let mut all: Vec<String> = viewers().iter().map(|v| v.machine.clone()).collect();
            all.dedup();
            conf::set_member(&mut data, args[1], &m, args[2] == "add", &all).unwrap_or_else(|e| die(e));
            live = args[1] == act;
        }
        ("primary", 1 | 2) => {
            // the machine this place is anchored on, by its id, label, host name or a monitor (D-050)
            let mut m = args.get(1).map(|s| s.to_string());
            if let Some(x) = m.clone().filter(|x| x != "-") {
                let hosts = conf::trusted_hosts(&conf_dir());
                if let Some(found) = conf::find_machine(&x, viewers(), &hosts) {
                    m = Some(found);
                }
                let x = m.clone().unwrap_or_default();
                if !viewers().iter().any(|v| v.machine == x) {
                    die(format!("no machine {x} (cc-home machine list; the machine= option names it, else it is the host)"));
                }
            }
            let w = data.get_mut("workspaces").and_then(|ws| ws.get_mut(&act)).unwrap_or_else(|| die(format!("no workspace {act}")));
            match m.as_deref() {
                Some("-") => drop(w.remove("primary")),
                Some(x) => w.set("primary", Json::Str(x.into())),
                None => {}
            }
        }
        ("known-network", 3) if args[1] == "add" || args[1] == "remove" => {
            // the networks that mean this place
            let w = data.get_mut("workspaces").and_then(|ws| ws.get_mut(&act)).unwrap_or_else(|| die(format!("no workspace {act}")));
            let names = w.setdefault("known_networks", Json::Arr(Vec::new()));
            let net = Json::Str(args[2].into());
            if let Json::Arr(a) = names {
                match a.iter().position(|n| *n == net) {
                    None if args[1] == "add" => a.push(net),
                    Some(i) if args[1] == "remove" => drop(a.remove(i)),
                    _ => {}
                }
            }
            if !w.at("known_networks").truthy() {
                w.remove("known_networks");
            }
        }
        ("", _) => {}
        _ => die(WORKSPACE_USAGE),
    }
    if !cmd.is_empty() {
        write_all(&data);
    }
    if live {
        panels_reload();
    }
    let mut all: Vec<&(String, Json)> = data.at("workspaces").items().iter().collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, w) in all {
        let rooms = conf::rooms(w);
        let room = if !rooms.is_empty() { format!("room {}", rooms.join(", room ")) } else if name == conf::TEMPORARY { "any room".into() } else { "no room yet".into() };
        let mut anchor = String::new();
        if w.at("primary").truthy() {
            anchor += &format!(", primary {}", w.at("primary").text());
        }
        if w.at("known_networks").truthy() {
            anchor += &format!(", known networks {}", w.at("known_networks").list().iter().map(Json::text).collect::<Vec<_>>().join(", "));
        }
        let mut spots: Vec<&str> = w.at("spots").items().iter().map(|(k, _)| k.as_str()).collect();
        spots.sort();
        let spots = if spots.is_empty() { "no spots".into() } else { spots.join(", ") };
        if let Some(ms) = conf::workspace_machines(&data, name) {
            anchor += &format!(", machines {}", if ms.is_empty() { "none".into() } else { ms.join(", ") });
        }
        if name == conf::TEMPORARY {
            anchor += ", Temporary: for travelling";
        }
        let star = if data.at("workspace").str() == Some(name) { '*' } else { ' ' };
        println!("{star} {name}: {spots} ({room}{anchor})");
    }
}

// ---- networks

/// `nmcli -t <args>`'s lines, split at unescaped colons. An error isn't the same as "no network",
/// because callers (the launcher) fall back on it.
fn nmcli(args: &[&str]) -> Result<Vec<Vec<String>>, String> {
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    let pre = "can't read the network name";
    let mut child = std::process::Command::new("nmcli").arg("-t").args(args)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().map_err(|e| format!("{pre}: {}: 'nmcli'", py_err(&e)))?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed().as_secs_f64() < 5.0 => std::thread::sleep(std::time::Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let argv: Vec<String> = ["nmcli", "-t"].iter().chain(args).map(|a| format!("'{a}'")).collect();
                return Err(format!("{pre}: Command '[{}]' timed out after 5 seconds", argv.join(", ")));
            }
        }
    };
    let (mut out, mut err) = (String::new(), String::new());
    let _ = child.stdout.take().map(|mut o| o.read_to_string(&mut out));
    let _ = child.stderr.take().map(|mut o| o.read_to_string(&mut err));
    if !status.success() {
        let code = status.code().unwrap_or_else(|| -status.signal().unwrap_or(0));
        return Err(format!("{pre}: nmcli says {}", if err.trim().is_empty() { code.to_string() } else { err.trim().to_owned() }));
    }
    Ok(out.lines().map(|l| l.replace("\\:", "\0").split(':').map(str::to_owned).collect()).collect())
}

/// The networks the headset is on, from NetworkManager: the wifi name, plus the connection name
/// of a wired link, since a hardlined headset has no wifi. Empty when offline.
fn current_networks() -> Result<Vec<String>, String> {
    let pick = |rows: Vec<Vec<String>>, want: &str| -> Vec<String> {
        rows.into_iter().filter(|f| f[0] == want).filter_map(|f| f.get(1).map(|n| n.replace('\0', ":"))).collect()
    };
    let wifi = pick(nmcli(&["-f", "active,ssid", "dev", "wifi"])?, "yes");
    let wired = pick(nmcli(&["-f", "type,name", "connection", "show", "--active"])?, "802-3-ethernet");
    Ok([wifi, wired].concat())
}

fn knows(w: &Json, nets: &[String]) -> bool {
    w.at("known_networks").list().iter().any(|n| n.str().is_some_and(|n| nets.iter().any(|x| x == n)))
}

/// The monitors to connect at launch. Only the ones marked autoconnect=yes, and only on one of
/// the workspace's known networks (name defaults to the active one; no list means no gate).
fn autoconnect_list(nets: &[String], data: &Json, name: Option<&str>) -> Vec<String> {
    let name = name.filter(|n| !n.is_empty()).map(str::to_owned).or_else(|| data.at("workspace").str().map(str::to_owned));
    let w = name.as_ref().map_or(&Json::Null, |n| data.at("workspaces").at(n));
    if w.at("known_networks").truthy() && !knows(w, nets) {
        return vec![];
    }
    // only its machines (with no list, all of them)
    marked().into_iter().filter(|m| viewers().iter().find(|v| v.name == *m).is_none_or(|v| conf::is_member(data, name.as_deref().unwrap_or(""), &v.machine))).collect()
}

/// The monitors marked autoconnect=yes. Missing counts as no.
fn marked() -> Vec<String> {
    viewers().iter().filter(|v| v.opt("autoconnect").unwrap_or("no") == "yes").map(|v| v.name.clone()).collect()
}

/// The workspace these networks point to: the active one if it knows one of them, or else the
/// only workspace that does.
fn workspace_for(nets: &[String], data: &Json) -> Option<String> {
    let known: Vec<&String> = data.at("workspaces").items().iter().filter(|(_, w)| knows(w, nets)).map(|(n, _)| n).collect();
    match data.at("workspace").str() {
        Some(a) if known.iter().any(|n| *n == a) => Some(a.into()),
        _ => (known.len() == 1).then(|| known[0].clone()),
    }
}

fn any_known(data: &Json, names: &[Option<String>]) -> bool {
    names.iter().any(|n| n.as_ref().is_some_and(|n| data.at("workspaces").at(n).at("known_networks").truthy()))
}

/// Gets things ready for cc-panels' next start (the launcher runs this right before it). Writes
/// autoconnect.txt, the monitors per workspace ("name<TAB>a,b", "*" for a workspace made at that
/// start), and workspace-hint.txt, the workspace the network points to. If the network can't be
/// read, every workspace gets only the monitors marked autoconnect=yes, ungated, never all of them.
fn write_autoconnect() {
    let data = read_all();
    let yes = marked().join(",");
    let names: Vec<String> = data.at("workspaces").items().iter().map(|(n, _)| n.clone()).collect();
    let nets = if names.iter().any(|n| data.at("workspaces").at(n).at("known_networks").truthy()) { current_networks() } else { Ok(vec![]) };
    let (mut lines, hint) = match nets {
        Ok(nets) => (names.iter().map(|n| format!("{n}\t{}", autoconnect_list(&nets, &data, Some(n)).join(","))).collect::<Vec<_>>(),
                     workspace_for(&nets, &data).unwrap_or_default()),
        Err(e) => {
            eprintln!("autoconnect: {e}; only the monitors marked autoconnect=yes, network not checked");
            (names.iter().map(|n| format!("{n}\t{yes}")).collect(), String::new())
        }
    };
    lines.push(format!("*\t{yes}")); // a new room's workspace, which has no known networks yet
    let _ = std::fs::create_dir_all(cache(""));
    for (name, text) in [("autoconnect.txt", lines.join("\n") + "\n"), ("workspace-hint.txt", format!("{hint}\n"))] {
        let tmp = cache(&format!("{name}.tmp"));
        if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, cache(name))) {
            die(format!("can't write {}: {e}", cache(name)));
        }
    }
    println!("{}{}", lines.join("\n"), if hint.is_empty() { String::new() } else { format!("\nhint {hint}") });
}

/// `cc-home autoconnect [--all] [network ...]`: exit 0 with no output means "connect none".
fn autoconnect(args: &[&str]) {
    let data = read_all();
    let all = args.contains(&"--all");
    let mut nets: Vec<String> = args.iter().filter(|a| !a.starts_with("--")).map(|a| a.to_string()).collect();
    let names: Vec<Option<String>> =
        if all { data.at("workspaces").items().iter().map(|(n, _)| Some(n.clone())).collect() } else { vec![data.at("workspace").str().map(str::to_owned)] };
    if nets.is_empty() && any_known(&data, &names) {
        nets = current_networks().unwrap_or_else(|e| die(e)); // only asked when some workspace needs it
    }
    for n in &names {
        let got = autoconnect_list(&nets, &data, n.as_deref());
        if all {
            println!("{}\t{}", n.as_deref().unwrap_or(""), got.join(","));
        } else if !got.is_empty() {
            println!("{}", got.join("\n"));
        }
    }
}

/// `cc-home workspace-for [network ...]`: nothing printed (exit 0) when there's none to suggest.
fn workspace_for_cmd(args: &[&str]) {
    let data = read_all();
    if !data.at("workspaces").items().iter().any(|(_, w)| w.at("known_networks").truthy()) {
        return;
    }
    let nets = if args.is_empty() { current_networks().unwrap_or_else(|e| die(e)) } else { args.iter().map(|a| a.to_string()).collect() };
    if let Some(w) = workspace_for(&nets, &data) {
        println!("{w}");
    }
}

fn network() {
    let data = read_all();
    let nets = current_networks().unwrap_or_else(|e| die(e));
    println!("networks: {}", if nets.is_empty() { "none".into() } else { nets.join(", ") });
    let mut all: Vec<&(String, Json)> = data.at("workspaces").items().iter().collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, w) in all {
        if knows(w, &nets) {
            println!("  known to workspace {name}{}", if data.at("workspace").str() == Some(name) { " (active)" } else { "" });
        }
    }
}

fn main() {
    // Run as session/cc-launch, cc-panels, cc-box ... (links to us), or as cc-home desktop|panels|rest|session|box|install.
    let mut raw = std::env::args_os();
    let name = raw.next().and_then(|a| std::path::Path::new(&a).file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let name = if name.starts_with("cc-home") { "cc-home".into() } else { name };
    if let Some(code) = session::dispatch(&name, &raw.collect::<Vec<_>>()) {
        std::process::exit(code)
    }
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) }; // so `cc-home list | head` exits quietly, like a shell tool
    let argv: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    scan::PROGRESS.store(argv.iter().any(|a| a == "--progress"), std::sync::atomic::Ordering::Relaxed); // scan and align print the window's step lines too
    let argv: Vec<&str> = argv.iter().map(String::as_str).filter(|a| *a != "--progress").collect();
    let (cmd, args) = argv.split_first().map_or(("", &[][..]), |(c, a)| (*c, a));
    let first = args.first().copied();
    match (cmd, args.len()) {
        ("save", _) => save(first.unwrap_or("home")),
        ("apply", _) => apply(first.unwrap_or("home")),
        ("list", _) => list(),
        ("forget", 1..) => forget(args[0]),
        ("reanchor", 1) => reanchor(args[0]),
        ("workspace", _) => workspace(args),
        ("workspace-for", _) => workspace_for_cmd(args),
        ("autoconnect", _) if args.contains(&"--write") => write_autoconnect(),
        ("autoconnect", _) => autoconnect(args),
        ("network", _) => network(),
        ("machine", _) => machine::main(args),
        ("hibernate", _) => hibernate::main(first).unwrap_or_else(|e| die(e)),
        ("scan", _) => scan::scan(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>()),
        ("calibrate", 1..) => scan::calibrate(args[0], args.get(1).copied().unwrap_or("any")),
        ("refit", _) => scan::refit(),
        ("selftest", _) => selftest(),
        _ => usage(),
    }
}
