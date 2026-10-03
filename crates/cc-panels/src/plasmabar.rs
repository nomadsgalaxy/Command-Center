//! Plasma's own taskbar (docs/window-panels-design2.md, "Plasma taskbar as built"). This is
//! the session's real KDE panel, plasmashell's dock, shown as one floating overlay,
//! `controlcenter.plasma`, with its popups: the launcher, the tray, task previews, the
//! calendar, menus and tooltips. It's one overlay because SteamVR's 128 are shared by every app.
//!
//! Two KWin screencasts feed it:
//!   - the panel's own window, while that's all there is to show. It only sends frames when
//!     the panel changes, so an idle bar is free.
//!   - the panel's whole output, for the first frame and while a popup is open. I crop it with
//!     SetOverlayTextureBounds to the panel's rectangle, grown upward to take in the popups,
//!     so the bar itself stays where it is. This stream copies every repaint of the output (a
//!     video on WL-0 counts), so it only runs when it has to.
//!
//! cc-windows.js reports the geometry of the panel and plasmashell's other windows as "shell"
//! events; they never become window panels. taskbar.rs places the overlay in its frame's well,
//! 2 mm in front of the frame since the Frame draws by depth, from settings.json "taskbar".
//!
//! Lasers and the mouse drive it through fake_input at the matching point in the session
//! (windows.rs `shell_mouse`). There's no raise gate because the panel and its popups sit
//! above every window. A click on it moves typing to the session, for Kickoff's search.
//! Lasers normally reach the bar through the taskbar's frame (`on_bar`). This overlay only
//! takes them itself while a popup you click into is open, or a tooltip the laser went up into
//! off the bar (`TIP`, a task's preview), because otherwise SteamVR's laser would hit it over
//! the frame's chips.
//!
//! ponytail: an idle panel keeps the output's stream going until it next repaints (its clock,
//! a hover), since KWin sends a window's stream nothing until it changes.
use crate::geometry::{Mat, Placement};
use crate::kvm::KVM;
use crate::session::{Output, Session, StreamEvent};
use crate::{call, capture, session, taskbar, vr, windows};
use freerdp_sys::{PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3, PTR_FLAGS_DOWN, PTR_FLAGS_MOVE};
use openvr_sys as sys;
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const KEY: u64 = 1 << 32; // its streams' keys in capture.rs, past the window slots (0..SLOTS): the output's, then the window's
const BAR_FPS: u32 = 15; // frame-rate cap for the bar's own stream, it's not video (docs/stutter-plan.md 4b)
const POPUP_FPS: u32 = 30; // the output's cap, same as a window panel's, since it shows the popups (Kickoff's hover, its search's echo)
const LINGER: Duration = Duration::from_secs(3); // hidden this long and its streams close, so a glance at the wrist doesn't reopen them

/// x, y, w, h in the session's global (logical) coordinates, as KWin reports geometry.
pub type Rect = [i32; 4];

/// A popup of Plasma's is open on its panel's output (windows.rs `shell_outside`).
pub static OPEN: AtomicBool = AtomicBool::new(false);
/// A laser went up off the bar with a tooltip open (a task's preview with thumbnails, close and
/// media controls; taskbar.rs). This overlay takes lasers until it closes or the laser leaves it.
pub static TIP: AtomicBool = AtomicBool::new(false);
/// What the overlay shows now (None: hidden), for input from any thread.
static CROP: Mutex<Option<Rect>> = Mutex::new(None);
/// The panel and every open plasmashell popup, shown here or not. They're above every window.
static ABOVE: Mutex<Vec<Rect>> = Mutex::new(Vec::new());

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let (x0, y0) = (a[0].max(b[0]), a[1].max(b[1]));
    let (x1, y1) = ((a[0] + a[2]).min(b[0] + b[2]), (a[1] + a[3]).min(b[1] + b[3]));
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
    [x0, y0, (a[0] + a[2]).max(b[0] + b[2]) - x0, (a[1] + a[3]).max(b[1] + b[3]) - y0]
}

