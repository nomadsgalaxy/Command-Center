//! Panels' backs. I wanted "the freedom to place them backwards, but still have an idea
//! that a panel is there by seeing the passthrough, but dim, like you're looking at the other
//! side of a projection".
//!
//! SteamVR overlays are one-sided, so when a panel turns away from you we show a second overlay
//! in its place, turned half round, with the picture mirrored (like seeing it through the
//! screen), faint and dark.
//!
//! A curved panel's back has to follow its curve, and SteamVR can't do that itself: an overlay
//! only bends toward its own front, and a back faces the other way. So the back is flat strips
//! on the arc's chords, each showing its slice of the picture, with just enough strips to keep
//! every one within SAG of the arc (MAX_STRIPS at most).
//!
//! Overlays are scarce (128 shared by every app, and a panel already uses two), so the strips
//! come from a shared budget (STRIPS). Only panels facing away get a back, nearest first, each
//! made when needed and destroyed after. One that doesn't fit gets a single flat back on the
//! whole arc's chord instead, and once the budget's gone, nothing.
//!
//! A back shows the panel's own texture (the shared handle its last frame went up with, see
//! `shown`), so there's no second upload and no copy. A picture uploaded from memory (the
//! Machines and Preferences windows, a window without dmabuf) has no handle, so its back is a
//! plain sheet.
//!
//! A laser press on a back carries its panel (grab.rs poll), so you can grab a panel that was
//! left backwards and turn it round. The card is one-sided too, so it's no use from behind.
use crate::geometry::{Mat, Placement, V3, dot, norm};
use crate::{call, vr};
use openvr_sys as sys;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const STRIPS: usize = 12; // calibration knob: at most this many back overlays at once (overlays are shared by every app)
const MAX_STRIPS: usize = 6; // calibration knob: the most strips one curved back is made of
const SAG: f64 = 0.002; // calibration knob: m a strip may sit off the arc (keep it under BEHIND so it stays behind the picture)
const ALPHA: f32 = 0.3; // calibration knob: how much of the back shows (passthrough shows through the rest)
const DIM: f32 = 0.45; // calibration knob: colour is scaled by this so it's darker than the front
// calibration knob: the Frame shows an overlay under 1 alpha with IgnoreTextureAlpha as a tinted
// sheet, so the back keeps the texture's alpha. Set it true if a picture's alpha is 0, since that
// back would vanish
const IGNORE_ALPHA: bool = false;
const HYST: f64 = 0.05; // calibration knob: hysteresis (cosine, ~3°): facing away past this gets a back, coming back past it loses it
const BEHIND: f64 = 0.004; // m behind the picture, so it's nearer you than the card (2 mm) and wins laser hits from behind
const FILL: [u8; 4] = [128, 128, 128, 255]; // a back without the panel's texture
const SWAP: f64 = 0.2; // calibration knob: m nearer a panel has to be to take another's back, so it doesn't churn at the cutoff
const RETRY: Duration = Duration::from_secs(2); // calibration knob: after a back couldn't be made, wait this long before trying another

/// Each picture overlay's last shared texture (0 if uploaded from memory) and its back's strips
/// (empty if it has no back).
static TEX: Mutex<Vec<(vr::Handle, u64, Vec<vr::Handle>)>> = Mutex::new(Vec::new());

/// A picture went up as shared texture `tex` (0 if from memory), so its back, if it has one,
/// shows it too.
pub fn shown(front: vr::Handle, tex: u64) {
    let mut t = TEX.lock().unwrap();
    let e = entry(&mut t, front);
    e.1 = tex;
    if tex != 0 {
        for &h in &e.2 {
            texture(h, tex);
        }
    }
}

fn entry(t: &mut Vec<(vr::Handle, u64, Vec<vr::Handle>)>, front: vr::Handle) -> &mut (vr::Handle, u64, Vec<vr::Handle>) {
    let n = t.iter().position(|e| e.0 == front).unwrap_or_else(|| {
        t.push((front, 0, Vec::new()));
        t.len() - 1
    });
    &mut t[n]
}

