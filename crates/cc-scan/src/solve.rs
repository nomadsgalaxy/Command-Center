//! The scan's maths, ported from home/solve.py. It does two things:
//! (1) It finds the mirror camera's intrinsics and the left eye's offset from tags at known places
//! (cc-home refit). Without a refit it uses the last good camera.
//! (2) It finds each real monitor's pose in the room and its shape: flat, or curved left-right "h"
//! or top-bottom "v" with a radius. That's fitted jointly over every passthrough frame, and each
//! frame gets its own head pose, at its time minus the mirror's lag. The lag is fitted too.
//! Misreads (something in the room decoded as one of our tags) get dropped first. scipy's
//! least_squares (soft_l1) becomes a robust Levenberg-Marquardt on the same cost, and solvePnP
//! becomes a homography pose refined the same way.
//!
//! job: {"visible": shots.json, "hidden": shots.json, "head": track.json, "screens": {n: ...},
//!       "camera": [10], "refit_camera": bool, "monitors": [{"name", "screen", "tags": {id: [[fx, fy] x4]},
//!       "mm": [w, h], "curve"?, "radius"?, "virtual"?}]}
//! Output: "#" lines, then one JSON line per placed monitor, the same way solve.py prints them.
use crate::panels::{rotmat, rotvec};
use nalgebra::{DMatrix, DVector, Matrix3, Matrix4, Vector3, Vector4};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

const MIN_CHECK_PX: f64 = 30.0;
const MIN_TAGS: usize = 4;

type Uv = [f64; 2];

// ---------------------------------------------------------------- numbers

/// Same as numpy's round(x, n): multiply, round half to even, then divide.
pub fn round(x: f64, n: i32) -> f64 {
    let p = 10f64.powi(n);
    (x * p).round_ties_even() / p
}

pub fn median(v: &[f64]) -> f64 {
    percentile(v, 50.0)
}

/// Same as numpy.percentile, with linear interpolation.
pub fn percentile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = (s.len() - 1) as f64 * q / 100.0;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
}

/// The robust least squares solve.py uses (loss soft_l1, f_scale c). Returns the x that minimises
/// 0.5 c^2 sum 2(sqrt(1 + (f/c)^2) - 1), and the residuals at that x.
pub fn least_squares(fun: &dyn Fn(&DVector<f64>) -> DVector<f64>, x0: DVector<f64>, c: f64) -> (DVector<f64>, DVector<f64>) {
    let cost = |f: &DVector<f64>| f.iter().map(|r| 2.0 * ((1.0 + (r / c).powi(2)).sqrt() - 1.0)).sum::<f64>() * 0.5 * c * c;
    let n = x0.len();
    let mut x = x0;
    let mut f = fun(&x);
    let mut e = cost(&f);
    let mut lambda = 1e-3;
    let eps = f64::EPSILON.sqrt();
    for _ in 0..100 * n.max(1) {
        // Forward-difference Jacobian, with scipy's '2-point' steps.
        let mut j = DMatrix::zeros(f.len(), n);
        for k in 0..n {
            let h = eps * x[k].abs().max(1.0) * if x[k] < 0.0 { -1.0 } else { 1.0 };
            let mut xk = x.clone();
            xk[k] += h;
            let fk = fun(&xk);
            j.set_column(k, &((fk - &f) / h));
        }
        // IRLS weights: rho'((f/c)^2).
        let w = DVector::from_iterator(f.len(), f.iter().map(|r| 1.0 / (1.0 + (r / c).powi(2)).sqrt()));
        let jw = DMatrix::from_fn(f.len(), n, |i, k| j[(i, k)] * w[i]);
        let a = jw.transpose() * &j;
        let g = jw.transpose() * &f;
        if g.amax() < 1e-10 {
            break;
        }
        let mut improved = false;
        for _ in 0..30 {
            let mut m = a.clone();
            for k in 0..n {
                m[(k, k)] += lambda * a[(k, k)].max(1e-12);
            }
            let Some(step) = m.lu().solve(&(-&g)) else {
                lambda *= 10.0;
                continue;
            };
            let xn = &x + &step;
            let fnew = fun(&xn);
            let en = cost(&fnew);
            if en.is_finite() && en < e {
                let done = step.norm() < 1e-10 * (x.norm() + 1e-10) || (e - en) < 1e-12 * e;
                x = xn;
                f = fnew;
                e = en;
                lambda = (lambda / 3.0).max(1e-12);
                improved = true;
                if done {
                    return (x, f);
                }
                break;
            }
            lambda *= 4.0;
        }
        if !improved {
            break;
        }
    }
    (x, f)
}

// ---------------------------------------------------------------- geometry

/// Local points (x right, y up, z towards the viewer) of a panel curved around the viewer. uv is in
/// metres along its surface, measured from the middle.
pub fn surface(uv: &[Uv], axis: &str, r: f64) -> Vec<Vector3<f64>> {
    uv.iter().map(|&[u, v]| match axis {
        "h" if r != 0.0 => Vector3::new(r * (u / r).sin(), v, r * (1.0 - (u / r).cos())),
        "v" if r != 0.0 => Vector3::new(u, r * (v / r).sin(), r * (1.0 - (v / r).cos())),
        _ => Vector3::new(u, v, 0.0),
    }).collect()
}

