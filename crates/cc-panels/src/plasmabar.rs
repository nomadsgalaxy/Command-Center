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
use crate::events::{Button, PointerEvent, PointerKind, SurfaceEvent};
use crate::geometry::{Mat, Placement, V3, norm};
use crate::overlay::{At, Flag, InputMode, Live, Overlays, Place, Surfaces, apply};
use crate::session::{Output, Session, StreamEvent};
use crate::{capture, session, taskbar, vr, windows};
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
/// What the overlay shows now and the panel in it (None: hidden), for input from any thread.
static CROP: Mutex<Option<(Rect, Rect)>> = Mutex::new(None);
/// The panel and every open plasmashell popup, shown here or not. They're above every window.
static ABOVE: Mutex<Vec<Rect>> = Mutex::new(Vec::new());

/// With ~/.cache/control-center/plasma-debug there when cc-panels starts, laser clicks on the bar
/// and every placement of the overlay go to the log, to find where a click lands.
fn debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::path::Path::new(&std::env::var("HOME").unwrap_or_default()).join(".cache/control-center/plasma-debug").exists())
}

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

/// A point SteamVR gives in the texture's coordinates (x right, y up, 0 to 1 over the whole
/// texture) as fractions right and up of the part `b` shows (texture bounds, v down).
fn in_bounds(b: [f32; 4], x: f64, y: f64) -> (f64, f64) {
    let [u0, v0, u1, v1] = b.map(f64::from);
    ((x - u0) / (u1 - u0).max(1e-9), (y - (1.0 - v1)) / (v1 - v0).max(1e-9))
}

/// The crop as bounds on the output's stream: u, v min, then max (u right, v down, 0 to 1).
fn bounds(crop: Rect, out: Rect) -> [f32; 4] {
    let (w, h) = (out[2].max(1) as f32, out[3].max(1) as f32);
    let (u, v) = ((crop[0] - out[0]) as f32 / w, (crop[1] - out[1]) as f32 / h);
    [u, v, u + crop[2] as f32 / w, v + crop[3] as f32 / h]
}

/// A point on the overlay (fractions right and up from its bottom left) mapped through the
/// crop into the session's global coordinates, clamped to its last pixel.
pub(crate) fn global(crop: Rect, fx: f64, fy: f64) -> (f64, f64) {
    let (w, h) = ((crop[2] - 1).max(0) as f64, (crop[3] - 1).max(0) as f64);
    (crop[0] as f64 + fx.clamp(0.0, 1.0) * w, crop[1] as f64 + (1.0 - fy.clamp(0.0, 1.0)) * h)
}

/// The session point under the overlay at fractions right and up from its bottom left, for
/// kvm.rs and the lasers. None while it's hidden.
pub fn at(fx: f64, fy: f64) -> Option<(f64, f64)> {
    CROP.lock().unwrap().map(|c| global(c.0, fx, fy))
}

/// How the overlay was last placed: its matrix, the device it's on (None: the room), the crop, its
/// width in mm, the texture bounds and the curve in mm.
type Placed = Option<(Mat, Option<crate::ids::Dev>, Rect, u32, [f32; 4], u32)>;

/// Plasma's overlay at `local` on device `dev` (None is the room), `width` wide and bent to
/// `curve`, showing the part of its texture in `b`. The bounds are only sent when they differ from
/// the last ones (`was`), because the crop moves far more often than it changes.
fn place_for(local: &Mat, dev: Option<crate::ids::Dev>, width: f64, curve: f64, b: [f32; 4], was: Option<[f32; 4]>) -> Place {
    let place = Place::new(At::on(dev, *local), width).curve(curve);
    if was != Some(b) { place.bounds(b) } else { place }
}

/// Where a ray from `from` through `to` passes through the overlay (`pl`, as kvm.rs places
/// it, showing `crop`), as fractions right and up of `panel`. None if that's off the panel.
/// The mouse's ray lands on the overlay itself. A laser lands on the taskbar's frame 2 mm
/// behind it (taskbar.rs FRONT), so the frame carries its hit on along the laser to here, and
/// both end up on the bar you see the pointer on.
pub(crate) fn panel_at(pl: &Placement, crop: Rect, panel: Rect, from: &V3, to: &V3) -> Option<(f64, f64)> {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let n = norm(&d);
    let (_, u, v) = pl.hit(from, &d.map(|x| x / n)).filter(|_| n > 1e-9)?;
    // global's mapping without its clamp, then back through it for the panel
    let x = crop[0] as f64 + (u / pl.width + 0.5) * (crop[2] - 1).max(0) as f64;
    let y = crop[1] as f64 + (0.5 - v / pl.height) * (crop[3] - 1).max(0) as f64;
    let (fx, fy) = ((x - panel[0] as f64) / (panel[2] - 1).max(1) as f64, 1.0 - (y - panel[1] as f64) / (panel[3] - 1).max(1) as f64);
    ((0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy)).then_some((fx, fy))
}

/// Where a ray from `o` along unit `d` crosses the overlay as kvm.rs places it (`pl`, showing
/// `crop`), in the session, clamped to its edges. It's the mouse's mapping (kvm.rs), and a laser
/// on this overlay goes by it too, so both land on what's drawn there now, whatever crop or
/// bounds SteamVR had when it made the event.
pub(crate) fn ray_at(pl: &Placement, crop: Rect, o: &V3, d: &V3) -> Option<(f64, f64)> {
    let (_, u, v) = pl.hit(o, d)?;
    let (cu, cv) = pl.clamped(u, v);
    let (fx, fy) = pl.fraction(cu, cv);
    Some(global(crop, fx, fy))
}

