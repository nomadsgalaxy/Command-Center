//! cc-scan is Command Center's camera scan in pure Rust (docs/rust-host.md R9). It replaces
//! home/scan.py, pattern.py and solve.py. It covers the VR mirror's frames with the head's pose,
//! the ArUco tags in those frames (OpenCV's detector, ported and checked against OpenCV on
//! recorded passthrough frames), the tag layouts shown on the monitors, and the fit of each
//! monitor's place and shape.
pub mod aruco;
pub mod camera;
pub mod contours;
pub mod dict;
pub mod hud;
pub mod image;
pub mod lag;
pub mod panels;
pub mod pattern;
pub mod scan;
pub mod solve;

use aruco::{Marker, Params};

/// CLOCK_MONOTONIC in seconds. It's the same clock as Python's time.monotonic, so it matches cc-panels' and the scan's files.
pub fn now() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}
use image::Gray;

/// A tag found in a frame, in the same form scan.py's detect() reports it.
#[derive(Clone, Debug)]
pub struct Tag {
    pub id: usize,
    pub corners: [(f32, f32); 4],
    pub side_px: f32,
}

/// Port of scan.py's detect(). It runs a Gaussian 3x3 for the passthrough noise, then CLAHE 3.0 / 8x8
/// for the low contrast, then detects with the passthrough parameters. Each tag gets its mean side.
pub fn detect(gray: &Gray, dict: &dict::Dict) -> Vec<Tag> {
    let g = image::clahe(&image::gaussian3(gray), 3.0, 8);
    aruco::detect(&g, dict, &Params::passthrough()).into_iter().map(|Marker { id, corners }| {
        let side = (0..4).map(|k| {
            let (a, b) = (corners[k], corners[(k + 1) % 4]);
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
        }).sum::<f32>() / 4.0;
        Tag { id, corners, side_px: side }
    }).collect()
}

pub const KEY_BASE: usize = 900; // Same as pair.py's KEY_TAG_BASE. Ids 900-999 each carry two digits.

/// Reads the 6-digit pairing key from exactly three key tags (DICT_4X4_1000), or returns None,
/// like scan.py's read_key. The tags are ordered along their own x axis, so a turned head or a
/// portrait monitor reads the key the same way.
pub fn read_key(tags: &[Tag]) -> Option<String> {
    let mut keys: Vec<&Tag> = tags.iter().filter(|t| (KEY_BASE..KEY_BASE + 100).contains(&t.id)).collect();
    if keys.len() != 3 {
        return None;
    }
    let axis = keys.iter().fold((0.0f32, 0.0f32), |a, t| (a.0 + t.corners[1].0 - t.corners[0].0, a.1 + t.corners[1].1 - t.corners[0].1));
    let along = |t: &Tag| {
        let (mx, my) = (t.corners.iter().map(|c| c.0).sum::<f32>() / 4.0, t.corners.iter().map(|c| c.1).sum::<f32>() / 4.0);
        mx * axis.0 + my * axis.1
    };
    keys.sort_by(|a, b| along(a).partial_cmp(&along(b)).unwrap_or(std::cmp::Ordering::Equal));
    Some(keys.iter().map(|t| format!("{:02}", t.id - KEY_BASE)).collect())
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use crate::aruco::perspective;

    /// Same as pair.py's key_tag_cells: three tags, each 6x6 with a 1-cell white margin, 2 white cells apart.
    fn key_cells(key: &str) -> Vec<Vec<u8>> {
        let tag = |k: usize| -> Vec<Vec<u8>> {
            let bits = dict::CODES[KEY_BASE + key[2 * k..2 * k + 2].parse::<usize>().unwrap()];
            let mut t = vec![vec![1u8; 8]; 8];
            for y in 1..7 {
                for x in 1..7 {
                    t[y][x] = if (2..6).contains(&y) && (2..6).contains(&x) { ((bits >> (15 - 4 * (y - 2) - (x - 2))) & 1) as u8 } else { 0 };
                }
            }
            t
        };
        let (a, b, c) = (tag(0), tag(1), tag(2));
        (0..8).map(|y| [&a[y][..], &[1, 1], &b[y][..], &[1, 1], &c[y][..]].concat()).collect()
    }

    /// Same as scan.py's keytest. It draws the key tags, views them at an angle, makes them soft and
    /// low contrast in landscape and portrait, and checks that the key reads back.
    #[test]
    fn keys_read_back() {
        for key in ["071299", "000000", "999900", "123456"] {
            let cells = key_cells(key);
            let (cw, ch) = (cells[0].len() * 24 + 240, cells.len() * 24 + 240);
            for turn in [false, true] {
                let (w, h) = if turn { (ch, cw) } else { (cw, ch) };
                let src = |x: usize, y: usize| -> u8 {
                    let (x, y) = if turn { (y, ch - 1 - x) } else { (x, y) }; // A quarter turn clockwise.
                    if x < 120 || y < 120 || x >= cw - 120 || y >= ch - 120 {
                        return 40;
                    }
                    cells[(y - 120) / 24][(x - 120) / 24] * 255
                };
                let (wf, hf) = (w as f32, h as f32);
                let m = perspective(&[(0.0, 0.0), (wf, 0.0), (wf, hf), (0.0, hf)], &[(60.0, 30.0), (wf - 90.0, 80.0), (wf - 40.0, hf - 60.0), (30.0, hf - 20.0)]);
                let inv = {
                    let a = nalgebra::Matrix3::from_row_slice(&m);
                    a.try_inverse().unwrap()
                };
                let mut g = image::Gray::new(w, h);
                for y in 0..h {
                    for x in 0..w {
                        let p = inv * nalgebra::Vector3::new(x as f64, y as f64, 1.0);
                        let (sx, sy) = ((p.x / p.z).round(), (p.y / p.z).round());
                        let v = if sx >= 0.0 && sy >= 0.0 && (sx as usize) < w && (sy as usize) < h { src(sx as usize, sy as usize) } else { 40 };
                        g.px[y * w + x] = (v as f32 * 0.6 + 70.0 * 0.4) as u8; // Low contrast.
                    }
                }
                let soft = image::gaussian3(&image::gaussian3(&g));
                let got = read_key(&detect(&soft, &dict::DICT_4X4_1000));
                assert_eq!(got.as_deref(), Some(key), "{key} turned {turn}");
            }
        }
    }
}
