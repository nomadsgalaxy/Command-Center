//! cc-panels: Command Center's own VR panels. Each remote monitor in viewers.conf is a SteamVR
//! overlay showing its RDP stream (krdp on the machine, FreeRDP here, H.264 decoded with
//! FFmpeg), placed at its saved spot (home.json) with its curve. The controllers' lasers, and
//! anything else SteamVR sends overlay mouse events for, work it: moves, clicks and scrolling
//! go to that machine. Our own keyboard and mouse drive every panel (kvm.rs), and there's one
//! shared clipboard across them and the Frame's own windows (clipboard/).
//!
//!   cc-panels [--for MIN] [name ...]   the named viewers, or all of them, for MIN minutes
//!                                      (default 2; 0 = until stopped). Right Ctrl + Esc ends it.
//!   Moving the mouse wakes the pointer (straight ahead); 30 s without mouse use puts it to sleep.
//!   Typing is click to type: a click on a panel sends the keyboards there, and a click off our
//!   panels, the dashboard opening, or a game gives them back to the Frame. A Right Ctrl tap
//!   hands them over either way too.
//!   Off our panels the mouse drives SteamVR's own laser (the cc_pointer driver, on whichever
//!   hand is free) for the dashboard, Steam and the desktop. Right Ctrl + D opens the dashboard.
//!   CC_GAZE=1 or Right Ctrl + G turns on gaze lock: the pointer stays on the panel you look at.
mod assets;
mod attention;
mod back;
mod capture;
mod clipboard;
mod config;
mod control;
mod geometry;
mod gaze;
mod grab;
mod gpu;
mod kvm;
mod kwin;
mod laser;
mod plasmabar;
mod popout;
mod rdp;
mod vnc;
#[allow(dead_code)] // virtual outputs for desktop mode aren't wired up yet; the spike uses them
mod session;
mod taskbar;
mod theme;
mod vr;
mod windows;

use freerdp_sys::{PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3, PTR_FLAGS_DOWN, PTR_FLAGS_MOVE};
use geometry::{Pose, panel_matrix};
use kvm::KVM;
use openvr_sys as sys;
use std::path::PathBuf;
use std::sync::atomic::Ordering::*;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub static QUIT: AtomicBool = AtomicBool::new(false);
pub static HIDDEN: AtomicBool = AtomicBool::new(false); // every panel hidden, by the camera scan or the user
pub static THEATER: AtomicUsize = AtomicUsize::new(usize::MAX); // the panel in theater mode, the rest dim; MAX means none
pub static PANELS: OnceLock<Vec<Panel>> = OnceLock::new();

pub fn panels() -> &'static [Panel] {
    PANELS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

pub fn panel(i: usize) -> &'static Panel {
    &panels()[i]
}

/// The repository root, worked out from target/<profile>/cc-panels.
pub fn root() -> PathBuf {
    let exe = std::fs::read_link("/proc/self/exe").unwrap_or_default();
    exe.ancestors().find(|a| a.ends_with("target")).and_then(|t| t.parent()).map(PathBuf::from).unwrap_or_default()
}

/// A region that changed since one buffer was last written.
pub struct Stale([AtomicI32; 4]);

impl Stale {
    const EMPTY: [i32; 4] = [1 << 30, 1 << 30, 0, 0];
    fn new() -> Self {
        Stale(Self::EMPTY.map(AtomicI32::new))
    }
    pub fn grow(&self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.0[0].fetch_min(x0, Relaxed);
        self.0[1].fetch_min(y0, Relaxed);
        self.0[2].fetch_max(x1, Relaxed);
        self.0[3].fetch_max(y1, Relaxed);
    }
    pub fn take(&self) -> (i32, i32, i32, i32) {
        let [a, b, c, d] = [0, 1, 2, 3].map(|i| self.0[i].swap(Self::EMPTY[i], Relaxed));
        (a, b, c, d)
    }
}

/// What a panel shows: a remote machine's screen (krdp; R-3 says it only ever reaches that
/// machine), or whichever Frame window holds this slot of the window pool (windows.rs).
pub enum Source {
    Rdp,
    Window(Mutex<Option<windows::Win>>),
}

/// Remote slots made at start with no viewer, so the Machines window's Add machine can fill
/// one (Panel::fill) without a restart. Each costs 2 of SteamVR's 128 overlays (picture and card).
/// ponytail: more adds than this in one run need a restart (machines.rs says so).
const SPARES: usize = 2;