fn inside(r: Rect, x: f64, y: f64) -> bool {
    x >= r[0] as f64 && y >= r[1] as f64 && x < (r[0] + r[2]) as f64 && y < (r[1] + r[3]) as f64
}

/// What the overlay shows of the output: the panel, grown to take in its open popups. Only
/// what's on the output, since the stream has nothing else. None if the panel isn't on it.
fn crop(panel: Rect, popups: &[Rect], out: Rect) -> Option<Rect> {
    let bar = intersect(panel, out)?;
    Some(popups.iter().filter_map(|&r| intersect(r, out)).fold(bar, union))
}

/// How far the crop's middle is from the panel's, in metres right and up at mpp metres a
/// pixel. The overlay moves that much so the bar stays put while the crop grows.
fn shift(panel: Rect, crop: Rect, mpp: f64) -> (f64, f64) {
    let mid = |r: Rect, k: usize| r[k] as f64 + r[k + 2] as f64 / 2.0;
    ((mid(crop, 0) - mid(panel, 0)) * mpp, (mid(panel, 1) - mid(crop, 1)) * mpp)
}

/// The crop as bounds on the output's stream: u, v min, then max (u right, v down, 0 to 1).
fn bounds(crop: Rect, out: Rect) -> [f32; 4] {
    let (w, h) = (out[2].max(1) as f32, out[3].max(1) as f32);
    let (u, v) = ((crop[0] - out[0]) as f32 / w, (crop[1] - out[1]) as f32 / h);
    [u, v, u + crop[2] as f32 / w, v + crop[3] as f32 / h]
}

/// A point on the overlay (fractions right and up from its bottom left) mapped through the
/// crop into the session's global coordinates, clamped to its last pixel.
fn global(crop: Rect, fx: f64, fy: f64) -> (f64, f64) {
    let (w, h) = ((crop[2] - 1).max(0) as f64, (crop[3] - 1).max(0) as f64);
    (crop[0] as f64 + fx.clamp(0.0, 1.0) * w, crop[1] as f64 + (1.0 - fy.clamp(0.0, 1.0)) * h)
}

/// The session point under the overlay at fractions right and up from its bottom left, for
/// kvm.rs and the lasers. None while it's hidden.
pub fn at(fx: f64, fy: f64) -> Option<(f64, f64)> {
    CROP.lock().unwrap().map(|c| global(c, fx, fy))
}

/// Is this session point under Plasma's panel or one of its open popups? windows.rs needs it
/// because a window panel's click there would land on Plasma, not its window.
pub fn covers(x: f64, y: f64) -> bool {
    ABOVE.lock().unwrap().iter().any(|&r| inside(r, x, y))
}

/// An output's place in the session, in logical units.
fn logical(o: &Output) -> Rect {
    let s = o.scale.max(1);
    [o.x, o.y, o.w / s, o.h / s]
}

/// What a stream is of: the window's uuid or the output's name, the output's place, and the
/// session connection (Arc::as_ptr). If any of it changes, it's a new stream.
type Of = (String, Rect, usize);

/// One of its two streams.
struct Src {
    key: u64,
    name: &'static str,
    retry: Duration, // after a failure or a close, don't reopen sooner than this
    fps: u32,        // its frame-rate cap
    of: Option<Of>,
    stream: Option<session::Stream>,
    feed: Option<Arc<capture::Feed>>,
    shown: capture::Shown,
    mark: u32, // frame count when we last switched away from it
    retry_at: Option<Instant>,
}

impl Src {
    fn new(key: u64, name: &'static str, retry: Duration, fps: u32) -> Src {
        Src { key, name, retry, fps, of: None, stream: None, feed: None, shown: capture::Shown::default(), mark: 0, retry_at: None }
    }

    /// A frame arrived since we last switched away from it.
    fn fresh(&self) -> bool {
        self.feed.as_ref().is_some_and(|f| f.frames.load(Relaxed) > self.mark)
    }

