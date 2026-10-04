//! The commands that use the camera, ported from home/cc-home.py's scan, refit, calibrate and
//! pair_scan, and built on cc-scan:
//! - align of remote monitors (scan / machine align): every chosen monitor shows its tags through
//!   its machine's agent, the headset's mirror reads them, one fit covers all of them, and they get
//!   placed and made home
//! - refit of the mirror camera from a tag board
//! - calibrate: a monitor's corners touched with a controller's tip
//! - pairing by reading the key off the host's screen
//!
//! Output, progress lines and files match cc-home.py's.
use super::{V3, cache, conf_dir, cross, degrees, die, dot, load, make_home, normalize, place, pose_from_axes, room_move, store, store_pose, target, viewers, Panels};
use crate::machine::{self, Fail, args, py_text, truthy};
use crate::ssh;
use cc_proto::agent::Client;
use cc_proto::conf::{Json, py_float, py_round};
use cc_scan::scan::{Answer, Scan};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub static PROGRESS: AtomicBool = AtomicBool::new(false);

/// One machine-readable line per step of a scan, with --progress. cc-panels' Machines window logs
/// them and offers a reanchor on @reanchor. The lines: @mode, @prepare (step=monitor|viewer|tags),
/// @setup, @calibrate, @size, @capture (pct samples baseline_cm coverage), @solving, @placed,
/// @skipped, @failed, @reanchor and @done (total_s and each phase's seconds).
pub fn progress(event: &str, name: &str, kv: &[(&str, String)]) {
    if PROGRESS.load(Relaxed) {
        println!("@{event} {name}{}", kv.iter().map(|(k, v)| format!(" {k}={v}")).collect::<String>());
    }
}

/// Python's round(x) of a float: an int, with ties going to even.
fn iround(x: f64) -> i64 {
    x.round_ties_even() as i64
}

fn secs(t: Instant) -> f64 {
    t.elapsed().as_secs_f64()
}

/// Other programs holding the VR mirror camera open (live, a web page in a browser had it), not
/// counting SteamVR's own writer (V4L2Cam). This only reads their fds' links and never opens the
/// device itself.
fn camera_users() -> Vec<String> {
    let mut names: Vec<String> = vec![];
    for pid in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let p = pid.path();
        if !pid.file_name().to_string_lossy().bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let holds = std::fs::read_dir(p.join("fd")).into_iter().flatten().flatten()
            .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|l| l == Path::new(cc_scan::camera::DEVICE)));
        if let Some(n) = holds.then(|| std::fs::read_to_string(p.join("comm")).ok()).flatten() {
            names.push(n.trim().to_owned());
        }
    }
    names.sort();
    names.dedup();
    names.retain(|n| n != "V4L2Cam");
    names
}

fn camera_stopped(e: &str) -> String {
    let users = camera_users();
    format!("{e}\nthe camera stopped{}", if users.is_empty() { String::new() } else { format!(": it's in use by {}", users.join(", ")) })
}

/// What align needs from the camera session. cc_scan's Scan is the real one, and the tests have a fake.
pub trait Eye {
    fn say(&self, text: &str);
    fn want(&self, ids: Vec<usize>);
    fn sizes(&self, sides: Vec<(usize, f64)>);
    fn shots(&mut self, n: usize) -> Result<Answer, String>;
    fn monitor(&mut self, name: Option<&str>);
    fn save(&self);
}

impl Eye for Scan {
    fn say(&self, text: &str) {
        Scan::say(self, text)
    }
    fn want(&self, ids: Vec<usize>) {
        Scan::want(self, ids)
    }
    fn sizes(&self, sides: Vec<(usize, f64)>) {
        Scan::sizes(self, sides)
    }
    fn shots(&mut self, n: usize) -> Result<Answer, String> {
        Scan::shots(self, n)
    }
    fn monitor(&mut self, name: Option<&str>) {
        Scan::monitor(self, name)
    }
    fn save(&self) {
        Scan::save(self)
    }
}

fn open_camera(path: &Path) -> Scan {
    Scan::open(path, &conf_dir()).unwrap_or_else(|e| die(camera_stopped(&e)))
}

// ---- align

/// The "align" setting in settings.json (what I asked for: "allow users to desire higher accuracy or quicker alignments").
#[derive(Clone, Copy)]
pub struct Mode {
    baseline: f64,
    samples: f64,
    coverage: f64,
    gate: f64,
    capture_for: f64,
}

// Fast is 20 frames and 6 cm of head travel. On the two real scans I kept (2026-10-03), the first
// 20 frames fitted within ~1.5-3.6 mm of the full run's corners, which is inside the fit's own
// 1.4-4.3 mm. Head travel barely mattered (20 frames over 1 cm: 1.5 mm), and the old 24 / 12 cm had
// users swinging their heads around.
const FAST: Mode = Mode { baseline: 0.06, samples: 20.0, coverage: 0.7, gate: 12.0, capture_for: 60.0 };
const PRECISE: Mode = Mode { baseline: 0.25, samples: 40.0, coverage: 0.85, gate: 6.0, capture_for: 90.0 };
// Entering a workspace (cc-home workspace enter): the four corner tags only, no calibrating, and done as
// soon as one monitor has 12 frames over 2 cm of head travel. That's a few seconds of looking. In the
// simulation in cc-scan's solve.rs (quick_corners_pin_the_desk) it puts a spot 1 m from the monitor
// within a few mm of where the full grid does; one tag alone is several times worse, since its yaw
// rests on a square a tenth of the monitor's width.
const QUICK: Mode = Mode { baseline: 0.02, samples: 12.0, coverage: 1.0, gate: 12.0, capture_for: 30.0 };
const MAX_MM: f64 = 10.0; // a monitor fit worse than this (millimetres at the monitor) gets reported, not used

fn align_mode() -> &'static str {
    let s: Value = std::fs::read(conf_dir().join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    match s.get("align").and_then(Value::as_str) {
        Some("precise") => "precise",
        _ => "fast",
    }
}

/// Why a monitor's tag screen went away, the way align words it (from the agent's error or event).
fn skipped_why(why: &str) -> &str {
    match why {
        "escaped" => "closed on that machine",
        "tags-limit" => "that machine's tag-screen limit for this hour is used up",
        "blocked" => "tag screens blocked on that machine for now",
        "busy" => "another Frame is showing tags there",
        w => w,
    }
}

/// The monitors whose tag screen went away (Esc over there, an agent's refusal, a viewer that
/// died), and why.
#[derive(Clone, Default)]
pub struct Gone(Arc<Mutex<Vec<(String, Option<String>)>>>);

impl Gone {
    fn add(&self, name: &str, why: Option<String>) {
        let mut g = self.0.lock().unwrap();
        match g.iter_mut().find(|(n, _)| n == name) {
            Some(e) => e.1 = why.or(e.1.take()),
            None => g.push((name.to_owned(), why)),
        }
    }
    fn has(&self, name: &str) -> bool {
        self.0.lock().unwrap().iter().any(|(n, _)| n == name)
    }
    fn why(&self, name: &str) -> String {
        self.0.lock().unwrap().iter().find(|(n, _)| n == name).and_then(|(_, w)| w.clone()).unwrap_or_else(|| "escaped".into())
    }
}

/// A monitor being aligned.
pub struct Mon {
    name: String,
    screen: i64,
    mm: Value,
    host: String,
    output: String,
    pixels: (i64, i64),
    draw: Option<(i64, i64)>, // its agent's native pixels
    tags: Map<String, Value>,
    order: Vec<String>, // the tags' ids in the order Python's dict had them, for job.json
    curve: Value,
    radius: Value,
    index: i64,
    base: usize,
    frame: Value,
    dense: Option<Value>,
    dense_order: Vec<String>,
    agent: Option<Arc<Mutex<Client>>>, // shared by the machine's monitors (AgentView)
}

impl Mon {
    fn size(&self) -> (i64, i64) {
        self.draw.unwrap_or(self.pixels)
    }
    fn mm(&self) -> (f64, f64) {
        (self.mm[0].as_f64().unwrap_or(0.0), self.mm[1].as_f64().unwrap_or(0.0))
    }
    /// A scan image the way the agent draws it (docs/agent.md 3a): the layout's tags in pixels.
    fn tag_params(&self, image: &str) -> Value {
        let js = match image {
            "frame.png" => Some(&self.frame),
            "dense.png" => self.dense.as_ref(),
            _ => None,
        };
        let Some(js) = js else { return json!({"bg": "wait", "tags": []}) };
        let (w, h) = self.size();
        let (w, h) = (w as f64, h as f64);
        let tags: Vec<Value> = js["tags"].as_object().into_iter().flatten().map(|(i, c)| {
            let f = |a: usize, b: usize| c[a][b].as_f64().unwrap_or(0.0);
            json!([i.parse::<i64>().unwrap_or(0), iround(f(0, 0) * w), iround(f(0, 1) * h), iround((f(1, 0) - f(0, 0)) * w)])
        }).collect();
        json!({"bg": "white", "tags": tags})
    }
    fn ids(js: &Value) -> Vec<usize> {
        js["tags"].as_object().into_iter().flatten().filter_map(|(i, _)| i.parse().ok()).collect()
    }
}