/// A panel's viewer. A spare is NONE until Add machine (or a pop-out, popout.rs) fills it, and
/// a pop-out's slot is emptied again when it ends.
/// ponytail: each fill leaks its Viewer (a few hundred bytes), so `p.v` stays a plain reference
pub struct Fill(Mutex<Option<&'static config::Viewer>>);
static NONE: config::Viewer = config::Viewer { name: String::new(), user: String::new(), host: String::new(), port: 0, screen: 0, w: 1920, h: 1080, auto: false, machine: String::new(), label: String::new(), pop: None, vnc: None };

impl std::ops::Deref for Fill {
    type Target = config::Viewer;
    fn deref(&self) -> &config::Viewer {
        self.0.lock().unwrap().unwrap_or(&NONE)
    }
}

pub struct Panel {
    pub index: usize,
    pub v: Fill, // ponytail: for a window slot it's only the name (win-N); move it into Source::Rdp(Viewer) if that confuses
    pub src: Source,
    pub overlay: vr::Handle,
    pub card: vr::Handle,  // its frame, grab bar and tag, all one overlay (grab.rs)
    pub accent: [f64; 3], // its kind's colour: cyan for a remote machine, violet for a Frame window
    tag: Mutex<(u32, Option<Arc<grab::TagImg>>)>, // its border tag's image (assets.rs) and how many times it's changed
    zoom: AtomicU64, // a window panel's text size, which window_px_per_m is divided by (f64 bits)
    size: [AtomicU32; 2],         // its picture in pixels: the remote's desktop or the window's stream
    live: AtomicBool,             // shown and in use (for a window slot: it has a window and a frame)
    gone: AtomicBool,             // a remote removed in the Machines window: no row, no chip, never connects again
    min: AtomicBool,              // hidden by its taskbar chip or its v (taskbar.rs), so picture and card are gone
    vert: AtomicBool,             // curved top to bottom, so its overlay and picture are turned a quarter (gpu.rs)
    pub lock: Mutex<()>, // guards the GDI between its RDP thread (decode, then gpu::write) and resizes or detach
    pub gpu: Mutex<gpu::Shared>, // its two GPU buffers: its RDP thread writes them, the main loop shows them
    pub rdp: AtomicPtr<freerdp_sys::rdpContext>,
    pub wake: AtomicPtr<std::ffi::c_void>, // wakes its RDP thread's wait (rdp::poke)
    pub dirty: AtomicBool,
    pub connected: AtomicBool,
    pub stale: [Stale; 2], // per GPU buffer
    pub cliprdr: AtomicPtr<freerdp_sys::CliprdrClientContext>, // its clipboard channel, while connected
    level: AtomicU8,            // how much of its stream is worth having (attention.rs)
    pub change: AtomicU32,      // the most of its picture one frame changed since the last tick, in thousandths (attention.rs)
}

impl Panel {
    fn new(index: usize, v: config::Viewer, src: Source, accent: [f64; 3], tag: Option<&(usize, usize, Vec<u8>)>) -> Result<Panel, String> {
        let overlay = vr::create_overlay(&format!("controlcenter.panel.{}", v.name), &format!("Command Center {}", v.name))
            .map_err(|e| format!("{}: can't create its overlay: {e}", v.name))?;
        let p = Panel {
            index,
            card: grab::create(&v.name),
            live: AtomicBool::new(matches!(src, Source::Rdp)),
            min: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            vert: AtomicBool::new(false),
            size: [AtomicU32::new(v.w), AtomicU32::new(v.h)],
            v: Fill(Mutex::new(Some(Box::leak(Box::new(v))))),
            src,
            overlay,
            accent,
            tag: Mutex::new((0, tag.cloned().map(Arc::new))),
            zoom: AtomicU64::new(1f64.to_bits()),
            lock: Mutex::new(()),
            gpu: Mutex::default(),
            rdp: AtomicPtr::default(),
            wake: AtomicPtr::default(),
            dirty: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            stale: [Stale::new(), Stale::new()],
            cliprdr: AtomicPtr::default(),
            level: AtomicU8::new(attention::Level::Full as u8),
            change: AtomicU32::new(0),
        };
        // Lasers work it with the dashboard closed, wheels scroll it in steps, and it's never see-through.
        call!(ov, SetOverlayInputMethod, p.overlay, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, p.overlay, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        call!(ov, SetOverlayFlag, p.overlay, sys::VROverlayFlags_SendVRDiscreteScrollEvents, true);
        call!(ov, SetOverlayFlag, p.overlay, sys::VROverlayFlags_IgnoreTextureAlpha, true);
        p.set_mouse_scale();
        Ok(p)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.size[0].load(Relaxed).max(1), self.size[1].load(Relaxed).max(1))
    }

    /// A window's stream changed size, so overlay mouse events now come in its new pixels.
    pub fn set_size(&self, w: u32, h: u32) {
        self.size[0].store(w, Relaxed);
        self.size[1].store(h, Relaxed);
        self.set_mouse_scale();
    }

    fn set_mouse_scale(&self) {
        let (w, h) = self.size();
        let (w, h) = if self.vert() { (h, w) } else { (w, h) }; // a turned overlay's own axes
        let mut scale = sys::HmdVector2_t { v: [w as f32, h as f32] };
        call!(ov, SetOverlayMouseScale, self.overlay, &mut scale);
    }

    /// Its tag and how many times it has changed, so the card knows when to redraw.
    pub fn tag(&self) -> (u32, Option<Arc<grab::TagImg>>) {
        self.tag.lock().unwrap().clone()
    }

    pub fn zoom(&self) -> f64 {
        f64::from_bits(self.zoom.load(Relaxed))
    }

    /// 0.5 (small text) to 3 (big). Anything else, like nothing saved, is 1.
    pub fn set_zoom(&self, z: f64) {
        let z = if z > 0.0 { z.clamp(0.5, 3.0) } else { 1.0 };
        self.zoom.store(z.to_bits(), Relaxed);
    }

    pub fn set_tag(&self, tag: Option<&grab::TagImg>) {
        let mut t = self.tag.lock().unwrap();
        *t = (t.0.wrapping_add(1), tag.cloned().map(Arc::new));
    }

    pub fn vert(&self) -> bool {
        self.vert.load(Relaxed)
    }

    pub fn set_vert(&self, on: bool) {
        if self.vert.swap(on, Relaxed) != on {
            self.dirty.store(true, std::sync::atomic::Ordering::Release); // its picture goes up turned (or turned back) now
            rdp::poke(self); // its RDP thread may be asleep in its wait
            self.set_mouse_scale();
        }
    }

    /// Has a viewer and isn't removed, so it's not a spare (SPARES) or a remote removed since start.
    pub fn used(&self) -> bool {
        self.v.0.lock().unwrap().is_some() && !self.gone.load(Relaxed)
    }

    /// Gives a spare its viewer (Add machine). False if it already had one.
    pub fn fill(&self, v: config::Viewer) -> bool {
        let (w, h) = (v.w, v.h);
        {
            let mut f = self.v.0.lock().unwrap();
            if f.is_some() {
                return false;
            }
            *f = Some(Box::leak(Box::new(v)));
        }
        self.gone.store(false, Relaxed);
        self.set_size(w, h);
        let name = std::ffi::CString::new(format!("Command Center {}", self.v.name)).unwrap_or_default();
        call!(ov, SetOverlayName, self.overlay, name.as_ptr() as *mut _);
        true
    }

    /// Turns it back into a spare when a pop-out ends (popout.rs): no viewer, picture, tag or
    /// turn, and its RDP thread has already ended. Typing there goes back to the monitor it
    /// came from, because the slot's next window (maybe another machine's) shouldn't get it
    /// without a click.
    pub fn empty(&self) {
        self.set_live(false);
        call!(ov, HideOverlay, self.overlay);
        call!(ov, ClearOverlayTexture, self.overlay); // so the next fill shows nothing until its own first frame
        {
            let mut k = KVM.lock().unwrap();
            if k.kbd == self.index {
                let from = self.v.pop.as_ref().and_then(|(m, _)| panels().iter().position(|q| q.used() && q.v.name == *m));
                k.type_to(from.unwrap_or(0));
            }
        }
        self.set_tag(None);
        self.set_vert(false);
        self.set_minimized(false);
        *self.v.0.lock().unwrap() = None;
    }

    /// Removed from viewers.conf in the Machines window: disconnect it, then it's gone for this run.
    pub fn remove(&self) {
        if self.live() {
            disconnect(self);
        }
        self.gone.store(true, Relaxed);
    }

    pub fn live(&self) -> bool {
        self.live.load(Relaxed)
    }

    pub fn set_live(&self, on: bool) {
        self.live.store(on, Relaxed);
    }

    /// Hidden by its taskbar chip or a window's v. A window panel's window is minimized in KWin
    /// too, or was already minimized there by Plasma's taskbar.
    pub fn minimized(&self) -> bool {
        self.min.load(Relaxed)
    }

    pub fn set_minimized(&self, on: bool) {
        self.min.store(on, Relaxed);
    }

    /// Out of sight for now, card included: minimized, or hidden for another panel's theater
    /// mode. It catches no lasers and the mouse can't land on it.
    pub fn away(&self) -> bool {
        let theater = THEATER.load(Relaxed);
        // a menu of the theater panel's window stays with it
        self.minimized() || (theater != usize::MAX && theater != self.index && windows::root(self.index) != theater)
    }

    pub fn level(&self) -> attention::Level {
        attention::Level::from_u8(self.level.load(Relaxed))
    }

    /// Its key in the spots: a viewer's name, or a window's `app:<id>`.
    pub fn spot_key(&self) -> String {
        match &self.src {
            Source::Window(w) => w.lock().unwrap().as_ref().map_or_else(|| self.v.name.clone(), |w| w.key.clone()),
            Source::Rdp => self.v.name.clone(),
        }
    }

    /// Pointer input at x, y in its picture's pixels from the top left, in RDP's flags. R-3: a
    /// remote machine's panel only ever reaches that machine, and a window's only the session.
    pub fn mouse(&self, flags: u32, x: f64, y: f64) {
        if flags & PTR_FLAGS_DOWN != 0 {
            popout::pressed(self.index); // W5: tracks a popped-out window's host focus, as far as we can tell
        }
        match self.src {
            Source::Rdp => {
                if flags == PTR_FLAGS_MOVE {
                    windows::leave(); // off every window panel
                }
                if flags & PTR_FLAGS_DOWN != 0 {
                    windows::shell_outside(); // a click outside Plasma's open popup closes it
                }
                if self.v.vnc.is_some() { vnc::mouse(self, flags, x, y) } else { rdp::mouse(self, flags, x, y) }
            }
            Source::Window(_) => windows::mouse(self, flags, x, y),
        }
    }

    /// A key: its evdev code, and 0 for up, 1 for down, 2 for the kernel's repeat.
    pub fn key(&self, code: u16, value: i32) {
        match self.src {
            Source::Rdp if self.v.vnc.is_some() => vnc::key(self, code, value),
            Source::Rdp => {
                let sc = kvm::scancode(code);
                if sc != 0 {
                    rdp::key(self, value != 0, value == 2, sc);
                }
            }
            Source::Window(_) => windows::key(code, value),
        }
    }

    /// Wheel notches, positive is up or right (evdev's convention).
    pub fn wheel(&self, horizontal: bool, notches: f64) {
        match self.src {
            Source::Rdp if self.v.vnc.is_some() => vnc::wheel(self, horizontal, notches),
            Source::Rdp => rdp::wheel(self, horizontal, (notches * 120.0).round() as i32 * if horizontal { -1 } else { 1 }),
            Source::Window(_) => windows::wheel(horizontal, notches),
        }
    }

    /// There's something to take input: a connected machine or a shown window.
    pub fn takes_input(&self) -> bool {
        match self.src {
            Source::Rdp if self.v.vnc.is_some() => self.connected.load(Relaxed),
            Source::Rdp => rdp::input(self).is_some(),
            Source::Window(_) => self.live(),
        }
    }

    /// Saves where it is now as its home spot.
    pub fn save_spot(&self, pl: &geometry::Placement) -> std::io::Result<()> {
        let screen = matches!(self.src, Source::Rdp).then_some(self.v.screen);
        let zoom = matches!(self.src, Source::Window(_)).then(|| self.zoom());
        config::save_home_pose(&self.spot_key(), screen, &pl.pose(), pl.height, zoom)
    }
}

/// Sends laser (overlay mouse) events on a panel to its machine. Overlay mouse coordinates are
/// in the mouse scale we set (the stream's pixels), from the bottom left.
/// The last device a button was pressed on is the primary one. While the mouse is (awake,
/// used in the last 30 s, or clicked last), laser moves are drained and dropped so a
/// controller can't pull the remote pointer out from under it. A controller's click takes
/// over right away: the mouse sleeps and that click goes through. Returns how many events
/// there were and whether any worked it (a move, click or scroll to its machine, which is
/// D-045's input).
fn laser(p: &Panel, mouse_has_it: &mut bool, grab: &mut grab::Grab) -> (u32, bool) {
    let (mut e, mut n, mut worked): (vr::VREvent_t, u32, bool) = (unsafe { std::mem::zeroed() }, 0, false);
    while call!(ov, PollNextOverlayEvent, p.overlay, &mut e, size_of::<vr::VREvent_t>() as u32) {
        n += 1;
        if grab.panel_event(p.index, &e, &mut KVM.lock().unwrap()) {
            continue; // the release of a drag, not meant for the remote
        }
        if *mouse_has_it && e.eventType == sys::EVREventType_VREvent_MouseButtonDown {
            controller_pressed();
            *mouse_has_it = false;
        }
        if *mouse_has_it || !p.takes_input() {
            continue;
        }
        let window = matches!(p.src, Source::Window(_));
        if window && e.eventType == sys::EVREventType_VREvent_FocusLeave {
            windows::leave();
            continue;
        }
        if window && e.eventType == sys::EVREventType_VREvent_MouseButtonDown {
            let mut k = KVM.lock().unwrap();
            k.type_to(p.index); // typing follows the laser's click too
            if !k.engaged {
                k.set_engaged(true, &format!("clicked {} with a controller", p.v.name));
            }
        }
        worked |= matches!(
            e.eventType,
            sys::EVREventType_VREvent_MouseMove | sys::EVREventType_VREvent_MouseButtonDown | sys::EVREventType_VREvent_MouseButtonUp | sys::EVREventType_VREvent_ScrollDiscrete | sys::EVREventType_VREvent_ScrollSmooth
        );
        let m = unsafe { e.data.mouse };
        let (w, h) = p.size();
        let (w, h) = (w as f32, h as f32);
        // from the overlay's bottom left; on a turned one x runs up the picture and y from its right
        let (x, y) = if p.vert() { (w - 1.0 - m.y, h - 1.0 - m.x) } else { (m.x, h - 1.0 - m.y) };
        let (x, y) = (x.clamp(0.0, w - 1.0) as f64, y.clamp(0.0, h - 1.0) as f64);
        match e.eventType {
            sys::EVREventType_VREvent_MouseMove => p.mouse(PTR_FLAGS_MOVE, x, y),
            t @ (sys::EVREventType_VREvent_MouseButtonDown | sys::EVREventType_VREvent_MouseButtonUp) => {
                let b = match m.button {
                    sys::EVRMouseButton_VRMouseButton_Right => PTR_FLAGS_BUTTON2,
                    sys::EVRMouseButton_VRMouseButton_Middle => PTR_FLAGS_BUTTON3,
                    _ => PTR_FLAGS_BUTTON1,
                };
                let down = if t == sys::EVREventType_VREvent_MouseButtonDown { PTR_FLAGS_DOWN } else { 0 };
                p.mouse(b | down, x, y);
            }
            sys::EVREventType_VREvent_ScrollDiscrete | sys::EVREventType_VREvent_ScrollSmooth => {
                let dy = unsafe { e.data.scroll.ydelta };
                if dy != 0.0 {
                    p.wheel(false, dy as f64);
                }
            }
            _ => {}
        }
    }
    (n, worked)
}

/// A real controller's button was pressed, so it's the primary device now.
fn controller_pressed() {
    let mut k = KVM.lock().unwrap();
    if k.awake {
        eprintln!("controller button: the controller has the pointer");
        k.set_awake(false);
    }
}

fn distance(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    geometry::norm(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}

/// Puts the cursor overlay where the KVM's cursor is, and works out each panel's pixels
/// per degree from where the head is now.
fn update_pointer(cursor: vr::Handle, beam: &mut laser::Beam) {
    let eye = vr::head_position();
    let mut k = KVM.lock().unwrap();
    for (i, p) in panels().iter().enumerate() {
        let d = distance(&k.place[i].c, &eye).max(0.2);
        k.ppd[i] = p.size().0 as f64 / (2.0 * (k.place[i].width / 2.0 / d).atan()).to_degrees();
    }
    let hidden = HIDDEN.load(Relaxed);
    if !k.awake || hidden {
        call!(ov, HideOverlay, cursor);
        beam.tick(false, [0.0; 3], false);
        return;
    }
    k.land(); // the head may have moved, so the cursor lands on what you see now
    let (at, on_ours) = k.cursor();
    let dot = beam.tick(true, at, on_ours);
    if dot == laser::Dot::Hidden {
        call!(ov, HideOverlay, cursor); // SteamVR's own laser dot is the cursor there
    }
    let (r, g, b) = if dot == laser::Dot::Amber { (1.0, 0.7, 0.2) } else { (1.0, 1.0, 1.0) };
    call!(ov, SetOverlayColor, cursor, r, g, b);
    let (p, pl) = (panel(k.active), k.place[k.active]);
    // The dot always faces you so it's round from any angle, just in front of what it's on.
    // Off a panel (on a taskbar, or floating) it's pulled 6 mm toward you, because Plasma's bar
    // sits 2 mm in front of our frame and the Frame draws by depth (live: the dot was half behind the bar).
    let toward_eye = |p: [f64; 3]| {
        let v = [eye[0] - p[0], eye[1] - p[1], eye[2] - p[2]];
        let n = geometry::norm(&v).max(1e-6);
        [0, 1, 2].map(|i| p[i] + v[i] / n * 0.006)
    };
    let at = k.free.map(toward_eye).unwrap_or_else(|| {
        let (w, h) = p.size();
        let s = pl.on_surface((k.x / w as f64 - 0.5) * pl.width, (0.5 - k.y / h as f64) * pl.height, 0.004);
        [s[0][3] as f64, s[1][3] as f64, s[2][3] as f64]
    });
    let (yaw, pitch) = geometry::angles(&[at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]]);
    let m = panel_matrix(&Pose { centre: at, yaw, pitch, ..Default::default() });
    let d = distance(&at, &eye);
    call!(ov, SetOverlayWidthInMeters, cursor, (2.0 * d * 0.2f64.to_radians().tan()) as f32); // 0.4 degrees across at any distance
    vr::place(cursor, &m);
    if dot != laser::Dot::Hidden {
        call!(ov, ShowOverlay, cursor);
    }
}

/// D-042: sets each panel's level this tick (attention.rs) and logs it when it changes. An
/// empty window slot has none. A window waiting for its first frame, or a panel being carried,
/// runs at full rate, and so does one in sight taking your input, since you may be looking at
/// the keyboard. D-045: a remote out of view is peripheral, not paused; after that, what it
/// shows and your input refine it.
fn attend(att: &mut [attention::Attention], seen: &attention::Seen, grab: &grab::Grab, now: Instant) {
    use attention::Level;
    let k = KVM.lock().unwrap(); // held throughout; the input thread never takes a window's lock under it
    let theater = THEATER.load(Relaxed);
    for p in panels() {
        let i = p.index;
        let window = matches!(p.src, Source::Window(_));
        if let Source::Window(w) = &p.src
            && w.lock().unwrap().is_none()
        {
            att[i] = Default::default();
            p.level.store(Level::Full as u8, Relaxed);
            continue;
        }
        att[i].sense(p.change.swap(0, Relaxed) as f64 / 1000.0, now);
        if k.working(i) {
            att[i].input = Some(now);
        }
        let (want, why) = if window && !p.live() && !p.minimized() {
            (Level::Full, "waiting for its first frame") // a paused stream might never send it
        } else if grab.busy(i) {
            (Level::Full, "held")
        } else {
            let w = match attention::want(seen, p.minimized(), theater != usize::MAX && theater != i, &k.place[i]) {
                (Level::Paused, "out of view") if !window => (Level::Peripheral, "out of view"),
                w => w,
            };
            att[i].refine(w, now)
        };
        if let Some(l) = att[i].step(want, now) {
            p.level.store(l as u8, Relaxed);
            rdp::poke(p); // its RDP thread may be waiting out the old level's interval (gpu::wait)
            let every = if window { attention::PERIPHERAL_WINDOW } else { attention::PERIPHERAL };
            match l {
                Level::Paused => eprintln!("{}: paused ({why})", p.v.name),
                Level::Quiet => eprintln!("{}: quiet ({} frame/s)", p.v.name, 1000 / attention::QUIET_EVERY.as_millis()),
                Level::Peripheral => eprintln!("{}: peripheral ({} frames/s, {why})", p.v.name, 1000 / every.as_millis()),
                Level::Full => eprintln!("{}: full rate ({why})", p.v.name),
            }
        }
    }
}

/// Where a main-loop tick's time went (docs/stutter-plan.md fix 0), timing each phase in turn.
struct Laps {
    start: Instant,
    at: Instant,
    laps: Vec<(&'static str, Duration)>,
}

impl Laps {
    fn new() -> Laps {
        let now = Instant::now();
        Laps { start: now, at: now, laps: Vec::with_capacity(16) } // ~13 phases, so it doesn't regrow every tick
    }

    fn lap(&mut self, name: &'static str) {
        let now = Instant::now();
        self.laps.push((name, now - self.at));
        self.at = now;
    }

    /// The last `part` of the previous phase was really this one, so it moves over as `name`.
    fn split(&mut self, name: &'static str, part: Duration) {
        if let Some(l) = self.laps.last_mut() {
            l.1 = l.1.saturating_sub(part);
        }
        self.laps.push((name, part));
    }
}

/// The main loop's tick while nothing moves (efficiency-plan.md 3). Input, a new frame on a
/// panel you're looking at, and KWin's news wake it sooner (vr::wake).
const IDLE_TICK: Duration = Duration::from_millis(40);
/// After the last thing that moved, the loop keeps the display's rate this long.
const HOLD: Duration = Duration::from_millis(500);
/// The gaze is sampled at most this often (~30 a second). The levels (attention.rs) and the
/// gaze lock's 150 ms dwell don't need more, and each sample costs two calls into SteamVR.
const GAZE_EVERY: Duration = Duration::from_millis(30);

/// The main loop's pace: a tick each display frame while anything moves (the pointer awake,
/// a laser nearby, something carried or fading, input, SteamVR events), otherwise every IDLE_TICK.
/// ponytail: ticks are spaced a frame apart, not locked to vsync's phase. Use
/// GetTimeSinceLastVsync if the cursor beats against the display.
struct Pace {
    frame: Duration, // one display frame
    next: Instant,   // the next tick's deadline
    fast_until: Instant,
    fast: bool,
    why: &'static str, // what last asked for the display's rate
    ticks: (u32, u32), // since the last status line: all ticks, and those at the display's rate
}

impl Pace {
    fn new(hz: f32, now: Instant) -> Pace {
        let mut p = Pace { frame: Duration::ZERO, next: now, fast_until: now, fast: false, why: "-", ticks: (0, 0) };
        p.set_rate(hz);
        p
    }

    /// Sets the display's refresh rate (90 when SteamVR doesn't say).
    fn set_rate(&mut self, hz: f32) {
        let hz = if (30.0..=240.0).contains(&hz) { hz } else { 90.0 };
        self.frame = Duration::from_secs_f64(1.0 / hz as f64);
    }

    /// After a tick that began at `start`, sets the next one's deadline. `why` is what's moving now.
    fn plan(&mut self, start: Instant, why: Option<&'static str>) {
        if let Some(w) = why {
            (self.fast_until, self.why) = (start + HOLD, w);
        }
        self.fast = start < self.fast_until;
        self.ticks = (self.ticks.0 + 1, self.ticks.1 + self.fast as u32);
        let grid = self.next + self.frame; // a frame past the last deadline, so there's no drift
        self.next = if !self.fast {
            start + IDLE_TICK
        } else if grid > start && grid <= start + self.frame {
            grid
        } else {
            start + self.frame // late (a slow tick), or coming out of idle
        };
    }

    /// When to run the next tick: the deadline. When idle and something woke the loop, a frame
    /// after the last tick began, so a burst of wakes runs at most at the display's rate.
    fn until(&self, start: Instant, woken: bool) -> Instant {
        if woken && !self.fast { self.next.min(start + self.frame) } else { self.next }
    }
}

/// Logs `slow tick 412ms: events 0 uploads 3 ...` (ms) when a tick takes over 20 ms.
fn slow_tick(total: Duration, laps: &[(&str, Duration)]) -> Option<String> {
    (total > Duration::from_millis(20)).then(|| laps.iter().fold(format!("slow tick {}ms:", total.as_millis()), |s, (n, d)| format!("{s} {n} {}", d.as_millis())))
}

/// The two assets.rs tags for a remote, keyed by its name:
///   - its card's: what the user calls it (config::display), or else its hostname and monitor;
///   - its taskbar chip's (and the Machines window's row): what the user calls it.
/// A comma would split assets.rs's fields, so it's shown as ‚.
fn remote_tags(v: &config::Viewer, all: &[config::Viewer]) -> [String; 2] {
    let [r, g, b] = grab::REMOTE.map(|c| c as u8);
    let shown = config::display(v, all).replace(',', "\u{201a}");
    let card = if shown == v.name { format!("{},{}", v.host, v.port) } else { format!("{shown},frame") };
    [format!("tag={},{card},{r},{g},{b}", v.name), format!("tag=chip-{},{shown},frame,{r},{g},{b}", v.name)]
}

/// Reads a border tag from panels/assets.rs: width and height (u32 LE), then RGBA.
fn load_tag(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    let b = std::fs::read(path).ok()?;
    let (w, h) = (u32::from_le_bytes(b.get(0..4)?.try_into().ok()?) as usize, u32::from_le_bytes(b.get(4..8)?.try_into().ok()?) as usize);
    (b.len() == 8 + w * h * 4).then(|| (w, h, b[8..].to_vec()))
}

/// The Desktop was closed on purpose (the power chip, Right Ctrl + Esc, quit, or SteamVR
/// quitting us for a game). cc-launch then stops the desktop session too, so what's left costs
/// next to nothing (session/cc-rest). A signal (a restart, systemctl stop) quits without this,
/// and the session stays up.
pub fn close_desktop(how: &str) {
    eprintln!("closing Desktop ({how}): its session stops too");
    let _ = std::fs::write(format!("{}/.cache/control-center/desktop-closed", config::home_dir()), how);
    QUIT.store(true, Relaxed);
}

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Relaxed);
}

