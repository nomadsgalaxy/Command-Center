//! Panels' backs. I wanted "the freedom to place them backwards, but still have an idea
//! that a panel is there by seeing the passthrough, but dim, like you're looking at the other
//! side of a projection".
//!
//! SteamVR overlays are one-sided, so when a panel turns away from you we show a second overlay
//! in its place, turned half round, with the picture mirrored (like seeing it through the
//! screen), faint and dark.
//!
//! A back is one flat overlay. On a curved panel it's as wide as the arc's chord and touches its
//! middle, since an overlay only bends toward its own front and a back faces the other way.
//! Following the curve would take several flat strips per back, and overlays are scarce (128 shared by every app, and a
//! panel already uses two), so I decided a flat sheet is enough to show a panel is there.
//!
//! Only panels facing away get a back, nearest first, each made when needed and destroyed
//! after, BACKS at most.
//!
//! A back shows the panel's own texture (the shared handle its last frame went up with, see
//! `shown`), so there's no second upload and no copy. A picture uploaded from memory (the
//! Machines and Preferences windows, a window without dmabuf) has no handle, so its back is a
//! plain sheet.
//!
//! A laser press on a back carries its panel (grab.rs poll), so you can grab a panel that was
//! left backwards and turn it round. The card is one-sided too, so it's no use from behind.
use crate::geometry::{Mat, Placement, V3, chord, dot, norm};
use crate::overlay::{At, Live, Place, apply};
use crate::texture::{self, TexHandle, Textures};
use crate::{call, vr};
use openvr_sys as sys;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BACKS: usize = 8; // calibration knob: at most this many backs at once (overlays are shared by every app)
const ALPHA: f32 = 0.3; // calibration knob: how much of the back shows (passthrough shows through the rest)
const DIM: f32 = 0.45; // calibration knob: colour is scaled by this so it's darker than the front
// calibration knob: the Frame shows an overlay under 1 alpha with IgnoreTextureAlpha as a tinted
// sheet, so the back keeps the texture's alpha. Set it true if a picture's alpha is 0, since that
// back would vanish
const IGNORE_ALPHA: bool = false;
const HYST: f64 = 0.05; // calibration knob: hysteresis (cosine, ~3°): facing away past this gets a back, coming back past it loses it
const BEHIND: f64 = 0.004; // m behind the picture (on a curved panel, behind its middle), so it's nearer you than the card (2 mm) and wins laser hits from behind
const FILL: [u8; 4] = [128, 128, 128, 255]; // a back without the panel's texture
const SWAP: f64 = 0.2; // calibration knob: m nearer a panel has to be to take another's back, so it doesn't churn at the cutoff
const RETRY: Duration = Duration::from_secs(2); // calibration knob: after a back couldn't be made, wait this long before trying another

/// Each picture overlay's last shared texture (0 if uploaded from memory) and its back (0 if none).
static TEX: Mutex<Vec<(vr::Handle, u64, vr::Handle)>> = Mutex::new(Vec::new());

/// A picture went up as shared texture `tex` (0 if from memory), so its back, if it has one,
/// shows it too.
pub fn shown(front: vr::Handle, tex: u64) {
    let mut t = TEX.lock().unwrap();
    let e = entry(&mut t, front);
    e.1 = tex;
    if tex != 0 && e.2 != crate::vr::Handle::NONE {
        texture(e.2, tex);
    }
}

fn entry(t: &mut Vec<(vr::Handle, u64, vr::Handle)>, front: vr::Handle) -> &mut (vr::Handle, u64, vr::Handle) {
    let n = t.iter().position(|e| e.0 == front).unwrap_or_else(|| {
        t.push((front, 0, crate::vr::Handle::NONE));
        t.len() - 1
    });
    &mut t[n]
}

