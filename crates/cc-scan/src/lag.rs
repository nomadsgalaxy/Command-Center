//! How far the mirror's frames lag the head pose, measured from the frames themselves, like
//! scan.py's Lag. It compares the image's shift between consecutive shots (phase correlation, a port
//! of OpenCV's phaseCorrelate) with the head's turn at each candidate lag, and picks the lag where
//! they agree best. Until there's enough motion it uses the guess. solve finds the lag again from
//! the fit anyway.
use crate::image::Gray;
use crate::panels::{HeadTrack, mat, rotvec};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use std::collections::VecDeque;

pub const SMALL: (usize, usize) = (480, 270);

/// Same as cv2.resize(grey, (480, 270)) for a 4x smaller frame, as floats. INTER_LINEAR samples
/// the middle two of each four.
pub fn shrink(g: &Gray) -> Vec<f32> {
    let (w, h) = SMALL;
    let (sx, sy) = (g.w as f64 / w as f64, g.h as f64 / h as f64);
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let fy = ((y as f64 + 0.5) * sy - 0.5).max(0.0);
        let (y0, wy) = (fy.floor() as usize, fy - fy.floor());
        let y1 = (y0 + 1).min(g.h - 1);
        for x in 0..w {
            let fx = ((x as f64 + 0.5) * sx - 0.5).max(0.0);
            let (x0, wx) = (fx.floor() as usize, fx - fx.floor());
            let x1 = (x0 + 1).min(g.w - 1);
            let p = |xx: usize, yy: usize| g.at(xx, yy) as f64;
            let v = (p(x0, y0) * (1.0 - wx) + p(x1, y0) * wx) * (1.0 - wy) + (p(x0, y1) * (1.0 - wx) + p(x1, y1) * wx) * wy;
            out[y * w + x] = v.round() as f32;
        }
    }
    out
}

fn fft2(data: &mut [Complex<f64>], w: usize, h: usize, inverse: bool) {
    let mut planner = FftPlanner::new();
    let (row, col) = if inverse { (planner.plan_fft_inverse(w), planner.plan_fft_inverse(h)) } else { (planner.plan_fft_forward(w), planner.plan_fft_forward(h)) };
    for r in data.chunks_exact_mut(w) {
        row.process(r);
    }
    let mut column = vec![Complex::new(0.0, 0.0); h];
    for x in 0..w {
        for y in 0..h {
            column[y] = data[y * w + x];
        }
        col.process(&mut column);
        for y in 0..h {
            data[y * w + x] = column[y];
        }
    }
}

/// Same as phaseCorrelate(a, b) with no window. Returns ((dx, dy), response).
pub fn phase_correlate(a: &[f32], b: &[f32], w: usize, h: usize) -> ((f64, f64), f64) {
    let mut fa: Vec<Complex<f64>> = a.iter().map(|&v| Complex::new(v as f64, 0.0)).collect();
    let mut fb: Vec<Complex<f64>> = b.iter().map(|&v| Complex::new(v as f64, 0.0)).collect();
    fft2(&mut fa, w, h, false);
    fft2(&mut fb, w, h, false);
    let eps = f32::EPSILON as f64;
    let mut c: Vec<Complex<f64>> = fa.iter().zip(&fb).map(|(x, y)| {
        let p = x * y.conj();
        let m = p.norm();
        p * m / (m * m + eps)
    }).collect();
    fft2(&mut c, w, h, true);
    // fftShift, then find the peak and its 5x5 weighted centroid.
    let (xm, ym) = (w / 2, h / 2);
    let at = |x: usize, y: usize| c[((y + ym) % h) * w + (x + xm) % w].re;
    let (mut px, mut py, mut best) = (0, 0, f64::MIN);
    for y in 0..h {
        for x in 0..w {
            if at(x, y) > best {
                best = at(x, y);
                (px, py) = (x, y);
            }
        }
    }
    let (mut cx, mut cy, mut sum) = (0.0, 0.0, 0.0);
    for y in py.saturating_sub(2)..=(py + 2).min(h - 1) {
        for x in px.saturating_sub(2)..=(px + 2).min(w - 1) {
            let v = at(x, y);
            cx += x as f64 * v;
            cy += y as f64 * v;
            sum += v;
        }
    }
    let response = sum / (w * h) as f64;
    let s = sum + f64::EPSILON;
    ((w as f64 / 2.0 - cx / s, h as f64 / 2.0 - cy / s), response)
}

