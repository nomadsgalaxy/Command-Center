//! Moving and resizing panels the way SteamVR windows do it. The look follows the Plasma
//! session (theme.rs): each panel sits in a Breeze-like frame, its folder tabs act as a
//! titlebar, and there's a grab bar under it. Point any laser at them (a controller's, or the
//! mouse's SteamVR laser) and pull the trigger. Not the grip, because on the Frame SteamVR
//! keeps the grip for switching a controller between gamepad and hands. What the trigger does depends on where:
//!   - the bar or an edge of the card: the panel rides rigidly on that device until you let
//!     go, and the joystick (or the wheel) pushes it away or pulls it closer;
//!   - the curve button beside the bar: while held, the point you took rides on the device.
//!     Pull it toward you to bend the panel, push it back to flatten it. The joystick or
//!     wheel bends it too, 3 cm a notch;
//!   - the button left of the bar, on an aligned monitor that got moved off its aligned place
//!     (say you pulled it closer to read it): puts it back. After a relocalization those places
//!     moved with the room, so it aligns that monitor again instead;
//!   - a corner of the card: the corner follows the laser along the panel's diagonal and
//!     resizes it about its centre, so it shrinks and grows from any corner and keeps its shape;
//!   - the bottom-right corner: works like a desktop window's, the top-left stays put. A window
//!     panel takes any width and height (its window follows, windows.rs), and a remote screen
//!     keeps the remote monitor's shape. In theater mode it's just a corner like the others.
//! The Machines and Preferences windows (control/) get a card too, in a slot after the
//! panels' (`Extra`), taken in turn.
//! Letting go saves the panel into the "home" spot. The card and bar stay shown but fully
//! transparent when nobody needs them, since SteamVR's laser still hits them. A laser or the
//! cursor near the panel fades them in, and they fade out slowly after (docs/panel-move-design.md).
//!
//! The card is one overlay per panel, drawn as a whole (a rounded rectangle around the
//! picture, with the grab bar and the tag painted in) and sitting just behind it. That way its
//! frame, border and glow have one depth and no joins. It used to be nine separate
//! pieces at slightly different depths, and they overlapped when seen at an angle. It also
//! means a panel costs SteamVR only two overlays of the 128 every app shares. The picture sits
//! in front and keeps every laser inside it; where on the card a laser lands says whether it's
//! the bar, an edge or a corner.
use crate::geometry::{Mat, Placement, Pose, V3, angles, dot, inv_rigid, mul, norm, panel_matrix};
use crate::kvm::Kvm;
use crate::theme::{self, Accent, MAGENTA, Rgb, Theme, Tokens};
use crate::{HIDDEN, Source, call, panels, vr};
use openvr_sys as sys;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

/// A border tag's image (width, height, RGBA) from assets.rs. It shows the machine and
/// monitor, or the app, in the card's folder tab.
pub type TagImg = (usize, usize, Vec<u8>);
/// The tag is a folder tab, like on a manila folder: the card's outline rises from its top
/// edge, goes over the tag's text and comes back down, with the frame and glow following it.
/// The card reaches TAB frame widths higher than on the other sides to hold it, but the border
/// itself keeps the same spacing all round.
const TAB: f64 = 0.4;

/// Where the tag goes for a panel w wide, h high, with frame g and a tag image `aspect` wide
/// per high. Returns (image width, its centre u, v, and the tab's inner span u0..u1 and top).
/// The text is 0.4 g high and starts past the rounded corner, and the image leaves room around
/// the text for its glow.
fn legend(w: f64, h: f64, g: f64, aspect: f64, ctls: usize) -> (f64, f64, f64, (f64, f64, f64)) {
    // Flush with the window's left edge (my call), so the tab's side runs straight on from the frame's.
    let start = -w / 2.0 + 0.15 * g;
    let room = if ctls > 0 { controls(w, h, g, ctls).2.0 - 0.3 * g - start } else { w - 0.3 * g };
    let iw = (0.4 * g * 128.0 / 64.0 * aspect).min(room.max(g)); // never past the far corner, nor the controls
    let pad = iw / aspect * 32.0 / 128.0;
    let end = start + iw - 2.0 * pad;
    let v = h / 2.0 + LINE * g + 0.2 * g; // the text just above the card's top line
    (iw, start - pad + iw / 2.0, v, (start - 0.15 * g, end + 0.15 * g, v + 0.16 * g))
}

/// A window's controls sit in the right-hand folder tab, mirroring the tag's. Left to right,
/// ending past the corner: minimize (v, hides the panel and its taskbar chip brings it back),
/// theater (^) and close (x).
pub const CTLS: usize = 3;
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ctl {
    Minimize,
    Theater,
    Close,
}
const CTL: [Ctl; CTLS] = [Ctl::Minimize, Ctl::Theater, Ctl::Close];

/// The controls for a panel w × h with frame g. Returns each button's centre u, their shared v,
/// and the tab's inner span u0..u1 and top (like legend()'s) around the last n buttons (close
/// alone is 1).
fn controls(w: f64, h: f64, g: f64, n: usize) -> ([f64; CTLS], f64, (f64, f64, f64)) {
    let step = 0.6 * g;
    let last = w / 2.0 - 0.3 * g; // flush with the window's right edge (my call), like the tag on the left
    let us: [f64; CTLS] = std::array::from_fn(|k| last - (CTLS - 1 - k) as f64 * step);
    let v = h / 2.0 + LINE * g + 0.2 * g; // the tag's text line
    (us, v, (us[CTLS - n] - 0.3 * g, last + 0.3 * g, v + 0.16 * g))
}

/// A smooth minimum: the union of two distance fields with a rounded join k wide.
fn smin(a: f64, b: f64, k: f64) -> f64 {
    let h = ((k - (a - b).abs()) / k).max(0.0);
    a.min(b) - h * h * k / 4.0
}

/// The part of the card a laser is on: an edge moves the panel, a corner resizes it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    Bar,
    Edge,          // the top or bottom edge (and the tag): carries the panel
    Knob,          // the curve button beside the bar: hold it, then push or pull to bend the panel
    Zoom,          // a window panel's zoom button, next to that: hold it and pull to make the text bigger
    Snap,          // left of the bar: back to the aligned place (or align again after a relocalization)
    Face,          // left of that: turns the panel in place to face the eye
    Ctl(usize),    // a window control in the right-hand tab (CTL)
    Corner(usize), // indexed like CORNER_SIGN
}
/// Each corner's side: (right +1 / left -1, top +1 / bottom -1).
pub const CORNER_SIGN: [(f64, f64); 4] = [(-1.0, 1.0), (1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)];
const BOTTOM_RIGHT: usize = 3; // the corner that stretches (Mode::Stretch)

// The card fades in quickly when a cursor or laser comes near, and fades out slowly (~1.5 s) after.
const FADE_IN: f32 = 0.15;
const FADE_OUT: f32 = 0.008;
const LINGER: Duration = Duration::from_millis(400); // a nearby laser still counts for this long after it leaves
const HOLD_SLOP: f64 = 0.25; // calibration knob: a hovered part stays held this many frames (g) past its edge (~4 mm)
const HOLD: Duration = Duration::from_secs(20); // ...but a laser sitting still on it holds it at most this long, in case a FocusLeave never comes
pub const LOST: Duration = Duration::from_millis(500); // a carrying device that's untracked this long drops the panel
// Knobs: an aligned monitor that's moved this far (centre, width, curve radius) or turned
// this much from its aligned place gets the snap-back button.
const SNAP_MOVE: f64 = 0.01; // m
const SNAP_TURN: f64 = 0.5; // degrees
const FACE_TURN: f64 = 3.0; // degrees: a panel turned less than this from facing the eye gets no face-me button
const MIN_WIDTH: f64 = 0.15;
const MAX_WIDTH: f64 = 6.0;

// ------------------------------------------------------------------ the look
//
// It's Plasma's look, whatever style is set (docs/plasma-look-design.md (b)). The frame is the
// scheme's titlebar colour, opaque, with Breeze's corner radius and a 1 px border. The window
// controls are Breeze's decoration buttons, the bar is a scrollbar handle, and the knobs are
// round buttons. The brand shows up in the accent: a laser nearby brightens the border into
// the panel kind's colour with a soft glow (like the cards on nomadsgalaxy.com), a corner
// under a laser thickens into a grip, and close lights up magenta. Sizes are in Breeze pixels
// (bp). The tag's text, 0.4 g, is Breeze's 10 pt title (13.3 px), so 1 bp = 0.03 g.

/// Each kind of panel has its own accent so its frame tells you what it is: Warp Cyan for a
/// remote machine's screen (krdp), Violet for a Frame window (each its own panel, windows.rs).
pub use crate::theme::{CYAN as REMOTE, VIOLET};
const BP: f64 = 0.03; // one Breeze pixel, in frame widths

/// What a card paints with: the scheme's tokens, plus its accent and close's magenta fitted to them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Paint {
    pub(crate) t: Tokens,
    pub(crate) acc: Accent,
    mag: Accent,
    radius: f64, // corners, in bp
}

impl Paint {
    pub(crate) fn new(th: &Theme, accent: Rgb) -> Paint {
        Paint { t: th.win, acc: th.win.accent(accent), mag: th.win.accent(MAGENTA), radius: th.radius }
    }
}

/// Draws an RGBA image by calling `f` at each pixel's centre, which returns (colour, alpha 0..1).
pub fn draw(w: usize, h: usize, f: impl Fn(f64, f64) -> ([f64; 3], f64)) -> Vec<u8> {
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (c, a) = f(x as f64 + 0.5, y as f64 + 0.5);
            px[(y * w + x) * 4..][..4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, (a.clamp(0.0, 1.0) * 255.0) as u8]);
        }
    }
    px
}