pub fn frac_to_uv(f: &[f64], size: (f64, f64)) -> Uv {
    [(f[0] - 0.5) * size.0, (0.5 - f[1]) * size.1]
}

fn mat34(v: &Value) -> Matrix4<f64> {
    let mut m = Matrix4::identity();
    for i in 0..3 {
        for j in 0..4 {
            m[(i, j)] = v[i][j].as_f64().unwrap_or(0.0);
        }
    }
    m
}

fn flip() -> Matrix4<f64> {
    Matrix4::from_diagonal(&Vector4::new(1.0, -1.0, -1.0, 1.0))
}

fn eye_of(cam: &[f64]) -> Matrix4<f64> {
    let mut e = Matrix4::identity();
    e.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&Vector3::new(cam[4], cam[5], cam[6])));
    e[(0, 3)] = cam[7];
    e[(1, 3)] = cam[8];
    e[(2, 3)] = cam[9];
    e
}

fn apply(m: &Matrix4<f64>, p: &Vector3<f64>) -> Vector3<f64> {
    (m * Vector4::new(p.x, p.y, p.z, 1.0)).xyz()
}

#[derive(Clone, Copy)]
struct K {
    fx: f64,
    fy: f64,
    cx: f64,
    cy: f64,
}

fn project(k: K, c: &Vector3<f64>) -> [f64; 2] {
    [c.x / c.z * k.fx + k.cx, c.y / c.z * k.fy + k.cy]
}

/// A planar object's pose (object -> OpenCV camera) from its points (z = 0) and pixels. It takes the
/// homography's pose and refines it on the reprojection error. It stands in for solvePnP's SQPNP /
/// IPPE_SQUARE, which is fine because it only seeds the fit and locates single tags for the misread
/// check.
fn pnp(obj: &[Vector3<f64>], px: &[[f64; 2]], k: K) -> Option<(Vector3<f64>, Vector3<f64>)> {
    if obj.len() < 4 {
        return None;
    }
    // Normalised image points, and a DLT homography object(x, y) -> image, with Hartley normalisation.
    let img: Vec<[f64; 2]> = px.iter().map(|p| [(p[0] - k.cx) / k.fx, (p[1] - k.cy) / k.fy]).collect();
    let norm = |pts: &[[f64; 2]]| {
        let n = pts.len() as f64;
        let (mx, my) = (pts.iter().map(|p| p[0]).sum::<f64>() / n, pts.iter().map(|p| p[1]).sum::<f64>() / n);
        let d = pts.iter().map(|p| ((p[0] - mx).powi(2) + (p[1] - my).powi(2)).sqrt()).sum::<f64>() / n;
        let s = if d > 0.0 { std::f64::consts::SQRT_2 / d } else { 1.0 };
        Matrix3::new(s, 0.0, -s * mx, 0.0, s, -s * my, 0.0, 0.0, 1.0)
    };
    let o2: Vec<[f64; 2]> = obj.iter().map(|p| [p.x, p.y]).collect();
    let (to, ti) = (norm(&o2), norm(&img));
    let mut a = DMatrix::zeros(2 * obj.len(), 9);
    for (i, (o, m)) in o2.iter().zip(&img).enumerate() {
        let on = to * Vector3::new(o[0], o[1], 1.0);
        let mn = ti * Vector3::new(m[0], m[1], 1.0);
        let (x, y, u, v) = (on.x, on.y, mn.x / mn.z, mn.y / mn.z);
        let r1 = [-x, -y, -1.0, 0.0, 0.0, 0.0, u * x, u * y, u];
        let r2 = [0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v];
        for j in 0..9 {
            a[(2 * i, j)] = r1[j];
            a[(2 * i + 1, j)] = r2[j];
        }
    }
    let svd = (a.transpose() * &a).svd(false, true);
    let vt = svd.v_t?;
    let (imin, _) = svd.singular_values.iter().enumerate().min_by(|x, y| x.1.partial_cmp(y.1).unwrap())?;
    let h = vt.row(imin);
    let hn = Matrix3::new(h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8]);
    let hm = ti.try_inverse()? * hn * to;
    // Decompose: [r1 r2 t] ~ H.
    let (h1, h2, h3) = (hm.column(0).into_owned(), hm.column(1).into_owned(), hm.column(2).into_owned());
    let mut s = 2.0 / (h1.norm() + h2.norm());
    if (h3 * s).z < 0.0 {
        s = -s; // Keep it in front of the camera.
    }
    let (r1, r2, t) = (h1 * s, h2 * s, h3 * s);
    let r3 = r1.cross(&r2);
    let m = Matrix3::from_columns(&[r1, r2, r3]);
    let svd = m.svd(true, true);
    let mut r = svd.u? * svd.v_t?;
    if r.determinant() < 0.0 {
        r = -r;
    }
    // Refine: 6 parameters, plain least squares on pixels (soft_l1 with a huge scale).
    let x0 = DVector::from_vec(vec![rotvec(&r).x, rotvec(&r).y, rotvec(&r).z, t.x, t.y, t.z]);
    let res = |p: &DVector<f64>| {
        let (rr, tt) = (rotmat(&Vector3::new(p[0], p[1], p[2])), Vector3::new(p[3], p[4], p[5]));
        DVector::from_iterator(2 * obj.len(), obj.iter().zip(px).flat_map(|(o, q)| {
            let c = rr * o + tt;
            let pr = project(k, &c);
            [pr[0] - q[0], pr[1] - q[1]]
        }))
    };
    let (p, _) = least_squares(&res, x0, 1e6);
    Some((Vector3::new(p[0], p[1], p[2]), Vector3::new(p[3], p[4], p[5])))
}