    /// Its newest frame onto the overlay, if there's one.
    fn tick(&mut self, cap: &capture::Capture, ov: vr::Handle) -> bool {
        let Some(f) = &self.feed else { return false };
        self.shown.tick(cap, self.key, f, ov, self.name)
    }

    /// Handles KWin's news. True if the stream failed or closed.
    fn pump(&mut self, cap: &capture::Capture) -> bool {
        let mut done = false;
        while let Some(e) = self.stream.as_ref().and_then(|s| s.events.try_recv().ok()) {
            match e {
                StreamEvent::Created(node) => {
                    let f = cap.open(self.key, node, self.name, self.fps);
                    // its frames don't wake the main loop, they go up on its next tick (40 ms idle).
                    // A laser or the mouse on it keeps the loop at the display's rate anyway.
                    f.quiet.store(true, Relaxed);
                    self.feed = Some(f);
                }
                StreamEvent::Failed(e) => {
                    eprintln!("{}: stream failed: {e}", self.name);
                    done = true; // KWin sends no Closed after it
                }
                StreamEvent::Closed => done = true,
            }
        }
        done
    }

    /// Closes it so KWin stops copying for us and gives its buffers back. It must not be on the overlay.
    fn close(&mut self, cap: &capture::Capture) {
        self.stream = None;
        if self.feed.take().is_some() {
            cap.close(self.key);
        }
        self.shown.free();
        (self.shown, self.mark, self.of) = (capture::Shown::default(), 0, None);
    }
}

pub struct Plasma {
    ov: vr::Handle,
    panel: Option<(String, Rect)>, // Plasma's panel, its dock window: uuid, geometry
    popups: Vec<(String, Rect, bool)>, // plasmashell's other open windows: popups, menus, tooltips (false)
    out: Src,                      // the panel's output
    win: Src,                      // the panel's own window
    on_win: bool,                  // which of them is on the overlay
    live: bool,                    // a frame is showing
    visible: bool,
    clickable: bool,            // it takes lasers itself (a popup is open), otherwise the taskbar's frame forwards them
    hidden_at: Option<Instant>, // when it stopped being wanted
    placed: Option<(Mat, u32, Rect, u32, [f32; 4], u32)>, // as last placed: its matrix, the device it's on (MAX: the room), the crop, width in mm, texture bounds, curve in mm
    held: u32,             // laser buttons down on it, in RDP's flags
    pub moving: bool,      // a laser on it this frame (events or a held press), which sets the main loop's pace
    pub released: Vec<u32>, // devices whose button came up on it, ending a hold on the taskbar's frame
}