/// Signed distance to a rectangle of half sizes hw × hh about 0 with corners rounded r.
pub fn rbox(x: f64, y: f64, hw: f64, hh: f64, r: f64) -> f64 {
    let (qx, qy) = (x.abs() - hw + r, y.abs() - hh + r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

pub fn mix(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

/// Paint `top` over `under` (both with their alpha).
pub fn over(under: ([f64; 3], f64), top: ([f64; 3], f64)) -> ([f64; 3], f64) {
    let a = top.1 + under.1 * (1.0 - top.1);
    if a <= 0.0 {
        return (under.0, 0.0);
    }
    (mix(under.0, top.0, top.1 / a), a)
}

/// Coverage of a line `half` px either side of 0 at signed distance d (a soft 1 px edge).
pub fn line(d: f64, half: f64) -> f64 {
    (half + 0.5 - d.abs()).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Look {
    Rest,
    Lit,   // a laser or the cursor near it
    Carry, // the panel is being carried or resized
}

/// The card's colour at signed distance d (card pixels, < 0 inside its outline), with bp card
/// pixels to a Breeze pixel. Inside is the opaque frame with a 1 bp border along its edge (the
/// accent when lit, 2 bp while carried). Outside, when lit, is the accent's glow. Past that the
/// card is transparent and only there to give the laser something to hit.
const LINE: f64 = 0.3; // the outline's distance from the picture, in frame widths (10 bp)
fn card(d: f64, bp: f64, p: &Paint, look: Look) -> ([f64; 3], f64) {
    let fill = (p.t.frame, (0.5 - d).clamp(0.0, 1.0));
    let glow = if look != Look::Rest && d > 0.0 { p.acc.glow * (1.0 - d / (10.0 * bp)).max(0.0).powi(2) } else { 0.0 };
    let half = (0.5 * bp * if look == Look::Carry { 2.0 } else { 1.0 }).max(0.5);
    let edge = if look == Look::Rest { p.t.border } else { p.acc.line };
    over(over((p.acc.line, glow), fill), (edge, line(d + half, half)))
}

/// Which part of the card a point is on (u, v from the panel's centre, in metres). None means
/// it's inside the picture.
fn part_at(u: f64, v: f64, w: f64, h: f64) -> Option<Part> {
    let (out_u, out_v) = (u.abs() > w / 2.0, v.abs() > h / 2.0);
    match (out_u, out_v) {
        (false, false) => None,
        (true, true) => CORNER_SIGN.iter().position(|&(sx, sy)| sx * u > 0.0 && sy * v > 0.0).map(Part::Corner),
        (true, false) | (false, true) => Some(Part::Edge),
    }
}

/// The part of a card (for a w × h picture) at (u, v) from the picture's middle, held the same
/// way as prefs.rs held(). A button, the bar or a corner counts where it actually is. On the
/// plain frame (or the picture) within HOLD_SLOP frames of the part it was on, it stays on that
/// part. Otherwise a laser riding a part's edge lights it on and off, and each flip costs a
/// repaint and a whole-card upload. The Extra has a dead zone too: its buttons end 7 mm from
/// its edge, so a press that slips off one shouldn't turn into a carry. There it keeps the
/// part it was on, or none.
fn held_part(spec: &CardSpec, w: f64, h: f64, extra: bool, was: Option<Part>, u: f64, v: f64) -> Option<Part> {
    let g = spec.g;
    let dead = |u: f64, v: f64| extra && u.abs() < w / 2.0 + g / 2.0 && v.abs() < h / 2.0 + g / 2.0;
    let at = |u: f64, v: f64| {
        if let Some(c) = spec.on_ctl(u, v) {
            Some(Part::Ctl(c))
        } else if spec.on_knob(u, v) {
            Some(Part::Knob)
        } else if spec.on_zoom(u, v) {
            Some(Part::Zoom)
        } else if spec.on_snap(u, v) {
            Some(Part::Snap)
        } else if spec.on_face(u, v) {
            Some(Part::Face)
        } else if spec.on_bar(u, v) {
            Some(Part::Bar)
        } else if dead(u, v) {
            None
        } else {
            part_at(u, v, w, h)
        }
    };
    let here = at(u, v);
    if here.is_some_and(|p| p != Part::Edge) {
        return here;
    }
    let s = HOLD_SLOP * g;
    let near = |p: Part| dead(u, v) || [(s, 0.0), (-s, 0.0), (0.0, s), (0.0, -s)].iter().any(|&(du, dv)| at(u + du, v + dv) == Some(p));
    was.filter(|&p| near(p)).or(here)
}

/// The curve radius that puts a w-wide panel's edges `sag` metres in front of its middle
/// (the arc's sagitta: sag = R(1 − cos(w/2R))). Under a centimetre it's 0 (flat), and it never
/// wraps past a half circle (R ≥ w/π).
pub fn radius_for(sag: f64, w: f64) -> f64 {
    let least = w / std::f64::consts::PI;
    if sag < 0.01 {
        return 0.0;
    }
    if sag >= least {
        return least;
    }
    let sagitta = |r: f64| r * (1.0 - (w / (2.0 * r)).cos());
    let (mut lo, mut hi) = (least, 1.0e4); // sagitta falls as the radius grows
    for _ in 0..60 {
        let mid = (lo * hi).sqrt();
        if sagitta(mid) > sag { lo = mid } else { hi = mid }
    }
    (lo * hi).sqrt()
}

/// A wider panel keeps its curve, except it can't wrap past a half circle, so then the radius
/// grows with it. That was my choice: resizing should never dead-end.
pub fn curve_for_width(curve: f64, width: f64) -> f64 {
    if curve > 0.0 { curve.max(width / std::f64::consts::PI) } else { 0.0 }
}

/// Everything needed to draw the card for a panel w × h (metres) with a frame g wide, as a
/// tw × th image covering (w + 2g) × (h + 2g). It's a rounded rectangle around the picture
/// (its corners round about the picture's corners), with `grip`'s corner thickened into a grip.
#[derive(Clone)]
struct CardSpec {
    w: f64,
    h: f64,
    g: f64,
    tab: Option<(f64, f64, f64)>, // the folder tab's inner span u0..u1 and top (legend())
    tag: Option<(Arc<TagImg>, f64, f64, f64)>, // its image, and where: width, centre u, v
    bar: (f64, Look),                          // the grab bar's width, and its look
    knob: Look,                                // the curve button's look
    zoom: Option<Look>,                        // a window panel's zoom button, and its look
    snap: Option<(Look, Glyph)>,               // the snap-back (or align) button left of the bar, and its look
    face: Option<Look>,                        // the face-me button left of that (none when it faces you), and its look
    ctl: Option<[Look; CTLS]>,                 // a window panel's controls, and their looks
    ctls: usize,                               // how many of them, from the right (close alone: 1)
    size: (usize, usize),
    paint: Paint,
    look: Look,
    grip: Option<usize>, // the corner thickened into a grip
}

impl CardSpec {
    /// How much higher than its other sides the card reaches, for the tab.
    fn top(&self) -> f64 {
        if self.tab.is_some() || self.ctl.is_some() { TAB * self.g } else { 0.0 }
    }

    /// The window control under (u, v).
    fn on_ctl(&self, u: f64, v: f64) -> Option<usize> {
        self.ctl?;
        let (us, cv, _) = controls(self.w, self.h, self.g, self.ctls);
        let g = self.g;
        (CTLS - self.ctls..CTLS).find(|&k| (u - us[k]).abs() <= 0.3 * g && (v - cv).abs() <= 0.3 * g)
    }

    fn ctl_rect(&self) -> (usize, usize, usize, usize) {
        let (_, _, (u0, u1, top)) = controls(self.w, self.h, self.g, self.ctls);
        self.rect(u0 - 0.2 * self.g, u1 + 0.2 * self.g, self.h / 2.0, top + 0.3 * self.g)
    }

    /// The grab bar's centre v and height, with room for its glow. It sits under the picture,
    /// clear of the hairline.
    fn bar_box(&self) -> (f64, f64) {
        let bw = self.bar.0;
        (-(self.h / 2.0 + 0.6 * self.g + bw * 12.0 / BAR_W as f64), bw * BAR_H as f64 / BAR_W as f64)
    }

    /// How much lower than its other sides the card reaches, for a bar wider than the frame holds.
    fn bottom(&self) -> f64 {
        let (bv, bh) = self.bar_box();
        (-bv + bh / 2.0 - self.h / 2.0 - self.g).max(0.0)
    }

    fn scale(&self) -> (f64, f64) {
        (self.size.0 as f64 / (self.w + 2.0 * self.g), self.size.1 as f64 / (self.h + 2.0 * self.g + self.top() + self.bottom()))
    }

    /// The texture's pixel rectangle (x0, x1, y0, y1) over u0..u1, v0..v1, a pixel wider all round.
    fn rect(&self, u0: f64, u1: f64, v0: f64, v1: f64) -> (usize, usize, usize, usize) {
        let (sx, sy) = self.scale();
        let x = |u: f64| ((u + self.w / 2.0 + self.g) * sx).max(0.0);
        let y = |v: f64| ((self.h / 2.0 + self.g + self.top() - v) * sy).max(0.0);
        let (tw, th) = self.size;
        ((x(u0) as usize).saturating_sub(1), (x(u1).ceil() as usize + 1).min(tw), (y(v1) as usize).saturating_sub(1), (y(v0).ceil() as usize + 1).min(th))
    }

    fn corner_rect(&self, c: usize) -> (usize, usize, usize, usize) {
        let (cx, cy) = CORNER_SIGN[c];
        let (a, b) = (self.w / 2.0, self.w / 2.0 + self.g);
        let (lo, hi) = (self.h / 2.0, self.h / 2.0 + self.g);
        let (u0, u1) = if cx > 0.0 { (a, b) } else { (-b, -a) };
        let (v0, v1) = if cy > 0.0 { (lo, hi) } else { (-hi, -lo) };
        self.rect(u0, u1, v0, v1)
    }

    /// The bar and the buttons beside it, with their glows. It includes the snap button's room
    /// whether the button's there or not, so one repaint can put it in or take it out.
    fn bar_rect(&self) -> (usize, usize, usize, usize) {
        let (bv, bh) = self.bar_box();
        let (ku, _, r) = if self.zoom.is_some() { self.zoom_at() } else { self.knob_at() };
        self.rect(self.snap_at().0 - 4.6 * r, ku + 2.0 * r, bv - bh / 2.0, bv + bh / 2.0) // and the face-me button's
    }

    /// Is (u, v) on the grab bar's pill?
    fn on_bar(&self, u: f64, v: f64) -> bool {
        let ((bv, bh), bw) = (self.bar_box(), self.bar.0);
        u.abs() <= bw / 2.0 && (v - bv).abs() <= bh / 4.0
    }

    /// The curve button, a round ghost button as tall as the pill, just right of the bar.
    /// Returns (centre u, v, radius).
    fn knob_at(&self) -> (f64, f64, f64) {
        let ((bv, bh), bw) = (self.bar_box(), self.bar.0);
        let r = bh / 4.0;
        (bw / 2.0 + 0.6 * r, bv, r) // the pill's own glow room is about one r inside its box
    }

    /// The snap button: the curve button's mirror, just left of the bar.
    fn snap_at(&self) -> (f64, f64, f64) {
        let (ku, kv, r) = self.knob_at();
        (-ku, kv, r)
    }

    fn on_snap(&self, u: f64, v: f64) -> bool {
        let (su, sv, r) = self.snap_at();
        self.snap.is_some() && (u - su).hypot(v - sv) <= r * 1.3
    }

    /// The face-me button, left of where the snap button goes whether it's shown or not.
    /// Pressing face-me can bring the snap button up, and that mustn't land under the laser.
    fn face_at(&self) -> (f64, f64, f64) {
        let (su, sv, r) = self.snap_at();
        (su - 2.6 * r, sv, r)
    }

    fn on_face(&self, u: f64, v: f64) -> bool {
        let (fu, fv, r) = self.face_at();
        self.face.is_some() && (u - fu).hypot(v - fv) <= r * 1.3
    }

    fn on_knob(&self, u: f64, v: f64) -> bool {
        let (ku, kv, r) = self.knob_at();
        (u - ku).hypot(v - kv) <= r * 1.3
    }

    /// The zoom button: right of the curve button.
    fn zoom_at(&self) -> (f64, f64, f64) {
        let (ku, kv, r) = self.knob_at();
        (ku + 2.6 * r, kv, r)
    }

    fn on_zoom(&self, u: f64, v: f64) -> bool {
        let (zu, zv, r) = self.zoom_at();
        self.zoom.is_some() && (u - zu).hypot(v - zv) <= r * 1.3
    }

    fn pixel(&self, x: f64, y: f64) -> ([f64; 3], f64) {
        let (w, h, g) = (self.w, self.h, self.g);
        let (sx, sy) = self.scale();
        let (u, v) = (x / sx - (w + 2.0 * g) / 2.0, h / 2.0 + g + self.top() - y / sy);
        let p = &self.paint;
        if u.abs() <= w / 2.0 && v.abs() <= h / 2.0 {
            return (p.t.frame, 0.0); // the picture's own area, which is in front
        }
        // The outline is a rounded rectangle LINE out from the picture, with the folder tabs
        // (boxes over the tag's text and the controls) joined on with rounded shoulders, all
        // as one distance field.
        let (l, r) = (LINE * g, p.radius * BP * g);
        let mut d = rbox(u, v, w / 2.0 + l, h / 2.0 + l, r);
        let ctl = self.ctl.map(|looks| (looks, controls(w, h, g, self.ctls)));
        // The controls' tab runs out to the card's right edge. A step a third of a frame in from
        // the corner read as a glitch; flush, it reads as the titlebar's corner. Its right half
        // joins without the rounding, which would bulge where the two edges meet.
        // The tag's tab is flush with the left edge the same way (my call), with its
        // left half joined hard. `side` is the half that meets the card's edge (-1 left, +1 right).
        for (u0, u1, top, side) in [self.tab.map(|t| (-w / 2.0, t.1, t.2, -1.0)), ctl.map(|c| (c.1.2.0, w / 2.0, c.1.2.2, 1.0))].into_iter().flatten() {
            let tab = rbox(u - (u0 + u1) / 2.0, v - (h / 2.0 + top) / 2.0, (u1 - u0) / 2.0 + l, (top - h / 2.0) / 2.0 + l, r);
            d = if side * (u - (u0 + u1) / 2.0) > 0.0 { d.min(tab) } else { smin(d, tab, 0.15 * g) };
        }
        let bp = BP * g * sx; // a Breeze pixel, in card pixels
        let mut out = card(d * sx, bp, p, self.look);
        // The grip is the corner's stretch of the border, drawn 3 bp thick in the accent while a laser is on it.
        let du = (u.abs() - w / 2.0).max(0.0);
        let dv = (v.abs() - h / 2.0).max(0.0);
        match (self.grip, part_at(u, v, w, h)) {
            (Some(c), Some(Part::Corner(k))) if c == k => {
                let a = line(d * sx + 1.5 * bp, 1.5 * bp) * (du.min(dv) * sx).clamp(0.0, 1.0);
                out = over(out, (p.acc.line, a));
            }
            _ => {}
        }
        // The grab bar under the picture, drawn in its own 256-wide units.
        let ((bv, bh), bw) = (self.bar_box(), self.bar.0);
        let (bx, by) = ((u + bw / 2.0) / bw * BAR_W as f64, (bv + bh / 2.0 - v) / bh * BAR_H as f64);
        if (0.0..BAR_W as f64).contains(&bx) && (0.0..BAR_H as f64).contains(&by) {
            out = over(out, bar_pixel(p, self.bar.1, bx, by, sx * bw / BAR_W as f64, BP * g * BAR_W as f64 / bw));
        }
        let (ku, kv, r) = self.knob_at();
        if (u - ku).abs() < 2.0 * r && (v - kv).abs() < 2.0 * r {
            out = over(out, knob_pixel(&p.t, &p.acc, self.knob, (u - ku) * sx, (v - kv) * sx, r * sx, Glyph::Curve, bp));
        }
        let (su, sv, r) = self.snap_at();
        if let Some((look, glyph)) = self.snap.filter(|_| (u - su).abs() < 2.0 * r && (v - sv).abs() < 2.0 * r) {
            out = over(out, knob_pixel(&p.t, &p.acc, look, (u - su) * sx, (v - sv) * sx, r * sx, glyph, bp));
        }
        let (fu, fv, r) = self.face_at();
        if let Some(look) = self.face.filter(|_| (u - fu).abs() < 2.0 * r && (v - fv).abs() < 2.0 * r) {
            out = over(out, knob_pixel(&p.t, &p.acc, look, (u - fu) * sx, (v - fv) * sx, r * sx, Glyph::Face, bp));
        }
        let (zu, zv, r) = self.zoom_at();
        if let Some(look) = self.zoom.filter(|_| (u - zu).abs() < 2.0 * r && (v - zv).abs() < 2.0 * r) {
            out = over(out, knob_pixel(&p.t, &p.acc, look, (u - zu) * sx, (v - zv) * sx, r * sx, Glyph::Zoom, bp));
        }
        // The window controls, in their own units.
        if let Some((looks, (us, cv, _))) = ctl {
            for (k, cu) in us.iter().enumerate().skip(CTLS - self.ctls) {
                let (x, y) = ((u - cu) / g, (v - cv) / g); // frame widths from the button's centre
                if x.abs() < 0.32 && y.abs() < 0.32 {
                    out = over(out, ctl_pixel(CTL[k], looks[k], p, x, y, g * sx));
                }
            }
        }
        // The tag, in its folder tab.
        if let Some((img, iw, tu, tv)) = &self.tag {
            let ih = iw * img.1 as f64 / img.0 as f64;
            let (fx, fy) = (img.0 as f64 / iw, img.1 as f64 / ih); // the tag's pixels per metre
            let (ix, iy) = ((u - tu + iw / 2.0) * fx, (tv + ih / 2.0 - v) * fy);
            if ix > -1.0 && iy > -1.0 && ix < img.0 as f64 + 1.0 && iy < img.1 as f64 + 1.0 {
                let (hx, hy) = (0.5 * fx / sx, 0.5 * fy / sy); // half this card pixel, in tag pixels
                out = over(out, tint(average(img, ix - hx, iy - hy, ix + hx, iy + hy), p.t.text, p.t.dim, p.acc.line));
            }
        }
        out
    }

    fn texture(&self) -> Vec<u8> {
        draw(self.size.0, self.size.1, |x, y| self.pixel(x, y))
    }

    /// A card nobody sees (alpha 0, it's only there to catch lasers). It keeps the card's shape
    /// for placing it and for lasers, but at a sixteenth of the pixels each way, transparent and
    /// at Rest, so the first look anyone actually sees (Lit or Carry) draws it in full.
    fn stub(&mut self) -> Vec<u8> {
        self.size = (self.size.0.div_ceil(16), self.size.1.div_ceil(16));
        self.look = Look::Rest;
        vec![0; self.size.0 * self.size.1 * 4]
    }

    /// Repaints just a corner or the bar when a grip or a hover comes or goes. That's a few
    /// thousand pixels instead of the whole card.
    fn repaint(&self, buf: &mut [u8], (x0, x1, y0, y1): (usize, usize, usize, usize)) {
        let tw = self.size.0;
        for y in y0..y1 {
            for x in x0..x1 {
                let (c, a) = self.pixel(x as f64 + 0.5, y as f64 + 0.5);
                buf[(y * tw + x) * 4..][..4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, (a.clamp(0.0, 1.0) * 255.0) as u8]);
            }
        }
    }
}

/// Full card draws happen off the main thread, because a 1920x598 card is 70-150 ms of pixels,
/// which is a lot of the main loop's ticks. There's one worker, and only each panel's latest
/// request waits for it. Each result comes back with its request's number, and one that a newer
/// request has overtaken gets dropped (`answers`).
type Job = (u64, String, CardSpec); // request number, the panel's name (for the log), the card
#[derive(Default)]
struct Jobs {
    queue: Mutex<BTreeMap<usize, Job>>,
    wake: Condvar,
}

impl Jobs {
    /// Queues panel i's card, replacing any it was still waiting for.
    fn put(&self, i: usize, job: Job) {
        self.queue.lock().unwrap().insert(i, job);
        self.wake.notify_one();
    }

    fn cancel(&self, i: usize) {
        self.queue.lock().unwrap().remove(&i);
    }

    /// The next card to draw. Blocks until there is one.
    fn next(&self) -> (usize, Job) {
        let mut q = self.queue.lock().unwrap();
        loop {
            if let Some(job) = q.pop_first() {
                return job;
            }
            q = self.wake.wait(q).unwrap();
        }
    }
}

struct Painter {
    jobs: Arc<Jobs>,
    done: mpsc::Receiver<(usize, u64, Vec<u8>)>, // panel, request number, texture
    seq: u64,
}

impl Painter {
    fn new() -> Painter {
        let jobs = Arc::new(Jobs::default());
        let (tx, done) = mpsc::channel();
        let q = jobs.clone();
        std::thread::Builder::new()
            .name("cc-cards".into())
            .spawn(move || {
                loop {
                    let (i, (seq, name, spec)) = q.next();
                    let draw = || vr::timed(|| format!("{name}: card drawn {}x{}", spec.size.0, spec.size.1), || spec.texture());
                    // A panic here would leave every card waiting for an answer that never
                    // comes, so end the process, same as when cards were drawn on the main
                    // thread. The panic hook has already logged it and systemd starts it again.
                    let Ok(buf) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(draw)) else {
                        std::process::exit(101);
                    };
                    if tx.send((i, seq, buf)).is_err() {
                        return;
                    }
                    vr::wake(); // the main loop may be idling, so wake it to upload this now
                }
            })
            .expect("the card painter thread");
        Painter { jobs, done, seq: 0 }
    }

    /// Asks for panel i's card (replacing any it was still waiting for) and returns the request's number.
    fn ask(&mut self, i: usize, name: &str, spec: CardSpec) -> u64 {
        self.seq += 1;
        self.jobs.put(i, (self.seq, name.into(), spec));
        self.seq
    }
}

/// Does a texture drawn for request `seq` answer the panel's latest request (`pending`)?
fn answers(pending: &Option<(u64, Drawn)>, seq: u64) -> bool {
    pending.as_ref().is_some_and(|(s, _)| *s == seq)
}

/// A window control at (x, y) frame widths from its centre (px is pixels per frame width).
/// It's Breeze's decoration button: an 18 bp circle with a 10 bp glyph stroked 1.5 bp. At rest
/// it's just the glyph. Lit, it gets an accent-tinted circle (close goes solid magenta with a
/// dark x, like Breeze's red). Pressed, it's a solid accent circle.
pub(crate) fn ctl_pixel(ctl: Ctl, look: Look, p: &Paint, x: f64, y: f64, px: f64) -> ([f64; 3], f64) {
    let stroke = (0.75 * BP * px).max(0.7);
    let seg = |ax: f64, ay: f64, bx: f64, by: f64| {
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        line((x - ax - t * dx).hypot(y - ay - t * dy) * px, stroke)
    };
    let s = 5.0 * BP;
    let ink_a = match ctl {
        Ctl::Minimize => seg(-s, s * 0.5, 0.0, -s * 0.5).max(seg(0.0, -s * 0.5, s, s * 0.5)), // v
        Ctl::Theater => seg(-s, -s * 0.5, 0.0, s * 0.5).max(seg(0.0, s * 0.5, s, -s * 0.5)), // ^
        Ctl::Close => seg(-s, -s, s, s).max(seg(-s, s, s, -s)),                              // x
    };
    let close = ctl == Ctl::Close;
    let a = if close { &p.mag } else { &p.acc };
    let inside = (0.5 - (x.hypot(y) - 9.0 * BP) * px).clamp(0.0, 1.0);
    let (disc, ink) = match look {
        Look::Rest => ((a.line, 0.0), p.t.text),
        Look::Lit if !close => ((a.fill.0, a.fill.1 * inside), p.t.text),
        Look::Lit | Look::Carry => ((a.line, inside), a.ink),
    };
    over(disc, (ink, ink_a))
}

/// Turns a tag mask's pixel into colours. In assets.rs's mask, R is the name, G the secondary text and B the dot.
pub fn tint((c, a): ([f64; 3], f64), text: Rgb, dim: Rgb, dot: Rgb) -> ([f64; 3], f64) {
    let sum = c[0] + c[1] + c[2];
    if sum <= 0.0 {
        return (text, 0.0);
    }
    ([0, 1, 2].map(|i| (text[i] * c[0] + dim[i] * c[1] + dot[i] * c[2]) / sum), a)
}

/// The tag image's colour over x0..x1, y0..y1 (in its pixels), averaged by alpha. A card pixel
/// smaller than the tag's takes the nearest one, and a larger one doesn't alias the text.
pub fn average(img: &TagImg, x0: f64, y0: f64, x1: f64, y1: f64) -> ([f64; 3], f64) {
    let (w, h, px) = (img.0 as isize, img.1 as isize, &img.2);
    let (xa, xb) = ((x0.round() as isize).max(0), (x1.round() as isize).max(x0.round() as isize + 1).min(w));
    let (ya, yb) = ((y0.round() as isize).max(0), (y1.round() as isize).max(y0.round() as isize + 1).min(h));
    let (mut c, mut a, mut n) = ([0.0; 3], 0.0, 0.0);
    for y in ya..yb {
        for x in xa..xb {
            let p = &px[((y * w + x) * 4) as usize..][..4];
            let pa = p[3] as f64 / 255.0;
            for k in 0..3 {
                c[k] += p[k] as f64 * pa;
            }
            (a, n) = (a + pa, n + 1.0);
        }
    }
    if a <= 0.0 { ([0.0; 3], 0.0) } else { (c.map(|x| x / a), a / n) }
}

/// The card's texture size. The frame gets at least 60 px, for a crisp hairline and tag text
/// 24 px high, but the whole thing stays within 1.5 M pixels since SteamVR limits raw uploads.
fn card_size(w: f64, h: f64, g: f64) -> (usize, usize) {
    let (fw, fh) = (w + 2.0 * g, h + 2.0 * g);
    let mut scale = (1024.0 / fw).max(60.0 / g);
    scale = scale.min((1.5e6 / (fw * fh)).sqrt()).min(1920.0 / fw).min(1920.0 / fh); // and no side over 1920
    (((fw * scale).floor() as usize).max(8), ((fh * scale).floor() as usize).max(8))
}

const BAR_W: usize = 256;
const BAR_H: usize = 48; // the pill is 24 high, with room for its glow

/// The grab bar at (x, y) of its BAR_W × BAR_H box, with k card pixels to one of its own and bp
/// of its own to a Breeze pixel. It's Breeze's scrollbar handle: a pill in the button colour
/// with a 1 bp border and three grip dots in the dim text colour. Lit, the border takes the
/// accent over an accent tint, with the glow. Carrying, it's solid accent. The edges stay a card
/// pixel soft at any k.
fn bar_pixel(p: &Paint, look: Look, x: f64, y: f64, k: f64, bp: f64) -> ([f64; 3], f64) {
    let r = 12.0;
    let (w, cy) = (BAR_W as f64, BAR_H as f64 / 2.0);
    let cx = x.clamp(r + 12.0, w - r - 12.0);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r; // < 0 inside the pill
    let mut px = button(&p.t, &p.acc, look, d * k, r * k, bp * k);
    let dot = if look == Look::Carry { p.acc.ink } else { p.t.dim };
    for n in [-1.0, 0.0, 1.0] {
        let dd = ((x - (w / 2.0 + n * 11.0)).powi(2) + (y - cy).powi(2)).sqrt();
        px = over(px, (dot, ((1.5 * bp - dd) * k + 0.5).clamp(0.0, 1.0)));
    }
    px
}

/// A Breeze button's body at signed distance d (pixels, < 0 inside) from its outline, with a
/// glow that falls off over `reach` pixels and bp pixels to a Breeze pixel. At rest it's the
/// button colour with a 1 bp border. Lit, it gets an accent tint and border and glows. Held,
/// it's solid accent and glows.
pub(crate) fn button(t: &Tokens, acc: &Accent, look: Look, d: f64, reach: f64, bp: f64) -> ([f64; 3], f64) {
    let inside = (0.5 - d).clamp(0.0, 1.0);
    let glow = (acc.line, if look != Look::Rest && d > 0.0 { acc.glow * (1.0 - d / reach).max(0.0).powi(2) } else { 0.0 });
    let half = (0.5 * bp).max(0.5);
    let (fill, edge) = match look {
        Look::Rest => (t.raised, t.border),
        Look::Lit => (over((t.raised, 1.0), acc.fill).0, acc.line),
        Look::Carry => (acc.line, acc.line),
    };
    over(over(glow, (fill, inside)), (edge, line(d + half, half)))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Glyph {
    Curve, // a screen bent toward you, seen from above
    Zoom,  // a magnifier
    Snap,  // back to the aligned place: an arrow curling back
    Align, // align again: a target
    Face,  // face me: an eye
}

/// A round button at (x, y) pixels from its centre, r pixels in radius, bp pixels to a Breeze
/// pixel. It's the bar's handle made round (Kirigami's RoundButton), with its glyph stroked
/// 1.5 bp in the text colour, and it lights up or goes solid the same way the bar does.
#[allow(clippy::too_many_arguments)]
pub fn knob_pixel(t: &Tokens, acc: &Accent, look: Look, x: f64, y: f64, r: f64, glyph: Glyph, bp: f64) -> ([f64; 3], f64) {
    let d = x.hypot(y) - r; // < 0 inside
    let stroke = (0.75 * bp).max(0.6);
    let glyph_a = match glyph {
        // an arc with its ends up (the lower half of a circle)
        Glyph::Curve => {
            let big = 0.8 * r;
            let arc = (x.hypot(y - big + 0.25 * r) - big).abs();
            if x.abs() < 0.55 * r && y < 0.2 * r { line(arc, stroke) } else { 0.0 }
        }
        // a ring up and left, with its handle going down and right
        Glyph::Zoom => {
            let (cx, cy, rr) = (-0.12 * r, 0.12 * r, 0.3 * r);
            let ring = line((x - cx).hypot(y - cy) - rr, stroke);
            let t = ((x - cx) - (y - cy)) / 2f64.sqrt(); // along the handle's diagonal
            let off = ((x - cx) + (y - cy)) / 2f64.sqrt();
            let handle = if t > rr && t < rr + 0.32 * r { line(off, stroke * 1.2) } else { 0.0 };
            ring.max(handle)
        }
        // three quarters of a ring, open at the top right, with an arrowhead on that end
        Glyph::Snap => {
            let rr = 0.38 * r;
            let a = y.atan2(x); // y is up, so the gap runs from 0 to 90 degrees
            let ring = if !(0.0..std::f64::consts::FRAC_PI_2).contains(&a) { line(x.hypot(y) - rr, stroke) } else { 0.0 };
            let tip = [rr, 0.0]; // the ring's end at 0 degrees, with the head pointing down (clockwise)
            let seg = |ax: f64, ay: f64, bx: f64, by: f64| {
                let (dx, dy) = (bx - ax, by - ay);
                let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                line((x - ax - t * dx).hypot(y - ay - t * dy), stroke)
            };
            let h = 0.2 * r;
            ring.max(seg(tip[0] - h, tip[1] + h, tip[0], tip[1])).max(seg(tip[0] + h, tip[1] + h, tip[0], tip[1]))
        }
        // a ring with a dot, the kind of target align looks for
        Glyph::Align => line(x.hypot(y) - 0.36 * r, stroke).max(((0.1 * r - x.hypot(y)) + 0.5).clamp(0.0, 1.0)),
        // an eye: a lens shape (where two circles overlap) around a pupil
        Glyph::Face => {
            let (rr, c) = (0.5 * r, 0.3 * r);
            let lens = (x.hypot(y - c) - rr).max(x.hypot(y + c) - rr);
            line(lens, stroke).max(((0.12 * r - x.hypot(y)) + 0.5).clamp(0.0, 1.0))
        }
    };
    let ink = if look == Look::Carry { acc.ink } else { t.text };
    over(button(t, acc, look, d, r, bp), (ink, glyph_a))
}

/// Shows an RGBA picture on h through its GPU buffers (gpu::show_raw), falling back to SetOverlayRaw.
pub fn set_raw(h: vr::Handle, px: &[u8], w: usize, ht: usize) -> bool {
    if crate::gpu::show_raw(h, px, w, ht) {
        return true;
    }
    match vr::timed(|| format!("SetOverlayRaw {w}x{ht}"), || call!(ov, SetOverlayRaw, h, px.as_ptr() as *mut _, w as u32, ht as u32, 4)) {
        0 => true,
        e => {
            eprintln!("controls: SteamVR refused a {w}x{ht} texture: {}", vr::error_name(e));
            false
        }
    }
}

/// Creates a panel's card overlay. It gets drawn on its first update.
pub fn create(name: &str) -> vr::Handle {
    let h = vr::create_overlay(&format!("controlcenter.card.{name}"), &format!("Command Center card {name}")).unwrap_or(0);
    // Sort order puts it under its picture. That's set along with the panels' paint order (paint_order).
    call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
    call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
    call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_SendVRDiscreteScrollEvents, true);
    call!(ov, SetOverlayAlpha, h, 0.0);
    call!(ov, ShowOverlay, h);
    h
}

/// The controls' size from the panel's width and distance: never under about 1.7 degrees, so
/// they stay easy to hit on a small or far panel. Returns the bar's width and the card's frame, which is 1.4 grips
/// so the corners are easy to hit.
fn chrome(pl: &Placement) -> (f64, f64) {
    let head = vr::head_position();
    let dist = norm(&[pl.c[0] - head[0], pl.c[1] - head[1], pl.c[2] - head[2]]).max(0.2);
    let least = dist * 0.03;
    let bar = (0.012 * dist * pl.width).sqrt().clamp(least, least.max(pl.width * 0.5));
    (bar, 1.4 * (bar * 0.13).max(dist * 0.018))
}

// ------------------------------------------------------------------ carrying and resizing

#[derive(Clone, Copy)]
enum Mode {
    Move { rel: Mat },                             // device -> panel
    Resize { sx: f64, sy: f64, gx: f64, gy: f64 }, // the corner, and where on it it was grabbed
    Stretch { gx: f64, gy: f64, free: bool },      // the bottom-right corner with the top-left fixed; free means any shape (a window's)
    Curve { local: [f64; 3], z0: f64, s0: f64 },  // the curve button: the point grabbed rides on the device
    Zoom { local: [f64; 3], z0: f64, zoom0: f64 }, // the zoom button: same idea
}

struct Drag {
    dev: u32,
    mode: Mode,
    lost: Option<Instant>,
}

/// What a panel's card looks like right now, as uploaded and placed.
struct Drawn {
    spec: CardSpec,  // as drawn (patched in place for a grip or the bar's look)
    dims: [i64; 3],  // the (w, h, g) in mm it was drawn for
    card_at: Instant,
    placed: Option<(Mat, i64, i64, i64)>, // the panel's matrix, width, height and curve (mm) it was placed for
    tag: u32,                        // the panel's tag version (its change count) it was drawn with
    theme: u32,                      // the theme it was drawn with (theme::generation)
}

pub struct Grab {
    drags: Vec<Option<Drag>>,
    near: Vec<Option<Instant>>, // when a laser was last on the panel or its controls
    focus: Vec<u8>,             // a laser's on the panel's picture (1) or its card (2), so it stays near
    fade: Vec<f32>,
    hover: Vec<Option<Part>>, // the part a laser is on
    drawn: Vec<Option<Drawn>>,
    hidden: bool, // every panel's hidden, so their controls are too
    shown: Vec<bool>, // each panel's controls are shown (not while hidden, and not for an empty window slot)
    cards: Vec<Vec<u8>>, // each card's texture as uploaded (patched in place for a grip)
    turned: Vec<bool>,   // each card's texture went up rotated a quarter turn (its panel curves top to bottom)
    painter: Painter,    // full card draws, on their own thread
    pending: Vec<Option<(u64, Drawn)>>, // the full draw each card is waiting for: its request's number, and what it'll be
    done: Vec<Option<(u64, Vec<u8>)>>,  // each card's latest texture back from the painter, and its request's number
    order: Vec<usize>, // the panels from farthest to nearest, as last painted
    pressed: Vec<(usize, Ctl)>, // window controls pressed, for windows.rs to act on
    dim: vr::Handle,             // theater mode's backdrop: dark, in front of your eyes, painted just under the panel
    dim_alpha: f32,
    dimmed: Vec<bool>,           // faded out because another panel is in theater mode
    theater: usize,              // the panel in theater mode as last painted (MAX means none)
    dim_gen: u32,                // the theme the backdrop was painted for
    painted: Option<Instant>,    // when paint_order last ran (None: run it now)
    moving: bool,                // something moved or faded this frame, so main.rs keeps the display's rate
    pub released: Vec<u32>, // devices whose button came up on a panel or card this frame (a hold that started on the taskbar)
    extra: Option<Extra>,   // the window in the slot after the panels', while it's open
    scanned: Vec<Option<Pose>>, // each panel's aligned place (home.json "scanned"), reread every second
    scanned_read: Option<Instant>,
    stale: Vec<Option<Pose>>, // those places as of the last relocalization; if they're still the same, they moved with the room
    aligns: Vec<usize>,       // align buttons pressed after a relocalization, for machines.rs
    backs: crate::back::Backs, // panels turned away from you show their backs (back.rs)
    pub game: bool,            // a VR game is running (the taskbar knows), so no backs
}

/// A window that isn't a panel (Machines, Preferences) but gets a card like theirs. It takes
/// the last slot of Grab's lists. It's not in Kvm, since its place lives here, and not in
/// paint_order either, since its sort orders are fixed.
struct Extra {
    name: &'static str, // its spot: spots.home.<name>
    open: &'static AtomicBool, // its window's open flag; the card's close clears it
    win: vr::Handle,
    card: vr::Handle,
    accent: Rgb,
    pl: Placement,
    shown: bool,
}

impl Extra {
    /// Moves it to m, w wide (keeping its shape) and bent to curve.
    fn place(&mut self, m: &Mat, w: f64, curve: f64) {
        self.pl = Placement::from_matrix(m, w, self.pl.height / self.pl.width, curve);
        call!(ov, SetOverlayWidthInMeters, self.win, w as f32);
        call!(ov, SetOverlayCurvature, self.win, crate::taskbar::curvature(w, curve));
        vr::place(self.win, m);
    }
}

/// Whose card it is, for drawing and polling: a panel's or the Extra's.
struct Owner {
    name: &'static str,
    card: vr::Handle,
    overlay: vr::Handle,
    accent: Rgb,
    tag: (u32, Option<Arc<TagImg>>),
    live: bool,
    away: bool,
    window: bool, // a window panel, so any shape and it can zoom
    ctls: usize,  // how many controls it has, counted from the right
}

/// Theater mode's backdrop: an opaque dark square 8 m wide, 2 m in front of the headset. It
/// moves with your head so it always fills your view (about ±63°), and it stays hidden until
/// theater mode needs it. Its colour gets repainted for each theme (`paint_backdrop`).
fn backdrop() -> vr::Handle {
    let h = vr::create_overlay("controlcenter.theater", "Command Center theater").unwrap_or(0);
    call!(ov, SetOverlayWidthInMeters, h, 8.0);
    let mut t = sys::HmdMatrix34_t { m: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, -2.0]] };
    call!(ov, SetOverlayTransformTrackedDeviceRelative, h, sys::k_unTrackedDeviceIndex_Hmd as u32, &mut t);
    call!(ov, SetOverlayAlpha, h, 0.0);
    h
}

