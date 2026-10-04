//! The taskbar (docs/plasma-look-design.md (c), docs/window-panels-design2.md §10). It's one
//! frame with Plasma's own taskbar on top and our row of chips under it. Plasma's bar is
//! plasmabar.rs, a separate overlay that sits in front of ours inside the frame. The chips,
//! left to right:
//!   - a cyan one per remote machine and a violet one per Frame window;
//!   - "+N" for windows waiting for a slot;
//!   - "Workspace", which opens and closes the Workspace window (control/workspace.rs; its
//!     Machines pages are control/machines.rs);
//!   - a gear for Preferences (control/prefs.rs);
//!   - last, a power chip. One click arms it (magenta, "Close?" for ARM) and a second click
//!     within ARM closes Desktop (QUIT, same as Right Ctrl + Esc). The desktop session stops,
//!     and the next open restores your apps but not their contents (cc-home hibernate).
//!
//! Ours is one overlay, `controlcenter.taskbar`, since SteamVR's 128 overlays are shared by
//! every app. It's drawn in the colours of Plasma's panel (theme.rs `shell`) with Breeze's
//! sizes in bar units. One unit is one logical pixel of Plasma's panel, so the two scale
//! together. It only takes the width it needs: Plasma's panel fits its content, and so do
//! the chips.
//!
//! A chip hides its panel (a window gets minimized in KWin) or brings it back (a window is
//! un-minimized and activated, so it's raised and gets the keyboard). Like Plasma's task
//! manager, a hidden panel's chip is dim and the typing panel's is lit. Chip labels are the
//! tags assets.rs draws (a window's app name, a machine's viewer name) and "+N" uses its
//! glyphs, so there's no font code here.
//!
//! It's handled like a panel (grab.rs), with lasers coming in as overlay mouse events (the
//! mouse scale is its units). An edge carries it (fixed only), a corner resizes both rows
//! together (settings.json taskbar_scale), and holding the curve knob under the chips bends
//! it: pull toward you to bend, push to flatten (taskbar_curve). The mouse's cursor lands on
//! it too (kvm.rs `bar`) and its clicks reach the chips.
//!
//! Placement is settings.json "taskbar", read at startup; `taskbar <mode>` on @controlcenter
//! switches it:
//!   fixed (default)  In the room, 0.25 m under the krdp panels and centred under their span.
//!                    It's placed again when a spot is applied or a krdp panel is dropped.
//!                    Once you carry it by an edge it saves spots.home.taskbar, and that wins
//!                    from then on. With no krdp panels it goes where follow would put it at
//!                    startup and stays frozen there.
//!   follow           0.7 m out and 0.45 m under where you look (the bottom of the view),
//!                    facing you. It eases after the head (yaw and pitch) a moment behind.
//!   wrist            Along the back of the wrist you don't point with (the off hand; changing
//!                    the pointing hand in Preferences moves it live). It's 22 cm wide, wider
//!                    once crowded so the text stays readable, and it fades in as you turn the
//!                    wrist up to look at it, like a watch.
//! It hides while a VR game runs and while every panel is hidden. In theater mode follow and
//! wrist still show, since they're nearer than the backdrop; fixed hides because the Frame
//! draws by depth, not sort order.
//! ponytail: no file watch; hand edits to settings.json apply on restart.
use crate::geometry::{Mat, Placement, Pose, V3, angles, direction, dot, inv_rigid, mul, norm, panel_matrix};
use crate::grab::{self, Look, TagImg, average, line, over, rbox, tint};
use crate::theme::{self, Accent, Tokens};
use crate::kvm::KVM;
use crate::laser::{self, Hand};
use crate::windows::Windows;
use crate::{HIDDEN, Source, call, config, panel, panels, vr};
use openvr_sys as sys;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Fixed,
    Follow,
    Wrist,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "fixed" => Some(Mode::Fixed),
            "follow" => Some(Mode::Follow),
            "wrist" => Some(Mode::Wrist),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Fixed => "fixed",
            Mode::Follow => "follow",
            Mode::Wrist => "wrist",
        }
    }
}

// The frame in bar units (Plasma's logical pixels), from the outside in: REACH of transparent
// laser reach, the border, PAD, Plasma's bar, ROW_GAP, the chip row, then BOTTOM with the knob.
const REACH: f64 = 28.0; // calibration knob: 17 mm at follow's scale, easy to aim at
const PAD: f64 = 3.0;
const ROW_GAP: f64 = 2.0;
pub(crate) const ROW: f64 = 30.0; // the chip row, tiles TILE high (I wanted it compact)
const TILE: f64 = 28.0;
const BOTTOM: f64 = 18.0; // under the chips, holding the curve knob
const KNOB: f64 = 7.0; // the knob's radius
const RADIUS: f64 = 8.0; // calibration knob: the frame's corner radius, set against Plasma's panel corners in the stream
const CORNER: f64 = 28.0; // calibration knob: how far in from the border a corner (resize) reaches, both ways
const GLOW: f64 = 10.0; // a lit frame's glow, outside its border
const MIN_ROW: f64 = 80.0; // the narrowest the content gets (no Plasma and no chips, just the knob)
const MAX_ROW: f64 = 1920.0; // past this, the panel chips all narrow by the same factor
const MAX_CHIP: f64 = 260.0;
const CHIP_PAD: f64 = 7.0; // from a chip's label to each end
const GEAR: f64 = 32.0; // the Preferences chip: its gear plus room around it
const GAP: f64 = 3.0; // between chips
const ARM: Duration = Duration::from_secs(3); // the power chip closes on a second click within this
/// The power chip's label while armed, drawn by assets.rs at start as tag-ui-<key>.rgba.
pub const LABELS: [(&str, &str); 1] = [("close", "Close?")];
pub(crate) const LABEL: f64 = 0.30; // scales assets.rs's 44 px tags and glyphs to Plasma's 10 pt (13.3 u)
pub(crate) const TAG_TEXT: f64 = 36.0; // a tag image's text starts this far in (assets.rs: 32 of room plus 4)
pub(crate) const TAG_ROOM: f64 = 76.0; // and the image is this much wider than its text
pub(crate) const GLYPHS: &str = "0123456789:+"; // assets.rs' glyphs.rgba, in its order
const FRONT: f64 = 0.002; // Plasma's overlay sits this far in front of ours (m), since the Frame draws by depth
pub const SORT: u32 = 190; // over every panel (10 + 4 per rank) and theater's backdrop; Plasma's bar is 191, the cursor 200

const MPP: f64 = 0.000625; // follow: metres per unit (Plasma's 50 px panel is 31 mm high at OUT); fixed is further away, so bigger
const OUT: f64 = 0.7; // follow: this far ahead of where you look,
const BELOW: f64 = 0.45; // and this far below it, so 33° (atan(BELOW/OUT)) under the gaze, facing you
const SLACK: f64 = 3.0; // follow only starts after the head once it's this far off (degrees),
const SLACK_AT: f64 = 0.02; // or it's moved this far (m), so head jitter doesn't move it
const PITCH: f64 = 55.0; // the gaze pitch is clamped down to this (the bar's 33° below that, at your feet),
const PITCH_UP: f64 = 85.0; // and up to this
const EASE: f64 = 12.0; // the spring's rate (1/s): settles in about 0.4 s
const NEAR_YAW: f64 = 0.01; // follow's easing stops this close (degrees; 0.1 mm at the bar's edge),
const NEAR_AT: f64 = 3e-4; // and this close for the head position (m; 0.02 degrees from the eye)
const GAME_EVERY: Duration = Duration::from_millis(300); // how often we poll for a VR game (GetCurrentSceneProcessId)
const UNDER: f64 = 0.25; // fixed: below the lowest krdp panel's bottom edge
const WRIST_W: f64 = 0.22; // the wrist bar's width, but never under WRIST_MPP per unit (it widens instead)
// calibration knob: chip text size at the wrist (2.3 mm caps, read at 30-40 cm). ponytail:
// Plasma's bar's width counts too, so a crowded one widens past the forearm; add a second row if that bites
const WRIST_MPP: f64 = 0.00025;
const LOOK: f64 = 0.6; // wrist: shown while dot(its front, toward the eyes) is over this

/// What's on the chip row, left to right.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Chip {
    Panel(usize), // a remote machine's (cyan) or a Frame window's (violet) panel, by index
    More(usize),  // windows waiting for a slot
    Workspace,    // the Workspace window (and its Machines page): lit while either's open
    Prefs,        // the Preferences window's gear: lit while it's open
    Power,        // Close Desktop: lit (magenta, "Close?") while armed
}

/// The part of the frame at a point (`part_at`).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Part {
    Edge,          // the border, reach and padding, and between chips: carries it (fixed)
    Corner(usize), // indexed as grab::CORNER_SIGN: resizes it
    Knob,          // the curve knob
    Chip(Chip),
    Plasma(f64, f64), // Plasma's bar, as fractions right and up from its bottom left (its events get passed on)
}

/// What a press on a part starts. It lasts until you let go.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Take {
    Carry,
    Resize(usize),
    Curve,
}

/// What pressing `part` does in `mode` (a chip is a click, not a hold). Only fixed can be
/// carried, since follow and wrist place themselves, but all three resize and bend.
fn hold_for(part: Part, mode: Mode) -> Option<Take> {
    match part {
        Part::Edge if mode == Mode::Fixed => Some(Take::Carry),
        Part::Edge | Part::Chip(_) | Part::Plasma(..) => None,
        Part::Corner(k) => Some(Take::Resize(k)),
        Part::Knob => Some(Take::Curve),
    }
}

/// A chip as drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
struct State {
    chip: Chip,
    label: u32,  // its panel's tag change count, since a window's app name shows up a little later
    dim: bool,   // its panel is out of sight
    lit: bool,   // its panel has the keyboard
    hover: bool, // a laser or the cursor is on it
}

/// A chip with what it shows.
struct Item {
    s: State,
    accent: [f64; 3],
    label: Option<Arc<TagImg>>, // a panel's tag image
    text: String,               // otherwise glyphs ("+2")
}

/// How the frame's own parts look.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Looks {
    frame: Look,         // lit while a laser or the cursor is on it, Carry while carried or resized
    knob: Look,          // Carry while bending
    grip: Option<usize>, // the corner under a laser or being resized, which gets a thicker border
}

/// The frame's layout, in units from its top left.
#[derive(Clone, Debug, PartialEq)]
struct Frame {
    w: f64,
    h: f64,                      // all of it, reach included
    well: Option<[f64; 4]>,      // Plasma's bar: x, y, w, h (None: the session has none)
    row: f64,                    // the chip row's top
    laid: Vec<(Chip, f64, f64)>, // each chip's span
    knob: (f64, f64),            // the curve knob's centre
}

/// SetOverlayCurvature's value for a bar `width` wide on a circle of radius `r` (0: flat).
pub fn curvature(width: f64, r: f64) -> f32 {
    if r > 0.0 { (width / (2.0 * std::f64::consts::PI * r)).min(1.0) as f32 } else { 0.0 }
}

/// Each chip's span (x0, x1) from the row's start, GAP apart, plus the row's width. If they
/// don't fit in MAX_ROW, the panel chips all narrow by the same factor and their labels get clipped.
fn layout(chips: &[(Chip, f64)]) -> (Vec<(Chip, f64, f64)>, f64) {
    let flex: f64 = chips.iter().filter(|c| matches!(c.0, Chip::Panel(_))).map(|c| c.1).sum();
    let all = chips.iter().map(|c| c.1 + GAP).sum::<f64>() - GAP;
    let f = if all > MAX_ROW && flex > 0.0 { ((MAX_ROW - all + flex) / flex).max(0.1) } else { 1.0 };
    let mut x = 0.0;
    let laid = chips
        .iter()
        .map(|&(c, w)| {
            let w = if matches!(c, Chip::Panel(_)) { w * f } else { w };
            x += w + GAP;
            (c, x - w - GAP, x - GAP)
        })
        .collect();
    (laid, (x - GAP).max(0.0))
}

/// The frame around Plasma's bar (pw × ph, None if there's no panel) and the chips (their
/// widths). It's as wide as the wider of the two, and both are centred.
fn frame(plasma: Option<(f64, f64)>, chips: &[(Chip, f64)]) -> Frame {
    let (laid, row_w) = layout(chips);
    let cw = plasma.map_or(0.0, |p| p.0).max(row_w).max(MIN_ROW);
    let top = plasma.map_or(0.0, |p| p.1 + ROW_GAP);
    let (w, h) = (cw + 2.0 * (PAD + REACH), PAD + top + ROW + BOTTOM + 2.0 * REACH);
    let row = REACH + PAD + top;
    let x0 = (w - row_w) / 2.0;
    Frame {
        w,
        h,
        well: plasma.map(|(pw, ph)| [(w - pw) / 2.0, REACH + PAD, pw, ph]),
        row,
        laid: laid.into_iter().map(|(c, a, b)| (c, a + x0, b + x0)).collect(),
        knob: (w / 2.0, row + ROW + BOTTOM / 2.0),
    }
}

