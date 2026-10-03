//! The Preferences window: settings.json's user settings, each applied the moment it's clicked.
//!   Layout     Taskbar [Fixed|Follow|Wrist] (`taskbar`, same as control.rs's `taskbar <mode>`);
//!              Window text [-] N% [+], 70..150% (`window_px_per_m`, where 1400 is 100%. The
//!              windows get asked for their new sizes on windows.rs's next tick)
//!   Input      Pointing hand [Left|Right] (`dominant_hand`, for the laser and the wrist taskbar)
//!   Alignment  Align [Fast|Precise] (`align`, cc-home's; the Machines window's switch sets it too)
//! plus a status line for settings.json's errors.
//!
//! The taskbar's gear and `prefs show|hide` open and close it. It takes turns with the other
//! windows at grab.rs's Extra slot (opening one closes the other, control::show), so it costs two
//! overlays while open (itself and its card), made when it opens and destroyed when it closes.
//! It's placed the same way as the Machines window (ui::spot, spots.home.prefs), and its labels
//! are assets.rs's tags (LABELS).
use super::ui::{self, BTN_H, GAP, MPP, PAD, Px, ROW, Rect, SCALE, SORT, TITLE, text, typed, typed_w};
use crate::geometry::{Placement, panel_matrix};
use crate::grab::{self, Look, Paint, TagImg, over, tint};
use crate::kvm::KVM;
use crate::laser::{self, Hand};
use crate::taskbar::{Mode, Taskbar, tex_scale};
use crate::windows::Windows;
use crate::{HIDDEN, call, config, theme, vr};
use openvr_sys as sys;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Whether it should be open. The gear, `prefs show|hide` and its card's close set it, and tick
/// follows.
pub static OPEN: AtomicBool = AtomicBool::new(false);

/// Its labels, drawn by assets.rs at start as tag-ui-<key>.rgba. Align, Fast and Precise are
/// shared with the Machines window.
pub const LABELS: [(&str, &str); 12] = [
    ("prefs", "Preferences"),
    ("layout", "Layout"),
    ("taskbar", "Taskbar"),
    ("fixed", "Fixed"),
    ("follow", "Follow"),
    ("wrist", "Wrist"),
    ("text", "Window text"),
    ("input", "Input"),
    ("hand", "Pointing hand"),
    ("left", "Left"),
    ("right", "Right"),
    ("alignment", "Alignment"),
];

const W: f64 = 560.0; // in units (the taskbar's: a Breeze logical pixel)
const ROWS: usize = 8; // Layout, Taskbar, Window text, Input, Pointing hand, Alignment, Align, and the status
const H: f64 = TITLE + ROWS as f64 * ROW + PAD;
const SEG: f64 = 84.0; // one segment's width
const STEP: [f64; 3] = [BTN_H, 64.0, BTN_H]; // [-] N% [+]
const MODES: [Mode; 3] = [Mode::Fixed, Mode::Follow, Mode::Wrist];
const BASE: f64 = 1400.0; // window_px_per_m at 100%

/// What's under a laser.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Taskbar(usize), // which segment
    Minus,
    Plus,
    Hand(usize),
    Align(usize),
}

/// Window text's percent for window_px_per_m d, in 10% steps from 70 to 150.
fn pct_of(d: f64) -> u32 {
    ((BASE * 10.0 / d).round() * 10.0).clamp(70.0, 150.0) as u32
}

/// window_px_per_m for pct.
fn density_of(pct: u32) -> f64 {
    (BASE * 100.0 / pct as f64).round()
}

fn row_y(i: usize) -> f64 {
    TITLE + (i as f64 + 0.5) * ROW
}

/// The span of a segmented button with n segments, right-aligned.
fn span(n: usize) -> (f64, f64) {
    (W - PAD - n as f64 * SEG, W - PAD)
}

fn step(k: usize) -> (f64, f64) {
    ui::right(W - PAD, &STEP, k)
}