/// A monitor's tag screen through its machine's agent. It shows the screen, keeps it alive (a
/// screen only lives 60 s unless it's shown again), and watches the agent's events for Esc.
/// Monitors of one machine share its connection, since the agent keeps only a Frame's newest one
/// (cc-host agent.rs), so `peers` is every (name, index) on it: whichever view polls an event
/// marks the monitor it's for.
struct AgentView {
    name: String,
    index: i64,
    peers: Arc<Vec<(String, i64)>>,
    c: Arc<Mutex<Client>>,
    last: Arc<Mutex<Option<Value>>>,
    done: Arc<AtomicBool>,
    gone: Gone,
}

fn on_event(e: &Value, peers: &[(String, i64)], gone: &Gone) {
    let ev = e["event"].as_str().unwrap_or("");
    for (name, index) in peers {
        if (ev == "escaped" || ev == "blocked") && e.get("index").map_or(true, |i| i.as_i64() == Some(*index)) {
            gone.add(name, Some(ev.to_owned()));
        }
    }
}

impl AgentView {
    fn start(name: &str, index: i64, peers: Arc<Vec<(String, i64)>>, c: Arc<Mutex<Client>>, gone: Gone) -> AgentView {
        let v = AgentView { name: name.into(), index, peers, c, last: Default::default(), done: Default::default(), gone };
        let (name, peers, c, last, done, gone) = (v.name.clone(), v.peers.clone(), v.c.clone(), v.last.clone(), v.done.clone(), v.gone.clone());
        std::thread::spawn(move || {
            let mut beat = Instant::now();
            while !done.load(Relaxed) && !gone.has(&name) {
                {
                    let mut c = c.lock().unwrap();
                    if c.poll(Duration::from_millis(10), |e| on_event(e, &peers, &gone)).is_err() {
                        gone.add(&name, None); // the agent went
                    }
                    let shown = last.lock().unwrap().clone();
                    if let Some(p) = shown.filter(|_| beat.elapsed().as_secs_f64() > 30.0) {
                        beat = Instant::now();
                        let _ = c.call("tags", args(json!({"op": "show", "index": index, "params": p})), Duration::from_secs(10), |e| on_event(e, &peers, &gone));
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        v
    }

    fn put(&self, params: Value) {
        *self.last.lock().unwrap() = Some(params.clone());
        for attempt in 0..2 {
            let r = self.c.lock().unwrap().call("tags", args(json!({"op": "show", "index": self.index, "params": params})), Duration::from_secs(10),
                                                  |e| on_event(e, &self.peers, &self.gone))
                .unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()})); // the agent went
            if truthy(&r["ok"]) {
                return;
            }
            if r["error"] == "wait" && attempt == 0 {
                std::thread::sleep(Duration::from_millis(3100)); // right after a cancel there's a 3 s gap, so it doesn't flash
                continue;
            }
            println!("   {}: the agent said {}", self.name, py_text(&r["error"]));
            // tags-limit, blocked or busy, which isn't an Esc
            self.gone.add(&self.name, Some(if truthy(&r["error"]) { py_text(&r["error"]) } else { "gone".into() }));
            return;
        }
    }

    fn close(self) {
        self.done.store(true, Relaxed);
        let _ = self.c.lock().unwrap().call("tags", args(json!({"op": "hide", "index": self.index})), Duration::from_secs(10), |_| {});
    }
}

enum View {
    Agent(AgentView),
    Ssh(ssh::Viewer),
    None,
}

struct Align<'a> {
    eye: &'a mut dyn Eye,
    mons: Vec<Mon>,
    views: Vec<View>,
    gone: Gone,
    want: Mode,
    at: Option<V3>,
    skip: bool,
    calibrate_for: f64,
    times: Vec<(&'static str, f64)>,
    quick: bool, // entering a workspace: corner tags only, and the first monitor done ends it
}

impl Align<'_> {
    fn mon(&self, name: &str) -> usize {
        self.mons.iter().position(|m| m.name == name).expect("one of ours")
    }

    fn say(&self, text: &str, want: Vec<usize>) {
        self.eye.say(text);
        self.eye.want(want);
    }

    /// One frame: the tag ids read in it, or none if the head wasn't still.
    fn grab(&mut self) -> Result<HashSet<usize>, String> {
        let a = self.eye.shots(1).map_err(|e| camera_stopped(&e))?;
        self.at = a.at.map(|p| p.map(|v| (v * 1000.0).round() / 1000.0)); // (in mm, like scan.py said it)
        self.skip |= a.skip;
        Ok(a.found.into_iter().collect())
    }

    /// Whether the HUD's skip button was clicked. True once per click.
    fn skipped(&mut self) -> bool {
        std::mem::take(&mut self.skip)
    }

    /// Every monitor's tags' sides in metres, so the HUD puts its outlines at their depth.
    fn sizes(&self, which: &[usize]) {
        let mut out = vec![];
        for &k in which {
            let m = &self.mons[k];
            for (i, c) in &m.tags {
                let side = (c[1][0].as_f64().unwrap_or(0.0) - c[0][0].as_f64().unwrap_or(0.0)) * m.mm().0 / 1000.0;
                out.push((i.parse().unwrap_or(0), (side * 1e4).round() / 1e4));
            }
        }
        self.eye.sizes(out);
    }

    /// Puts an image on each monitor that's still there (frame.png or dense.png, else wait.png).
    fn show(&mut self, images: &[(&str, &str)]) {
        for (m, v) in self.mons.iter().zip(self.views.iter_mut()) {
            if self.gone.has(&m.name) {
                continue;
            }
            let image = images.iter().find(|(n, _)| *n == m.name).map_or("wait.png", |x| x.1);
            let mut params = m.tag_params(image);
            match v {
                View::Agent(a) => a.put(params),
                View::Ssh(s) => {
                    params["size"] = json!([m.size().0, m.size().1]); // cc-host tagscreen draws at the output's pixels (same as the agent's tags_show sends it)
                    if !s.show(&params) {
                        self.gone.add(&m.name, None); // gone or hung
                    }
                }
                View::None => {}
            }
        }
    }

    /// The monitors among names whose tag screen went away. They're skipped, not solved.
    fn escaped(&mut self, names: &[String]) -> Vec<String> {
        let out: Vec<String> = names.iter().filter(|n| self.gone.has(n)).cloned().collect();
        for n in &out {
            let why = self.gone.why(n);
            println!("   {n}: skipped ({})", skipped_why(&why));
            progress("skipped", n, &[("why", why)]);
            let k = self.mon(n);
            self.mons[k].dense = None;
        }
        out
    }

    /// Entering a workspace: each monitor shows just its four corner tags (big, so they read from
    /// anywhere at a desk), and capture takes them as its "grid".
    fn corners(&mut self) {
        for m in self.mons.iter_mut() {
            let ids: Vec<String> = (m.base..m.base + 4).map(|i| i.to_string()).collect();
            let tags: Map<String, Value> = ids.iter().filter_map(|i| Some((i.clone(), m.frame["tags"].get(i)?.clone()))).collect();
            (m.tags, m.order, m.dense_order, m.dense) = (Map::new(), vec![], ids, Some(json!({"tags": tags})));
        }
    }

    /// Step 2, for every monitor at once: works out each one's grid size from the tags read in its
    /// still frames (6 of them). A monitor gets dropped after 3 unreadable tries or a skip.
    fn calibrate(&mut self) -> Result<(), String> {
        println!("1. calibrating tag sizes (look at each screen for a moment)");
        let frames: Vec<(String, &str)> = self.mons.iter().map(|m| (m.name.clone(), "frame.png")).collect();
        self.show(&frames.iter().map(|(n, i)| (n.as_str(), *i)).collect::<Vec<_>>());
        self.sizes(&(0..self.mons.len()).collect::<Vec<_>>());
        struct Left {
            k: usize,
            n: usize,
            counts: HashMap<usize, usize>,
            tries: usize,
        }
        let mut left: Vec<Left> = (0..self.mons.len()).map(|k| Left { k, n: 0, counts: HashMap::new(), tries: 0 }).collect();
        for m in &self.mons {
            progress("calibrate", &m.name, &[]);
        }
        let (mut said, deadline) = (String::new(), Instant::now() + Duration::from_secs_f64(self.calibrate_for));
        while !left.is_empty() && Instant::now() < deadline {
            let names: Vec<String> = left.iter().map(|c| self.mons[c.k].name.clone()).collect();
            let out = self.escaped(&names);
            left.retain(|c| !out.contains(&self.mons[c.k].name));
            if left.is_empty() {
                break;
            }
            let text = format!("calibrating tag sizes, look at each screen for a moment: {}",
                               left.iter().map(|c| format!("{} {}/6", self.mons[c.k].name, c.n)).collect::<Vec<_>>().join(", "));
            if text != said {
                self.say(&text, left.iter().flat_map(|c| Mon::ids(&self.mons[c.k].frame)).collect());
                said = text;
            }
            let ids = self.grab()?;
            if self.skipped() {
                // the ones that aren't done yet get skipped
                for c in &left {
                    println!("   {}: skipped (the HUD was clicked)", self.mons[c.k].name);
                    progress("skipped", &self.mons[c.k].name, &[("why", "clicked".into())]);
                }
                return Ok(());
            }
            let mut i = 0;
            while i < left.len() {
                let c = &mut left[i];
                let m = &self.mons[c.k];
                let own: HashSet<usize> = Mon::ids(&m.frame).into_iter().filter(|t| ids.contains(t)).collect();
                if own.len() < 4 && !(0..4).all(|k| own.contains(&(m.base + k))) {
                    i += 1;
                    continue;
                }
                c.n += 1;
                for t in &own {
                    *c.counts.entry(*t).or_default() += 1;
                }
                if c.n < 6 {
                    i += 1;
                    continue;
                }
                let ok: Vec<i64> = m.frame["sides_px"].as_object().into_iter().flatten()
                    .filter(|(t, _)| t.parse().ok().and_then(|t: usize| c.counts.get(&t)).copied().unwrap_or(0) as f64 >= 0.6 * c.n as f64)
                    .filter_map(|(_, s)| s.as_i64()).collect();
                // margin: read straight on, the grid has to read from a lean too
                let side = ok.iter().min().map(|s| (*s as f64 * 1.35) as i64);
                let (w, h) = m.size();
                let lay = side.map(|s| cc_scan::pattern::dense(w, h, m.base, s));
                let order: Vec<String> = lay.as_ref().map_or(vec![], |l| l.rects.iter().map(|r| r.0.to_string()).collect());
                let dense = lay.map(|l| l.json);
                let fits = dense.as_ref().map_or(0, |d| d["fits"].as_i64().unwrap_or(0));
                let name = m.name.clone();
                if dense.is_some() && fits > 3 {
                    let side = side.unwrap_or(0);
                    println!("   {name}: smallest reliable tag {side} native px ({fits} fit)");
                    progress("size", &name, &[("state", "ok".into()), ("px", side.to_string()), ("fit", fits.to_string())]);
                    let k = c.k;
                    (self.mons[k].dense, self.mons[k].dense_order) = (dense, order);
                    left.remove(i);
                    continue;
                }
                // Which tags read decides the rest. More than 3 of the smallest readable size have
                // to fit, or there's nothing to place it by (from me: clean the cameras or get closer).
                let why = if dense.is_some() { format!("only {fits} tags of the smallest readable size fit") } else { "no tag reads reliably".into() };
                c.tries += 1;
                println!("   {name}: {why}: clean the headset's cameras or move closer");
                progress("size", &name, &[("state", "unreadable".into()), ("attempt", c.tries.to_string())]);
                if c.tries >= 3 {
                    progress("skipped", &name, &[("why", "unreadable".into())]);
                    left.remove(i);
                    continue;
                }
                (c.n, c.counts) = (0, HashMap::new());
                self.say(&format!("{name}: {why}. Clean the cameras or move closer"), vec![]);
                said.clear();
                std::thread::sleep(Duration::from_secs(3));
                i += 1;
            }
        }
        for c in &left {
            println!("   {}: no tags read in {} s; skipping it", self.mons[c.k].name, self.calibrate_for);
            progress("skipped", &self.mons[c.k].name, &[("why", "not-seen".into())]);
        }
        Ok(())
    }

    /// Step 3, for every calibrated monitor at once: each usable frame with 4+ of a monitor's tags is
    /// a sample for it, until each is complete (coverage, baseline, samples) or capture_for s runs out.
    fn capture(&mut self) -> Result<(), String> {
        println!("2. capturing (move your head a little side to side)");
        let live: Vec<usize> = (0..self.mons.len()).filter(|&k| self.mons[k].dense.is_some()).collect();
        for &k in &live {
            let d = self.mons[k].dense.clone().unwrap_or_default();
            self.mons[k].tags.extend(d["tags"].as_object().cloned().unwrap_or_default());
            let order = self.mons[k].dense_order.clone();
            self.mons[k].order.extend(order);
        }
        self.sizes(&live);
        let dense: Vec<(String, &str)> = live.iter().map(|&k| (self.mons[k].name.clone(), "dense.png")).collect();
        self.show(&dense.iter().map(|(n, i)| (n.as_str(), *i)).collect::<Vec<_>>());
        struct St {
            k: usize,
            ids: HashSet<usize>,
            seen: HashMap<usize, usize>,
            at: Vec<V3>,
            pct: i64,
            told: Option<Instant>,
            cov: f64,
            base: f64,
        }
        let mut state: Vec<St> = live.iter().map(|&k| St {
            k, ids: Mon::ids(self.mons[k].dense.as_ref().unwrap_or(&Value::Null)).into_iter().collect(), seen: HashMap::new(), at: vec![],
            pct: 0, told: None, cov: 0.0, base: 0.0,
        }).collect();
        let want = self.want;
        let pct = |s: &mut St| {
            s.cov = s.seen.values().filter(|v| **v >= 2).count() as f64 / s.ids.len().max(1) as f64;
            s.base = s.at.iter().enumerate().flat_map(|(i, a)| s.at[i + 1..].iter().map(move |b| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()))
                .fold(0.0, f64::max);
            (100.0 * 1f64.min(s.cov / want.coverage).min(s.base / want.baseline).min(s.at.len() as f64 / want.samples)) as i64
        };
        let (mut said, deadline) = (String::new(), Instant::now() + Duration::from_secs_f64(want.capture_for));
        let quick = self.quick;
        while Instant::now() < deadline && if quick { state.iter().all(|s| s.pct < 100) } else { state.iter().any(|s| s.pct < 100) } {
            let names: Vec<String> = state.iter().map(|s| self.mons[s.k].name.clone()).collect();
            let out = self.escaped(&names);
            state.retain(|s| !out.contains(&self.mons[s.k].name));
            let text = format!("{}: {}", if quick { "look at one of these monitors" } else { "move your head a little side to side" },
                               state.iter().map(|s| format!("{} {}%", self.mons[s.k].name, s.pct)).collect::<Vec<_>>().join(", "));
            if text != said {
                self.say(&text, state.iter().filter(|s| s.pct < 100).flat_map(|s| s.ids.iter().copied()).collect());
                said = text;
            }
            let ids = self.grab()?;
            if self.skipped() {
                println!("   capture ended early (the HUD was clicked)");
                break;
            }
            let at = self.at;
            for s in state.iter_mut() {
                let own: Vec<usize> = s.ids.iter().copied().filter(|t| ids.contains(t)).collect();
                let Some(at) = at.filter(|_| own.len() >= 4) else { continue };
                s.at.push(at);
                for t in own {
                    *s.seen.entry(t).or_default() += 1;
                }
                s.pct = pct(s);
                if s.told.is_none_or(|t| secs(t) >= 1.0) || s.pct >= 100 {
                    s.told = Some(Instant::now());
                    progress("capture", &self.mons[s.k].name, &[("pct", s.pct.to_string()), ("samples", s.at.len().to_string()),
                             ("baseline_cm", iround(s.base * 100.0).to_string()), ("coverage", iround(s.cov * 100.0).to_string())]);
                }
            }
        }
        let names: Vec<String> = state.iter().map(|s| self.mons[s.k].name.clone()).collect();
        let out = self.escaped(&names);
        state.retain(|s| !out.contains(&self.mons[s.k].name));
        for s in &state {
            let name = self.mons[s.k].name.clone();
            println!("   {name}: {}% ({} samples)", s.pct, s.at.len());
            if s.at.len() < 3 {
                // too little to fit, so it's not placed
                println!("   {name}: too few samples to place it");
                progress("failed", &name, &[("why", "few-samples".into())]);
                self.mons[s.k].dense = None;
            }
        }
        Ok(())
    }

    /// Calibrate, capture, and one solve for all of them while the panels are still hidden. Returns
    /// the solve's lines, or None when no monitor finished.
    fn run(&mut self, work: &Path, camera: &Json) -> Result<Option<Vec<String>>, String> {
        self.eye.monitor(None); // shots span every monitor, and the solve splits them by tag ids
        let mut t = Instant::now();
        if self.quick {
            self.corners();
        } else {
            self.calibrate()?;
            self.times.push(("calibrate", secs(t)));
        }
        t = Instant::now();
        if self.mons.iter().any(|m| m.dense.is_some()) {
            self.capture()?;
        }
        self.times.push(("capture", secs(t)));
        let placed: Vec<&Mon> = self.mons.iter().filter(|m| m.dense.is_some()).collect(); // so a skipped monitor isn't fitted from stray frames
        if placed.is_empty() {
            return Ok(None);
        }
        let job = write_job(work, camera, &placed);
        for m in &placed {
            progress("solving", &m.name, &[]);
        }
        let names: Vec<&str> = placed.iter().map(|m| m.name.as_str()).collect();
        self.say(&format!("working out where {} {}", names.join(", "), if names.len() == 1 { "is" } else { "are" }), vec![]);
        self.eye.save();
        let t = Instant::now();
        let mut lines = vec![];
        if let Err(e) = cc_scan::solve::solve(&job, &mut |l| lines.push(l)) {
            lines.push(format!("#error {e}")); // a failed solve gets reported, and the rest are still placed
        }
        self.times.push(("solve", secs(t)));
        Ok(Some(lines))
    }
}

/// The solve's job, written to job.json exactly the way cc-home.py's json.dump wrote it (same keys,
/// order and numbers), and returned the way the solve reads it.
fn write_job(work: &Path, camera: &Json, placed: &[&Mon]) -> Value {
    let s = |p: PathBuf| Json::Str(p.to_string_lossy().into_owned());
    let mons = placed.iter().map(|m| Json::Obj(vec![
        ("name".into(), Json::Str(m.name.clone())),
        ("screen".into(), Json::Num(m.screen.to_string())),
        ("tags".into(), Json::Obj(m.order.iter().map(|k| (k.clone(), Json::from_serde(&m.tags[k]))).collect())),
        ("mm".into(), Json::from_serde(&m.mm)),
        ("curve".into(), Json::from_serde(&m.curve)),
        ("radius".into(), Json::from_serde(&m.radius)),
    ])).collect();
    let job = Json::Obj(vec![
        ("visible".into(), s(work.join("visible.json"))),
        ("hidden".into(), s(work.join("hidden.json"))),
        ("head".into(), s(work.join("hidden-head.json"))),
        ("screens".into(), Json::obj()),
        ("camera".into(), camera.clone()),
        ("monitors".into(), Json::Arr(mons)),
    ]).dumps();
    let _ = std::fs::write(work.join("job.json"), &job);
    serde_json::from_str(&job).unwrap_or(Value::Null)
}

/// Gets one monitor ready (machines run in parallel): its output and size from its machine's
/// agent (docs/agent.md), or with CC_SSH=1, over SSH with the tag viewer copied there. None means
/// it already said why.
fn ready(v: &cc_proto::conf::Viewer, agent: &Result<Option<Arc<Mutex<Client>>>, machine::Fail>) -> Result<Option<Mon>, String> {
    let name = &v.name;
    let index = (machine::port_of(v) - 3400).rem_euclid(10); // a paired Frame's slot k serves monitor m on 3400 + 10k + m
    progress("prepare", name, &[("step", "monitor".into())]);
    let radius = v.opt("radius").filter(|r| { let (a, b) = r.split_once('.').unwrap_or((r, "0")); !a.is_empty() && !b.is_empty() && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()) })
        .and_then(|r| r.parse::<f64>().ok()).map_or(Value::Null, |r| json!(r));
    let curve = v.opt("curve").map_or(Value::Null, |c| json!(c));
    let mut mon = Mon { name: name.clone(), screen: v.screen, mm: Value::Null, host: v.machine.clone(), output: String::new(), pixels: v.pixels, draw: None,
                        tags: Map::new(), order: vec![], curve, radius, index, base: 0, frame: Value::Null, dense: None, dense_order: vec![], agent: None };
    match agent {
        Err(e) => {
            println!("{name}: {}", match &e { Fail::NotPaired(m) | Fail::NoAgent(m) => m });
            progress("skipped", name, &[("why", e.state().into())]);
            Ok(None)
        }
        Ok(Some(cl)) => {
            let r = cl.lock().unwrap().call("monitors", Map::new(), Duration::from_secs(10), |_| {}).map_err(|e| format!("{name}: its agent: {e}"))?;
            let Some(m) = r["monitors"].as_array().into_iter().flatten().find(|m| m["index"].as_i64() == Some(index)).cloned() else {
                println!("{name}: its machine's agent shares no monitor {index}");
                progress("failed", name, &[("why", "no-monitor".into())]);
                return Ok(None);
            };
            (mon.mm, mon.output, mon.agent) = (m["mm"].clone(), py_text(&m["output"]), Some(cl.clone()));
            mon.draw = Some((m["width"].as_i64().unwrap_or(v.pixels.0), m["height"].as_i64().unwrap_or(v.pixels.1)));
            Ok(Some(mon))
        }
        Ok(None) => {
            // CC_SSH=1 is diagnostics over SSH. The machine's cc-host tagscreen draws the tags it's sent.
            let user_host = ssh::login(v, &conf_dir());
            progress("prepare", name, &[("step", "monitor".into())]);
            let out = ssh::remote(&user_host, &format!("{}\n~/.local/bin/cc-share list", ssh::SESSION), "this", "");
            let prefix = format!("monitor {index}:");
            let Some(output) = out.stdout.lines().find(|l| l.starts_with(&prefix)).and_then(|l| l.split_whitespace().nth(2)).map(str::to_owned) else {
                println!("{name}: can't find its monitor on {user_host} ({})", if out.stderr.trim().is_empty() { out.stdout.trim() } else { out.stderr.trim() });
                progress("failed", name, &[("why", "no-monitor".into())]);
                return Ok(None);
            };
            progress("prepare", name, &[("step", "viewer".into())]);
            // Clear earlier tags, and get its size in mm from kscreen (the same way cc-host's agent reads it).
            let info = ssh::remote(&user_host, &format!("{}\npkill -f '[c]c-host tagscreen' || true\ntest -x {} || {{ echo 'no cc-host there: cc-share install' >&2; exit 1; }}\nkscreen-doctor -j",
                                                        ssh::SESSION, ssh::CC_HOST), "this", "");
            let mm = (info.code == 0).then(|| serde_json::from_str::<Value>(&info.stdout).ok()).flatten()
                .and_then(|v| v["outputs"].as_array()?.iter().find(|o| o["name"] == output.as_str()).map(ssh::output_mm));
            let Some(mm) = mm else {
                println!("{name}: can't show tags on {user_host} (needs cc-host and kscreen-doctor): {}", ssh::tail(info.stderr.trim(), 200));
                progress("failed", name, &[("why", "no-cc-host".into())]);
                return Ok(None);
            };
            (mon.mm, mon.host, mon.output) = (mm, user_host, output);
            Ok(Some(mon))
        }
    }
}