fn texture(h: vr::Handle, mut tex: u64) -> bool {
    let mut t = sys::Texture_t {
        handle: &mut tex as *mut _ as *mut std::ffi::c_void,
        eType: sys::ETextureType_TextureType_SharedTextureHandle,
        eColorSpace: sys::EColorSpace_ColorSpace_Gamma,
    };
    call!(ov, SetOverlayTexture, h, &mut t) == 0
}

/// Does a panel (centre c, front z) face away from the eye? `was` is last time's answer, for
/// hysteresis.
pub fn away(c: &V3, z: &V3, eye: &V3, was: bool) -> bool {
    let e = [eye[0] - c[0], eye[1] - c[1], eye[2] - c[2]];
    let cos = dot(z, &e) / norm(&e).max(1e-9);
    cos < if was { HYST } else { -HYST }
}

/// A back's matrix: the picture's, turned half round about its own vertical axis and moved
/// `behind` metres back, toward whoever sees the back (negative moves it forward).
pub fn back_matrix(m: &Mat, behind: f64) -> Mat {
    let mut b = *m;
    for r in 0..3 {
        (b[r][0], b[r][2], b[r][3]) = (-m[r][0], -m[r][2], m[r][3] - behind as f32 * m[r][2]);
    }
    b
}

/// Which panels get a back, and how many strips each. Takes those facing away (slot, distance,
/// strips wanted), nearest first, while `budget` lasts. One that doesn't fit gets one flat strip.
pub fn share(mut want: Vec<(usize, f64, usize)>, mut budget: usize) -> Vec<(usize, usize)> {
    want.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut out = Vec::new();
    for (i, _, n) in want {
        let n = if n <= budget { n } else { budget.min(1) };
        if n > 0 {
            budget -= n;
            out.push((i, n));
        }
    }
    out
}

/// A flat sheet for an arc `w` long round radius `curve` (0 = flat), as (width, metres forward).
/// It's the arc's chord, which sits at most s off the arc (in its middle).
pub fn flat(w: f64, curve: f64) -> (f64, f64) {
    if curve <= 0.0 {
        return (w, 0.0);
    }
    let a = (w / (2.0 * curve)).min(std::f64::consts::PI); // half the arc's angle
    (2.0 * curve * a.sin(), curve * (1.0 - a.cos()))
}

/// How many strips a back `w` wide round radius `curve` needs: the fewest that stay within SAG
/// of the arc.
pub fn strips(w: f64, curve: f64) -> usize {
    (1..MAX_STRIPS).find(|&n| flat(w / n as f64, curve).1 <= SAG).unwrap_or(MAX_STRIPS)
}

/// Strip k of n of a panel's back: its matrix and width. Strip 0 is the picture's left end,
/// which is on the right seen from behind.
pub fn strip(pl: &Placement, n: usize, k: usize) -> (Mat, f64) {
    // a panel curved top to bottom is its picture's overlay a quarter turn round (kvm.rs)
    let o = if pl.vert { pl.turned() } else { *pl };
    let part = o.width / n as f64;
    let (w, fwd) = flat(part, o.curve);
    (back_matrix(&o.on_surface(-o.width / 2.0 + (k as f64 + 0.5) * part, 0.0, fwd), BEHIND), w)
}

/// Strip k of n's slice of the picture as uMin, uMax, mirrored since it's seen through the screen.
fn bounds(n: usize, k: usize) -> (f32, f32) {
    ((k + 1) as f32 / n as f32, k as f32 / n as f32)
}

/// A panel as the backs see it: its slot, picture overlay and place, whether it can have a back at
/// all (not hidden, away, in a game or empty), and whether it's being carried. A carried panel's
/// back comes first, since that may be what it was picked up by.
pub struct Front {
    pub i: usize,
    pub overlay: vr::Handle,
    pub pl: Placement,
    pub ok: bool,
    pub held: bool,
}

