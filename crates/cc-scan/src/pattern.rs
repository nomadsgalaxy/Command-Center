//! The scan's tag layouts for one monitor, ported from home/pattern.py. They use ArUco 4x4 tags from
//! DICT_4X4_250 with ids base..base+49: the four corner tags are base+0..3, the size ladder is
//! base+4..9, and the dense grid is base+10.. Each layout gives pattern.py's JSON (every tag's
//! corners as fractions of the screen, TL TR BR BL, plus what the step needs) and the tags in
//! pixels. The hosts draw them from those (docs/agent.md 3a). render() draws them here for the
//! camera refit's board, which is shown on an overlay.
use crate::dict::CODES;
use crate::image::Gray;
use serde_json::{Map, Value, json};

const QUIET: f64 = 0.25; // White around each tag, as a fraction of its side.

pub struct Layout {
    pub json: Value,
    /// (id, x, y, side px).
    pub rects: Vec<(usize, i64, i64, i64)>,
}

/// The top band the host's banner uses (tagshow banner_h). No tag or quiet zone goes there.
pub fn band(h: i64) -> i64 {
    24.max(h / 36)
}

fn put(tags: &mut Map<String, Value>, rects: &mut Vec<(usize, i64, i64, i64)>, id: usize, x: i64, y: i64, side: i64, w: i64, h: i64) {
    let (wf, hf) = (w as f64, h as f64);
    let c = |px: i64, py: i64| json!([px as f64 / wf, py as f64 / hf]);
    tags.insert(id.to_string(), json!([c(x, y), c(x + side, y), c(x + side, y + side), c(x, y + side)]));
    rects.push((id, x, y, side));
}

/// Python's int() of a float, which rounds towards zero.
fn int(v: f64) -> i64 {
    v as i64
}