fn texture(h: vr::Handle, tex: u64) -> bool {
    texture::Live.present(h, TexHandle(tex)).is_ok()
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

/// A panel's back: its matrix and width. It's as wide as the arc's chord and touches the arc's
/// middle, BEHIND it, so all of it stays behind the picture.
pub fn sheet(pl: &Placement) -> (Mat, f64) {
    // a panel curved top to bottom is its picture's overlay a quarter turn round (kvm.rs)
    let o = if pl.vert { pl.turned() } else { *pl };
    (back_matrix(&o.on_surface(0.0, 0.0, 0.0), BEHIND), chord(o.width, o.curve).0)
}

/// Where a panel's back goes and how its picture is squeezed: the sheet's matrix and width, and
/// the same height as the picture with its whole width squeezed onto the chord. `front` is the
/// picture's own texel aspect, which the back's texture shares when it shows it (`tex`). Otherwise
/// the back is FILL's 4x4.
fn back_place(pl: &Placement, front: f32, tex: bool) -> Place {
    let o = if pl.vert { pl.turned() } else { *pl };
    let (m, w) = sheet(pl);
    let aspect = if tex { front * (w / o.width) as f32 } else { (w / o.height) as f32 };
    Place::new(At::Room(m), w).texel(aspect)
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
    h: vr::Handle,
    front: vr::Handle,
    tex: bool,                                  // showing the panel's texture, otherwise FILL
    tried: u64,                                 // the texture last tried, so one that failed isn't tried again
    placed: Option<(Mat, i64, i64, i64, bool)>, // the picture's matrix, width, height, curve (mm) and tex it was placed for
}

#[derive(Default)]
pub struct Backs {
    away: Vec<bool>, // whether each slot faced away, as last seen
    pool: Vec<Back>,
    failed: Option<Instant>, // when a back last couldn't be made (see RETRY)
}

impl Backs {
    /// Every tick: decides which panels have a back and puts each where its picture is.
    pub fn tick(&mut self, fronts: &[Front], eye: &V3) {
        let mut want = Vec::new();
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
                want.push((
                    f.i,
                    if f.held {
                        0.0
                    } else if has {
                        d - SWAP
                    } else {
                        d
                    },
                ));
            }
        }
        let keep = nearest(want, BACKS);
        for b in self.pool.extract_if(.., |b| !keep.contains(&b.i)) {
            crate::surface::destroy(b.h);
            let mut t = TEX.lock().unwrap();
            t.iter_mut().filter(|e| e.2 == b.h).for_each(|e| e.2 = crate::vr::Handle::NONE);
            t.retain(|e| e.1 != 0 || e.2 != crate::vr::Handle::NONE); // don't keep entries for pictures from memory, or the Extras' would pile up
        }
        for &i in &keep {
            let Some(f) = fronts.iter().find(|f| f.i == i) else { continue };
            if self.pool.iter().any(|b| b.i == i) {
                continue;
            }
            if self.failed.is_some_and(|t| t.elapsed() < RETRY) {
                break;
            }
            let Some(h) = make(i) else {
                self.failed = Some(Instant::now());
                break;
            };
            let mut sort = 0;
            call!(ov, GetOverlaySortOrder, f.overlay.0, &mut sort);
            call!(ov, SetOverlaySortOrder, h.0, sort);
            self.pool.push(Back { i, h, front: f.overlay, tex: false, tried: 0, placed: None });
        }
        for b in &mut self.pool {
            let Some(f) = fronts.iter().find(|f| f.i == b.i) else { continue };
            // the panel's texture once it has one, or a new one (shown keeps it current after that)
            let tex = {
                let mut t = TEX.lock().unwrap();
                let e = entry(&mut t, b.front);
                let first = e.2 != b.h;
                if first {
                    e.2 = b.h;
                }
                if first || ((e.1 != 0) != b.tex && e.1 != b.tried) {
                    b.tried = e.1;
                    b.tex = e.1 != 0 && texture(b.h, e.1);
                    if !b.tex {
                        let _ = texture::Live.present_pixels(b.h, &FILL.repeat(16), 4, 4);
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
                let mut front = 1.0;
                if tex {
                    call!(ov, GetOverlayTexelAspect, b.front.0, &mut front);
                }
                apply(&mut Live, b.h, &back_place(&f.pl, front, tex));
            }
        }
    }

    /// The paint order changed (grab.rs paint_order), so each back re-sorts with its picture.
    pub fn resort(&self) {
        for b in &self.pool {
            let mut sort = 0;
            call!(ov, GetOverlaySortOrder, b.front.0, &mut sort);
            call!(ov, SetOverlaySortOrder, b.h.0, sort);
        }
    }

    /// Laser presses and releases on the backs, as (slot, device, pressed).
    pub fn events(&self) -> Vec<(usize, crate::ids::Dev, bool)> {
        use crate::events::{Button, PointerEvent, PointerKind as P, SurfaceEvent as S};
        let mut e: vr::VREvent_t = Default::default();
        let mut out = Vec::new();
        for b in &self.pool {
            while call!(ov, PollNextOverlayEvent, b.h.0, &mut e, size_of::<vr::VREvent_t>() as u32) {
                match vr::surface_event(&e) {
                    S::Pointer(PointerEvent { kind: P::Down(Button::Left), from, .. }) => out.push((b.i, from, true)),
                    S::Pointer(PointerEvent { kind: P::Up(_), from, .. }) => out.push((b.i, from, false)),
                    _ => {}
                }
            }
        }
        out
    }
}

/// Which panels get a back: those facing away (slot, distance), nearest first, `budget` at most.
fn nearest(mut want: Vec<(usize, f64)>, budget: usize) -> Vec<usize> {
    want.sort_by(|a, b| a.1.total_cmp(&b.1));
    want.into_iter().take(budget).map(|w| w.0).collect()
}

/// Makes a back: dim and faint, its texture mirrored left to right (seen through the screen). A
/// laser press on it starts a carry.
fn make(i: usize) -> Option<vr::Handle> {
    let h = crate::surface::create(crate::surface::Kind::Back, Some(i), &format!("controlcenter.back.{i}"), &format!("Command Center back {i}"))
        .inspect_err(|e| eprintln!("backs: no overlay for panel {i}: {e}"))
        .ok()?;
    call!(ov, SetOverlayInputMethod, h.0, sys::VROverlayInputMethod_Mouse);
    call!(ov, SetOverlayFlag, h.0, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
    call!(ov, SetOverlayFlag, h.0, sys::VROverlayFlags_IgnoreTextureAlpha, IGNORE_ALPHA);
    let mut bounds = sys::VRTextureBounds_t { uMin: 1.0, vMin: 0.0, uMax: 0.0, vMax: 1.0 };
    call!(ov, SetOverlayTextureBounds, h.0, &mut bounds);
    call!(ov, SetOverlayAlpha, h.0, ALPHA);
    call!(ov, SetOverlayColor, h.0, DIM, DIM, DIM);
    call!(ov, ShowOverlay, h.0);
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
        assert_eq!(chord(1.2, 0.0), (1.2, 0.0));
        let (w, s) = chord(1.2, 1.0); // 1.2 m round a 1 m radius: 0.6 rad each side
        assert!((w - 2.0 * 0.6f64.sin()).abs() < 1e-9 && (s - (1.0 - 0.6f64.cos())).abs() < 1e-9, "{w} {s}");
        assert!(w < 1.2 && s > 0.17 && s < 0.18);
    }

    #[test]
    fn the_nearest_get_a_back() {
        let want = vec![(0, 3.0), (1, 1.0), (2, 0.0), (3, 2.0)];
        assert_eq!(nearest(want.clone(), 3), vec![2, 1, 3]);
        assert_eq!(nearest(want, 0), Vec::<usize>::new());
    }

    fn col(m: &Mat, j: usize) -> V3 {
        [0, 1, 2].map(|r| m[r][j] as f64)
    }

    fn close(a: &V3, b: &V3) -> bool {
        (0..3).all(|r| (a[r] - b[r]).abs() < 1e-5)
    }

    #[test]
    fn a_curved_back_touches_the_middle_facing_out() {
        let pose = Pose { centre: [0.3, 1.5, -1.2], yaw: 25.0, pitch: -10.0, roll: 5.0, ..Default::default() };
        let m = panel_matrix(&pose);
        for vert in [false, true] {
            let pl = Placement { vert, ..Placement::from_matrix(&m, 1.2, 0.5, 1.5) };
            let (b, w) = sheet(&pl);
            let o = if vert { pl.turned() } else { pl };
            assert!((w - chord(o.width, o.curve).0).abs() < 1e-9, "vert {vert}: the chord's width");
            let mid = o.on_surface(0.0, 0.0, 0.0);
            let z = col(&b, 2);
            assert!(close(&z, &col(&mid, 2).map(|v| -v)), "vert {vert}: facing out of the back");
            let off = [0, 1, 2].map(|r| col(&b, 3)[r] - col(&mid, 3)[r]);
            assert!(close(&off, &z.map(|v| v * BEHIND)), "vert {vert}: BEHIND the middle");
        }
    }

    #[test]
    fn a_back_is_a_sheet_of_the_chord_squeezed_to_the_pictures_height() {
        use crate::geometry::{Pose, panel_matrix};
        let m = panel_matrix(&Pose { centre: [0.0, 1.2, -1.0], yaw: 30.0, ..Default::default() });
        let pl = Placement::from_matrix(&m, 1.6, 0.5625, 1.0);
        let (sheet_m, w) = sheet(&pl);
        let p = back_place(&pl, 1.0, true);
        assert_eq!(p.at, crate::overlay::At::Room(sheet_m));
        assert_eq!(p.width, w);
        assert!(w < pl.width, "a curved panel's chord is shorter than its arc");
        assert_eq!(p.texel, Some((1.0 * (w / pl.width)) as f32), "showing the picture: its own texel aspect, squeezed to the chord");
        assert_eq!(back_place(&pl, 0.7, true).texel, Some((0.7 * (w / pl.width)) as f32), "whatever the picture's is");
        assert_eq!(back_place(&pl, 0.7, false).texel, Some((w / pl.height) as f32), "the plain fill is squeezed to the picture's height");
        assert!(p.curve.is_none(), "a sheet is flat and says nothing about curvature");
        let turned = Placement { vert: true, ..pl };
        assert_eq!(back_place(&turned, 1.0, true).width, chord(turned.turned().width, 1.0).0, "a panel curved top to bottom: the chord of its turned width");
    }
}