impl Frame {
    /// How far (units) Plasma's bar's middle is above the frame's.
    fn plasma_up(&self) -> f64 {
        self.well.map_or(0.0, |r| self.h / 2.0 - (r[1] + r[3] / 2.0))
    }
}

/// Texture pixels per unit for a frame w × h units scaled up k times (taskbar_scale, at
/// least 1). It's k, capped to what SteamVR takes as a raw upload (1920 a side, 1.5 MP), so a
/// bigger frame comes out sharper instead of stretched.
/// ponytail: under 1 (a Plasma panel 1860+ wide) edges and labels alias a little; supersample
/// if that shows. Plasma's own row is a stream at its own pixels, so it softens when scaled
/// up whatever we do.
pub(crate) fn tex_scale(w: f64, h: f64, k: f64) -> f64 {
    k.min(1920.0 / w).min((1.5e6 / (w * h)).sqrt())
}

/// The chip at (x, y), units from the frame's top left.
fn chip_at(f: &Frame, x: f64, y: f64) -> Option<Chip> {
    if !(f.row..=f.row + ROW).contains(&y) {
        return None;
    }
    f.laid.iter().find(|&&(_, x0, x1)| (x0..=x1).contains(&x)).map(|c| c.0)
}

/// The part at (x, y), units from the frame's top left. Plasma's bar's overlay sits in front
/// but takes no lasers (except for a popup, plasmabar.rs), so the frame passes on the ones
/// that land on its well.
fn part_at(f: &Frame, x: f64, y: f64) -> Option<Part> {
    if let Some(r) = f.well.filter(|r| x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3]) {
        return Some(Part::Plasma((x - r[0]) / r[2], (r[1] + r[3] - y) / r[3]));
    }
    if (x - f.knob.0).hypot(y - f.knob.1) <= 1.3 * KNOB {
        return Some(Part::Knob);
    }
    if let Some(c) = chip_at(f, x, y) {
        return Some(Part::Chip(c));
    }
    let near = REACH + CORNER;
    let sx = if x < near { -1.0 } else if x > f.w - near { 1.0 } else { 0.0 };
    let sy = if y < near { 1.0 } else if y > f.h - near { -1.0 } else { 0.0 };
    Some(grab::CORNER_SIGN.iter().position(|&c| c == (sx, sy)).map_or(Part::Edge, Part::Corner))
}

/// The point on the frame's surface (`bar`, kvm.rs's placement of it) under an overlay mouse
/// event at (x, y), its units right and up from its bottom left. SteamVR's mouse runs along the
/// curve, the way `Placement::hit` measures u.
fn frame_point(bar: &Placement, f: &Frame, x: f64, y: f64) -> V3 {
    let m = bar.on_surface((x / f.w - 0.5) * bar.width, (y / f.h - 0.5) * bar.height, 0.0);
    [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64]
}

/// The frame's look: Carry while carried or resized, and Lit under a laser or the cursor, but
/// only in fixed, where an edge carries it. Follow and wrist can't be carried, and lighting
/// the frame means a full draw (20-120 ms on the main loop). As follow's bar slides under a
/// resting laser or the mouse's cursor, those draws would come and go several times a second.
fn frame_look(mode: Mode, carried: bool, hover: Option<Part>, mouse_on: bool) -> Look {
    if carried {
        Look::Carry
    } else if mode == Mode::Fixed && (hover.is_some_and(|p| !matches!(p, Part::Plasma(..))) || mouse_on) {
        Look::Lit
    } else {
        Look::Rest
    }
}

/// The new scale when a corner drags a frame from w0 wide (at scale s0) to w wide. Both rows
/// grow together, about the centre.
fn rescale(s0: f64, w0: f64, w: f64) -> f64 {
    (s0 * w / w0.max(1e-6)).clamp(0.5, 3.0)
}

/// Where Plasma's overlay goes for our frame at `m` bent to radius r (0 is flat): its middle
/// u right and v up on the frame's surface (along the arc), FRONT toward you, facing the way
/// the surface does there. That keeps it on our cylinder wherever its crop moves it
/// (plasmabar.rs).
pub fn in_well(m: &Mat, r: f64, u: f64, v: f64) -> Mat {
    Placement::from_matrix(m, 1.0, 1.0, r).on_surface(u, v, FRONT)
}

/// A tag image (assets.rs) at LABEL, centred in a chip cw wide (left-aligned when clipped).
fn label_pixel(img: &TagImg, x: f64, y: f64, cw: f64) -> ([f64; 3], f64) {
    let (text, ih) = ((img.0 as f64 - TAG_ROOM) * LABEL, img.1 as f64 * LABEL);
    let (ox, oy) = (((cw - text) / 2.0).max(CHIP_PAD) - TAG_TEXT * LABEL, (ROW - ih) / 2.0);
    let (sx, sy, half) = ((x - ox) / LABEL, (y - oy) / LABEL, 0.5 / LABEL);
    if sx < 0.0 || sy < 0.0 || sx > img.0 as f64 || sy > img.1 as f64 {
        return ([0.0; 3], 0.0);
    }
    average(img, sx - half, sy - half, sx + half, sy + half)
}

/// A line of glyphs centred in a chip cw wide (`set` is the atlas's characters, in order).
pub(crate) fn text_pixel(glyphs: Option<&TagImg>, set: &str, text: &str, x: f64, y: f64, cw: f64) -> ([f64; 3], f64) {
    let Some(g) = glyphs else { return ([0.0; 3], 0.0) };
    let cell = g.0 as f64 / set.len() as f64;
    let n = text.chars().count() as f64;
    let (ox, oy) = ((cw - n * cell * LABEL) / 2.0, (ROW - g.1 as f64 * LABEL) / 2.0);
    let k = ((x - ox) / (cell * LABEL)).floor();
    let sy = (y - oy) / LABEL;
    let Some(gi) = text.chars().nth(k.max(0.0) as usize).and_then(|c| set.find(c)).filter(|_| k >= 0.0 && sy >= 0.0 && sy <= g.1 as f64) else {
        return ([0.0; 3], 0.0);
    };
    let (c0, half) = (gi as f64 * cell, 0.5 / LABEL);
    let sx = c0 + (x - ox - k * cell * LABEL) / LABEL;
    average(g, (sx - half).max(c0), sy - half, (sx + half).min(c0 + cell), sy + half)
}

/// One pixel of a chip cw wide at (x, y) units from its top left (the row's), in the panel's
/// tokens t with its accent fitted to them (acc). It copies Plasma 6's task manager: no tile at
/// rest, an accent tint with an accent outline under a laser, and a stronger tint for the one
/// with the keyboard. The panel's accent is the indicator line on the tile's bottom edge: full
/// width for the keyboard's, none while out of sight (the label dims then).
fn chip_pixel(it: &Item, t: &Tokens, acc: &Accent, glyphs: Option<&TagImg>, x: f64, y: f64, cw: f64) -> ([f64; 3], f64) {
    let s = it.s;
    let d = rbox(x - cw / 2.0, y - ROW / 2.0, cw / 2.0, TILE / 2.0, 4.0);
    let inside = (0.5 - d).clamp(0.0, 1.0);
    let tile = match (s.lit, s.hover) {
        (true, _) => (acc.fill.0, (acc.fill.1 * 1.5).min(1.0) * inside),
        (false, true) => (acc.fill.0, acc.fill.1 * inside),
        _ => (t.surface, 0.0),
    };
    let outline = (acc.line, if s.hover { line(d + 0.5, 0.5) } else { 0.0 });
    let iw = match s.chip {
        Chip::Panel(_) if s.dim => 0.0,
        Chip::Panel(_) if s.lit => cw,
        Chip::Panel(_) => 0.4 * cw,
        Chip::Workspace | Chip::Prefs | Chip::Power if s.lit => cw,
        Chip::More(_) | Chip::Workspace | Chip::Prefs | Chip::Power => 0.0,
    };
    let ind = if iw > 0.0 { (0.5 - rbox(x - cw / 2.0, y - (ROW + TILE) / 2.0 + 1.5, iw / 2.0, 1.5, 1.5)).clamp(0.0, 1.0) * inside } else { 0.0 };
    let ink = if s.dim && !s.hover { t.wdim } else { t.wtext }; // on the panel's surface, so Window's colours
    let text = tint(
        match &it.label {
            // the label ("Close?") only shows while armed; at rest it's just there to size the chip
            _ if s.chip == Chip::Power && !(s.lit && it.label.is_some()) => ([255.0, 0.0, 0.0], power(x - cw / 2.0, y - ROW / 2.0)),
            Some(img) => label_pixel(img, x, y, cw),
            None if s.chip == Chip::Prefs => ([255.0, 0.0, 0.0], gear(x - cw / 2.0, y - ROW / 2.0)), // a tag's text colour
            None => text_pixel(glyphs, GLYPHS, &it.text, x, y, cw),
        },
        ink,
        t.wdim,
        acc.line,
    );
    over(over(over(tile, outline), (acc.line, ind)), (text.0, text.1 * inside))
}

/// The gear's coverage at (x, y) units from its centre: 8 teeth (cos 8θ, squared off) round a hole.
fn gear(x: f64, y: f64) -> f64 {
    let r = x.hypot(y);
    let outer = 6.5 + 3.0 * (8.0 * y.atan2(x)).cos().clamp(-0.5, 0.5); // 5 in a gap, 8 on a tooth
    (outer - r + 0.5).clamp(0.0, 1.0) * (r - 2.5 + 0.5).clamp(0.0, 1.0)
}

/// The power glyph's coverage at (x, y) units from its centre (y down): a ring 1.6 thick
/// open at the top, a bar down through the gap into the middle.
fn power(x: f64, y: f64) -> f64 {
    let ring = (0.8 - (x.hypot(y) - 6.5).abs() + 0.5).clamp(0.0, 1.0) * if y < 0.0 && x.abs() < 3.5 { 0.0 } else { 1.0 };
    let bar = (0.8 - x.abs() + 0.5).clamp(0.0, 1.0) * if (-8.5..=0.5).contains(&y) { 1.0 } else { 0.0 };
    ring.max(bar)
}

/// A click on the power chip, `armed` since that instant (None if not). Returns (armed now,
/// close): the first click arms it, a second within ARM closes, and one after that arms it again.
fn press_power(armed: Option<Instant>, now: Instant) -> (Option<Instant>, bool) {
    match armed {
        Some(t) if now - t < ARM => (None, true),
        _ => (Some(now), false),
    }
}

/// What the frame is drawn from.
struct Scene<'a> {
    f: &'a Frame,
    items: &'a [Item],
    accs: Vec<Accent>, // each chip's accent, fitted to t
    acc: Accent,       // the frame's own: violet, the Frame's colour
    glyphs: Option<&'a TagImg>,
    t: &'a Tokens,
    lk: Looks,
}

impl<'a> Scene<'a> {
    fn new(f: &'a Frame, items: &'a [Item], glyphs: Option<&'a TagImg>, t: &'a Tokens, lk: Looks) -> Scene<'a> {
        Scene { f, items, accs: items.iter().map(|it| t.accent(it.accent)).collect(), acc: t.accent(grab::VIOLET), glyphs, t, lk }
    }

    /// One pixel at (x, y) units from the top left. From the bottom up: Plasma's panel surface,
/// opaque, inside a 1 u border with RADIUS corners (violet and glowing when lit, 2 u when
/// carried or resized); the corner under a laser thickened into a grip; a 1 u outline round
/// Plasma's bar; the chips; the knob.
    fn pixel(&self, x: f64, y: f64) -> ([f64; 3], f64) {
        let (f, t, acc, lk) = (self.f, self.t, &self.acc, self.lk);
        let (cx, cy) = (x - f.w / 2.0, y - f.h / 2.0);
        let d = rbox(cx, cy, f.w / 2.0 - REACH, f.h / 2.0 - REACH, RADIUS);
        let glow = if lk.frame != Look::Rest && d > 0.0 { acc.glow * (1.0 - d / GLOW).max(0.0).powi(2) } else { 0.0 };
        let half = if lk.frame == Look::Carry { 1.0 } else { 0.5 };
        let edge = if lk.frame == Look::Rest { t.border } else { acc.line };
        let mut out = over(over((acc.line, glow), (t.surface, (0.5 - d).clamp(0.0, 1.0))), (edge, line(d + half, half)));
        if let Some(k) = lk.grip {
            let (sx, sy) = grab::CORNER_SIGN[k];
            let near = REACH + CORNER;
            if sx * cx > f.w / 2.0 - near && -sy * cy > f.h / 2.0 - near {
                out = over(out, (acc.line, line(d + 1.5, 1.5)));
            }
        }
        if let Some(r) = f.well {
            let dw = rbox(x - r[0] - r[2] / 2.0, y - r[1] - r[3] / 2.0, r[2] / 2.0, r[3] / 2.0, 0.0);
            out = over(out, (t.border, line(dw - 0.5, 0.5))); // just outside it, so the seam looks intentional
        }
        if (f.row..=f.row + ROW).contains(&y) {
            for ((&(_, x0, x1), it), a) in f.laid.iter().zip(self.items).zip(&self.accs) {
                if x > x0 - 1.0 && x < x1 + 1.0 {
                    out = over(out, chip_pixel(it, t, a, self.glyphs, x - x0, y - f.row, x1 - x0));
                }
            }
        }
        let (kx, ky) = f.knob;
        if (x - kx).abs() < 2.0 * KNOB && (y - ky).abs() < 2.0 * KNOB {
            out = over(out, grab::knob_pixel(t, acc, lk.knob, x - kx, ky - y, KNOB, grab::Glyph::Curve, 1.0));
        }
        out
    }

    /// The texture at s pixels a unit.
    fn draw(&self, s: f64) -> Vec<u8> {
        grab::draw((self.f.w * s) as usize, (self.f.h * s) as usize, |x, y| self.pixel(x / s, y / s))
    }

    /// Draws chip n again into `buf` (as `draw(s)` gave it): its span of the row plus a pixel
    /// all round, exactly what a full draw would give there.
    fn repaint(&self, s: f64, buf: &mut [u8], n: usize) {
        let (tw, th) = ((self.f.w * s) as usize, (self.f.h * s) as usize);
        let (_, x0, x1) = self.f.laid[n];
        let lo = |u: f64| ((u * s).max(0.0) as usize).saturating_sub(1);
        let hi = |u: f64, n: usize| ((u * s).ceil() as usize + 1).min(n);
        for y in lo(self.f.row)..hi(self.f.row + ROW, th) {
            for x in lo(x0 - 1.0)..hi(x1 + 1.0, tw) {
                let (c, a) = self.pixel((x as f64 + 0.5) / s, (y as f64 + 0.5) / s);
                buf[(y * tw + x) * 4..][..4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, (a.clamp(0.0, 1.0) * 255.0) as u8]);
            }
        }
    }
}

