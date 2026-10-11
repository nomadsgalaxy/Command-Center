//! Where panels are. Poses live in SteamVR's standing space (x right, y up, -z forward), the same
//! convention cc-home's spots use, so a saved spot drops straight in. The curve formulas are
//! here too (`bend`, `sagitta`, `radius_for`, `chord`); the numbers they share are in limits.rs.

use crate::limits::FLAT_SAG;

pub type V3 = [f64; 3];
pub type Mat = [[f32; 4]; 3]; // OpenVR's HmdMatrix34_t rows

/// A saved place: the centre, the direction you look to see the front (yaw and pitch in
/// degrees), roll about the front, width in metres, curve radius (0 = flat), and whether the
/// curve runs top to bottom (vert) instead of left to right.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    pub centre: V3,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    pub width: f64,
    pub curve: f64,
    pub vert: bool,
}

pub fn dot(a: &V3, b: &V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: &V3, b: &V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub fn norm(v: &V3) -> f64 {
    dot(v, v).sqrt()
}

/// Rotates v about a unit axis by an angle in radians (Rodrigues' formula).
pub fn rotate(v: &V3, a: &V3, angle: f64) -> V3 {
    let (c, s, d) = (angle.cos(), angle.sin(), dot(a, v));
    let x = cross(a, v);
    [0, 1, 2].map(|i| v[i] * c + x[i] * s + a[i] * d * (1.0 - c))
}

/// The direction a yaw and pitch (degrees) look. Yaw 0 is -z, and positive yaw turns left.
pub fn direction(yaw: f64, pitch: f64) -> V3 {
    let (y, p) = (yaw.to_radians(), pitch.to_radians());
    [-y.sin() * p.cos(), p.sin(), -y.cos() * p.cos()]
}

/// The yaw and pitch (degrees) of a direction.
pub fn angles(d: &V3) -> (f64, f64) {
    let n = norm(d).max(1e-12);
    ((-d[0]).atan2(-d[2]).to_degrees(), (d[1] / n).clamp(-1.0, 1.0).asin().to_degrees())
}

/// A panel's matrix from a saved pose.
pub fn panel_matrix(p: &Pose) -> Mat {
    let (yw, pt, rl) = (p.yaw.to_radians(), p.pitch.to_radians(), p.roll.to_radians());
    let f = [-yw.sin() * pt.cos(), pt.sin(), -yw.cos() * pt.cos()];
    let z = [-f[0], -f[1], -f[2]]; // the front
    let n = (z[2] * z[2] + z[0] * z[0]).sqrt() + 1e-12;
    let x0 = [z[2] / n, 0.0, -z[0] / n]; // horizontal right
    let y0 = cross(&z, &x0);
    let (c, s) = (rl.cos(), rl.sin());
    let mut m = [[0f32; 4]; 3];
    for i in 0..3 {
        m[i][0] = (x0[i] * c + y0[i] * s) as f32;
        m[i][1] = (y0[i] * c - x0[i] * s) as f32;
        m[i][2] = z[i] as f32;
        m[i][3] = p.centre[i] as f32;
    }
    m
}

/// a · b for rigid poses (rotation plus translation, as OpenVR's 3x4 matrices).
pub fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut m = [[0f32; 4]; 3];
    for i in 0..3 {
        for j in 0..4 {
            m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum::<f32>() + if j == 3 { a[i][3] } else { 0.0 };
        }
    }
    m
}

/// The inverse of a rigid pose: transpose the rotation and rotate the translation back.
pub fn inv_rigid(a: &Mat) -> Mat {
    let mut m = [[0f32; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = a[j][i];
        }
        m[i][3] = -(0..3).map(|k| a[k][i] * a[k][3]).sum::<f32>();
    }
    m
}

/// Where a panel is right now: its axes, centre, size and curve.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub c: V3,
    pub x: V3,
    pub y: V3,
    pub z: V3,
    pub width: f64,
    pub height: f64,
    pub curve: f64,
    /// The curve is about the panel's horizontal axis (top to bottom) instead of its vertical one.
    pub vert: bool,
}

impl Placement {
    pub fn from_matrix(m: &Mat, width: f64, aspect: f64, curve: f64) -> Self {
        let col = |j: usize| [0, 1, 2].map(|i| m[i][j] as f64);
        Placement { c: col(3), x: col(0), y: col(1), z: col(2), width, height: width * aspect, curve, vert: false }
    }