// ---------------------------------------------------------------- the camera refit

fn cam_points(p: &DVector<f64>, heads: &[Matrix4<f64>], world: &[Vector3<f64>]) -> Vec<Vector3<f64>> {
    let mut eye = Matrix4::identity();
    eye.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&Vector3::new(p[4], p[5], p[6])));
    eye[(0, 3)] = p[7];
    eye[(1, 3)] = p[8];
    eye[(2, 3)] = p[9];
    heads.iter().zip(world).map(|(h, w)| {
        let w2c = flip() * (h * eye).try_inverse().unwrap_or(Matrix4::identity());
        apply(&w2c, w)
    }).collect()
}

/// Fits fx fy cx cy, the eye rotation and the eye offset robustly, from virtual corners at known places.
fn fit_camera(heads: &[Matrix4<f64>], world: &[Vector3<f64>], pixels: &[[f64; 2]]) -> (Vec<f64>, f64, f64) {
    let res = |p: &DVector<f64>| {
        let c = cam_points(p, heads, world);
        DVector::from_iterator(2 * c.len(), c.iter().zip(pixels).flat_map(|(c, q)| [c.x / c.z * p[0] + p[2] - q[0], c.y / c.z * p[1] + p[3] - q[1]]))
    };
    let mut p0 = vec![1070.0, 1070.0, 960.0, 540.0];
    p0.extend([0.0; 6]);
    let (x, f) = least_squares(&res, DVector::from_vec(p0), 2.0);
    let e: Vec<f64> = f.as_slice().chunks(2).map(|r| (r[0] * r[0] + r[1] * r[1]).sqrt()).collect();
    (x.as_slice().to_vec(), median(&e), percentile(&e, 90.0))
}

// ---------------------------------------------------------------- a monitor

struct Frame {
    w2c: Matrix4<f64>,
    uv: Vec<Uv>,
    px: Vec<[f64; 2]>,
}

struct Monitor<'a> {
    frames: &'a [Frame],
    k: K,
}

impl Monitor<'_> {
    fn residuals(&self, p: &DVector<f64>, axis: &str, r: f64) -> DVector<f64> {
        let (rot, t) = (rotmat(&Vector3::new(p[0], p[1], p[2])), Vector3::new(p[3], p[4], p[5]));
        let n: usize = self.frames.iter().map(|f| f.uv.len()).sum();
        let mut out = Vec::with_capacity(2 * n);
        for f in self.frames {
            let r3 = f.w2c.fixed_view::<3, 3>(0, 0).into_owned();
            let t3 = f.w2c.fixed_view::<3, 1>(0, 3).into_owned();
            for (s, q) in surface(&f.uv, axis, r).iter().zip(&f.px) {
                let cam = r3 * (rot * s + t) + t3;
                let pr = project(self.k, &cam);
                out.push(pr[0] - q[0]);
                out.push(pr[1] - q[1]);
            }
        }
        DVector::from_vec(out)
    }

    fn fit(&self, axis: &str, r: f64, p0: &DVector<f64>) -> (DVector<f64>, f64) {
        let (x, f) = least_squares(&|p| self.residuals(p, axis, r), p0.clone(), 2.0);
        let e: Vec<f64> = f.as_slice().chunks(2).map(|v| (v[0] * v[0] + v[1] * v[1]).sqrt()).collect();
        let cap = percentile(&e, 90.0);
        (x, (e.iter().map(|v| v.min(cap).powi(2)).sum::<f64>() / e.len() as f64).sqrt())
    }

    /// Monitor -> world from the frame with the most corners, using PnP on the flat model.
    fn initial(&self) -> DVector<f64> {
        let f = self.frames.iter().rev().max_by_key(|f| f.uv.len()).unwrap();
        let (rv, tv) = pnp(&surface(&f.uv, "flat", 0.0), &f.px, self.k).unwrap_or((Vector3::zeros(), Vector3::new(0.0, 0.0, 1.0)));
        let mut m2c = Matrix4::identity();
        m2c.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&rv));
        m2c.fixed_view_mut::<3, 1>(0, 3).copy_from(&tv);
        let m2w = f.w2c.try_inverse().unwrap_or(Matrix4::identity()) * m2c;
        let rw = rotvec(&m2w.fixed_view::<3, 3>(0, 0).into_owned());
        DVector::from_vec(vec![rw.x, rw.y, rw.z, m2w[(0, 3)], m2w[(1, 3)], m2w[(2, 3)]])
    }

    fn best_radius(&self, axis: &str, p0: &DVector<f64>) -> (f64, DVector<f64>, f64) {
        let grid: Vec<f64> = (0..25).map(|i| 0.4 * (20.0f64 / 0.4).powf(i as f64 / 24.0)).collect();
        let fits: Vec<(f64, DVector<f64>, f64)> = grid.iter().map(|&r| {
            let (p, e) = self.fit(axis, r, p0);
            (r, p, e)
        }).collect();
        let i = (0..fits.len()).min_by(|&a, &b| fits[a].2.partial_cmp(&fits[b].2).unwrap()).unwrap();
        let (mut lo, mut hi) = (grid[i.saturating_sub(1)], grid[(i + 1).min(grid.len() - 1)]);
        let g = (5f64.sqrt() - 1.0) / 2.0;
        let p = fits[i].1.clone();
        for _ in 0..25 {
            let (a, b) = (hi - g * (hi - lo), lo + g * (hi - lo));
            if self.fit(axis, a, &p).1 < self.fit(axis, b, &p).1 {
                hi = b;
            } else {
                lo = a;
            }
        }
        let r = (lo + hi) / 2.0;
        let (pp, e) = self.fit(axis, r, &p);
        (r, pp, e)
    }
}