/// Where laser event `t` lands in the session, and whether that was the fallback; None drops the
/// event. With a pose (`ray`, origin and unit direction) it's the ray through the overlay as placed
/// (`pl`, showing `crop`). Only a device with no pose falls back to SteamVR's point (x, y), put
/// through the bounds `b` it was made with. A ray that misses (it points away) is the careful case:
///   - a move stays at that device's `last` point,
///   - a press is dropped, so it never clicks whatever was last under the laser,
///   - a release goes where the pointer is (None), and laser() always sends it for a held press.
fn laser_point(
    kind: Option<PointerKind>,
    pl: Option<Placement>,
    crop: Option<Rect>,
    ray: Option<(V3, V3)>,
    last: Option<(f64, f64)>,
    (b, x, y): ([f32; 4], f64, f64),
) -> Option<(Option<(f64, f64)>, bool)> {
    let Some((o, d)) = ray else {
        let (fx, fy) = in_bounds(b, x, y);
        return Some((crop.map(|c| global(c, fx, fy)), true));
    };
    match pl.zip(crop).and_then(|(pl, c)| ray_at(&pl, c, &o, &d)) {
        Some(p) => Some((Some(p), false)),
        None if matches!(kind, Some(PointerKind::Down(_))) => None,
        None if matches!(kind, Some(PointerKind::Up(_))) => Some((None, false)),
        None => Some((last, false)),
    }
}

/// Each laser's last point in the session, for a ray that misses. They're only good for the crop
/// they were made in, so a different crop (a menu opening or closing) forgets them all.
#[derive(Default)]
struct Lasts {
    crop: Option<Rect>,
    at: Vec<(u32, (f64, f64))>,
}

impl Lasts {
    fn get(&mut self, dev: u32, crop: Option<Rect>) -> Option<(f64, f64)> {
        if crop != self.crop {
            (self.crop, self.at) = (crop, Vec::new());
        }
        self.at.iter().find(|l| l.0 == dev).map(|l| l.1)
    }

    fn set(&mut self, dev: u32, p: (f64, f64)) {
        self.at.retain(|l| l.0 != dev);
        self.at.push((dev, p));
    }
}

/// `panel_at` for a laser from `from` that hit the taskbar's frame at `to`: Some(None) if it
/// passes Plasma's bar by, None while the bar isn't showing.
pub fn laser_on_panel(from: &V3, to: &V3) -> Option<Option<(f64, f64)>> {
    let pl = crate::kvm::lock().plasma?;
    let (crop, panel) = (*CROP.lock().unwrap())?;
    Some(panel_at(&pl, crop, panel, from, to))
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
    panel: Option<(String, Rect)>,     // Plasma's panel, its dock window: uuid, geometry
    popups: Vec<(String, Rect, bool)>, // plasmashell's other open windows: popups, menus, tooltips (false)
    out: Src,                          // the panel's output
    win: Src,                          // the panel's own window
    on_win: bool,                      // which of them is on the overlay
    live: bool,                        // a frame is showing
    visible: bool,
    clickable: bool,            // it takes lasers itself (a popup is open), otherwise the taskbar's frame forwards them
    hidden_at: Option<Instant>, // when it stopped being wanted
    placed: Placed,
    held: u32,                          // laser buttons down on it, in RDP's flags
    pub moving: bool,                   // a laser on it this frame (events or a held press), which sets the main loop's pace
    pub released: Vec<crate::ids::Dev>, // devices whose button came up on it, ending a hold on the taskbar's frame
    last: Lasts,                        // each laser's last point in the session, for a ray that misses
}