    /// The same panel turned a quarter round so it curves about its vertical axis: x' = up,
    /// y' = left. SteamVR overlays only curve about their vertical axis, so this is how a
    /// vertically curved panel is shown: its overlay's matrix and width, and the hit maths.
    pub fn turned(&self) -> Placement {
        Placement { x: self.y, y: self.x.map(|v| -v), width: self.height, height: self.width, vert: false, ..*self }
    }

    /// Turns it back into a saved place (the inverse of panel_matrix).
    pub fn pose(&self) -> Pose {
        let (yaw, pitch) = angles(&self.z.map(|v| -v));
        let n = (self.z[2] * self.z[2] + self.z[0] * self.z[0]).sqrt() + 1e-12;
        let x0 = [self.z[2] / n, 0.0, -self.z[0] / n];
        let y0 = cross(&self.z, &x0);
        let roll = dot(&self.x, &y0).atan2(dot(&self.x, &x0)).to_degrees();
        Pose { centre: self.c, yaw, pitch, roll, width: self.width, curve: self.curve, vert: self.vert }
    }

    pub fn matrix(&self) -> Mat {
        let mut m = [[0f32; 4]; 3];
        for i in 0..3 {
            m[i] = [self.x[i] as f32, self.y[i] as f32, self.z[i] as f32, self.c[i] as f32];
        }
        m
    }

    /// Where a ray (origin o, unit direction d) meets the panel's surface, extended past its
    /// edges. Returns the distance along the ray, then u right and v up from the middle (along
    /// the arc when curved). Only the side facing the ray's origin counts.
    pub fn hit(&self, o: &V3, d: &V3) -> Option<(f64, f64, f64)> {
        if self.vert && self.curve > 0.0 {
            // On the turned panel u' = v and v' = -u.
            return self.turned().hit(o, d).map(|(t, u, v)| (t, -v, u));
        }
        // In the panel's own frame: x right, y up, z out of the front.
        let rel = [o[0] - self.c[0], o[1] - self.c[1], o[2] - self.c[2]];
        let lo = [dot(&rel, &self.x), dot(&rel, &self.y), dot(&rel, &self.z)];
        let ld = [dot(d, &self.x), dot(d, &self.y), dot(d, &self.z)];
        if self.curve <= 0.0 {
            if ld[2] >= -1e-9 || lo[2] <= 0.0 {
                return None; // parallel, going away, or from behind
            }
            let t = -lo[2] / ld[2];
            return Some((t, lo[0] + ld[0] * t, lo[1] + ld[1] * t));
        }
        // Curved about a vertical axis at (0, *, R): x^2 + (z - R)^2 = R^2, the near side (z < R).
        let r = self.curve;
        let (ox, oz) = (lo[0], lo[2] - r);
        let a = ld[0] * ld[0] + ld[2] * ld[2];
        if a < 1e-12 {
            return None;
        }
        let b = 2.0 * (ox * ld[0] + oz * ld[2]);
        let c = ox * ox + oz * oz - r * r;
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let sq = disc.sqrt();
        // From inside the cylinder (where you sit) the far root is the front surface.
        for t in [(-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a)] {
            let (x, z) = (lo[0] + ld[0] * t, lo[2] + ld[2] * t);
            let ang = x.atan2(r - z);
            // the front faces the axis: normal (-sin, 0, cos); a ray must meet it head on
            if t > 0.0 && z < r && ld[2] * ang.cos() - ld[0] * ang.sin() < 0.0 {
                return Some((t, r * ang, lo[1] + ld[1] * t));
            }
        }
        None
    }

    /// A point on the surface as a pose facing the way the surface does there. u is right and v
    /// up from the middle (along the arc when curved), dz is out of the surface.
    pub fn on_surface(&self, u: f64, v: f64, dz: f64) -> Mat {
        if self.vert && self.curve > 0.0 {
            // Back from the turned panel's axes: x = -y', y = x'.
            let m = self.turned().on_surface(v, -u, dz);
            return [0, 1, 2].map(|i| [-m[i][1], m[i][0], m[i][2], m[i][3]]);
        }
        let mut pos = [u, v, dz];
        let mut r = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        if self.curve > 0.0 {
            let a = u / self.curve;
            let (c, s) = (a.cos(), a.sin());
            pos[0] = self.curve * s - dz * s;
            pos[2] = self.curve - self.curve * c + dz * c;
            r = [[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]];
        }
        let mut m = [[0f32; 4]; 3];
        for i in 0..3 {
            let axis = [self.x[i], self.y[i], self.z[i]];
            for j in 0..3 {
                m[i][j] = (axis[0] * r[0][j] + axis[1] * r[1][j] + axis[2] * r[2][j]) as f32;
            }
            m[i][3] = (self.c[i] + axis[0] * pos[0] + axis[1] * pos[1] + axis[2] * pos[2]) as f32;
        }
        m
    }