// ---------------------------------------------------------------- the head track

type Track = (Vec<f64>, Vec<Matrix4<f64>>);

fn load_track(v: &Value) -> Option<Track> {
    let a = v.as_array()?;
    if a.len() < 2 {
        return None;
    }
    let mut ts = vec![];
    let mut ps = vec![];
    for row in a {
        let r: Vec<f64> = row.as_array()?.iter().filter_map(Value::as_f64).collect();
        if r.len() != 13 {
            return None;
        }
        ts.push(r[0]);
        let mut m = Matrix4::identity();
        for i in 0..3 {
            for j in 0..4 {
                m[(i, j)] = r[1 + 4 * i + j];
            }
        }
        ps.push(m);
    }
    Some((ts, ps))
}

/// The head's pose at time t, between the two polls around it. It's None outside them.
fn pose_at(track: &Track, t: f64) -> Option<Matrix4<f64>> {
    let (ts, ps) = track;
    let i = ts.partition_point(|&x| x < t);
    if i == 0 || i >= ts.len() {
        return None;
    }
    let f = (t - ts[i - 1]) / (ts[i] - ts[i - 1]).max(1e-6);
    let (a, b) = (&ps[i - 1], &ps[i]);
    let m = a.fixed_view::<3, 3>(0, 0) * (1.0 - f) + b.fixed_view::<3, 3>(0, 0) * f;
    let svd = m.svd(true, true);
    let r = svd.u? * svd.v_t?;
    let mut out = Matrix4::identity();
    out.fixed_view_mut::<3, 3>(0, 0).copy_from(&r);
    for k in 0..3 {
        out[(k, 3)] = a[(k, 3)] * (1.0 - f) + b[(k, 3)] * f;
    }
    Some(out)
}

// ---------------------------------------------------------------- main