/// However main ends, this lets the devices go and puts the session's windows back
/// how they were (kwin.rs).
struct GiveBack;
impl Drop for GiveBack {
    fn drop(&mut self) {
        laser::hide();
        KVM.lock().unwrap_or_else(|e| e.into_inner()).return_devices();
        kwin::restore();
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--assets") && args.len() >= 4 {
        assets::draw(&args[2], &args[3], &args[4..]); // `--assets <dir> <font> tag=...` draws the Machines window's new tags
        return std::process::ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Shows a remote panel and starts its RDP thread after an `after` pause (start-up staggers
/// them). Used at start-up and by `viewer connect` (control.rs), so both keep the handle for shutdown.
fn connect(p: &'static Panel, after: Duration) -> std::thread::JoinHandle<()> {
    p.set_live(true);
    // (its sort order: painted back to front with the others, grab.rs paint_order)
    call!(ov, ShowOverlay, p.overlay);
    std::thread::spawn(move || {
        std::thread::sleep(after);
        if p.v.vnc.is_some() { vnc::run(p) } else { rdp::run(p) }
    })
}

/// Places a remote at its saved spot, or else a metre and a half in front of you. Returns its
/// pose and whether it was saved. Used at start and when Add machine fills one.
pub fn place_remote(p: &Panel) -> (Pose, bool) {
    let saved = config::home_pose(&p.v.name, Some(p.v.screen));
    let pose = saved.unwrap_or(Pose { centre: [0.0, 1.5, -1.5], width: 1.2, ..Default::default() });
    p.set_vert(pose.vert); // curved top to bottom means turned (kvm.rs set_place)
    KVM.lock().unwrap().set_pose(p.index, &panel_matrix(&pose), pose.width, pose.curve);
    (pose, saved.is_some())
}

/// Ends a remote's session (the Machines window, `viewer disconnect`): hidden, not live, and its
/// RDP thread woken from its wait. It ends within a second (main.rs joins it). Connect starts fresh.
pub fn disconnect(p: &Panel) {
    p.set_live(false);
    call!(ov, HideOverlay, p.overlay);
    rdp::poke(p);
    eprintln!("{}: disconnecting", p.v.name);
}

fn run() -> Result<(), String> {
    // One cc-panels at a time, since a second one couldn't own the overlays anyway.
    let cache = format!("{}/.cache/control-center", config::home_dir());
    let _ = std::fs::create_dir_all(&cache);
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(format!("{cache}/cc-panels.lock"));
    let lock = lock.map_err(|e| format!("lock: {e}"))?;
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("cc-panels is already running".into());
    }
    // A panic anywhere gives the laser's hand back before anything else.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        laser::hide();
        default_hook(info);
    }));
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }

    let mut minutes = 2.0; // a test session by default; everything comes back after this
    let mut wanted = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--restore" {
            kwin::restore(); // the wrapper, after a crash: put the windows back as they were
            return Ok(());
        }
        if a == "--for" {
            minutes = args.next().and_then(|m| m.parse().ok()).unwrap_or(minutes);
        } else {
            wanted.push(a);
        }
    }
    let viewers = config::viewers(&wanted);
    if viewers.is_empty() {
        return Err("no viewers to show (viewers.conf)".into());
    }

    // The Plasma session's look (theme.rs), then the cursor and the border tags in its font.
    let mut look = theme::init();
    let assets = format!("{cache}/assets");
    let font = theme::font();
    // Windows' tags are cached by app (windows.rs), and assets.rs only draws the glyphs when
    // they're missing, so they get redrawn for a new font (or the first time masks are drawn, with no stamp yet).
    if std::fs::read_to_string(format!("{assets}/font")).ok().as_deref() != Some(font.as_str()) {
        for e in std::fs::read_dir(&assets).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("tag-app-") || n == "glyphs.rgba" {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let all = config::viewers(&[]);
    let tags: Vec<String> = viewers.iter().flat_map(|v| remote_tags(v, &all)) // each panel's border tag and its taskbar chip's label
        .chain(control::machines::LABELS.iter().chain(&control::prefs::LABELS).chain(&control::workspace::LABELS).chain(&taskbar::LABELS).map(|(k, text)| format!("tag=ui-{k},{text},frame,0,0,0"))) // the Machines and Preferences windows'
        .collect();
    assets::draw(&assets, &font, &tags);
    let _ = std::fs::write(format!("{assets}/font"), &font);
    {
        let mut k = KVM.lock().unwrap();
        if let Some(s) = std::env::var("CC_SENSITIVITY").ok().and_then(|s| s.parse().ok()) {
            k.sensitivity = s;
        }
        k.gaze_lock = std::env::var("CC_GAZE").is_ok_and(|v| v == "1");
        k.rescan();
    }
    let _give_back = GiveBack;

    vr::init().map_err(|e| format!("SteamVR: {e}"))?;
    let dev = gpu::device().ok_or("no GPU buffers (GBM on renderD128)")?;
    // The workspace for this room (its SteamVR tracking universe). Its spots place the panels.
    let mut err = 0;
    let mut universe = call!(sys, GetUint64TrackedDeviceProperty, 0, sys::ETrackedDeviceProperty_Prop_CurrentUniverseId_Uint64, &mut err);
    let (workspace, new) = config::choose_workspace(universe);
    eprintln!(
        "workspace: {workspace}{}",
        if new { " (made for this room: the Workspace window's Save as workspace keeps it)" } else { "" }
    );
    let mut list = Vec::new();
    for v in viewers {
        let tag = load_tag(&format!("{assets}/tag-{}.rgba", v.name));
        list.push(Panel::new(list.len(), v, Source::Rdp, grab::REMOTE, tag.as_ref())?);
    }
    for n in 1..=SPARES {
        let v = config::Viewer { name: format!("spare-{n}"), ..NONE.clone() }; // its overlays' keys
        // best-effort, since SteamVR's 128 overlays are shared (if none are left, an add shows after a restart)
        let mut p = match Panel::new(list.len(), v, Source::Rdp, grab::REMOTE, None) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("spare-{n}: no overlay ({e}); no more spares");
                break;
            }
        };
        p.v = Fill(Mutex::new(None));
        p.set_live(false);
        list.push(p);
    }
    let first_window = list.len();
    for n in 1..=windows::SLOTS {
        let v = config::Viewer { name: format!("win-{n}"), user: String::new(), host: String::new(), port: 0, screen: 0, w: 1280, h: 800, auto: false, machine: String::new(), label: String::new(), pop: None, vnc: None };
        list.push(Panel::new(list.len(), v, Source::Window(Mutex::new(None)), grab::VIOLET, None)?);
    }
    let _ = PANELS.set(list);
    clipboard::start(); // the Frame session's clipboard; each RDP session brings its own channel
    {
        let mut k = KVM.lock().unwrap();
        k.place = vec![geometry::Placement::from_matrix(&[[0.0; 4]; 3], 1.0, 1.0, 0.0); panels().len()];
        k.ppd = vec![30.0; panels().len()];
    }

    let mut threads = Vec::new(); // (panel, its RDP thread)
    // Which remotes connect now (the launcher's cc-home autoconnect --write; if none, all of them)
    let auto = config::autoconnect(&workspace);
    if let Some(a) = &auto {
        eprintln!("autoconnect: {}", if a.is_empty() { "none".into() } else { a.join(", ") });
    }
    for p in panels()[..first_window].iter().filter(|p| p.used()) {
        let (pose, saved) = place_remote(p);
        if !config::member(&p.v) {
            p.set_live(false); // made but not connected: not one of this workspace's machines
            eprintln!("{}: not in workspace {workspace}", p.v.name);
            continue;
        }
        if auto.as_ref().is_some_and(|a| !a.contains(&p.v.name)) {
            p.set_live(false); // made but not connected: not marked autoconnect=yes, or not on a known network
            eprintln!("{}: not connected at start (autoconnect)", p.v.name);
            continue;
        }
        // one remote a second, not all at once, because their first full screens are the heaviest decode of start-up
        threads.push((p.index, connect(p, Duration::from_secs(p.index as u64))));
        eprintln!(
            "{}: {}:{} -> overlay controlcenter.panel.{}, {:.2} m wide{}",
            p.v.name, p.v.host, p.v.port, p.v.name, pose.width, if saved { "" } else { " (no saved spot)" }
        );
    }
    let mut windows = windows::Windows::start(first_window);

    let cursor = vr::create_overlay("controlcenter.cursor", "Command Center cursor").unwrap_or(0);
    vr::set_from_file(cursor, &format!("{assets}/cursor.png"));
    call!(ov, SetOverlaySortOrder, cursor, 200);
    {
        let mut k = KVM.lock().unwrap();
        let (w, h) = panel(0).size();
        (k.x, k.y) = (w as f64 / 2.0, h as f64 / 2.0);
        if k.device_count() > 0 {
            k.set_awake(true); // the keyboards stay the Frame's until a click on a panel (click to type)
        } else {
            k.recenter();
        }
    }
    let gaze = gaze::Gaze::new();
    if gaze.is_none() {
        eprintln!("gaze: no eye tracking; the pointer goes where the mouse takes it");
    }
    let input = std::thread::Builder::new().name("cc-input".into()).spawn(kvm::input_loop).map_err(|e| format!("input thread: {e}"))?;
    let mut beam = laser::Beam::new();
    let leases = std::thread::Builder::new().name("cc-lease".into()).spawn(laser::lease_thread).map_err(|e| format!("lease thread: {e}"))?;
    let control = control::open();

    let start = Instant::now();
    let mut last_slow = start; // the last slow tick; the warm-up lasts until they calm down
    eprintln!("warm-up: every panel at 5 frames/s until the start-up calms");
    if minutes > 0.0 {
        eprintln!("running for {minutes} min; Right Ctrl + Esc ends it");
    } else {
        eprintln!("running until stopped; Right Ctrl + Esc ends it");
    }
    let mut grab = grab::Grab::new(panels().len());
    let mut bar = taskbar::Taskbar::new(&assets);
    let mut machines = control::machines::Machines::new(&assets);
    let mut prefs = control::prefs::Prefs::new(&assets);
    let mut workspace_win = control::workspace::Workspace::new(&assets);
    let mut buffers: Vec<gpu::Buffers> = panels().iter().map(|_| gpu::Buffers::default()).collect();
    let mut status = Instant::now();
    let mut ticks = 0u64;
    let mut att: Vec<attention::Attention> = panels().iter().map(|_| Default::default()).collect();
    let mut on_head = true; // ponytail: assume it's worn at start; the first take-off (VREvent 104) says otherwise
    let mut gaze_seen: Option<([f64; 3], [f64; 3], Instant)> = None; // the last valid gaze sample
    let display_hz = || {
        let mut err = 0;
        call!(sys, GetFloatTrackedDeviceProperty, 0, sys::ETrackedDeviceProperty_Prop_DisplayFrequency_Float, &mut err)
    };
    let mut pace = Pace::new(display_hz(), Instant::now());
    let _ = vr::MAIN.set(std::thread::current()); // vr::wake ends the wait between ticks
    let mut gaze_tried: Option<Instant> = None;
    let mut last_event = 0; // the last SteamVR event's type, for the loop's status line
    let mut was_away = false; // hidden or under a game last tick
    while !QUIT.load(Relaxed) {
        let mut laps = Laps::new();
        ticks += 1;
        if ticks % 30 == 0 {
            look.poll(); // on a Plasma theme switch everything gets redrawn in the new one
        }
        if minutes > 0.0 && start.elapsed().as_secs_f64() > minutes * 60.0 {
            eprintln!("time's up: ending");
            QUIT.store(true, Relaxed); // every thread (input, streams) ends on this
            break;
        }
        if status.elapsed() > Duration::from_secs(5) {
            status = Instant::now();
            let k = KVM.lock().unwrap();
            for (p, b) in panels()[..first_window].iter().zip(&mut buffers).filter(|(p, _)| p.used()) {
                eprintln!(
                    "{}: {}, {:.1} frames/s, visible {}, {}, {:.0} px/deg",
                    p.v.name,
                    if p.connected.load(Relaxed) { "connected" } else { "not connected" },
                    b.uploads as f64 / 5.0,
                    call!(ov, IsOverlayVisible, p.overlay) as i32,
                    format!("{:?}", p.level()).to_lowercase(),
                    k.ppd[p.index]
                );
                b.uploads = 0;
            }
            drop(k);
            windows.status();
            let mut k = KVM.lock().unwrap();
            eprintln!(
                "kvm: {} on {} at {:.0},{:.0}, typing to {}, looking at {} (gaze lock {}); sent {} moves, {} clicks, {} keys, {} wheel; {} devices",
                match (k.awake, k.engaged) {
                    (true, true) => "awake, keyboard to the remotes",
                    (true, false) => "awake, keyboard to the Frame",
                    (false, true) => "asleep, keyboard to the remotes",
                    (false, false) => "asleep, keyboard to the Frame",
                },
                panel(k.active).v.name,
                k.x,
                k.y,
                if k.kbd_shell { "plasma" } else { panel(k.kbd).v.name.as_str() },
                k.focus.map_or("-", |f| panel(f).v.name.as_str()),
                if k.gaze_lock { "on" } else { "off" },
                k.moves,
                k.clicks,
                k.keystrokes,
                k.wheels,
                k.device_count()
            );
            (k.moves, k.clicks, k.keystrokes, k.wheels) = (0, 0, 0, 0);
            drop(k);
            pace.set_rate(display_hz()); // (changed in SteamVR's settings)
            let (all, fast) = std::mem::take(&mut pace.ticks);
            eprintln!(
                "loop: {:.1} ticks/s, {}% at {:.0} Hz; last kept there by {}{}",
                all as f64 / 5.0,
                fast * 100 / all.max(1),
                1.0 / pace.frame.as_secs_f64(),
                pace.why,
                if pace.why == "SteamVR events" { format!(" (type {last_event})") } else { String::new() }
            );
        }
        laps.lap("status");
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        let mut events = 0;
        while call!(sys, PollNextEvent, &mut e, size_of::<vr::VREvent_t>() as u32) {
            (events, last_event) = (events + 1, e.eventType);
            if e.eventType == sys::EVREventType_VREvent_Quit {
                close_desktop("SteamVR quit it"); // e.g. a game launched over the Desktop
            }
            if e.eventType == sys::EVREventType_VREvent_DashboardActivated {
                KVM.lock().unwrap().set_engaged(false, "dashboard opened"); // click to type: typing goes to it
            }
            if e.eventType == sys::EVREventType_VREvent_ButtonPress && vr::is_real_controller(e.trackedDeviceIndex) {
                controller_pressed(); // anywhere, not only on our panels
            }
            // the Frame relocalized (vrserver.txt: "Standing origin changed. Sending reset standing zero pose event")
            // (a new universe, like the dummy one's first localization at start, doesn't move any saved spot)
            if e.eventType == sys::EVREventType_VREvent_StandingZeroPoseReset {
                let now = call!(sys, GetUint64TrackedDeviceProperty, 0, sys::ETrackedDeviceProperty_Prop_CurrentUniverseId_Uint64, &mut err);
                if now == universe {
                    machines.relocalized();
                    grab.relocalized(); // aligned monitors' snap buttons switch to align
                } else {
                    eprintln!("tracking: universe {universe} -> {now}");
                    universe = now;
                    // ponytail: the workspace is picked at start. A room change mid-session shows
                    // on the Workspace page (Use / Load there) instead of switching under you
                    config::UNIVERSE.store(cc_proto::conf::room(now), Relaxed);
                }
            }
            // the headset's (device 0) proximity sensor: off the head, every stream pauses
            let worn = match e.eventType {
                sys::EVREventType_VREvent_TrackedDeviceUserInteractionStarted => Some(true),
                sys::EVREventType_VREvent_TrackedDeviceUserInteractionEnded => Some(false),
                _ => None,
            };
            if let Some(on) = worn.filter(|&on| e.trackedDeviceIndex == 0 && on != on_head) {
                on_head = on;
                eprintln!("headset: {}", if on { "on" } else { "off" });
            }
        }
        laps.lap("events");
        let now = Instant::now();
        let head = vr::head().map(|m| ([0, 1, 2].map(|r| m[r][3] as f64), [0, 1, 2].map(|r| -m[r][2] as f64)));
        let gaze_ray = gaze_seen.filter(|g| now - g.2 < kvm::GAZE_STALE).map(|g| (g.0, g.1));
        let seen = attention::Seen { hidden: HIDDEN.load(Relaxed), game: bar.game(), on_head, head, gaze: gaze_ray };
        attend(&mut att, &seen, &grab, now);
        // click to type: the Desktop hidden or a game starting gives the keyboards back, once on the change
        let away = seen.hidden || seen.game;
        if away && !was_away {
            KVM.lock().unwrap().set_engaged(false, if seen.hidden { "Desktop hidden" } else { "game running" });
        }
        was_away = away;
        laps.lap("attention");
        for (p, b) in panels().iter().zip(&mut buffers) {
            // A paused remote isn't asked to stop sending (Suppress Output), because krdp ignores
            // the Refresh Rect after it and a still desktop stayed stale (live). It keeps decoding
            // and only its uploads wait (gpu::write), so when you look back the picture is current.
            b.present(dev, p); // its RDP thread already wrote it (gpu::write)
        }
        gpu::raw_tick(); // UI overlays' pictures that waited a tick (gpu::show_raw)
        laps.lap("uploads");
        let mut mouse_has_it = KVM.lock().unwrap().awake;
        let mut lasered = 0;
        for p in panels() {
            let (n, worked) = laser(p, &mut mouse_has_it, &mut grab);
            if worked {
                att[p.index].input = Some(now); // D-045: a laser working it counts as input (ImageLoaded, Shown... don't)
            }
            lasered += n;
        }
        laps.lap("lasers");
        {
            let mut k = KVM.lock().unwrap(); // the input thread may hold it
            laps.lap("kvm-lock");
            grab.poll(&mut k);
            grab.game = bar.game(); // as of its last tick
            grab.update(&mut k);
        }
        laps.lap("grab");
        windows.tick(&mut grab);
        laps.lap("windows");
        bar.tick(&mut grab, &mut windows);
        laps.lap("taskbar");
        laps.split("plasmabar", bar.plasma_time);
        for i in grab.take_aligns() {
            // a card's align button (after a relocalization): the Machines window shows its
            // progress and the "move everything with it" offer
            control::machines::OPEN.store(true, Relaxed);
            machines.align_panel(i);
        }
        machines.tick(&mut grab, &mut windows, &mut bar);
        laps.lap("machines");
        prefs.tick(&mut grab, &mut windows, &mut bar);
        laps.lap("prefs");
        workspace_win.tick(&mut grab, &mut windows, &mut bar);
        laps.lap("workspace");
        if let Some(s) = &control {
            control::handle(s, &mut grab, &mut windows, &mut bar);
            for i in control::to_connect() {
                if !panels()[i].used() {
                    continue; // removed while queued
                }
                if threads.iter().any(|(j, t)| *j == i && !t.is_finished()) {
                    control::connect_later(i); // its last session is still ending (disconnect), try next tick
                    continue;
                }
                threads.retain(|(j, _)| *j != i); // (it ended)
                eprintln!("{}: connecting (viewer connect)", panels()[i].v.name);
                threads.push((i, connect(&panels()[i], Duration::ZERO)));
            }
        }
        popout::tick(&mut KVM.lock().unwrap());
        laps.lap("control");
        if let Some(g) = &gaze
            && gaze_tried.is_none_or(|t| laps.start.duration_since(t) >= GAZE_EVERY)
        {
            gaze_tried = Some(laps.start);
            let sample = g.sample();
            if let Some((o, d)) = sample {
                gaze_seen = Some((o, d, Instant::now())); // where you look decides the levels (attend)
            }
            KVM.lock().unwrap().gaze(sample);
        }
        laps.lap("gaze");
        update_pointer(cursor, &mut beam);
        laps.lap("pointer");
        if let Some(line) = slow_tick(laps.start.elapsed(), &laps.laps) {
            eprintln!("{line}");
            last_slow = Instant::now();
        }
        // The start-up warm-up (attention::WARM, every panel at 5 frames/s) ends once the ticks calm down.
        let settled = || panels().iter().all(|p| match &p.src {
            // or not connected at start (autoconnect), so there's nothing to wait for
            Source::Rdp => p.connected.load(Relaxed) || !p.live(),
            // a paused window sends no frames until you look (live: a tray app's never came)
            Source::Window(w) => w.lock().unwrap().is_none() || p.live() || p.level() == attention::Level::Paused,
        });
        if attention::WARM.load(Relaxed) && attention::warm_over(start.elapsed(), last_slow.elapsed(), settled()) {
            attention::WARM.store(false, Relaxed);
            eprintln!("warm-up over after {:.1} s: panels at their own rates", start.elapsed().as_secs_f64());
        }
        let awake = KVM.lock().unwrap().awake;
        let why = [
            (awake, "the pointer (awake)"),
            (lasered > 0, "a laser on a panel"),
            // a panel you're looking at shows every frame its stream sends. Uploads happen once a
            // tick, and idle ticks capped watched screens at 25 frames/s (live: "a little choppier")
            (panels().iter().any(|p| p.live() && p.level() == attention::Level::Full), "a panel looked at"),
            (grab.moving(), "a card (near, fading) or carrying"),
            (bar.moving(), "the taskbar"),
            (machines.moving(), "the Machines window"),
            (prefs.moving(), "the Preferences window"),
            (workspace_win.moving(), "the Workspace window"),
            (windows.plasma_moving(), "Plasma's bar"),
            (kvm::POKED.swap(false, Relaxed), "input"),
            (events > 0, "SteamVR events"),
        ];
        pace.plan(laps.start, why.into_iter().find_map(|(on, w)| on.then_some(w)));
        loop {
            let now = Instant::now();
            let until = pace.until(laps.start, vr::WOKEN.load(Relaxed));
            if now >= until {
                break;
            }
            std::thread::park_timeout(until - now);
        }
        if !pace.fast {
            vr::WOKEN.store(false, Relaxed); // (left set at the display's rate, so no wake interrupts the frame's wait)
        }
    }

    let _ = input.join();
    let _ = leases.join(); // it sends the driver "hide" as it ends
    {
        // give the devices back straight away, not after slow sessions have ended
        let mut k = KVM.lock().unwrap();
        k.set_awake(false);
        k.set_engaged(false, "closing");
        k.return_devices();
    }
    call!(ov, DestroyOverlay, cursor);
    bar.destroy();
    machines.destroy();
    prefs.destroy();
    workspace_win.destroy();
    windows.stop();
    for (_, t) in threads {
        let _ = t.join();
    }
    for p in panels() {
        call!(ov, DestroyOverlay, p.overlay);
    }
    for (p, b) in panels().iter().zip(&mut buffers) {
        b.free(&mut p.gpu.lock().unwrap()); // the RDP threads have ended
    }
    vr::shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_tags_show_the_label_without_breaking_assets_fields() {
        let v = config::Viewer { name: "desk-wide".into(), host: "10.0.0.1".into(), port: 3401, label: "Desk, left".into(), ..Default::default() };
        let [card, chip] = remote_tags(&v, &[]);
        assert!(card.starts_with("tag=desk-wide,Desk\u{201a} left,frame,"), "{card}");
        assert!(chip.starts_with("tag=chip-desk-wide,Desk\u{201a} left,frame,"), "{chip}");
        assert_eq!(chip.split(',').count(), 6, "assets.rs's six fields");
        let v = config::Viewer { label: String::new(), machine: "../x".into(), ..v };
        assert!(remote_tags(&v, &[])[0].starts_with("tag=desk-wide,10.0.0.1,3401,"), "no label: hostname and monitor");
    }

    #[test]
    fn slow_ticks_say_where_the_time_went() {
        let ms = Duration::from_millis;
        assert_eq!(slow_tick(ms(20), &[("grab", ms(20))]), None, "20 ms is a tick, not a slow one");
        let mut l = Laps::new();
        l.laps = vec![("grab", ms(18)), ("taskbar", ms(33))];
        l.split("plasmabar", ms(2));
        assert_eq!(slow_tick(ms(412), &l.laps).unwrap(), "slow tick 412ms: grab 18 taskbar 31 plasmabar 2");
    }

    #[test]
    fn the_loop_keeps_the_display_rate_only_while_something_moves() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut p = Pace::new(100.0, t0);
        assert_eq!(p.frame, ms(10));
        assert_eq!(Pace::new(0.0, t0).frame, Duration::from_secs_f64(1.0 / 90.0), "no rate from SteamVR: 90 Hz");
        // idle: every IDLE_TICK, and a wake pulls the tick in to a frame after the last one began
        p.plan(t0, None);
        assert!(!p.fast && p.next == t0 + IDLE_TICK);
        assert_eq!(p.until(t0, false), t0 + IDLE_TICK);
        assert_eq!(p.until(t0, true), t0 + ms(10));
        // something moves: a tick each frame, on the grid from the last deadline (no drift)
        let t1 = t0 + ms(25);
        p.plan(t1, Some("pointer"));
        assert!(p.fast && p.next == t1 + ms(10), "from idle: a frame from now");
        p.plan(t1 + ms(11), None);
        assert_eq!(p.next, t1 + ms(20), "on the grid, though this tick began late");
        assert_eq!(p.until(t1 + ms(11), true), t1 + ms(20), "at the display's rate a wake changes nothing");
        p.plan(t1 + ms(45), None);
        assert_eq!(p.next, t1 + ms(55), "a slow tick: a frame from now, no burst to catch up");
        // HOLD after the last movement, it's idle again
        let t2 = t1 + HOLD + ms(1);
        p.plan(t2, None);
        assert!(!p.fast && p.next == t2 + IDLE_TICK);
        assert_eq!((p.ticks, p.why), ((5, 3), "pointer"));
    }
}