/// Each chip's width: its label and CHIP_PAD either side (at most MAX_CHIP), or its glyphs'.
fn widths(items: &[Item], glyphs: Option<&TagImg>) -> Vec<(Chip, f64)> {
    let cell = glyphs.map_or(30.0, |g| g.0 as f64 / GLYPHS.len() as f64) * LABEL;
    items
        .iter()
        .map(|it| {
            let w = match it.s.chip {
                Chip::Panel(_) | Chip::Workspace => it.label.as_ref().map_or(120.0, |l| (l.0 as f64 - TAG_ROOM) * LABEL + 2.0 * CHIP_PAD).min(MAX_CHIP),
                Chip::More(_) => it.text.len() as f64 * cell + 2.0 * CHIP_PAD,
                Chip::Prefs => GEAR,
                // same width armed or not, so arming moves no chip (or what was under the laser) and only repaints this one
                Chip::Power => it.label.as_ref().map_or(GEAR, |l| ((l.0 as f64 - TAG_ROOM) * LABEL + 2.0 * CHIP_PAD).max(GEAR)),
            };
            (it.s.chip, w)
        })
        .collect()
}

/// An angle (degrees) into -180..180.
fn wrap(a: f64) -> f64 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// One dt step of a critically damped spring toward target: returns position and speed.
/// Within `near` and nearly still, it snaps there, because otherwise the tail keeps `moving`
/// (and the loop at the display's rate) for a second after anyone could see a difference.
fn ease(x: f64, v: f64, target: f64, dt: f64, near: f64) -> (f64, f64) {
    let v = v + (EASE * EASE * (target - x) - 2.0 * EASE * v) * dt;
    let x = x + v * dt;
    if (target - x).abs() < near && v.abs() < near * 10.0 { (target, 0.0) } else { (x, v) }
}

/// The way a head pose faces, level: its yaw.
fn yaw_of(m: &Mat) -> f64 {
    angles(&[-m[0][2] as f64, 0.0, -m[2][2] as f64]).0
}

/// Where a head pose looks: yaw and pitch, with pitch kept within -PITCH..PITCH_UP. The yaw is
/// the level direction the face is turned (forward cos - up sin), so it holds steady looking
/// straight up or down.
fn gaze(m: &Mat) -> (f64, f64) {
    let (f, u) = ([0, 1, 2].map(|i| -m[i][2] as f64), [0, 1, 2].map(|i| m[i][1] as f64));
    let (s, c) = (f[1].clamp(-1.0, 1.0), (1.0 - f[1] * f[1]).max(0.0).sqrt());
    let (yaw, _) = angles(&[0, 1, 2].map(|i| f[i] * c - u[i] * s));
    (yaw, s.asin().to_degrees().clamp(-PITCH, PITCH_UP))
}

/// Follow mode: the gaze (yaw, pitch) and head position the bar is placed for, easing after the head.
#[derive(Default)]
struct Follow {
    target: Option<(f64, f64, V3)>, // the head yaw, pitch and position it's heading for
    yaw: f64,
    pitch: f64,
    at: V3,
    speed: [f64; 5], // yaw, pitch, x, y, z
    chasing: bool,   // went past SLACK: tracks the head every frame until it's caught up
}

impl Follow {
    /// One frame with the head at `at` looking at (yaw, pitch). Returns what the bar is placed
/// for now. Past SLACK the head becomes the target every frame until it's caught up, and the
/// spring is the lag. `held` means a laser (or a press) is on it, so it stays put under it.
    fn step(&mut self, yaw: f64, pitch: f64, at: V3, dt: f64, held: bool) -> (f64, f64, V3) {
        let Some((ty, tp, tat)) = self.target else {
            (self.target, self.yaw, self.pitch, self.at) = (Some((yaw, pitch, at)), yaw, pitch, at);
            return (yaw, pitch, at);
        };
        let off = |(y, p, a): (f64, f64, V3), k: f64| norm(&[at[0] - a[0], at[1] - a[1], at[2] - a[2]]) > SLACK_AT * k || wrap(yaw - y).abs() > SLACK * k || (pitch - p).abs() > SLACK * k;
        self.chasing = !held && (self.chasing || off((ty, tp, tat), 1.0));
        if self.chasing {
            self.target = Some((yaw, pitch, at));
        }
        let (ty, tp, tat) = self.target.unwrap();
        (self.yaw, self.speed[0]) = ease(self.yaw, self.speed[0], self.yaw + wrap(ty - self.yaw), dt, NEAR_YAW); // the short way round
        (self.pitch, self.speed[1]) = ease(self.pitch, self.speed[1], tp, dt, NEAR_YAW);
        for i in 0..3 {
            (self.at[i], self.speed[i + 2]) = ease(self.at[i], self.speed[i + 2], tat[i], dt, NEAR_AT);
        }
        let slow = self.speed[..2].iter().all(|v| v.abs() < 1.0) && norm(&[self.speed[2], self.speed[3], self.speed[4]]) < 0.01;
        self.chasing &= !slow || off((self.yaw, self.pitch, self.at), 0.5); // caught up and slowed: it rests again
        (self.yaw, self.pitch, self.at)
    }
}

/// Follow's place for the head at `at` looking at (yaw, pitch): OUT ahead and BELOW under that
/// (the bottom of the view), facing the eye.
fn follow_pose(yaw: f64, pitch: f64, at: &V3) -> Pose {
    let pitch = pitch - BELOW.atan2(OUT).to_degrees();
    let d = direction(yaw, pitch);
    let r = OUT.hypot(BELOW);
    Pose { centre: [0, 1, 2].map(|i| at[i] + d[i] * r), yaw, pitch, ..Default::default() }
}

/// Fixed: UNDER below the lowest krdp panel's bottom edge, centred under their span as seen
/// from the eye (the leftmost and rightmost bottom corners), facing the eye. None if there
/// are no panels.
/// ponytail: uses the flat panel's corners (a curved one's are a little nearer), and the span
/// can't wrap behind you.
fn under(panels: &[Placement], eye: &V3) -> Option<Pose> {
    let corners: Vec<V3> = panels
        .iter()
        .flat_map(|p| [-1.0, 1.0].map(|s| [0, 1, 2].map(|k| p.c[k] + s * p.x[k] * p.width / 2.0 - p.y[k] * p.height / 2.0)))
        .collect();
    let yaw = |c: &V3| angles(&[c[0] - eye[0], c[1] - eye[1], c[2] - eye[2]]).0; // more is further left
    let left = corners.iter().max_by(|a, b| yaw(a).total_cmp(&yaw(b)))?;
    let right = corners.iter().min_by(|a, b| yaw(a).total_cmp(&yaw(b)))?;
    let low = corners.iter().map(|c| c[1]).fold(f64::INFINITY, f64::min);
    let centre = [(left[0] + right[0]) / 2.0, low - UNDER, (left[2] + right[2]) / 2.0];
    let (yaw, pitch) = angles(&[centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]]);
    Some(Pose { centre, yaw, pitch, ..Default::default() })
}

/// The hand the wrist bar is on: the one you don't point with.
fn off_hand(pointing: Hand) -> Hand {
    if pointing == Hand::Right { Hand::Left } else { Hand::Right }
}

/// Where the bar sits on `hand`'s controller (in its frame: x right, y up, -z where it points),
/// worn like a watch. It runs along the forearm behind the grip (text going elbow to hand) with
/// its face out of the back of the hand (-x on the left, +x on the right), so it faces you as
/// the wrist turns up. settings.json "taskbar_wrist": [x, y, z, tilt°] is the calibration knob
/// (x mirrored on the right; tilt turns the face about the forearm). ponytail: this uses the
/// controller's raw pose, not its grip component (the Frame's tip is angled, vr.rs); the knob
/// covers the difference.
fn wrist_local(hand: Hand) -> Mat {
    let k = config::settings()["taskbar_wrist"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_f64()).collect::<Vec<_>>())
        .filter(|v| v.len() == 4)
        .unwrap_or(vec![-0.04, 0.0, 0.15, 0.0]);
    let sx = if hand == Hand::Left { 1.0 } else { -1.0 };
    let (c, s) = (k[3].to_radians().cos(), k[3].to_radians().sin());
    // columns: x (0,0,-sx), y (0,-1,0) and z (-sx,0,0), turned by tilt about x
    let (y, z) = ([sx * s, -c, 0.0], [-sx * c, -s, 0.0]);
    let m = |r: usize, t: f64| [if r == 2 { -sx } else { 0.0 }, y[r], z[r], t].map(|v| v as f32);
    [m(0, k[0] * sx), m(1, k[1]), m(2, k[2])]
}

/// Is a bar at m (its front its z) turned toward the eye, by more than LOOK?
fn facing(m: &Mat, eye: &V3) -> bool {
    let to = [eye[0] - m[0][3] as f64, eye[1] - m[1][3] as f64, eye[2] - m[2][3] as f64];
    dot(&[m[0][2] as f64, m[1][2] as f64, m[2][2] as f64], &to) / norm(&to).max(1e-6) > LOOK
}

/// A press held on the frame: which device, and what it's doing. `lost` is when that device
/// went untracked; it lets go after grab::LOST.
struct Hold {
    dev: u32,
    kind: Kind,
    lost: Option<Instant>,
}

#[derive(Clone, Copy)]
enum Kind {
    Carry { rel: Mat },                                      // fixed: device -> frame
    Resize { k: usize, gx: f64, gy: f64, s0: f64, w0: f64 }, // the corner, where on it it was grabbed (m), and the scale and width (m) at that point
    Curve { local: [f64; 3], z0: f64, from: f64 },           // the point grabbed (in the device's frame), plus how far in front and the sagitta at that point
}

pub struct Taskbar {
    ov: vr::Handle,
    mode: Mode,
    glyphs: Option<TagImg>,
    pub(crate) names: Vec<Option<Arc<TagImg>>>, // each remote machine's chip label, by panel index
    machines: Option<Arc<TagImg>>,   // the Workspace chip's label
    close: Option<Arc<TagImg>>,      // the power chip's label while armed
    armed: Option<Instant>,          // the power chip was clicked then: a second click within ARM closes
    drawn: Option<(Vec<State>, Option<(i32, i32)>, Looks, u32, u32)>, // what was last drawn: the chips, Plasma's bar's size, the looks, the theme (theme::generation), the scale in quarters
    px: Vec<u8>,       // the texture as uploaded (empty if refused), patched one chip at a time
    drawn_at: Option<Instant>, // its last full draw
    pub plasma_time: Duration, // time spent on Plasma's bar this tick (plasmabar in the slow-tick line)
    frame: Frame,      // as uploaded
    mpp: f64,          // metres per unit right now
    pose: Option<Mat>, // fixed: where it is in the room (None until placed)
    follow: Follow,
    wrist: [Mat; 2],                   // on the left hand, then the right (wrist_local)
    wrist_dev: Option<(Hand, Option<u32>)>, // the controller it's on, as last logged
    hold: Option<Hold>,
    scale: f64,          // the user's size for both rows (settings.json taskbar_scale)
    curve: f64,          // the rows' curve radius in metres, 0 for flat (taskbar_curve)
    hover: Option<Part>, // the part under a laser (Plasma's bar included)
    placed: Option<(Mat, u32, u32, u32)>, // as last placed: its matrix, the device it's on (MAX means the room), its width and curve in mm
    shown: bool,
    alpha: f32,    // wrist: fading in while you look at it
    game: bool,    // a VR game is running
    carried: bool, // a krdp panel was being carried last frame
    moving: bool,  // held, hovered, moved or fading this frame (main.rs keeps the display's rate)
    game_at: Option<Instant>, // when we last polled for a VR game
    last: Instant,
}