    // ---- points and hits on the surface
    // u is right of the middle and v up from it, in metres along the surface (the arc, when curved).

    /// Whether (u, v) is on the surface grown by `margin` metres all round (negative: shrunk).
    pub fn covers(&self, u: f64, v: f64, margin: f64) -> bool {
        u.abs() <= self.width / 2.0 + margin && v.abs() <= self.height / 2.0 + margin
    }

    /// (u, v) moved to the nearest point on the surface.
    pub fn clamped(&self, u: f64, v: f64) -> (f64, f64) {
        (u.clamp(-self.width / 2.0, self.width / 2.0), v.clamp(-self.height / 2.0, self.height / 2.0))
    }

    /// Where a ray meets the surface, if that's inside its edges less `inset`: (distance, u, v).
    pub fn hit_in(&self, o: &V3, d: &V3, inset: f64) -> Option<(f64, f64, f64)> {
        self.hit(o, d).filter(|&(_, u, v)| self.covers(u, v, -inset))
    }

    /// Where a ray meets the surface or the card around it, whose margins are at the sides, below
    /// and above: (distance, u, v).
    pub fn hit_card(&self, o: &V3, d: &V3, [side, below, above]: [f64; 3]) -> Option<(f64, f64, f64)> {
        self.hit(o, d).filter(|&(_, u, v)| u.abs() <= self.width / 2.0 + side && v >= -self.height / 2.0 - below && v <= self.height / 2.0 + above)
    }

    /// The point (u, v) a pixel (x right, y down, from the top left) of a `size` picture is at.
    pub fn at_pixel(&self, (w, h): (u32, u32), x: f64, y: f64) -> (f64, f64) {
        ((x / w as f64 - 0.5) * self.width, (0.5 - y / h as f64) * self.height)
    }

    /// The pixel of a `size` picture that (u, v) is on, held inside the picture.
    pub fn pixel_at(&self, (w, h): (u32, u32), u: f64, v: f64) -> (f64, f64) {
        (((u / self.width + 0.5) * w as f64).clamp(0.0, w as f64 - 1.0), ((0.5 - v / self.height) * h as f64).clamp(0.0, h as f64 - 1.0))
    }

    /// (u, v) as fractions across and up the surface, 0 to 1 over its edges.
    pub fn fraction(&self, u: f64, v: f64) -> (f64, f64) {
        (u / self.width + 0.5, v / self.height + 0.5)
    }
}

// ------------------------------------------------------------------ rays and overlay events

/// A panel a ray meets, as `nearest` finds it: (slot, distance along the ray, u, v).
pub type Seen = (usize, f64, f64, f64);

/// The nearest of `places` (slot, placement) that a ray (origin o, unit direction d) meets inside
/// its edges.
pub fn nearest<'a>(places: impl Iterator<Item = (usize, &'a Placement)>, o: &V3, d: &V3) -> Option<Seen> {
    places.filter_map(|(i, pl)| pl.hit_in(o, d, 0.0).map(|(t, u, v)| (i, t, u, v))).min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Where a ray meets a bar (the taskbar, or Plasma's), if no panel it meets (`seen`) is nearer:
/// (distance, u, v). The Frame draws by depth, so a nearer panel covers the bar, and SteamVR's laser
/// sees it that way too. `inset` is the clear margin at its edges that it doesn't take.
pub fn bar_hit(bar: Option<Placement>, inset: f64, o: &V3, d: &V3, seen: Option<Seen>) -> Option<(f64, f64, f64)> {
    let b = bar?;
    b.hit(o, d).filter(|&(t, u, v)| b.covers(u, v, -inset) && seen.is_none_or(|h| h.1 > t))
}

/// A ray on `pl` or its card (margins: sides, below, above), if it's nearer than `seen` and
/// `bars` (a distance): (distance, u, v).
pub fn card_at(pl: &Placement, margins: [f64; 3], o: &V3, d: &V3, seen: Option<Seen>, bars: f64) -> Option<(f64, f64, f64)> {
    pl.hit_card(o, d, margins).filter(|&(t, _, _)| seen.is_none_or(|h| h.1 > t) && t < bars)
}

/// An overlay mouse event's point (x right, y up, from the overlay's bottom left, in its mouse
/// scale) as a point on the surface it shows, still from the surface's bottom left. A panel
/// curved top to bottom (`vert`) is shown turned a quarter (`Placement::turned`), so its overlay's
/// x runs up the surface and its y from the surface's right edge, `w` away. For pixel indexes pass
/// the width less one.
pub fn surface_point<T: Copy + std::ops::Sub<Output = T>>(vert: bool, w: T, x: T, y: T) -> (T, T) {
    if vert { (w - y, x) } else { (x, y) }
}

/// A point `y` up from the bottom of something `h` tall, counted down from its top instead, which
/// is how the windows' rows and the pictures' pixels run. Overlay mouse events count from the bottom.
pub fn from_top<T: std::ops::Sub<Output = T>>(h: T, y: T) -> T {
    h - y
}

// ------------------------------------------------------------------ in front of the head

/// The headset's pose when it isn't tracked yet: 1.6 m up, looking down -z.
pub const STAND_IN_HEAD: Mat = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.6], [0.0, 0.0, 1.0, 0.0]];