/// Finds each chosen remote monitor in the room by camera and makes that its home, in one pass for
/// all of them (I wanted it to measure as you look around, not in steps).
/// 1. Getting ready: each machine in parallel, through its agent, and its tags laid out.
/// 2. Calibrating: every monitor shows its corners and size ladder. The smallest tag read in most
///    of 6 usable frames, x1.35, is its grid's size. If 3 or fewer of those fit, it says to clean
///    the cameras or move closer, up to 3 tries.
/// 3. Capturing: every monitor shows its grid, and each usable frame with 4+ of a monitor's tags is
///    a sample, until each has the align mode's coverage, baseline and samples, or capture_for s.
/// 4. One solve for all, while the panels are still hidden, then placed and made home.
pub fn scan(names: &[String]) {
    let t0 = Instant::now();
    let mode = align_mode();
    progress("mode", mode, &[]);
    let (lines, sock, times) = look(names, if mode == "precise" { PRECISE } else { FAST }, None);
    fit(&lines, &sock, &conf_dir().join("mirror-camera.json"), t0, &times)
}

/// The scan itself, up to the solve's lines (scan's steps 1-3 and its solve). `quick` is entering a
/// workspace (corners only, the first monitor done ends it), with its saved poses, whose shape (flat,
/// or curved and its radius) the solve takes as known. It dies when nothing got solved.
fn look(names: &[String], want: Mode, quick: Option<&Json>) -> (Vec<String>, Panels, Vec<(&'static str, f64)>) {
    let t0 = Instant::now();
    let work = PathBuf::from(cache("scan"));
    let _ = std::fs::create_dir_all(&work);
    let cam_file = conf_dir().join("mirror-camera.json");
    if let Ok(f) = std::env::var("CC_HOME_SOLVED") {
        // For tests/cross.rs: it gives a solve's output, so no camera and no hosts.
        let lines: Vec<String> = std::fs::read_to_string(&f).unwrap_or_else(|e| die(format!("{f}: {e}"))).lines().map(str::to_owned).collect();
        return (lines, target(), vec![]);
    }
    // Show the HUD from the start. Getting the machines ready takes a while, and with nothing
    // showing it felt frozen to me.
    let mut cam = open_camera(&work.join("hidden.json"));
    cam.gate(want.gate);
    let todo: Vec<&cc_proto::conf::Viewer> = viewers().iter().filter(|v| names.is_empty() || names.contains(&v.name)).collect();
    cam.say(&format!("getting {} monitor{} ready: {}", todo.len(), if todo.len() == 1 { "" } else { "s" },
                     todo.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", ")));
    cam.want(vec![]);
    // One connection per machine: its agent keeps only a Frame's newest one, so a second
    // connection for another of its monitors would close the first.
    let mut machines: Vec<&str> = todo.iter().map(|v| v.machine.as_str()).collect();
    machines.sort_unstable();
    machines.dedup();
    let got: Vec<Result<Option<Mon>, String>> = std::thread::scope(|s| {
        let todo = &todo;
        let hs: Vec<_> = machines.iter().map(|&m| s.spawn(move || {
            let agent = machine::agent_for(m, "align").map(|c| c.map(|c| Arc::new(Mutex::new(c))));
            todo.iter().filter(|v| v.machine == m).map(|v| ready(v, &agent)).collect::<Vec<_>>()
        })).collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_else(|_| vec![Err("getting a monitor ready failed".into())])).collect()
    });
    let mut mons = vec![];
    for g in got {
        match g {
            Ok(Some(m)) => mons.push(m),
            Ok(None) => {}
            Err(e) => {
                cam.finish(); // failed getting ready, so the camera and its HUD go too
                die(e);
            }
        }
    }
    for (k, m) in mons.iter_mut().enumerate() {
        m.base = 50 * k;
        if let Some(saved) = quick.map(|q| q.at(&m.name)) {
            (m.curve, m.radius) = saved_shape(saved);
        } // ids base..base+49 (DICT_4X4_250) don't overlap, so they can all show at once
        progress("prepare", &m.name, &[("step", "tags".into())]);
        let (w, h) = m.size();
        let lay = cc_scan::pattern::frame(w, h, m.base); // corners + ladder in one image
        m.frame = lay.json;
        m.tags.extend(m.frame["tags"].as_object().cloned().unwrap_or_default());
        m.order.extend(lay.rects.iter().map(|r| r.0.to_string()));
    }
    for m in &mons {
        let (a, b) = m.mm();
        println!("{}: {} on {}, {a:.0} x {b:.0} mm", m.name, m.output, m.host);
        progress("setup", &m.name, &[("output", m.output.clone()), ("mm", format!("{a:.0}x{b:.0}"))]);
    }
    if mons.is_empty() {
        cam.say("nothing to align: see the log");
        cam.finish();
        die("nothing to scan");
    }
    let mut times = vec![("ready", secs(t0))];
    let camera: Json = match std::fs::read_to_string(&cam_file) {
        Ok(t) => Json::parse(&t).unwrap_or(Json::Null),
        Err(_) => {
            cam.finish();
            die("no mirror camera fit yet: run cc-home refit");
        }
    };
    let _ = std::fs::write(work.join("visible.json"), "[]");
    let sock = target();
    let gone = Gone::default();
    let mut views = vec![];
    let peers: Vec<(Option<Arc<Mutex<Client>>>, String, i64)> = mons.iter().map(|m| (m.agent.clone(), m.name.clone(), m.index)).collect();
    for m in mons.iter_mut() {
        views.push(match m.agent.take() {
            Some(c) => {
                let on: Vec<(String, i64)> = peers.iter().filter(|p| p.0.as_ref().is_some_and(|o| Arc::ptr_eq(o, &c))).map(|p| (p.1.clone(), p.2)).collect();
                View::Agent(AgentView::start(&m.name, m.index, Arc::new(on), c, gone.clone()))
            }
            None => {
                let (g, n) = (gone.clone(), m.name.clone());
                match ssh::Viewer::start(&m.host, &m.output, move || g.add(&n, None)) {
                    Ok(v) => View::Ssh(v),
                    Err(e) => {
                        println!("{}: can't show tags on {} ({e})", m.name, m.host);
                        gone.add(&m.name, Some("viewer-failed".into()));
                        View::None
                    }
                }
            }
        });
    }
    let _ = sock.ask("hide", 10.0);
    let mut a = Align { eye: &mut cam, mons, views, gone, want, at: None, skip: false, calibrate_for: 90.0, times: vec![], quick: quick.is_some() };
    let r = a.run(&work, &camera);
    a.show(&[]);
    times.append(&mut a.times);
    let views = std::mem::take(&mut a.views);
    drop(a);
    cam.finish();
    let _ = sock.ask("hud hide", 10.0); // the scan hides it too; this covers one that died
    let _ = sock.ask("show", 10.0);
    for v in views {
        match v {
            View::Agent(a) => a.close(),
            View::Ssh(s) => s.close(),
            View::None => {}
        }
    }
    match r.unwrap_or_else(|e| die(e)) {
        Some(lines) => (lines, sock, times),
        None => {
            done(t0, &times);
            die("no monitor finished its scan");
        }
    }
}

