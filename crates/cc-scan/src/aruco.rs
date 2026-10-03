//! ArUco 4x4 detection, ported from OpenCV 4.13's ArucoDetector (aruco_detector.cpp) with the
//! parameters scan.py sets. Candidates come from the adaptive threshold at several window sizes.
//! They get filtered, read through a perspective warp and Otsu, and matched against the
//! dictionary, and then the corners are refined with cornerSubPix. scan.py's detect() runs it on
//! CLAHE(Gaussian(grey)).
use crate::contours;
use crate::dict::Dict;
use crate::image::{Gray, otsu, threshold_inv};

pub type Pt = (f32, f32);

#[derive(Clone, Debug)]
pub struct Params {
    pub win_min: usize,
    pub win_max: usize,
    pub win_step: usize,
    pub thresh_c: f64,
    pub min_perimeter_rate: f64,
    pub max_perimeter_rate: f64,
    pub poly_accuracy_rate: f64,
    pub min_corner_distance_rate: f64,
    pub min_distance_to_border: f32,
    pub min_marker_distance_rate: f32,
    pub min_group_distance: f32,
    pub pixel_per_cell: usize,
    pub ignored_margin_per_cell: f64,
    pub max_erroneous_border_rate: f64,
    pub min_otsu_std_dev: f64,
    pub error_correction_rate: f64,
    pub subpix: bool,
    pub refine_win: i32,
    pub relative_refine_win: f32,
    pub refine_iters: usize,
    pub refine_accuracy: f64,
}

impl Default for Params {
    /// The same defaults as OpenCV's DetectorParameters().
    fn default() -> Params {
        Params {
            win_min: 3,
            win_max: 23,
            win_step: 10,
            thresh_c: 7.0,
            min_perimeter_rate: 0.03,
            max_perimeter_rate: 4.0,
            poly_accuracy_rate: 0.03,
            min_corner_distance_rate: 0.05,
            min_distance_to_border: 3.0,
            min_marker_distance_rate: 0.125,
            min_group_distance: 0.21,
            pixel_per_cell: 4,
            ignored_margin_per_cell: 0.13,
            max_erroneous_border_rate: 0.35,
            min_otsu_std_dev: 5.0,
            error_correction_rate: 0.6,
            subpix: false,
            refine_win: 5,
            relative_refine_win: 0.3,
            refine_iters: 30,
            refine_accuracy: 0.1,
        }
    }
}