fn corr(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let cov: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let (va, vb) = (a.iter().map(|x| (x - ma).powi(2)).sum::<f64>(), b.iter().map(|y| (y - mb).powi(2)).sum::<f64>());
    cov / (va * vb).sqrt()
}

fn std(a: &[f64]) -> f64 {
    let n = a.len() as f64;
    let m = a.iter().sum::<f64>() / n;
    (a.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n).sqrt()
}

pub struct Lag {
    pub value: f64,
    pub measured: bool,
    frames: VecDeque<(f64, Vec<f32>)>,
}

impl Lag {
    /// Starts from the last lag solve found for this headset and camera (mirror-lag.json), or -20 ms
    /// if there isn't one. That's what it found on the Frame, because polled poses are stamped when
    /// their answer arrives, which is a little late.
    pub fn new(conf: &std::path::Path) -> Lag {
        let guess = std::fs::read(conf.join("mirror-lag.json")).ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v["lag_ms"].as_f64()).map_or(-0.02, |ms| ms / 1000.0);
        Lag { value: guess, measured: false, frames: VecDeque::with_capacity(60) }
    }

    pub fn add(&mut self, t: f64, g: &Gray, head: &HeadTrack) {
        if self.frames.len() == 60 {
            self.frames.pop_front();
        }
        self.frames.push_back((t, shrink(g)));
        if self.frames.len() >= 12 && self.frames.len() % 4 == 0 {
            self.estimate(head);
        }
    }

    fn estimate(&mut self, head: &HeadTrack) {
        let (w, h) = SMALL;
        let fr: Vec<&(f64, Vec<f32>)> = self.frames.iter().collect();
        let mut moves = vec![];
        for p in fr.windows(2) {
            let ((t0, g0), (t1, g1)) = (p[0], p[1]);
            if t1 - t0 < 0.5 {
                let ((dx, dy), response) = phase_correlate(g0, g1, w, h);
                if response > 0.05 {
                    moves.push((*t0, *t1, dx, dy));
                }
            }
        }
        if moves.len() < 8 {
            return;
        }
        let mut best: Option<(f64, f64)> = None;
        let mut lag = 0.0;
        'lags: while lag < 0.3 - 1e-9 {
            let mut turns = vec![];
            for &(t0, t1, _, _) in &moves {
                let (Some(a), Some(b)) = (head.at(t0 - lag), head.at(t1 - lag)) else {
                    lag += 0.01;
                    continue 'lags;
                };
                let (ra, _) = mat(&a.0);
                let (rb, _) = mat(&b.0);
                let rv = rotvec(&(ra.transpose() * rb)); // In the head's frame.
                turns.push((rv[1], rv[0])); // Yaw shifts the image sideways, pitch shifts it up and down.
            }
            let (yaw, pitch): (Vec<f64>, Vec<f64>) = turns.into_iter().unzip();
            if std(&yaw) < 0.003 {
                return; // Hardly turning, so there's nothing to measure by.
            }
            let mx: Vec<f64> = moves.iter().map(|m| m.2).collect();
            let my: Vec<f64> = moves.iter().map(|m| m.3).collect();
            let score = corr(&mx, &yaw).abs() + if std(&pitch) > 0.003 { corr(&my, &pitch).abs() } else { 0.0 };
            if best.is_none_or(|b| score > b.0) {
                best = Some((score, lag));
            }
            lag += 0.01;
        }
        if let Some((score, l)) = best
            && score > 0.8
        {
            if !self.measured || (l - self.value).abs() > 0.005 {
                eprintln!("mirror lag {:.0} ms (fit {score:.2})", l * 1000.0);
            }
            self.value = l;
            self.measured = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_shift() {
        let (w, h) = SMALL;
        // A texture shifted circularly by (7, -3), since that's what the DFT assumes.
        let mut seed = 12345u32;
        let tex: Vec<f32> = (0..w * h).map(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as f32
        }).collect();
        let shifted: Vec<f32> = (0..w * h).map(|i| {
            let (x, y) = (i % w, i / w);
            tex[((y + h + 3) % h) * w + (x + w - 7) % w]
        }).collect();
        let ((dx, dy), r) = phase_correlate(&tex, &shifted, w, h);
        assert!((dx - 7.0).abs() < 0.3 && (dy + 3.0).abs() < 0.3 && r > 0.05, "{dx} {dy} {r}");
    }
}