impl Plasma {
    pub fn new() -> Plasma {
        let h = crate::surface::create(crate::surface::Kind::Plasma, None, "controlcenter.plasma", "Command Center Plasma taskbar").unwrap_or_else(|e| {
            eprintln!("plasma: no overlay: {e}"); // SteamVR's 128 are shared, and we're out
            crate::vr::Handle::NONE
        });
        // no input method until a popup is open (`tick` sets it)
        crate::overlay::Live.flag(h, Flag::Interactive, true);
        crate::overlay::Live.flag(h, Flag::DiscreteScroll, true);
        crate::overlay::Live.flag(h, Flag::IgnoreAlpha, true);
        crate::overlay::Live.order(h, taskbar::SORT + 1); // in front of the taskbar's frame, in case depth isn't used
        crate::overlay::Live.mouse_scale(h, [1.0, 1.0]); // events as fractions, since the crop changes under them
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
            last: Lasts::default(),
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
        self.panel.as_ref().filter(|_| self.ov != crate::vr::Handle::NONE).map(|p| (p.1[2], p.1[3]))
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
    pub fn tick(&mut self, ses: Option<&Arc<Session>>, cap: &capture::Capture, at: Option<(Mat, Mat, Option<crate::ids::Dev>)>, lift: f64, mpp: f64, curve: f64) {
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
            crate::overlay::Live.input(self.ov, if clickable { InputMode::Pointer } else { InputMode::None });
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
                crate::overlay::Live.show(self.ov);
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
            if debug() {
                eprintln!("plasma-debug: placed crop {c:?} panel {p:?} width {width:.3} m shift {dx:.3},{dy:.3} bounds {b:?} clickable {} on_win {}", self.clickable, self.on_win);
            }
            apply(&mut Live, self.ov, &place_for(&local, dev, width, curve, b, self.placed.map(|q| q.4)));
            self.placed = placed;
            if crate::trace::on() {
                let aspect = c[3] as f64 / c[2].max(1) as f64;
                let v = serde_json::json!({"crop": c, "panel": p, "bounds": b, "world": world, "width": width, "aspect": aspect, "curve": curve, "clickable": self.clickable});
                crate::trace::state("plasma", v);
            }
        }
        *CROP.lock().unwrap() = Some((c, p));
        crate::kvm::lock().plasma = Some(Placement::from_matrix(&world, width, c[3] as f64 / c[2].max(1) as f64, curve));
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
        let mut e: vr::VREvent_t = Default::default();
        self.moving = self.held != 0;
        // A laser goes where its ray crosses the overlay as placed now (ray_at), the way the
        // mouse does. SteamVR's own point is only the fallback, for a device with no pose: it's
        // in the whole texture's coordinates (c937884), so with a popup open, and the texture the
        // whole output, it's turned into fractions of the crop. That point depends on which
        // texture and bounds SteamVR had when it made the event, and right after a menu opens
        // that isn't always what's drawn. In the headset on Oct 29, SteamVR's point (through
        // in_bounds) was right across the open menu but pulled toward its middle up and down, one
        // or two items low at the top. The ray fixes that if SteamVR's hit quad isn't the picture
        // as drawn; it doesn't if the crop to the session's coordinates (global) is what's off.
        // Copies, taken once: no lock is held while the loop asks OpenVR for poses.
        let b = self.placed.map_or([0.0, 0.0, 1.0, 1.0], |q| q.4);
        let (pl, crop) = (crate::kvm::lock().plasma, CROP.lock().unwrap().map(|c| c.0));
        let mut lasers: Vec<(u32, Option<crate::geometry::Mat>)> = Vec::new(); // each device's laser, once a batch
        crate::trace::tick("plasma");
        while crate::trace::poll("plasma", self.ov, &mut e) {
            let dev = e.trackedDeviceIndex;
            // a pointer's kind and its point; anything else still goes through below as it always
            // has (it counts as a laser this frame and can move its device's last point), with what
            // its mouse data holds. Phase 1 drops that. So this is the one overlay loop that still
            // reads SteamVR's raw event, not Pointing: it moves behind it with that fix (events.rs).
            let (kind, x, y, from) = match vr::surface_event(&e) {
                SurfaceEvent::Pointer(pe) => (Some(pe.kind), pe.x, pe.y, pe.from),
                _ => {
                    let m = unsafe { e.data.mouse };
                    (None, m.x, m.y, vr::dev(dev))
                }
            };
            let laser = match lasers.iter().find(|r| r.0 == dev) {
                Some(r) => r.1,
                None => {
                    let l = vr::laser_pose(dev);
                    lasers.push((dev, l));
                    l
                }
            };
            // while a trace runs, the laser this event was mapped by, after the event's own line
            // (trace::poll wrote it), so a replay or a check of SteamVR's point has the same ray
            if crate::trace::on() && matches!(kind, Some(PointerKind::Move | PointerKind::Down(_) | PointerKind::Up(_))) {
                crate::trace::pose(dev, laser.as_ref());
            }
            let ray: Option<(V3, V3)> = laser.map(|l| ([l[0][3], l[1][3], l[2][3]].map(f64::from), [-l[0][2], -l[1][2], -l[2][2]].map(f64::from)));
            let last = self.last.get(dev, crop);
            let Some((spot, fell_back)) = laser_point(kind, pl, crop, ray, last, (b, x as f64, y as f64)) else {
                if debug() {
                    eprintln!("plasma-debug: a press with the laser's ray off the overlay, dropped");
                }
                continue;
            };
            if let Some(p) = spot {
                self.last.set(dev, p);
            }
            if debug() {
                // how often the fallback fires, for the log: per run, counted on every event
                static EVENTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                static FALLBACKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let n = EVENTS.fetch_add(1, Relaxed) + 1;
                let f = FALLBACKS.fetch_add(u32::from(fell_back), Relaxed) + u32::from(fell_back);
                if e.eventType != sys::EVREventType_VREvent_MouseMove {
                    let (fx, fy) = in_bounds(b, x as f64, y as f64);
                    eprintln!(
                        "plasma-debug: overlay event {} at {:.3},{:.3} (crop {fx:.3},{fy:.3}) -> {spot:?}{}, crop {crop:?}; fallback {f} of {n} events this run (poses and placement both standing)",
                        e.eventType,
                        x,
                        y,
                        if fell_back { " (fallback, no pose)" } else { " by its ray" }
                    );
                }
            }
            self.laser(kind, from, spot);
        }
    }

    /// A laser event on the taskbar's frame over the bar, at fractions right and up from the
    /// bar's bottom left (taskbar.rs). Handled as if it hit this overlay at that point of the panel.
    pub fn on_bar(&mut self, e: &PointerEvent, fx: f64, fy: f64) {
        let at = self.panel.as_ref().map(|p| global(p.1, fx, fy));
        if debug() && e.kind != PointerKind::Move {
            eprintln!("plasma-debug: frame event {} at panel {fx:.3},{fy:.3} -> {at:?} clickable {}", vr::openvr_type(e.kind), self.clickable);
        }
        self.laser(Some(e.kind), e.from, at);
    }

    /// One laser event, at `at` in the session.
    fn laser(&mut self, kind: Option<PointerKind>, from: crate::ids::Dev, at: Option<(f64, f64)>) {
        self.moving = true;
        if matches!(kind, Some(PointerKind::Up(_))) {
            self.released.push(from);
        }
        let b = match kind {
            Some(PointerKind::Down(Button::Right) | PointerKind::Up(Button::Right)) => PTR_FLAGS_BUTTON2,
            Some(PointerKind::Down(Button::Middle) | PointerKind::Up(Button::Middle)) => PTR_FLAGS_BUTTON3,
            _ => PTR_FLAGS_BUTTON1,
        };
        // always send the release for a press that went in, hidden or not, or it stays held
        if matches!(kind, Some(PointerKind::Up(_))) && self.held & b != 0 {
            self.held &= !b;
            windows::shell_mouse(b, at);
            return;
        }
        if !self.visible {
            return;
        }
        let mut k = crate::kvm::lock();
        if matches!(kind, Some(PointerKind::Down(_))) && vr::is_real_controller(vr::index(from)) && k.awake {
            k.set_awake(false); // the last device pressed is primary (R-2)
        }
        if k.awake {
            return; // the mouse has the pointer, and a laser can't take it out from under it (main.rs laser)
        }
        match kind {
            Some(PointerKind::Move) => windows::shell_mouse(PTR_FLAGS_MOVE, at),
            Some(PointerKind::Down(_)) => {
                k.type_to_shell(); // typing follows the click, for Kickoff's search
                self.held |= b;
                windows::shell_mouse(b | PTR_FLAGS_DOWN, at);
            }
            Some(PointerKind::Leave) => {
                TIP.store(false, Relaxed);
                windows::leave();
            }
            Some(PointerKind::Scroll { dy, .. }) if dy != 0.0 => windows::wheel(false, dy as f64),
            _ => {}
        }
    }

    /// Hides it and lets go of any laser press where the pointer is, since its release may never come.
    fn hide(&mut self) {
        crate::overlay::Live.hide(self.ov);
        (self.visible, self.placed) = (false, None);
        self.last = Lasts::default(); // a crop from before it hid means nothing now
        *CROP.lock().unwrap() = None;
        crate::kvm::lock().plasma = None;
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
        crate::surface::destroy(self.ov);
    }
}