struct Back {
    i: usize,
    hs: Vec<vr::Handle>, // its strips, left to right on the picture (just one if flat)
    front: vr::Handle,
    tex: bool,                              // showing the panel's texture, otherwise FILL
    tried: u64,                             // the texture last tried, so one that failed isn't tried again
    placed: Option<(Mat, i64, i64, i64, bool)>, // the picture's matrix, width, height, curve (mm) and tex it was placed for
}

#[derive(Default)]
pub struct Backs {
    away: Vec<bool>, // whether each slot faced away, as last seen
    pool: Vec<Back>,
    full_said: Option<Instant>,
    failed: Option<Instant>, // when a back last couldn't be made (see RETRY)
}

impl Backs {
    /// Every tick: decides which panels have a back and puts each where its picture is.
    pub fn tick(&mut self, fronts: &[Front], eye: &V3) {
        let mut want = Vec::new();
        let mut wanted = 0;
        for f in fronts {
            if self.away.len() <= f.i {
                self.away.resize(f.i + 1, false);
            }
            // a carried panel keeps its back, since its laser's release may only land there
            let a = f.ok && ((f.held && self.away[f.i]) || away(&f.pl.c, &f.pl.z, eye, self.away[f.i]));
            self.away[f.i] = a;
            if a {
                let d = norm(&[f.pl.c[0] - eye[0], f.pl.c[1] - eye[1], f.pl.c[2] - eye[2]]);
                let has = self.pool.iter().any(|b| b.i == f.i);
                let n = if f.pl.vert { strips(f.pl.height, f.pl.curve) } else { strips(f.pl.width, f.pl.curve) };
                wanted += n;
                want.push((f.i, if f.held { 0.0 } else if has { d - SWAP } else { d }, n));
            }
        }
        if wanted > STRIPS && self.full_said.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            self.full_said = Some(Instant::now());
            eprintln!("backs: {} panels face away wanting {wanted} strips, {STRIPS} to go round: the farthest show a flat back or none", want.len());
        }
        let keep = share(want, STRIPS);
        // remake a back whose strip count changed (budget, curve or width)
        for b in self.pool.extract_if(.., |b| !keep.contains(&(b.i, b.hs.len()))) {
            for &h in &b.hs {
                call!(ov, DestroyOverlay, h);
            }
            let mut t = TEX.lock().unwrap();
            t.iter_mut().filter(|e| e.2 == b.hs).for_each(|e| e.2.clear());
            t.retain(|e| e.1 != 0 || !e.2.is_empty()); // don't keep entries for pictures from memory, or the Extras' would pile up
        }
        for &(i, n) in &keep {
            let Some(f) = fronts.iter().find(|f| f.i == i) else { continue };
            if self.pool.iter().any(|b| b.i == i) {
                continue;
            }
            if self.failed.is_some_and(|t| t.elapsed() < RETRY) {
                break;
            }
            let Some(hs) = make(i, n) else {
                self.failed = Some(Instant::now());
                break;
            };
            eprintln!("backs: panel {i}, {n} strip(s), {:.3} x {:.3} m round {:.2} m{}", f.pl.width, f.pl.height, f.pl.curve, if f.pl.vert { " (top to bottom)" } else { "" });
            let mut sort = 0;
            call!(ov, GetOverlaySortOrder, f.overlay, &mut sort);
            for &h in &hs {
                call!(ov, SetOverlaySortOrder, h, sort);
            }
            self.pool.push(Back { i, hs, front: f.overlay, tex: false, tried: 0, placed: None });
        }
        for b in &mut self.pool {
            let Some(f) = fronts.iter().find(|f| f.i == b.i) else { continue };
            // the panel's texture once it has one, or a new one (shown keeps it current after that)
            let tex = {
                let mut t = TEX.lock().unwrap();
                let e = entry(&mut t, b.front);
                let first = e.2 != b.hs;
                if first {
                    e.2 = b.hs.clone();
                }
                if first || ((e.1 != 0) != b.tex && e.1 != b.tried) {
                    b.tried = e.1;
                    b.tex = e.1 != 0 && b.hs.iter().all(|&h| texture(h, e.1));
                    if !b.tex {
                        for &h in &b.hs {
                            let mut px = FILL.repeat(16);
                            call!(ov, SetOverlayRaw, h, px.as_mut_ptr() as *mut _, 4, 4, 4);
                        }
                    }
                }
                b.tex
            };
            // a panel curved top to bottom is its picture's overlay a quarter turn round (kvm.rs)
            let o = if f.pl.vert { f.pl.turned() } else { f.pl };
            let mm = |x: f64| (x * 1000.0).round() as i64;
            let placed = Some((o.matrix(), mm(o.width), mm(o.height), mm(o.curve), tex));
            if b.placed != placed {
                b.placed = placed;
                let n = b.hs.len();
                let mut front = 1.0;
                if tex {
                    call!(ov, GetOverlayTexelAspect, b.front, &mut front);
                }
                for (k, &h) in b.hs.iter().enumerate() {
                    let (m, w) = strip(&f.pl, n, k);
                    // the same height as the picture: a strip shows 1/n of its texture's width
                    // (SteamVR sizes by the bounds, like plasmabar.rs's crop), squeezed to its chord
                    let aspect = if tex { front * (n as f64 * w / o.width) as f32 } else { (n as f64 * w / o.height) as f32 }; // FILL's 4x4
                    call!(ov, SetOverlayWidthInMeters, h, w as f32);
                    call!(ov, SetOverlayTexelAspect, h, aspect);
                    vr::place(h, &m);
                }
            }
        }
    }

    /// The paint order changed (grab.rs paint_order), so each back re-sorts with its picture.
    pub fn resort(&self) {
        for b in &self.pool {
            let mut sort = 0;
            call!(ov, GetOverlaySortOrder, b.front, &mut sort);
            for &h in &b.hs {
                call!(ov, SetOverlaySortOrder, h, sort);
            }
        }
    }

    /// Laser presses and releases on the backs (any strip), as (slot, device, pressed).
    pub fn events(&self) -> Vec<(usize, u32, bool)> {
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        let mut out = Vec::new();
        for (b, &h) in self.pool.iter().flat_map(|b| b.hs.iter().map(move |h| (b, h))) {
            while call!(ov, PollNextOverlayEvent, h, &mut e, size_of::<vr::VREvent_t>() as u32) {
                match e.eventType {
                    sys::EVREventType_VREvent_MouseButtonDown if unsafe { e.data.mouse.button } == sys::EVRMouseButton_VRMouseButton_Left => {
                        out.push((b.i, e.trackedDeviceIndex, true))
                    }
                    sys::EVREventType_VREvent_MouseButtonUp => out.push((b.i, e.trackedDeviceIndex, false)),
                    _ => {}
                }
            }
        }
        out
    }
}