impl Taskbar {
    pub fn new(assets: &str) -> Taskbar {
        let h = vr::create_overlay("controlcenter.taskbar", "Command Center taskbar").unwrap_or_else(|e| {
            eprintln!("taskbar: no overlay: {e}"); // SteamVR's 128 are shared, and they're used up
            0
        });
        call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_SendVRDiscreteScrollEvents, true);
        call!(ov, SetOverlaySortOrder, h, SORT);
        let mode = match config::settings()["taskbar"].as_str() {
            None => Mode::Fixed,
            Some(s) => Mode::parse(s).unwrap_or_else(|| {
                eprintln!("settings.json: taskbar {s:?} isn't fixed, follow or wrist; using fixed");
                Mode::Fixed
            }),
        };
        eprintln!("taskbar: {}", mode.name());
        let names = panels()
            .iter()
            .map(|p| matches!(p.src, Source::Rdp).then(|| crate::load_tag(&format!("{assets}/tag-chip-{}.rgba", p.v.name)).map(Arc::new)).flatten())
            .collect();
        Taskbar {
            ov: h,
            mode,
            glyphs: crate::load_tag(&format!("{assets}/glyphs.rgba")),
            names,
            machines: crate::load_tag(&format!("{assets}/tag-ui-workspace.rgba")).map(Arc::new),
            close: crate::load_tag(&format!("{assets}/tag-ui-close.rgba")).map(Arc::new),
            armed: None,
            drawn: None,
            px: Vec::new(),
            drawn_at: None,
            plasma_time: Duration::ZERO,
            frame: frame(None, &[]),
            mpp: MPP,
            pose: None,
            follow: Follow::default(),
            wrist: [Hand::Left, Hand::Right].map(wrist_local),
            wrist_dev: None,
            hold: None,
            scale: config::settings()["taskbar_scale"].as_f64().map_or(1.0, |s| s.clamp(0.5, 3.0)),
            curve: config::settings()["taskbar_curve"].as_f64().filter(|r| *r > 0.0).unwrap_or(0.0),
            hover: None,
            placed: None,
            shown: false,
            alpha: 1.0,
            game: false,
            carried: false,
            moving: false,
            game_at: None,
            last: Instant::now(),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Sets remote i's chip label (its tag was redrawn after an add or a rename) and redraws the bar.
    pub(crate) fn set_name(&mut self, i: usize, tag: Option<Arc<TagImg>>) {
        (self.names[i], self.drawn) = (tag, None);
    }

    /// Switches placement to m now and saves it to settings.json (`taskbar <mode>`, Preferences).
    /// Err means it switched but the setting wasn't written.
    pub fn choose(&mut self, m: Mode) -> Result<(), String> {
        self.set_mode(m);
        config::set_setting("taskbar", serde_json::json!(m.name()))
    }

    /// Switch placement now.
    fn set_mode(&mut self, m: Mode) {
        (self.mode, self.hold, self.placed, self.pose, self.follow) = (m, None, None, None, Follow::default());
        self.alpha = if m == Mode::Wrist { 0.0 } else { 1.0 };
        call!(ov, SetOverlayAlpha, self.ov, self.alpha);
        eprintln!("taskbar: {}", m.name());
    }

    /// The curve radius as drawn. It's the user's, but like a panel's (grab.rs) it never wraps
    /// past a half circle as the frame grows (Plasma's panel fits its apps, and a corner scales it).
    fn radius(&self) -> f64 {
        grab::curve_for_width(self.curve, self.frame.w * self.mpp)
    }

    fn carrying(&self) -> bool {
        matches!(self.hold, Some(Hold { kind: Kind::Carry { .. }, .. }))
    }

    /// `place taskbar ...` (cc-home applying a spot): moves it there, if it's fixed.
    pub fn place(&mut self, p: &Pose) {
        if self.mode == Mode::Fixed && !self.carrying() {
            self.set_fixed(p);
        }
    }

    /// Fixed: its own spot if it has one, else under the krdp panels, else (no panels) wherever
    /// follow would put it right now, frozen. Never while it's being carried.
    pub fn refix(&mut self) {
        if self.mode != Mode::Fixed || self.carrying() {
            return;
        }
        if let Some(p) = config::home_pose("taskbar", None) {
            return self.set_fixed(&p);
        }
        let eye = vr::head_position();
        let rdp: Vec<Placement> = {
            let k = KVM.lock().unwrap();
            panels().iter().filter(|p| matches!(p.src, Source::Rdp) && p.used()).map(|p| k.place[p.index]).collect()
        };
        let pose = match under(&rdp, &eye) {
            Some(p) => p,
            None => {
                let Some(h) = vr::head() else { return }; // no headset pose yet: place_now asks again
                follow_pose(yaw_of(&h), 0.0, &eye)
            }
        };
        self.set_fixed(&pose);
    }

    fn set_fixed(&mut self, p: &Pose) {
        self.pose = Some(panel_matrix(p));
        self.fit();
    }

    /// Fixed: scales its units for its distance, so it looks as big as follow's.
    fn fit(&mut self) {
        let Some(m) = self.pose else { return };
        let eye = vr::head_position();
        let d = norm(&[m[0][3] as f64 - eye[0], m[1][3] as f64 - eye[1], m[2][3] as f64 - eye[2]]);
        self.mpp = MPP * (d / OUT).max(1.0) * self.scale;
    }

    /// Held, under a laser, or moving (follow's easing) or fading (wrist) this frame.
    pub fn moving(&self) -> bool {
        self.moving
    }

    /// A VR game is running (polled every GAME_EVERY), so every stream pauses (attention.rs).
    pub fn game(&self) -> bool {
        self.game
    }

    /// Runs every frame: its events and the mouse's clicks, its chips, where it goes, whether it
    /// shows, and Plasma's bar in its frame.
    pub fn tick(&mut self, grab: &mut grab::Grab, windows: &mut Windows) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f64().min(1.0 / 30.0); // ease's Euler step goes unstable past ~0.07 s and rings near that
        self.last = now;
        let before = (self.placed, self.alpha);
        if self.game_at.is_none_or(|t| now - t >= GAME_EVERY) {
            self.game_at = Some(now); // by time, since the idle loop only ticks every 40 ms
            let game = call!(apps, GetCurrentSceneProcessId) != 0;
            if game != self.game {
                self.game = game;
                eprintln!("taskbar: {}", if game { "a VR game is running: hidden" } else { "no VR game: back" });
            }
        }
        self.events(grab, windows);
        // Let go over a panel, a card or Plasma's bar in front of it, and that overlay got the
        // release (and a release over a panel or card ends a press on Plasma's bar)
        if !grab.released.is_empty() {
            windows.plasma_release();
        }
        let released: Vec<u32> = grab.released.drain(..).chain(windows.plasma_released()).collect();
        if self.hold.as_ref().is_some_and(|h| released.contains(&h.dev)) {
            self.let_go();
        }
        self.holding(now);
        // The mouse's cursor on it: its hover, and its clicks.
        let (on, clicks) = {
            let mut k = KVM.lock().unwrap();
            (k.on_bar.filter(|_| self.shown), std::mem::take(&mut k.bar_clicks)) // (stale while it's hidden)
        };
        let mouse = on.and_then(|(u, v)| chip_at(&self.frame, u / self.mpp + self.frame.w / 2.0, self.frame.h / 2.0 - v / self.mpp));
        if let Some(c) = mouse.filter(|_| clicks > 0) {
            self.click(c, grab, windows);
        }
        // A krdp panel was put down, so fixed goes back under them.
        let carried = panels().iter().any(|p| matches!(p.src, Source::Rdp) && grab.busy(p.index));
        if self.carried && !carried {
            self.refix();
        }
        self.carried = carried;
        // Fixed is usually further away than theater's backdrop (2 m), and on the Frame depth, not
        // sort order, decides what's on top (grab.rs), so it hides then. Follow and wrist are nearer.
        let theater = crate::THEATER.load(Relaxed) != usize::MAX;
        let want = !self.game && !HIDDEN.load(Relaxed) && !(self.mode == Mode::Fixed && theater);
        if want {
            self.paint(windows, mouse, on.is_some());
        }
        let at = if want { self.place_now(dt, on.is_some()) } else { None };
        if at.is_none() && self.hold.is_some() {
            self.let_go(); // hidden while held: it stays where it is
        }
        if let Some((_, local, dev)) = at {
            self.put(&local, dev);
        }
        // Plasma's bar in its well, in front. Below alpha 1 it would show as a tinted sheet
        // (IgnoreTextureAlpha), so it's hidden while the wrist fades.
        let (up, r) = (self.frame.plasma_up() * self.mpp, self.radius());
        let t = Instant::now();
        windows.tick_plasma(at.filter(|_| self.alpha >= 0.99), up, self.mpp, r);
        self.plasma_time = t.elapsed();
        if at.is_some() != self.shown {
            self.shown = at.is_some();
            if self.shown { call!(ov, ShowOverlay, self.ov) } else { call!(ov, HideOverlay, self.ov) };
        }
        let (w, h) = (self.frame.w, self.frame.h);
        let mut k = KVM.lock().unwrap();
        k.bar = at.map(|(m, ..)| Placement::from_matrix(&m, w * self.mpp, h / w, r));
        k.bar_inset = REACH * self.mpp; // for the cursor; the clear reach does nothing for it
        self.moving = self.hold.is_some() || self.hover.is_some() || (self.placed, self.alpha) != before;
    }

    /// Laser events on it: hovering, clicking a chip, taking an edge, a corner or the knob.
    fn events(&mut self, grab: &mut grab::Grab, windows: &mut Windows) {
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        while call!(ov, PollNextOverlayEvent, self.ov, &mut e, size_of::<vr::VREvent_t>() as u32) {
            let (m, dev) = (unsafe { e.data.mouse }, e.trackedDeviceIndex);
            let y = self.frame.h - m.y as f64; // its units, from the top
            let mut part = part_at(&self.frame, m.x as f64, y);
            // Plasma's bar is 2 mm in front of us (FRONT), so a slanted laser goes through it a
            // few pixels from where it hits our well, or over its edge while it hits our padding.
            // On it and where goes by that, the way the mouse's ray sees it (plasmabar.rs).
            let pointer = [sys::EVREventType_VREvent_MouseMove, sys::EVREventType_VREvent_MouseButtonDown, sys::EVREventType_VREvent_MouseButtonUp].contains(&e.eventType);
            let bar = if pointer && self.frame.well.is_some() { KVM.lock().unwrap().bar } else { None }; // (unlocked again: laser_on_panel takes it)
            if let Some(bar) = bar
                && let Some(l) = vr::laser_pose(dev)
                && let Some(on) = crate::plasmabar::laser_on_panel(&[l[0][3] as f64, l[1][3] as f64, l[2][3] as f64], &frame_point(&bar, &self.frame, m.x as f64, m.y as f64))
            {
                part = match (on, part) {
                    (Some((fx, fy)), _) => Some(Part::Plasma(fx, fy)),
                    (None, Some(Part::Plasma(..))) => Some(Part::Edge), // our well's rim, showing past the bar
                    (None, p) => p,
                };
            }
            let was = matches!(self.hover, Some(Part::Plasma(..)));
            // on Plasma's bar, pass its events on (a scroll goes by where the laser last was)
            match (e.eventType, part, self.hover) {
                (sys::EVREventType_VREvent_ScrollDiscrete, _, Some(Part::Plasma(fx, fy)))
                | (sys::EVREventType_VREvent_MouseMove | sys::EVREventType_VREvent_MouseButtonDown | sys::EVREventType_VREvent_MouseButtonUp, Some(Part::Plasma(fx, fy)), _) => {
                    windows.plasma_laser(&e, fx, fy)
                }
                _ => {}
            }
            match e.eventType {
                sys::EVREventType_VREvent_MouseMove | sys::EVREventType_VREvent_FocusLeave => {
                    self.hover = part.filter(|_| e.eventType == sys::EVREventType_VREvent_MouseMove);
                    if was && !matches!(self.hover, Some(Part::Plasma(..))) {
                        let up = e.eventType == sys::EVREventType_VREvent_MouseMove && self.frame.well.is_some_and(|r| y < r[1]);
                        if up && crate::plasmabar::OPEN.load(Relaxed) {
                            crate::plasmabar::TIP.store(true, Relaxed); // moved up into its tooltip (a preview), which Plasma's overlay takes
                        } else {
                            crate::windows::leave(); // off Plasma's bar: its hover ends (tooltips close)
                        }
                    }
                }
                sys::EVREventType_VREvent_MouseButtonDown if m.button == sys::EVRMouseButton_VRMouseButton_Left => {
                    if vr::is_real_controller(dev) {
                        let mut k = KVM.lock().unwrap();
                        if k.awake {
                            k.set_awake(false); // the last device pressed is primary (R-2)
                        }
                    }
                    if !matches!(part, Some(Part::Plasma(..))) {
                        crate::windows::shell_outside(); // a click outside Plasma's open popup
                    }
                    match part {
                        Some(Part::Chip(c)) => self.click(c, grab, windows),
                        Some(p) if self.hold.is_none() => {
                            if let Some(t) = hold_for(p, self.mode) {
                                self.take(t, dev);
                            }
                        }
                        _ => {}
                    }
                }
                // SteamVR sends a release to the overlay under the laser, not the one pressed, so a
                // press on Plasma's bar or a panel that's let go over the frame ends here too
                sys::EVREventType_VREvent_MouseButtonUp => {
                    windows.plasma_release();
                    grab.end_by(dev, &KVM.lock().unwrap());
                    if self.hold.as_ref().is_some_and(|h| h.dev == dev) {
                        self.let_go();
                    }
                }
                // the wheel or joystick while bending: 3 cm more (or less) curve per notch
                sys::EVREventType_VREvent_ScrollDiscrete => {
                    let n = unsafe { e.data.scroll.ydelta } as f64;
                    if let Some(Hold { kind: Kind::Curve { from, .. }, .. }) = self.hold.as_mut() {
                        *from = (*from + 0.03 * n).max(0.0);
                    }
                }
                _ => {}
            }
        }
    }