impl Plasma {
    pub fn new() -> Plasma {
        let h = vr::create_overlay("controlcenter.plasma", "Command Center Plasma taskbar").unwrap_or_else(|e| {
            eprintln!("plasma: no overlay: {e}"); // SteamVR's 128 are shared, and we're out
            0
        });
        // no input method until a popup is open (`tick` sets it)
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_SendVRDiscreteScrollEvents, true);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_IgnoreTextureAlpha, true);
        call!(ov, SetOverlaySortOrder, h, taskbar::SORT + 1); // in front of the taskbar's frame, in case depth isn't used
        let mut scale = sys::HmdVector2_t { v: [1.0, 1.0] }; // events as fractions, since the crop changes under them
        call!(ov, SetOverlayMouseScale, h, &mut scale);
        Plasma {
            ov: h,
            panel: None,
            popups: Vec::new(),
            out: Src::new(KEY, "plasma output", Duration::from_secs(2), POPUP_FPS),
            // ponytail: KWin may refuse a window stream of the dock, so we fall back to the output's and retry rarely
            win: Src::new(KEY + 1, "plasma panel", Duration::from_secs(30), BAR_FPS),
            on_win: false,
            live: false,
            visible: false,
            clickable: false,
            hidden_at: None,
            placed: None,
            held: 0,
            moving: false,
            released: Vec::new(),
        }
    }

    /// A "shell" event: Plasma's panel or another plasmashell window was shown (with its
    /// geometry) or went away.
    pub fn event(&mut self, e: &Value) {
        let uuid = e["uuid"].as_str().unwrap_or_default().to_string();
        let r = windows::geom(e);
        let shown = e["shown"].as_bool().unwrap_or(false) && r[2] > 0 && r[3] > 0;
        if e["role"] == "panel" {
            if shown {
                self.panel = Some((uuid, r)); // ponytail: one panel (the last reported)
            } else if self.panel.as_ref().is_some_and(|p| p.0 == uuid) {
                self.panel = None;
            }
        } else {
            self.popups.retain(|p| p.0 != uuid);
            if shown {
                self.popups.push((uuid, r, e["role"] != "tooltip"));
            }
        }
    }

    /// The script (re)loaded, so it'll report everything again.
    pub fn clear(&mut self) {
        (self.panel, self.popups) = (None, Vec::new());
    }

    /// The panel's width and height in pixels, while the session has one and we have an
    /// overlay. taskbar.rs's frame holds it, and since it's fit-content it changes as apps open.
    pub fn size(&self) -> Option<(i32, i32)> {
        self.panel.as_ref().filter(|_| self.ov != 0).map(|p| (p.1[2], p.1[3]))
    }

    fn src(&mut self, win: bool) -> &mut Src {
        if win { &mut self.win } else { &mut self.out }
    }

    /// Runs every frame (taskbar.rs, through windows.rs).
    ///   - `at`: where the taskbar's frame is, in the room and on its device (MAX: the room).
    ///     None while it's hidden or fading, because below alpha 1 this overlay would look like
    ///     a tinted sheet.
    ///   - `lift`: how many metres above the frame's middle the bar's well is.
    ///   - `mpp`: metres a pixel.
    ///   - `curve`: the frame's curve radius as drawn (taskbar.rs), 0 for flat.
    /// It streams while shown (and for LINGER after), only puts new frames up, and places the
    /// crop on the frame's surface.
    pub fn tick(&mut self, ses: Option<&Arc<Session>>, cap: &capture::Capture, at: Option<(Mat, Mat, u32)>, lift: f64, mpp: f64, curve: f64) {
        self.events();
        let now = Instant::now();
        self.hidden_at = if at.is_some() { None } else { self.hidden_at.or(Some(now)) };
        // the output the panel is on
        let out = ses.zip(self.panel.as_ref()).and_then(|(s, (_, p))| {
            let c = [p[0] + p[2] / 2, p[1] + p[3] / 2, 1, 1];
            s.outputs.lock().unwrap().iter().find(|o| intersect(logical(o), c).is_some()).cloned()
        });
        let rects: Vec<Rect> = self.popups.iter().map(|p| p.1).collect();
        // from the session, streamed or not, because a popup left open while the bar's hidden still closes on a click outside
        let open = out.as_ref().is_some_and(|o| rects.iter().any(|&r| intersect(r, logical(o)).is_some()));
        OPEN.store(open, Relaxed);
        let on = |p: &&(String, Rect, bool)| out.as_ref().is_some_and(|o| intersect(p.1, logical(o)).is_some());
        let tip = self.popups.iter().filter(on).any(|p| !p.2);
        if !tip {
            TIP.store(false, Relaxed);
        }
        let clickable = self.popups.iter().filter(on).any(|p| p.2) || tip && TIP.load(Relaxed);
        if clickable != self.clickable {
            self.clickable = clickable;
            let m = if clickable { sys::VROverlayInputMethod_Mouse } else { sys::VROverlayInputMethod_None };
            call!(ov, SetOverlayInputMethod, self.ov, m);
        }
        *ABOVE.lock().unwrap() = self.panel.iter().map(|p| p.1).chain(rects.iter().copied()).collect();
        let want = out.is_some() && self.hidden_at.is_none_or(|t| now - t < LINGER);
        let sid = ses.map_or(0, |s| Arc::as_ptr(s) as usize);
        // The window's stream while wanted; the output's for the first frame and while a popup is open.
        let win_of = self.panel.as_ref().filter(|_| want).map(|p| (p.0.clone(), [0; 4], sid));
        let out_of = out.as_ref().filter(|_| want && (open || !(self.live && self.on_win))).map(|o| (o.name.clone(), logical(o), sid));
        self.sync(false, out_of, ses, out.as_ref(), cap, now);
        self.sync(true, win_of, ses, None, cap, now);
        // Which one goes up: the window's once it has a frame, unless a popup is open and the
        // output's has one. Only a new frame goes up, so an idle bar costs nothing here.
        let cur = self.live.then_some(self.on_win);
        let pick = match (open, cur) {
            (false, Some(true)) => true,
            (false, _) => self.win.fresh(),
            (true, Some(true)) => !self.out.fresh(),
            (true, _) => false,
        };
        let ov = self.ov;
        let up = self.src(pick).tick(cap, ov);
        if !up && let Some(c) = cur.filter(|&c| c != pick) {
            self.src(c).tick(cap, ov); // the other's frame isn't ready (or can't be imported), so keep this one going
        }
        if up {
            if cur == Some(true) && !pick {
                self.win.mark = self.win.feed.as_ref().map_or(0, |f| f.frames.load(Relaxed)); // switch back to it on its next frame
            }
            if !self.live {
                eprintln!("plasma: first frame ({})", self.src(pick).name);
            }
            (self.on_win, self.live) = (pick, true);
        }
        let cropped = self.panel.as_ref().map(|p| p.1).and_then(|p| {
            if self.on_win {
                return Some((p, p, [0.0, 0.0, 1.0, 1.0]));
            }
            let o = self.out.of.as_ref()?.1;
            let c = crop(p, &rects, o)?;
            Some((p, c, bounds(c, o)))
        });
        let show = self.live && at.is_some() && cropped.is_some();
        if show != self.visible {
            self.visible = show;
            if show {
                call!(ov, ShowOverlay, self.ov);
            } else {
                self.hide();
            }
        }
        let (true, Some((world, local, dev)), Some((p, c, b))) = (show, at, cropped) else { return };
        let (dx, dy) = shift(p, c, mpp);
        let (world, local) = (taskbar::in_well(&world, curve, dx, lift + dy), taskbar::in_well(&local, curve, dx, lift + dy));
        let width = c[2] as f64 * mpp;
        let placed = Some((local, dev, c, (width * 1000.0).round() as u32, b, (curve * 1000.0).round() as u32));
        if self.placed != placed {
            if self.placed.is_none_or(|q| q.4 != b) {
                let mut t = sys::VRTextureBounds_t { uMin: b[0], vMin: b[1], uMax: b[2], vMax: b[3] };
                call!(ov, SetOverlayTextureBounds, self.ov, &mut t);
            }
            call!(ov, SetOverlayWidthInMeters, self.ov, width as f32);
            call!(ov, SetOverlayCurvature, self.ov, crate::taskbar::curvature(width, curve));
            if dev == u32::MAX {
                vr::place(self.ov, &local);
            } else {
                let mut t = sys::HmdMatrix34_t { m: local };
                call!(ov, SetOverlayTransformTrackedDeviceRelative, self.ov, dev, &mut t);
            }
            self.placed = placed;
        }
        *CROP.lock().unwrap() = Some(c);
        KVM.lock().unwrap().plasma = Some(Placement::from_matrix(&world, width, c[3] as f64 / c[2].max(1) as f64, curve));
    }

    /// Points one stream at what it should be of (None closes it), opening it no sooner than its
    /// retry after a failure, and handles KWin's news on it.
    fn sync(&mut self, win: bool, of: Option<Of>, ses: Option<&Arc<Session>>, out: Option<&Output>, cap: &capture::Capture, now: Instant) {
        if self.src(win).stream.is_some() && self.src(win).of != of {
            self.drop_src(win, cap);
        }
        let s = self.src(win);
        if let (Some(o), Some(ses)) = (of, ses)
            && s.stream.is_none()
            && s.retry_at.is_none_or(|t| now >= t)
        {
            s.stream = if win { ses.stream_window(&o.0, true) } else { out.and_then(|x| ses.stream_output(&x.wl, true)) };
            match s.stream {
                Some(_) => eprintln!("{}: streaming {}", s.name, o.0),
                None => s.retry_at = Some(now + s.retry),
            }
            s.of = Some(o);
        }
        if self.src(win).pump(cap) {
            self.drop_src(win, cap);
            let s = self.src(win);
            s.retry_at = Some(now + s.retry);
        }
    }

    /// Closes a stream, hiding the overlay first if it's the one up, before its buffers go.
    fn drop_src(&mut self, win: bool, cap: &capture::Capture) {
        if self.live && self.on_win == win {
            if self.visible {
                self.hide();
            }
            self.live = false;
        }
        self.src(win).close(cap);
    }

    /// Laser events on it go into the session. The mouse's come through kvm.rs.
    fn events(&mut self) {
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        self.moving = self.held != 0;
        while call!(ov, PollNextOverlayEvent, self.ov, &mut e, size_of::<vr::VREvent_t>() as u32) {
            let m = unsafe { e.data.mouse };
            self.laser(&e, at(m.x as f64, m.y as f64));
        }
    }

    /// A laser event on the taskbar's frame over the bar, at fractions right and up from the
    /// bar's bottom left (taskbar.rs). Handled as if it hit this overlay at that point of the panel.
    pub fn on_bar(&mut self, e: &vr::VREvent_t, fx: f64, fy: f64) {
        let at = self.panel.as_ref().map(|p| global(p.1, fx, fy));
        self.laser(e, at);
    }

    /// One laser event, at `at` in the session.
    fn laser(&mut self, e: &vr::VREvent_t, at: Option<(f64, f64)>) {
        self.moving = true;
        let (m, t) = (unsafe { e.data.mouse }, e.eventType);
        if t == sys::EVREventType_VREvent_MouseButtonUp {
            self.released.push(e.trackedDeviceIndex);
        }
        let b = match m.button {
            sys::EVRMouseButton_VRMouseButton_Right => PTR_FLAGS_BUTTON2,
            sys::EVRMouseButton_VRMouseButton_Middle => PTR_FLAGS_BUTTON3,
            _ => PTR_FLAGS_BUTTON1,
        };
        // always send the release for a press that went in, hidden or not, or it stays held
        if t == sys::EVREventType_VREvent_MouseButtonUp && self.held & b != 0 {
            self.held &= !b;
            windows::shell_mouse(b, at);
            return;
        }
        if !self.visible {
            return;
        }
        let mut k = KVM.lock().unwrap();
        if t == sys::EVREventType_VREvent_MouseButtonDown && vr::is_real_controller(e.trackedDeviceIndex) && k.awake {
            k.set_awake(false); // the last device pressed is primary (R-2)
        }
        if k.awake {
            return; // the mouse has the pointer, and a laser can't take it out from under it (main.rs laser)
        }
        match t {
            sys::EVREventType_VREvent_MouseMove => windows::shell_mouse(PTR_FLAGS_MOVE, at),
            sys::EVREventType_VREvent_MouseButtonDown => {
                k.type_to_shell(); // typing follows the click, for Kickoff's search
                self.held |= b;
                windows::shell_mouse(b | PTR_FLAGS_DOWN, at);
            }
            sys::EVREventType_VREvent_FocusLeave => {
                TIP.store(false, Relaxed);
                windows::leave();
            }
            sys::EVREventType_VREvent_ScrollDiscrete | sys::EVREventType_VREvent_ScrollSmooth => {
                let dy = unsafe { e.data.scroll.ydelta };
                if dy != 0.0 {
                    windows::wheel(false, dy as f64);
                }
            }
            _ => {}
        }
    }

    /// Hides it and lets go of any laser press where the pointer is, since its release may never come.
    fn hide(&mut self) {
        call!(ov, HideOverlay, self.ov);
        (self.visible, self.placed) = (false, None);
        *CROP.lock().unwrap() = None;
        KVM.lock().unwrap().plasma = None;
        self.release_held();
    }

    /// Lets go of every laser press on it in the session, where the pointer is, because its
    /// release came up on another overlay (the taskbar's frame round it, a panel, a card).
    /// ponytail: any device's release lets go every device's press (held is per button, not
    /// per device); two lasers on Plasma's bar at once is rare.
    pub fn release_held(&mut self) {
        for b in [PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3] {
            if self.held & b != 0 {
                windows::shell_mouse(b, None);
            }
        }
        self.held = 0;
    }

    /// Shuts both streams and the overlay down on exit.
    pub fn stop(&mut self, cap: &capture::Capture) {
        self.drop_src(false, cap);
        self.drop_src(true, cap);
        call!(ov, DestroyOverlay, self.ov);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WL0: Rect = [0, 740, 1920, 1080];
    const BAR: Rect = [514, 1770, 891, 50]; // Plasma's floating panel, centred at the bottom of WL-0

    #[test]
    fn the_crop_grows_with_open_popups_on_the_output_only() {
        assert_eq!(crop(BAR, &[], WL0), Some(BAR));
        let kickoff = [514, 1100, 650, 660]; // above its launcher at the bar's left end
        assert_eq!(crop(BAR, &[kickoff], WL0), Some([514, 1100, 891, 720]));
        let tray = [1300, 1300, 400, 460]; // past the bar's right end
        assert_eq!(crop(BAR, &[kickoff, tray], WL0), Some([514, 1100, 1186, 720]));
        // a menu hanging off the output gets cut at its edge, and one on WL-2 isn't taken in
        assert_eq!(crop(BAR, &[[-100, 1500, 300, 200]], WL0), Some([0, 1500, 1405, 320]));
        assert_eq!(crop(BAR, &[[3360, 740, 400, 300]], WL0), Some(BAR));
        assert_eq!(crop(BAR, &[], [1920, 0, 1440, 2560]), None, "the panel isn't on that output");
    }

    #[test]
    fn the_bar_stays_put_while_the_crop_grows() {
        let mpp = 0.001;
        assert_eq!(shift(BAR, BAR, mpp), (0.0, 0.0));
        // grown 670 px upward at the same width: the overlay's middle goes up half that, the bar's stays put
        let (dx, dy) = shift(BAR, [514, 1100, 891, 720], mpp);
        assert!(dx.abs() < 1e-12 && (dy - 0.335).abs() < 1e-12, "{dx} {dy}");
        // and wider to the right, so its middle goes right by half of that
        let (dx, _) = shift(BAR, [514, 1100, 1186, 720], mpp);
        assert!((dx - 0.1475).abs() < 1e-12, "{dx}");
        let b = bounds(BAR, WL0);
        assert!((b[0] - 514.0 / 1920.0).abs() < 1e-6 && (b[2] - 1405.0 / 1920.0).abs() < 1e-6);
        assert!((b[1] - 1030.0 / 1080.0).abs() < 1e-6 && (b[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn overlay_points_map_through_the_crop() {
        assert_eq!(global(BAR, 0.0, 1.0), (514.0, 1770.0), "top left");
        assert_eq!(global(BAR, 1.0, 0.0), (1404.0, 1819.0), "bottom right: its last pixel");
        assert_eq!(global(BAR, 0.5, 0.5), (514.0 + 445.0, 1770.0 + 24.5));
        assert_eq!(global(BAR, 2.0, -1.0), (1404.0, 1819.0), "clamped to it");
        // once grown, the same fraction lands in the popup above
        assert_eq!(global([514, 1100, 891, 720], 0.0, 1.0), (514.0, 1100.0));
    }

    #[test]
    fn points_under_the_panel_or_a_popup_are_covered() {
        let kickoff = [514, 1100, 650, 660];
        assert!(inside(BAR, 514.0, 1770.0) && inside(BAR, 1404.9, 1819.9));
        assert!(!inside(BAR, 1405.0, 1800.0) && !inside(BAR, 600.0, 1769.0), "its right and top edges are past it");
        assert!(inside(kickoff, 600.0, 1500.0) && !inside(kickoff, 1200.0, 1500.0));
    }
}