fn read_json(path: &Value) -> Value {
    path.as_str().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

fn shot_still(s: &Value) -> bool {
    s.get("still").and_then(Value::as_bool).unwrap_or_else(|| s["head_moved"].as_f64().unwrap_or(1.0) < 0.002)
}

fn corners(t: &Value) -> Vec<[f64; 2]> {
    t["corners"].as_array().map(|c| c.iter().map(|p| [p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(0.0)]).collect()).unwrap_or_default()
}

fn fracs(v: &Value) -> Vec<Vec<f64>> {
    v.as_array().map(|c| c.iter().map(|p| vec![p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(0.0)]).collect()).unwrap_or_default()
}

/// solve.py's main on a parsed job. It emits the output lines: "#..." and one JSON line per monitor.
pub fn solve(job: &Value, out: &mut dyn FnMut(String)) -> Result<(), String> {
    let still = |v: Value| -> Vec<Value> { v.as_array().cloned().unwrap_or_default().into_iter().filter(shot_still).collect() };
    let vis = still(read_json(&job["visible"]));
    let mut hid = still(read_json(&job["hidden"]));
    let monitors: Vec<Value> = job["monitors"].as_array().cloned().unwrap_or_default();
    // tag id -> (monitor index, its four corners as fractions).
    let mut by_id: HashMap<i64, (usize, Vec<Vec<f64>>)> = HashMap::new();
    for (mi, m) in monitors.iter().enumerate() {
        for (tid, fr) in m["tags"].as_object().into_iter().flatten() {
            if let Ok(id) = tid.parse::<i64>() {
                by_id.insert(id, (mi, fracs(fr)));
            }
        }
    }
    let mon_of = |t: &Value| t["id"].as_i64().and_then(|i| by_id.get(&i)).map(|x| x.0);

    // 1. The mirror camera.
    let (mut heads, mut world, mut pixels) = (vec![], vec![], vec![]);
    for shot in &vis {
        for t in shot["tags"].as_array().into_iter().flatten() {
            let Some((mi, fr)) = t["id"].as_i64().and_then(|i| by_id.get(&i)) else { continue };
            let m = &monitors[*mi];
            let g = &job["screens"][m["screen"].to_string().trim_matches('"')];
            let v3 = |q: &str| Vector3::new(g[q][0].as_f64().unwrap_or(0.0), g[q][1].as_f64().unwrap_or(0.0), g[q][2].as_f64().unwrap_or(0.0));
            let (c, x, y, z) = (v3("center"), v3("x"), v3("y"), v3("z"));
            let size = (g["metres"].as_f64().unwrap_or(0.0), g["height"].as_f64().unwrap_or(0.0));
            let uv: Vec<Uv> = fr.iter().map(|f| frac_to_uv(f, size)).collect();
            let curve = g["curve"].as_f64().unwrap_or(0.0);
            for (p, px) in surface(&uv, if curve > 0.0 { "h" } else { "flat" }, curve).iter().zip(corners(t)) {
                heads.push(mat34(&shot["head"]));
                world.push(c + x * p.x + y * p.y + z * p.z);
                pixels.push(px);
            }
        }
    }
    let mut camera: Option<Vec<f64>> = job["camera"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect());
    let mut cam_note = "the last good one";
    if pixels.len() >= 40 {
        let (p, med, p90) = fit_camera(&heads, &world, &pixels);
        let off: Vec<f64> = p[7..10].iter().map(|v| round(v * 1000.0, 1)).collect();
        out(format!("# mirror camera from {} virtual tags: f {:.0}/{:.0}, centre {:.0},{:.0}, eye offset {:?} mm, tilt {:.2} deg; median {med:.2} px, p90 {p90:.2} px",
            pixels.len() / 4, p[0], p[1], p[2], p[3], off, Vector3::new(p[4], p[5], p[6]).norm().to_degrees(), ));
        if med < 2.0 && (camera.is_none() || job["refit_camera"].as_bool().unwrap_or(false)) {
            out(format!("#camera {}", json!(p)));
            camera = Some(p);
            cam_note = "fitted now";
        }
    }
    let Some(cam) = camera.filter(|c| c.len() == 10) else {
        return Err("no mirror camera fit (~/.config/control-center/mirror-camera.json): run cc-home refit".into());
    };
    out(format!("# using the mirror camera {cam_note}"));
    let k = K { fx: cam[0], fy: cam[1], cx: cam[2], cy: cam[3] };
    let eye = eye_of(&cam);
    let mm = |m: &Value| (m["mm"][0].as_f64().unwrap_or(0.0) / 1000.0, m["mm"][1].as_f64().unwrap_or(0.0) / 1000.0);
    let name = |mi: usize| monitors[mi]["name"].as_str().unwrap_or("").to_owned();
    let mine = |shot: &Value, mi: usize| -> bool {
        let own = shot.get("monitor").and_then(Value::as_str).is_none_or(|n| n == name(mi));
        own && shot["tags"].as_array().into_iter().flatten().filter(|t| mon_of(t) == Some(mi)).count() >= MIN_TAGS
    };

    // Misreads: locate each reading on its own. Keep the readings that agree with their tag's median
    // place, and the tags whose median distance agrees with the monitor's other tags.
    let place_of = |t: &Value, shot: &Value, mi: usize| -> Option<(Vector3<f64>, f64)> {
        let fr = &by_id.get(&t["id"].as_i64()?)?.1;
        let uv: Vec<Uv> = fr.iter().map(|f| frac_to_uv(f, mm(&monitors[mi]))).collect();
        let a = (uv[1][0] - uv[0][0]) / 2.0;
        let obj = [Vector3::new(-a, a, 0.0), Vector3::new(a, a, 0.0), Vector3::new(a, -a, 0.0), Vector3::new(-a, -a, 0.0)];
        let (_, tv) = pnp(&obj, &corners(t), k)?;
        let cv = Vector3::new(tv.x, -tv.y, -tv.z); // OpenCV -> OpenVR axes.
        Some((apply(&(mat34(&shot["head"]) * eye), &cv), tv.norm()))
    };
    let mut readings: BTreeMap<i64, Vec<(usize, Vector3<f64>, f64)>> = BTreeMap::new();
    for (si, shot) in hid.iter().enumerate() {
        for t in shot["tags"].as_array().into_iter().flatten() {
            let Some(mi) = mon_of(t) else { continue };
            if t["side_px"].as_f64().unwrap_or(0.0) >= MIN_CHECK_PX && mine(shot, mi)
                && let Some((w, d)) = place_of(t, shot, mi)
            {
                readings.entry(t["id"].as_i64().unwrap()).or_default().push((si, w, d));
            }
        }
    }
    let mut good: HashSet<(usize, i64)> = HashSet::new();
    for mi in 0..monitors.len() {
        let seen: Vec<(&i64, &Vec<(usize, Vector3<f64>, f64)>)> = readings.iter().filter(|(tid, _)| by_id[tid].0 == mi).collect();
        if seen.is_empty() {
            continue;
        }
        let medians: Vec<(Vector3<f64>, f64)> = seen.iter().map(|(_, r)| {
            let comp = |i: usize| median(&r.iter().map(|x| x.1[i]).collect::<Vec<_>>());
            (Vector3::new(comp(0), comp(1), comp(2)), median(&r.iter().map(|x| x.2).collect::<Vec<_>>()))
        }).collect();
        let dist = median(&medians.iter().map(|m| m.1).collect::<Vec<_>>());
        for ((tid, r), (mw, md)) in seen.iter().zip(&medians) {
            if (md - dist).abs() > 0.25 * dist {
                out(format!("# {}: tag {tid} dropped as a misread ({md:.2} m away, others {dist:.2} m)", name(mi)));
                continue;
            }
            for (si, w, _) in r.iter() {
                if (w - mw).norm() < 0.03 {
                    good.insert((*si, **tid));
                }
            }
        }
    }
    for (si, shot) in hid.iter_mut().enumerate() {
        let kept: Vec<Value> = shot["tags"].as_array().into_iter().flatten().filter(|t| t["id"].as_i64().is_some_and(|id| good.contains(&(si, id)))).cloned().collect();
        shot["tags"] = Value::Array(kept);
    }

    // 2. Each real monitor.
    let track = load_track(&read_json(&job["head"]));
    let frames_for = |mi: usize, lag: Option<f64>| -> (Vec<Frame>, HashSet<i64>) {
        let size = mm(&monitors[mi]);
        let (mut frames, mut ids) = (vec![], HashSet::new());
        for shot in &hid {
            let own: Vec<&Value> = if mine(shot, mi) { shot["tags"].as_array().into_iter().flatten().filter(|t| mon_of(t) == Some(mi)).collect() } else { vec![] };
            if own.is_empty() {
                continue;
            }
            let at = match (lag, &track, shot["taken"].as_f64()) {
                (Some(l), Some(tr), Some(taken)) => pose_at(tr, taken - l),
                _ => None,
            };
            let head = at.unwrap_or_else(|| mat34(&shot["head"]));
            let w2c = flip() * (head * eye).try_inverse().unwrap_or(Matrix4::identity());
            let uv = own.iter().flat_map(|t| by_id[&t["id"].as_i64().unwrap()].1.iter().map(|f| frac_to_uv(f, size)).collect::<Vec<_>>()).collect();
            let px = own.iter().flat_map(|t| corners(t)).collect();
            frames.push(Frame { w2c, uv, px });
            ids.extend(own.iter().filter_map(|t| t["id"].as_i64()));
        }
        (frames, ids)
    };
    let real: Vec<usize> = (0..monitors.len()).filter(|&mi| !monitors[mi]["virtual"].as_bool().unwrap_or(false)).collect();
    let known_shape = |mi: usize| -> (String, f64) {
        let m = &monitors[mi];
        match (m["curve"].as_str(), m["radius"].as_f64()) {
            (Some(c @ ("h" | "v")), Some(r)) if r != 0.0 => (c.to_owned(), r),
            _ => ("flat".to_owned(), 0.0),
        }
    };
    let mut lag = None;
    if track.is_some() && hid.iter().any(|s| s.get("taken").is_some()) {
        let lag_error = |l: f64| -> f64 {
            let mut total = 0.0;
            for &mi in &real {
                let (fr, ids) = frames_for(mi, Some(l));
                if ids.len() >= 3 {
                    let mon = Monitor { frames: &fr, k };
                    let (axis, r) = known_shape(mi);
                    total += mon.fit(&axis, r, &mon.initial()).1;
                }
            }
            total
        };
        let steps: Vec<f64> = (0..).map(|i| -0.06 + 0.02 * i as f64).take_while(|&l| l < 0.205).collect();
        let coarse = steps.iter().map(|&l| (lag_error(l), l)).min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap().1;
        let l = [coarse - 0.01, coarse, coarse + 0.01].iter().map(|&l| (lag_error(l), l)).min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap().1;
        let used = hid.iter().rev().find_map(|s| s["lag"].as_f64());
        out(format!("#lag {l:.3}"));
        out(match used {
            Some(u) => format!("# mirror lag from the fit: {:.0} ms (scan.py measured {:.0} ms)", l * 1000.0, u * 1000.0),
            None => format!("# mirror lag from the fit: {:.0} ms", l * 1000.0),
        });
        lag = Some(l);
    }
    for &mi in &real {
        let m = &monitors[mi];
        let size = mm(m);
        let (mut frames, ids) = frames_for(mi, lag);
        if ids.len() < 3 {
            out(format!("# {}: only {} tag position(s) seen through passthrough, need 3+", name(mi), ids.len()));
            continue;
        }
        let mon = Monitor { frames: &frames, k };
        let p0 = mon.initial();
        let (flat_p, flat_err) = mon.fit("flat", 0.0, &p0);
        let known = m["curve"].as_str();
        let pinned = if matches!(known, Some("h" | "v")) { m["radius"].as_f64().filter(|r| *r != 0.0) } else { None };
        let axes: Vec<&str> = if pinned.is_some() || known == Some("flat") { vec![] } else if let Some(a @ ("h" | "v")) = known { vec![a] } else { vec!["h", "v"] };
        let mut fits: Vec<(String, f64, DVector<f64>, f64)> = vec![("flat".into(), 0.0, flat_p.clone(), flat_err)];
        for a in axes {
            let (r, p, e) = mon.best_radius(a, &flat_p);
            fits.push((a.into(), r, p, e));
        }
        let (axis, r, mut p, mut err) = if let Some(rad) = pinned {
            let a = known.unwrap().to_owned();
            let (p, e) = mon.fit(&a, rad, &flat_p);
            fits.push((a.clone(), rad, p.clone(), e));
            (a, rad, p, e)
        } else if let Some(kn @ ("h" | "v" | "flat")) = known {
            fits.iter().find(|f| f.0 == kn).cloned().unwrap()
        } else {
            let best = fits.iter().min_by(|a, b| a.3.partial_cmp(&b.3).unwrap()).cloned().unwrap();
            if best.0 != "flat" && best.3 > 0.8 * flat_err { fits[0].clone() } else { best }
        };
        // Drop frames that are far off the rest, then fit again.
        let per: Vec<f64> = frames.iter().map(|f| {
            let one = Monitor { frames: std::slice::from_ref(f), k };
            let res = one.residuals(&p, &axis, r);
            (res.iter().map(|v| v * v).sum::<f64>() / res.len() as f64).sqrt()
        }).collect();
        let med = median(&per);
        let keep: Vec<usize> = (0..frames.len()).filter(|&i| per[i] <= 2.0 * med).collect();
        if keep.len() < frames.len() && keep.len() >= 5 {
            let dropped = frames.len() - keep.len();
            let kept: Vec<Frame> = keep.iter().map(|&i| Frame { w2c: frames[i].w2c, uv: frames[i].uv.clone(), px: frames[i].px.clone() }).collect();
            frames = kept;
            let mon = Monitor { frames: &frames, k };
            (p, err) = mon.fit(&axis, r, &p);
            out(format!("# {}: {dropped} frame(s) over twice the median error dropped", name(mi)));
        }
        let rot = rotmat(&Vector3::new(p[0], p[1], p[2]));
        let centre = Vector3::new(p[3], p[4], p[5]);
        let dist = frames.iter().map(|f| (f.w2c.try_inverse().unwrap_or(Matrix4::identity()).fixed_view::<3, 1>(0, 3) - centre).norm()).fold(f64::INFINITY, f64::min);
        let fits_s: Vec<String> = fits.iter().map(|(a, rr, _, e)| format!("{a}{} {e:.2}", if a != "flat" { format!(" R {rr:.2} m") } else { String::new() })).collect();
        out(format!("# {}: {} tag positions in {} frames, {dist:.2} m away; rms px {} -> {axis}{}{}", name(mi), ids.len(), frames.len(), fits_s.join(", "),
            if r != 0.0 { format!(" R {r:.2} m") } else { String::new() },
            if pinned.is_some() { " (radius pinned by viewers.conf)" } else if known.is_some() { " (as viewers.conf says)" } else { "" }));
        let col = |j: usize| -> Vec<f64> { (0..3).map(|i| round(rot[(i, j)], 5)).collect() };
        out(json!({"name": m["name"], "screen": m["screen"], "centre": (0..3).map(|i| round(centre[i], 4)).collect::<Vec<_>>(),
                   "x": col(0), "y": col(1), "z": col(2), "width": size.0, "height": size.1, "axis": axis, "radius": round(r, 3),
                   "rms": round(err, 2), "rms_mm": round(err * dist / k.fx * 1000.0, 1), "distance": round(dist, 3)}).to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same as solve.py's selftest. A 1000R ultrawide (and the same one turned portrait, curved
    /// top-bottom) is seen from several head poses with 0.3 px noise. It has to come out curved on the
    /// right axis, with the radius within 10% and the centre within 5 mm.
    #[test]
    fn recovers_a_curved_monitor() {
        for axis in ["h", "v"] {
            let k = K { fx: 1070.0, fy: 1070.0, cx: 960.0, cy: 540.0 };
            let true_r = 1.0;
            let mut m2w = Matrix4::identity();
            m2w.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&Vector3::new(0.05, 0.3, 0.02)));
            m2w.fixed_view_mut::<3, 1>(0, 3).copy_from(&Vector3::new(-0.3, 1.75, -0.8));
            let side = 0.18;
            let (mut s0, mut s1) = (vec![0.08, 0.0, -0.08], vec![-0.45, 0.0, 0.45]);
            if axis == "v" {
                std::mem::swap(&mut s0, &mut s1);
            }
            let mut seed = 7u64;
            let mut noise = || {
                // Fixed pseudo-random, roughly normal noise, 0.3 px.
                let mut s = 0.0;
                for _ in 0..12 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    s += (seed >> 11) as f64 / (1u64 << 53) as f64;
                }
                (s - 6.0) * 0.3
            };
            let mut frames = vec![];
            let mut j = 0;
            for &cy in &s0 {
                for &cx in &s1 {
                    let uv: Vec<Uv> = vec![[cx - side / 2.0, cy + side / 2.0], [cx + side / 2.0, cy + side / 2.0], [cx + side / 2.0, cy - side / 2.0], [cx - side / 2.0, cy - side / 2.0]];
                    let mut head = Matrix4::identity();
                    head.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&Vector3::new(0.02 * ((j % 3) as f64 - 1.0), 0.31 + 0.01 * j as f64, 0.0)));
                    head.fixed_view_mut::<3, 1>(0, 3).copy_from(&Vector3::new(0.01 * j as f64, 1.7, 0.0));
                    let w2c = flip() * head.try_inverse().unwrap();
                    let px = surface(&uv, axis, true_r).iter().map(|s| {
                        let c = apply(&w2c, &apply(&m2w, s));
                        let p = project(k, &c);
                        [p[0] + noise(), p[1] + noise()]
                    }).collect();
                    frames.push(Frame { w2c, uv, px });
                    j += 1;
                }
            }
            let mon = Monitor { frames: &frames, k };
            let (flat_p, flat_err) = mon.fit("flat", 0.0, &mon.initial());
            let (r, p, err) = mon.best_radius(axis, &flat_p);
            let centre_err = (Vector3::new(p[3], p[4], p[5]) - m2w.fixed_view::<3, 1>(0, 3)).norm();
            assert!(err < 0.8 * flat_err && (r - true_r).abs() < 0.1 * true_r && centre_err < 0.005, "{axis}: flat {flat_err} curved R {r} rms {err} centre off {centre_err}");
        }
    }
    /// How well a few seconds of one monitor pins down where it is, for entering a workspace in a new
    /// room (cc-home workspace enter): one tag, the four corner tags, and the full align's dense grid,
    /// on the same simulated 600 x 340 mm monitor 0.7 m away. 12 frames over 2 cm of head travel, 0.5 px
    /// of corner noise and 1 mm / 0.1 deg of head tracking noise per frame. What matters is the far end
    /// of the desk: where a point 1 m to the side of the monitor lands (the error a spot there gets).
    /// One tag's square is too small a lever for yaw, so it's out; the corners are close to the grid.
    #[test]
    fn quick_corners_pin_the_desk() {
        let k = K { fx: 1070.0, fy: 1070.0, cx: 960.0, cy: 540.0 };
        let (w, h) = (0.6, 0.34);
        let lay = |l: crate::pattern::Layout| -> Vec<Vec<Uv>> {
            l.json["tags"].as_object().unwrap().values().map(|c| fracs(c).iter().map(|f| frac_to_uv(f, (w, h))).collect()).collect()
        };
        let corners: Vec<Vec<Uv>> = lay(crate::pattern::frame(1920, 1080, 0)).into_iter().take(4).collect();
        let one = vec![corners[0].clone()];
        let grid = lay(crate::pattern::dense(1920, 1080, 0, 150));
        let mut seed = 11u64;
        let mut noise = |sd: f64| {
            let mut s = 0.0;
            for _ in 0..12 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                s += (seed >> 11) as f64 / (1u64 << 53) as f64;
            }
            (s - 6.0) * sd
        };
        let mut worst = |tags: &Vec<Vec<Uv>>| -> f64 {
            let mut worst: f64 = 0.0;
            for trial in 0..8 {
                let mut m2w = Matrix4::identity();
                m2w.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&Vector3::new(-0.08, 0.25 + 0.05 * trial as f64, 0.0)));
                m2w.fixed_view_mut::<3, 1>(0, 3).copy_from(&Vector3::new(-0.2, 1.2, -0.7));
                let mut frames = vec![];
                for j in 0..12 {
                    let mut head = Matrix4::identity();
                    head.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&Vector3::new(-0.1, 0.2, 0.0)));
                    head.fixed_view_mut::<3, 1>(0, 3).copy_from(&Vector3::new(0.02 * j as f64 / 11.0, 1.5, 0.0));
                    let seen = flip() * head.try_inverse().unwrap();
                    // what tracking says the head was (off by a little), which the fit has to use
                    let mut told = head;
                    let jitter = rotmat(&Vector3::new(noise(0.1f64.to_radians()), noise(0.1f64.to_radians()), noise(0.1f64.to_radians())));
                    told.fixed_view_mut::<3, 3>(0, 0).copy_from(&(jitter * head.fixed_view::<3, 3>(0, 0)));
                    for i in 0..3 {
                        told[(i, 3)] += noise(0.001);
                    }
                    let (mut uv, mut px) = (vec![], vec![]);
                    for t in tags {
                        for q in t {
                            let p = project(k, &apply(&seen, &apply(&m2w, &Vector3::new(q[0], q[1], 0.0))));
                            uv.push(*q);
                            px.push([p[0] + noise(0.5), p[1] + noise(0.5)]);
                        }
                    }
                    frames.push(Frame { w2c: flip() * told.try_inverse().unwrap(), uv, px });
                }
                let mon = Monitor { frames: &frames, k };
                let (p, _) = mon.fit("flat", 0.0, &mon.initial());
                let mut fit = Matrix4::identity();
                fit.fixed_view_mut::<3, 3>(0, 0).copy_from(&rotmat(&Vector3::new(p[0], p[1], p[2])));
                fit.fixed_view_mut::<3, 1>(0, 3).copy_from(&Vector3::new(p[3], p[4], p[5]));
                let far = Vector3::new(1.0, 0.0, 0.0);
                worst = worst.max((apply(&fit, &far) - apply(&m2w, &far)).norm() * 1000.0);
            }
            worst
        };
        let (one, corners, grid) = (worst(&one), worst(&corners), worst(&grid));
        eprintln!("1 m from the monitor, worst of 8: one tag {one:.1} mm, four corner tags {corners:.1} mm, the full grid {grid:.1} mm");
        assert!(corners < 10.0 && corners < one / 2.0, "one {one:.1} corners {corners:.1} grid {grid:.1}");
    }
}