    /// ponytail: "+N" does nothing ("windows" lists the waiting windows); no long-press Close yet.
    fn click(&mut self, c: Chip, grab: &mut grab::Grab, windows: &mut Windows) {
        // (the window opens or closes on its own tick)
        if c == Chip::Workspace {
            use crate::control::{machines, show, workspace};
            if machines::OPEN.load(Relaxed) || workspace::OPEN.load(Relaxed) {
                show(&machines::OPEN, false);
                show(&workspace::OPEN, false);
            } else {
                show(&workspace::OPEN, true);
            }
        }
        if c == Chip::Prefs {
            crate::control::show(&crate::control::prefs::OPEN, !crate::control::prefs::OPEN.load(Relaxed));
        }
        if c == Chip::Power {
            let quit;
            (self.armed, quit) = press_power(self.armed, Instant::now());
            if quit {
                crate::close_desktop("taskbar");
            } else {
                eprintln!("taskbar: Close Desktop armed: click again within {} s", ARM.as_secs());
            }
        }
        if let Chip::Panel(i) = c {
            let p = crate::panel(i);
            if matches!(p.src, crate::Source::Rdp) && !p.live() {
                crate::control::connect_later(i); // left out at start (autoconnect), so its dim chip connects it
            } else {
                windows.toggle(i, grab);
            }
        }
    }

    /// A press on an edge, a corner or the knob by a device's laser: held from now.
    fn take(&mut self, t: Take, dev: u32) {
        let Some(pl) = KVM.lock().unwrap().bar else { return };
        let kind = match t {
            Take::Carry => {
                let (Some(d), Some(b)) = (grab::pose_of(dev), self.pose) else { return };
                Kind::Carry { rel: mul(&inv_rigid(&d), &b) }
            }
            Take::Resize(k) => {
                let (sx, sy) = grab::CORNER_SIGN[k];
                let Some((u, v)) = grab::laser_on(&pl, dev) else { return };
                Kind::Resize { k, gx: u - sx * pl.width / 2.0, gy: v - sy * pl.height / 2.0, s0: self.scale, w0: pl.width }
            }
            Take::Curve => {
                let Some((local, z0)) = grab::taken(&pl, dev) else { return };
                let from = if pl.curve > 0.0 { pl.curve * (1.0 - (pl.width / (2.0 * pl.curve)).cos()) } else { 0.0 };
                Kind::Curve { local, z0, from }
            }
        };
        eprintln!("taskbar: {t:?} by device {dev}");
        self.hold = Some(Hold { dev, kind, lost: None });
    }

    /// While held: carried on the device, resized by where the laser puts the corner, or bent
    /// by how far the grabbed point has come toward you (along the frame's normal).
    fn holding(&mut self, now: Instant) {
        let Some(h) = self.hold.as_mut() else { return };
        let Some(d) = grab::pose_of(h.dev) else {
            if now - *h.lost.get_or_insert(now) > grab::LOST {
                self.let_go(); // lost tracking: put it down where it was
            }
            return;
        };
        h.lost = None;
        let (dev, kind) = (h.dev, h.kind);
        let Some(pl) = KVM.lock().unwrap().bar else { return };
        match kind {
            Kind::Carry { rel } => self.pose = Some(mul(&d, &rel)),
            Kind::Resize { k, gx, gy, s0, w0 } => {
                let (sx, sy) = grab::CORNER_SIGN[k];
                let Some((u, v)) = grab::laser_on(&pl, dev) else { return };
                self.scale = rescale(s0, w0, grab::width_for(sx, sy, u - gx, v - gy, pl.height / pl.width));
                if self.mode == Mode::Fixed {
                    self.fit();
                }
            }
            Kind::Curve { local, z0, from } => self.curve = grab::radius_for(from + grab::depth_of(&pl, &d, &local) - z0, pl.width),
        }
    }

    /// Let go. A carried frame saves its spot, a resized one saves taskbar_scale, and a bent one
    /// taskbar_curve (settings.json).
    fn let_go(&mut self) {
        let Some(h) = self.hold.take() else { return };
        let (key, v) = match h.kind {
            Kind::Carry { .. } => return self.put_down(),
            Kind::Resize { .. } => ("taskbar_scale", (self.scale * 1000.0).round() / 1000.0),
            Kind::Curve { .. } => ("taskbar_curve", (self.curve * 1000.0).round() / 1000.0),
        };
        match config::set_setting(key, serde_json::json!(v)) {
            Ok(()) => eprintln!("taskbar: {key} {v}, saved"),
            Err(e) => eprintln!("taskbar: {key} {v}, but {e}"),
        }
    }

    /// Put down after a carry: it stays, and its place is saved as spots.home.taskbar.
    fn put_down(&mut self) {
        let Some(m) = self.pose else { return };
        let (w, h) = (self.frame.w * self.mpp, self.frame.h * self.mpp);
        let pose = Placement::from_matrix(&m, w, h / w, 0.0).pose();
        match config::save_home_pose("taskbar", None, &pose, h, None) {
            Ok(()) => eprintln!("taskbar: placed, saved to home (its spot from now on)"),
            Err(e) => eprintln!("taskbar: placed, but saving home failed: {e}"),
        }
    }

    /// Draws its frame and chips, but only again when something on it changed.
    /// ponytail: labels are app names (tags); no Icon= icons, and no caption on hover yet.
    fn paint(&mut self, windows: &Windows, mouse: Option<Chip>, mouse_on: bool) {
        let (kbd, engaged) = {
            let k = KVM.lock().unwrap();
            (if k.kbd_shell { usize::MAX } else { k.kbd }, k.engaged)
        };
        let mut chips = Vec::new();
        for p in panels() {
            let slotted = match &p.src {
                Source::Rdp => p.used() && crate::config::member(&p.v), // (not a spare or removed, machines.rs, and in this workspace)
                Source::Window(w) => w.lock().unwrap().is_some(),
            };
            if slotted {
                chips.push(Chip::Panel(p.index));
            }
        }
        if windows.waiting() > 0 {
            chips.push(Chip::More(windows.waiting()));
        }
        chips.push(Chip::Workspace);
        chips.push(Chip::Prefs);
        chips.push(Chip::Power);
        let armed = self.armed.is_some_and(|t| t.elapsed() < ARM);
        let hover = match self.hover {
            Some(Part::Chip(c)) => Some(c),
            _ => mouse,
        };
        let states: Vec<State> = chips
            .iter()
            .map(|&chip| {
                let (label, dim, lit) = match chip {
                    Chip::Panel(i) => {
                        let p = panel(i);
                        (p.tag().0, p.away() || !p.live(), engaged && kbd == i && p.live() && !p.away())
                    }
                    Chip::More(_) => (0, false, false),
                    Chip::Workspace => (0, false, crate::control::machines::OPEN.load(Relaxed) || crate::control::workspace::OPEN.load(Relaxed)),
                    Chip::Prefs => (0, false, crate::control::prefs::OPEN.load(Relaxed)),
                    Chip::Power => (0, false, armed),
                };
                State { chip, label, dim, lit, hover: hover == Some(chip) }
            })
            .collect();
        let held = self.hold.as_ref().map(|h| &h.kind);
        let looks = Looks {
            frame: frame_look(self.mode, matches!(held, Some(Kind::Carry { .. } | Kind::Resize { .. })), self.hover, mouse_on),
            knob: match held {
                Some(Kind::Curve { .. }) => Look::Carry,
                _ if self.hover == Some(Part::Knob) => Look::Lit,
                _ => Look::Rest,
            },
            grip: match (held, self.hover) {
                (Some(&Kind::Resize { k, .. }), _) | (None, Some(Part::Corner(k))) => Some(k),
                _ => None,
            },
        };
        let plasma = windows.plasma_size();
        let quarters = (self.scale.max(1.0) * 4.0).round() as u32; // a resize redraws a quarter step at a time
        let key = (states, plasma, looks, theme::generation(), quarters);
        if self.drawn.as_ref() == Some(&key) {
            return;
        }
        let items: Vec<Item> = key
            .0
            .iter()
            .map(|&s| {
                let (accent, label, text) = match s.chip {
                    Chip::Panel(i) => {
                        let p = panel(i);
                        let label = if matches!(p.src, Source::Rdp) && p.v.pop.is_none() { self.names[i].clone() } else { p.tag().1 };
                        (p.accent, label, String::new())
                    }
                    Chip::More(n) => (grab::VIOLET, None, format!("+{n}")),
                    Chip::Workspace => (grab::REMOTE, self.machines.clone(), String::new()),
                    Chip::Prefs => (grab::VIOLET, None, String::new()),
                    Chip::Power => (if s.lit { theme::MAGENTA } else { grab::VIOLET }, self.close.clone(), String::new()),
                };
                Item { s, accent, label, text }
            })
            .collect();
        let f = frame(plasma.map(|(w, h)| (w as f64, h as f64)), &widths(&items, self.glyphs.as_ref()));
        let s = tex_scale(f.w, f.h, quarters as f64 / 4.0);
        let shell = theme::get().shell;
        let scene = Scene::new(&f, &items, self.glyphs.as_ref(), &shell, looks);
        let (tw, th) = ((f.w * s) as usize, (f.h * s) as usize);
        // Only chip looks changed (a laser sliding along them, the keyboard moving, one going out
        // of sight) and the layout and everything else is as drawn, so just redraw those chips.
        if let Some(d) = self.drawn.as_ref().filter(|d| (&d.1, &d.2, d.3, d.4) == (&key.1, &key.2, key.3, key.4) && d.0.len() == key.0.len() && f == self.frame && !self.px.is_empty()) {
            for n in (0..key.0.len()).filter(|&n| d.0[n] != key.0[n]) {
                scene.repaint(s, &mut self.px, n);
            }
            if !grab::set_raw(self.ov, &self.px, tw, th) {
                self.px.clear();
            }
            self.drawn = Some(key);
            return;
        }
        // A full draw, at most every 100 ms. A window opening brings its chip, live, its tag and
        // Plasma's bar's new width within a few hundred ms, so this draws once or twice, not four times.
        let now = Instant::now();
        if self.drawn_at.is_some_and(|t| now - t < Duration::from_millis(100)) {
            return; // (not drawn, so it tries again next frame)
        }
        self.drawn_at = Some(now);
        self.px = vr::timed(|| format!("taskbar: drawn {tw}x{th}"), || scene.draw(s));
        if grab::set_raw(self.ov, &self.px, tw, th) {
            let mut scale = sys::HmdVector2_t { v: [f.w as f32, f.h as f32] }; // events in its units
            call!(ov, SetOverlayMouseScale, self.ov, &mut scale);
            self.frame = f;
        } else {
            self.px.clear();
        }
        self.drawn = Some(key); // even if refused, so it tries again on the next change, not every frame
    }