/// Python's // on ints, which rounds towards minus infinity.
fn floordiv(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// Six tags, each 1.35x smaller than the last, in a row along the box's long edge (ids base+4..9).
fn place_ladder(tags: &mut Map<String, Value>, rects: &mut Vec<(usize, i64, i64, i64)>, w: i64, h: i64, bx: (i64, i64, i64, i64), base: usize) -> Map<String, Value> {
    let (x0, y0, x1, y1) = bx;
    let (bw, bh) = (x1 - x0, y1 - y0);
    let (long, short) = (bw.max(bh), bw.min(bh));
    let mut sides: Vec<i64> = (0..6).map(|i| floordiv(int(short as f64 * 0.6 / 1.35f64.powi(i)), 6) * 6).collect();
    while sides.iter().sum::<i64>() as f64 * (1.0 + 2.0 * QUIET) > long as f64 {
        sides = sides.iter().map(|&s| floordiv(int(s as f64 * 0.9), 6) * 6).collect();
    }
    let mut pos = int((long as f64 - sides.iter().sum::<i64>() as f64 * (1.0 + 2.0 * QUIET)) / 2.0);
    let mut out = Map::new();
    for (k, &s) in sides.iter().enumerate() {
        pos += int(QUIET * s as f64);
        let (a, b) = if bw >= bh { (x0 + pos, y0 + floordiv(bh - s, 2)) } else { (x0 + floordiv(bw - s, 2), y0 + pos) };
        put(tags, rects, base + 4 + k, a, b, s, w, h);
        pos += s + int(QUIET * s as f64);
        out.insert((base + 4 + k).to_string(), json!(s));
    }
    out
}

/// The corner tags with the size ladder between them, like pattern.py's frame.
pub fn frame(w: i64, h: i64, base: usize) -> Layout {
    let b = band(h);
    let side = int(w.min(h - b) as f64 * 0.3);
    let q = int(QUIET * side as f64);
    let spots = [(q, b + q), (w - q - side, b + q), (w - q - side, h - q - side), (q, h - q - side)];
    let (mut tags, mut rects) = (Map::new(), vec![]);
    for (k, &(x, y)) in spots.iter().enumerate() {
        put(&mut tags, &mut rects, base + k, x, y, side, w, h);
    }
    let bx = if w > h { (2 * q + side, b + q, w - 2 * q - side, h - q) } else { (q, b + 2 * q + side, w - q, h - 2 * q - side) };
    let mut sides = place_ladder(&mut tags, &mut rects, w, h, bx, base);
    for k in 0..4 {
        sides.insert((base + k).to_string(), json!(side));
    }
    Layout { json: json!({"tags": tags, "corner_side_px": side, "sides_px": sides}), rects }
}

/// Just the ladder, across the screen below the banner.
pub fn ladder(w: i64, h: i64, base: usize) -> Layout {
    let (mut tags, mut rects) = (Map::new(), vec![]);
    let sides = place_ladder(&mut tags, &mut rects, w, h, (0, band(h), w, h), base);
    Layout { json: json!({"tags": tags, "sides_px": sides}), rects }
}

/// As many tags of that size as fit, up to 40 (ids base+10..).
pub fn dense(w: i64, h: i64, base: usize, side: i64) -> Layout {
    let side = side - side % 6;
    let pitch = side as f64 * (1.0 + QUIET);
    let top = band(h);
    let across = |len: f64| int((len - QUIET * side as f64) / pitch);
    let fits = across(w as f64) * across((h - top) as f64);
    if fits <= 3 {
        return Layout { json: json!({"tags": {}, "grid": [0, 0], "side_px": side, "fits": fits}), rects: vec![] };
    }
    let (mut cols, mut rows) = (2.max(across(w as f64)), 2.max(across((h - top) as f64)));
    while rows * cols > 40 {
        if cols >= rows {
            cols -= 1;
        } else {
            rows -= 1;
        }
    }
    let (mut tags, mut rects) = (Map::new(), vec![]);
    let s = side as f64;
    for r in 0..rows {
        for c in 0..cols {
            let x = int(QUIET * s + (w as f64 - s - 2.0 * QUIET * s) * c as f64 / (cols - 1) as f64);
            let y = int(top as f64 + QUIET * s + ((h - top) as f64 - s - 2.0 * QUIET * s) * r as f64 / (rows - 1) as f64);
            put(&mut tags, &mut rects, base + 10 + (r * cols + c) as usize, x, y, side, w, h);
        }
    }
    Layout { json: json!({"tags": tags, "grid": [rows, cols], "side_px": side, "fits": fits}), rects }
}

/// A grid of tags for the camera refit. It's shown on an overlay, not a monitor.
pub fn board(w: i64, h: i64, base: usize, cols: i64, rows: i64) -> Layout {
    let side = floordiv(int((w as f64 / (cols as f64 * (1.0 + 2.0 * QUIET))).min(h as f64 / (rows as f64 * (1.0 + 2.0 * QUIET)))), 6) * 6;
    let (mut tags, mut rects) = (Map::new(), vec![]);
    for r in 0..rows {
        for c in 0..cols {
            let x = int((c as f64 + 0.5) * w as f64 / cols as f64 - side as f64 / 2.0);
            let y = int((r as f64 + 0.5) * h as f64 / rows as f64 - side as f64 / 2.0);
            put(&mut tags, &mut rects, base + (r * cols + c) as usize, x, y, side, w, h);
        }
    }
    Layout { json: json!({"tags": tags, "side_px": side}), rects }
}

/// Draws the layout, white with black tags, like generateImageMarker: 6 cells, nearest-neighbour up to the side.
pub fn render(w: usize, h: usize, rects: &[(usize, i64, i64, i64)]) -> Gray {
    let mut g = Gray { w, h, px: vec![255; w * h] };
    for &(id, x, y, side) in rects {
        let bits = CODES[id];
        for dy in 0..side {
            for dx in 0..side {
                let (cx, cy) = ((dx * 6 / side) as usize, (dy * 6 / side) as usize);
                let white = (1..5).contains(&cx) && (1..5).contains(&cy) && (bits >> (15 - 4 * (cy - 1) - (cx - 1))) & 1 == 1;
                let (px, py) = (x + dx, y + dy);
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    g.px[py as usize * w + px as usize] = if white { 255 } else { 0 };
                }
            }
        }
    }
    g
}

/// Takes pattern.py's command line, the way `scan.py serve` takes it ("frame w h dir base label",
/// ...), and returns the layout, or None for a bad line.
pub fn run(argv: &[&str]) -> Option<Layout> {
    let num = |i: usize| argv.get(i)?.parse::<i64>().ok();
    let (w, h, base) = (num(1)?, num(2)?, num(4)? as usize);
    match *argv.first()? {
        "frame" => Some(frame(w, h, base)),
        "ladder" => Some(ladder(w, h, base)),
        "dense" => Some(dense(w, h, base, num(5)?)),
        "board" => {
            let (c, r) = argv.get(5)?.split_once('x')?;
            Some(board(w, h, base, c.parse().ok()?, r.parse().ok()?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every layout reads back with the detector, with exactly the right ids, like pattern.py's check().
    #[test]
    fn layouts_detect_themselves() {
        for (w, h) in [(1920i64, 1080i64), (1440, 2560), (5120, 1440)] {
            for l in [frame(w, h, 0), ladder(w, h, 50), dense(w, h, 100, 120), board(w, h, 200, 6, 4)] {
                let g = render(w as usize, h as usize, &l.rects);
                let mut found: Vec<usize> = crate::aruco::detect(&g, &crate::dict::DICT_4X4_250, &crate::aruco::Params::default()).iter().map(|m| m.id).collect();
                let mut want: Vec<usize> = l.rects.iter().map(|r| r.0).collect();
                found.sort();
                want.sort();
                assert_eq!(found, want, "{w}x{h}");
            }
        }
    }
}
