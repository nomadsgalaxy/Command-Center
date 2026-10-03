//! findContours(RETR_LIST, CHAIN_APPROX_NONE), ported from OpenCV 4.13's contours_new.cpp. It's
//! Suzuki-Abe border following on a 0/1 image padded with a zero border. Porting it exactly means
//! the scan finds the same candidate squares as OpenCV, point for point.

const DELTAS: [(isize, isize); 8] = [(1, 0), (1, -1), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1), (1, 1)];
const NEW: i8 = 2;
const RIGHT: i8 = 2 | -128; // MASK8_NEW | MASK8_RIGHT

/// Returns every border (outer and hole) of the 1-regions of `bin` (w x h, 0 or 1). Each border
/// lists all its pixels in order, and the borders come in the order a raster scan meets them.
pub fn find(bin: &[u8], w: usize, h: usize) -> Vec<Vec<(i32, i32)>> {
    let (pw, ph) = (w + 2, h + 2);
    let mut img = vec![0i8; pw * ph];
    for y in 0..h {
        for x in 0..w {
            img[(y + 1) * pw + x + 1] = (bin[y * w + x] != 0) as i8;
        }
    }
    let delta = |s: i8| -> isize {
        let (dx, dy) = DELTAS[(s & 7) as usize];
        dx + dy * pw as isize
    };
    let mut out = Vec::new();
    for y in 1..ph - 1 {
        let mut prev: i8 = 0;
        let mut x = 1;
        while x < pw - 1 {
            let p = img[y * pw + x];
            if p == prev {
                x += 1;
                continue;
            }
            let hole = if prev == 0 && p == 1 {
                false
            } else if p == 0 && prev >= 1 {
                true
            } else {
                prev = p;
                x += 1;
                continue;
            };
            let sx = x - hole as usize;
            out.push(fetch(&mut img, &delta, y * pw + sx, (sx as i32 - 1, y as i32 - 1), hole));
            prev = img[y * pw + x];
            x += 1;
        }
    }
    out
}

fn fetch(img: &mut [i8], delta: &dyn Fn(i8) -> isize, start: usize, origin: (i32, i32), hole: bool) -> Vec<(i32, i32)> {
    let mut pts = Vec::new();
    let i0 = start as isize;
    let mut s_end: i8 = if hole { 0 } else { 4 };
    let mut s = s_end;
    let mut i1;
    loop {
        s = (s - 1) & 7;
        i1 = i0 + delta(s);
        if img[i1 as usize] != 0 || s == s_end {
            break;
        }
    }
    if s == s_end {
        img[i0 as usize] = RIGHT;
        pts.push(origin);
        return pts;
    }
    let mut pt = origin;
    let mut i3 = i0;
    let mut i4;
    loop {
        s_end = s;
        s = s.min(15);
        loop {
            // OpenCV does: while (s < MAX_SIZE - 1) { ++s; ... break on a nonzero pixel }
            if s >= 15 {
                i4 = i3 + delta(s);
                break;
            }
            s += 1;
            i4 = i3 + delta(s);
            if img[i4 as usize] != 0 {
                break;
            }
        }
        s &= 7;
        if ((s as i32 - 1) as u32) < s_end as u32 {
            img[i3 as usize] = RIGHT;
        } else if img[i3 as usize] == 1 {
            img[i3 as usize] = NEW;
        }
        pts.push(pt);
        let (dx, dy) = DELTAS[s as usize];
        pt = (pt.0 + dx as i32, pt.1 + dy as i32);
        if i4 == i0 && i3 == i1 {
            break;
        }
        i3 = i4;
        s = (s + 4) & 7;
    }
    pts
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_square_and_its_hole() {
        // A 5x5 ring of 1s around a 3x3 hole, in a 9x9 image.
        let (w, h) = (9, 9);
        let mut b = vec![0u8; w * h];
        for y in 2..7 {
            for x in 2..7 {
                b[y * w + x] = 1;
            }
        }
        for y in 3..6 {
            for x in 3..6 {
                b[y * w + x] = 0;
            }
        }
        let c = super::find(&b, w, h);
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(c[0].len(), 16); // The outer border: 4 sides of 5, each corner counted once.
        assert_eq!(c[0][0], (2, 2));
        assert!(c[1].len() >= 8); // The hole's border.
    }
}