    /// Where the frame goes this frame: its matrix in the room, its matrix on the device it's on,
    /// and that device (MAX for the room). None means it isn't shown now (the wrist is out of
    /// sight, or untracked).
    fn place_now(&mut self, dt: f64, mouse_on: bool) -> Option<(Mat, Mat, u32)> {
        Some(match self.mode {
            Mode::Fixed => {
                if self.pose.is_none() {
                    self.refix();
                }
                let m = self.pose?;
                (m, m, u32::MAX)
            }
            Mode::Follow => {
                self.mpp = MPP * self.scale;
                let h = vr::head()?;
                let (yaw, pitch) = gaze(&h);
                // also hold still under the mouse's cursor, since chips sliding under it would each repaint
                let held = self.hover.is_some() || self.hold.is_some() || mouse_on;
                let (yaw, pitch, at) = self.follow.step(yaw, pitch, [h[0][3] as f64, h[1][3] as f64, h[2][3] as f64], dt, held);
                let m = panel_matrix(&follow_pose(yaw, pitch, &at));
                (m, m, u32::MAX)
            }
            Mode::Wrist => {
                self.mpp = (WRIST_W / self.frame.w).max(WRIST_MPP); // its own size, not the fixed bar's corner scale
                // the off hand (laser::dominant, which Preferences changes live)
                let hand = off_hand(laser::dominant());
                let role = if hand == Hand::Left {
                    sys::ETrackedControllerRole_TrackedControllerRole_LeftHand
                } else {
                    sys::ETrackedControllerRole_TrackedControllerRole_RightHand
                };
                let dev = call!(sys, GetTrackedDeviceIndexForControllerRole, role);
                // (not our cc_pointer, which takes that hand's role while the mouse borrows it)
                let real = dev != sys::k_unTrackedDeviceIndexInvalid as u32 && vr::is_real_controller(dev);
                if self.wrist_dev != Some((hand, real.then_some(dev))) {
                    self.wrist_dev = Some((hand, real.then_some(dev)));
                    if real { eprintln!("taskbar: wrist on device {dev} ({hand:?} hand)") } else { eprintln!("taskbar: wrist: no {hand:?} controller") };
                }
                let local = self.wrist[(hand == Hand::Right) as usize];
                let world = real.then(|| grab::pose_of(dev)).flatten().map(|d| mul(&d, &local));
                let facing = world.is_some_and(|m| facing(&m, &vr::head_position()));
                let alpha = (self.alpha + if facing { 0.15 } else { -0.08 }).clamp(0.0, 1.0);
                if alpha != self.alpha {
                    self.alpha = alpha;
                    call!(ov, SetOverlayAlpha, self.ov, alpha);
                }
                // fully faded it's hidden, not just see-through, because an invisible overlay still catches lasers
                if alpha <= 0.0 {
                    return None;
                }
                (world?, local, dev)
            }
        })
    }

    /// Puts the frame at `local` on device `dev` (MAX for the room), only when that changed.
    fn put(&mut self, local: &Mat, dev: u32) {
        let (width, r) = (self.frame.w * self.mpp, self.radius());
        let placed = Some((*local, dev, (width * 1000.0).round() as u32, (r * 1000.0).round() as u32));
        if self.placed != placed {
            self.placed = placed;
            call!(ov, SetOverlayWidthInMeters, self.ov, width as f32);
            call!(ov, SetOverlayCurvature, self.ov, curvature(width, r));
            if dev == u32::MAX {
                vr::place(self.ov, local);
            } else {
                let mut t = sys::HmdMatrix34_t { m: *local };
                call!(ov, SetOverlayTransformTrackedDeviceRelative, self.ov, dev, &mut t);
            }
        }
    }