/// Clears the crop a replay left (trace::fake_vr).
#[cfg(test)]
pub fn reset_for_test() {
    *CROP.lock().unwrap() = None;
}

/// Replays a trace (trace.rs) through the real events() and the fake SteamVR (vr::fake): each
/// placement line sets the overlay up as tick() left it, and each tick's events and poses go
/// through events() in one pass, the way they were polled (docs/vr-harness.md).
#[cfg(test)]
impl Plasma {
    fn replay(&mut self, trace: &str) {
        let lines: Vec<Value> = trace.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let mut i = 0;
        while i < lines.len() {
            let l = &lines[i];
            i += 1;
            if let Some(v) = l.get("plasma") {
                self.replay_placed(v);
            } else if let Some((dev, m)) = crate::trace::pose_from(l) {
                let mut s = vr::fake::SCRIPT.lock().unwrap();
                let s = s.get_or_insert_with(Default::default);
                s.poses.retain(|p| p.0 != dev);
                s.poses.extend(m.map(|m| (dev, m)));
            } else if l["tick"] == "plasma" {
                // this pass's events, up to the next pass or placement
                while i < lines.len() && lines[i].get("tick").is_none() && lines[i].get("plasma").is_none() {
                    if lines[i]["ev"] == "plasma"
                        && let Some(e) = crate::trace::event_from(&lines[i])
                    {
                        vr::fake::SCRIPT.lock().unwrap().get_or_insert_with(Default::default).events.push_back(e);
                    }
                    i += 1;
                }
                self.events();
            }
        }
    }