/// The level direction (no up or down) `v` points in, as a unit vector. Straight up or down has
/// none, and comes out as zeros.
pub fn level(v: V3) -> V3 {
    let f = [v[0], 0.0, v[2]];
    let len = norm(&f).max(1e-6);
    f.map(|c| c / len)
}

/// The level direction a head pose faces.
pub fn level_ahead(head: &Mat) -> V3 {
    level([-head[0][2] as f64, 0.0, -head[2][2] as f64])
}

/// The point `dist` metres ahead of `eye` along the level direction `f`, `drop` below it and
/// `aside` to the left of the line (a staircase of panels uses both).
pub fn ahead_of(eye: &V3, f: &V3, dist: f64, drop: f64, aside: f64) -> V3 {
    [eye[0] + f[0] * dist - f[2] * aside, eye[1] - drop, eye[2] + f[2] * dist + f[0] * aside]
}

// ------------------------------------------------------------------ curves
// A panel's curve is a radius in metres (0 is flat). SteamVR wants it as a fraction, and everything
// that bends a panel agrees on the same rules, so they're written once, here.

/// SetOverlayCurvature's value for an overlay `width` wide on a circle of radius `r` (0: flat):
/// width / (2πr), and at most 1, a closed cylinder.
pub fn bend(width: f64, r: f64) -> f32 {
    if r > 0.0 { (width / (2.0 * std::f64::consts::PI * r)).min(1.0) as f32 } else { 0.0 }
}

/// The smallest radius an arc `w` wide can have: a half circle (R = w/π). Past that it would wrap
/// round behind itself.
pub fn least_radius(w: f64) -> f64 {
    w / std::f64::consts::PI
}

/// How far the edges of an arc `w` wide on radius `r` come toward you from its middle: the arc's
/// sagitta, R(1 − cos(w/2R)). 0 when flat.
pub fn sagitta(w: f64, r: f64) -> f64 {
    if r > 0.0 { r * (1.0 - (w / (2.0 * r)).cos()) } else { 0.0 }
}

/// The curve radius that puts a w-wide panel's edges `sag` metres in front of its middle
/// (`sagitta`'s inverse). Under `FLAT_SAG` it's 0 (flat), and it never wraps past a half circle.
pub fn radius_for(sag: f64, w: f64) -> f64 {
    let least = least_radius(w);
    if sag < FLAT_SAG {
        return 0.0;
    }
    if sag >= least {
        return least;
    }
    let (mut lo, mut hi) = (least, 1.0e4); // sagitta falls as the radius grows
    for _ in 0..60 {
        let mid = (lo * hi).sqrt();
        if sagitta(w, mid) > sag { lo = mid } else { hi = mid }
    }
    (lo * hi).sqrt()
}

/// A wider panel keeps its curve, except it can't wrap past a half circle, so then the radius
/// grows with it. That was my choice: resizing should never dead-end.
pub fn curve_for_width(curve: f64, width: f64) -> f64 {
    if curve > 0.0 { curve.max(least_radius(width)) } else { 0.0 }
}