    pub fn destroy(&self) {
        crate::gpu::forget(self.ov);
        call!(ov, DestroyOverlay, self.ov);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLASMA: (f64, f64) = (891.0, 50.0); // Plasma's floating panel as measured

    #[test]
    fn ease_lands_on_its_target() {
        // a 35 degree turn and a 0.6 m walk at 90 Hz: lands exactly, and still, inside 1.1 s (no 2 s tail)
        for (to, near) in [(35.0, NEAR_YAW), (0.6, NEAR_AT)] {
            let (mut x, mut v, mut n) = (0.0, 0.0, 0);
            while (x, v) != (to, 0.0) {
                (x, v) = ease(x, v, to, 1.0 / 90.0, near);
                n += 1;
                assert!(n < 100, "still easing after {n} steps: {x} {v}");
            }
        }
    }

    #[test]
    fn chips_lay_out_and_narrow_when_crowded() {
        let (laid, w) = layout(&[(Chip::Panel(0), 150.0), (Chip::Panel(5), 120.0), (Chip::More(2), 40.0)]);
        assert_eq!(w, 150.0 + 120.0 + 40.0 + 2.0 * GAP);
        assert_eq!(laid[0], (Chip::Panel(0), 0.0, 150.0));
        for pair in laid.windows(2) {
            assert!((pair[1].1 - pair[0].2 - GAP).abs() < 1e-9, "{pair:?}");
        }
        assert_eq!(layout(&[]).1, 0.0);
        // Too many to fit: the panel chips narrow, "+N" keeps its size.
        let many: Vec<(Chip, f64)> = (0..20).map(|i| (Chip::Panel(i), MAX_CHIP)).chain([(Chip::More(3), 40.0), (Chip::Workspace, 90.0)]).collect();
        let (laid, w) = layout(&many);
        assert!((w - MAX_ROW).abs() < 1e-6, "{w}");
        let (more, machines) = (laid[laid.len() - 2], laid[laid.len() - 1]);
        assert!((more.2 - more.1 - 40.0).abs() < 1e-9 && (laid[0].2 - laid[0].1) < MAX_CHIP);
        assert!(machines.0 == Chip::Workspace && (machines.2 - machines.1 - 90.0).abs() < 1e-9, "Machines keeps its width, last");
    }

    #[test]
    fn frame_lays_out_around_plasma() {
        // Plasma's bar wider than the chips: the frame is Plasma's width, both centred.
        let chips = [(Chip::Panel(0), 150.0), (Chip::Panel(1), 120.0)];
        let f = frame(Some(PLASMA), &chips);
        assert_eq!((f.w, f.h), (891.0 + 2.0 * PAD + 2.0 * REACH, PAD + 50.0 + ROW_GAP + ROW + BOTTOM + 2.0 * REACH));
        assert_eq!(f.well, Some([REACH + PAD, REACH + PAD, 891.0, 50.0]));
        let row_mid = (f.laid[0].1 + f.laid[1].2) / 2.0;
        assert!((row_mid - f.w / 2.0).abs() < 1e-9 && f.knob.0 == f.w / 2.0, "centred");
        assert_eq!(f.row, REACH + PAD + 50.0 + ROW_GAP);
        assert!(f.knob.1 + KNOB <= f.h - REACH && f.knob.1 - KNOB >= f.row + ROW, "the knob is inside, under the chips");
        // Plasma's bar's middle above the frame's
        assert!((f.plasma_up() - (f.h / 2.0 - (REACH + PAD + 25.0))).abs() < 1e-9 && f.plasma_up() > 0.0);
        // Chips wider than Plasma's bar: the frame is theirs, Plasma's centred over them.
        let wide: Vec<(Chip, f64)> = (0..5).map(|i| (Chip::Panel(i), 250.0)).collect();
        let f = frame(Some(PLASMA), &wide);
        assert_eq!(f.w, 5.0 * 250.0 + 4.0 * GAP + 2.0 * (PAD + REACH));
        assert_eq!(f.laid[0].1, REACH + PAD);
        assert_eq!(f.well.unwrap()[0], (f.w - 891.0) / 2.0);
        // No Plasma panel: the chip row alone.
        let f = frame(None, &chips);
        assert_eq!((f.well, f.row, f.h), (None, REACH + PAD, PAD + ROW + BOTTOM + 2.0 * REACH));
        assert_eq!(f.plasma_up(), 0.0);
        // Plasma 2560 wide: the texture stays within SteamVR's raw upload limits.
        let f = frame(Some((2560.0, 50.0)), &chips);
        let s = tex_scale(f.w, f.h, 1.0);
        let (tw, th) = ((f.w * s) as usize, (f.h * s) as usize);
        assert!(tw <= 1920 && tw * th <= 1_500_000 && s < 1.0, "{s}");
        assert_eq!(tex_scale(959.0, 186.0, 1.0), 1.0, "the usual frame: a pixel a unit");
        assert_eq!(tex_scale(959.0, 186.0, 1.5), 1.5, "scaled up: sharper");
        assert_eq!(tex_scale(959.0, 186.0, 3.0), 1920.0 / 959.0, "within 1920 a side");
    }

    #[test]
    fn frame_parts() {
        let f = frame(Some(PLASMA), &[(Chip::Panel(0), 150.0), (Chip::Panel(1), 120.0)]);
        let well = f.well.unwrap();
        assert_eq!(part_at(&f, well[0] + 445.5, well[1] + 25.0), Some(Part::Plasma(0.5, 0.5)), "Plasma's: passed on");
        assert_eq!(part_at(&f, well[0], well[1] + well[3]), Some(Part::Plasma(0.0, 0.0)), "its bottom left");
        assert_eq!(part_at(&f, well[0] + well[2], well[1]), Some(Part::Plasma(1.0, 1.0)), "its top right");
        assert_eq!(part_at(&f, well[0] + 100.0, f.row + 1.0), Some(Part::Edge), "just under it: ours");
        assert_eq!(part_at(&f, f.knob.0 + 1.1 * KNOB, f.knob.1), Some(Part::Knob));
        let mid = |k: usize| (f.laid[k].1 + f.laid[k].2) / 2.0;
        assert_eq!(part_at(&f, mid(1), f.row + ROW / 2.0), Some(Part::Chip(Chip::Panel(1))));
        assert_eq!(part_at(&f, f.laid[0].2 + GAP / 2.0, f.row + ROW / 2.0), Some(Part::Edge), "between two chips");
        assert_eq!(part_at(&f, f.w / 2.0, 3.0), Some(Part::Edge), "the reach above");
        assert_eq!(part_at(&f, 3.0, f.h / 2.0), Some(Part::Edge), "the reach left");
        assert_eq!(part_at(&f, REACH + PAD + 2.0, f.row + ROW / 2.0), Some(Part::Edge), "the padding beside the row");
        // the four corners, in grab::CORNER_SIGN's order (right +1, top +1; y runs down here)
        for (k, (sx, sy)) in grab::CORNER_SIGN.iter().enumerate() {
            let x = if *sx > 0.0 { f.w - 5.0 } else { 5.0 };
            let y = if *sy > 0.0 { 5.0 } else { f.h - 5.0 };
            assert_eq!(part_at(&f, x, y), Some(Part::Corner(k)), "corner {k}");
            let x = if *sx > 0.0 { f.w - REACH - CORNER + 1.0 } else { REACH + CORNER - 1.0 };
            let y = if *sy > 0.0 { REACH + 1.0 } else { f.h - REACH - 1.0 };
            assert_eq!(part_at(&f, x, y), Some(Part::Corner(k)), "corner {k}, just inside the border");
        }
        assert_eq!(part_at(&f, REACH + CORNER + 1.0, 3.0), Some(Part::Edge), "past the corner's reach");
    }

    #[test]
    fn only_a_fixed_frame_lights_under_a_laser() {
        // follow's bar moving under a resting laser or cursor never changes its look (a full draw)
        for (m, lit) in [(Mode::Fixed, Look::Lit), (Mode::Follow, Look::Rest), (Mode::Wrist, Look::Rest)] {
            assert_eq!(frame_look(m, false, Some(Part::Edge), false), lit, "{m:?}");
            assert_eq!(frame_look(m, false, Some(Part::Chip(Chip::Prefs)), false), lit, "{m:?}");
            assert_eq!(frame_look(m, false, None, true), lit, "{m:?}: the mouse");
            assert_eq!(frame_look(m, false, None, false), Look::Rest);
            assert_eq!(frame_look(m, false, Some(Part::Plasma(0.5, 0.5)), false), Look::Rest, "Plasma's bar's");
            assert_eq!(frame_look(m, true, None, false), Look::Carry, "resized");
        }
    }

    #[test]
    fn hold_for_modes() {
        for m in [Mode::Fixed, Mode::Follow, Mode::Wrist] {
            assert_eq!(hold_for(Part::Edge, m), (m == Mode::Fixed).then_some(Take::Carry), "{m:?}");
            assert_eq!(hold_for(Part::Corner(2), m), Some(Take::Resize(2)));
            assert_eq!(hold_for(Part::Knob, m), Some(Take::Curve));
            assert_eq!(hold_for(Part::Chip(Chip::Panel(0)), m), None, "a chip is a click");
        }
    }

    #[test]
    fn corner_resize_scales_both_rows() {
        let f = frame(Some(PLASMA), &[(Chip::Panel(0), 150.0)]);
        let (w0, a) = (f.w * MPP, f.h / f.w);
        for (sx, sy) in grab::CORNER_SIGN {
            // taken at the corner, dragged out 10% along the diagonal
            let w = grab::width_for(sx, sy, sx * w0 * 0.55, sy * a * w0 * 0.55, a);
            assert!((rescale(1.0, w0, w) - 1.1).abs() < 1e-9);
            assert!((rescale(2.0, w0, w) - 2.2).abs() < 1e-9, "from the scale it had");
        }
        assert_eq!(rescale(1.0, w0, w0 * 10.0), 3.0);
        assert_eq!(rescale(1.0, w0, 0.0), 0.5);
    }

    #[test]
    fn plasma_sits_in_its_well() {
        // in the frame's own axes, so tilted (follow's 33°) it's up along the frame, not straight up
        let m = panel_matrix(&follow_pose(0.0, 0.0, &[0.0, 1.6, 0.0]));
        let up = 0.03;
        let p = in_well(&m, 0.0, 0.0, up);
        let d: Vec<f64> = (0..3).map(|r| (p[r][3] - m[r][3]) as f64).collect();
        let want: Vec<f64> = (0..3).map(|r| m[r][1] as f64 * up + m[r][2] as f64 * FRONT).collect();
        assert!((0..3).all(|r| (d[r] - want[r]).abs() < 1e-6), "{d:?} vs {want:?}");
        // its front is toward the eye (the frame is 0.7 m ahead, -z): FRONT nearer
        let eye = [0.0, 1.6, 0.0];
        let dist = |q: &Mat| norm(&[q[0][3] as f64 - eye[0], q[1][3] as f64 - eye[1], q[2][3] as f64 - eye[2]]);
        assert!(dist(&in_well(&m, 0.0, 0.0, 0.0)) < dist(&m) - FRONT * 0.9);
        assert_eq!((p[0][0], p[1][1], p[2][2]), (m[0][0], m[1][1], m[2][2]), "facing the same way");
        // Bent (r 0.5 m) with a popup's crop moving it 0.1 m along: all of Plasma's surface
        // stays just in front of the frame's (inside its circle, by under FRONT), not behind it.
        let r = 0.5;
        let p = Placement::from_matrix(&in_well(&m, r, 0.1, up), 0.5, 0.1, r);
        let back = inv_rigid(&m);
        for u in [-0.25, -0.1, 0.0, 0.1, 0.25] {
            let q = mul(&back, &p.on_surface(u, 0.0, 0.0)); // in the frame's own axes
            let off = r - (q[0][3] as f64).hypot(q[2][3] as f64 - r);
            assert!(off > 0.0 && off <= FRONT + 1e-5, "u {u}: {off}");
        }
    }

    #[test]
    fn a_laser_on_the_frame_reaches_plasma_where_the_mouse_would() {
        // Follow at taskbar_scale 0.713, flat and bent to 1.263 m, with the crop just the panel
        // and grown up for a popup. SteamVR's laser hits our frame (mouse events in its units,
        // along the curve), and the panel point we pass on has to be where the mouse's ray
        // through the same spot lands on Plasma's overlay (kvm.rs: `plasmabar::at` of its hit).
        use crate::plasmabar::{Rect, global, panel_at};
        let panel = [818, 1390, 924, 50];
        let f = frame(Some((924.0, 50.0)), &[(Chip::Panel(0), 150.0)]);
        let well = f.well.unwrap();
        let mpp = MPP * 0.713;
        let m = panel_matrix(&follow_pose(10.0, -5.0, &[0.0, 1.6, 0.0]));
        let (mut checked, mut worst_before, mut missed_before) = (0, 0.0f64, 0);
        for crop in [panel, [818, 900, 924, 540]] {
            let (dx, dy) = (0.0, (crop[3] - panel[3]) as f64 / 2.0 * mpp); // plasmabar.rs `shift`
            for curve in [0.0, 1.263] {
                let r = grab::curve_for_width(curve, f.w * mpp);
                let bar = Placement::from_matrix(&m, f.w * mpp, f.h / f.w, r); // kvm.rs `bar`
                let at = in_well(&m, r, dx, f.plasma_up() * mpp + dy);
                let pl = Placement::from_matrix(&at, crop[2] as f64 * mpp, crop[3] as f64 / crop[2] as f64, r); // kvm.rs `plasma`
                for hand in [[0.25, 1.15, -0.25], [-0.3, 1.3, -0.1], [0.0, 1.0, -0.45]] {
                    // the laser aimed at points across the frame's well and a little past it
                    for (a, b) in [(-0.02, 0.5), (0.03, 0.2), (0.3, 0.5), (0.5, 0.5), (0.7, 0.97), (0.97, 0.8), (0.5, -0.05), (0.5, 1.06)] {
                        let p = frame_point(&bar, &f, well[0] + a * well[2], f.h - well[1] - (1.0 - b) * well[3]);
                        let q = [p[0] - hand[0], p[1] - hand[1], p[2] - hand[2]];
                        let d = q.map(|x| x / norm(&q));
                        // the mouse: its ray on Plasma's overlay, if that's on the panel
                        let unclamped = |c: Rect, fx: f64, fy: f64| (c[0] as f64 + fx * (c[2] - 1) as f64, c[1] as f64 + (1.0 - fy) * (c[3] - 1) as f64); // `global`
                        let mouse = pl.hit(&hand, &d).map(|(_, u, v)| unclamped(crop, u / pl.width + 0.5, v / pl.height + 0.5));
                        let mouse = mouse.filter(|&(x, y)| x >= panel[0] as f64 && x <= (panel[0] + panel[2] - 1) as f64 && y >= panel[1] as f64 && y <= (panel[1] + panel[3] - 1) as f64);
                        // the laser: what SteamVR sends the frame, and what we make of it
                        let (_, u, v) = bar.hit(&hand, &d).unwrap();
                        let (x, y) = ((u / bar.width + 0.5) * f.w, (v / bar.height + 0.5) * f.h);
                        let laser = panel_at(&pl, crop, panel, &hand, &frame_point(&bar, &f, x, y)).map(|(fx, fy)| global(panel, fx, fy));
                        let at = format!("crop {crop:?} curve {curve} hand {hand:?} ({a}, {b})");
                        match (laser, mouse) {
                            (Some(l), Some(mo)) => assert!((l.0 - mo.0).hypot(l.1 - mo.1) < 0.01, "{at}: laser {l:?}, mouse {mo:?}"),
                            (l, mo) => assert!(l.is_none() && mo.is_none(), "{at}: laser {l:?}, mouse {mo:?}"),
                        }
                        checked += mouse.is_some() as u32;
                        // before: our well's own fractions, 2 mm behind
                        match (part_at(&f, x, f.h - y), mouse) {
                            (Some(Part::Plasma(fx, fy)), Some(mo)) => {
                                let b = global(panel, fx, fy);
                                worst_before = worst_before.max((b.0 - mo.0).hypot(b.1 - mo.1));
                            }
                            (Some(Part::Plasma(..)), None) | (_, Some(_)) => missed_before += 1,
                            _ => {}
                        }
                    }
                }
            }
        }
        assert!(checked >= 60, "only {checked} landed on the bar");
        // FRONT / mpp is 4.5 px here, times the laser's slant, and a steep laser near an edge
        // went to the wrong overlay part
        assert!(worst_before > 3.0 && missed_before > 0, "before: {worst_before} px, {missed_before} missed");
    }

    #[test]
    fn the_curve_never_wraps_past_half_a_circle() {
        // the radius saved for a narrower frame, as `radius` draws it once the frame has grown
        let w = frame(Some(PLASMA), &[]).w * MPP;
        assert_eq!(grab::curve_for_width(0.4, w), 0.4, "narrow: the user's");
        let r = grab::curve_for_width(0.4, 3.0 * w); // a corner scaled it up 3x
        assert!(r > 0.4 && (curvature(3.0 * w, r) - 0.5).abs() < 1e-6, "{r}");
    }

    fn st(chip: Chip) -> State {
        State { chip, label: 0, dim: false, lit: false, hover: false }
    }

    #[test]
    fn the_frame_draws_its_chips() {
        // a name 80 px wide in its room: R, rows 32..96 (assets.rs's 32 px above and below)
        let iw = 80 + TAG_ROOM as usize;
        let img = Arc::new((iw, 128usize, (0..128).flat_map(|y| if (32..96).contains(&y) { [255u8, 0, 0, 255] } else { [0; 4] }.repeat(iw)).collect()));
        let items = vec![
            Item { s: st(Chip::Panel(0)), accent: grab::REMOTE, label: Some(img), text: String::new() },
            Item { s: State { dim: true, ..st(Chip::Panel(1)) }, accent: grab::VIOLET, label: None, text: String::new() },
            Item { s: State { lit: true, ..st(Chip::Panel(2)) }, accent: grab::VIOLET, label: None, text: String::new() },
        ];
        let f = frame(Some(PLASMA), &widths(&items, None));
        let t = theme::Theme::default().shell;
        let lk = Looks { frame: Look::Rest, knob: Look::Rest, grip: None };
        let sc = Scene::new(&f, &items, None, &t, lk);
        let px = sc.draw(1.0);
        assert_eq!(px.len(), f.w as usize * f.h as usize * 4);
        let at = |x: f64, y: f64| sc.pixel(x, y);
        let mid = |k: usize| (f.laid[k].1 + f.laid[k].2) / 2.0;
        assert_eq!(at(1.0, 1.0).1, 0.0, "the reach is clear");
        assert!(at(f.w / 2.0, REACH + 2.0).1 > 0.99, "the frame is opaque");
        let label = at(mid(0), f.row + ROW / 2.0);
        assert!(label.0.iter().zip(&t.wtext).all(|(a, b)| (a - b).abs() < 1.0), "the label in the text colour: {label:?}");
        let bottom = f.row + (ROW + TILE) / 2.0 - 1.5;
        let acc = |k: usize| t.accent(items[k].accent).line;
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        assert!(near(at(mid(0), bottom).0, acc(0)), "an indicator under it");
        assert!(!near(at(f.laid[0].1 + 3.0, bottom).0, acc(0)), "40% of its width");
        assert!(near(at(f.laid[2].1 + 6.0, bottom).0, acc(2)), "the keyboard's: the whole width");
        assert!(near(at(mid(1), bottom).0, t.surface), "out of sight: none");
        assert!(near(at(mid(1), f.row + 4.0).0, t.surface), "no tile at rest");
        let lit = at(mid(2), f.row + 6.0).0;
        assert!(!near(lit, t.surface), "the keyboard's has a tile: {lit:?}");
    }

    #[test]
    fn a_chip_repaints_as_drawing_it_all_would() {
        let mut items = vec![
            Item { s: st(Chip::Panel(0)), accent: grab::REMOTE, label: None, text: String::new() },
            Item { s: st(Chip::Panel(1)), accent: grab::VIOLET, label: None, text: String::new() },
            Item { s: st(Chip::More(2)), accent: grab::VIOLET, label: None, text: "+2".into() },
        ];
        let f = frame(Some(PLASMA), &widths(&items, None));
        let t = theme::Theme::default().shell;
        let lk = Looks { frame: Look::Lit, knob: Look::Rest, grip: None };
        let s = tex_scale(f.w, f.h, 1.25); // not a whole number of pixels a unit
        let mut px = Scene::new(&f, &items, None, &t, lk).draw(s);
        (items[1].s.hover, items[1].s.lit, items[0].s.dim) = (true, true, true);
        let after = Scene::new(&f, &items, None, &t, lk);
        after.repaint(s, &mut px, 0);
        after.repaint(s, &mut px, 1);
        assert!(px == after.draw(s), "patch differs");
    }

    #[test]
    fn the_gear_comes_last_with_its_teeth() {
        let items = vec![
            Item { s: st(Chip::Panel(0)), accent: grab::REMOTE, label: None, text: String::new() },
            Item { s: st(Chip::Workspace), accent: grab::REMOTE, label: None, text: String::new() },
            Item { s: st(Chip::Prefs), accent: grab::VIOLET, label: None, text: String::new() },
        ];
        let f = frame(Some(PLASMA), &widths(&items, None));
        let &(c, x0, x1) = f.laid.last().unwrap();
        assert!(c == Chip::Prefs && (x1 - x0 - GEAR).abs() < 1e-9, "last, GEAR wide");
        let t = theme::Theme::default().shell;
        let sc = Scene::new(&f, &items, None, &t, Looks { frame: Look::Rest, knob: Look::Rest, grip: None });
        let (cx, cy) = ((x0 + x1) / 2.0, f.row + ROW / 2.0);
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        assert!(near(sc.pixel(cx + 7.0, cy).0, t.wtext), "a tooth: ink");
        let a = std::f64::consts::PI / 8.0;
        assert!(near(sc.pixel(cx + 7.0 * a.cos(), cy + 7.0 * a.sin()).0, t.surface), "a gap: none");
        assert!(near(sc.pixel(cx, cy).0, t.surface), "the hole");
    }

    #[test]
    fn power_closes_on_a_second_click_within_arm() {
        let t = Instant::now();
        assert_eq!(press_power(None, t), (Some(t), false), "the first arms");
        let soon = t + ARM - Duration::from_millis(1);
        assert_eq!(press_power(Some(t), soon), (None, true), "a second within ARM closes");
        let late = t + ARM;
        assert_eq!(press_power(Some(t), late), (Some(late), false), "a later one arms again");
    }

    #[test]
    fn the_power_chip_comes_after_the_gear_and_keeps_its_width_armed() {
        let img = Arc::new((200, 44, vec![255u8; 200 * 44 * 4]));
        let mut items = vec![
            Item { s: st(Chip::Workspace), accent: grab::REMOTE, label: None, text: String::new() },
            Item { s: st(Chip::Prefs), accent: grab::VIOLET, label: None, text: String::new() },
            Item { s: st(Chip::Power), accent: grab::VIOLET, label: None, text: String::new() },
        ];
        let f = frame(Some(PLASMA), &widths(&items, None));
        let n = f.laid.len();
        let (&(g, _, g1), &(c, x0, x1)) = (&f.laid[n - 2], &f.laid[n - 1]);
        assert!(g == Chip::Prefs && c == Chip::Power && x0 > g1 && (x1 - x0 - GEAR).abs() < 1e-9, "last, after the gear, GEAR wide");
        items[2].label = Some(img.clone()); // "Close?" loaded: as wide as it, armed or not
        let f = frame(Some(PLASMA), &widths(&items, None));
        let &(_, x0, x1) = f.laid.last().unwrap();
        assert!((x1 - x0 - ((200.0 - TAG_ROOM) * LABEL + 2.0 * CHIP_PAD)).abs() < 1e-9, "as wide as \"Close?\"");
        let t = theme::Theme::default().shell;
        let sc = Scene::new(&f, &items, None, &t, Looks { frame: Look::Rest, knob: Look::Rest, grip: None });
        let (cx, cy) = ((x0 + x1) / 2.0, f.row + ROW / 2.0);
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        assert!(near(sc.pixel(cx, cy - 4.0).0, t.wtext), "the bar: ink");
        assert!(near(sc.pixel(cx, cy + 6.5).0, t.wtext), "the ring's bottom: ink");
        assert!(near(sc.pixel(cx + 3.0, cy + 2.0).0, t.surface), "inside the ring: none");
        items[2] = Item { s: State { lit: true, ..st(Chip::Power) }, accent: theme::MAGENTA, label: Some(img), text: String::new() };
        let armed = frame(Some(PLASMA), &widths(&items, None));
        assert!(armed == f, "arming moves nothing: the same frame, so only the chip repaints");
        let sc = Scene::new(&armed, &items, None, &t, Looks { frame: Look::Rest, knob: Look::Rest, grip: None });
        assert!(!near(sc.pixel(cx + 3.0, cy + 2.0).0, t.surface), "armed: the label over the glyph's hole");
    }

    /// Dev tool: CC_DUMP=<dir with assets.rs's chip tags and glyphs> [CC_SCHEME=<.colors>]
    /// [CC_PLASMA_THEME=<desktoptheme colors>] cargo test -- --ignored taskbar_dump writes the
    /// frame as raw RGBA, with two remote machines and two windows (rest, lit, hovered, dimmed),
    /// "+2", and a stand-in for Plasma's bar in its well. It writes three: at rest, lit with the
    /// knob and a corner held, and that again with the hit map tinted over it.
    #[test]
    #[ignore]
    fn taskbar_dump() {
        let dir = std::env::var("CC_DUMP").unwrap();
        let load = |n: &str| crate::load_tag(&format!("{dir}/{n}.rgba")).map(Arc::new);
        let glyphs = load("glyphs").map(|g| (*g).clone());
        let items = vec![
            Item { s: st(Chip::Panel(0)), accent: grab::REMOTE, label: load("tag-chip-desk-wide"), text: String::new() },
            Item { s: State { lit: true, ..st(Chip::Panel(1)) }, accent: grab::REMOTE, label: load("tag-chip-work-laptop"), text: String::new() },
            Item { s: State { hover: true, ..st(Chip::Panel(2)) }, accent: grab::VIOLET, label: load("tag-app-org.kde.konsole"), text: String::new() },
            Item { s: State { dim: true, ..st(Chip::Panel(3)) }, accent: grab::VIOLET, label: load("tag-app-Vivaldi-flatpak"), text: String::new() },
            Item { s: st(Chip::More(2)), accent: grab::VIOLET, label: None, text: "+2".into() },
        ];
        let mut th = std::env::var("CC_SCHEME").map_or_else(|_| theme::Theme::default(), |p| theme::from_scheme(&p));
        if let (Ok(s), Ok(p)) = (std::env::var("CC_SCHEME"), std::env::var("CC_PLASMA_THEME")) {
            th.shell = theme::with_panel(&s, &p);
        }
        let t = th.shell;
        let f = frame(Some(PLASMA), &widths(&items, glyphs.as_ref()));
        let well = f.well.unwrap();
        // the stand-in for the stream: icons, a task, the tray and the clock
        let stand_in = |x: f64, y: f64| {
            let (u, v) = (x - well[0], y - well[1]);
            let blocks = [(8.0, 32.0), (48.0, 32.0), (88.0, 32.0), (128.0, 160.0), (well[2] - 200.0, 120.0), (well[2] - 70.0, 60.0)];
            let on = blocks.iter().any(|&(x0, w)| u > x0 && u < x0 + w && v > 9.0 && v < 41.0);
            (if on { grab::mix(t.surface, t.text, 0.3) } else { t.surface }, 1.0)
        };
        let looks = [
            ("rest", Looks { frame: Look::Rest, knob: Look::Rest, grip: None }, false),
            ("held", Looks { frame: Look::Carry, knob: Look::Carry, grip: Some(3) }, false),
            ("hits", Looks { frame: Look::Lit, knob: Look::Lit, grip: Some(0) }, true),
        ];
        for (name, lk, hits) in looks {
            let sc = Scene::new(&f, &items, glyphs.as_ref(), &t, lk);
            let px = grab::draw(f.w as usize, f.h as usize, |x, y| {
                let mut c = sc.pixel(x, y);
                if x >= well[0] && x <= well[0] + well[2] && y >= well[1] && y <= well[1] + well[3] {
                    c = stand_in(x, y);
                }
                if hits {
                    let tint = match part_at(&f, x, y) {
                        None | Some(Part::Plasma(..)) => [128.0, 128.0, 128.0],
                        Some(Part::Edge) => [0.0, 0.0, 255.0],
                        Some(Part::Corner(_)) => [255.0, 0.0, 0.0],
                        Some(Part::Knob) => [255.0, 255.0, 0.0],
                        Some(Part::Chip(_)) => [0.0, 255.0, 0.0],
                    };
                    c = over((c.0, c.1.max(0.6)), (tint, 0.35));
                }
                c
            });
            std::fs::write(format!("{dir}/taskbar_{name}_{}x{}.rgba", f.w as usize, f.h as usize), px).unwrap();
        }
    }

    #[test]
    fn follow_keeps_to_the_bottom_of_the_view_a_moment_behind() {
        let dt = 0.011;
        let mut f = Follow::default();
        let head = [0.0, 1.6, 0.0];
        assert_eq!(f.step(10.0, 0.0, head, dt, false), (10.0, 0.0, head));
        // Jitter (1°, 1 cm): it stays still.
        for _ in 0..50 {
            assert_eq!(f.step(11.0, -1.0, [0.01, 1.6, 0.0], dt, false), (10.0, 0.0, head));
        }
        // Looking 30° down: it comes at once, easing (about 0.4 s), never past.
        for n in 1..=100 {
            let (_, pitch, _) = f.step(10.0, -30.0, head, dt, false);
            assert!(pitch >= -30.0 - 0.1, "overshot: {pitch}");
            assert!(n > 1 || pitch < -0.5, "at once");
            assert!(n != 40 || pitch < -28.5, "0.44 s on: {pitch}"); // 95%
        }
        assert_eq!(f.pitch, -30.0, "there, and still");
        // To -190° (170°): the short way round, up from 10°, not down past -180°.
        let (yaw, _, _) = f.step(-190.0, -30.0, head, dt, false);
        assert!(yaw > 10.0, "{yaw}");
        // Walked: it comes along.
        for _ in 0..100 {
            f.step(170.0, -30.0, [0.6, 1.6, 0.0], dt, false);
        }
        assert!((f.at[0] - 0.6).abs() < 1e-3 && (wrap(f.yaw - 170.0)).abs() < 0.1, "{:?} {}", f.at, f.yaw);
        // Under the gaze by atan(BELOW/OUT), OUT ahead and BELOW under level, facing the eye.
        let p = follow_pose(0.0, 0.0, &head);
        assert!(norm(&[p.centre[0], p.centre[1] - (1.6 - BELOW), p.centre[2] + OUT]) < 1e-9, "{:?}", p.centre);
        let p = follow_pose(0.0, -30.0, &head);
        let to = [p.centre[0] - head[0], p.centre[1] - head[1], p.centre[2] - head[2]];
        let (_, pitch) = angles(&to);
        assert!((pitch - (-30.0 - BELOW.atan2(OUT).to_degrees())).abs() < 1e-9 && (p.pitch - pitch).abs() < 1e-9, "the bottom of the view, facing the eye");
        assert!((wrap(350.0) + 10.0).abs() < 1e-9 && (wrap(-190.0) - 170.0).abs() < 1e-9);
        let down = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 1.6], [0.0, -1.0, 0.0, 0.0]]; // looking straight down
        assert_eq!(gaze(&down), (0.0, -PITCH));
        // Past straight down (leaning back) or up: the yaw holds, no flip to 180°.
        let look = |t: f64| {
            let (s, c) = (t.to_radians().sin(), t.to_radians().cos());
            [[1.0, 0.0, 0.0, 0.0], [0.0, c as f32, -s as f32, 1.6], [0.0, s as f32, c as f32, 0.0]]
        };
        for t in [-89.0, -91.0, 80.0, 91.0] {
            assert!(wrap(gaze(&look(t)).0).abs() < 1e-3, "{t}: {:?}", gaze(&look(t)));
        }
        assert!((gaze(&look(80.0)).1 - 80.0).abs() < 1e-3, "up isn't clamped at 55");
        // A slow turn (6°/s): it follows smoothly, never more than a few degrees behind, no steps.
        let mut f = Follow::default();
        let (mut last, mut worst) = (f.step(0.0, 0.0, head, dt, false).0, 0.0f64);
        for n in 1..=500 {
            let head_yaw = n as f64 * dt * 6.0;
            let (yaw, ..) = f.step(head_yaw, 0.0, head, dt, false);
            if n > 100 {
                worst = worst.max((yaw - last).abs());
                assert!(head_yaw - yaw < SLACK, "{n}: left {} behind", head_yaw - yaw);
            }
            last = yaw;
        }
        assert!(worst < 0.2, "a step of {worst}° in a frame");
        // A laser on it: it stays put.
        let was = f.target;
        for _ in 0..50 {
            f.step(60.0, -20.0, head, dt, true);
        }
        assert_eq!(f.target, was);
    }

    #[test]
    fn the_wrist_bar_is_a_watch_on_the_off_hand() {
        assert_eq!(off_hand(Hand::Right), Hand::Left);
        assert_eq!(off_hand(Hand::Left), Hand::Right);
        let (l, r) = (wrist_local(Hand::Left), wrist_local(Hand::Right));
        for m in [l, r] {
            let c = |j: usize| [0, 1, 2].map(|i| m[i][j] as f64);
            let (x, y, z) = (c(0), c(1), c(2));
            assert!((dot(&x, &x) - 1.0).abs() < 1e-6 && dot(&x, &y).abs() < 1e-6 && dot(&y, &z).abs() < 1e-6, "orthonormal");
            assert!(norm(&[0, 1, 2].map(|i| crate::geometry::cross(&x, &y)[i] - z[i])) < 1e-6, "right-handed");
        }
        // mirror images: the controller's x flipped, and the bar's (so it stays right-handed)
        let (mi, nj) = ([-1.0f32, 1.0, 1.0], [-1.0f32, 1.0, 1.0, 1.0]);
        assert!((0..3).all(|i| (0..4).all(|j| (r[i][j] - mi[i] * nj[j] * l[i][j]).abs() < 1e-6)), "{l:?} {r:?}");
        assert!(l[0][3] < 0.0 && r[0][3] > 0.0 && l[2][3] > 0.0, "on the back of each hand, behind the grip");
        assert!(l[0][2] < -0.99 && r[0][2] > 0.99, "facing out of the back of each hand");
        // The left controller pointing ahead (-z), the eye above and behind it: side on, hidden.
        let eye = [0.0, 1.6, 0.0];
        let ahead: Mat = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.1], [0.0, 0.0, 1.0, -0.35]];
        assert!(!facing(&mul(&ahead, &l), &eye));
        // Turned as a watch: the forearm across the chest (pointing right, +x), the back of the
        // hand up (-x up): it faces the eye, its text left to right.
        let watch: Mat = [[0.0, 0.0, -1.0, 0.0], [-1.0, 0.0, 0.0, 1.2], [0.0, 1.0, 0.0, -0.3]];
        let w = mul(&watch, &l);
        assert!(facing(&w, &eye), "{w:?}");
        assert!(w[0][0] > 0.99, "text runs to the right");
        assert!(w[2][1] < -0.99, "its top away from you");
    }

    #[test]
    fn fixed_goes_under_the_remote_panels() {
        let eye = [0.0, 1.6, 0.0];
        let flat = |x: f64, y: f64, z: f64, w: f64| {
            let pose = Pose { centre: [x, y, z], width: w, ..Default::default() };
            Placement::from_matrix(&panel_matrix(&pose), w, 0.5625, 0.0)
        };
        // Two side by side, the right one lower: centred under both, under the lower one.
        let p = under(&[flat(-0.6, 1.5, -2.0, 1.0), flat(0.6, 1.4, -2.0, 1.0)], &eye).unwrap();
        let low = 1.4 - 0.5625 / 2.0;
        assert!(p.centre[0].abs() < 1e-6 && (p.centre[1] - (low - UNDER)).abs() < 1e-6 && (p.centre[2] + 2.0).abs() < 1e-6, "{:?}", p.centre); // (f32 matrices)
        assert!(p.yaw.abs() < 1e-4 && p.pitch < 0.0, "it faces the eye: {} {}", p.yaw, p.pitch);
        // One off to the left: under its middle.
        let p = under(&[flat(-1.0, 1.5, -2.0, 0.8)], &eye).unwrap();
        assert!((p.centre[0] + 1.0).abs() < 1e-6 && p.yaw > 0.0);
        assert!(under(&[], &eye).is_none());
    }
}