/// The backdrop's colour: the scheme's window colour pushed close to black, so theater stays
/// a dark room even on a light scheme.
fn backdrop_colour(t: &Tokens) -> [u8; 4] {
    let c = mix(t.surface, [0.0; 3], 0.85);
    [c[0] as u8, c[1] as u8, c[2] as u8, 255]
}

fn paint_backdrop(h: vr::Handle) {
    let mut px = backdrop_colour(&theme::get().win).repeat(16);
    call!(ov, SetOverlayRaw, h, px.as_mut_ptr() as *mut _, 4, 4, 4);
}

/// Every tracked device's pose in standing space.
fn poses() -> Vec<sys::TrackedDevicePose_t> {
    let n = sys::k_unMaxTrackedDeviceCount as usize;
    let mut all: Vec<sys::TrackedDevicePose_t> = vec![unsafe { std::mem::zeroed() }; n];
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, 0.0, all.as_mut_ptr(), n as u32);
    all
}

/// The mouse, as a device number just past SteamVR's (kvm.rs card_mouse). Its pose and laser
/// are kvm's ray as of the last poll.
pub const MOUSE: u32 = sys::k_unMaxTrackedDeviceCount as u32;
static MOUSE_RAY: Mutex<Option<Mat>> = Mutex::new(None);

