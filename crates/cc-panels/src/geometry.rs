//! Where panels are. Poses live in SteamVR's standing space (x right, y up, -z forward), the same
//! convention cc-home's spots use, so a saved spot drops straight in.

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
    fn rotate_quarter_turn() {
        let v = rotate(&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0], std::f64::consts::FRAC_PI_2);
        assert!(norm(&[v[0], v[1], v[2] + 1.0]) < 1e-9);
    }
}
