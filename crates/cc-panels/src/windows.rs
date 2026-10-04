//! Frame windows as panels (docs/window-panels-design2.md). Every program window in the
//! nested session takes a slot in the window pool: a violet panel with the same card, grab bar
//! and spots as the krdp panels, showing KWin's screencast of that window (session.rs, capture.rs).
//! KWin's side is panels/cc-windows.js (kwin.rs), which reports the windows and lays them out on
//! the session's outputs. A panel is as many metres wide as its window has pixels over
//! `window_px_per_m` (settings.json, 1400 by default), so text stays 1:1. Resizing the panel by
//! a corner resizes the window (place), but the window's real size gets the last word.
//! Each window's original state goes to kwin-restore.json so kwin.rs can put it back on exit,
//! and its panel's pose goes to win-poses.json so a restart puts it back where it was.
//! Input goes back through KWin's fake_input (`mouse`, `key`, `wheel`, from any thread).
//! A window minimized in KWin (Plasma's taskbar, a v, a chip) is hidden here, and shown again
//! when it's back. Plasma's own panel and popups belong to plasmabar.rs, fed from here.
use crate::attention::{self, Level};
use crate::geometry::{Mat, Pose, angles, panel_matrix};
use crate::kvm::KVM;
use crate::session::{self, Session, StreamEvent};
use crate::{HIDDEN, Panel, call, capture, config, grab, kwin, panel, plasmabar, vr};
use freerdp_sys::{PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3, PTR_FLAGS_DOWN};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// ponytail: SteamVR caps overlays at 128 (k_unMaxOverlayCount), shared by every app (Steam's
/// dashboard, other overlay apps). A slot takes 2: the picture, plus its card with the bar and tag
/// drawn in. At 4 each, 16 slots hit the cap live (win-16). Make overlays on demand if 16 turns
/// out to be too few.
/// Any more windows wait on the unslotted list until a slot frees up.
pub const SLOTS: usize = 16;
const STUCK: Duration = Duration::from_millis(1500); // if a resized window's stream goes this long without following, reopen it
const WINDOW_FPS: u32 = 30; // a window's stream's frame-rate cap (docs/stutter-plan.md 4b)
const THEATER_FPS: u32 = 60; // the theater panel's cap, since it's video
const EDGE_FPS: u32 = 3; // cap for a panel that's peripheral (or quiet) for EDGE_AFTER: KWin's cap at or under our uploads (attention::PERIPHERAL_WINDOW)
const EDGE_AFTER: Duration = Duration::from_secs(5);
const GRACE: Duration = Duration::from_secs(15); // KWin gone: panels stay frozen this long
const NUDGE: Duration = Duration::from_millis(300); // no frame this long after the stream: wake the window
const PLACE_EVERY: Duration = Duration::from_millis(150); // window resizes while a corner is dragged
const SETTLE: Duration = Duration::from_millis(600); // after that, the window's own size wins
const OUTPUT: (u32, u32) = (2560, 1440); // Virtual-1, where the windows live (session/cc-desktop SIZE), so none gets bigger
const PARK: &str = "Virtual-2"; // an output no window is put on (session/cc-desktop): the pointer rests there when it's off our panels
const SHELL: usize = usize::MAX; // Input.hover: on Plasma's bar (plasmabar.rs)
const DRAGGED: Duration = Duration::from_secs(3); // a window made this soon after a window drag (a torn-off tab) goes where it was
const LIFT: f64 = 0.02; // a popup's panel sits this far in front of its parent's, so it's nearer for the pointer and painted over it
const POPUP_AFTER: Duration = Duration::from_secs(3); // a popup this soon after a click, a key or another popup is a menu; later, a tooltip

/// Pointer and keys into the session, from the input loop (our mouse and keyboards) and the
/// main loop (the lasers). KWin scripts can't see the stacking order, so we don't model it.
/// Instead a window gets raised when the pointer comes onto its panel, and a press waits for
/// its window's `raised` before it goes, so it can't land in another window sitting on top.
struct Input {
    session: Option<Arc<Session>>,
    cmds: Option<kwin::Queue>,
    raised: Option<String>, // the window last raised for us, as long as nothing else can be above it
    asked: Option<String>,  // the raise in flight: only its answer (seq) counts
    seq: u64,
    hover: Option<usize>,   // the window panel the pointer is on (SHELL: Plasma's bar; None held: off every window, drag_off)
    at: (f64, f64),         // where the pointer was last sent on a window
    pending: Option<Press>,
    down: u32, // buttons down in the session (bit: code - BTN_LEFT)
    wheel: f64, // wl_pointer units a notch (CC_WHEEL; 15 is libinput's)
    wait: Duration, // how long a press waits for its raise (CC_RAISE_MS)
    last: Option<Instant>, // the last button or key sent, or popup seen: a popup soon after is a menu, not a tooltip
}

/// A press waiting for its window to be raised.
struct Press {
    uuid: String,
    at: (f64, f64),
    button: u32,
    up: bool, // released meanwhile (a quick click), so it goes right after
    since: Instant,
}

static INPUT: Mutex<Input> = Mutex::new(Input {
    session: None,
    cmds: None,
    raised: None,
    asked: None,
    seq: 0,
    hover: None,
    at: (0.0, 0.0),
    pending: None,
    down: 0,
    wheel: 15.0,
    wait: Duration::from_millis(100),
    last: None,
});

impl Input {
    /// Raise a window. Nothing counts as raised until its answer comes back, since a raise of
    /// another window may still be ahead of a press in KWin.
    fn raise(&mut self, uuid: &str) {
        if self.raised.as_deref() == Some(uuid) || self.asked.as_deref() == Some(uuid) {
            return;
        }
        let Some(q) = self.cmds.clone() else { return };
        self.covered();
        self.asked = Some(uuid.into());
        q.lock().unwrap().push(json!({"c": "raise", "uuid": uuid, "n": self.seq}));
        kwin::kick();
    }

    /// KWin's answer to raise n: true when it's the latest and still holds, the window now on top.
    fn answered(&mut self, uuid: &str, n: Option<u64>) -> bool {
        if n != Some(self.seq) || self.asked.as_deref() != Some(uuid) {
            return false; // overtaken by a later raise, or something got mapped/activated since
        }
        (self.raised, self.asked) = (Some(uuid.into()), None);
        true
    }

    /// Something may be above the window we raised now, so answers to raises already asked are stale.
    fn covered(&mut self) {
        (self.raised, self.asked) = (None, None);
        self.seq += 1;
    }

    /// A button, once only, because KWin would pass on an unmatched release.
    fn button(&mut self, s: &Session, code: u32, down: bool) {
        let bit = 1 << (code - session::BTN_LEFT);
        if down == (self.down & bit != 0) {
            return;
        }
        self.down ^= bit;
        self.last = Some(Instant::now());
        s.button(code, down);
    }
}

/// RDP's button flags (what kvm.rs and the lasers speak), as evdev's.
fn button_code(flags: u32) -> Option<u32> {
    match flags & (PTR_FLAGS_BUTTON1 | PTR_FLAGS_BUTTON2 | PTR_FLAGS_BUTTON3) {
        PTR_FLAGS_BUTTON1 => Some(session::BTN_LEFT),
        PTR_FLAGS_BUTTON2 => Some(session::BTN_RIGHT),
        PTR_FLAGS_BUTTON3 => Some(session::BTN_MIDDLE),
        _ => None,
    }
}

/// A point x, y in a window's stream (`stream_w` pixels wide) as a point in the session. A
/// popup's panel maps to the popup's own place, not its parent's.
fn session_point(cg: [i32; 4], stream_w: u32, x: f64, y: f64) -> (f64, f64) {
    let px = stream_w as f64 / cg[2].max(1) as f64; // stream pixels per session unit
    (cg[0] as f64 + x / px, cg[1] as f64 + y / px)
}