/// What's at (x, y), in units from the top left.
fn hit(x: f64, y: f64) -> Option<Hit> {
    if y < TITLE {
        return None;
    }
    let i = ((y - TITLE) / ROW) as usize;
    if (y - row_y(i)).abs() > BTN_H / 2.0 {
        return None;
    }
    let on = |(x0, x1): (f64, f64)| (x0..=x1).contains(&x);
    match i {
        1 => ui::segment(3, span(3), x).map(Hit::Taskbar),
        2 if on(step(0)) => Some(Hit::Minus),
        2 if on(step(2)) => Some(Hit::Plus),
        4 => ui::segment(2, span(2), x).map(Hit::Hand),
        6 => ui::segment(2, span(2), x).map(Hit::Align),
        _ => None,
    }
}

/// The area hit a's look covers (x0, y0, x1, y1): its control (all of it, for a segmented one)
/// and the space around it.
fn rect(a: Hit) -> Rect {
    let ((x0, x1), i) = match a {
        Hit::Taskbar(_) => (span(3), 1),
        Hit::Minus => (step(0), 2),
        Hit::Plus => (step(2), 2),
        Hit::Hand(_) => (span(2), 4),
        Hit::Align(_) => (span(2), 6),
    };
    let cy = row_y(i);
    (x0 - 8.0, cy - ROW / 2.0, x1 + 8.0, cy + ROW / 2.0)
}

/// All of row i.
fn whole_row(i: usize) -> Rect {
    (0.0, row_y(i) - ROW / 2.0, W, row_y(i) + ROW / 2.0)
}

/// What a laser at (x, y) is on: a control, or else the one it was already on while it's still
/// near it. That way the gaps between rows and segments don't drop it, so there's no flicker and a
/// press that slips a little still lands.
fn held(was: Option<Hit>, x: f64, y: f64) -> Option<Hit> {
    hit(x, y).or(was.filter(|&a| {
        let (x0, y0, x1, y1) = rect(a);
        (x0..=x1).contains(&x) && (y0..=y1).contains(&y)
    }))
}

/// The areas to redraw going from a to b. None means redraw all of it.
fn dirty(a: &Key, b: &Key) -> Option<Vec<Rect>> {
    if a.theme != b.theme {
        return None;
    }
    let mut r: Vec<Rect> = if a.hover != b.hover { [a.hover, b.hover].into_iter().flatten().map(rect).collect() } else { Vec::new() };
    let changed = [(a.taskbar != b.taskbar, 1), (a.pct != b.pct, 2), (a.left != b.left, 4), (a.precise != b.precise, 6), (a.status != b.status, 7)];
    r.extend(changed.into_iter().filter(|c| c.0).map(|c| whole_row(c.1)));
    Some(r)
}

/// What it's drawn from. When this changes, it gets redrawn.
#[derive(Clone, PartialEq, Debug)]
struct Key {
    taskbar: usize, // index into MODES
    pct: u32,
    left: bool,
    precise: bool,
    hover: Option<Hit>,
    status: String,
    theme: u32,
}

/// Its labels, plus ascii.rgba.
#[derive(Default)]
struct Ui {
    title: Option<TagImg>,
    heads: [Option<TagImg>; 3],  // Layout, Input, Alignment
    names: [Option<TagImg>; 4],  // Taskbar, Window text, Pointing hand, Align
    modes: [Option<TagImg>; 3],  // Fixed, Follow, Wrist
    hands: [Option<TagImg>; 2],  // Left, Right
    aligns: [Option<TagImg>; 2], // Fast, Precise
    ascii: Option<TagImg>,
}

struct Scene<'a> {
    k: &'a Key,
    p: Paint,
    ui: &'a Ui,
}