pub fn pose_of(dev: u32) -> Option<Mat> {
    if dev == MOUSE {
        return *MOUSE_RAY.lock().unwrap();
    }
    let p = *poses().get(dev as usize)?;
    p.bPoseIsValid.then_some(p.mDeviceToAbsoluteTracking.m)
}

/// Where a device's laser meets the panel, in the device's own frame so it rides along like a
/// carried panel, and how far in front of the panel's middle that point is.
pub fn taken(pl: &Placement, dev: u32) -> Option<([f64; 3], f64)> {
    let (l, d) = (laser_of(dev)?, pose_of(dev)?);
    let o = [l[0][3] as f64, l[1][3] as f64, l[2][3] as f64];
    let dir = [-l[0][2] as f64, -l[1][2] as f64, -l[2][2] as f64];
    let (t, _, _) = pl.hit(&o, &dir)?;
    let at = [0, 1, 2].map(|r| o[r] + dir[r] * t);
    Some((to_device(&d, &at), dot(&[at[0] - pl.c[0], at[1] - pl.c[1], at[2] - pl.c[2]], &pl.z)))
}

/// How far a point fixed to a device (from `taken`) is in front of the panel's middle now.
pub fn depth_of(pl: &Placement, m: &Mat, local: &[f64; 3]) -> f64 {
    let at = [0, 1, 2].map(|r| m[r][0] as f64 * local[0] + m[r][1] as f64 * local[1] + m[r][2] as f64 * local[2] + m[r][3] as f64);
    dot(&[at[0] - pl.c[0], at[1] - pl.c[1], at[2] - pl.c[2]], &pl.z)
}

/// A device's laser (vr::laser_pose). For the mouse, the laser is just its pose.
fn laser_of(dev: u32) -> Option<Mat> {
    if dev == MOUSE { pose_of(dev) } else { vr::laser_pose(dev) }
}

/// A world point in a device's own frame. This works because its pose is rigid.
fn to_device(d: &Mat, p: &[f64; 3]) -> [f64; 3] {
    let q = [0, 1, 2].map(|r| p[r] - d[r][3] as f64);
    [0, 1, 2].map(|c| (0..3).map(|r| d[r][c] as f64 * q[r]).sum())
}

/// Where a device's laser meets the panel's surface, extended past its edges, as (u, v).
pub fn laser_on(pl: &Placement, dev: u32) -> Option<(f64, f64)> {
    let l = laser_of(dev)?;
    let o = [l[0][3] as f64, l[1][3] as f64, l[2][3] as f64];
    let d = [-l[0][2] as f64, -l[1][2] as f64, -l[2][2] as f64];
    pl.hit(&o, &d).map(|(_, u, v)| (u, v))
}

/// A corner's new width from where the laser puts it. The corner moves along the panel's
/// diagonal and the centre stays put, so the panel keeps its aspect and doesn't wander.
pub fn width_for(sx: f64, sy: f64, cu: f64, cv: f64, aspect: f64) -> f64 {
    2.0 * (sx * cu + aspect * sy * cv) / (1.0 + aspect * aspect)
}

/// Puts the bottom-right corner at (cu, cv) (from the centre, along the surface) while the
/// top-left stays where it is, and returns the panel's new matrix, width, height and curve.
/// With `aspect` (height per width) it keeps its shape and the corner moves along the diagonal
/// from the top-left. With None it takes any shape up to `max` (w, h), which is how a window
/// works, as big as the output. The new panel sits on the cylinder through the top-left, or a
/// new cylinder when the curve grows with the width.
fn stretched(pl: &Placement, cu: f64, cv: f64, aspect: Option<f64>, max: (f64, f64)) -> (Mat, f64, f64, f64) {
    let (du, dv) = (cu + pl.width / 2.0, pl.height / 2.0 - cv); // from the top-left
    let (w, h) = match aspect {
        Some(a) => {
            let w = ((du + a * dv) / (1.0 + a * a)).clamp(MIN_WIDTH, MAX_WIDTH);
            (w, a * w)
        }
        None => (du.clamp(MIN_WIDTH, max.0.clamp(MIN_WIDTH, MAX_WIDTH)), dv.clamp(MIN_WIDTH, max.1.clamp(MIN_WIDTH, MAX_WIDTH))),
    };
    let curve = curve_for_width(pl.curve, w);
    let top_left = pl.on_surface(-pl.width / 2.0, pl.height / 2.0, 0.0);
    (Placement::from_matrix(&top_left, w, h / w, curve).on_surface(w / 2.0, -h / 2.0, 0.0), w, h, curve)
}

impl Grab {
    pub fn new(n: usize) -> Grab {
        let n = n + 1; // plus the Extra's slot
        Grab { drags: (0..n).map(|_| None).collect(), near: vec![None; n], focus: vec![0; n], fade: vec![0.0; n], hover: vec![None; n], drawn: (0..n).map(|_| None).collect(), hidden: false, shown: vec![true; n], cards: vec![Vec::new(); n], turned: vec![false; n], painter: Painter::new(), pending: (0..n).map(|_| None).collect(), done: vec![None; n], order: Vec::new(), pressed: Vec::new(), dim: backdrop(), dim_alpha: 0.0, dimmed: vec![false; n], theater: usize::MAX, dim_gen: u32::MAX, painted: None, moving: false, released: Vec::new(), extra: None, scanned: Vec::new(), scanned_read: None, stale: Vec::new(), aligns: Vec::new(), backs: Default::default(), game: false }
    }

    /// The Extra's slot, right after the panels'.
    pub fn slot(&self) -> usize {
        self.drags.len() - 1
    }

    /// Who owns slot i's card. None for the Extra's slot while it's closed.
    fn owner(&self, i: usize) -> Option<Owner> {
        if let Some(p) = panels().get(i) {
            let window = matches!(p.src, Source::Window(_));
            let ctls = if window { CTLS } else if p.v.pop.is_some() { 1 } else { 0 }; // a pop-out gets just close (popout.rs)
            return Some(Owner { name: &p.v.name, card: p.card, overlay: p.overlay, accent: p.accent, tag: p.tag(), live: p.live(), away: p.away(), window, ctls });
        }
        let x = self.extra.as_ref().filter(|_| i == self.slot())?;
        Some(Owner { name: x.name, card: x.card, overlay: x.win, accent: x.accent, tag: (0, None), live: true, away: !x.shown, window: false, ctls: 1 })
    }

    /// Where slot i's owner is.
    fn at(&self, i: usize, k: &Kvm) -> Placement {
        match &self.extra {
            Some(x) if i == self.slot() => x.pl,
            _ => k.place[i],
        }
    }

    /// Moves slot i's owner to m, w wide and bent to curve. h is its height; None keeps its shape, and the Extra always keeps its shape.
    fn put(&mut self, i: usize, k: &mut Kvm, m: &Mat, w: f64, h: Option<f64>, curve: f64) {
        if i == self.slot() {
            if let Some(x) = self.extra.as_mut() {
                x.place(m, w, curve);
            }
            return;
        }
        match h {
            Some(h) => k.set_place(i, m, w, h, curve),
            None => k.set_pose(i, m, w, curve),
        }
    }

    /// Whether another window has the Extra's slot. It's one at a time, so opening one closes the other.
    pub fn extra_taken(&self) -> bool {
        self.extra.is_some()
    }

    /// Gives a window a card like a panel's (`Extra`) at pl. It's placed right away and its card
    /// is made under it with sort order `sort`. The card's close stores false in `open`. The slot
    /// has to be free first (extra_taken).
    pub fn open_extra(&mut self, name: &'static str, win: vr::Handle, sort: u32, accent: Rgb, pl: Placement, open: &'static AtomicBool) {
        let card = create(name);
        call!(ov, SetOverlaySortOrder, card, sort);
        let mut x = Extra { name, open, win, card, accent, pl, shown: false }; // its owner's first tick shows it (set_extra_shown)
        x.place(&pl.matrix(), pl.width, pl.curve);
        self.extra = Some(x);
    }

    /// Destroys name's card and clears its slot. The window itself is the caller's to close. If another window has the slot, it does nothing.
    pub fn close_extra(&mut self, name: &str) {
        let Some(x) = self.extra.take_if(|x| x.name == name) else { return };
        crate::gpu::forget(x.card);
        call!(ov, DestroyOverlay, x.card);
        let i = self.slot();
        (self.drags[i], self.drawn[i], self.near[i], self.hover[i], self.fade[i], self.focus[i]) = (None, None, None, None, 0.0, 0);
        (self.pending[i], self.done[i], self.shown[i], self.dimmed[i]) = (None, None, true, false);
        self.painter.jobs.cancel(i);
    }

    /// A recall (control.rs `place`): moves the Extra's window to m, w wide and bent to curve,
    /// and ends any carry. If it's closed (or the slot is another window's) it does nothing,
    /// since it'll open where it was saved anyway.
    pub fn place_extra(&mut self, name: &str, m: &Mat, w: f64, curve: f64) {
        let Some(x) = self.extra.as_mut().filter(|x| x.name == name) else { return };
        x.place(m, w, curve);
        let i = self.slot();
        self.drags[i] = None;
    }

    /// Gives the Extra's window a new shape (height over width, from machines.rs's rows or its
    /// form), keeping its middle and width. Its card follows through draw's resize.
    pub fn reshape_extra(&mut self, aspect: f64) {
        if let Some(x) = self.extra.as_mut() {
            x.pl = Placement::from_matrix(&x.pl.matrix(), x.pl.width, aspect, x.pl.curve);
        }
    }

    /// Shows or hides the Extra's window. Its card follows.
    pub fn set_extra_shown(&mut self, shown: bool) {
        if let Some(x) = self.extra.as_mut() {
            x.shown = shown;
        }
    }

    /// An event on panel i's own picture. A laser there brings its controls in. A button
    /// release ends that device's drags, since it may be over another panel by then, and
    /// returns true if it ended any.
    pub fn panel_event(&mut self, i: usize, e: &vr::VREvent_t, k: &mut Kvm) -> bool {
        match e.eventType {
            sys::EVREventType_VREvent_MouseMove | sys::EVREventType_VREvent_FocusEnter => {
                self.touch(i, 1, true);
                false
            }
            sys::EVREventType_VREvent_FocusLeave => {
                self.touch(i, 1, false);
                false
            }
            sys::EVREventType_VREvent_MouseButtonUp => self.end_by(e.trackedDeviceIndex, k),
            _ => false,
        }
    }

    /// A laser came onto (or moved on) slot i's picture or card (`bit` is focus's bit), or left
    /// it. The slot counts as near while a laser's on either one, even resting, because a still
    /// mouse or a steady hand sends no moves (HOLD at most). After it's off both, it stays near
    /// for LINGER.
    fn touch(&mut self, i: usize, bit: u8, on: bool) {
        if on { self.focus[i] |= bit } else { self.focus[i] &= !bit }
        let until = Instant::now() + if self.focus[i] != 0 { HOLD } else { LINGER };
        self.near[i] = Some(if on { until } else { self.near[i].map_or(until, |t| t.min(until)) });
    }

    /// The part of panel i's card under an overlay mouse event. The card's mouse scale is its
    /// size in metres, measured from the bottom left.
    fn card_part(&self, i: usize, e: &vr::VREvent_t, k: &Kvm) -> Option<Part> {
        let pl = &self.at(i, k);
        let spec = &self.drawn[i].as_ref()?.spec;
        let g = spec.g;
        let fw = pl.width + 2.0 * g;
        let m = unsafe { e.data.mouse }; // metres from the card's bottom left
        // on a turned card (its panel curves top to bottom), the event's bottom left is the card's bottom right
        let (mx, my) = if pl.vert { (fw - m.y as f64, m.x as f64) } else { (m.x as f64, m.y as f64) };
        let (u, v) = (mx - fw / 2.0, my - (pl.height / 2.0 + g + spec.bottom()));
        held_part(spec, pl.width, pl.height, i == self.slot(), self.hover[i], u, v)
    }

