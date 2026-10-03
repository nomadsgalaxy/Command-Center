//! 8-bit grey images and the few OpenCV operations the scan needs: BGR to grey, the 3x3 Gaussian,
//! CLAHE, the adaptive threshold and Otsu. They're ported to give exactly the same pixels.

#[derive(Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    pub fn new(w: usize, h: usize) -> Gray {
        Gray { w, h, px: vec![0; w * h] }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    /// Converts packed RGB (the mirror's RGB3, or a decoded JPEG) with cvtColor's fixed-point weights.
    pub fn from_rgb(w: usize, h: usize, rgb: &[u8]) -> Gray {
        let px = rgb.chunks_exact(3).map(|p| ((p[0] as u32 * 19595 + p[1] as u32 * 38470 + p[2] as u32 * 7471 + 32768) >> 16) as u8).collect();
        Gray { w, h, px }
    }
}

/// The BORDER_REFLECT_101 index.
#[inline]
fn reflect101(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let mut i = i;
    while i < 0 || i >= n {
        i = if i < 0 { -i } else { 2 * n - 2 - i };
    }
    i as usize
}

/// GaussianBlur(3x3, sigma 0): [1 2 1] x [1 2 1] / 16, rounded. OpenCV's 8-bit fixed point gives exactly this.
pub fn gaussian3(g: &Gray) -> Gray {
    let (w, h) = (g.w, g.h);
    let mut tmp = vec![0u16; w * h];
    for y in 0..h {
        let row = &g.px[y * w..(y + 1) * w];
        for x in 0..w {
            let l = row[reflect101(x as isize - 1, w)] as u16;
            let r = row[reflect101(x as isize + 1, w)] as u16;
            tmp[y * w + x] = l + 2 * row[x] as u16 + r;
        }
    }
    let mut out = Gray::new(w, h);
    for y in 0..h {
        let (u, d) = (reflect101(y as isize - 1, h), reflect101(y as isize + 1, h));
        for x in 0..w {
            let s = tmp[u * w + x] as u32 + 2 * tmp[y * w + x] as u32 + tmp[d * w + x] as u32;
            out.px[y * w + x] = ((s + 8) >> 4) as u8;
        }
    }
    out
}

/// Same as cv::saturate_cast<uchar>(float): round half to even, then clamp.
#[inline]
fn sat_u8(v: f32) -> u8 {
    v.round_ties_even().clamp(0.0, 255.0) as u8
}

/// Same as createCLAHE(clip, (tiles, tiles)).apply.
pub fn clahe(g: &Gray, clip: f64, tiles: usize) -> Gray {
    let (tw, th) = (g.w.div_ceil(tiles), g.h.div_ceil(tiles));
    let (tw, th) = if g.w % tiles == 0 && g.h % tiles == 0 { (g.w / tiles, g.h / tiles) } else { (tw, th) };
    let total = (tw * th) as i32;
    let lut_scale = 255.0f32 / total as f32;
    let limit = ((clip * total as f64 / 256.0) as i32).max(1);
    let mut lut = vec![0u8; tiles * tiles * 256];
    for k in 0..tiles * tiles {
        let (ty, tx) = (k / tiles, k % tiles);
        let mut hist = [0i32; 256];
        for y in ty * th..(ty + 1) * th {
            let yy = reflect101(y as isize, g.h);
            for x in tx * tw..(tx + 1) * tw {
                hist[g.at(reflect101(x as isize, g.w), yy) as usize] += 1;
            }
        }
        let mut clipped = 0;
        for v in hist.iter_mut() {
            if *v > limit {
                clipped += *v - limit;
                *v = limit;
            }
        }
        let (batch, mut residual) = (clipped / 256, clipped % 256);
        for v in hist.iter_mut() {
            *v += batch;
        }
        if residual != 0 {
            let step = (256 / residual).max(1) as usize;
            let mut i = 0;
            while i < 256 && residual > 0 {
                hist[i] += 1;
                i += step;
                residual -= 1;
            }
        }
        let mut sum = 0;
        for i in 0..256 {
            sum += hist[i];
            lut[k * 256 + i] = sat_u8(sum as f32 * lut_scale);
        }
    }
    let mut out = Gray::new(g.w, g.h);
    let (inv_tw, inv_th) = (1.0f32 / tw as f32, 1.0f32 / th as f32);
    let xs: Vec<(usize, usize, f32)> = (0..g.w).map(|x| {
        let txf = (x as f32).mul_add(inv_tw, -0.5); // OpenCV's build fuses these multiply-adds on aarch64.
        let t1 = txf.floor() as isize;
        let xa = txf - t1 as f32;
        ((t1.max(0)) as usize, ((t1 + 1) as usize).min(tiles - 1), xa)
    }).collect();
    for y in 0..g.h {
        let tyf = (y as f32).mul_add(inv_th, -0.5);
        let t1 = tyf.floor() as isize;
        let ya = tyf - t1 as f32;
        let (p1, p2) = (t1.max(0) as usize * tiles, ((t1 + 1) as usize).min(tiles - 1) * tiles);
        for x in 0..g.w {
            let v = g.at(x, y) as usize;
            let (a, b, xa) = xs[x];
            let l = |p: usize, t: usize| lut[(p + t) * 256 + v] as f32;
            // Fuse the multiply-adds the way GCC fuses OpenCV's on aarch64, so every pixel matches.
            let xa1 = 1.0 - xa;
            let lerp = |pa: f32, pb: f32| pa.mul_add(xa1, pb * xa);
            let (t1, t2) = (lerp(l(p1, a), l(p1, b)), lerp(l(p2, a), l(p2, b)));
            let res = t1.mul_add(1.0 - ya, t2 * ya);
            out.px[y * g.w + x] = sat_u8(res);
        }
    }
    out
}