impl Scene<'_> {
    /// Row i: a heading, a setting (its name and its control), or the status.
    fn row(&self, i: usize, x: f64, y: f64) -> Px {
        let (t, k, ui, cy) = (&self.p.t, self.k, self.ui, row_y(i));
        let ink = |px: Px, c| tint(px, c, c, c);
        let (name, out) = match i {
            0 | 3 | 5 => return ink(text(ui.heads[i / 2].as_ref(), x, y, PAD + 4.0, cy), t.wdim),
            7 => {
                let left = (PAD + 4.0).min(W - PAD - typed_w(ui.ascii.as_ref(), &k.status)); // a long one shows its end
                return if x < W - PAD { ink(typed(ui.ascii.as_ref(), &k.status, x, y, left, cy), t.wtext) } else { ([0.0; 3], 0.0) };
            }
            1 => (0, self.seg(&ui.modes, k.taskbar, |h| matches!(h, Hit::Taskbar(_)), 3, cy, x, y)),
            2 => (1, self.stepper(cy, x, y)),
            4 => (2, self.seg(&ui.hands, k.left as usize ^ 1, |h| matches!(h, Hit::Hand(_)), 2, cy, x, y)),
            _ => (3, self.seg(&ui.aligns, k.precise as usize, |h| matches!(h, Hit::Align(_)), 2, cy, x, y)),
        };
        let name = if x < span(3).0 - GAP { ink(text(ui.names[name].as_ref(), x, y, PAD + 16.0, cy), t.wtext) } else { ([0.0; 3], 0.0) };
        over(name, out)
    }

    /// A segmented button with n segments (labels l). `chosen` is solid and the hovered one (is) is lit.
    fn seg(&self, l: &[Option<TagImg>], chosen: usize, is: fn(Hit) -> bool, n: usize, cy: f64, x: f64, y: f64) -> Px {
        let hover = match self.k.hover {
            Some(h @ (Hit::Taskbar(s) | Hit::Hand(s) | Hit::Align(s))) if is(h) => Some(s),
            _ => None,
        };
        let labels: Vec<_> = l.iter().map(Option::as_ref).collect();
        ui::segmented(&self.p, &labels, chosen, hover, span(n), cy, x, y)
    }

    /// [-] N% [+]
    fn stepper(&self, cy: f64, x: f64, y: f64) -> Px {
        let (t, g) = (&self.p.t, self.ui.ascii.as_ref());
        let mut out = ([0.0; 3], 0.0);
        for (k, s, h) in [(0, "-", Some(Hit::Minus)), (1, &*format!("{}%", self.k.pct), None), (2, "+", Some(Hit::Plus))] {
            let (x0, x1) = step(k);
            if x <= x0 - 7.0 || x >= x1 + 7.0 {
                continue;
            }
            if h.is_some() {
                out = over(out, ui::button(&self.p, if self.k.hover == h { Look::Lit } else { Look::Rest }, (x0, x1), x, y, cy));
            }
            out = over(out, tint(typed(g, s, x, y, (x0 + x1 - typed_w(g, s)) / 2.0, cy), t.wtext, t.wtext, t.wtext));
        }
        out
    }

    /// One pixel at (x, y), in units from the top left: a Breeze window holding the rows.
    fn pixel(&self, x: f64, y: f64) -> Px {
        let Some((mut out, d)) = ui::chrome(&self.p, self.ui.title.as_ref(), W, H, x, y) else { return ([0.0; 3], 0.0) };
        let i = ((y - TITLE) / ROW).floor();
        if i >= 0.0 && (i as usize) < ROWS {
            out = over(out, self.row(i as usize, x, y));
        }
        ui::border(&self.p, out, d)
    }
}

pub struct Prefs {
    ov: Option<vr::Handle>,
    shown: bool,
    ui: Arc<Ui>,
    precise: bool, // settings.json's align, read when the window opens
    status: String,
    hover: Option<Hit>,
    painter: ui::Painter<Key>,
}

impl Prefs {
    pub fn new(assets: &str) -> Prefs {
        let load = |k: &str| crate::load_tag(&format!("{assets}/tag-ui-{k}.rgba"));
        Prefs {
            ov: None,
            shown: false,
            ui: Arc::new(Ui {
                title: load("prefs"),
                heads: ["layout", "input", "alignment"].map(load),
                names: ["taskbar", "text", "hand", "align"].map(load),
                modes: ["fixed", "follow", "wrist"].map(load),
                hands: ["left", "right"].map(load),
                aligns: ["fast", "precise"].map(load),
                ascii: crate::load_tag(&format!("{assets}/ascii.rgba")),
            }),
            precise: false,
            status: String::new(),
            hover: None,
            painter: ui::Painter::default(),
        }
    }

    /// True under a laser, so the loop keeps the display's rate.
    pub fn moving(&self) -> bool {
        self.hover.is_some()
    }