    /// Handles events on every card and on the backs. A press on a back carries the panel, so you can turn it round.
    pub fn poll(&mut self, k: &mut Kvm) {
        for (i, dev, down) in self.backs.events() {
            if down { self.start(i, Part::Bar, dev, k) } else { self.end_by(dev, k); }
        }
        *MOUSE_RAY.lock().unwrap() = k.awake.then(|| k.ray());
        // The mouse's presses on a card with no laser (kvm.rs card_mouse) land where its ray meets the card.
        for (i, down) in std::mem::take(&mut k.card_mouse) {
            if !down {
                self.end_by(MOUSE, k);
                continue;
            }
            let i = if i == usize::MAX { self.slot() } else { i };
            let pl = self.at(i, k);
            let part = self.drawn[i].as_ref().zip(laser_on(&pl, MOUSE)).and_then(|(d, (u, v))| held_part(&d.spec, pl.width, pl.height, i == self.slot(), self.hover[i], u, v));
            eprintln!("{}: card pressed by the mouse on {part:?}", self.owner(i).map_or("", |o| o.name));
            crate::windows::shell_outside();
            self.press(i, part, MOUSE, k);
        }
        let notches = std::mem::take(&mut k.card_wheel);
        if let Some(i) = (notches != 0.0).then(|| self.drags.iter().position(|d| d.as_ref().is_some_and(|d| d.dev == MOUSE))).flatten() {
            self.scroll(i, notches, k);
        }
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        for i in 0..self.drags.len() {
            let Some(p) = self.owner(i) else { continue };
            {
                while call!(ov, PollNextOverlayEvent, p.card, &mut e, size_of::<vr::VREvent_t>() as u32) {
                    let part = self.card_part(i, &e, k);
                    match e.eventType {
                        sys::EVREventType_VREvent_MouseMove | sys::EVREventType_VREvent_FocusEnter => {
                            self.touch(i, 2, true);
                            self.hover[i] = part;
                        }
                        sys::EVREventType_VREvent_FocusLeave => {
                            self.touch(i, 2, false);
                            self.hover[i] = None;
                        }
                        sys::EVREventType_VREvent_MouseButtonDown if unsafe { e.data.mouse.button } == sys::EVRMouseButton_VRMouseButton_Left => {
                            eprintln!("{}: card pressed by device {} on {part:?}", p.name, e.trackedDeviceIndex);
                            crate::windows::shell_outside(); // a click outside Plasma's open popup
                            if vr::is_real_controller(e.trackedDeviceIndex) && k.awake {
                                k.set_awake(false); // whichever device was pressed last is primary (R-2)
                            }
                            self.press(i, part, e.trackedDeviceIndex, k);
                        }
                        sys::EVREventType_VREvent_MouseButtonUp => {
                            self.end_by(e.trackedDeviceIndex, k);
                        }
                        sys::EVREventType_VREvent_ScrollDiscrete => self.scroll(i, unsafe { e.data.scroll.ydelta } as f64, k),
                        _ => {}
                    }
                }
            }
        }
    }

    /// The wheel (or joystick) on slot i while it's held. A carry gets pushed away or pulled
    /// closer, a bend gets 3 cm more (or less) curve a notch, and a zoom changes 10% a notch.
    fn scroll(&mut self, i: usize, notches: f64, k: &mut Kvm) {
        match self.drags[i].as_mut().map(|d| &mut d.mode) {
            Some(Mode::Move { .. }) => self.push(i, notches, k),
            Some(Mode::Curve { s0, .. }) => *s0 = (*s0 + 0.03 * notches).max(0.0),
            Some(Mode::Zoom { zoom0, .. }) => *zoom0 = (*zoom0 * 1.1f64.powf(notches)).clamp(0.5, 3.0),
            _ => {}
        }
    }

    /// A press on a part of slot i's card: either a button, or the start of a drag on dev.
    fn press(&mut self, i: usize, part: Option<Part>, dev: u32, k: &mut Kvm) {
        match part {
            // the Extra's close; its window does the closing on its own tick
            Some(Part::Ctl(_)) if i == self.slot() => {
                if let Some(x) = &self.extra {
                    x.open.store(false, Relaxed);
                }
            }
            Some(Part::Ctl(c)) => self.pressed.push((i, CTL[c])),
            Some(Part::Snap) => self.snap(i, k),
            Some(Part::Face) => self.face(i, k),
            Some(part) => self.start(i, part, dev, k),
            None => {}
        }
    }

    fn start(&mut self, i: usize, part: Part, dev: u32, k: &Kvm) {
        let pl = &self.at(i, k);
        let mode = match part {
            // theater mode has its own size, so there this corner works like the others
            Part::Corner(BOTTOM_RIGHT) if crate::THEATER.load(Relaxed) != i => {
                let Some((u, v)) = laser_on(pl, dev) else { return };
                Mode::Stretch { gx: u - pl.width / 2.0, gy: v + pl.height / 2.0, free: self.owner(i).is_some_and(|o| o.window) }
            }
            Part::Corner(c) => {
                let (sx, sy) = CORNER_SIGN[c];
                let Some((u, v)) = laser_on(pl, dev) else { return };
                Mode::Resize { sx, sy, gx: u - sx * pl.width / 2.0, gy: v - sy * pl.height / 2.0 }
            }
            Part::Knob => {
                // The edges come toward you as far as the point you grabbed does.
                let Some((local, z0)) = taken(pl, dev) else { return };
                let s0 = if pl.curve > 0.0 { pl.curve * (1.0 - (pl.width / (2.0 * pl.curve)).cos()) } else { 0.0 };
                Mode::Curve { local, z0, s0 }
            }
            Part::Ctl(_) | Part::Snap | Part::Face => return, // buttons, not drags (poll handles them)
            Part::Zoom => {
                let Some((local, z0)) = taken(pl, dev) else { return };
                Mode::Zoom { local, z0, zoom0: panels()[i].zoom() }
            }
            Part::Bar | Part::Edge => {
                let Some(d) = pose_of(dev) else { return };
                Mode::Move { rel: mul(&inv_rigid(&d), &pl.matrix()) }
            }
        };
        let what = match mode {
            Mode::Resize { .. } | Mode::Stretch { .. } => "resized",
            Mode::Curve { .. } => "curved",
            Mode::Zoom { .. } => "zoomed",
            Mode::Move { .. } => "carried",
        };
        self.drags[i] = Some(Drag { dev, mode, lost: None });
        eprintln!("{}: {what} by device {dev}", self.owner(i).map_or("", |o| o.name));
    }

    /// Letting go: the panel stays where it is and its new place is saved into "home".
    fn end(&mut self, i: usize, k: &Kvm) {
        if self.drags[i].take().is_some() {
            self.save(i, k);
        }
    }

    /// Saves slot i's place into "home", same as releasing a carry does.
    fn save(&self, i: usize, k: &Kvm) {
        let pl = self.at(i, k);
        if crate::THEATER.load(Relaxed) == i {
            return; // theater mode's place is just for now, not its spot
        }
        let (name, saved) = match panels().get(i) {
            Some(p) => (p.v.name.as_str(), p.save_spot(&pl)),
            None => {
                let Some(x) = &self.extra else { return };
                (x.name, crate::config::save_home_pose(x.name, None, &pl.pose(), pl.height, None))
            }
        };
        match saved {
            Ok(()) => eprintln!("{name}: placed ({:.2} m wide), saved to home", pl.width),
            Err(e) => eprintln!("{name}: placed, but saving home failed: {e}"),
        }
    }

    /// A device's button came up somewhere (the taskbar's frame counts too), so whatever it was
    /// carrying or resizing gets put down.
    pub fn end_by(&mut self, dev: u32, k: &Kvm) -> bool {
        self.released.push(dev);
        let mut any = false;
        for i in 0..self.drags.len() {
            if self.drags[i].as_ref().is_some_and(|d| d.dev == dev) {
                self.end(i, k);
                any = true;
            }
        }
        any
    }

    /// An aligned monitor's snap button. It puts the monitor back in its aligned place and saves
    /// that as home, like a move. After a relocalization it asks for an align of that monitor
    /// instead, which machines.rs runs (take_aligns).
    fn snap(&mut self, i: usize, k: &mut Kvm) {
        match self.snap_glyph(i, &k.place[i]) {
            Some(Glyph::Align) => self.aligns.push(i),
            Some(_) => {
                let (Some(pose), p) = (self.scanned[i], &panels()[i]) else { return };
                self.drags[i] = None;
                p.set_vert(pose.vert); // curved top to bottom means turned (kvm.rs set_place)
                k.set_pose(i, &panel_matrix(&pose), pose.width, pose.curve);
                match p.save_spot(&k.place[i]) {
                    Ok(()) => eprintln!("{}: back to its aligned place, saved to home", p.v.name),
                    Err(e) => eprintln!("{}: back to its aligned place, but saving home failed: {e}", p.v.name),
                }
            }
            None => {}
        }
    }

    /// The face-me button turns slot i in place to face the eye and saves it like a move.
    /// It's instant, since no move here eases.
    fn face(&mut self, i: usize, k: &mut Kvm) {
        let pl = self.at(i, k);
        self.drags[i] = None;
        self.put(i, k, &panel_matrix(&facing(&pl, &vr::head_position())), pl.width, Some(pl.height), pl.curve);
        self.save(i, k);
    }

    /// The snap button for panel i at pl. It's Align when a relocalization left its aligned
    /// place unchanged (so that place moved with the room), Snap when the panel's off that
    /// place, and otherwise none. Only aligned remote screens get one, and not in theater mode,
    /// since that place is just for now.
    fn snap_glyph(&self, i: usize, pl: &Placement) -> Option<Glyph> {
        let aligned = self.scanned.get(i).copied().flatten()?;
        if crate::THEATER.load(Relaxed) == i {
            return None;
        }
        if self.stale.get(i).copied().flatten() == Some(aligned) {
            return Some(Glyph::Align);
        }
        moved(&pl.pose(), &aligned).then_some(Glyph::Snap)
    }

    /// The Frame relocalized (main.rs), so every aligned place is off now until it's aligned again.
    pub fn relocalized(&mut self) {
        self.stale = self.scanned.clone();
    }