/// adaptiveThreshold(255, MEAN_C, BINARY_INV, win, c) as 0/1. It's 1 where src <= mean - c. The
/// mean is over the window (BORDER_REPLICATE), rounded to 8 bits the way boxFilter rounds it.
pub fn threshold_inv(g: &Gray, win: usize, c: f64) -> Vec<u8> {
    let win = win | 1;
    let r = win / 2;
    let (w, h) = (g.w, g.h);
    let (pw, ph) = (w + 2 * r, h + 2 * r);
    // Integral image of the replicate-padded image.
    let mut ii = vec![0u32; (pw + 1) * (ph + 1)];
    for y in 0..ph {
        let sy = y.saturating_sub(r).min(h - 1);
        let mut run = 0u32;
        for x in 0..pw {
            let sx = x.saturating_sub(r).min(w - 1);
            run += g.at(sx, sy) as u32;
            ii[(y + 1) * (pw + 1) + x + 1] = ii[y * (pw + 1) + x + 1] + run;
        }
    }
    let scale = 1.0 / (win * win) as f64;
    let delta = c.ceil() as i32;
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, y0, x1, y1) = (x, y, x + win, y + win);
            let s = ii[y1 * (pw + 1) + x1] + ii[y0 * (pw + 1) + x0] - ii[y0 * (pw + 1) + x1] - ii[y1 * (pw + 1) + x0];
            let mean = (s as f64 * scale).round_ties_even() as i32;
            out[y * w + x] = (g.at(x, y) as i32 - mean <= -delta) as u8;
        }
    }
    out
}

/// Otsu's threshold of these pixels, ported from cv::threshold's getThreshVal_Otsu. Its quirks are
/// kept: mu1 is scaled by q1 even on the skipped bins.
pub fn otsu(px: &[u8]) -> u8 {
    let mut hist = [0u32; 256];
    for &p in px {
        hist[p as usize] += 1;
    }
    let scale = 1.0 / px.len() as f64;
    let mu: f64 = (0..256).map(|i| i as f64 * hist[i] as f64).sum::<f64>() * scale;
    let (mut mu1, mut q1, mut best, mut thr) = (0.0f64, 0.0f64, 0.0f64, 0u8);
    let eps = f32::EPSILON as f64;
    for i in 0..256 {
        let p = hist[i] as f64 * scale;
        mu1 *= q1;
        q1 += p;
        let q2 = 1.0 - q1;
        if q1.min(q2) < eps || q1.max(q2) > 1.0 - eps {
            continue;
        }
        mu1 = (mu1 + i as f64 * p) / q1;
        let mu2 = (mu - q1 * mu1) / q2;
        let sigma = q1 * q2 * (mu1 - mu2) * (mu1 - mu2);
        if sigma > best {
            best = sigma;
            thr = i as u8;
        }
    }
    thr
}