    /// Every frame: opens or closes it as OPEN says, handles its events, and draws and shows it.
    pub fn tick(&mut self, grab: &mut grab::Grab, windows: &mut Windows, bar: &mut Taskbar) {
        let open = OPEN.load(Relaxed);
        if open && self.ov.is_none() {
            self.open(grab);
        } else if !open && let Some(h) = self.ov.take() {
            grab.close_extra("prefs");
            crate::gpu::forget(h);
            call!(ov, DestroyOverlay, h);
            (self.shown, self.hover, self.painter) = (false, None, ui::Painter::default());
            eprintln!("prefs: closed");
        }
        let Some(h) = self.ov else { return };
        self.events(h, grab, windows, bar);
        // hidden while the scan hides the panels, and while a VR game runs
        let show = !HIDDEN.load(Relaxed) && !bar.game();
        if show != self.shown {
            self.shown = show;
            grab.set_extra_shown(show);
            if show { call!(ov, ShowOverlay, h) } else { call!(ov, HideOverlay, h) };
        }
        if show {
            self.paint(h, windows, bar);
        }
    }

    /// Creates it where it was last put down, else ahead of the eyes (ui::spot), along with its card.
    fn open(&mut self, grab: &mut grab::Grab) {
        if grab.extra_taken() {
            return; // the Machines window still has the slot; it closes on its tick and this opens on the next one
        }
        let h = match vr::create_overlay("controlcenter.prefs", "Command Center preferences") {
            Ok(h) => h,
            Err(e) => {
                eprintln!("prefs: no overlay: {e}"); // SteamVR's 128 are shared and we're out of them
                OPEN.store(false, Relaxed);
                return;
            }
        };
        call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        call!(ov, SetOverlaySortOrder, h, SORT);
        let pose = ui::spot("prefs", W * MPP);
        self.precise = super::machines::precise(); // whatever settings.json says now
        let pl = Placement::from_matrix(&panel_matrix(&pose), pose.width, H / W, pose.curve);
        grab.open_extra("prefs", h, SORT - 1, theme::VIOLET, pl, &OPEN);
        self.ov = Some(h);
        eprintln!("prefs: opened");
    }

    /// Laser events: hovering, and presses on a control.
    fn events(&mut self, h: vr::Handle, grab: &mut grab::Grab, windows: &mut Windows, bar: &mut Taskbar) {
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        while ui::next_event(h, &mut e) {
            let (m, dev) = (unsafe { e.data.mouse }, e.trackedDeviceIndex);
            let at = held(self.hover, m.x as f64, H - m.y as f64); // window units; events count from the bottom left
            // its card fades in, and a release ends a carry (even one pressed elsewhere and let go over it)
            grab.panel_event(grab.slot(), &e, &mut KVM.lock().unwrap());
            match e.eventType {
                sys::EVREventType_VREvent_MouseMove => self.hover = at,
                sys::EVREventType_VREvent_FocusLeave => self.hover = None,
                sys::EVREventType_VREvent_MouseButtonDown if m.button == sys::EVRMouseButton_VRMouseButton_Left => {
                    if vr::is_real_controller(dev) {
                        let mut k = KVM.lock().unwrap();
                        if k.awake {
                            k.set_awake(false); // the last device pressed becomes primary (R-2)
                        }
                    }
                    if let Some(a) = at {
                        self.act(a, windows, bar);
                    }
                }
                // a press on Plasma's bar that was let go over it (taskbar.rs)
                sys::EVREventType_VREvent_MouseButtonUp => windows.plasma_release(),
                _ => {}
            }
        }
    }

    /// A click: applies it now and writes it to settings.json, with any error going in the status.
    fn act(&mut self, a: Hit, windows: &mut Windows, bar: &mut Taskbar) {
        let same = match a {
            Hit::Taskbar(k) => MODES[k] == bar.mode(), // skip it, since set_mode would restart its placement
            Hit::Hand(k) => (k == 0) == (laser::dominant() == Hand::Left),
            Hit::Align(k) => (k == 1) == self.precise,
            Hit::Minus | Hit::Plus => false,
        };
        if same {
            return;
        }
        let r = match a {
            Hit::Taskbar(k) => bar.choose(MODES[k]),
            Hit::Minus | Hit::Plus => {
                let pct = pct_of(windows.px_per_m());
                let pct = if a == Hit::Plus { (pct + 10).min(150) } else { pct.saturating_sub(10).max(70) };
                let d = density_of(pct);
                eprintln!("prefs: window text {pct}% ({d} px/m)");
                windows.set_density(d);
                config::set_setting("window_px_per_m", serde_json::json!(d as u32))
            }
            Hit::Hand(k) => {
                let h = [Hand::Left, Hand::Right][k];
                eprintln!("prefs: pointing hand {h:?}");
                laser::set_dominant(h);
                config::set_setting("dominant_hand", serde_json::json!(["left", "right"][k]))
            }
            Hit::Align(k) => {
                self.precise = k == 1;
                let v = ["fast", "precise"][k];
                eprintln!("prefs: align {v}");
                config::set_setting("align", serde_json::json!(v))
            }
        };
        self.status = r.err().unwrap_or_default();
    }