    /// The overlay as a placement line says tick() placed it.
    pub(crate) fn replay_placed(&mut self, v: &Value) {
        let rect = |k: &str| -> Rect { serde_json::from_value(v[k].clone()).unwrap() };
        let (c, p) = (rect("crop"), rect("panel"));
        self.panel = Some(("replay".into(), p)); // what on_bar maps the frame's points through
        let b: [f32; 4] = serde_json::from_value(v["bounds"].clone()).unwrap();
        let world: Mat = serde_json::from_value(v["world"].clone()).unwrap();
        let (width, aspect, curve) = (v["width"].as_f64().unwrap(), v["aspect"].as_f64().unwrap(), v["curve"].as_f64().unwrap());
        (self.visible, self.clickable, self.live) = (true, v["clickable"].as_bool().unwrap_or(true), true);
        self.placed = Some((world, None, c, (width * 1000.0).round() as u32, b, (curve * 1000.0).round() as u32));
        *CROP.lock().unwrap() = Some((c, p));
        let mut k = crate::kvm::lock();
        k.plasma = Some(Placement::from_matrix(&world, width, aspect, curve));
        k.awake = false; // the lasers have the pointer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_trace_replays_through_the_real_event_loop() {
        let _vr = crate::trace::fake_vr();
        let mut p = Plasma::new();
        // Kickoff open over the bar on WL-0, as plasmabar's tick() would record it
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        let b = bounds(grown, WL0);
        let world = crate::geometry::panel_matrix(&crate::geometry::Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let placed = serde_json::json!({"crop": grown, "panel": BAR, "bounds": b, "world": world, "width": 0.6, "aspect": grown[3] as f64 / grown[2] as f64, "curve": 0.0, "clickable": true});
        // a laser on an item in Kickoff's list: SteamVR's point is of the whole texture, y up
        let item = (600.0, 1200.0);
        let (x, y) = ((item.0 - WL0[0] as f64) / 1920.0, 1.0 - (item.1 - WL0[1] as f64) / 1080.0);
        let ev = |t: u32| {
            let mut e: vr::VREvent_t = Default::default();
            (e.eventType, e.trackedDeviceIndex) = (t, 3);
            e.data.mouse = sys::VREvent_Mouse_t { x: x as f32, y: y as f32, button: sys::EVRMouseButton_VRMouseButton_Left, cursorIndex: 0 };
            crate::trace::event_line("plasma", &e).unwrap().to_string()
        };
        let trace = [
            serde_json::json!({ "plasma": placed }).to_string(),
            serde_json::json!({"tick": "plasma"}).to_string(),
            ev(sys::EVREventType_VREvent_MouseMove),
            ev(sys::EVREventType_VREvent_MouseButtonDown),
            serde_json::json!({"tick": "plasma"}).to_string(),
            ev(sys::EVREventType_VREvent_MouseButtonUp),
        ]
        .join("\n");
        p.replay(&format!("{trace}\n{{\"tick\":\"plasma\",\"t\":1.2")); // the last line cut off mid-write: skipped
        let sent = std::mem::take(&mut *windows::SHELL_SENT.lock().unwrap());
        let flags: Vec<u32> = sent.iter().map(|s| s.0).collect();
        assert_eq!(flags, [PTR_FLAGS_MOVE, PTR_FLAGS_BUTTON1 | PTR_FLAGS_DOWN, PTR_FLAGS_BUTTON1], "a move, the press, its release");
        for (_, at) in &sent {
            let at = at.unwrap();
            assert!((at.0 - item.0).abs() < 1.5 && (at.1 - item.1).abs() < 1.5, "{at:?} is the item at {item:?}");
        }
        assert_eq!(p.held, 0, "nothing left held");
    }

    /// The bar placed with Kickoff open, and a maker of its laser events on one of Kickoff's items,
    /// as trace lines (the same set-up as the replay test above).
    fn kickoff() -> (String, impl Fn(u32) -> String) {
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        let world = crate::geometry::panel_matrix(&crate::geometry::Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let placed =
            serde_json::json!({"crop": grown, "panel": BAR, "bounds": bounds(grown, WL0), "world": world, "width": 0.6, "aspect": grown[3] as f64 / grown[2] as f64, "curve": 0.0, "clickable": true});
        let (x, y) = ((600.0 - WL0[0] as f64) / 1920.0, 1.0 - (1200.0 - WL0[1] as f64) / 1080.0);
        let ev = move |t: u32| {
            let mut e: vr::VREvent_t = Default::default();
            (e.eventType, e.trackedDeviceIndex) = (t, 3);
            e.data.mouse = sys::VREvent_Mouse_t { x: x as f32, y: y as f32, button: sys::EVRMouseButton_VRMouseButton_Left, cursorIndex: 0 };
            crate::trace::event_line("plasma", &e).unwrap().to_string()
        };
        (serde_json::json!({ "plasma": placed }).to_string(), ev)
    }

    /// One pass of the event loop with these events.
    fn pass(p: &mut Plasma, events: &[String]) {
        p.replay(&format!("{{\"tick\":\"plasma\"}}\n{}", events.join("\n")));
    }

    /// What went to the session, what SteamVR was told and what the bar holds, since last time.
    fn observe(log: &mut crate::golden::Log, p: &Plasma) {
        use crate::golden::num;
        for (f, at) in std::mem::take(&mut *windows::SHELL_SENT.lock().unwrap()) {
            log.line(format!("session pointer {f:#x} at {}", at.map_or("where it is".into(), |(x, y)| format!("{} {}", num(x), num(y)))));
        }
        for o in std::mem::take(&mut *windows::SHELL_OTHER.lock().unwrap()) {
            log.line(format!("session {o}"));
        }
        log.vr_calls();
        log.line(format!("held {:#x}, visible {}", p.held, p.visible));
    }

    /// Golden (golden.rs): a laser presses on Kickoff, then leaves the bar with the button still
    /// down, comes back, and lets go. What happens today is what's pinned, which isn't the same as
    /// right: a FLAG line marks what may be a bug, for its own branch.
    #[test]
    fn golden_focus_lost_with_a_button_held() {
        let _vr = crate::trace::fake_vr();
        let mut log = crate::golden::Log::default();
        let mut p = Plasma::new();
        let (placed, ev) = kickoff();
        log.step("the bar, placed with Kickoff open");
        p.replay(&placed);
        observe(&mut log, &p);
        log.step("a laser moves onto an item and presses");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseMove), ev(sys::EVREventType_VREvent_MouseButtonDown)]);
        observe(&mut log, &p);
        log.step("it leaves the bar with the button still down: FocusLeave, no Up");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_FocusLeave)]);
        observe(&mut log, &p);
        if p.held != 0 {
            // recorded, not blessed: if SteamVR never sends this Up, the computer keeps the button
            // down. Whether it does is what the press, look away, release trace will show.
            log.line("FLAG: the session still holds the button after FocusLeave (docs/openxr-plan.md, golden test 4)");
        }
        log.step("a pass with nothing");
        pass(&mut p, &[]);
        observe(&mut log, &p);
        log.step("it comes back and moves");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_FocusEnter), ev(sys::EVREventType_VREvent_MouseMove)]);
        observe(&mut log, &p);
        log.step("the button comes up");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseButtonUp)]);
        observe(&mut log, &p);
        crate::golden::check("plasmabar-focus-lost-with-a-button-held", &log.0);
    }

    /// Golden: a laser presses on Kickoff, then the bar goes away (hide, as when its window closes)
    /// with the press still in, and the Up arrives after, for an overlay that's gone.
    #[test]
    fn golden_surface_gone_with_a_press_outstanding() {
        let _vr = crate::trace::fake_vr();
        let mut log = crate::golden::Log::default();
        let mut p = Plasma::new();
        let (placed, ev) = kickoff();
        log.step("the bar, placed with Kickoff open");
        p.replay(&placed);
        observe(&mut log, &p);
        log.step("a laser moves onto an item and presses");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseMove), ev(sys::EVREventType_VREvent_MouseButtonDown)]);
        observe(&mut log, &p);
        log.step("the bar goes away with the press still in");
        p.hide();
        observe(&mut log, &p);
        log.step("the Up arrives after it's gone");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseButtonUp)]);
        observe(&mut log, &p);
        log.step("and a stray move");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseMove)]);
        observe(&mut log, &p);
        crate::golden::check("plasmabar-surface-gone-with-a-press-outstanding", &log.0);
    }

    /// Golden: the mouse has the pointer (it was used last), so a laser's moves on the bar are
    /// dropped. A press from our virtual pointer (cc_pointer, not a real controller) doesn't
    /// take it over; a real controller's press does, and goes through.
    #[test]
    fn golden_the_mouse_then_a_controller() {
        const POINTER: u32 = 9; // cc_pointer's virtual controller
        let _vr = crate::trace::fake_vr();
        crate::vr::fake::SCRIPT.lock().unwrap().get_or_insert_with(Default::default).types = vec![(POINTER, "cc_pointer".into())];
        let mut log = crate::golden::Log::default();
        let mut p = Plasma::new();
        let (placed, ev) = kickoff(); // its events come from device 3, a real controller
        let from = |dev: u32, t: u32| {
            let mut v: serde_json::Value = serde_json::from_str(&ev(t)).unwrap();
            v["dev"] = dev.into();
            v.to_string()
        };
        let awake = |log: &mut crate::golden::Log| log.line(format!("mouse has the pointer: {}", crate::kvm::lock().awake));
        log.step("the bar placed, then the mouse used");
        p.replay(&placed);
        crate::kvm::lock().awake = true; // as the mouse waking leaves it (set_awake also recenters on the panels, which a test has none of)
        observe(&mut log, &p);
        awake(&mut log);
        log.step("a laser moves on the bar");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseMove)]);
        observe(&mut log, &p);
        awake(&mut log);
        log.step("our virtual pointer presses");
        pass(&mut p, &[from(POINTER, sys::EVREventType_VREvent_MouseButtonDown), from(POINTER, sys::EVREventType_VREvent_MouseButtonUp)]);
        observe(&mut log, &p);
        awake(&mut log);
        log.step("a real controller presses");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseButtonDown)]);
        observe(&mut log, &p);
        awake(&mut log);
        log.step("and lets go");
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseButtonUp)]);
        observe(&mut log, &p);
        awake(&mut log);
        crate::kvm::lock().awake = false; // as the other replays leave it
        crate::golden::check("plasmabar-the-mouse-then-a-controller", &log.0);
    }

    /// While a trace runs, each pointer event on the bar is followed by its laser's pose, so
    /// SteamVR's point can be checked against the ray afterwards. A focus event gets none.
    #[test]
    fn a_traced_pointer_event_is_followed_by_its_lasers_pose() {
        let _vr = crate::trace::fake_vr();
        let m = crate::geometry::panel_matrix(&crate::geometry::Pose { centre: [0.1, 1.2, -0.3], ..Default::default() });
        crate::vr::fake::SCRIPT.lock().unwrap().get_or_insert_with(Default::default).poses = vec![(3, m)];
        let path = std::env::temp_dir().join(format!("cc-trace-plasma-pose-{}.jsonl", std::process::id()));
        crate::trace::record_to(&path);
        let mut p = Plasma::new();
        let (placed, ev) = kickoff();
        p.replay(&placed);
        pass(&mut p, &[ev(sys::EVREventType_VREvent_MouseMove), ev(sys::EVREventType_VREvent_FocusLeave), ev(sys::EVREventType_VREvent_MouseButtonDown)]);
        let text = crate::trace::stop_recording();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let at = |t: u32| lines.iter().position(|v| v["ev"] == "plasma" && v["type"] == t).unwrap_or_else(|| panic!("no event {t}: {text}"));
        for t in [sys::EVREventType_VREvent_MouseMove, sys::EVREventType_VREvent_MouseButtonDown] {
            let next = &lines[at(t) + 1];
            assert_eq!(next["pose"], 3, "event {t}: its laser's pose next: {text}");
            assert_eq!(crate::trace::pose_from(next), Some((3, Some(m))), "the pose SteamVR had");
        }
        assert!(lines.get(at(sys::EVREventType_VREvent_FocusLeave) + 1).is_none_or(|v| v.get("pose").is_none()), "a focus event has no pose: {text}");
    }

    #[test]
    fn steamvr_points_map_into_the_crop() {
        // the bar alone: the texture is the bar, so nothing changes
        assert_eq!(in_bounds([0.0, 0.0, 1.0, 1.0], 0.25, 0.75), (0.25, 0.75));
        // a popup open: the crop is [658, 841, 919, 599] of a 2560x1440 output
        let b = bounds([658, 841, 919, 599], [0, 0, 2560, 1440]);
        let (fx, fy) = in_bounds(b, 658.0 / 2560.0, 0.0); // the crop's bottom-left corner, in texture terms
        assert!(fx.abs() < 1e-4 && fy.abs() < 1e-4, "{fx} {fy}");
        let (fx, fy) = in_bounds(b, 1577.0 / 2560.0, 1.0 - 841.0 / 1440.0); // its top-right
        assert!((fx - 1.0).abs() < 1e-4 && (fy - 1.0).abs() < 1e-4, "{fx} {fy}");
    }

    const WL0: Rect = [0, 740, 1920, 1080];
    const BAR: Rect = [514, 1770, 891, 50]; // Plasma's floating panel, centred at the bottom of WL-0

    /// The overlay as tick() places it for `crop`, in a frame at `m`: shifted so the bar stays put.
    fn placed_for(m: &Mat, crop: Rect, mpp: f64, curve: f64) -> Placement {
        let (dx, dy) = shift(BAR, crop, mpp);
        let world = taskbar::in_well(m, curve, dx, 0.1 + dy);
        Placement::from_matrix(&world, crop[2] as f64 * mpp, crop[3] as f64 / crop[2] as f64, curve)
    }

    /// A laser 0.6 m back from where session point `p` is drawn on `pl` (showing `crop`), aimed
    /// at it a little from below, the way a hand holds it: its origin and direction.
    fn laser_at(pl: &Placement, crop: Rect, p: (f64, f64)) -> (V3, V3) {
        let fx = (p.0 - crop[0] as f64) / (crop[2] - 1) as f64;
        let fy = 1.0 - (p.1 - crop[1] as f64) / (crop[3] - 1) as f64;
        let t = pl.on_surface((fx - 0.5) * pl.width, (fy - 0.5) * pl.height, 0.0);
        let to = [t[0][3], t[1][3], t[2][3]].map(f64::from);
        let n = [t[0][2], t[1][2], t[2][2]].map(f64::from); // out of the surface
        let from = [to[0] + n[0] * 0.6, to[1] + n[1] * 0.6 - 0.2, to[2] + n[2] * 0.6];
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let l = norm(&d);
        (from, d.map(|x| x / l))
    }

    #[test]
    fn a_laser_lands_on_the_start_menu_where_its_drawn_as_it_opens() {
        use crate::geometry::{Pose, panel_matrix};
        let m = panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let (mpp, kickoff) = (0.0006, [514, 1100, 650, 660]);
        let grown = crop(BAR, &[kickoff], WL0).unwrap();
        let item = (600.0, 1200.0); // an app in Kickoff's list, near its top left
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1.5 && (a.1 - b.1).abs() < 1.5;
        for curve in [0.0, 2.0] {
            // 1. the bar alone, from the window's stream: the launcher on the bar
            let bar = placed_for(&m, BAR, mpp, curve);
            let (o, d) = laser_at(&bar, BAR, (540.0, 1795.0));
            assert!(close(ray_at(&bar, BAR, &o, &d).unwrap(), (540.0, 1795.0)), "curve {curve}");
            // 2. Kickoff's open, but the output's stream has no frame yet: the overlay still
            //    shows only the bar, and a laser on it still lands on the bar
            let (o, d) = laser_at(&bar, BAR, (900.0, 1780.0));
            assert!(close(ray_at(&bar, BAR, &o, &d).unwrap(), (900.0, 1780.0)));
            // 3. the output's frame is up and the crop has grown: the menu's item, where it's drawn
            let pl = placed_for(&m, grown, mpp, curve);
            let (o, d) = laser_at(&pl, grown, item);
            assert!(close(ray_at(&pl, grown, &o, &d).unwrap(), item), "curve {curve}: {:?}", ray_at(&pl, grown, &o, &d));
            // and the bar is where it was, so the launcher's still under the same laser
            let (o, d) = laser_at(&bar, BAR, (540.0, 1795.0));
            assert!(close(ray_at(&pl, grown, &o, &d).unwrap(), (540.0, 1795.0)), "curve {curve}");
        }
        // SteamVR's own point for that item, in the whole texture's coordinates (y up)
        let b = bounds(grown, WL0);
        let tex = ((item.0 - WL0[0] as f64) / 1920.0, 1.0 - (item.1 - WL0[1] as f64) / 1080.0);
        let (fx, fy) = in_bounds(b, tex.0, tex.1);
        assert!(close(global(grown, fx, fy), item), "mapped with the bounds on screen, it's right");
        // with the bounds from the frame before the switch (the bar's window, [0,0,1,1]), the
        // first events after the menu opens land off it
        let (fx, fy) = in_bounds([0.0, 0.0, 1.0, 1.0], tex.0, tex.1);
        let stale = global(grown, fx, fy);
        assert!((stale.0 - item.0).abs() > 50.0 || (stale.1 - item.1).abs() > 50.0, "{stale:?}");
        // and if SteamVR's point were already of the part shown, in_bounds maps it twice
        let shown = ((item.0 - grown[0] as f64) / (grown[2] - 1) as f64, 1.0 - (item.1 - grown[1] as f64) / (grown[3] - 1) as f64);
        let (fx, fy) = in_bounds(b, shown.0, shown.1);
        let twice = global(grown, fx, fy);
        assert!((twice.0 - item.0).abs() > 50.0 || (twice.1 - item.1).abs() > 50.0, "{twice:?}");
    }

    #[test]
    fn with_no_pose_a_laser_still_maps_by_steamvrs_point() {
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        let b = bounds(grown, WL0);
        let item = (600.0, 1200.0);
        let tex = ((item.0 - WL0[0] as f64) / 1920.0, 1.0 - (item.1 - WL0[1] as f64) / 1080.0);
        let (at, fell_back) = laser_point(MOVE, None, Some(grown), None, None, (b, tex.0, tex.1)).unwrap();
        let at = at.unwrap();
        assert!(fell_back && (at.0 - item.0).abs() < 1.5 && (at.1 - item.1).abs() < 1.5, "{at:?}");
        assert_eq!(laser_point(MOVE, None, None, None, None, (b, tex.0, tex.1)), Some((None, true)), "hidden: nowhere");
        assert_eq!(laser_point(UP, None, Some(grown), None, None, (b, tex.0, tex.1)).map(|r| r.1), Some(true), "tracking lost: a release still goes in");
        // with a ray, the ray wins
        use crate::geometry::{Pose, panel_matrix};
        let m = panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let pl = placed_for(&m, grown, 0.0006, 0.0);
        let (o, d) = laser_at(&pl, grown, (900.0, 1500.0));
        let (at, fell_back) = laser_point(MOVE, Some(pl), Some(grown), Some((o, d)), None, (b, tex.0, tex.1)).unwrap();
        assert!(!fell_back && (at.unwrap().0 - 900.0).abs() < 1.5);
        // a pose whose ray points away from the overlay: that laser's last point, never the fallback
        let away = (o, d.map(|x| -x));
        assert_eq!(laser_point(MOVE, Some(pl), Some(grown), Some(away), Some((901.0, 1501.0)), (b, tex.0, tex.1)), Some((Some((901.0, 1501.0)), false)));
        assert_eq!(laser_point(MOVE, Some(pl), Some(grown), Some(away), None, (b, tex.0, tex.1)), Some((None, false)), "no last point: nowhere, not SteamVR's");
    }

    const MOVE: Option<PointerKind> = Some(PointerKind::Move);
    const DOWN: Option<PointerKind> = Some(PointerKind::Down(Button::Left));
    const UP: Option<PointerKind> = Some(PointerKind::Up(Button::Left));

    #[test]
    fn a_press_with_the_ray_off_the_overlay_clicks_nothing_and_a_release_still_goes() {
        use crate::geometry::{Pose, panel_matrix};
        let m = panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        let pl = placed_for(&m, grown, 0.0006, 0.0);
        let item = (600.0, 1200.0);
        let (o, d) = laser_at(&pl, grown, item);
        let mut lasts = Lasts::default();
        // on the item: it's the laser's last point
        let (at, _) = laser_point(MOVE, Some(pl), Some(grown), Some((o, d)), lasts.get(3, Some(grown)), ([0.0; 4], 0.0, 0.0)).unwrap();
        lasts.set(3, at.unwrap());
        // the ray goes off the overlay, then a press and a release
        let away = Some((o, d.map(|x| -x)));
        let last = lasts.get(3, Some(grown));
        assert_eq!(laser_point(MOVE, Some(pl), Some(grown), away, last, ([0.0; 4], 0.0, 0.0)), Some((last, false)), "a move stays put");
        assert_eq!(laser_point(DOWN, Some(pl), Some(grown), away, last, ([0.0; 4], 0.0, 0.0)), None, "the press is dropped: the item isn't clicked");
        assert_eq!(laser_point(UP, Some(pl), Some(grown), away, last, ([0.0; 4], 0.0, 0.0)), Some((None, false)), "the release goes in, where the pointer is");
    }

    /// Replayed through the event loop: a laser presses on Kickoff's item by its ray, then loses
    /// tracking, and its release still lets the press go in the session. A pass with no events
    /// in between sends nothing and changes nothing.
    #[test]
    fn a_release_after_the_laser_loses_tracking_still_lets_go() {
        use crate::geometry::Pose;
        let _vr = crate::trace::fake_vr();
        let mut p = Plasma::new();
        let (placed, ev) = kickoff(); // the bar with Kickoff open, at kickoff's world below
        p.replay(&placed);
        let world = crate::geometry::panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        let pl = Placement::from_matrix(&world, 0.6, grown[3] as f64 / grown[2] as f64, 0.0);
        let item = (600.0, 1200.0);
        let (o, d) = laser_at(&pl, grown, item);
        let aim = crate::stage::fake::aimed(o, [o[0] + d[0], o[1] + d[1], o[2] + d[2]]);
        let pose = |m: Option<crate::geometry::Mat>| crate::trace::pose_line(3, m.as_ref()).to_string();
        p.replay(&[pose(Some(aim)), "{\"tick\":\"plasma\"}".into(), ev(sys::EVREventType_VREvent_MouseButtonDown)].join("\n"));
        let sent = std::mem::take(&mut *windows::SHELL_SENT.lock().unwrap());
        assert_eq!(sent.len(), 1, "the press: {sent:?}");
        let at = sent[0].1.unwrap();
        assert!((at.0 - item.0).abs() < 2.0 && (at.1 - item.1).abs() < 2.0, "by its ray, on the item: {at:?}");
        assert_eq!(p.held, PTR_FLAGS_BUTTON1);
        pass(&mut p, &[]);
        assert!(windows::SHELL_SENT.lock().unwrap().is_empty() && p.held == PTR_FLAGS_BUTTON1, "an empty pass: nothing sent, still held");
        p.replay(&[pose(None), "{\"tick\":\"plasma\"}".into(), ev(sys::EVREventType_VREvent_MouseButtonUp)].join("\n"));
        let sent = std::mem::take(&mut *windows::SHELL_SENT.lock().unwrap());
        assert_eq!(sent.iter().map(|s| s.0).collect::<Vec<_>>(), [PTR_FLAGS_BUTTON1], "the release went in with no pose: {sent:?}");
        assert_eq!(p.held, 0, "nothing left held");
    }

    #[test]
    fn a_new_crop_forgets_the_lasers_last_points() {
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        let mut lasts = Lasts::default();
        assert_eq!(lasts.get(3, Some(grown)), None);
        lasts.set(3, (600.0, 1200.0));
        assert_eq!(lasts.get(3, Some(grown)), Some((600.0, 1200.0)), "same crop: kept");
        assert_eq!(lasts.get(3, Some(BAR)), None, "the menu closed: gone");
        lasts.set(3, (540.0, 1795.0));
        assert_eq!(lasts.get(3, Some(grown)), None, "and it opened again: gone again");
        assert_eq!(lasts.get(3, None), None, "hidden");
    }

    #[test]
    fn a_ray_just_past_a_curved_edge_lands_on_the_edge() {
        use crate::geometry::{Pose, panel_matrix};
        let m = panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        let pl = placed_for(&m, grown, 0.0006, 2.0);
        // aimed at a point 2 cm past the right edge, along the arc, from in front of it
        let t = pl.on_surface(pl.width / 2.0 + 0.02, 0.0, 0.0);
        let to = [t[0][3], t[1][3], t[2][3]].map(f64::from);
        let n = [t[0][2], t[1][2], t[2][2]].map(f64::from);
        let from = [to[0] + n[0] * 0.6, to[1] + n[1] * 0.6, to[2] + n[2] * 0.6];
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let l = norm(&d);
        let (at, fell_back) = laser_point(MOVE, Some(pl), Some(grown), Some((from, d.map(|x| x / l))), None, ([0.0, 0.0, 1.0, 1.0], 0.5, 0.5)).unwrap();
        let right = (grown[0] + grown[2] - 1) as f64;
        assert!(!fell_back && (at.unwrap().0 - right).abs() < 0.5, "{at:?}, the right edge is {right}");
    }

    #[test]
    fn closing_the_start_menu_brings_the_laser_back_to_the_bar() {
        use crate::geometry::{Pose, panel_matrix};
        let m = panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let mpp = 0.0006;
        let grown = crop(BAR, &[[514, 1100, 650, 660]], WL0).unwrap();
        for curve in [0.0, 2.0] {
            let (open, shut) = (placed_for(&m, grown, mpp, curve), placed_for(&m, BAR, mpp, curve));
            // a laser on a task on the bar while the menu's open, then the menu closes under it
            let task = (1100.0, 1795.0);
            let (o, d) = laser_at(&open, grown, task);
            let on = |pl: &Placement, c: Rect| ray_at(pl, c, &o, &d).unwrap();
            let close = |a: (f64, f64)| (a.0 - task.0).abs() < 1.5 && (a.1 - task.1).abs() < 1.5;
            assert!(close(on(&open, grown)) && close(on(&shut, BAR)), "curve {curve}: same task before and after");
            // where the menu was is off the bar now, so it's clamped to the bar's top edge
            let (o, d) = laser_at(&open, grown, (600.0, 1200.0));
            let y = ray_at(&shut, BAR, &o, &d).unwrap().1;
            assert!((y - BAR[1] as f64).abs() < 1.5, "curve {curve}: {y}");
        }
    }

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

    #[test]
    fn plasma_is_placed_where_the_frame_puts_it_and_its_crop_is_only_sent_when_it_changes() {
        use crate::geometry::{Pose, panel_matrix};
        use crate::overlay::fake::{Call, Writes};
        let m = panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], ..Default::default() });
        let (b0, b1) = ([0.0, 0.1, 0.9, 0.5], [0.0, 0.1, 0.9, 0.6]);
        let mut w = Writes::default();
        crate::overlay::apply(&mut w, crate::vr::Handle(4), &place_for(&m, None, 0.89, 2.0, b0, None));
        assert_eq!(w.0.first(), Some(&Call::Bounds(crate::vr::Handle(4), b0)), "the first placement sends the crop, before the width");
        assert_eq!(w.0.last(), Some(&Call::Room(crate::vr::Handle(4), m)));
        let mut w = Writes::default();
        crate::overlay::apply(&mut w, crate::vr::Handle(4), &place_for(&m, None, 0.89, 2.0, b0, Some(b0)));
        assert!(!w.0.iter().any(|c| matches!(c, Call::Bounds(..))), "the same crop again: not resent");
        let mut w = Writes::default();
        crate::overlay::apply(&mut w, crate::vr::Handle(4), &place_for(&m, Some(crate::ids::Dev::Controller(crate::ids::Ctl::new(2))), 0.89, 0.0, b1, Some(b0)));
        assert!(w.0.contains(&Call::Bounds(crate::vr::Handle(4), b1)), "a new crop is");
        assert_eq!(w.0.last(), Some(&Call::Device(crate::vr::Handle(4), 2, m)), "on a controller when the frame is on one");
    }
}