    /// Align buttons pressed since the last call (panel numbers).
    pub fn take_aligns(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.aligns)
    }

    /// The window controls pressed since the last call.
    pub fn take_pressed(&mut self) -> Vec<(usize, Ctl)> {
        std::mem::take(&mut self.pressed)
    }

    /// A spot recall or `place` takes the panel away from whoever's carrying it.
    pub fn cancel(&mut self, i: usize) {
        self.drags[i] = None;
    }

    /// Whether anything was carried, a laser or the cursor was near a panel, or a card or the
    /// theater backdrop was fading, as of the last update.
    pub fn moving(&self) -> bool {
        self.moving
    }

    /// Whether it's being carried or resized right now.
    pub fn busy(&self, i: usize) -> bool {
        self.drags[i].is_some()
    }

    /// Whether it's being resized by a corner or zoomed right now. The window follows a few times a second.
    pub fn resizing(&self, i: usize) -> bool {
        matches!(self.drags[i], Some(Drag { mode: Mode::Resize { .. } | Mode::Stretch { .. } | Mode::Zoom { .. }, .. }))
    }

    /// Whether a window panel is being stretched to any shape right now. Its height is the
    /// dragged one, not its window's (yet).
    pub fn stretching(&self, i: usize) -> bool {
        matches!(self.drags[i], Some(Drag { mode: Mode::Stretch { free: true, .. }, .. }))
    }

    /// Joystick or wheel while carrying pushes it away or pulls it closer along the line from
    /// the head, 8% a notch.
    fn push(&mut self, i: usize, notches: f64, k: &mut Kvm) {
        let Some(Drag { dev, mode: Mode::Move { rel }, .. }) = self.drags[i].as_mut() else { return };
        let Some(d) = pose_of(*dev) else { return };
        let head = vr::head_position();
        let mut p = mul(&d, rel);
        let to = [0, 1, 2].map(|r| p[r][3] as f64 - head[r]);
        let len = norm(&to);
        let next = (len * (1.0 + 0.08 * notches)).clamp(0.25, 5.0);
        for r in 0..3 {
            p[r][3] = (head[r] + to[r] / (len + 1e-9) * next) as f32;
        }
        *rel = mul(&inv_rigid(&d), &p);
        let pl = self.at(i, k);
        self.put(i, k, &p, pl.width, None, pl.curve);
    }

    /// Runs every frame. Carried panels follow their device and resized ones follow their
    /// laser, then the bar and card follow their panel, fade, and take their look.
    pub fn update(&mut self, k: &mut Kvm) {
        let now = Instant::now();
        let all = poses();
        self.moving = self.drags.iter().any(Option::is_some);
        for i in 0..self.drags.len() {
            let Some(drag) = self.drags[i].as_mut() else { continue };
            let tracked = if drag.dev == MOUSE { pose_of(MOUSE) } else { all.get(drag.dev as usize).filter(|p| p.bPoseIsValid).map(|p| p.mDeviceToAbsoluteTracking.m) };
            if tracked.is_none() {
                if drag.lost.is_some_and(|t| now - t > LOST) {
                    self.end(i, k);
                } else {
                    drag.lost = drag.lost.or(Some(now));
                }
                continue;
            }
            drag.lost = None;
            let (dev, mode) = (drag.dev, drag.mode);
            let pl = self.at(i, k);
            match mode {
                Mode::Move { rel } => {
                    let m = mul(&tracked.unwrap(), &rel);
                    self.put(i, k, &m, pl.width, None, pl.curve);
                }
                Mode::Resize { sx, sy, gx, gy } => {
                    let Some((u, v)) = laser_on(&pl, dev) else { continue };
                    let aspect = pl.height / pl.width;
                    let width = width_for(sx, sy, u - gx, v - gy, aspect).clamp(MIN_WIDTH, MAX_WIDTH);
                    if (width - pl.width).abs() > 1e-4 {
                        self.put(i, k, &pl.matrix(), width, None, curve_for_width(pl.curve, width));
                    }
                }
                Mode::Stretch { gx, gy, free } => {
                    let Some((u, v)) = laser_on(&pl, dev) else { continue };
                    let max = if free { crate::windows::max_size(&panels()[i]) } else { (MAX_WIDTH, MAX_WIDTH) };
                    let (m, w, h, curve) = stretched(&pl, u - gx, v - gy, (!free).then(|| pl.height / pl.width), max);
                    if (w - pl.width).abs() > 1e-4 || (h - pl.height).abs() > 1e-4 {
                        self.put(i, k, &m, w, free.then_some(h), curve);
                    }
                }
                Mode::Zoom { local, z0, zoom0 } => {
                    // Pull toward you for bigger text (twice as big for 17 cm), push for smaller.
                    // The panel keeps its size and the window reflows (windows.rs).
                    let z = depth_of(&pl, &tracked.unwrap(), &local);
                    panels()[i].set_zoom(zoom0 * (4.0 * (z - z0)).exp());
                }
                Mode::Curve { local, z0, s0 } => {
                    // Pull the edge toward you to bend the panel, push it back to flatten.
                    let z = depth_of(&pl, &tracked.unwrap(), &local);
                    let curve = radius_for(s0 + z - z0, pl.width);
                    if (curve - pl.curve).abs() > 1e-3 {
                        self.put(i, k, &pl.matrix(), pl.width, None, curve);
                    }
                }
            }
        }
        // The Extra's place and its card's margins, for the mouse (kvm.rs land()). This is as
        // of now, whether it's open, carried, reshaped or closed.
        let slot = self.slot();
        k.extra = self.extra.as_ref().filter(|x| x.shown && !HIDDEN.load(Relaxed)).map(|x| {
            let m = self.drawn[slot].as_ref().map_or([chrome(&x.pl).1; 3], |d| [d.spec.g, d.spec.g + d.spec.bottom(), d.spec.g + d.spec.top()]);
            (x.pl, m)
        });
        // Same for each panel's card margins (kvm.rs card_hit).
        k.cards = k.place.iter().enumerate().map(|(i, pl)| self.drawn[i].as_ref().map_or([chrome(pl).1; 3], |d| [d.spec.g, d.spec.g + d.spec.bottom(), d.spec.g + d.spec.top()])).collect();
        // Reread the aligned places once a second, since an align or a workspace switch changes them.
        if self.scanned_read.is_none_or(|t| now - t > Duration::from_secs(1)) {
            self.scanned_read = Some(now);
            let keys: Vec<(&str, i32)> = panels().iter().map(|p| (p.v.name.as_str(), if matches!(p.src, Source::Rdp) { p.v.screen } else { -1 })).collect();
            self.scanned = crate::config::scanned_poses(&keys);
            for (s, p) in self.scanned.iter_mut().zip(panels()) {
                if !matches!(p.src, Source::Rdp) {
                    *s = None; // windows aren't aligned
                }
            }
        }
        for (i, seq, buf) in self.painter.done.try_iter() {
            self.done[i] = Some((seq, buf)); // keep only the latest, an older one is stale anyway
        }
        let hidden = HIDDEN.load(Relaxed);
        if hidden != self.hidden {
            self.hidden = hidden;
            if hidden {
                for i in 0..self.drags.len() {
                    self.end(i, k); // there's nothing left to release on, so put it down where it is
                }
            }
        }
        for i in 0..self.drags.len() {
            let Some(p) = self.owner(i) else { continue };
            // Hidden panels' controls shouldn't catch lasers, and neither should ones that are
            // away (minimized, or hidden for theater mode). An empty window slot has none.
            let show = !hidden && p.live && !p.away;
            if show != self.shown[i] {
                self.shown[i] = show;
                if show { call!(ov, ShowOverlay, p.card) } else { call!(ov, HideOverlay, p.card) };
                if !p.live {
                    (self.drags[i], self.drawn[i], self.near[i], self.hover[i], self.fade[i], self.focus[i]) = (None, None, None, None, 0.0, 0);
                    (self.pending[i], self.done[i]) = (None, None);
                    self.painter.jobs.cancel(i);
                }
            }
            if !p.live {
                continue;
            }
            // Our own cursor on a panel brings its controls in too, so the mouse can find them.
            // On the Extra it brings in the Extra's card, not the card of the panel behind it.
            let ours = k.awake && if i == self.slot() { k.on_extra } else { k.active == i && !k.on_extra };
            let want = !hidden && (self.drags[i].is_some() || self.near[i].is_some_and(|t| now < t) || ours);
            let before = self.fade[i];
            // Don't fade in over a stub (only a stub is at Rest). The fade waits for the full
            // card the painter is drawing, otherwise it'd be over before that's back.
            let stub = self.drawn[i].as_ref().is_none_or(|d| d.spec.look == Look::Rest);
            if !(want && stub) {
                self.fade[i] = (self.fade[i] + if want { FADE_IN } else { -FADE_OUT }).clamp(0.0, 1.0);
            }
            let first = self.drawn[i].is_none();
            let visible = want || self.fade[i] > 0.0;
            let seen = visible && self.shown[i]; // not while hidden or away, since its overlay is too
            self.draw(i, &p, &self.at(i, k), visible, seen, now);
            // When another panel's in theater mode, this one goes away along with its card.
            // SteamVR's sort order alone didn't keep nearer panels under the backdrop, and a
            // faded picture shows up as a violet sheet on the Frame. It comes back when theater
            // ends, unless it's minimized (its taskbar chip, or its v), which hides it the same way.
            let dim = p.away;
            if dim != self.dimmed[i] {
                self.dimmed[i] = dim;
                // the card too, because an invisible card still catches lasers (in front of the theater panel)
                if dim {
                    self.end(i, k); // put it down where it is, there's nothing left to release on
                    call!(ov, HideOverlay, p.overlay);
                    call!(ov, HideOverlay, p.card);
                } else if !hidden {
                    call!(ov, ShowOverlay, p.overlay);
                    call!(ov, ShowOverlay, p.card);
                }
            }
            self.moving |= want || self.fade[i] != before;
            if self.fade[i] != before || want || first || dim {
                call!(ov, SetOverlayAlpha, p.card, if dim { 0.0 } else { self.fade[i] });
            }
        }
        // Theater mode: the room and every other panel go dark behind it, like Steam's does,
        // fading in and out over about half a second.
        let theater = crate::THEATER.load(Relaxed);
        if self.dim_gen != theme::generation() {
            self.dim_gen = theme::generation();
            paint_backdrop(self.dim);
        }
        let target = if theater != usize::MAX && !hidden { 0.88 } else { 0.0 };
        if self.dim_alpha != target {
            self.moving = true;
            self.dim_alpha = if target > self.dim_alpha { (self.dim_alpha + 0.04).min(target) } else { (self.dim_alpha - 0.06).max(target) };
            call!(ov, SetOverlayAlpha, self.dim, self.dim_alpha);
            if self.dim_alpha > 0.0 { call!(ov, ShowOverlay, self.dim) } else { call!(ov, HideOverlay, self.dim) };
        }
        if theater != self.theater {
            self.theater = theater;
            self.order.clear(); // sort again now so the theater panel goes on top
            self.painted = None;
        }
        self.paint_order(k);
        let fronts: Vec<crate::back::Front> = (0..self.drags.len())
            .filter_map(|i| {
                // not owner(), since its tag (a lock and a clone) isn't needed here
                let (overlay, ok) = match panels().get(i) {
                    Some(p) => (p.overlay, p.live() && !p.away()),
                    None => self.extra.as_ref().filter(|_| i == self.slot()).map(|x| (x.win, x.shown))?,
                };
                Some(crate::back::Front { i, overlay, pl: self.at(i, k), ok: ok && !hidden && !self.game, held: self.drags[i].is_some() })
            })
            .collect();
        self.backs.tick(&fronts, &vr::head_position());
    }

    /// SteamVR doesn't depth-test overlays against each other, it just paints them in sort
    /// order. So panels get painted back to front, each as a group (its card, then its picture,
    /// then its bar), and a nearer panel and its frame cover a farther one. Which one's nearer is
    /// decided where they overlap in view (see `in_front`), not by their centres, because a wide
    /// curved panel's wing can be in front of a panel whose centre is nearer. This runs a few
    /// times a second, and the sort orders only change when the order does. The label and
    /// cursor stay on top.
    fn paint_order(&mut self, k: &Kvm) {
        // (by time, not ticks, because the main loop's rate changes, main.rs Pace)
        if self.painted.is_some_and(|t| t.elapsed() < Duration::from_millis(230)) {
            return;
        }
        self.painted = Some(Instant::now());
        let eye = vr::head_position();
        let n = panels().len();
        // once per panel, not per pair, since chrome() asks SteamVR where the head is
        let margin: Vec<f64> = (0..n).map(|i| chrome(&k.place[i]).1).collect();
        let live = |i: usize| panels()[i].live();
        // front[a][b]: a covers b where they overlap
        let front: Vec<Vec<bool>> =
            (0..n).map(|a| (0..n).map(|b| a != b && live(a) && live(b) && in_front(&k.place[a], margin[a], &k.place[b], margin[b], &eye)).collect()).collect();
        // Back to front: keep taking a panel that covers none of the rest. Ties and cycles
        // are broken by distance.
        let dist = |i: usize| {
            let c = k.place[i].c;
            norm(&[c[0] - eye[0], c[1] - eye[1], c[2] - eye[2]])
        };
        let mut left: Vec<usize> = (0..n).filter(|&i| live(i)).collect();
        left.sort_by(|&a, &b| dist(b).total_cmp(&dist(a)));
        let mut order = Vec::new();
        while !left.is_empty() {
            let pick = left.iter().position(|&a| left.iter().all(|&b| !front[a][b])).unwrap_or(0);
            order.push(left.remove(pick));
        }
        // In theater mode its panel goes last, the backdrop just under it, and everything else under that.
        let theater = crate::THEATER.load(Relaxed);
        if let Some(at) = order.iter().position(|&i| i == theater) {
            let t = order.remove(at);
            order.push(t);
        }
        if order == self.order {
            return;
        }
        if theater != usize::MAX {
            call!(ov, SetOverlaySortOrder, self.dim, 10 + (order.len() as u32 - 1) * 4 - 1);
        }
        for (rank, &i) in order.iter().enumerate() {
            let (p, base) = (&panels()[i], 10 + rank as u32 * 4);
            call!(ov, SetOverlaySortOrder, p.card, base);
            call!(ov, SetOverlaySortOrder, p.overlay, base + 1);
        }
        self.backs.resort();
        self.order = order;
    }

    /// Places, sizes and draws panel i's card (with its bar and tag) for where it is and how it
    /// should look. When nobody can see it (faded out, hidden or away), it's only a stub of its
    /// shape (`stub`).
    fn draw(&mut self, i: usize, p: &Owner, pl: &Placement, visible: bool, seen: bool, now: Instant) {
        let (bar_w, g) = chrome(pl);
        let drag = self.drags[i].as_ref().map(|d| &d.mode);
        // It stays Lit until it's faded out (the glow fades with it), and goes to Rest only once it's invisible.
        let look = if drag.is_some() { Look::Carry } else if visible { Look::Lit } else { Look::Rest };
        let held = match drag {
            Some(Mode::Resize { sx, sy, .. }) => CORNER_SIGN.iter().position(|&c| c == (*sx, *sy)),
            Some(Mode::Stretch { .. }) => Some(BOTTOM_RIGHT),
            _ => None,
        };
        let hovered = match self.hover[i] {
            Some(Part::Corner(c)) => Some(c),
            _ => None,
        };
        let grip = held.or(hovered);
        let bar_look = match (drag, self.hover[i]) {
            (Some(Mode::Move { .. }), _) => Look::Carry,
            (_, Some(Part::Bar)) => Look::Lit,
            _ => Look::Rest,
        };
        let knob_look = match (drag, self.hover[i]) {
            (Some(Mode::Curve { .. }), _) => Look::Carry,
            (_, Some(Part::Knob)) => Look::Lit,
            _ => Look::Rest,
        };
        let window = p.window;
        let zoom_look = window.then_some(match (drag, self.hover[i]) {
            (Some(Mode::Zoom { .. }), _) => Look::Carry,
            (_, Some(Part::Zoom)) => Look::Lit,
            _ => Look::Rest,
        });
        let ctl_look = (p.ctls > 0).then(|| std::array::from_fn(|c| if self.hover[i] == Some(Part::Ctl(c)) { Look::Lit } else { Look::Rest }));
        let snap = self.snap_glyph(i, pl).map(|g| (if self.hover[i] == Some(Part::Snap) { Look::Lit } else { Look::Rest }, g));
        // shown past FACE_TURN and hidden again only under 2/3 of it, so head sway doesn't make it blink
        let shown = self.drawn[i].as_ref().is_some_and(|d| d.spec.face.is_some());
        let face = (turn(&pl.pose(), &facing(pl, &vr::head_position())) > if shown { FACE_TURN * 2.0 / 3.0 } else { FACE_TURN }).then_some(if self.hover[i] == Some(Part::Face) { Look::Lit } else { Look::Rest });
        let mm = |x: f64| (x * 1000.0).round() as i64;
        let dims = [mm(pl.width), mm(pl.height), mm(g)];
        let (tag_n, tag) = p.tag.clone();
        let drawn_in = theme::generation();
        // A full draw came back from the painter. Show it if it answers the latest request (a
        // stale one gets dropped), repaint any grip or hover since then into it below, then do
        // one upload.
        let mut upload = false;
        let mut was = None; // what was shown before, so we can keep it if SteamVR refuses the new one
        if let Some((seq, buf)) = self.done[i].take()
            && answers(&self.pending[i], seq)
        {
            let d = self.pending[i].take().map(|p| p.1);
            was = Some((std::mem::replace(&mut self.drawn[i], d), std::mem::replace(&mut self.cards[i], buf)));
            upload = true;
        }
        // Draw the card again for a new look, a new tag, or a new size. A new size means 2% of
        // the panel, or an eighth of the frame (at least 3 mm; the frame follows the head's
        // distance), compared to the size it was drawn (or last asked) for.
        // While resizing, it redraws at most ~8 times a second, and never while a draw is still
        // out. A newer ask would make that one stale, and with the size changing every frame
        // none would ever land, so the card stretches a little in between.
        // It doesn't redraw while it's unseen (it gets drawn when it's next wanted), or for
        // fading out to Rest, since nobody sees that and the next Lit draws again.
        // A theme switch redraws only while it's seen, and only if what the card paints with
        // changed (a panel-only switch doesn't).
        // Unseen, a first draw or a new tag is a stub at Rest, and the first look anyone sees
        // draws it in full. Before this, a window opening drew two full Rest cards nobody saw.
        // Full draws are the painter's job, and the card shows what it has until one comes back.
        // A first draw that's seen right away is a stub until then too.
        let asked = self.pending[i].is_some();
        let again = match self.pending[i].as_mut().map(|p| &mut p.1).or(self.drawn[i].as_mut()) {
            None => true,
            Some(d) => {
                let old = d.dims;
                let resized = (0..2).any(|n| (dims[n] - old[n]).abs() * 50 > dims[n].abs()) || (dims[2] - old[2]).abs() > (dims[2] / 8).max(3);
                let resize_now = resized && seen && !asked && (now - d.card_at > Duration::from_millis(120) || drag.is_none());
                let mut repaint = false;
                if drawn_in != d.theme && seen {
                    repaint = Paint::new(&theme::get(), p.accent) != d.spec.paint;
                    d.theme = drawn_in; // if it's unchanged, don't check again every frame
                }
                resize_now || (seen && look != d.spec.look && look != Look::Rest) || tag_n != d.tag || repaint
            }
        };
        if again {
            let (tab, tag) = match tag {
                Some(t) => {
                    let (iw, u, v, tab) = legend(pl.width, pl.height, g, t.0 as f64 / t.1 as f64, p.ctls);
                    (Some(tab), Some((t, iw, u, v)))
                }
                None => (None, None),
            };
            let paint = Paint::new(&theme::get(), p.accent);
            let mut spec = CardSpec { w: pl.width, h: pl.height, g, tab, tag, bar: (bar_w, bar_look), knob: knob_look, zoom: zoom_look, snap, face, ctl: ctl_look, ctls: p.ctls, size: (0, 0), paint, look, grip };
            spec.size = card_size(pl.width, pl.height + spec.top() + spec.bottom(), g);
            let card = |spec| Drawn { spec, dims, card_at: now, placed: None, tag: tag_n, theme: drawn_in };
            if self.drawn[i].is_none() || !seen {
                // a stub right now (it's cheap), replacing any full draw still out
                let mut stub = spec.clone();
                let buf = stub.stub();
                (self.pending[i], self.done[i]) = (None, None);
                self.painter.jobs.cancel(i);
                let shown = (std::mem::replace(&mut self.drawn[i], Some(card(stub))), std::mem::replace(&mut self.cards[i], buf));
                was = was.or(Some(shown));
                upload = true;
            }
            if seen {
                let seq = self.painter.ask(i, p.name, spec.clone());
                self.pending[i] = Some((seq, card(spec)));
            }
        }
        // A grip or the bar's look changed, so repaint just those bits of what's shown. A full
        // draw that's still out picks them up when it's back (above). Skip stubs, they stay
        // transparent.
        if let Some(d) = self.drawn[i].as_mut().filter(|d| d.spec.look != Look::Rest) {
            let mut rects = Vec::new();
            if grip != d.spec.grip {
                rects.extend([d.spec.grip, grip].into_iter().flatten().map(|c| d.spec.corner_rect(c)));
            }
            if bar_look != d.spec.bar.1 || knob_look != d.spec.knob || zoom_look != d.spec.zoom || snap != d.spec.snap || face != d.spec.face {
                rects.push(d.spec.bar_rect());
            }
            if ctl_look != d.spec.ctl {
                d.spec.ctl = ctl_look;
                rects.push(d.spec.ctl_rect());
            }
            if !rects.is_empty() {
                (d.spec.grip, d.spec.bar.1, d.spec.knob, d.spec.zoom, d.spec.snap, d.spec.face) = (grip, bar_look, knob_look, zoom_look, snap, face);
                for r in rects {
                    d.spec.repaint(&mut self.cards[i], r);
                }
                upload = true;
            }
        }
        // A panel curved top to bottom is turned a quarter round for SteamVR (kvm.rs set_place),
        // so its card goes up turned too and bends with it.
        if self.turned[i] != pl.vert && self.drawn[i].is_some() {
            upload = true;
            if let Some(d) = self.drawn[i].as_mut() {
                d.placed = None; // place it again, turned or not
            }
        }
        if upload && let Some(d) = self.drawn[i].as_ref() {
            // a new texture's size may differ, so it gets placed again (placed is None)
            // ponytail: the whole card uploads on the main loop (Prefs' is ~5 MB, 6-15 ms), same
            // as ui::Painter's windows (it only moves the draw off). Getting it off the loop needs
            // its own thread owning the card's SetOverlayRaw, since OpenVR doesn't promise its
            // overlay calls are thread-safe, or a GPU texture per card patched by rect and
            // SetOverlayTexture.
            let (w, h) = d.spec.size;
            let ok = if pl.vert {
                // ponytail: the whole card gets rotated on the CPU every upload (~1.5 Mpx); do it patch by patch if that ever shows up
                let mut t = vec![0u8; self.cards[i].len()];
                unsafe { crate::gpu::copy_rect(self.cards[i].as_ptr(), w * 4, t.as_mut_ptr(), h * 4, w, h, true) };
                set_raw(p.card, &t, h, w)
            } else {
                set_raw(p.card, &self.cards[i], w, h)
            };
            if ok {
                self.turned[i] = pl.vert;
            } else if let Some((d, buf)) = was {
                (self.drawn[i], self.cards[i]) = (d, buf); // SteamVR's still showing that one
            }
        }
        let Some(d) = self.drawn[i].as_mut() else { return };
        // Follow the panel, but only when it moved or changed size. The card's sized for the
        // frame it was drawn with, so it fits the picture exactly between redraws too.
        let placed = Some((pl.matrix(), mm(pl.width), mm(pl.height), mm(pl.curve)));
        if d.placed != placed {
            d.placed = placed;
            let bend = |len: f64| if pl.curve > 0.0 { (len / (2.0 * std::f64::consts::PI * pl.curve)).min(1.0) } else { 0.0 };
            let s = &d.spec;
            let (top, bottom) = (s.top(), s.bottom()); // the tab's room on top, the bar's below
            let (fw, fh) = (pl.width + 2.0 * s.g, pl.height + 2.0 * s.g + top + bottom);
            let (tw, th) = (s.size.0 as f64, s.size.1 as f64);
            let c = p.card;
            let at = pl.on_surface(0.0, (top - bottom) / 2.0, -0.002); // 2 mm behind the picture
            // turned (its panel curves top to bottom): x' is up and y' is left, same as the picture's overlay
            let (ow, oh, at) = if pl.vert { (fh, fw, [0, 1, 2].map(|r| [at[r][1], -at[r][0], at[r][2], at[r][3]])) } else { (fw, fh, at) };
            let (tw, th) = if pl.vert { (th, tw) } else { (tw, th) };
            call!(ov, SetOverlayWidthInMeters, c, ow as f32);
            call!(ov, SetOverlayTexelAspect, c, (ow * th / (oh * tw)) as f32); // exactly oh high
            call!(ov, SetOverlayCurvature, c, bend(ow) as f32);
            let mut scale = sys::HmdVector2_t { v: [ow as f32, oh as f32] }; // events in metres
            call!(ov, SetOverlayMouseScale, c, &mut scale);
            vr::place(c, &at);
        }
    }

}