/// Makes a back of n strips: dim and faint, each showing its slice of the texture mirrored left to
/// right (seen through the screen). A laser press on it starts a carry. If one strip can't be
/// made, none are.
fn make(i: usize, n: usize) -> Option<Vec<vr::Handle>> {
    let mut hs = Vec::new();
    for k in 0..n {
        match strip_overlay(i, n, k) {
            Some(h) => hs.push(h),
            None => {
                for h in hs {
                    call!(ov, DestroyOverlay, h);
                }
                return None;
            }
        }
    }
    Some(hs)
}

fn strip_overlay(i: usize, n: usize, k: usize) -> Option<vr::Handle> {
    let h = vr::create_overlay(&format!("controlcenter.back.{i}.{k}"), &format!("Command Center back {i} ({}/{n})", k + 1))
        .inspect_err(|e| eprintln!("backs: no overlay for panel {i}: {e}"))
        .ok()?;
    call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
    call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
    call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_IgnoreTextureAlpha, IGNORE_ALPHA);
    let (u_min, u_max) = bounds(n, k);
    let mut bounds = sys::VRTextureBounds_t { uMin: u_min, vMin: 0.0, uMax: u_max, vMax: 1.0 };
    call!(ov, SetOverlayTextureBounds, h, &mut bounds);
    call!(ov, SetOverlayAlpha, h, ALPHA);
    call!(ov, SetOverlayColor, h, DIM, DIM, DIM);
    call!(ov, ShowOverlay, h);
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Pose, panel_matrix};

    #[test]
    fn facing_away_with_hysteresis() {
        let (c, eye) = ([0.0, 1.6, -1.0], [0.0, 1.6, 0.0]);
        assert!(!away(&c, &[0.0, 0.0, 1.0], &eye, false), "facing you");
        assert!(away(&c, &[0.0, 0.0, -1.0], &eye, false), "its back to you");
        let edge = [1.0, 0.0, 0.02f64]; // nearly edge-on, turned a little toward you
        let n = norm(&edge);
        let edge = edge.map(|v| v / n);
        assert!(!away(&c, &edge, &eye, false) && away(&c, &edge, &eye, true), "edge on: as it was");
    }

    #[test]
    fn a_back_is_its_picture_turned_round() {
        let m = panel_matrix(&Pose { centre: [0.3, 1.5, -1.2], yaw: 25.0, pitch: -10.0, roll: 5.0, ..Default::default() });
        let b = back_matrix(&m, 0.0);
        for r in 0..3 {
            assert_eq!(b[r][3], m[r][3], "the same centre");
            assert_eq!(b[r][2], -m[r][2], "facing the other way");
            assert_eq!(b[r][1], m[r][1], "the same up");
        }
        let b = back_matrix(&m, 0.004);
        let off: f32 = (0..3).map(|r| (b[r][3] - m[r][3]) * m[r][2]).sum();
        assert!((off + 0.004).abs() < 1e-6, "4 mm behind: {off}");
    }

    #[test]
    fn a_curved_back_lies_flat_on_the_chord() {
        assert_eq!(flat(1.2, 0.0), (1.2, 0.0));
        let (w, s) = flat(1.2, 1.0); // 1.2 m round a 1 m radius: 0.6 rad each side
        assert!((w - 2.0 * 0.6f64.sin()).abs() < 1e-9 && (s - (1.0 - 0.6f64.cos())).abs() < 1e-9, "{w} {s}");
        assert!(w < 1.2 && s > 0.17 && s < 0.18);
    }

    #[test]
    fn the_nearest_get_the_strips_the_rest_a_flat_back() {
        let want = vec![(0, 3.0, 1), (1, 1.0, 6), (2, 0.0, 1), (3, 2.0, 6), (4, 5.0, 1), (5, 6.0, 1)];
        // 2 (1), 1 (6), 3 wants 6 but 5 are left: flat (1), 0 (1), 4 (1), 5: none left
        assert_eq!(share(want, 10), vec![(2, 1), (1, 6), (3, 1), (0, 1), (4, 1)]);
        assert_eq!(share(vec![(7, 1.0, 4)], 12), vec![(7, 4)]);
        assert_eq!(share(vec![(7, 1.0, 4)], 0), vec![]);
    }

    #[test]
    fn as_few_strips_as_keep_within_sag() {
        assert_eq!(strips(1.2, 0.0), 1, "flat");
        assert_eq!(strips(0.05, 1.0), 1, "a sliver: its chord is close enough");
        assert_eq!(strips(1.2, 1.0), MAX_STRIPS, "1.2 m round 1 m wants ~10: capped");
        for (w, r) in [(0.2, 2.0), (0.6, 1.9), (0.8, 3.0), (1.2, 4.0), (0.4, 1.09)] {
            let n = strips(w, r);
            assert!(n > 1 && n < MAX_STRIPS, "{w} {r}: {n}");
            assert!(flat(w / n as f64, r).1 <= SAG && flat(w / (n - 1) as f64, r).1 > SAG, "{w} {r}: {n} is the fewest");
        }
    }

    fn col(m: &Mat, j: usize) -> V3 {
        [0, 1, 2].map(|r| m[r][j] as f64)
    }

    fn close(a: &V3, b: &V3) -> bool {
        (0..3).all(|r| (a[r] - b[r]).abs() < 1e-5)
    }

    #[test]
    fn strips_lie_on_the_arc_facing_out_of_its_back() {
        let pose = Pose { centre: [0.3, 1.5, -1.2], yaw: 25.0, pitch: -10.0, roll: 5.0, ..Default::default() };
        let m = panel_matrix(&pose);
        for vert in [false, true] {
            // 1.2 m wide, 0.6 high, round 1.5 m: along the curve, 1.2 m (left-right) or 0.6 (top-bottom)
            let pl = Placement { vert, ..Placement::from_matrix(&m, 1.2, 0.5, 1.5) };
            let len = if vert { pl.height } else { pl.width };
            // a point on the arc (s along it from the middle): u right, or v up when curved top to bottom
            let arc = |s: f64| col(&if vert { pl.on_surface(0.0, s, 0.0) } else { pl.on_surface(s, 0.0, 0.0) }, 3);
            for n in 1..=MAX_STRIPS {
                for k in 0..n {
                    let (b, w) = strip(&pl, n, k);
                    let (x, y, z, c) = (col(&b, 0), col(&b, 1), col(&b, 2), col(&b, 3));
                    let on = [0, 1, 2].map(|r| c[r] - BEHIND * z[r]); // back onto the chord
                    let s0 = -len / 2.0 + k as f64 * len / n as f64;
                    let s1 = s0 + len / n as f64;
                    // seen from behind, x runs the other way, so its +x end is the picture's start
                    let end = |sign: f64| [0, 1, 2].map(|r| on[r] + sign * w / 2.0 * x[r]);
                    assert!(close(&end(1.0), &arc(s0)) && close(&end(-1.0), &arc(s1)), "vert {vert} {k}/{n}: ends on the arc");
                    // facing the way the picture's back does in its middle
                    let mid = if vert { pl.on_surface(0.0, (s0 + s1) / 2.0, 0.0) } else { pl.on_surface((s0 + s1) / 2.0, 0.0, 0.0) };
                    assert!(close(&z, &col(&mid, 2).map(|v| -v)), "vert {vert} {k}/{n}: facing out");
                    // the overlay's up is the arc's axis, which is left on the picture when it's turned (y' = -x)
                    let up = if vert { col(&mid, 0).map(|v| -v) } else { col(&mid, 1) };
                    assert!(close(&y, &up), "vert {vert} {k}/{n}: upright on the axis");
                }
            }
        }
    }

    #[test]
    fn strips_show_the_picture_mirrored_without_gaps() {
        for n in 1..=MAX_STRIPS {
            assert_eq!((bounds(n, 0).1, bounds(n, n - 1).0), (0.0, 1.0), "{n}: all of it");
            for k in 0..n {
                let (lo, hi) = bounds(n, k);
                assert!(lo > hi, "{n} {k}: mirrored");
                assert!(((lo + hi) / 2.0 - (k as f32 + 0.5) / n as f32).abs() < 1e-6, "{n} {k}: its own slice");
                if k + 1 < n {
                    assert_eq!(bounds(n, k).0, bounds(n, k + 1).1, "{n} {k}: no gap");
                }
            }
        }
    }
}