impl Params {
    /// scan.py's PARAMS. Passthrough is soft and low contrast, so bit reading is more forgiving and the
    /// threshold search is wider. Corners get refined.
    pub fn passthrough() -> Params {
        Params {
            subpix: true,
            error_correction_rate: 1.0,
            ignored_margin_per_cell: 0.3,
            pixel_per_cell: 10,
            win_max: 63,
            win_step: 6,
            ..Params::default()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Marker {
    pub id: usize,
    /// TL, TR, BR, BL, in the order OpenCV reports them.
    pub corners: [Pt; 4],
}

// ---------------------------------------------------------------- candidates

/// Port of approxPolyDP(closed): OpenCV's Ramer-Douglas-Peucker, including its start-point search and final clean-up.
fn approx_poly_dp(src: &[(i32, i32)], eps: f64) -> Vec<(i32, i32)> {
    let count = src.len();
    if count == 0 {
        return vec![];
    }
    let eps = eps * eps;
    let mut dst: Vec<(i32, i32)> = Vec::with_capacity(count);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0usize;
    let mut right_start = 0usize;
    let mut start_pt = (0, 0);
    let mut le_eps = false;
    let read = |pos: &mut usize| {
        let p = src[*pos];
        *pos += 1;
        if *pos >= count {
            *pos = 0;
        }
        p
    };
    for _ in 0..3 {
        let mut max_dist = 0.0f64;
        pos = (pos + right_start) % count;
        start_pt = read(&mut pos);
        for j in 1..count {
            let pt = read(&mut pos);
            let (dx, dy) = ((pt.0 - start_pt.0) as f64, (pt.1 - start_pt.1) as f64);
            let d = dx * dx + dy * dy;
            if d > max_dist {
                max_dist = d;
                right_start = j;
            }
        }
        le_eps = max_dist <= eps;
    }
    if !le_eps {
        let slice_start = pos % count;
        let right_end = slice_start;
        let slice_end = (right_start + slice_start) % count;
        right_start = slice_end;
        stack.push((right_start, right_end));
        stack.push((slice_start, slice_end));
    } else {
        dst.push(start_pt);
    }
    while let Some((s_start, s_end)) = stack.pop() {
        let end_pt = src[s_end];
        pos = s_start;
        start_pt = read(&mut pos);
        let mut split = 0usize;
        if pos != s_end {
            let (dx, dy) = ((end_pt.0 - start_pt.0) as f64, (end_pt.1 - start_pt.1) as f64);
            let seg = dx * dx + dy * dy;
            let mut maxd = 0.0f64;
            while pos != s_end {
                let pt = read(&mut pos);
                let (px, py) = ((pt.0 - start_pt.0) as f64, (pt.1 - start_pt.1) as f64);
                let proj = px * dx + py * dy;
                let d = if proj < 0.0 {
                    (px * px + py * py) * seg
                } else if proj > seg {
                    let (ex, ey) = ((pt.0 - end_pt.0) as f64, (pt.1 - end_pt.1) as f64);
                    (ex * ex + ey * ey) * seg
                } else {
                    let c = py * dx - px * dy;
                    c * c
                };
                if d > maxd {
                    maxd = d;
                    split = (pos + count - 1) % count;
                }
            }
            le_eps = maxd <= eps * seg;
        } else {
            le_eps = true;
            start_pt = src[s_start];
        }
        if le_eps {
            dst.push(start_pt);
        } else {
            stack.push((split, s_end));
            stack.push((s_start, split));
        }
    }
    // Clean-up: drop points that sit on (almost) straight lines.
    let count = dst.len();
    let mut new_count = count;
    let mut pos = count - 1;
    let rd = |dst: &Vec<(i32, i32)>, pos: &mut usize| {
        let p = dst[*pos];
        *pos += 1;
        if *pos >= count {
            *pos = 0;
        }
        p
    };
    let mut start = rd(&dst, &mut pos);
    let mut wpos = pos;
    let mut pt = rd(&dst, &mut pos);
    let mut i = 0;
    while i < count && new_count > 2 {
        let end = rd(&dst, &mut pos);
        let (dx, dy) = ((end.0 - start.0) as f64, (end.1 - start.1) as f64);
        let dist = (((pt.0 - start.0) as f64) * dy - ((pt.1 - start.1) as f64) * dx).abs();
        let inner = ((pt.0 - start.0) as f64) * ((end.0 - pt.0) as f64) + ((pt.1 - start.1) as f64) * ((end.1 - pt.1) as f64);
        if dist * dist <= 0.5 * eps * (dx * dx + dy * dy) && dx != 0.0 && dy != 0.0 && inner >= 0.0 {
            new_count -= 1;
            start = end;
            dst[wpos] = end;
            wpos += 1;
            if wpos >= count {
                wpos = 0;
            }
            pt = rd(&dst, &mut pos);
            i += 2;
            continue;
        }
        start = pt;
        dst[wpos] = pt;
        wpos += 1;
        if wpos >= count {
            wpos = 0;
        }
        pt = end;
        i += 1;
    }
    dst.truncate(new_count);
    dst
}

fn is_convex(p: &[(i32, i32)]) -> bool {
    let n = p.len();
    let mut sign = 0i64;
    for i in 0..n {
        let (a, b, c) = (p[i], p[(i + 1) % n], p[(i + 2) % n]);
        let cross = (b.0 - a.0) as i64 * (c.1 - b.1) as i64 - (b.1 - a.1) as i64 * (c.0 - b.0) as i64;
        if cross != 0 {
            if sign != 0 && (cross > 0) != (sign > 0) {
                return false;
            }
            sign = cross;
        }
    }
    true
}

struct Cand {
    corners: [Pt; 4],
    contour_len: usize,
    perimeter: f32,
    parent: isize,
    depth: usize,
    close: Vec<[Pt; 4]>,
}

fn marker_contours(bin: &[u8], w: usize, h: usize, p: &Params) -> Vec<([Pt; 4], usize)> {
    let big = w.max(h) as f64;
    let (min_px, max_px) = ((p.min_perimeter_rate * big) as usize, (p.max_perimeter_rate * big) as usize);
    let mut out = Vec::new();
    for c in contours::find(bin, w, h) {
        if c.len() < min_px || c.len() > max_px {
            continue;
        }
        let a = approx_poly_dp(&c, c.len() as f64 * p.poly_accuracy_rate);
        if a.len() != 4 || !is_convex(&a) {
            continue;
        }
        let mut min_d = big * big;
        for j in 0..4 {
            let (dx, dy) = ((a[j].0 - a[(j + 1) % 4].0) as f64, (a[j].1 - a[(j + 1) % 4].1) as f64);
            min_d = min_d.min(dx * dx + dy * dy);
        }
        let min_corner = c.len() as f64 * p.min_corner_distance_rate;
        if min_d < min_corner * min_corner {
            continue;
        }
        let mut q = [(0.0, 0.0); 4];
        for j in 0..4 {
            q[j] = (a[j].0 as f32, a[j].1 as f32);
        }
        // Make it clockwise.
        let (dx1, dy1) = ((q[1].0 - q[0].0) as f64, (q[1].1 - q[0].1) as f64);
        let (dx2, dy2) = ((q[2].0 - q[0].0) as f64, (q[2].1 - q[0].1) as f64);
        if dx1 * dy2 - dy1 * dx2 < 0.0 {
            q.swap(1, 3);
        }
        out.push((q, c.len()));
    }
    out
}

fn dist2(a: Pt, b: Pt) -> f32 {
    (a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1)
}

fn perimeter(c: &[Pt; 4]) -> f32 {
    (0..4).map(|i| dist2(c[i], c[(i + 1) % 4]).sqrt()).sum()
}

fn average_distance(m1: &[Pt; 4], m2: &[Pt; 4]) -> f32 {
    let mut best = f32::MAX;
    for fc in 0..4 {
        let d = (0..4).map(|c| dist2(m1[(c + fc) % 4], m2[c])).sum::<f32>() / 4.0;
        best = best.min(d);
    }
    best.sqrt()
}

/// Same as pointPolygonTest(poly, p, false) >= 0, for a convex quad.
fn inside(poly: &[Pt; 4], p: Pt) -> bool {
    // OpenCV's crossing test. A point on the edge counts as inside.
    let mut result = false;
    let mut v0 = poly[3];
    for &v in poly.iter() {
        if (v0.1 <= p.1 && v.1 <= p.1) || (v0.1 > p.1 && v.1 > p.1) || (v0.0 < p.0 && v.0 < p.0) {
            if p.1 == v.1 && (p.0 == v.0 || (p.1 == v0.1 && ((v0.0 <= p.0 && p.0 <= v.0) || (v.0 <= p.0 && p.0 <= v0.0)))) {
                return true;
            }
            v0 = v;
            continue;
        }
        let dist = (p.1 - v0.1) as f64 * (v.0 - v0.0) as f64 - (p.0 - v0.0) as f64 * (v.1 - v0.1) as f64;
        if dist == 0.0 {
            return true;
        }
        if v.1 < v0.1 {
            if dist < 0.0 {
                result = !result;
            }
        } else if dist > 0.0 {
            result = !result;
        }
        v0 = v;
    }
    result
}

fn filter_too_close(cands: Vec<([Pt; 4], usize)>, w: usize, h: usize, p: &Params, marker_size: usize) -> Vec<Cand> {
    let mut tree: Vec<Cand> = cands.into_iter().map(|(c, n)| Cand { perimeter: perimeter(&c), corners: c, contour_len: n, parent: -1, depth: 0, close: vec![] }).collect();
    tree.sort_by(|a, b| b.perimeter.partial_cmp(&a.perimeter).unwrap_or(std::cmp::Ordering::Equal)); // Stable sort, biggest first.
    let n = tree.len();
    let mut group_id = vec![-1isize; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut selected = vec![true; n];
    for i in 0..n {
        for j in i + 1..n {
            let d = average_distance(&tree[i].corners, &tree[j].corners);
            if d < tree[j].perimeter * p.min_marker_distance_rate {
                selected[i] = false;
                selected[j] = false;
                match (group_id[i], group_id[j]) {
                    (-1, -1) => {
                        group_id[i] = groups.len() as isize;
                        group_id[j] = groups.len() as isize;
                        groups.push(vec![i, j]);
                    }
                    (g, -1) => {
                        group_id[j] = g;
                        groups[g as usize].push(j);
                    }
                    (-1, g) => {
                        group_id[i] = g;
                        groups[g as usize].push(i);
                    }
                    _ => {}
                }
            }
        }
        if selected[i] {
            selected[i] = false;
            group_id[i] = groups.len() as isize;
            groups.push(vec![i]);
        }
    }
    let module = |c: &[Pt; 4]| perimeter(c) / (4.0 * (marker_size + 2) as f32);
    let b = p.min_distance_to_border;
    for g in groups.iter_mut() {
        g.sort(); // The largest (lowest index) goes first.
        let mut curr = g[0];
        if tree[curr].corners.iter().any(|c| c.0 < b || c.1 < b || c.0 > w as f32 - 1.0 - b || c.1 > h as f32 - 1.0 - b) {
            continue;
        }
        selected[curr] = true;
        for &id in &g[1..] {
            let d = average_distance(&tree[id].corners, &tree[curr].corners);
            if d > p.min_group_distance * module(&tree[id].corners) {
                curr = id;
                let c = tree[id].corners;
                tree[g[0]].close.push(c);
            }
        }
    }
    let mut out: Vec<Cand> = tree.into_iter().zip(selected).filter(|(_, s)| *s).map(|(c, _)| c).collect();
    for i in (0..out.len()).rev() {
        for j in (0..i).rev() {
            if out[i].corners.iter().all(|&c| inside(&out[j].corners, c)) {
                out[i].parent = j as isize;
                out[j].depth = out[j].depth.max(out[i].depth + 1);
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------- reading a candidate

/// Port of getPerspectiveTransform. Returns the 3x3 map from src[i] to dst[i].
pub fn perspective(src: &[Pt; 4], dst: &[Pt; 4]) -> [f64; 9] {
    let mut a = [[0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (src[i].0 as f64, src[i].1 as f64);
        let (u, v) = (dst[i].0 as f64, dst[i].1 as f64);
        a[i] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u, u];
        a[i + 4] = [0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v, v];
    }
    // Gaussian elimination with partial pivoting on the 8x8 system.
    for c in 0..8 {
        let piv = (c..8).max_by(|&i, &j| a[i][c].abs().partial_cmp(&a[j][c].abs()).unwrap()).unwrap();
        a.swap(c, piv);
        let d = a[c][c];
        if d.abs() < 1e-12 {
            return [0.0; 9];
        }
        for k in c..9 {
            a[c][k] /= d;
        }
        for r in 0..8 {
            if r != c {
                let f = a[r][c];
                for k in c..9 {
                    a[r][k] -= f * a[c][k];
                }
            }
        }
    }
    let mut m = [0f64; 9];
    for i in 0..8 {
        m[i] = a[i][8];
    }
    m[8] = 1.0;
    m
}

fn invert3(m: &[f64; 9]) -> [f64; 9] {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
    let d = if det != 0.0 { 1.0 / det } else { 0.0 };
    [
        (m[4] * m[8] - m[5] * m[7]) * d,
        (m[2] * m[7] - m[1] * m[8]) * d,
        (m[1] * m[5] - m[2] * m[4]) * d,
        (m[5] * m[6] - m[3] * m[8]) * d,
        (m[0] * m[8] - m[2] * m[6]) * d,
        (m[2] * m[3] - m[0] * m[5]) * d,
        (m[3] * m[7] - m[4] * m[6]) * d,
        (m[1] * m[6] - m[0] * m[7]) * d,
        (m[0] * m[4] - m[1] * m[3]) * d,
    ]
}

/// Port of _extractBits. Returns the candidate's cells (marker plus border), row-major, with 1 for white.
fn extract_bits(img: &Gray, corners: &[Pt; 4], marker_size: usize, p: &Params) -> Vec<u8> {
    let cells = marker_size + 2;
    let cell = p.pixel_per_cell;
    let size = cells * cell;
    let s = (size - 1) as f32;
    let m = perspective(corners, &[(0.0, 0.0), (s, 0.0), (s, s), (0.0, s)]);
    let inv = invert3(&m);
    // Same as warpPerspective(INTER_NEAREST, BORDER_CONSTANT 0).
    let mut warped = vec![0u8; size * size];
    for y in 0..size {
        for x in 0..size {
            let (xf, yf) = (x as f64, y as f64);
            let w = inv[6] * xf + inv[7] * yf + inv[8];
            let w = if w != 0.0 { 1.0 / w } else { 0.0 };
            let sx = ((inv[0] * xf + inv[1] * yf + inv[2]) * w).clamp(i32::MIN as f64, i32::MAX as f64).round_ties_even() as i64;
            let sy = ((inv[3] * xf + inv[4] * yf + inv[5]) * w).clamp(i32::MIN as f64, i32::MAX as f64).round_ties_even() as i64;
            if sx >= 0 && sy >= 0 && (sx as usize) < img.w && (sy as usize) < img.h {
                warped[y * size + x] = img.at(sx as usize, sy as usize);
            }
        }
    }
    let mut bits = vec![0u8; cells * cells];
    // Is there enough contrast for Otsu? Check the inner region, half a cell in from the edge.
    let (lo, hi) = (cell / 2, size - cell / 2);
    let inner: Vec<f64> = (lo..hi).flat_map(|y| (lo..hi).map(move |x| (x, y))).map(|(x, y)| warped[y * size + x] as f64).collect();
    let mean = inner.iter().sum::<f64>() / inner.len() as f64;
    let sd = (inner.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / inner.len() as f64).sqrt();
    if sd < p.min_otsu_std_dev {
        bits.fill((mean > 127.0) as u8);
        return bits;
    }
    let t = otsu(&warped);
    let margin = (p.ignored_margin_per_cell * cell as f64) as usize;
    let side = cell - 2 * margin;
    for y in 0..cells {
        for x in 0..cells {
            let (x0, y0) = (x * cell + margin, y * cell + margin);
            let white = (y0..y0 + side).flat_map(|yy| (x0..x0 + side).map(move |xx| (xx, yy))).filter(|&(xx, yy)| warped[yy * size + xx] > t).count();
            bits[y * cells + x] = (white > side * side / 2) as u8;
        }
    }
    bits
}

fn identify(img: &Gray, corners: &[Pt; 4], dict: &Dict, p: &Params) -> Option<(usize, usize)> {
    let n = 4;
    let cells = n + 2;
    let bits = extract_bits(img, corners, n, p);
    let mut border = 0;
    for y in 0..cells {
        border += bits[y * cells] as usize + bits[y * cells + cells - 1] as usize;
    }
    for x in 1..cells - 1 {
        border += bits[x] as usize + bits[(cells - 1) * cells + x] as usize;
    }
    if border > (n as f64 * n as f64 * p.max_erroneous_border_rate) as usize {
        return None;
    }
    let mut code = 0u16;
    for y in 1..=n {
        for x in 1..=n {
            code = (code << 1) | bits[y * cells + x] as u16;
        }
    }
    dict.identify(code)
}

// ---------------------------------------------------------------- refinement

/// getRectSubPix's bilinear sample, with border replicate.
fn sample(img: &Gray, x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let px = |xi: f32, yi: f32| img.at((xi as isize).clamp(0, img.w as isize - 1) as usize, (yi as isize).clamp(0, img.h as isize - 1) as usize) as f32;
    let (a, b, c, d) = (px(x0, y0), px(x0 + 1.0, y0), px(x0, y0 + 1.0), px(x0 + 1.0, y0 + 1.0));
    (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy
}

/// Port of cornerSubPix(win, zeroZone (-1,-1), MAX_ITER|EPS).
fn corner_subpix(img: &Gray, c: Pt, win: i32, iters: usize, eps: f64) -> Pt {
    let ww = (2 * win + 1) as usize;
    let mut mask = vec![0f32; ww * ww];
    for i in 0..ww {
        let y = (i as f32 - win as f32) / win as f32;
        let vy = (-y * y).exp();
        for j in 0..ww {
            let x = (j as f32 - win as f32) / win as f32;
            mask[i * ww + j] = vy * (-x * x).exp();
        }
    }
    let eps = eps * eps;
    let inside = |p: Pt| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < img.w as f32 && p.1 < img.h as f32;
    let (ct, mut ci) = (c, c);
    let sw = ww + 2;
    let mut buf = vec![0f32; sw * sw];
    for _ in 0..iters.clamp(1, 100) {
        // The (ww+2)^2 patch centred on ci.
        let (ox, oy) = (ci.0 - (sw as f32 - 1.0) * 0.5, ci.1 - (sw as f32 - 1.0) * 0.5);
        for i in 0..sw {
            for j in 0..sw {
                buf[i * sw + j] = sample(img, ox + j as f32, oy + i as f32);
            }
        }
        let (mut a, mut b, mut cc, mut bb1, mut bb2) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for i in 0..ww {
            let py = i as f64 - win as f64;
            for j in 0..ww {
                let m = mask[i * ww + j] as f64;
                let at = |di: isize, dj: isize| buf[((i as isize + 1 + di) as usize) * sw + (j as isize + 1 + dj) as usize] as f64;
                let tgx = at(0, 1) - at(0, -1);
                let tgy = at(1, 0) - at(-1, 0);
                let (gxx, gxy, gyy) = (tgx * tgx * m, tgx * tgy * m, tgy * tgy * m);
                let px = j as f64 - win as f64;
                a += gxx;
                b += gxy;
                cc += gyy;
                bb1 += gxx * px + gxy * py;
                bb2 += gxy * px + gyy * py;
            }
        }
        let det = a * cc - b * b;
        if det.abs() <= f64::EPSILON * f64::EPSILON {
            break;
        }
        let s = 1.0 / det;
        let n = ((ci.0 as f64 + cc * s * bb1 - b * s * bb2) as f32, (ci.1 as f64 - b * s * bb1 + a * s * bb2) as f32);
        let err = ((n.0 - ci.0) * (n.0 - ci.0) + (n.1 - ci.1) * (n.1 - ci.1)) as f64;
        if !inside(n) {
            break;
        }
        ci = n;
        if err <= eps {
            break;
        }
    }
    if (ci.0 - ct.0).abs() > win as f32 || (ci.1 - ct.1).abs() > win as f32 {
        ci = ct;
    }
    ci
}

// ---------------------------------------------------------------- the detector

/// Port of detectMarkers on a grey image. scan.py gives it CLAHE(Gaussian(grey)).
pub fn detect(img: &Gray, dict: &Dict, p: &Params) -> Vec<Marker> {
    let marker_size = 4;
    let dict = dict.with_rate(dict.max_correction, p.error_correction_rate);
    let scales: Vec<usize> = (0..=(p.win_max - p.win_min) / p.win_step).map(|i| p.win_min + i * p.win_step).collect();
    let per_scale: Vec<Vec<([Pt; 4], usize)>> = std::thread::scope(|s| {
        let hs: Vec<_> = scales.iter().map(|&win| s.spawn(move || marker_contours(&threshold_inv(img, win, p.thresh_c), img.w, img.h, p))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    let cands: Vec<([Pt; 4], usize)> = per_scale.into_iter().flatten().collect();
    let mut sel = filter_too_close(cands, img.w, img.h, p, marker_size);
    let n = sel.len();
    let max_depth = sel.iter().map(|c| c.depth).max().unwrap_or(0);
    let mut depths: Vec<Vec<usize>> = vec![vec![]; max_depth + 1];
    for (i, c) in sel.iter().enumerate() {
        depths[c.depth].push(i);
    }
    let mut found: Vec<Option<(usize, usize)>> = vec![None; n];
    let mut was = vec![false; n];
    let (mut counter, mut depth) = (0, 0);
    while counter < n && depth <= max_depth {
        for &v in &depths[depth] {
            was[v] = true;
            found[v] = identify(img, &sel[v].corners, &dict, p);
            if found[v].is_none() {
                for k in 0..sel[v].close.len() {
                    let c = sel[v].close[k];
                    if let Some(f) = identify(img, &c, &dict, p) {
                        found[v] = Some(f);
                        sel[v].corners = c;
                        break;
                    }
                }
            }
        }
        for &v in &depths[depth] {
            if found[v].is_some() {
                let mut parent = sel[v].parent;
                while parent != -1 {
                    if !was[parent as usize] {
                        was[parent as usize] = true;
                        counter += 1;
                    }
                    parent = sel[parent as usize].parent;
                }
            }
            counter += 1;
        }
        depth += 1;
    }
    let mut out = Vec::new();
    for (i, f) in found.into_iter().enumerate() {
        let Some((id, rot)) = f else { continue };
        let mut c = sel[i].corners;
        c.rotate_right(rot); // Same as std::rotate(begin, begin + 4 - rot, end).
        if p.subpix {
            let module = perimeter(&c) / (4.0 * (marker_size + 2) as f32);
            let win = ((p.relative_refine_win * module).round_ties_even() as i32).max(1).min(p.refine_win);
            for k in 0..4 {
                c[k] = corner_subpix(img, c[k], win, p.refine_iters, p.refine_accuracy);
            }
        }
        let _ = sel[i].contour_len;
        out.push(Marker { id, corners: c });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_finds_a_square() {
        let mut c = vec![];
        for x in 0..10 {
            c.push((x, 0));
        }
        for y in 0..10 {
            c.push((10, y));
        }
        for x in (1..=10).rev() {
            c.push((x, 10));
        }
        for y in (1..=10).rev() {
            c.push((0, y));
        }
        let a = approx_poly_dp(&c, c.len() as f64 * 0.03);
        assert_eq!(a.len(), 4, "{a:?}");
        assert!(is_convex(&a));
    }
}