/// A flat sheet for an arc `w` long round radius `curve` (0 = flat), as (width, metres forward).
/// It's the arc's chord, which sits at most the sagitta off the arc (in its middle).
pub fn chord(w: f64, curve: f64) -> (f64, f64) {
    if curve <= 0.0 {
        return (w, 0.0);
    }
    let a = (w / (2.0 * curve)).min(std::f64::consts::PI); // half the arc's angle
    (2.0 * curve * a.sin(), curve * (1.0 - a.cos()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_matrix_is_a_rotation_and_faces_back_along_the_look() {
        let p = Pose { centre: [0.3, 1.2, -0.8], yaw: 35.0, pitch: -10.0, roll: 7.0, width: 1.0, curve: 0.0, vert: false };
        let m = panel_matrix(&p);
        let pl = Placement::from_matrix(&m, 1.0, 0.5, 0.0);
        for (a, b) in [(pl.x, pl.y), (pl.y, pl.z), (pl.x, pl.z)] {
            assert!(dot(&a, &b).abs() < 1e-5);
        }
        let look = [-(35f64.to_radians().sin()) * (-10f64).to_radians().cos(), (-10f64).to_radians().sin(), -(35f64.to_radians().cos()) * (-10f64).to_radians().cos()];
        assert!((dot(&pl.z, &look) + 1.0).abs() < 1e-5);
        assert!(norm(&[pl.c[0] - 0.3, pl.c[1] - 1.2, pl.c[2] + 0.8]) < 1e-6);
    }

    #[test]
    fn hit_finds_surface_points_flat_and_curved() {
        let p = Pose { centre: [0.2, 1.4, -1.1], yaw: 20.0, pitch: 5.0, roll: 3.0, width: 1.3, curve: 0.0, vert: false };
        let m = panel_matrix(&p);
        for curve in [0.0, 1.0, 1.8] {
            let pl = Placement::from_matrix(&m, 1.3, 0.3, curve);
            let eye = [0.0, 1.6, 0.0];
            for (u, v) in [(0.0, 0.0), (0.5, 0.15), (-0.6, -0.18), (0.3, -0.05)] {
                let s = pl.on_surface(u, v, 0.0);
                let pt = [s[0][3] as f64, s[1][3] as f64, s[2][3] as f64];
                let d = [pt[0] - eye[0], pt[1] - eye[1], pt[2] - eye[2]];
                let n = norm(&d);
                let (t, hu, hv) = pl.hit(&eye, &d.map(|x| x / n)).expect("hit");
                assert!((t - n).abs() < 1e-4 && (hu - u).abs() < 1e-4 && (hv - v).abs() < 1e-4, "curve {curve}: {t} {hu} {hv} vs {n} {u} {v}");
            }
            // From behind the panel: no hit.
            let behind = [pl.c[0] - pl.z[0], pl.c[1] - pl.z[1], pl.c[2] - pl.z[2]];
            assert!(pl.hit(&behind, &pl.z).is_none(), "curve {curve}: hit from behind");
            // From off to the side, outside the curve, at the middle: the front, not the back.
            let side = [pl.c[0] + pl.x[0] * 1.5 + pl.z[0] * 0.5, pl.c[1] + pl.x[1] * 1.5 + pl.z[1] * 0.5, pl.c[2] + pl.x[2] * 1.5 + pl.z[2] * 0.5];
            let to = [pl.c[0] - side[0], pl.c[1] - side[1], pl.c[2] - side[2]];
            let n = norm(&to);
            let (t, u, _) = pl.hit(&side, &to.map(|x| x / n)).expect("side hit");
            assert!((t - n).abs() < 1e-4 && u.abs() < 1e-4, "curve {curve}: side hit {t} {u}");
        }
    }

    #[test]
    fn vertical_curve_bends_top_to_bottom_and_hits_round_trip() {
        let p = Pose { centre: [0.5, 1.3, -0.7], yaw: -40.0, pitch: 2.0, roll: -1.0, width: 0.39, curve: 0.0, vert: false };
        let mut pl = Placement::from_matrix(&panel_matrix(&p), 0.39, 0.698 / 0.393, 1.09);
        pl.vert = true;
        let local = |m: Mat| {
            let rel = [0, 1, 2].map(|i| m[i][3] as f64 - pl.c[i]);
            [dot(&rel, &pl.x), dot(&rel, &pl.y), dot(&rel, &pl.z)]
        };
        // Bent along v only: the top comes out towards the viewer, the side edges don't.
        let top = local(pl.on_surface(0.0, 0.3, 0.0));
        let side = local(pl.on_surface(0.18, 0.0, 0.0));
        assert!(top[2] > 0.03 && (top[1] - 1.09 * (0.3f64 / 1.09).sin()).abs() < 1e-5, "{top:?}");
        assert!(side[2].abs() < 1e-6 && (side[0] - 0.18).abs() < 1e-6, "{side:?}");
        // The surface's x axis stays the panel's right; its normal tilts down at the top.
        let m = pl.on_surface(0.0, 0.3, 0.0);
        assert!(dot(&[0, 1, 2].map(|i| m[i][0] as f64), &pl.x) > 0.9999);
        assert!(dot(&[0, 1, 2].map(|i| m[i][2] as f64), &pl.y) < -0.2);
        let eye = [0.0, 1.6, 0.0];
        for (u, v) in [(0.0, 0.0), (0.15, 0.3), (-0.17, -0.32), (0.05, -0.1)] {
            let s = pl.on_surface(u, v, 0.0);
            let pt = [s[0][3] as f64, s[1][3] as f64, s[2][3] as f64];
            let d = [pt[0] - eye[0], pt[1] - eye[1], pt[2] - eye[2]];
            let n = norm(&d);
            let (t, hu, hv) = pl.hit(&eye, &d.map(|x| x / n)).expect("hit");
            assert!((t - n).abs() < 1e-4 && (hu - u).abs() < 1e-4 && (hv - v).abs() < 1e-4, "{t} {hu} {hv} vs {n} {u} {v}");
        }
        let behind = [0, 1, 2].map(|i| pl.c[i] - pl.z[i]);
        assert!(pl.hit(&behind, &pl.z).is_none());
        // Its turned form: x' up, y' left, the visible height as its width.
        let t = pl.turned();
        assert!(dot(&t.x, &pl.y) > 0.9999 && dot(&t.y, &pl.x) < -0.9999 && (t.width - pl.height).abs() < 1e-9);
    }

    #[test]
    fn pose_round_trips_and_rigid_inverse_undoes() {
        let p = Pose { centre: [0.4, 1.3, -1.2], yaw: -50.0, pitch: 12.0, roll: 8.0, width: 1.4, curve: 1.9, vert: false };
        let q = Placement::from_matrix(&panel_matrix(&p), p.width, 0.5, p.curve).pose();
        for (a, b) in [(p.yaw, q.yaw), (p.pitch, q.pitch), (p.roll, q.roll), (p.width, q.width), (p.curve, q.curve)] {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
        assert!(norm(&[p.centre[0] - q.centre[0], p.centre[1] - q.centre[1], p.centre[2] - q.centre[2]]) < 1e-5);
        let a = panel_matrix(&p);
        let b = panel_matrix(&Pose { centre: [-1.0, 0.2, 0.5], yaw: 100.0, pitch: -30.0, roll: 0.0, width: 1.0, curve: 0.0, vert: false });
        let back = mul(&inv_rigid(&a), &mul(&a, &b));
        for i in 0..3 {
            for j in 0..4 {
                assert!((back[i][j] - b[i][j]).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn angles_invert_direction() {
        for (y, p) in [(0.0, 0.0), (35.0, -20.0), (-120.0, 60.0)] {
            let (y2, p2) = angles(&direction(y, p));
            assert!((y - y2).abs() < 1e-9 && (p - p2).abs() < 1e-9);
        }
    }

    #[test]
    fn side_edge_depth_gives_the_curve() {
        let w = 1.6;
        assert_eq!(radius_for(0.005, w), 0.0); // under a centimetre is flat
        assert_eq!(radius_for(5.0, w), w / std::f64::consts::PI); // never past a half circle
        for r in [0.8, 1.5, 3.0] {
            let sag = r * (1.0 - (w / (2.0 * r)).cos());
            assert!((radius_for(sag, w) - r).abs() < 1e-6, "{r}");
            assert_eq!(sagitta(w, r), sag);
        }
        assert_eq!(sagitta(w, 0.0), 0.0, "flat");
        assert_eq!(curve_for_width(1.0, 2.0), 1.0);
        assert!((curve_for_width(1.0, 4.0) - 4.0 / std::f64::consts::PI).abs() < 1e-12);
        assert_eq!(curve_for_width(0.0, 4.0), 0.0);
    }

    #[test]
    fn bend_is_width_over_the_circumference_and_closes_at_one() {
        assert_eq!(bend(2.0, 0.0), 0.0, "flat");
        assert_eq!(bend(2.0, -1.0), 0.0, "no negative radius");
        assert!((bend(1.0, 1.0) - 1.0 / (2.0 * std::f32::consts::PI)).abs() < 1e-7);
        assert!((bend(std::f64::consts::PI, 1.0) - 0.5).abs() < 1e-7, "a half circle is half a cylinder");
        assert_eq!(bend(100.0, 1.0), 1.0, "a closed cylinder at most");
    }

    #[test]
    fn the_chord_is_flat_for_a_flat_panel_and_reaches_across_a_half_circle() {
        assert_eq!(chord(1.5, 0.0), (1.5, 0.0));
        let r = 1.0;
        let (w, fwd) = chord(std::f64::consts::PI * r, r); // a half circle
        assert!((w - 2.0 * r).abs() < 1e-12 && (fwd - r).abs() < 1e-12, "{w} {fwd}");
        let (w, fwd) = chord(10.0, r); // wrapped past a whole circle: capped at half
        assert!(w.abs() < 1e-9 && (fwd - 2.0 * r).abs() < 1e-12, "{w} {fwd}");
        // short of a half circle, how far the chord sits behind the arc's middle is the sagitta
        assert!((chord(1.2, 2.0).1 - sagitta(1.2, 2.0)).abs() < 1e-12);
    }

    fn panel(width: f64, height: f64, curve: f64) -> Placement {
        Placement::from_matrix(&panel_matrix(&Pose { centre: [0.2, 1.3, -1.2], yaw: 15.0, pitch: -3.0, roll: 2.0, ..Default::default() }), width, height / width, curve)
    }

    #[test]
    fn covers_clamps_and_maps_pixels_on_the_surface() {
        let pl = panel(1.6, 0.9, 0.0);
        assert!(pl.covers(0.8, 0.45, 0.0) && !pl.covers(0.801, 0.0, 0.0) && !pl.covers(0.0, 0.451, 0.0), "its edges");
        assert!(pl.covers(0.85, 0.0, 0.05) && !pl.covers(0.85, 0.0, 0.04), "grown by a margin");
        assert!(pl.covers(0.75, 0.0, -0.05) && !pl.covers(0.76, 0.0, -0.05), "and shrunk");
        assert_eq!(pl.clamped(2.0, -2.0), (0.8, -0.45));
        assert_eq!(pl.clamped(0.1, 0.2), (0.1, 0.2));
        // a 1920x1080 picture: the middle pixel is the middle, the top-left pixel is the top-left
        assert_eq!(pl.at_pixel((1920, 1080), 960.0, 540.0), (0.0, 0.0));
        assert_eq!(pl.at_pixel((1920, 1080), 0.0, 0.0), (-0.8, 0.45));
        let (x, y) = pl.pixel_at((1920, 1080), -0.8, 0.45);
        assert_eq!((x, y), (0.0, 0.0));
        assert_eq!(pl.pixel_at((1920, 1080), 9.0, -9.0), (1919.0, 1079.0), "held inside the picture");
        let (u, v) = (0.31, -0.12);
        let (x, y) = pl.pixel_at((1920, 1080), u, v);
        let (u2, v2) = pl.at_pixel((1920, 1080), x, y);
        assert!((u - u2).abs() < 1e-9 && (v - v2).abs() < 1e-9, "and back");
        assert_eq!(pl.fraction(0.0, 0.0), (0.5, 0.5));
        assert_eq!(pl.fraction(-0.8, 0.45), (0.0, 1.0));
    }

    #[test]
    fn a_ray_hits_inside_the_edges_or_the_card_around_them() {
        let pl = panel(1.6, 0.9, 0.0);
        let eye = [0.0, 1.6, 0.0];
        let toward = |u: f64, v: f64| {
            let m = pl.on_surface(u, v, 0.0);
            let d = [m[0][3] as f64 - eye[0], m[1][3] as f64 - eye[1], m[2][3] as f64 - eye[2]];
            d.map(|x| x / norm(&d))
        };
        assert!(pl.hit_in(&eye, &toward(0.7, 0.4), 0.0).is_some() && pl.hit_in(&eye, &toward(0.9, 0.0), 0.0).is_none(), "inside its edges");
        assert!(pl.hit_in(&eye, &toward(0.79, 0.0), 0.02).is_none() && pl.hit_in(&eye, &toward(0.77, 0.0), 0.02).is_some(), "less a clear margin");
        let card = [0.02, 0.05, 0.03]; // sides, below, above
        assert!(pl.hit_card(&eye, &toward(0.81, 0.0), card).is_some() && pl.hit_card(&eye, &toward(0.83, 0.0), card).is_none(), "its sides");
        assert!(pl.hit_card(&eye, &toward(0.0, -0.49), card).is_some() && pl.hit_card(&eye, &toward(0.0, -0.51), card).is_none(), "below, for the bar");
        assert!(pl.hit_card(&eye, &toward(0.0, 0.47), card).is_some() && pl.hit_card(&eye, &toward(0.0, 0.49), card).is_none(), "above, for the tab");
    }

    #[test]
    fn the_nearest_panel_wins_and_bars_and_cards_give_way_to_a_nearer_one() {
        let at = |z: f64, w: f64| Placement::from_matrix(&panel_matrix(&Pose { centre: [0.0, 1.0, z], ..Default::default() }), w, 0.5, 0.0);
        let (far, near, narrow) = (at(-2.0, 2.0), at(-1.0, 2.0), at(-0.5, 0.2));
        let places = [far, near, narrow];
        let (o, d) = ([0.0, 1.0, 0.0], [0.0, 0.0, -1.0]);
        let hit = nearest(places.iter().enumerate(), &o, &d);
        assert_eq!(hit.map(|h| (h.0, (h.1 * 1000.0).round())), Some((2, 500.0)), "the narrow one in front");
        let off = [0.3, 0.0, -1.0].map(|x| x / (1.0f64 + 0.09f64).sqrt());
        assert_eq!(nearest(places.iter().enumerate(), &o, &off).map(|h| h.0), Some(1), "off the narrow one: the next");
        assert_eq!(nearest(places.iter().enumerate().filter(|&(i, _)| i != 2), &o, &d).map(|h| h.0), Some(1), "the caller says which are reachable");
        assert!(nearest(std::iter::empty(), &o, &d).is_none());
        // a bar between: in front of the far one, behind the near one
        let bar = |z: f64| Some(at(z, 1.0));
        assert!(bar_hit(bar(-1.5), 0.0, &o, &d, Some((0, 2.0, 0.0, 0.0))).is_some(), "nearer than the panel it would be covered by");
        assert!(bar_hit(bar(-1.5), 0.0, &o, &d, Some((1, 1.0, 0.0, 0.0))).is_none(), "covered by a nearer panel");
        assert!(bar_hit(None, 0.0, &o, &d, None).is_none());
        assert!(bar_hit(bar(-1.5), 0.49, &o, &d, None).is_none(), "the clear margin");
        let card = [0.02, 0.05, 0.03];
        assert!(card_at(&near, card, &o, &d, None, f64::INFINITY).is_some());
        assert!(card_at(&near, card, &o, &d, Some((2, 0.5, 0.0, 0.0)), f64::INFINITY).is_none(), "a nearer panel covers it");
        assert!(card_at(&near, card, &o, &d, None, 0.9).is_none(), "and a nearer bar");
    }

    #[test]
    fn overlay_events_are_counted_from_the_bottom_and_a_turned_panels_run_sideways() {
        assert_eq!(surface_point(false, 800.0, 30.0, 40.0), (30.0, 40.0));
        assert_eq!(surface_point(true, 800.0, 30.0, 40.0), (760.0, 30.0), "x runs up the surface, y from its right edge");
        assert_eq!(from_top(600.0, 40.0), 560.0);
        // pixel indexes: the last pixel of a 1920x1080 picture is 1919, 1079 from the top left
        let (x, y) = surface_point(false, 1920.0f32 - 1.0, 5.0, 0.0);
        assert_eq!((x, from_top(1080.0f32 - 1.0, y)), (5.0, 1079.0));
        // the turned panel's overlay is 1080 wide and 1920 tall; its bottom-left pixel is the picture's bottom-right
        let (x, y) = surface_point(true, 1920.0f32 - 1.0, 0.0, 0.0);
        assert_eq!((x, from_top(1080.0f32 - 1.0, y)), (1919.0, 1079.0));
    }

    #[test]
    fn level_directions_ignore_up_and_down_and_points_ahead_stay_on_the_line() {
        // a head turned to look along -x
        let m = [[0.0, 0.0, 1.0, 1.0], [0.0, 1.0, 0.0, 1.7], [-1.0, 0.0, 0.0, 2.0]];
        let f = level_ahead(&m);
        assert!((f[0] + 1.0).abs() < 1e-9 && f[1] == 0.0 && f[2].abs() < 1e-9, "{f:?}");
        assert_eq!(level([0.0, 5.0, 0.0]), [0.0, 0.0, 0.0], "straight up has no level direction");
        assert!((norm(&level([3.0, 9.0, 4.0])) - 1.0).abs() < 1e-12);
        assert_eq!(level_ahead(&STAND_IN_HEAD), [0.0, 0.0, -1.0], "the stand-in looks down -z");
        let eye = [1.0, 1.7, 2.0];
        assert_eq!(ahead_of(&eye, &[0.0, 0.0, -1.0], 1.2, 0.0, 0.0), [1.0, 1.7, 0.8]);
        let at = ahead_of(&eye, &[0.0, 0.0, -1.0], 1.2, 0.1, 0.1);
        assert!((at[0] - (1.0 + 0.0 - (-1.0) * 0.1)).abs() < 1e-12 && (at[1] - 1.6).abs() < 1e-12 && (at[2] - 0.8).abs() < 1e-12, "a step down and to the left: {at:?}");
    }

    #[test]
    fn rotate_quarter_turn() {
        let v = rotate(&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0], std::f64::consts::FRAC_PI_2);
        assert!(norm(&[v[0], v[1], v[2] + 1.0]) < 1e-9);
    }
}