/// A saved pose's shape the way viewers.conf gives it to the solve: (curve, radius).
fn saved_shape(p: &Json) -> (Value, Value) {
    let r = |k: &str| p.at(k).num().filter(|r| *r > 0.0);
    match (r("vcurve"), r("curve")) {
        (Some(v), _) => (json!("v"), json!(v)),
        (None, Some(h)) => (json!("h"), json!(h)),
        _ => (json!("flat"), Value::Null),
    }
}

fn done(t0: Instant, times: &[(&str, f64)]) {
    let mut kv = vec![("total_s", iround(secs(t0)).to_string())];
    let keys: Vec<String> = times.iter().map(|(k, _)| format!("{k}_s")).collect();
    kv.extend(keys.iter().zip(times).map(|(k, (_, v))| (k.as_str(), iround(*v).to_string())));
    progress("done", "-", &kv);
}

/// Takes the solve's lines. Keeps the mirror's lag and camera, places each monitor's fit and saves
/// it in "scanned" (its last scan is kept as "before-align" for reanchor), and makes what was
/// placed home.
fn fit(lines: &[String], sock: &Panels, cam_file: &Path, t0: Instant, times: &[(&str, f64)]) {
    let (mut spots, mut fitted, mut lag) = (load(), Json::obj(), None);
    for line in lines {
        if let Some(l) = line.strip_prefix("#lag ") {
            lag = l.split_whitespace().next().and_then(|x| x.parse::<f64>().ok());
            continue;
        }
        if let Some(e) = line.strip_prefix("#error ") {
            println!("  solve: {e}");
            continue;
        }
        if let Some(c) = line.strip_prefix("#camera ") {
            let _ = std::fs::write(cam_file, c);
            continue;
        }
        if line.starts_with('#') {
            println!("  {}", line.get(2..).unwrap_or(""));
            continue;
        }
        let Some(r) = Json::parse(line) else {
            println!("  solve: {line}");
            continue;
        };
        let name = r.at("name").text();
        let rms = r.at("rms_mm").num().unwrap_or(f64::INFINITY);
        if rms > MAX_MM {
            progress("failed", &name, &[("why", "poor-fit".into()), ("mm", iround(rms).to_string())]);
            println!("  {name}: fit too poor ({rms:.0} mm > {MAX_MM:.0} mm), not placed; scan again, moving your head more slowly");
            continue;
        }
        let pose = solved_pose(&r);
        let (axis, radius) = (r.at("axis").text(), r.at("radius").clone());
        if axis == "v" {
            println!("  {name}: curved top to bottom (R {:.2} m)", radius.num().unwrap_or(0.0));
        }
        let before = spots.at("scanned").at(&name).clone();
        if before.truthy() {
            // what it was, for cc-home reanchor (it may have been the room that moved, not the monitor)
            spots.setdefault("before-align", Json::obj()).set(&name, before.clone());
            let (mm, deg) = room_move(&before, &pose);
            if mm > 20.0 || deg > 1.0 {
                println!("  {name}: moved {mm:.0} mm and {deg:.1} deg since its last scan; if the room moved (tracking re-anchored), \
                          cc-home reanchor {name} moves everything else with it");
                progress("reanchor", &name, &[("mm", iround(mm).to_string()), ("deg", py_float(py_round(deg, 1)))]);
            }
        }
        store_pose(&mut spots, "scanned", &name, pose.clone());
        if let Err(e) = place(sock, &name, &pose) {
            println!("  {name}: {e}");
        }
        fitted.set(&name, pose);
        println!("  {name}: placed on the real monitor (fit {rms:.0} mm) and saved in scanned");
        progress("placed", &name, &[("mm", iround(rms).to_string()), ("axis", axis.clone()), ("radius", radius.text())]);
    }
    if fitted.truthy() {
        // Once placed it becomes home, like applying scanned (the old home is kept as "previous").
        make_home(&mut spots, &fitted);
        if let Some(lag) = lag {
            // this headset's mirror lag, for the next scan's first frames
            let _ = std::fs::write(conf_dir().join("mirror-lag.json"), format!("{{\"lag_ms\": {}}}", iround(lag * 1000.0)));
        }
    }
    store(spots);
    done(t0, times);
    println!("done in {:.0} s ({})", secs(t0), times.iter().map(|(k, v)| format!("{k} {v:.0} s")).collect::<Vec<_>>().join(", "));
}