    /// Redraws it off the main thread when anything changed, and only the parts that did.
    fn paint(&mut self, h: vr::Handle, windows: &Windows, bar: &Taskbar) {
        let key = Key {
            taskbar: MODES.iter().position(|&m| m == bar.mode()).unwrap_or(0),
            pct: pct_of(windows.px_per_m()),
            left: laser::dominant() == Hand::Left,
            precise: self.precise,
            hover: self.hover,
            status: self.status.clone(),
            theme: theme::generation(),
        };
        self.painter.paint(h, key, || {
            let (ui, p) = (self.ui.clone(), Paint::new(&theme::get(), theme::VIOLET));
            move |was: Option<Key>, px, k: &Key| {
                let sc = Scene { k, p, ui: &ui };
                let s = tex_scale(W, H, SCALE);
                ui::redraw(px, ((W * s) as usize, (H * s) as usize), s, (W, H), was.and_then(|w| dirty(&w, k)), |x, y| sc.pixel(x, y))
            }
        });
    }

    pub fn destroy(&mut self) {
        if let Some(h) = self.ov.take() {
            crate::gpu::forget(h);
            call!(ov, DestroyOverlay, h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(hover: Option<Hit>) -> Key {
        Key { taskbar: 1, pct: 100, left: false, precise: true, hover, status: "status".into(), theme: 0 }
    }

    #[test]
    fn hits() {
        let mid = |(x0, x1): (f64, f64)| (x0 + x1) / 2.0;
        let (s0, _) = span(3);
        assert_eq!(hit(s0 + 1.0, row_y(1)), Some(Hit::Taskbar(0)));
        assert_eq!(hit(s0 + SEG * 1.5, row_y(1) + BTN_H / 2.0 - 1.0), Some(Hit::Taskbar(1)));
        assert_eq!(hit(W - PAD - 1.0, row_y(1)), Some(Hit::Taskbar(2)));
        assert_eq!(hit(mid(step(0)), row_y(2)), Some(Hit::Minus));
        assert_eq!(hit(mid(step(2)), row_y(2)), Some(Hit::Plus));
        assert_eq!(hit(mid(step(1)), row_y(2)), None, "the percent");
        assert_eq!(hit(step(0).1 + GAP / 2.0, row_y(2)), None, "between - and the percent");
        assert_eq!(hit(span(2).0 + 1.0, row_y(4)), Some(Hit::Hand(0)));
        assert_eq!(hit(W - PAD - 1.0, row_y(4)), Some(Hit::Hand(1)));
        assert_eq!(hit(W - PAD - 1.0, row_y(6)), Some(Hit::Align(1)));
        assert_eq!(hit(span(2).0 - 1.0, row_y(6)), None, "left of it: its name");
        assert_eq!(hit(W - PAD - 1.0, TITLE / 2.0), None, "the title bar");
        assert_eq!(hit(W - PAD - 1.0, row_y(0)), None, "a heading");
        assert_eq!(hit(W - PAD - 1.0, row_y(1) + BTN_H / 2.0 + 1.0), None, "between rows");
        assert_eq!(hit(W - PAD - 1.0, row_y(7)), None, "the status");
        assert_eq!(hit(PAD + 20.0, row_y(1)), None, "a name");
        assert!(PAD + 16.0 + 160.0 < span(3).0 - GAP, "room for the names");
    }

    #[test]
    fn window_text_steps_round_trip() {
        assert_eq!(pct_of(BASE), 100);
        for pct in (70..=150).step_by(10) {
            assert_eq!(pct_of(density_of(pct)), pct);
        }
        assert_eq!(density_of(150), 933.0);
        assert_eq!(pct_of(3000.0), 70, "clamped");
        assert_eq!(pct_of(500.0), 150);
        assert_eq!(pct_of(1300.0), 110, "the nearest 10%");
    }

    #[test]
    fn it_draws_the_chosen_segment_in_the_accent() {
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::VIOLET));
        let k = key(None);
        let sc = Scene { k: &k, p, ui: &ui };
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        let seg = |n: usize, s: usize| span(n).0 + SEG * (s as f64 + 0.5);
        assert_eq!(sc.pixel(0.2, 0.2).1, 0.0, "round corners: clear");
        assert!(near(sc.pixel(W / 2.0, TITLE / 2.0).0, p.t.frame), "the title bar");
        assert!(near(sc.pixel(seg(3, 1), row_y(1)).0, p.acc.line), "follow: chosen");
        assert!(near(sc.pixel(seg(3, 0), row_y(1)).0, p.t.raised), "fixed: not");
        assert!(near(sc.pixel(seg(2, 1), row_y(4)).0, p.acc.line), "right hand");
        assert!(near(sc.pixel(seg(2, 1), row_y(6)).0, p.acc.line), "precise");
        let k = Key { left: true, precise: false, ..key(None) };
        let sc = Scene { k: &k, p, ui: &ui };
        assert!(near(sc.pixel(seg(2, 0), row_y(4)).0, p.acc.line) && near(sc.pixel(seg(2, 1), row_y(4)).0, p.t.raised), "left hand");
        assert!(near(sc.pixel(seg(2, 0), row_y(6)).0, p.acc.line), "fast");
    }

    #[test]
    fn a_patch_draws_as_all_of_it() {
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::VIOLET));
        let s = 1.5;
        let (tw, th) = ((W * s) as usize, (H * s) as usize);
        let hovers = [
            (None, Some(Hit::Taskbar(0))),
            (Some(Hit::Taskbar(0)), Some(Hit::Taskbar(2))),
            (Some(Hit::Minus), Some(Hit::Plus)),
            (Some(Hit::Plus), Some(Hit::Hand(0))),
            (Some(Hit::Hand(1)), Some(Hit::Align(0))),
            (Some(Hit::Align(1)), None),
        ];
        let mut cases: Vec<(Key, Key)> = hovers.into_iter().map(|(a, b)| (key(a), key(b))).collect();
        let h = Some(Hit::Taskbar(0));
        cases.extend([
            Key { taskbar: 0, ..key(h) },
            Key { pct: 150, ..key(h) },
            Key { left: true, ..key(None) },
            Key { precise: false, ..key(None) },
            Key { status: "settings.json: a much longer error than it was".into(), ..key(Some(Hit::Plus)) },
        ].map(|b| (key(h), b)));
        let draw = |k: &Key| {
            let sc = Scene { k, p, ui: &ui };
            grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
        };
        for (a, b) in cases {
            let sc = Scene { k: &b, p, ui: &ui };
            let t = ui::redraw(draw(&a), (tw, th), s, (W, H), dirty(&a, &b), |x, y| sc.pixel(x, y));
            assert!(!t.full && t.px == draw(&b), "{a:?} -> {b:?}");
        }
        assert!(dirty(&key(None), &Key { theme: 1, ..key(None) }).is_none(), "a new theme: all of it");
    }

    #[test]
    fn a_laser_between_controls_keeps_its_hover() {
        let (s0, _) = span(3);
        let gap = row_y(1) + BTN_H / 2.0 + 2.0; // below Fixed, above Window text's row
        assert_eq!(hit(s0 + 1.0, gap), None);
        assert_eq!(held(Some(Hit::Taskbar(0)), s0 + 1.0, gap), Some(Hit::Taskbar(0)));
        assert_eq!(held(Some(Hit::Taskbar(0)), W - PAD - 1.0, row_y(1)), Some(Hit::Taskbar(2)), "on another: that one");
        assert_eq!(held(Some(Hit::Taskbar(0)), PAD + 20.0, row_y(1)), None, "off it");
        assert_eq!(held(None, s0 + 1.0, gap), None);
    }
}