/// The angle in degrees between two poses' rotations.
fn turn(now: &Pose, at: &Pose) -> f64 {
    let (a, b) = (panel_matrix(now), panel_matrix(at));
    let trace: f64 = (0..3).map(|r| (0..3).map(|c| a[c][r] as f64 * b[c][r] as f64).sum::<f64>()).sum(); // trace of a's rotation⁻¹ · b's
    ((trace - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees()
}

/// pl turned in place to face the eye. It keeps its centre, size, curve and vert, and its
/// front (the normal at its middle) points at the eye, upright (roll 0).
fn facing(pl: &Placement, eye: &V3) -> Pose {
    let (yaw, pitch) = angles(&[0, 1, 2].map(|r| pl.c[r] - eye[r])); // the direction you'd look to see it
    Pose { yaw, pitch, roll: 0.0, ..pl.pose() }
}

/// Is a panel at `now` off its aligned place `at`? That means moved, turned, resized or bent
/// past the knobs (SNAP_MOVE, SNAP_TURN), or curved the other way.
fn moved(now: &Pose, at: &Pose) -> bool {
    let turn = turn(now, at);
    let shift = (0..3).map(|r| (now.centre[r] - at.centre[r]).powi(2)).sum::<f64>().sqrt();
    shift > SNAP_MOVE || turn > SNAP_TURN || (now.width - at.width).abs() > SNAP_MOVE || now.vert != at.vert
        || (now.curve - at.curve).abs() > SNAP_MOVE
}

/// Does panel `a` (with its card's margin) cover panel `b` as seen from the eye? Sight lines
/// through points on b (card included) that also pass through a vote for whichever panel they
/// hit first, and a covers b if more hit a first. If they don't overlap in view, it's false.
fn in_front(a: &Placement, ga: f64, b: &Placement, gb: f64, eye: &[f64; 3]) -> bool {
    let (mut a_first, mut b_first) = (0, 0);
    const N: usize = 9;
    for i in 0..N {
        for j in 0..N {
            let u = (i as f64 / (N - 1) as f64 - 0.5) * (b.width + 2.0 * gb);
            let v = (j as f64 / (N - 1) as f64 - 0.5) * (b.height + 2.0 * gb);
            let m = b.on_surface(u, v, 0.0);
            let q = [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64];
            let to = [q[0] - eye[0], q[1] - eye[1], q[2] - eye[2]];
            let d = norm(&to);
            if d < 1e-6 {
                continue;
            }
            let dir = to.map(|x| x / d);
            let Some((t, hu, hv)) = a.hit(eye, &dir) else { continue };
            if hu.abs() > a.width / 2.0 + ga || hv.abs() > a.height / 2.0 + ga {
                continue; // this sight line misses a
            }
            if t < d { a_first += 1 } else { b_first += 1 }
        }
    }
    a_first > b_first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_corner_resizes_about_the_centre() {
        let (w, a) = (1.6, 0.5625);
        for (sx, sy) in CORNER_SIGN {
            // The corner left where it is keeps the same width.
            assert!((width_for(sx, sy, sx * w / 2.0, sy * a * w / 2.0, a) - w).abs() < 1e-9);
            // Dragging it 10% further out along the diagonal makes it 10% wider.
            assert!((width_for(sx, sy, sx * w * 0.55, sy * a * w * 0.55, a) - w * 1.1).abs() < 1e-9);
        }
    }

    #[test]
    fn the_bottom_right_corner_stretches_from_the_top_left() {
        use crate::geometry::{Pose, panel_matrix};
        let at = |m: Mat| [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64];
        let near = |a: [f64; 3], b: [f64; 3]| norm(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]]) < 1e-5;
        let pose = Pose { centre: [0.3, 1.5, -1.4], yaw: 25.0, pitch: 6.0, roll: 2.0, width: 1.2, curve: 0.0, vert: false };
        for curve in [0.0, 1.8] {
            let pl = Placement::from_matrix(&panel_matrix(&pose), 1.2, 0.5625, curve);
            let top_left = at(pl.on_surface(-0.6, 0.3375, 0.0));
            // A window takes any shape: the corner lands where the laser put it and the top-left stays.
            let (cu, cv) = (0.9, -0.1); // 1.5 wide, 0.4375 high
            let big = (MAX_WIDTH, MAX_WIDTH);
            let (m, w, h, c) = stretched(&pl, cu, cv, None, big);
            assert!((w - 1.5).abs() < 1e-9 && (h - 0.4375).abs() < 1e-9, "curve {curve}: {w} x {h}");
            let np = Placement::from_matrix(&m, w, h / w, c);
            assert!(near(at(np.on_surface(-w / 2.0, h / 2.0, 0.0)), top_left), "curve {curve}: the top-left moved");
            assert!(near(at(np.on_surface(w / 2.0, -h / 2.0, 0.0)), at(pl.on_surface(cu, cv, 0.0))), "curve {curve}: the corner isn't at the laser");
            // A remote screen keeps its shape, with the top-left still fixed.
            let (m, w, h, c) = stretched(&pl, cu, cv, Some(0.5625), big);
            assert!((h / w - 0.5625).abs() < 1e-9 && w > 1.2, "curve {curve}: {w} x {h}");
            let np = Placement::from_matrix(&m, w, h / w, c);
            assert!(near(at(np.on_surface(-w / 2.0, h / 2.0, 0.0)), top_left), "curve {curve}: the top-left moved");
            // Left where it is, nothing changes.
            let (m, w, h, _) = stretched(&pl, 0.6, -0.3375, Some(0.5625), big);
            assert!((w - 1.2).abs() < 1e-9 && (h - 0.675).abs() < 1e-9 && near(at(m), pl.c));
            // Dragged past the top-left, it's the smallest panel, still hanging from the top-left.
            let (m, w, h, c) = stretched(&pl, -1.0, 1.0, None, big);
            assert_eq!((w, h), (MIN_WIDTH, MIN_WIDTH));
            let np = Placement::from_matrix(&m, w, h / w, c);
            assert!(near(at(np.on_surface(-w / 2.0, h / 2.0, 0.0)), top_left));
            // A window can't get bigger than the output, so the corner stops there.
            let (m, w, h, c) = stretched(&pl, 3.0, -2.0, None, (1.83, 1.03));
            assert_eq!((w, h), (1.83, 1.03));
            let np = Placement::from_matrix(&m, w, h / w, c);
            assert!(near(at(np.on_surface(-w / 2.0, h / 2.0, 0.0)), top_left), "curve {curve}: the top-left moved");
        }
        // Widened past a half circle, the curve grows (a new cylinder), and it still hangs from
        // the top-left.
        let pl = Placement::from_matrix(&panel_matrix(&Pose { width: 5.6, curve: 1.8, ..pose }), 5.6, 0.5625, 1.8);
        let top_left = at(pl.on_surface(-2.8, 1.575, 0.0));
        let (m, w, h, c) = stretched(&pl, 3.2, -1.575, None, (MAX_WIDTH, MAX_WIDTH));
        assert!((w - 6.0).abs() < 1e-9 && c > 1.8, "{w} m, curve {c}");
        let np = Placement::from_matrix(&m, w, h / w, c);
        assert!(near(at(np.on_surface(-w / 2.0, h / 2.0, 0.0)), top_left), "the top-left moved");
    }

    #[test]
    fn a_curved_wing_covers_a_panel_whose_centre_is_nearer() {
        use crate::geometry::{Pose, panel_matrix};
        // My layout on 2026-10-01: desk-wide is 4.29 m on a 1.915 m radius, and
        // desk-portrait sits off to its right, behind its wing, but with its centre nearer the eye.
        let wide = Pose { centre: [0.2677, 1.8979, -2.3907], yaw: -11.0, pitch: 7.456, roll: 0.0, width: 4.2898, curve: 1.915, vert: false };
        let portrait = Pose { centre: [2.0466, 1.7031, -0.8611], yaw: -73.987, pitch: 1.611, roll: 0.0, width: 0.9, curve: 0.0, vert: false };
        let w = Placement::from_matrix(&panel_matrix(&wide), wide.width, 1.2065 / 4.2898, wide.curve);
        let p = Placement::from_matrix(&panel_matrix(&portrait), portrait.width, 1.6 / 0.9, 0.0);
        let eye = [0.0, 1.6, 0.0];
        assert!(norm(&[p.c[0], p.c[1] - 1.6, p.c[2]]) < norm(&[w.c[0], w.c[1] - 1.6, w.c[2]]));
        assert!(in_front(&w, 0.05, &p, 0.05, &eye), "the wing should cover the portrait");
        assert!(!in_front(&p, 0.05, &w, 0.05, &eye));
    }

    #[test]
    fn side_edge_depth_gives_the_curve() {
        let w = 1.6;
        assert_eq!(radius_for(0.005, w), 0.0); // under a centimetre is flat
        assert_eq!(radius_for(5.0, w), w / std::f64::consts::PI); // never past a half circle
        for r in [0.8, 1.5, 3.0] {
            let sag = r * (1.0 - (w / (2.0 * r)).cos());
            assert!((radius_for(sag, w) - r).abs() < 1e-6, "{r}");
        }
        assert_eq!(curve_for_width(1.0, 2.0), 1.0);
        assert!((curve_for_width(1.0, 4.0) - 4.0 / std::f64::consts::PI).abs() < 1e-12);
        assert_eq!(curve_for_width(0.0, 4.0), 0.0);
        // a point grabbed in a device's frame comes back where it was
        let (c, sn) = (0.6f32.cos(), 0.6f32.sin());
        let d: Mat = [[c, 0.0, sn, 0.3], [0.0, 1.0, 0.0, 1.2], [-sn, 0.0, c, -0.5]];
        let p = [1.0, 1.5, -2.0];
        let l = to_device(&d, &p);
        let back = [0, 1, 2].map(|r| d[r][0] as f64 * l[0] + d[r][1] as f64 * l[1] + d[r][2] as f64 * l[2] + d[r][3] as f64);
        assert!((0..3).all(|r| (back[r] - p[r]).abs() < 1e-5), "{back:?}");
    }

    #[test]
    fn a_hovered_part_holds_past_its_edge() {
        let img = Arc::new((80, 12, [200u8, 220, 255, 255].repeat(80 * 12)));
        let (w, h, g) = (1.19, 0.34, 0.03);
        let spec = spec_for(w, h, g, 0.1, Look::Lit, &img, &Theme::default(), REMOTE);
        let (bv, bh) = spec.bar_box();
        let held = |was, u, v| held_part(&spec, w, h, false, was, u, v);
        assert_eq!(held(None, 0.0, bv), Some(Part::Bar));
        let off = bv - bh / 4.0 - 0.1 * g; // just under the bar
        assert_eq!(held(Some(Part::Bar), 0.0, off), Some(Part::Bar), "a few mm off it: still on it");
        assert_eq!(held(None, 0.0, off), Some(Part::Edge), "coming from elsewhere: the frame");
        assert_eq!(held(Some(Part::Bar), 0.0, off - g), Some(Part::Edge), "well off it");
        assert_eq!(held(Some(Part::Edge), 0.0, bv), Some(Part::Bar), "onto another part: that one");
        let corner = held(None, w / 2.0 + 0.5 * g, -h / 2.0 - 0.5 * g);
        assert_eq!(corner, Some(Part::Corner(3)));
        assert_eq!(held(corner, w / 2.0 - 0.1 * g, -h / 2.0 - 0.5 * g), corner, "a grip doesn't flicker along its edge");
        assert_eq!(held(None, w / 2.0 - 0.1 * g, -h / 2.0 - 0.5 * g), Some(Part::Edge));
        // the Extra's dead zone around its buttons keeps whatever it was on, or nothing
        let (us, cv, _) = controls(w, h, g, CTLS);
        let close = held_part(&spec, w, h, true, None, us[CTLS - 1], cv);
        assert_eq!(close, Some(Part::Ctl(CTLS - 1)));
        let dead = (0.0, h / 2.0 + 0.3 * g);
        assert_eq!(held_part(&spec, w, h, true, close, dead.0, dead.1), close);
        assert_eq!(held_part(&spec, w, h, true, None, dead.0, dead.1), None);
        assert_eq!(held_part(&spec, w, h, false, None, dead.0, dead.1), Some(Part::Edge), "a panel's: its frame");
    }

    #[test]
    fn card_parts() {
        let (w, h) = (1.6, 0.9);
        assert_eq!(part_at(0.0, 0.0, w, h), None);
        assert_eq!(part_at(0.81, 0.0, w, h), Some(Part::Edge));
        assert_eq!(part_at(-0.81, 0.0, w, h), Some(Part::Edge));
        assert_eq!(part_at(0.0, -0.46, w, h), Some(Part::Edge));
        assert_eq!(part_at(-0.81, 0.46, w, h), Some(Part::Corner(0)));
        assert_eq!(part_at(0.81, 0.46, w, h), Some(Part::Corner(1)));
        assert_eq!(part_at(-0.81, -0.46, w, h), Some(Part::Corner(2)));
        assert_eq!(part_at(0.81, -0.46, w, h), Some(Part::Corner(3)));
    }

    /// A card for a w × h panel with frame g and bar bw, with a tag image `img`, in a theme.
    fn spec_for(w: f64, h: f64, g: f64, bw: f64, look: Look, img: &Arc<TagImg>, th: &Theme, accent: Rgb) -> CardSpec {
        let (iw, u, v, tab) = legend(w, h, g, img.0 as f64 / img.1 as f64, CTLS);
        let mut s = CardSpec { w, h, g, tab: Some(tab), tag: Some((img.clone(), iw, u, v)), bar: (bw, Look::Rest), knob: Look::Rest, zoom: Some(Look::Rest), snap: None, face: None, ctl: Some([Look::Rest; CTLS]), ctls: CTLS, size: (0, 0), paint: Paint::new(th, accent), look, grip: None };
        s.size = card_size(w, h + s.top() + s.bottom(), g);
        s
    }

    #[test]
    fn face_me_turns_the_panel_in_place_to_the_eye() {
        let eye = [0.3, 1.6, 0.1];
        for (curve, vert) in [(0.0, false), (1.2, false), (1.2, true)] {
            let at = Pose { centre: [-0.8, 1.0, -1.1], yaw: 40.0, pitch: 15.0, roll: 9.0, width: 1.0, curve, vert };
            let pl = Placement { vert, ..Placement::from_matrix(&panel_matrix(&at), at.width, 0.56, curve) };
            let f = facing(&pl, &eye);
            assert!(turn(&pl.pose(), &f) > FACE_TURN, "turned away: the button shows");
            let m = Placement { vert, ..Placement::from_matrix(&panel_matrix(&f), f.width, 0.56, f.curve) };
            let to = [0, 1, 2].map(|r| eye[r] - m.c[r]);
            assert!(dot(&m.z, &to) / norm(&to) > 1.0 - 1e-6, "its normal points at the eye ({curve}, {vert})");
            assert!((0..3).all(|r| (m.c[r] - pl.c[r]).abs() < 1e-6), "centre kept");
            assert!(m.x[1].abs() < 1e-6 && f.roll == 0.0, "upright");
            assert_eq!((f.width, f.curve, f.vert), (at.width, at.curve, at.vert));
            assert!(turn(&m.pose(), &facing(&m, &eye)) < FACE_TURN, "facing: no button");
        }
    }

    #[test]
    fn snap_button_shows_only_off_the_aligned_place_and_is_hit_left_of_the_bar() {
        let at = Pose { centre: [0.2, 1.1, -0.9], yaw: 12.0, pitch: -5.0, roll: 0.0, width: 1.19, curve: 1.0, vert: false };
        let pl = Placement::from_matrix(&panel_matrix(&at), at.width, 0.28, at.curve);
        assert!(!moved(&pl.pose(), &at), "just aligned: no button");
        let near = |f: &dyn Fn(&mut Pose)| {
            let mut p = at;
            f(&mut p);
            moved(&p, &at)
        };
        assert!(!near(&|p| p.centre[2] += 0.005) && near(&|p| p.centre[2] += 0.02), "1 cm");
        assert!(!near(&|p| p.yaw += 0.3) && near(&|p| p.yaw += 1.0), "0.5 degrees");
        assert!(near(&|p| p.curve = 0.0) && near(&|p| p.vert = true) && near(&|p| p.width = 1.3));
        let img = Arc::new((80, 12, [200u8, 220, 255, 255].repeat(80 * 12)));
        let mut spec = spec_for(1.19, 0.34, 0.03, 0.1, Look::Lit, &img, &Theme::default(), REMOTE);
        let (su, sv, _) = spec.snap_at();
        assert!(su < -spec.bar.0 / 2.0 && !spec.on_snap(su, sv), "left of the bar; none drawn, none hit");
        spec.snap = Some((Look::Rest, Glyph::Snap));
        assert!(spec.on_snap(su, sv) && !spec.on_bar(su, sv) && !spec.on_knob(su, sv));
        let (x0, x1, _, _) = spec.bar_rect();
        let (sx, _) = spec.scale();
        assert!((x0 as f64) < (su - 1.3 * spec.knob_at().2 + spec.w / 2.0 + spec.g) * sx && x1 > x0, "its repaint covers it");
    }

    /// Breeze Light's tokens (BreezeLight.colors), for a light card.
    fn light() -> Theme {
        let t = Tokens { frame: [222.0, 224.0, 226.0], surface: [239.0, 240.0, 241.0], raised: [252.0; 3], text: [35.0, 38.0, 41.0], dim: [112.0, 125.0, 138.0], wtext: [35.0, 38.0, 41.0], wdim: [112.0, 125.0, 138.0], border: mix([222.0, 224.0, 226.0], [35.0, 38.0, 41.0], 0.25), dark: false };
        Theme { win: t, shell: t, radius: 5.0 }
    }

    #[test]
    fn card_textures_fit_and_stay_crisp() {
        let img = Arc::new((80, 12, [200u8, 220, 255, 255].repeat(80 * 12)));
        // desk-wide, desk-portrait, the laptop and a small one; the last one's bar is wider than its frame holds
        for ((w, h, g, bw), scheme) in [(3.2, 0.9, 0.05, 0.25), (0.9, 1.6, 0.035, 0.15), (1.2, 0.675, 0.04, 0.17), (0.2, 0.11, 0.02, 0.1)].into_iter().zip([Theme::default(), light(), light(), Theme::default()]) {
            let (tw, th) = card_size(w, h, g);
            assert!(tw * th <= 1_500_000, "{tw}x{th}");
            assert!(g * tw as f64 / (w + 2.0 * g) >= 14.0, "frame only {} px", g * tw as f64 / (w + 2.0 * g));
            assert!(tw <= 1920 && th <= 1920, "{tw}x{th}");
            let mut spec = spec_for(w, h, g, bw, Look::Lit, &img, &scheme, REMOTE);
            let (tw, th) = spec.size;
            let mut buf = spec.texture();
            assert_eq!(buf.len(), tw * th * 4);
            // The bar and tag are drawn in, so the pill's middle and the tag's are mostly opaque.
            let px = |buf: &[u8], u: f64, v: f64| {
                let (x0, _, y0, _) = spec.rect(u, u, v, v);
                buf[((y0 + 1) * tw + x0 + 1) * 4 + 3]
            };
            assert!(px(&buf, 0.0, spec.bar_box().0) > 100, "no bar");
            let t = spec.tag.as_ref().map(|t| (t.2, t.3)).unwrap();
            assert!(px(&buf, t.0, t.1) > 100, "no tag");
            assert!(spec.on_bar(0.0, spec.bar_box().0) && !spec.on_bar(0.0, 0.0));
            let (ku, kv, _) = spec.knob_at();
            assert!(spec.on_knob(ku, kv) && !spec.on_bar(ku + spec.bar.0 * 0.05, kv), "the knob is beside the bar");
            assert!(ku + 2.0 * spec.knob_at().2 < w / 2.0 + g, "the knob fits on the card");
            let (us, cv, (u0, _, _)) = controls(w, h, g, CTLS);
            assert_eq!(us.map(|u| spec.on_ctl(u, cv)), [Some(0), Some(1), Some(2)]);
            assert_eq!(CTL, [Ctl::Minimize, Ctl::Theater, Ctl::Close]); // v ^ x
            let tab_end = spec.tab.unwrap().1;
            assert!(tab_end < u0 || w < 0.5, "the tag's tab stops before the controls' ({tab_end} vs {u0})");
            let (zu, zv, zr) = spec.zoom_at();
            assert!(spec.on_zoom(zu, zv) && !spec.on_knob(zu, zv), "the zoom button is its own");
            assert!(zu + 2.0 * zr < w / 2.0 + g, "the zoom button fits on the card");
            // Patching a corner's grip or the bar's look in place has to give exactly the same
            // texture as drawing the whole thing with it.
            (spec.grip, spec.bar.1, spec.knob, spec.zoom, spec.ctl) = (Some(1), Look::Carry, Look::Lit, Some(Look::Carry), Some([Look::Lit, Look::Rest, Look::Carry]));
            spec.repaint(&mut buf, spec.corner_rect(1));
            spec.repaint(&mut buf, spec.ctl_rect());
            spec.repaint(&mut buf, spec.bar_rect());
            assert!(buf == spec.texture(), "patch differs");
            // Between the picture and its outline, the frame is opaque and in the scheme's titlebar colour.
            let (x, _, y, _) = spec.rect(0.0, 0.0, -h / 2.0 - 0.15 * g, -h / 2.0 - 0.15 * g);
            let at = &buf[((y + 1) * tw + x + 1) * 4..][..4];
            let f = spec.paint.t.frame.map(|c| c as u8);
            assert_eq!(at, [f[0], f[1], f[2], 255], "the frame");
        }
    }

    #[test]
    fn close_alone_is_where_a_windows_close_is() {
        let img = Arc::new((80, 12, [200u8, 220, 255, 255].repeat(80 * 12)));
        let mut spec = spec_for(0.44, 0.17, 0.03, 0.1, Look::Lit, &img, &Theme::default(), REMOTE);
        spec.ctls = 1; // like the Machines window's
        let (us, cv, (u0, u1, _)) = controls(0.44, 0.17, 0.03, 1);
        assert_eq!(us, controls(0.44, 0.17, 0.03, CTLS).0);
        assert!(u0 > us[1] && u0 < us[2] && u1 > us[2], "its tab round close alone");
        assert_eq!(us.map(|u| spec.on_ctl(u, cv)), [None, None, Some(2)]);
        assert_eq!(CTL[2], Ctl::Close);
    }

    #[test]
    fn an_unseen_card_is_a_stub_of_its_shape() {
        let img = Arc::new((80, 12, [200u8, 220, 255, 255].repeat(80 * 12)));
        let full = spec_for(1.2, 0.675, 0.04, 0.17, Look::Lit, &img, &Theme::default(), VIOLET);
        let mut stub = full.clone();
        let buf = stub.stub();
        assert_eq!(buf.len(), stub.size.0 * stub.size.1 * 4);
        assert!(stub.size.0 * 8 < full.size.0 && stub.size.1 * 8 < full.size.1, "{:?} of {:?}", stub.size, full.size);
        assert!(buf.iter().all(|&b| b == 0), "transparent");
        assert_eq!(stub.look, Look::Rest, "a seen look differs: drawn in full then");
        // lasers find the same parts on it, and it's placed at the same height
        assert_eq!((stub.top(), stub.bottom()), (full.top(), full.bottom()));
        let ((bv, _), (ku, kv, _), (us, cv, _)) = (full.bar_box(), full.knob_at(), controls(1.2, 0.675, 0.04, CTLS));
        assert!(stub.on_bar(0.0, bv) && stub.on_knob(ku, kv) && stub.on_ctl(us[2], cv) == Some(2));
    }

    #[test]
    fn only_the_latest_card_asked_for_is_drawn_and_shown() {
        let img = Arc::new((80, 12, [200u8, 220, 255, 255].repeat(80 * 12)));
        let spec = |look| spec_for(0.2, 0.11, 0.02, 0.1, look, &img, &Theme::default(), VIOLET);
        // In the queue, a newer request for a panel replaces its older one, and other panels wait their turn.
        let jobs = Jobs::default();
        jobs.put(0, (1, "a".into(), spec(Look::Lit)));
        jobs.put(1, (2, "b".into(), spec(Look::Lit)));
        jobs.put(0, (3, "a".into(), spec(Look::Carry)));
        jobs.put(2, (4, "c".into(), spec(Look::Lit)));
        jobs.cancel(2);
        let (i, (seq, _, s)) = jobs.next();
        assert_eq!((i, seq, s.look), (0, 3, Look::Carry));
        assert_eq!(jobs.next().1.0, 2);
        assert!(jobs.queue.lock().unwrap().is_empty(), "coalesced and cancelled");
        // Drawn on the worker: whatever comes back, only the latest request's texture gets shown.
        let mut p = Painter::new();
        let old = p.ask(0, "a", spec(Look::Lit));
        let new = p.ask(0, "a", spec(Look::Carry));
        let want = spec(Look::Carry);
        let pending = Some((new, Drawn { spec: want.clone(), dims: [0; 3], card_at: Instant::now(), placed: None, tag: 0, theme: 0 }));
        loop {
            let (i, seq, buf) = p.done.recv_timeout(Duration::from_secs(10)).expect("drawn");
            assert_eq!(i, 0);
            if seq == old {
                assert!(!answers(&pending, seq), "stale: dropped");
                continue;
            }
            assert!(seq == new && answers(&pending, seq));
            assert!(buf == want.texture(), "the latest card");
            break;
        }
        assert!(!answers(&None, new), "nothing asked for: nothing shown");
    }

    #[test]
    fn tag_mask_tints() {
        let (text, dim, dot) = ([1.0, 2.0, 3.0], [10.0, 20.0, 30.0], [100.0, 0.0, 50.0]);
        assert_eq!(tint(([255.0, 0.0, 0.0], 0.5), text, dim, dot), (text, 0.5), "R: the name");
        assert_eq!(tint(([0.0, 128.0, 0.0], 1.0), text, dim, dot), (dim, 1.0), "G: the secondary text");
        assert_eq!(tint(([0.0, 0.0, 9.0], 1.0), text, dim, dot), (dot, 1.0), "B: the dot");
        assert_eq!(tint(([0.0; 3], 0.0), text, dim, dot).1, 0.0);
        // The backdrop stays dark on a light scheme.
        assert!(backdrop_colour(&light().win)[..3].iter().all(|&c| c < 40));
    }

    /// Dev tool: CC_DUMP=<dir> [CC_SCHEME=<a .colors file>] cargo test -- --ignored dump writes
    /// the cards out as raw RGBA, named card_<look>_<scheme>_<kind>_WxH.rgba. It does a remote
    /// machine's (using the first tag-*.rgba in the dir that isn't an app's or a chip's) and a
    /// window's (tag-app-*), each at rest, lit and carried, in the scheme (Breeze Dark if none).
    #[test]
    #[ignore]
    fn dump() {
        let dir = std::env::var("CC_DUMP").unwrap();
        let (th, scheme) = match std::env::var("CC_SCHEME") {
            Ok(p) => (theme::from_scheme(&p), std::path::Path::new(&p).file_stem().unwrap().to_string_lossy().into_owned()),
            Err(_) => (Theme::default(), "builtin".into()),
        };
        let tags: Vec<std::path::PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        let find = |app: bool| {
            let p = tags.iter().find(|p| {
                let n = p.file_name().unwrap().to_string_lossy();
                n.starts_with("tag-") && n.ends_with(".rgba") && !n.starts_with("tag-chip-") && n.starts_with("tag-app-") == app
            });
            Arc::new(p.and_then(|p| crate::load_tag(&p.to_string_lossy())).unwrap_or((80, 12, [255u8, 0, 0, 255].repeat(80 * 12))))
        };
        for (kind, img, accent) in [("remote", find(false), REMOTE), ("window", find(true), VIOLET)] {
            for (n, l) in [("rest", Look::Rest), ("lit", Look::Lit), ("carry", Look::Carry)] {
                let mut spec = spec_for(1.2, 0.675, 0.05, 0.17, l, &img, &th, accent);
                (spec.grip, spec.bar.1, spec.knob, spec.zoom, spec.ctl) = ((l != Look::Rest).then_some(1), l, l, Some(l), Some([l; CTLS]));
                std::fs::write(format!("{dir}/card_{n}_{scheme}_{kind}_{}x{}.rgba", spec.size.0, spec.size.1), spec.texture()).unwrap();
            }
        }
    }
}