/// The pointer on a window panel, at x, y in its stream's pixels from the top left.
pub fn mouse(p: &Panel, flags: u32, x: f64, y: f64) {
    let Some(w) = win(p.index) else { return };
    let at = session_point(w.cg, p.size().0, x, y);
    // A popup is above its parent already, and the script can't raise it (it isn't adopted), so
    // nothing waits for a raise there.
    let popup = w.popup();
    let press = flags & PTR_FLAGS_DOWN != 0 && button_code(flags).is_some();
    if press {
        // Plasma's open popup is above every window (a raise can't lift one past it), and this
        // window is often under it (both on WL-0). So first a click outside it, to close it.
        shell_outside();
    }
    // A point under Plasma's panel or popup reaches that, not this window, so the pointer doesn't
    // go there and a press there only closes the popup. A release always goes, or it'd stay held.
    let under = plasmabar::covers(at.0, at.1);
    let mut s = INPUT.lock().unwrap();
    let Some(ses) = s.session.clone() else { return };
    if s.hover.is_none() && s.down != 0 && !press && let Some(b) = button_code(flags) {
        return let_go_off(&mut s, &ses, b); // a drag taken off every window (drag_off) ends there
    }
    // A drag carried here from another window panel raises this one too. KWin's own drags
    // raise what they hover, so its drop lands here (the raise's answer moves it again).
    if s.hover != Some(p.index) {
        s.hover = Some(p.index);
        if !popup {
            s.raise(&w.uuid);
        }
    }
    if under && s.down == 0 && (press || button_code(flags).is_none()) {
        return;
    }
    ses.pointer_to(at.0, at.1);
    s.at = at;
    let Some(b) = button_code(flags) else { return };
    if flags & PTR_FLAGS_DOWN == 0 {
        match s.pending.as_mut().filter(|pr| pr.button == b) {
            Some(pr) => pr.up = true,
            None => s.button(&ses, b, false),
        }
    } else if popup || s.raised.as_deref() == Some(w.uuid.as_str()) {
        s.button(&ses, b, true);
    } else {
        s.raise(&w.uuid);
        s.pending = Some(Press { uuid: w.uuid, at, button: b, up: false, since: Instant::now() });
    }
}

/// A key (evdev code) to the session's focused window. The kernel's repeats (2) get dropped
/// because the app repeats a held key itself, at the session's rate.
pub fn key(code: u16, value: i32) {
    let s = {
        let mut i = INPUT.lock().unwrap();
        i.last = Some(Instant::now());
        i.session.clone()
    };
    if let Some(s) = s.filter(|_| value != 2) {
        s.key(code as u32, value != 0);
    }
}

/// Wheel notches, positive up or right (evdev's way).
pub fn wheel(horizontal: bool, notches: f64) {
    let s = INPUT.lock().unwrap();
    if let Some(ses) = &s.session {
        ses.axis(horizontal, notches * s.wheel * if horizontal { 1.0 } else { -1.0 });
    }
}

/// The pointer left every window panel, so it goes to rest on the park output, which closes
/// tooltips and clears hover states. Not while a button is held in the session, since that's a drag.
pub fn leave() {
    let mut s = INPUT.lock().unwrap();
    if s.hover.is_none() || s.down != 0 {
        return;
    }
    s.hover = None;
    if let Some(ses) = &s.session {
        park(ses);
    }
}

/// A drag held from a window panel went off every window panel, so the pointer goes to the
/// park output, where no window is. That way a browser tab let go there becomes a window of its
/// own (Chromium makes one when a tab is dropped outside every tab strip). Back on a window
/// panel, the pointer moves there again.
/// ponytail: an app's own drag off the edge (a scrollbar) sees the pointer jump to the park
/// past TEAR_OFF (kvm.rs), like a real pointer far off would; KWin doesn't tell us whether it's a DnD.
pub fn drag_off() {
    let mut s = INPUT.lock().unwrap();
    if s.down == 0 || s.hover.is_none_or(|h| h == SHELL) {
        return;
    }
    s.hover = None;
    if let Some(ses) = s.session.clone() {
        park(&ses);
    }
}

/// A button let go off every window (drag_off) is let go there, on the park. A window that
/// came along (a browser's torn-off tab following the pointer) goes back to the windows' output,
/// so the park stays empty for the next one.
fn let_go_off(s: &mut Input, ses: &Session, b: u32) {
    s.button(ses, b, false);
    let (Some(q), Some(o)) = (s.cmds.clone(), ses.output(PARK)) else { return };
    let sc = o.scale.max(1);
    let on_park = |c: [i32; 4]| (o.x..o.x + o.w / sc).contains(&(c[0] + c[2] / 2)) && (o.y..o.y + o.h / sc).contains(&(c[1] + c[3] / 2));
    for w in crate::panels().iter().filter_map(|p| win(p.index)).filter(|w| on_park(w.cg)) {
        q.lock().unwrap().push(json!({"c": "place", "uuid": w.uuid, "w": 0, "h": 0}));
    }
    kwin::kick();
}

/// Moves the pointer to the middle of the park output; false if there isn't one.
/// ponytail: with no park output (the stock desktop's one screen) the pointer stays put; S6
fn park(ses: &Session) -> bool {
    let Some(o) = ses.output(PARK) else { return false };
    let sc = o.scale.max(1) as f64;
    ses.pointer_to(o.x as f64 + o.w as f64 / sc / 2.0, o.y as f64 + o.h as f64 / sc / 2.0);
    true
}

/// The pointer on Plasma's bar or one of its popups (plasmabar.rs), at a point in the
/// session (None: wherever it is), with a button in RDP's flags. There's no raise gate because
/// they're above every window. Coming back onto a window panel raises that window again.
pub fn shell_mouse(flags: u32, at: Option<(f64, f64)>) {
    let mut s = INPUT.lock().unwrap();
    let Some(ses) = s.session.clone() else { return };
    s.hover = Some(SHELL);
    if let Some((x, y)) = at {
        ses.pointer_to(x, y);
    }
    if let Some(b) = button_code(flags) {
        s.button(&ses, b, flags & PTR_FLAGS_DOWN != 0);
    }
}

/// A press outside Plasma's open popup (on a krdp panel, a card, our taskbar, or a window
/// panel whose window may be under it) clicks on the park output, so Plasma closes the popup
/// like it would for any click outside it. A window panel's press then waits for its raise
/// again (hover is off).
/// ponytail: the raise's answer doesn't prove the popup's gone, but a press still under
/// it is dropped (`mouse`), so it can't land in it.
/// ponytail: a laser click into nothing (SteamVR's own space) leaves it open.
pub fn shell_outside() {
    if !plasmabar::OPEN.load(Relaxed) {
        return;
    }
    let mut s = INPUT.lock().unwrap();
    let Some(ses) = s.session.clone().filter(|_| s.down == 0) else { return };
    if park(&ses) {
        s.button(&ses, session::BTN_LEFT, true);
        s.button(&ses, session::BTN_LEFT, false);
        s.hover = None;
    }
}

/// A window in a slot (or waiting for one), as the script reports it.
#[derive(Clone, Debug, Default)]
pub struct Win {
    pub uuid: String,
    pub key: String, // its spot: app:<desktop file name, else resource class>
    pub app: String,
    pub caption: String,
    pub cg: [i32; 4], // clientGeometry in the session: x, y, w, h
    pub parent: String, // a dialog's window ("" for none)
    pub kind: String,   // window | dialog | popup
    pub minimized: bool, // minimized in KWin, so its panel is hidden
}

pub fn geom(e: &Value) -> [i32; 4] {
    ["x", "y", "w", "h"].map(|k| e[k].as_f64().unwrap_or(0.0).round() as i32)
}

impl Win {
    pub fn popup(&self) -> bool {
        self.kind == "popup"
    }

    fn from(e: &Value) -> Option<Win> {
        let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
        let (uuid, app) = (s("uuid"), s("app"));
        if uuid.is_empty() {
            return None;
        }
        let key = format!("app:{}", if app.is_empty() { "unknown" } else { &app });
        let minimized = e["minimized"].as_bool().unwrap_or(false);
        Some(Win { uuid, key, app, caption: s("caption"), cg: geom(e), parent: s("parent"), kind: s("kind"), minimized })
    }
}

/// A slot's stream, on the main thread.
#[derive(Default)]
struct Slot {
    stream: Option<session::Stream>,
    feed: Option<Arc<capture::Feed>>,
    shown: capture::Shown,
    asked: Option<Instant>,   // stream_window sent
    created: Option<Instant>, // KWin made its node
    nudged: u8,
    placing: Option<(Instant, (u32, u32))>, // the last size sent to the window, and when
    tag: Option<mpsc::Receiver<()>>, // assets.rs drawing its tag
    frames: u32,                     // shown, as of the last status line
    arrived: u32,                    // and arrived
    theater: Option<(Pose, f64)>,    // in theater mode: where it was before, and its height
    anchored: bool,                  // last resized by its bottom-right corner, so it hangs from its top-left
    fmt: ((u32, u32), (i32, i32)),   // the stream's size as last seen, and the window's then
    stuck: Option<Instant>,          // the window resized since, but its stream hasn't
    paused: bool,                    // its stream is paused (attention.rs), so KWin renders nothing for it
    last_up: Option<Instant>,        // its last frame up; a peripheral panel only gets a few a second
    fps: u32,                        // its stream's frame-rate cap, as last asked
    edge: Option<Instant>,           // peripheral since
    follow: Option<(Mat, f64)>,      // a popup's place on its parent's panel, as last set
}