/// A solve's line as a spot's pose.
fn solved_pose(r: &Json) -> Json {
    let v3 = |k: &str| -> V3 {
        let l = r.at(k).list();
        [0, 1, 2].map(|i| l.get(i).and_then(Json::num).unwrap_or(0.0))
    };
    let mut pose = pose_from_axes(v3("centre"), v3("x"), v3("z"), r.at("width").num().unwrap_or(0.0), r.at("height").num().unwrap_or(0.0));
    let (axis, radius) = (r.at("axis").text(), r.at("radius").clone());
    pose.set("curve", if axis == "h" { radius.clone() } else { Json::Num("0".into()) });
    if axis == "v" {
        pose.set("vcurve", radius); // top to bottom, since cc-panels shows it on the panel turned a quarter
    }
    pose
}

// ---- entering a workspace in a new room (docs/workspaces.md)

/// `cc-home workspace enter <name> [monitor ...]`: you're back at workspace name, but SteamVR's room
/// is new (the Frame rebuilt its map), so its spots are off. Each of its monitors given (by default
/// every one with a saved scan there) shows its four corner tags, and whichever you look at first is
/// found. If it's the same monitor (size, tilt) and any others found agree, every spot moves with it
/// and this room is added to the workspace (conf::enter_workspace). Then it's the active one, live.
pub fn enter(name: &str, monitors: &[&str]) {
    let data = super::read_all();
    if data.at("workspaces").get(name).is_none() || name == cc_proto::conf::TEMPORARY {
        die(format!("no workspace {name} to enter (cc-home workspace)"));
    }
    let saved = data.at("workspaces").at(name).at("spots").at("scanned").clone();
    let mut todo: Vec<String> = vec![];
    for v in viewers() {
        let asked = monitors.is_empty() || monitors.contains(&v.name.as_str());
        if asked && saved.get(&v.name).is_some() && cc_proto::conf::is_member(&data, name, &v.machine) {
            todo.push(v.name.clone());
        } else if asked && !monitors.is_empty() {
            die(format!("{}: no saved scan of it in {name} (align it there first)", v.name));
        }
    }
    if todo.is_empty() {
        die("Connect one of this workspace's machines first: it needs a monitor to find where you are");
    }
    let universe = super::panels_universe();
    if cc_proto::conf::room(universe) == 0 {
        die("SteamVR doesn't know this room yet (or cc-panels isn't running): wait until tracking settles, then try again");
    }
    let t0 = Instant::now();
    progress("mode", "quick", &[]);
    let (lines, sock, times) = look(&todo, QUICK, Some(&saved));
    drop(sock); // its name is this process's, and panels_reload needs it at the end
    // the monitors found, best fit first
    let mut found: Vec<(f64, String, Json)> = vec![];
    for line in &lines {
        if line.starts_with('#') {
            if let Some(t) = line.strip_prefix("# ") {
                println!("  {t}");
            }
            continue;
        }
        let Some(r) = Json::parse(line) else { continue };
        let (m, rms) = (r.at("name").text(), r.at("rms_mm").num().unwrap_or(f64::INFINITY));
        if rms > MAX_MM {
            println!("  {m}: fit too poor ({rms:.0} mm > {MAX_MM:.0} mm), not used");
            progress("failed", &m, &[("why", "poor-fit".into()), ("mm", iround(rms).to_string())]);
        } else if todo.contains(&m) {
            found.push((rms, m, solved_pose(&r)));
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    done(t0, &times);
    let Some((rms, root, pose)) = found.first().cloned() else { die(format!("none of {}'s monitors was found: look at one for a few seconds and try again", name)) };
    for (_, m, p) in &found[1..] {
        let off = cc_proto::conf::misfit_mm(saved.at(m), p, saved.at(&root), &pose);
        if off > cc_proto::conf::SAME_DESK_MM {
            progress("failed", name, &[("why", "not-same-desk".into())]);
            die(format!("not {name}, or its desk changed: {m} is {off:.0} mm from where {root} puts it. Nothing changed; align its monitors there"));
        }
        println!("  {m} agrees with {root} ({off:.0} mm)");
    }
    let mut data = super::read_all(); // again, since cc-panels may have written it meanwhile
    match cc_proto::conf::enter_workspace(&mut data, name, universe, saved.at(&root), &pose) {
        Ok((mm, deg)) => {
            super::write_all(&data);
            progress("entered", name, &[("monitor", root.clone()), ("mm", iround(mm).to_string()), ("deg", py_float(py_round(deg, 1)))]);
            println!("entered {name} by {root} (fit {rms:.0} mm): moved everything {mm:.0} mm and {deg:.1} deg; this room is {name}'s now too (cc-home apply previous undoes the move)");
            super::panels_reload();
        }
        Err(e) => {
            progress("failed", name, &[("why", "not-same-monitor".into())]);
            die(format!("{root}: {e}. Not {name}, so nothing changed"));
        }
    }
}

// ---- refit

/// A board `ahead` metres in front of the head, level, at eye height and facing it.
/// Returns (centre, x, y, z).
pub fn board_pose(head: [[f64; 4]; 3], ahead: f64) -> (V3, V3, V3, V3) {
    let pos = [head[0][3], head[1][3], head[2][3]];
    let (fx, fz) = (-head[0][2], -head[2][2]); // the head's forward (-z), flattened to level
    let n = match fx.hypot(fz) {
        0.0 => 1.0,
        n => n,
    };
    let fwd = [fx / n, 0.0, fz / n];
    let z = fwd.map(|v| -v); // facing back at the head
    let y = [0.0, 1.0, 0.0];
    let x = cross(y, z);
    ([0, 1, 2].map(|i| pos[i] + ahead * fwd[i]), x, y, z)
}

/// Fits the mirror camera again (~/.config/control-center/mirror-camera.json). It puts a board of
/// tags on an overlay at a known place in the room (the mirror shows overlays) and looks at it from
/// three head positions. The fit is only kept if it's good (median under 2 px).
pub fn refit() {
    let (places, per, apart) = (["where you are", "lean left", "lean right"], 8, 0.10);
    let sock = target();
    let work = PathBuf::from(cache("scan/refit"));
    let _ = std::fs::create_dir_all(&work);
    let (w_px, h_px, width) = (1440i64, 864i64, 0.8);
    let board = cc_scan::pattern::board(w_px, h_px, 200, 6, 3);
    let ids: HashSet<usize> = Mon::ids(&board.json).into_iter().collect();
    let f: Vec<String> = sock.ask("head", 10.0).unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    let v: Vec<f64> = f.get(1..13).map_or(vec![], |x| x.iter().filter_map(|n| n.parse().ok()).collect());
    if f.first().map(String::as_str) != Some("ok") || v.len() < 12 {
        die("the headset isn't tracked (put it on) or cc-panels has no `head`");
    }
    let (centre, x, y, z) = board_pose([[v[0], v[1], v[2], v[3]], [v[4], v[5], v[6], v[7]], [v[8], v[9], v[10], v[11]]], 1.0);
    let height = width * h_px as f64 / w_px as f64;
    let place: Vec<String> = (0..3).flat_map(|r| [x[r], y[r], z[r], centre[r]]).map(|a| format!("{a:.5}")).collect();
    let shots = work.join("shots.json");
    let mut cam = open_camera(&shots);
    let img = cc_scan::pattern::render(w_px as usize, h_px as usize, &board.rects);
    if let Err(e) = cam.board(&img, &format!("{} {}", py_float(width), place.join(" "))) {
        eprintln!("the board: {e}");
    }
    cam.monitor(Some("board"));
    let say = |cam: &Scan, text: &str| {
        cam.say(text);
        cam.want(ids.iter().copied());
    };
    let (mut done, mut good, mut fail) = (Vec::<V3>::new(), 0, None);
    'places: for (k, at_where) in places.iter().enumerate() {
        say(&cam, &format!("camera refit: position {} of {}: {at_where}, look at the tag board, hold still", k + 1, places.len()));
        let (mut here, mut there, deadline) = (0, None::<V3>, Instant::now() + Duration::from_secs(30));
        while Instant::now() < deadline && here < per {
            let a = match cam.shots(1) {
                Ok(a) => a,
                Err(e) => {
                    fail = Some(camera_stopped(&e));
                    break 'places;
                }
            };
            if a.skip {
                break;
            }
            if there.is_none() {
                let far = |p: &V3| done.iter().all(|d| ((p[0] - d[0]).powi(2) + (p[1] - d[1]).powi(2) + (p[2] - d[2]).powi(2)).sqrt() >= apart);
                there = a.at.filter(far);
                continue;
            }
            here += (a.found.iter().filter(|t| ids.contains(t)).count() >= 8) as usize;
        }
        println!("  position {} ({at_where}): {here} good frames", k + 1);
        done.extend(there);
        good += here;
    }
    cam.finish();
    let _ = sock.ask("hud hide", 10.0);
    if let Some(e) = fail {
        die(e);
    }
    if good < 5 {
        die(format!("only {good} good frames of the board; nothing changed"));
    }
    let empty = work.join("none.json");
    let _ = std::fs::write(&empty, "[]");
    let cam_file = conf_dir().join("mirror-camera.json");
    let old: Option<Value> = cam_file.exists().then(|| std::fs::read(&cam_file).ok().and_then(|b| serde_json::from_slice(&b).ok())).flatten();
    let job = json!({"visible": shots, "hidden": empty, "camera": old, "refit_camera": true,
                     "screens": {"0": {"center": centre, "x": x, "y": y, "z": z, "metres": width, "height": height, "curve": 0}},
                     "monitors": [{"name": "board", "screen": 0, "tags": board.json["tags"], "mm": [width * 1000.0, height * 1000.0],
                                   "curve": "flat", "virtual": true}]});
    let _ = std::fs::write(work.join("job.json"), job.to_string());
    let mut new = None;
    let r = cc_scan::solve::solve(&job, &mut |line| {
        if let Some(c) = line.strip_prefix("#camera ") {
            new = Some(c.to_owned());
        } else if line.starts_with('#') {
            println!("  {}", line.get(2..).unwrap_or(""));
        }
    });
    let Some(new) = new else {
        die(format!("the fit wasn't good enough (median 2 px or more); kept the old one{}", r.err().map_or(String::new(), |e| format!("\n{}", ssh::tail(&e, 300)))));
    };
    if old.is_some() {
        let _ = std::fs::rename(&cam_file, conf_dir().join("mirror-camera.json.bak"));
    }
    std::fs::write(&cam_file, new).unwrap_or_else(|e| die(format!("can't write {}: {e}", cam_file.display())));
    println!("mirror camera fitted again from {good} frames{}", if old.is_some() { " (the old one kept as mirror-camera.json.bak)" } else { "" });
}

// ---- calibrate: a monitor's corners touched with a controller's tip

/// The pose of the rectangle with these corners (top left, top right, bottom left, with its front
/// facing you), plus the angle at the top left corner (90 when it's square).
pub fn pose_from_corners(tl: V3, tr: V3, bl: V3) -> Json {
    let sub = |a: V3, b: V3| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let (right, down) = (sub(tr, tl), sub(bl, tl));
    let x = normalize(right);
    let z = normalize(cross(right, normalize(sub(tl, bl)))); // x cross up gives the front
    let centre = [0, 1, 2].map(|i| tl[i] + right[i] / 2.0 + down[i] / 2.0);
    let len = |v: V3| dot(v, v).sqrt();
    let mut p = pose_from_axes(centre, x, z, len(right), len(down));
    p.set("corner_deg", Json::float(py_round(degrees(dot(x, normalize(down)).clamp(-1.0, 1.0).acos()), 2)));
    p
}

/// A Frame controller's tip held still. Samples cc-panels' `tip` every 11 ms over 0.6 s, like
/// cc-tip did, and returns the mean, the samples' RMS distance from it (how still the hand was),
/// and the hand.
fn tip(sock: &Panels, hand: &str) -> (V3, f64, String) {
    let (end, mut samples, mut used, mut last) = (Instant::now() + Duration::from_millis(600), Vec::<V3>::new(), String::new(), String::new());
    while Instant::now() < end {
        match sock.ask(&format!("tip {hand}"), 1.0) {
            Ok(r) => {
                let w: Vec<&str> = r.split_whitespace().collect();
                let p: Vec<f64> = w.get(1..4).map_or(vec![], |x| x.iter().filter_map(|n| n.parse().ok()).collect());
                if p.len() == 3 {
                    samples.push([p[0], p[1], p[2]]);
                    used = w.get(4).unwrap_or(&"").to_string();
                }
            }
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(11));
    }
    if samples.is_empty() {
        die(if last.is_empty() { "the controller isn't tracked".into() } else { last.trim_start_matches("error ").to_owned() });
    }
    let n = samples.len() as f64;
    let mean = [0, 1, 2].map(|i| samples.iter().map(|s| s[i]).sum::<f64>() / n);
    let spread = (samples.iter().map(|s| (0..3).map(|i| (s[i] - mean[i]).powi(2)).sum::<f64>()).sum::<f64>() / n).sqrt();
    // (cc-tip printed them with 5 decimals)
    (mean.map(|v| format!("{v:.5}").parse().unwrap_or(v)), format!("{spread:.5}").parse().unwrap_or(spread), used)
}

pub fn calibrate(name: &str, hand: &str) {
    let Some(v) = viewers().iter().find(|v| v.name == name) else {
        die(format!("no {name} in {}", conf_dir().join("viewers.conf").display()));
    };
    let sock = target();
    println!("Home for {name} (screen {}). For each corner: hold the controller's tip", v.screen);
    println!("on the corner of the picture (not the bezel), keep still, and press Enter.");
    let mut pts = vec![];
    for corner in ["top left", "top right", "bottom left"] {
        loop {
            use std::io::Write;
            print!("  {corner}: ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
                die("EOF when reading a line");
            }
            let (p, spread, used) = tip(&sock, hand);
            if spread > 0.004 {
                println!("    moved {:.0} mm while measuring; again, holding still", spread * 1000.0);
                continue;
            }
            println!("    {used} tip at {:.3} {:.3} {:.3}", p[0], p[1], p[2]);
            pts.push(p);
            break;
        }
    }
    let pose = pose_from_corners(pts[0], pts[1], pts[2]);
    let (w, h) = v.pixels;
    let (pw, ph, deg) = (pose.at("width").num().unwrap_or(0.0), pose.at("height").num().unwrap_or(1.0), pose.at("corner_deg").num().unwrap_or(0.0));
    let aspect = pw / ph;
    let picture = w as f64 / h as f64;
    println!("  {:.1} x {:.1} cm, corner {deg:.1} deg (90 is square), shape {aspect:.3} vs the picture's {picture:.3}", pw * 100.0, ph * 100.0);
    if (deg - 90.0).abs() > 4.0 || (aspect / picture - 1.0).abs() > 0.06 {
        println!("  that doesn't look like this monitor's rectangle; check the corners and calibrate again");
    }
    let mut spots = load();
    store_pose(&mut spots, "scanned", name, pose.clone());
    store(spots);
    place(&sock, name, &pose).unwrap_or_else(|e| die(e));
    println!("  saved in home and placed; `cc-home apply` puts it back here");
}

// ---- pairing by looking

/// Pairs by looking at the host's key screen. The host is the one announcing pair=1 (or addr). The
/// key gets read off its tags through the camera, then it pairs as if you'd typed it.
pub fn pair_scan(addr: Option<&str>, replace: bool) {
    let addr = match addr {
        Some(a) => a.to_owned(),
        None => {
            let showing: Vec<_> = machine::discover(3.0).into_iter().filter(|h| h.txt("pair") == Some("1")).collect();
            if showing.len() != 1 {
                for h in &showing {
                    println!("@host name={} addr={} pair=1", h.name, h.addr);
                }
                die(if showing.is_empty() { "no host is showing a pairing key (cc-share pair on it)" } else { "more than one host is showing a key: give its address" });
            }
            println!("@pair {} host={} state=scanning", showing[0].addr, showing[0].name);
            showing[0].addr.clone()
        }
    };
    let t0 = Instant::now();
    target(); // the HUD and the head pose come from cc-panels
    let work = PathBuf::from(cache("pairscan"));
    let _ = std::fs::create_dir_all(&work);
    let stopped = |e: &str| -> ! {
        let users = camera_users();
        println!("{}", if users.is_empty() { "@pairscan state=no-camera".into() } else { format!("@pairscan state=camera-busy by={}", users.join(",")) });
        die(camera_stopped(e))
    };
    let mut cam = Scan::open(&work.join("shots.json"), &conf_dir()).unwrap_or_else(|e| stopped(&e));
    cam.button("cancel");
    cam.say("Look at the pairing key on the host's screen");
    cam.want(900..1000);
    let r = (|| -> Result<(f64, f64), String> {
        cam.keyread().unwrap_or_else(|e| stopped(&e)); // the first frame proves the camera works
        let ready = secs(t0);
        println!("@pairscan state=looking");
        let (mut reads, mut key) = (Vec::<String>::new(), None);
        let start = Instant::now();
        while key.is_none() {
            if secs(start) > 60.0 {
                println!("@pairscan state=timeout");
                return Err("no pairing key seen in 60 s: is it on the host's screen?".into());
            }
            let (got, still, skip) = cam.keyread().unwrap_or_else(|e| stopped(&e));
            if skip {
                println!("@pairscan state=cancelled");
                return Err("cancelled".into());
            }
            if let Some(got) = got.filter(|_| still) {
                // the key itself is never printed
                reads.push(got);
                let n = reads.len();
                if n >= 2 && reads[n - 2] == reads[n - 1] {
                    key = Some(reads[n - 1].clone());
                }
            }
        }
        let scanned = secs(t0);
        println!("@pairscan state=read");
        cam.say("Key read: pairing");
        cam.want(vec![]);
        machine::pair(&addr, key.as_deref().unwrap_or(""), replace)?;
        Ok((ready, scanned))
    })();
    cam.finish(); // the HUD goes
    let (ready, scanned) = r.unwrap_or_else(|e| die(e));
    let total = secs(t0);
    println!("@done pair total_s={total:.1} ready_s={ready:.1} scan_s={:.1} pair_s={:.1}", scanned - ready, total - scanned);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Fake {
        answers: VecDeque<Answer>,
        said: Arc<Mutex<Vec<String>>>,
    }

    impl Eye for Fake {
        fn say(&self, text: &str) {
            self.said.lock().unwrap().push(text.into());
        }
        fn want(&self, _: Vec<usize>) {}
        fn sizes(&self, _: Vec<(usize, f64)>) {}
        fn shots(&mut self, _: usize) -> Result<Answer, String> {
            self.answers.pop_front().ok_or_else(|| "no frame".into())
        }
        fn monitor(&mut self, _: Option<&str>) {}
        fn save(&self) {}
    }

    fn mon(name: &str, base: usize) -> Mon {
        let frame = cc_scan::pattern::frame(1920, 1080, base).json;
        Mon { name: name.into(), screen: 1, mm: json!([600, 340]), host: "h".into(), output: "DP-1".into(), pixels: (1920, 1080), draw: None,
              order: frame["tags"].as_object().unwrap().keys().cloned().collect(), tags: frame["tags"].as_object().cloned().unwrap(), dense_order: vec![], curve: Value::Null, radius: Value::Null, index: 0, base, frame, dense: None, agent: None }
    }

    fn shot(at: Option<V3>, found: Vec<usize>, skip: bool) -> Answer {
        Answer { at, skip, found }
    }

    /// Calibrate then capture on a fake camera. Every frame tag read 6 times sizes the grid from the
    /// smallest ladder tag (x1.35), a head moving 1 cm a frame over the whole grid completes it (24
    /// samples, 12 cm, 70% coverage), and a monitor whose screen went away is skipped with its why.
    #[test]
    fn calibrate_and_capture() {
        let (a, b) = (mon("a", 0), mon("b", 50));
        let ids_a = Mon::ids(&a.frame);
        let mut answers: VecDeque<Answer> = (0..6).map(|_| shot(Some([0.0, 1.6, 0.0]), ids_a.clone(), false)).collect();
        let side = a.frame["sides_px"].as_object().unwrap().values().filter_map(Value::as_i64).min().unwrap();
        let dense = cc_scan::pattern::dense(1920, 1080, 0, (side as f64 * 1.35) as i64).json;
        for k in 0..30 {
            answers.push_back(shot(Some([k as f64 * 0.01, 1.6, 0.0]), Mon::ids(&dense), false));
        }
        let said = Arc::new(Mutex::new(vec![]));
        let mut eye = Fake { answers, said: said.clone() };
        let gone = Gone::default();
        gone.add("b", Some("tags-limit".into()));
        let mut al = Align { eye: &mut eye, mons: vec![a, b], views: vec![View::None, View::None], gone, want: FAST, at: None, skip: false,
                             calibrate_for: 5.0, times: vec![], quick: false };
        al.calibrate().unwrap();
        assert_eq!(al.mons[0].dense.as_ref().unwrap()["side_px"], dense["side_px"]);
        assert!(al.mons[1].dense.is_none(), "b's screen went");
        al.capture().unwrap();
        assert!(al.mons[0].dense.is_some(), "a complete");
        drop(al);
        assert_eq!(eye.answers.len(), 30 - 20, "complete at 20 samples");
        let said = said.lock().unwrap();
        assert!(said.iter().any(|s| s == "move your head a little side to side: a 0%"), "{said:?}");
        assert_eq!(skipped_why("tags-limit"), "that machine's tag-screen limit for this hour is used up");
    }

    /// Entering a workspace: no calibrating, both monitors show only their four corner tags, and the
    /// first one looked at for 12 frames over 2 cm ends it. The other, never seen, isn't solved.
    #[test]
    fn quick_corners_end_with_the_first_monitor() {
        let (a, b) = (mon("a", 0), mon("b", 50));
        let answers: VecDeque<Answer> = (0..30).map(|k| shot(Some([k as f64 * 0.002, 1.6, 0.0]), vec![50, 51, 52, 53, 54], false)).collect();
        let said = Arc::new(Mutex::new(vec![]));
        let mut eye = Fake { answers, said: said.clone() };
        let mut al = Align { eye: &mut eye, mons: vec![a, b], views: vec![View::None, View::None], gone: Gone::default(), want: QUICK, at: None,
                             skip: false, calibrate_for: 5.0, times: vec![], quick: true };
        al.corners();
        assert_eq!(al.mons[1].tag_params("dense.png")["tags"].as_array().unwrap().len(), 4, "corner tags only");
        al.capture().unwrap();
        assert!(al.mons[1].dense.is_some() && al.mons[0].dense.is_none(), "b looked at, a not");
        assert_eq!(al.mons[1].order, ["50", "51", "52", "53"]);
        drop(al);
        assert_eq!(eye.answers.len(), 30 - 12, "done at 12 frames");
        assert!(said.lock().unwrap().iter().any(|s| s.starts_with("look at one of these monitors: a 0%, b 0%")));
        let p = |t: &str| Json::parse(t).unwrap();
        assert_eq!(saved_shape(&p(r#"{"curve": 1.0}"#)), (json!("h"), json!(1.0)));
        assert_eq!(saved_shape(&p(r#"{"curve": 0, "vcurve": 1.8}"#)), (json!("v"), json!(1.8)));
        assert_eq!(saved_shape(&p(r#"{"curve": 0}"#)), (json!("flat"), Value::Null));
    }

    /// The HUD clicked while calibrating: the monitors left get skipped, and there's nothing to solve.
    #[test]
    fn skip_while_calibrating() {
        let mut eye = Fake { answers: [shot(None, vec![], true)].into_iter().collect(), said: Default::default() };
        let mut al = Align { eye: &mut eye, mons: vec![mon("a", 0)], views: vec![View::None], gone: Gone::default(), want: FAST, at: None,
                             skip: false, calibrate_for: 5.0, times: vec![], quick: false };
        assert_eq!(al.run(Path::new("/nonexistent"), &Json::Null).unwrap(), None);
        // a camera that stops is an error (the caller cleans up, then says it)
        let mut eye = Fake { answers: VecDeque::new(), said: Default::default() };
        let mut al = Align { eye: &mut eye, mons: vec![mon("a", 0)], views: vec![View::None], gone: Gone::default(), want: FAST, at: None,
                             skip: false, calibrate_for: 5.0, times: vec![], quick: false };
        assert!(al.run(Path::new("/nonexistent"), &Json::Null).unwrap_err().contains("the camera stopped"));
    }

    #[test]
    fn tag_params_as_the_agent_draws_them() {
        let m = mon("a", 0);
        let p = m.tag_params("frame.png");
        assert_eq!(p["bg"], "white");
        let t0 = p["tags"].as_array().unwrap().iter().find(|t| t[0] == 0).unwrap().clone();
        let side = m.frame["corner_side_px"].as_i64().unwrap();
        assert_eq!(t0[3].as_i64().unwrap(), side, "{t0}");
        assert_eq!(m.tag_params("wait.png"), json!({"bg": "wait", "tags": []}));
        assert_eq!(m.tag_params("dense.png"), json!({"bg": "wait", "tags": []}), "no grid yet");
    }

    /// job.json has to match what cc-home.py's write_job wrote for the same monitors, byte for byte
    /// (recorded in tests/fixtures/align-cross/job.json).
    #[test]
    fn job_json_as_python_wrote_it() {
        let dir = std::env::temp_dir().join(format!("cc-home-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cam = "[1078.2911645173851, 1075.2974390465752, 958.7035450492425, 537.0302171748745, 0.00141565644505503, -0.0006644649349442721, 0.00040261202487396676, -0.031907220096217256, 0.0011620463083413279, 0.00034898336601051495]";
        std::fs::write(dir.join("mirror-camera.json"), cam).unwrap();
        // name, pixels, grid side, mm (as an agent sends it), curve, radius
        let spec = [("desk-wide", (5120i64, 1440i64), 150i64, json!([1193, 336]), json!("h"), json!(1.0)),
                    ("desk-portrait", (1440, 2560), 97, json!([393.5, 698.0]), Value::Null, Value::Null)];
        let mut mons = vec![];
        for (k, (name, px, side, mm, curve, radius)) in spec.iter().enumerate() {
            let mut m = mon(name, 50 * k);
            (m.pixels, m.mm, m.curve, m.radius, m.screen) = (*px, mm.clone(), curve.clone(), radius.clone(), 4 + k as i64);
            let lay = cc_scan::pattern::frame(px.0, px.1, m.base);
            (m.tags, m.order) = (lay.json["tags"].as_object().cloned().unwrap(), lay.rects.iter().map(|r| r.0.to_string()).collect());
            let d = cc_scan::pattern::dense(px.0, px.1, m.base, *side);
            m.tags.extend(d.json["tags"].as_object().cloned().unwrap());
            m.order.extend(d.rects.iter().map(|r| r.0.to_string()));
            mons.push(m);
        }
        let camera = Json::parse(cam).unwrap();
        write_job(&dir, &camera, &mons.iter().collect::<Vec<_>>());
        let root = env!("CARGO_MANIFEST_DIR").trim_end_matches("/crates/cc-home");
        let python = std::fs::read_to_string(format!("{root}/tests/fixtures/align-cross/job.json")).unwrap().replace("@WORK@", &dir.to_string_lossy());
        let rust = std::fs::read_to_string(dir.join("job.json")).unwrap();
        assert!(rust == python, "job.json differs:\nrust   {}\npython {}", &rust[..rust.len().min(600)], &python[..python.len().min(600)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// selftest-nossh's tag screens, shown and hidden through a real agent (cc-host serve --fake on a
    /// port of its own, so build cc-host first or set CC_HOST_BIN=), with the keep-alive's poll in
    /// between and nothing gone.
    #[test]
    fn agent_tag_screens() {
        use base64::Engine;
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let dir = std::env::temp_dir().join(format!("cc-home-agent-{}", std::process::id()));
        let (host, frame) = (dir.join("host"), dir.join("frame"));
        let fk = cc_proto::agent::frame_key_or_new(&frame).unwrap();
        let hk = cc_proto::agent::frame_key_or_new(&dir.join("hk")).unwrap();
        for (rel, text) in [("host-key", std::fs::read_to_string(dir.join("hk/frame-key")).unwrap()), ("host-id", "test-host".into()),
                            ("trusted-frames/steam-frame.pub", b64(&fk.verifying_key().to_bytes())), ("frames/steam-frame.json", r#"{"slot": 1}"#.into())] {
            std::fs::create_dir_all(host.join(rel).parent().unwrap()).unwrap();
            std::fs::write(host.join(rel), text).unwrap();
        }
        let port = 40000 + (std::process::id() % 20000) as u16;
        let root = env!("CARGO_MANIFEST_DIR").trim_end_matches("/crates/cc-home");
        let bin = std::env::var("CC_HOST_BIN").unwrap_or_else(|_| format!("{root}/target/aarch64-unknown-linux-musl/release/cc-host"));
        assert!(std::path::Path::new(&bin).exists(), "no {bin}: cargo build --release --target aarch64-unknown-linux-musl -p cc-host (or CC_HOST_BIN=)");
        let mut fake = std::process::Command::new(&bin).args(["serve", "--fake", "--port", &port.to_string()])
            .env("CC_CONF", &host).stdout(std::process::Stdio::piped()).spawn().unwrap();
        let mut out = std::io::BufReader::new(fake.stdout.take().unwrap());
        let mut line = String::new();
        std::io::BufRead::read_line(&mut out, &mut line).unwrap();
        assert!(line.contains("listening"), "{line}");
        std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink())); // drain its log, since a closed pipe would stop it
        let t = cc_proto::agent::Trusted { addr: "127.0.0.1".into(), host_pk: hk.verifying_key().to_bytes(), frame: "steam-frame".into() };
        let c = Client::connect(&t, &fk, port, Duration::from_secs(5)).unwrap();
        let mut m = mon("a", 0);
        m.draw = Some((1920, 1080));
        let gone = Gone::default();
        // two views on the machine's one connection (a second connection would close the first);
        // the fake host has one monitor, so both show it
        let (c, peers) = (Arc::new(Mutex::new(c)), Arc::new(vec![("a".to_owned(), 0), ("b".to_owned(), 0)]));
        let v = AgentView::start("a", 0, peers.clone(), c.clone(), gone.clone());
        let w = AgentView::start("b", 0, peers, c, gone.clone());
        v.put(m.tag_params("frame.png"));
        w.put(m.tag_params("frame.png"));
        std::thread::sleep(Duration::from_millis(500)); // the watches poll meanwhile
        v.put(m.tag_params("wait.png"));
        w.put(m.tag_params("wait.png"));
        assert!(!gone.has("a") && !gone.has("b"), "{} / {}", gone.why("a"), gone.why("b"));
        v.close();
        w.close();
        let _ = fake.kill();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// cc-home.py's selftest: a known panel's corners give the panel back, and the refit board stands
/// ahead of the head.
    #[test]
    fn geometry() {
        super::super::selftest();
    }
}