/// A panel centred at the cursor's point p, facing you level (like `ahead`), 10 cm nearer so
/// it's in front of the panel the drag was on.
fn at_cursor(p: [f64; 3]) -> Pose {
    let eye = vr::head_position();
    let f = [p[0] - eye[0], 0.0, p[2] - eye[2]];
    let f = f.map(|c| c / crate::geometry::norm(&f).max(1e-6));
    let (yaw, pitch) = angles(&f);
    Pose { centre: [p[0] - f[0] * 0.1, p[1], p[2] - f[2] * 0.1], yaw, pitch, width: 1.0, ..Default::default() }
}

/// The window size a w × h metre panel wants at `density` pixels a metre, as sent. Never
/// bigger than the output.
fn window_size(w: f64, h: f64, density: f64) -> (u32, u32) {
    (((w * density).round() as u32).clamp(1, OUTPUT.0), ((h * density).round() as u32).clamp(1, OUTPUT.1))
}

static DENSITY: AtomicU64 = AtomicU64::new(0); // Windows::density (f64 bits), for max_size

/// window_px_per_m as currently set (the Frame windows' pixels per metre), 1400 until it's read.
pub fn px_per_m_now() -> f64 {
    Some(f64::from_bits(DENSITY.load(Relaxed))).filter(|d| *d > 0.0).unwrap_or(1400.0)
}

/// The biggest window panel p can be, in metres: its window as big as the output. grab.rs
/// stops a bottom-right stretch there.
pub fn max_size(p: &Panel) -> (f64, f64) {
    let d = f64::from_bits(DENSITY.load(Relaxed)) / p.zoom();
    if d > 0.0 { (OUTPUT.0 as f64 / d, OUTPUT.1 as f64 / d) } else { (f64::MAX, f64::MAX) }
}

/// Panel `pl` resized to nw × nh: about its centre, or hanging from its top-left when
/// `anchored` (stretched by its bottom-right corner, grab.rs).
pub fn resized(pl: &crate::geometry::Placement, nw: f64, nh: f64, anchored: bool) -> Mat {
    if anchored { pl.on_surface((nw - pl.width) / 2.0, (pl.height - nh) / 2.0, 0.0) } else { pl.matrix() }
}

/// A popup's panel on its parent's: at the popup's offset from its parent in the session, at the
/// parent panel's metres per session pixel, LIFT in front. Its matrix and width.
/// ponytail: flat, even on a curved parent (it touches the curve where it opened); bend it if a
/// wide menu on a tight curve bothers anyone.
fn popup_place(parent: &crate::geometry::Placement, pcg: [i32; 4], cg: [i32; 4]) -> (Mat, f64) {
    let (sx, sy) = (parent.width / pcg[2].max(1) as f64, parent.height / pcg[3].max(1) as f64);
    let mid = |g: [i32; 4]| (g[0] as f64 + g[2] as f64 / 2.0, g[1] as f64 + g[3] as f64 / 2.0);
    let ((x, y), (px, py)) = (mid(cg), mid(pcg));
    (parent.on_surface((x - px) * sx, (py - y) * sy, LIFT), cg[2] as f64 * sx)
}

/// Whether a popup is a menu to show: a submenu, or one that came soon after a click, a key or
/// another popup. One that comes from hovering alone is a tooltip (Qt's are popups on Wayland).
fn popup_wanted(parent_popup: bool, last: Option<Instant>, now: Instant) -> bool {
    parent_popup || last.is_some_and(|t| now.saturating_duration_since(t) < POPUP_AFTER)
}

/// Whether panel i shows an app's popup (a menu): no card, no chip, and it rides on its parent's panel.
pub fn is_popup(i: usize) -> bool {
    crate::panels().get(i).is_some_and(|_| win(i).is_some_and(|w| w.popup()))
}

/// The panel a popup panel hangs from in the end (a submenu's menu's window), or i itself.
pub fn root(i: usize) -> usize {
    let mut i = i;
    for _ in 0..8 {
        // a menu's submenu's submenu at most, and never a loop
        let Some(w) = crate::panels().get(i).and_then(|_| win(i)).filter(Win::popup) else { break };
        let Some(j) = crate::panels().iter().find(|p| win(p.index).is_some_and(|o| o.uuid == w.parent)) else { break };
        i = j.index;
    }
    i
}

/// The STUCK watchdog's wait: STUCK, or six of the stream's frame intervals if that's longer
/// (at its cap, `fps`; a peripheral panel only takes a frame every PERIPHERAL_WINDOW, a quiet one every QUIET_EVERY).
fn stuck_after(level: Level, fps: u32) -> Duration {
    let ours = match level {
        Level::Peripheral => attention::PERIPHERAL_WINDOW,
        Level::Quiet => attention::QUIET_EVERY,
        _ => Duration::ZERO,
    };
    let interval = (Duration::from_secs(1) / fps.max(1)).max(ours);
    STUCK.max(interval * 6)
}

pub struct Windows {
    first: usize, // panel index of slot 0
    slots: Vec<Slot>,
    unslotted: Vec<Win>, // oldest first
    session: Option<Arc<Session>>,
    sessions: mpsc::Receiver<Arc<Session>>,
    kwin: kwin::Kwin,
    cap: capture::Capture,
    saved: Map<String, Value>, // kwin-restore.json: each window's state from before we changed it
    poses: Map<String, Value>, // win-poses.json: each window's panel, by uuid
    resumed: Map<String, Value>, // hibernated apps' panels by key, each taken once (take_resumed)
    seen: HashSet<String>,     // windows the script has reported since it (re)loaded
    lost: Option<Instant>,     // KWin's bus went
    density: f64,              // window pixels per metre
    plasma: plasmabar::Plasma, // Plasma's own taskbar, placed by taskbar.rs
}

fn win(i: usize) -> Option<Win> {
    match &panel(i).src {
        crate::Source::Window(w) => w.lock().unwrap().clone(),
        crate::Source::Rdp => None,
    }
}

fn set_win(i: usize, w: Option<Win>) {
    if let crate::Source::Window(m) = &panel(i).src {
        *m.lock().unwrap() = w;
    }
}

/// The app's name for its tag: Name= from its .desktop file (ours, the container's, or the
/// host's through distrobox's /run/host), else the last part of its id.
fn app_name(app: &str) -> String {
    let home = config::home_dir();
    let dirs = [format!("{home}/.local/share/applications"), "/usr/share/applications".into(), "/run/host/usr/share/applications".into()];
    let name = dirs
        .iter()
        .filter_map(|d| std::fs::read_to_string(format!("{d}/{app}.desktop")).ok())
        .find_map(|t| t.lines().find_map(|l| l.strip_prefix("Name=").map(str::to_string)))
        .unwrap_or_else(|| app.rsplit('.').next().unwrap_or(app).to_string());
    name.replace(',', " ")
}

/// A file-name-safe id for an app's tag.
fn tag_id(app: &str) -> String {
    app.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

/// Where the apps cc-home's hibernate reopened should go (new windows, new uuids): key ->
/// [pose], one per window it had. Taken once, then the file moves on to .placed.
fn take_resumed() -> Map<String, Value> {
    let path = config::cache("desktop-hibernate.json.restored");
    let mut out = Map::new();
    for a in read_map(&path).get("apps").and_then(Value::as_array).into_iter().flatten() {
        if let Some(app) = a["app"].as_str() {
            out.insert(format!("app:{app}"), a["poses"].clone());
        }
    }
    let _ = std::fs::rename(&path, config::cache("desktop-hibernate.json.placed"));
    out
}

fn read_map(path: &str) -> Map<String, Value> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

impl Windows {
    pub fn start(first: usize) -> Windows {
        let (tx, sessions) = mpsc::channel();
        let kwin = kwin::Kwin::start();
        {
            let mut s = INPUT.lock().unwrap();
            s.cmds = Some(kwin.queue());
            let knob = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0);
            s.wheel = knob("CC_WHEEL").unwrap_or(s.wheel);
            s.wait = knob("CC_RAISE_MS").map_or(s.wait, |ms| Duration::from_millis(ms as u64));
        }
        std::thread::spawn(move || session::run(|| session::Env::discover(session::DESKTOP), |s| drop(tx.send(s)), &crate::QUIT));
        let density = config::settings()["window_px_per_m"].as_f64().filter(|d| *d > 100.0).unwrap_or(1400.0);
        DENSITY.store(density.to_bits(), Relaxed);
        Windows {
            first,
            slots: (0..SLOTS).map(|_| Slot::default()).collect(),
            unslotted: Vec::new(),
            session: None,
            sessions,
            kwin,
            cap: capture::Capture::start(),
            saved: Map::new(), // a crash's file gets restored (and moved aside) before the script saves anew
            poses: read_map(&config::cache("win-poses.json")),
            resumed: take_resumed(),
            seen: HashSet::new(),
            lost: None,
            density,
            plasma: plasmabar::Plasma::new(),
        }
    }

    fn slot_of(&self, uuid: &str) -> Option<usize> {
        (0..SLOTS).find(|&i| win(self.first + i).is_some_and(|w| w.uuid == uuid))
    }

    /// Every frame: news from the session and the script, frames onto the panels, sizes synced both ways.
    pub fn tick(&mut self, grab: &mut grab::Grab) {
        if let Some(s) = self.sessions.try_iter().last() {
            eprintln!("windows: session up ({:?} grant, screencast {}, fake input {})", s.grant, s.screencast.is_some(), s.fake.is_some());
            INPUT.lock().unwrap().session = Some(s.clone());
            self.session = Some(s);
            for i in 0..SLOTS {
                if win(self.first + i).is_some() && !panel(self.first + i).minimized() {
                    self.open(i); // a new connection, so every window streams anew
                }
            }
        }
        while let Ok(e) = self.kwin.events.try_recv() {
            self.event(&e, grab);
        }
        for (i, c) in grab.take_pressed() {
            if let Some(slot) = i.checked_sub(self.first).filter(|&s| s < SLOTS) {
                self.control(slot, c, grab);
            } else if c == grab::Ctl::Close && panel(i).v.pop.is_some() {
                crate::disconnect(panel(i)); // a pop-out's x: its RDP thread stops the window's server and frees the slot
            }
        }
        if self.lost.is_some_and(|t| t.elapsed() > GRACE) {
            eprintln!("windows: KWin gone for {} s: window panels closed (their places are kept)", GRACE.as_secs());
            self.lost = None;
            self.unslotted.clear();
            for i in 0..SLOTS {
                self.free(i, grab);
            }
        }
        for i in 0..SLOTS {
            self.tick_slot(i, grab);
        }
        let mut s = INPUT.lock().unwrap();
        let wait = s.wait;
        if let Some(pr) = s.pending.take_if(|pr| pr.since.elapsed() > wait) {
            // ponytail: dropped silently in the panel (no press glow); losing a click beats the wrong window getting it
            eprintln!("windows: a press dropped: its window wasn't raised within {} ms ({})", s.wait.as_millis(), pr.uuid);
        }
    }

    fn event(&mut self, e: &Value, grab: &mut grab::Grab) {
        let uuid = e["uuid"].as_str().unwrap_or_default();
        match e["e"].as_str().unwrap_or_default() {
            "hello" => {
                (self.seen, self.lost) = (HashSet::new(), None);
                self.plasma.clear();
            }
            "shell" => self.plasma.event(e),
            "ready" => {
                // everything open has been reported, so whatever wasn't is gone
                for i in 0..SLOTS {
                    if win(self.first + i).is_some_and(|w| !self.seen.contains(&w.uuid)) {
                        self.free(i, grab);
                    }
                }
                self.unslotted.retain(|w| self.seen.contains(&w.uuid));
                self.poses.retain(|u, _| self.seen.contains(u));
                eprintln!("windows: {} open", self.seen.len());
                self.fill(grab);
            }
            "add" => {
                INPUT.lock().unwrap().covered(); // a new window maps on top
                let Some(w) = Win::from(e) else { return };
                if w.popup() && self.slot_of(&w.uuid).is_none() {
                    let parent = self.slot_of(&w.parent).and_then(|j| win(self.first + j));
                    let mut s = INPUT.lock().unwrap();
                    if parent.as_ref().is_none_or(|pw| !popup_wanted(pw.popup(), s.last, Instant::now())) {
                        return; // a tooltip, or its window has no panel to show it on
                    }
                    s.last = Some(Instant::now()); // a menu bar's next menu, opened by hovering, is wanted too
                }
                self.seen.insert(w.uuid.clone());
                if let Some(i) = self.slot_of(&w.uuid) {
                    let min = w.minimized;
                    set_win(self.first + i, Some(w)); // known already (the script reloaded): same slot, same place
                    if min != panel(self.first + i).minimized() {
                        self.minimize(i, min, grab);
                    }
                } else if let Some(u) = self.unslotted.iter_mut().find(|u| u.uuid == w.uuid) {
                    *u = w;
                } else {
                    self.unslotted.push(w);
                    self.fill(grab);
                }
            }
            "remove" => {
                let mut s = INPUT.lock().unwrap();
                s.covered();
                if self.slot_of(uuid).and_then(|i| win(self.first + i)).is_some_and(|w| w.popup()) {
                    s.last = Some(Instant::now());
                }
                drop(s);
                self.unslotted.retain(|w| w.uuid != uuid);
                if let Some(i) = self.slot_of(uuid) {
                    self.free(i, grab);
                    self.fill(grab);
                }
            }
            "minimized" => {
                // Minimized in KWin (Plasma's taskbar, or our v or chip), its panel hides. When it's
                // back, it shows again where it was on a new stream's first frame (opened only now,
                // because one opened while it was minimized can stay blank), and that ends another
                // panel's theater, which would otherwise keep it hidden.
                let on = e["on"].as_bool().unwrap_or(false);
                if let Some(i) = self.slot_of(uuid) {
                    let mut w = win(self.first + i).unwrap();
                    w.minimized = on;
                    set_win(self.first + i, Some(w));
                    if on != panel(self.first + i).minimized() {
                        if !on && crate::THEATER.load(Relaxed) != self.first + i {
                            self.end_theater(grab);
                        }
                        self.minimize(i, on, grab);
                    }
                } else if let Some(w) = self.unslotted.iter_mut().find(|w| w.uuid == uuid) {
                    w.minimized = on;
                }
            }
            "full" => {
                // An app's own full screen is theater mode (like Steam's), and leaving it ends theater.
                let on = e["on"].as_bool().unwrap_or(false);
                if let Some(i) = self.slot_of(uuid).filter(|&i| self.slots[i].theater.is_some() != on) {
                    self.theater(i, grab);
                }
            }
            k @ ("geom" | "caption") => {
                let edit = |w: &mut Win| if k == "geom" { w.cg = geom(e) } else { w.caption = e["caption"].as_str().unwrap_or_default().into() };
                if let Some(i) = self.slot_of(uuid) {
                    let mut w = win(self.first + i).unwrap();
                    edit(&mut w);
                    set_win(self.first + i, Some(w));
                } else if let Some(w) = self.unslotted.iter_mut().find(|w| w.uuid == uuid) {
                    edit(w);
                }
            }
            "saved" => {
                // only the first one, since a reloaded script sees our changes, not the original
                if !uuid.is_empty() && !self.saved.contains_key(uuid) {
                    self.saved.insert(uuid.into(), e["s"].clone());
                    if let Err(err) = config::write_json(&kwin::restore_file(), &Value::Object(self.saved.clone())) {
                        eprintln!("windows: can't save {}: {err}", kwin::restore_file());
                    }
                }
            }
            "raised" => {
                let mut s = INPUT.lock().unwrap();
                if !s.answered(uuid, e["n"].as_u64()) {
                    return;
                }
                if let Some(pr) = s.pending.take_if(|pr| pr.uuid == uuid)
                    && let Some(ses) = s.session.clone()
                {
                    ses.pointer_to(pr.at.0, pr.at.1);
                    s.button(&ses, pr.button, true);
                    if pr.up {
                        s.button(&ses, pr.button, false);
                    }
                } else if s.down != 0
                    && s.hover.and_then(|h| (h != SHELL).then(|| win(h)).flatten()).is_some_and(|w| w.uuid == uuid)
                    && let Some(ses) = s.session.clone()
                {
                    // A drag carried onto it. KWin picks a drop target as the pointer moves, and the
                    // move here went out before the raise, so nudge it a pixel over and back now it's on top.
                    let (x, y) = s.at;
                    ses.pointer_to(x + 1.0, y);
                    ses.pointer_to(x, y);
                }
            }
            "activated" => {
                // activating the one we raised keeps it on top; anything else may cover it
                let mut s = INPUT.lock().unwrap();
                if s.raised.as_deref() != Some(uuid) && s.asked.as_deref() != Some(uuid) {
                    s.covered();
                }
            }
            "lost" => {
                eprintln!("windows: KWin's script is gone: panels stay frozen for {} s", GRACE.as_secs());
                self.lost = Some(Instant::now());
            }
            other => eprintln!("windows: unknown event {other:?}"),
        }
    }

    /// Waiting windows into free slots, oldest first.
    fn fill(&mut self, grab: &mut grab::Grab) {
        while !self.unslotted.is_empty() {
            let Some(i) = (0..SLOTS).find(|&i| win(self.first + i).is_none()) else { return };
            let w = self.unslotted.remove(0);
            self.adopt(i, w, grab);
        }
    }

    fn adopt(&mut self, i: usize, w: Win, grab: &mut grab::Grab) {
        let p = panel(self.first + i);
        let (cw, ch) = (w.cg[2].max(1) as u32, w.cg[3].max(1) as u32);
        if w.popup() {
            if self.slot_of(&w.parent).is_none() {
                return; // its window went meanwhile, and the popup with it
            }
            p.set_size(cw, ch);
            p.set_zoom(1.0);
            grab.cancel(p.index);
            eprintln!("{}: {} popup ({}x{}) on its window's panel", p.v.name, w.key, cw, ch);
            set_win(p.index, Some(w));
            self.slots[i].follow = None;
            self.place_popup(i);
            return self.open(i); // shown on its first frame
        }
        p.set_size(cw, ch);
        // Where it was (a restart, or a hibernated app reopened), else its app's spot (the first one open), else `ahead`.
        let app_open = (0..SLOTS).any(|j| win(self.first + j).is_some_and(|o| o.key == w.key && !o.popup()));
        let hibernated = || self.resumed.get_mut(&w.key)?.as_array_mut().filter(|v| !v.is_empty()).map(|v| v.remove(0));
        let (pose, zoom, from) = match self.poses.get(&w.uuid).cloned().or_else(hibernated).and_then(|v| Some((config::pose_from(&v)?, v["zoom"].as_f64()))) {
            Some((pose, zoom)) => (pose, zoom, "where it was"),
            None => match config::home_pose(&w.key, None).filter(|_| !app_open) {
                Some(pose) => (pose, config::home_zoom(&w.key), "its spot"),
                None => match KVM.lock().unwrap().window_drag.filter(|d| d.1.elapsed() < DRAGGED) {
                    Some((p, _)) => (at_cursor(p), None, "where the drag was"),
                    None => (self.ahead(), None, "ahead"),
                },
            },
        };
        p.set_zoom(zoom.unwrap_or(1.0));
        grab.cancel(p.index);
        KVM.lock().unwrap().set_pose(p.index, &panel_matrix(&pose), cw as f64 / self.density(p), pose.curve);
        eprintln!("{}: {} {:?} ({}x{}), {from}", p.v.name, w.key, w.caption, cw, ch);
        let (app, min) = (w.app.clone(), w.minimized);
        set_win(p.index, Some(w));
        self.tag(i, &app);
        // minimized in KWin: hidden until it's back, since its stream would be blank
        p.set_minimized(min);
        if !min {
            self.open(i);
        }
    }

    /// A window control on its card: close the window (like its own close button, so it may ask
    /// to save first in a dialog panel), turn theater mode on or off, or hide the panel.
    fn control(&mut self, i: usize, c: grab::Ctl, grab: &mut grab::Grab) {
        let p = panel(self.first + i);
        let Some(w) = win(p.index) else { return };
        eprintln!("{}: {c:?} pressed", p.v.name);
        match c {
            grab::Ctl::Close => self.kwin.send(json!({"c": "close", "uuid": w.uuid})),
            grab::Ctl::Theater => self.theater(i, grab),
            grab::Ctl::Minimize => self.toggle(p.index, grab),
        }
    }

    /// A taskbar chip (or a window's v) on panel `i`, for a window or a remote machine. If it's
    /// in sight, it hides (the theater panel leaves theater first), and a window gets minimized in
    /// KWin too. Otherwise it shows again (ending another panel's theater, which would keep it
    /// hidden), the keyboard goes to it, and a window is activated (un-minimized and raised). While
    /// hidden, its picture and card go away (grab.rs) and catch nothing.
    pub fn toggle(&mut self, i: usize, grab: &mut grab::Grab) {
        let p = panel(i);
        let slot = i.checked_sub(self.first).filter(|&s| s < SLOTS);
        let w = win(i);
        if matches!(p.src, crate::Source::Window(_)) && w.is_none() {
            return; // its window went this frame (a chip laid out last frame)
        }
        // in sight as its chip shows it: one with no first frame yet is dim, so it shows
        if p.live() && !p.away() {
            match (slot, w) {
                (Some(s), Some(w)) => {
                    self.minimize(s, true, grab);
                    self.kwin.send(json!({"c": "minimize", "uuid": w.uuid}));
                }
                _ => {
                    p.set_minimized(true);
                    // like free(): its held keys go up; typing stays put until a panel is clicked
                    let mut k = KVM.lock().unwrap();
                    if k.kbd == i {
                        k.release_keys();
                    }
                }
            }
            eprintln!("{}: hidden", p.v.name);
            return;
        }
        match (slot, w) {
            (Some(s), Some(w)) => {
                if p.minimized() && !w.minimized {
                    self.minimize(s, false, grab); // hidden here only (KWin wouldn't minimize it)
                }
                self.kwin.send(json!({"c": "activate", "uuid": w.uuid})); // minimized there, so its event will show it
            }
            _ => p.set_minimized(false),
        }
        self.end_theater(grab);
        KVM.lock().unwrap().type_to(i);
        eprintln!("{}: shown", p.v.name);
    }

    /// Slot i's window got minimized (in KWin, or by us ahead of it), so its panel hides and its
    /// stream closes: a minimized window's stream goes blank, and KWin needn't render it. When it's
    /// back, it gets a new stream and the panel shows where it was on its first frame (an idle one
    /// gets nudged).
    fn minimize(&mut self, i: usize, on: bool, grab: &mut grab::Grab) {
        let p = panel(self.first + i);
        p.set_minimized(on);
        if !on {
            call!(ov, HideOverlay, p.overlay); // hide before its buffers go; it's up again on the new stream's first frame
            p.set_live(false);
            return self.open(i);
        }
        if self.slots[i].theater.is_some() {
            self.theater(i, grab);
        }
        call!(ov, HideOverlay, p.overlay); // hide before its buffers go
        self.close_stream(i);
        p.set_live(false);
        // like free(): its held keys go up; typing stays put until a panel is clicked
        let mut k = KVM.lock().unwrap();
        if k.kbd == p.index {
            k.release_keys();
        }
    }

    /// `summon`: every window back in sight, un-minimized in KWin too.
    pub fn summon(&mut self, grab: &mut grab::Grab) {
        self.end_theater(grab);
        for i in 0..SLOTS {
            if panel(self.first + i).minimized() {
                self.restore(i, grab);
            }
        }
    }

    /// Brings slot i's hidden panel back: through KWin while it has the window minimized (its
    /// "minimized" event opens the stream once the window's back), else right here.
    fn restore(&mut self, i: usize, grab: &mut grab::Grab) {
        match win(self.first + i) {
            Some(w) if w.minimized => self.kwin.send(json!({"c": "unminimize", "uuid": w.uuid})),
            Some(_) => self.minimize(i, false, grab),
            None => {}
        }
    }

    /// Plasma's panel's size in pixels, while the session has one (taskbar.rs).
    pub fn plasma_size(&self) -> Option<(i32, i32)> {
        self.plasma.size()
    }

    /// A laser on Plasma's bar or its popups this frame (main.rs: the loop keeps the display's rate).
    pub fn plasma_moving(&self) -> bool {
        self.plasma.moving
    }

    /// Devices whose button came up on Plasma's bar since the last call (taskbar.rs).
    pub fn plasma_released(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.plasma.released)
    }

    /// A laser event on the taskbar's frame over Plasma's bar, at fractions of it (plasmabar.rs `on_bar`).
    pub fn plasma_laser(&mut self, e: &vr::VREvent_t, fx: f64, fy: f64) {
        self.plasma.on_bar(e, fx, fy);
    }

    /// A laser's release came up off Plasma's bar: its presses there let go (plasmabar.rs).
    pub fn plasma_release(&mut self) {
        self.plasma.release_held();
    }

    /// Plasma's taskbar this frame, in the taskbar's frame (plasmabar.rs `tick`).
    pub fn tick_plasma(&mut self, at: Option<(Mat, Mat, u32)>, up: f64, mpp: f64, curve: f64) {
        self.plasma.tick(self.session.as_ref(), &self.cap, at, up, mpp, curve);
    }

    /// Theater mode off, if a window panel is in it.
    pub fn end_theater(&mut self, grab: &mut grab::Grab) {
        let t = crate::THEATER.load(Relaxed);
        if let Some(j) = t.checked_sub(self.first).filter(|&j| j < SLOTS && self.slots[j].theater.is_some()) {
            self.theater(j, grab);
        }
    }

    /// Windows waiting for a slot (the taskbar's "+N").
    pub fn waiting(&self) -> usize {
        self.unslotted.len()
    }

    /// Theater mode: a big curved screen 1.8 m ahead at eye height (2 m wide, about 58°), with the
    /// room dark, the other panels hidden and the window at 1600x900. Toggled again, it goes back
    /// where it was.
    fn theater(&mut self, i: usize, grab: &mut grab::Grab) {
        let p = panel(self.first + i);
        grab.cancel(p.index);
        let mut k = KVM.lock().unwrap();
        if let Some((before, height)) = self.slots[i].theater.take() {
            crate::THEATER.store(usize::MAX, Relaxed);
            k.set_place(p.index, &panel_matrix(&before), before.width, height, before.curve); // its rate comes back in tick_slot
            // and the window back to its panel's size. The size sync compares with the stream's
            // size, which may not have followed theater's (KWin stalls on quick resizes)
            if let Some(w) = win(p.index) {
                let (ww, wh) = window_size(before.width, height, self.density(p));
                self.kwin.send(json!({"c": "place", "uuid": w.uuid, "w": ww, "h": wh}));
            }
            return;
        }
        let other = crate::THEATER.load(Relaxed);
        if let Some(j) = other.checked_sub(self.first).filter(|&j| j < SLOTS && j != i) {
            if let Some((before, height)) = self.slots[j].theater.take() {
                k.set_place(other, &panel_matrix(&before), before.width, height, before.curve); // one at a time
            }
        }
        self.slots[i].theater = Some((k.place[p.index].pose(), k.place[p.index].height));
        if p.minimized() {
            self.restore(i, grab); // its app's own full screen brings a hidden panel back
        }
        // A big window for the big screen, so a video player is big enough for 1080p (it goes back
        // to its own size after, from its panel's width). 1600x900, not 1920x1080, because that doesn't
        // fit WL-0 under its panel (1920x1030), so KWin put it on the 3840-wide WL-2, and redrawing
        // that for every video frame stuttered the headset. In the app's own full screen KWin keeps
        // the output's size.
        if let Some(w) = win(p.index) {
            self.kwin.send(json!({"c": "place", "uuid": w.uuid, "w": 1600, "h": 900}));
        }
        let m = vr::head().unwrap_or([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.6], [0.0, 0.0, 1.0, 0.0]]);
        let f = [-m[0][2] as f64, 0.0, -m[2][2] as f64];
        let len = crate::geometry::norm(&f).max(1e-6);
        let f = f.map(|c| c / len);
        let (yaw, pitch) = angles(&f);
        let centre = [m[0][3] as f64 + f[0] * 1.8, m[1][3] as f64, m[2][3] as f64 + f[2] * 1.8];
        let pose = Pose { centre, yaw, pitch, width: 2.0, curve: 1.8, ..Default::default() };
        k.set_pose(p.index, &panel_matrix(&pose), pose.width, pose.curve);
        crate::THEATER.store(p.index, Relaxed); // tick_slot applies THEATER_FPS: a renegotiation, which the 1600x900 resize brings anyway
    }

    /// window_px_per_m now.
    pub fn px_per_m(&self) -> f64 {
        self.density
    }

    /// Sets window_px_per_m to d (Preferences); every window gets asked its new size on the next tick.
    pub fn set_density(&mut self, d: f64) {
        self.density = d;
        DENSITY.store(d.to_bits(), Relaxed);
    }

    /// Window pixels per metre on panel p: the setting over its zoom (bigger text means fewer).
    fn density(&self, p: &crate::Panel) -> f64 {
        self.density / p.zoom()
    }

    /// 1.2 m ahead of the head, level, each new one 8 cm right and down from the last.
    fn ahead(&self) -> Pose {
        let n = (0..SLOTS).filter(|&j| panel(self.first + j).live()).count() % 6;
        let m = vr::head().unwrap_or([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.6], [0.0, 0.0, 1.0, 0.0]]);
        let f = [-m[0][2] as f64, 0.0, -m[2][2] as f64];
        let len = crate::geometry::norm(&f).max(1e-6);
        let f = f.map(|c| c / len);
        let (yaw, pitch) = angles(&f);
        let c = 0.08 * n as f64;
        let centre = [m[0][3] as f64 + f[0] * 1.2 - f[2] * c, m[1][3] as f64 - c, m[2][3] as f64 + f[2] * 1.2 + f[0] * c];
        Pose { centre, yaw, pitch, width: 1.0, ..Default::default() }
    }

    /// Its border tag, drawn once per app by assets.rs (off the main thread) and then cached.
    fn tag(&mut self, i: usize, app: &str) {
        let path = config::cache(&format!("assets/tag-app-{}.rgba", tag_id(app)));
        if let Some(t) = crate::load_tag(&path) {
            return panel(self.first + i).set_tag(Some(&t));
        }
        let (tx, rx) = mpsc::channel();
        let arg = format!("tag=app-{},{},frame,164,139,255", tag_id(app), app_name(app));
        std::thread::spawn(move || {
            crate::assets::draw(&config::cache("assets"), &crate::theme::font(), &[arg]);
            let _ = tx.send(());
        });
        self.slots[i].tag = Some(rx);
    }

    fn close_stream(&mut self, i: usize) {
        let s = &mut self.slots[i];
        s.stream = None; // KWin stops rendering it
        if s.feed.take().is_some() {
            self.cap.close(i as u64);
        }
        s.shown.free();
        s.shown = capture::Shown::default();
        (s.asked, s.created, s.nudged, s.frames, s.arrived) = (None, None, 0, 0, 0);
        (s.paused, s.last_up) = (false, None); // a new stream starts active
    }

    fn open(&mut self, i: usize) {
        self.close_stream(i);
        let Some(w) = win(self.first + i) else { return };
        let s = &mut self.slots[i];
        s.stream = self.session.as_ref().and_then(|ses| ses.stream_window(&w.uuid, false));
        s.asked = s.stream.as_ref().map(|_| Instant::now());
    }

    fn free(&mut self, i: usize, grab: &mut grab::Grab) {
        let p = panel(self.first + i);
        let Some(w) = win(p.index) else { return };
        if let Some((before, height)) = self.slots[i].theater.take() {
            crate::THEATER.store(usize::MAX, Relaxed);
            KVM.lock().unwrap().set_place(p.index, &panel_matrix(&before), before.width, height, before.curve);
        }
        self.keep_pose(i, &w.uuid);
        call!(ov, HideOverlay, p.overlay); // hide before its buffers go
        self.close_stream(i);
        p.set_live(false);
        p.set_minimized(false); // the slot's next window starts in sight
        p.set_tag(None);
        grab.cancel(p.index);
        {
            let mut k = KVM.lock().unwrap();
            if k.active == p.index {
                k.active = 0;
            }
            if k.kbd == p.index {
                // its held keys go up (Ctrl, after Ctrl+W); typing stays on the session, whose
                // focus moved on, and never jumps to a krdp panel nobody clicked
                k.release_keys();
            }
        }
        {
            let mut s = INPUT.lock().unwrap();
            if s.hover == Some(p.index) {
                s.hover = None;
            }
        }
        (self.slots[i].tag, self.slots[i].placing) = (None, None);
        set_win(p.index, None);
        eprintln!("{}: {} gone", p.v.name, w.key);
    }

    fn keep_pose(&mut self, i: usize, uuid: &str) {
        let p = panel(self.first + i);
        if p.live() && !is_popup(p.index) {
            let pl = KVM.lock().unwrap().place[p.index];
            let mut v = config::pose_json(&pl.pose(), pl.height);
            v["zoom"] = json!(p.zoom());
            self.poses.insert(uuid.into(), v);
        }
    }

    /// Puts popup slot i's panel on its parent's (popup_place), when either moved or resized.
    fn place_popup(&mut self, i: usize) {
        let p = panel(self.first + i);
        let Some(w) = win(p.index) else { return };
        let Some(pw) = self.slot_of(&w.parent).map(|j| self.first + j).and_then(|j| Some((j, win(j)?))) else { return };
        let mut k = KVM.lock().unwrap();
        let at = popup_place(&k.place[pw.0], pw.1.cg, w.cg);
        if self.slots[i].follow != Some(at) {
            self.slots[i].follow = Some(at);
            k.set_pose(p.index, &at.0, at.1, 0.0);
        }
    }

    fn tick_slot(&mut self, i: usize, grab: &mut grab::Grab) {
        let p = panel(self.first + i);
        let Some(w) = win(p.index) else { return };
        let mut closed = false;
        let density = self.density(p);
        let s = &mut self.slots[i];
        while let Some(e) = s.stream.as_ref().and_then(|st| st.events.try_recv().ok()) {
            match e {
                StreamEvent::Created(node) => {
                    s.created = Some(Instant::now());
                    let fps = if crate::THEATER.load(Relaxed) == p.index { THEATER_FPS } else { WINDOW_FPS };
                    s.feed = Some(self.cap.open(i as u64, node, &p.v.name, fps));
                    s.fps = fps;
                }
                StreamEvent::Failed(e) => eprintln!("{}: stream failed: {e}", p.v.name),
                StreamEvent::Closed => closed = true,
            }
        }
        if closed {
            self.free(i, grab); // the window went
            return self.fill(grab);
        }
        if let Some(rx) = &s.tag
            && !matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty))
        {
            s.tag = None;
            let t = crate::load_tag(&config::cache(&format!("assets/tag-app-{}.rgba", tag_id(&w.app))));
            p.set_tag(t.as_ref());
        }
        let Some(feed) = &s.feed else { return };
        let cg = (w.cg[2], w.cg[3]);
        // D-042: while paused, KWin renders nothing for it (attention.rs). When it's back, a window
        // resized meanwhile reopens at once, because KWin only follows a new size in a frame it
        // records. A stream with no frame yet runs at full rate wherever it is: paused (or reopened,
        // the panel still live) before its first frame, an idle window might never send one.
        p.change.fetch_max(feed.change.swap(0, Relaxed), Relaxed); // D-045: its damage, for attention.rs
        let fresh = s.shown.shown == 0;
        let level = if fresh { Level::Full } else { p.level() };
        feed.quiet.store(level != Level::Full, Relaxed); // only a frame taken right away wakes the main loop
        if (level == Level::Paused) != s.paused {
            s.paused = !s.paused;
            self.cap.set_active(i as u64, !s.paused);
            if !s.paused && cg != s.fmt.1 && s.shown.size != (0, 0) {
                eprintln!("{}: the window is {}x{} but its stream stayed {}x{} while paused: opening it again", p.v.name, cg.0, cg.1, s.shown.size.0, s.shown.size.1);
                return self.open(i);
            }
        }
        let now = Instant::now();
        // Peripheral (or quiet) for a while, KWin's cap comes down too, since it renders and copies
        // every damaged frame up to the cap only for us to drop most. It's a renegotiation each way, so
        // not for a glance aside, and not while paused (a paused stream records nothing to renegotiate in).
        s.edge = matches!(level, Level::Peripheral | Level::Quiet).then(|| s.edge.unwrap_or(now));
        let fps = if s.edge.is_some_and(|t| now - t > EDGE_AFTER) {
            EDGE_FPS
        } else if crate::THEATER.load(Relaxed) == p.index {
            THEATER_FPS
        } else {
            WINDOW_FPS
        };
        if !s.paused && fps != s.fps {
            s.fps = fps;
            self.cap.set_rate(i as u64, fps);
        }
        let up = !s.paused && attention::due(level, attention::PERIPHERAL_WINDOW, s.last_up, now) && s.shown.tick(&self.cap, i as u64, feed, p.overlay, &p.v.name);
        if up {
            s.last_up = Some(now);
        }
        // A window resized and its stream never followed (seen live after quick theater and
        // full screen toggles: the panel froze on its last frame), so open the stream again. Not
        // while it's paused, since nothing could follow.
        if s.paused {
            s.stuck = None;
        } else if s.shown.size != s.fmt.0 {
            (s.fmt, s.stuck) = ((s.shown.size, cg), None);
        } else if cg != s.fmt.1 && s.shown.size != (0, 0) {
            let since = *s.stuck.get_or_insert_with(Instant::now);
            if since.elapsed() > stuck_after(level, s.fps) {
                eprintln!("{}: the window is {}x{} but its stream stayed {}x{}: opening it again", p.v.name, cg.0, cg.1, s.shown.size.0, s.shown.size.1);
                (s.fmt, s.stuck) = ((s.shown.size, cg), None);
                return self.open(i);
            }
        } else {
            s.stuck = None;
        }
        if up && !p.live() {
            // shown on its first frame, so there's no black flash
            p.set_live(true);
            if !HIDDEN.load(Relaxed) && !p.away() {
                call!(ov, ShowOverlay, p.overlay);
            }
            eprintln!("{}: first frame +{}ms", p.v.name, s.asked.map_or(0, |t| t.elapsed().as_millis()));
        }
        // KWin only sends frames on damage, so an idle window's new stream gets a 1 px nudge and
        // back (a reopen too, since its panel stays live).
        if s.shown.shown == 0
            && let Some(t) = s.created
        {
            let step = match s.nudged {
                0 if t.elapsed() > NUDGE => 1,
                1 if t.elapsed() > NUDGE * 3 / 2 => -1,
                _ => 0,
            };
            if step != 0 {
                s.nudged += 1;
                if w.popup() {
                    // KWin won't resize a popup, but a stream resumed records a frame at once
                    // (WindowScreenCastSource::resume), so pause it and resume it
                    self.cap.set_active(i as u64, step < 0);
                } else {
                    self.kwin.send(json!({"c": "nudge", "uuid": w.uuid, "d": step}));
                }
            }
        }
        if w.popup() {
            // its size is its own, its place its parent's: none of the size sync below
            let (sw, sh) = s.shown.size;
            if sw > 0 && (sw, sh) != p.size() {
                p.set_size(sw, sh);
                s.follow = None;
            }
            return self.place_popup(i);
        }
        if grab.resizing(p.index) {
            s.anchored = grab.stretching(p.index);
        }
        // The stream's size is the window's, so refit the panel (1:1 pixels). Keep the width
        // while it's held (the corner being dragged decides that), and the height while it's
        // stretched by its bottom-right corner (grab.rs). A frame for an earlier size we asked for
        // (they keep coming for a while after letting go) keeps the panel's size too, until SETTLE,
        // because the size asked last is what it'll end up. After a stretch it hangs from its top-left.
        let (sw, sh) = s.shown.size;
        if sw > 0 && (sw, sh) != p.size() {
            p.set_size(sw, sh);
            let mut k = KVM.lock().unwrap();
            let pl = k.place[p.index];
            let asked = s.placing.is_some_and(|(t, sent)| sent != (sw, sh) && t.elapsed() < SETTLE);
            if grab.stretching(p.index) || asked {
                k.set_place(p.index, &pl.matrix(), pl.width, pl.height, pl.curve);
            } else {
                let width = if grab.busy(p.index) || s.theater.is_some() { pl.width } else { sw as f64 / density };
                let m = resized(&pl, width, width * sh as f64 / sw as f64, s.anchored);
                k.set_pose(p.index, &m, width, pl.curve);
            }
        }
        // The panel got resized (a corner, Right Ctrl + Shift + wheel), so the window follows: every
        // PLACE_EVERY while dragged and once at the end, then whatever size it took wins.
        // Done once the window is the panel's size (not the size sent, since a panel bigger than the
        // output snaps back to the window after SETTLE).
        let (pw, ph) = p.size();
        let pl = KVM.lock().unwrap().place[p.index];
        let fits = |m: f64, px: u32| ((m * density).round() as u32).abs_diff(px) <= 2;
        if !p.live() || (fits(pl.width, pw) && fits(pl.height, ph)) || s.theater.is_some() {
            s.placing = None;
            s.anchored &= grab.stretching(p.index);
            return;
        }
        let want = window_size(pl.width, pl.height, density);
        let resizing = grab.resizing(p.index);
        let send = match s.placing {
            None => true,
            Some((t, sent)) if resizing => sent != want && t.elapsed() >= PLACE_EVERY,
            Some((_, sent)) if sent != want => true,
            Some((t, _)) => {
                if t.elapsed() >= SETTLE {
                    s.placing = None; // it didn't take that size (a minimum, the output's edge), so snap back
                    let mut k = KVM.lock().unwrap();
                    let pl = k.place[p.index];
                    let (nw, nh) = (pw as f64 / density, ph as f64 / density);
                    k.set_pose(p.index, &resized(&pl, nw, nh, std::mem::take(&mut s.anchored)), nw, pl.curve);
                }
                false
            }
        };
        if send {
            self.kwin.send(json!({"c": "place", "uuid": w.uuid, "w": want.0, "h": want.1}));
            s.placing = Some((Instant::now(), want));
        }
    }

    /// The status line (every 5 s), and saving the panels' poses into win-poses.json.
    pub fn status(&mut self) {
        let mut parts = Vec::new();
        for i in 0..SLOTS {
            let p = panel(self.first + i);
            let Some(w) = win(p.index) else { continue };
            let s = &mut self.slots[i];
            // shown, and how many arrived when that's fewer: a stall upstream shows as both at 0
            let (n, a) = (s.shown.shown, s.feed.as_ref().map_or(0, |f| f.frames.load(Relaxed)));
            let (shown, arrived) = ((n - s.frames.min(n)) as f64 / 5.0, (a - s.arrived.min(a)) as f64 / 5.0);
            let level = match p.level() {
                Level::Paused => " paused",
                Level::Quiet => " quiet",
                Level::Peripheral => " peripheral",
                Level::Full => "",
            };
            parts.push(if arrived > shown { format!("{} {} {shown:.1}/s ({arrived:.1} in){level}", p.v.name, w.key) } else { format!("{} {} {shown:.1}/s{level}", p.v.name, w.key) });
            (s.frames, s.arrived) = (n, a);
            self.keep_pose(i, &w.uuid);
        }
        if !parts.is_empty() || !self.unslotted.is_empty() {
            eprintln!("windows: {}{}", parts.join(", "), if self.unslotted.is_empty() { String::new() } else { format!("; {} waiting for a slot", self.unslotted.len()) });
        }
        self.save_poses();
    }

    fn save_poses(&self) {
        let path = config::cache("win-poses.json");
        if read_map(&path) != self.poses
            && let Err(e) = config::write_json(&path, &Value::Object(self.poses.clone()))
        {
            eprintln!("windows: can't save {path}: {e}");
        }
    }

    /// `windows` on the control socket: one line per window, slotted or waiting.
    pub fn list(&self) -> String {
        let mut out = String::from("ok");
        let line = |slot: &str, w: &Win, state: &str| format!("\n{slot} {} {} {}x{}+{}+{} {state} {}", w.uuid, w.key, w.cg[2], w.cg[3], w.cg[0], w.cg[1], w.caption);
        for i in 0..SLOTS {
            let p = panel(self.first + i);
            if let Some(w) = win(p.index) {
                out += &line(&p.v.name, &w, if p.minimized() { "minimized" } else if p.live() { "shown" } else { "waiting-for-frame" });
            }
        }
        for w in &self.unslotted {
            out += &line("-", w, "no-slot");
        }
        out
    }

    /// On exit: poses kept, streams closed (kwin.rs restores the windows).
    pub fn stop(&mut self) {
        for i in 0..SLOTS {
            if let Some(w) = win(self.first + i) {
                self.keep_pose(i, &w.uuid);
            }
            self.close_stream(i);
        }
        self.plasma.stop(&self.cap);
        self.save_poses();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_raise_counts() {
        let q: kwin::Queue = Default::default();
        let mut s = Input { session: None, cmds: Some(q.clone()), raised: None, asked: None, seq: 0, hover: None, at: (0.0, 0.0), pending: None, down: 0, wheel: 15.0, wait: Duration::ZERO, last: None };
        s.raise("A");
        assert!(s.answered("A", Some(1)));
        s.raise("B"); // pointer onto B: A no longer counts as on top
        assert_eq!(s.raised, None);
        s.raise("A"); // and back before B's answer
        assert!(!s.answered("B", Some(2)) && !s.answered("A", Some(1)));
        assert!(s.answered("A", Some(3)));
        s.raise("A"); // already on top: nothing asked
        assert_eq!(q.lock().unwrap().len(), 3);
        s.raise("B");
        s.covered(); // a window mapped meanwhile
        assert!(!s.answered("B", Some(4)));
    }

    #[test]
    fn windows_parse_and_key_by_app() {
        let e = json!({"e": "add", "uuid": "{a}", "app": "org.kde.konsole", "caption": "~ : bash", "x": 3360, "y": 740, "w": 1280.4, "h": 800, "kind": "window", "parent": ""});
        let w = Win::from(&e).unwrap();
        assert_eq!((w.key.as_str(), w.cg), ("app:org.kde.konsole", [3360, 740, 1280, 800]));
        assert_eq!(Win::from(&json!({"app": "x"})).map(|w| w.uuid), None);
        assert_eq!(Win::from(&json!({"uuid": "{b}"})).unwrap().key, "app:unknown");
        assert_eq!(tag_id("org.kde/dolphin 2"), "org.kde_dolphin_2");
        assert_eq!(app_name("org.example.no-such-app"), "no-such-app");
    }

    #[test]
    fn a_resized_panel_asks_for_its_size_in_pixels() {
        assert_eq!(window_size(1.2, 0.5, 1400.0), (1680, 700));
        assert_eq!(window_size(1.2, 0.5, 1400.0 / 2.0), (840, 350), "zoomed in: fewer pixels");
        assert_eq!(window_size(3.0, 0.5, 1400.0), (2560, 700), "no wider than the output");
        assert_eq!(window_size(0.5, 2.0, 1400.0), (700, 1440), "nor taller");
        assert_eq!(window_size(0.0, 0.0, 1400.0), (1, 1));
    }

    #[test]
    fn a_stretched_panel_snaps_back_from_its_top_left() {
        use crate::geometry::Placement;
        let at = |m: Mat| [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64];
        for curve in [0.0, 1.8] {
            let pose = Pose { centre: [0.3, 1.5, -1.4], yaw: 25.0, width: 2.0, curve, ..Default::default() };
            let pl = Placement::from_matrix(&panel_matrix(&pose), 2.0, 0.6, curve);
            let top_left = at(pl.on_surface(-1.0, 0.6, 0.0));
            let (nw, nh) = (2560.0 / 1400.0, 1.0);
            let np = Placement::from_matrix(&resized(&pl, nw, nh, true), nw, nh / nw, curve);
            let moved = at(np.on_surface(-nw / 2.0, nh / 2.0, 0.0)).iter().zip(top_left).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
            assert!(moved < 1e-5, "curve {curve}: the top-left moved {moved}");
            assert_eq!(resized(&pl, nw, nh, false), pl.matrix(), "another corner's: about the centre");
        }
    }

    #[test]
    fn the_watchdog_waits_for_a_slow_stream() {
        assert_eq!(stuck_after(Level::Full, WINDOW_FPS), STUCK);
        assert_eq!(stuck_after(Level::Peripheral, WINDOW_FPS), STUCK.max(attention::PERIPHERAL_WINDOW * 6));
        assert_eq!(stuck_after(Level::Quiet, WINDOW_FPS), attention::QUIET_EVERY * 6);
        assert_eq!(stuck_after(Level::Peripheral, EDGE_FPS), Duration::from_secs(1) / EDGE_FPS * 6);
        assert_eq!(stuck_after(Level::Full, 1), Duration::from_secs(6)); // a slow cap: six of its frames
    }

    #[test]
    fn a_popup_rides_on_its_parent_where_it_is_in_the_session() {
        use crate::geometry::Placement;
        let at = |m: Mat| [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64];
        let pose = Pose { centre: [0.3, 1.5, -1.4], yaw: 25.0, pitch: -10.0, width: 1.0, ..Default::default() };
        let parent = Placement::from_matrix(&panel_matrix(&pose), 1.0, 0.8, 0.0); // 1000x800 at 1 mm a pixel
        let (pcg, cg) = ([100, 50, 1000, 800], [700, 150, 200, 300]); // a menu opened at (600, 100) in the window
        let (m, w) = popup_place(&parent, pcg, cg);
        assert!((w - 0.2).abs() < 1e-9, "the parent's scale: {w}");
        let popup = Placement::from_matrix(&m, w, 1.5, 0.0);
        let (corner, under) = (at(popup.on_surface(-0.1, 0.15, 0.0)), at(parent.on_surface(0.1, 0.3, LIFT)));
        let off = corner.iter().zip(under).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        assert!(off < 1e-5, "its top-left is over the window's (600, 100), LIFT nearer: off by {off}");
        // and a click on it goes to the popup's own place in the session, not its parent's
        assert_eq!(session_point(cg, 400, 100.0, 50.0), (750.0, 175.0));
        assert_eq!(session_point(pcg, 1000, 600.0, 100.0), (700.0, 150.0));
    }

    #[test]
    fn a_popup_after_a_click_is_a_menu_and_one_from_hovering_a_tooltip() {
        let now = Instant::now();
        let ago = |s: u64| now.checked_sub(Duration::from_secs(s));
        assert!(popup_wanted(false, ago(1), now), "right after a click or a key");
        assert!(!popup_wanted(false, ago(10), now), "hovering long after: a tooltip");
        assert!(!popup_wanted(false, None, now));
        assert!(popup_wanted(true, ago(10), now), "a submenu, however long the menu's been open");
    }

    #[test]
    fn rdp_buttons_map_to_evdev() {
        assert_eq!(button_code(PTR_FLAGS_BUTTON1 | PTR_FLAGS_DOWN), Some(session::BTN_LEFT));
        assert_eq!(button_code(PTR_FLAGS_BUTTON2), Some(session::BTN_RIGHT));
        assert_eq!(button_code(PTR_FLAGS_BUTTON3 | PTR_FLAGS_DOWN), Some(session::BTN_MIDDLE));
        assert_eq!(button_code(freerdp_sys::PTR_FLAGS_MOVE), None);
    }
}
