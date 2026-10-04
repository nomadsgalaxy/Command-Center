//! The host's screens, drawn into a plain buffer that the Wayland side only shows. There's the align's
//! tag screen and its wait screen, done like home/tagshow.py: tags come from the bit table, and no tag
//! may enter the banner band. There's the pairing key, done like home/pair.py's Screen: white on
//! near-black, the digits a quarter of the height, and the key's tags. And there's a calm grey prompt.
//! The font is Atkinson Hyperlegible Mono, bundled under the OFL.
use crate::aruco::{ADDR_TAG_BITS, KEY_TAG_BITS, TAG_BITS};
use serde_json::Value;

static FONT_BYTES: &[u8] = include_bytes!("../../../third_party/fonts/AtkinsonHyperlegibleMono[wght].ttf");

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    /// 0xAARRGGBB, which is how wl_shm's ARGB8888 wants it on little-endian.
    pub px: Vec<u32>,
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

fn grey(v: u8) -> u32 {
    rgb(v, v, v)
}

impl Canvas {
    pub fn new(w: usize, h: usize, fill: u32) -> Canvas {
        Canvas { w, h, px: vec![fill; w * h] }
    }

    pub fn rect(&mut self, x: i64, y: i64, w: i64, h: i64, c: u32) {
        let (x0, y0) = (x.max(0) as usize, y.max(0) as usize);
        let (x1, y1) = (((x + w).max(0) as usize).min(self.w), ((y + h).max(0) as usize).min(self.h));
        for row in y0..y1 {
            self.px[row * self.w + x0..row * self.w + x1.max(x0)].fill(c);
        }
    }

    /// Draws text centred in the box (x, y, w, h) at `size` px, shrunk to fit the width and blended over what's there.
    pub fn text(&mut self, s: &str, bx: i64, by: i64, bw: i64, bh: i64, size: f32, c: u32) {
        let font = fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default()).expect("the bundled font");
        let width = |sz: f32| s.chars().map(|ch| font.metrics(ch, sz).advance_width).sum::<f32>();
        let mut size = size;
        if width(size) > bw as f32 * 0.95 {
            size *= bw as f32 * 0.95 / width(size);
        }
        let lm = font.horizontal_line_metrics(size).map_or((size * 0.8, size * 0.2), |m| (m.ascent, -m.descent));
        let mut pen = bx as f32 + (bw as f32 - width(size)) / 2.0;
        let base = by as f32 + (bh as f32 - (lm.0 + lm.1)) / 2.0 + lm.0;
        let (cr, cg, cb) = ((c >> 16) & 255, (c >> 8) & 255, c & 255);
        for ch in s.chars() {
            let (m, bitmap) = font.rasterize(ch, size);
            let (gx, gy) = (pen.round() as i64 + m.xmin as i64, (base - m.height as f32 - m.ymin as f32).round() as i64);
            for row in 0..m.height {
                for col in 0..m.width {
                    let a = bitmap[row * m.width + col] as u32;
                    let (x, y) = (gx + col as i64, gy + row as i64);
                    if a == 0 || x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
                        continue;
                    }
                    let p = &mut self.px[y as usize * self.w + x as usize];
                    let (pr, pg, pb) = ((*p >> 16) & 255, (*p >> 8) & 255, *p & 255);
                    let mix = |f: u32, b: u32| (f * a + b * (255 - a)) / 255;
                    *p = 0xff00_0000 | mix(cr, pr) << 16 | mix(cg, pg) << 8 | mix(cb, pb);
                }
            }
            pen += m.advance_width;
        }
    }
}

/// Draws one tag at x, y: a black square `side` px wide with its 4x4 bits in white, in cells of side/6.
fn tag(c: &mut Canvas, bits: u16, x: i64, y: i64, side: i64) {
    let cell = side / 6;
    c.rect(x, y, 6 * cell, 6 * cell, grey(0));
    for r in 0..4 {
        for k in 0..4 {
            if bits >> (15 - 4 * r - k) & 1 == 1 {
                c.rect(x + (k as i64 + 1) * cell, y + (r as i64 + 1) * cell, cell, cell, grey(255));
            }
        }
    }
}

/// The band across the top that only the banner uses. pattern.py lays the tags out below it.
pub fn banner_h(h: i64) -> i64 {
    24.max(h / 36)
}

pub const BANNER: &str = "Command Center is aligning this screen  \u{b7}  Esc to cancel";

/// Draws the align's screen from parameters that were already checked ({bg: white|wait, tags: [[id, x, y, side]..]}).
pub fn tag_screen(p: &Value, w: usize, h: usize) -> Canvas {
    let wait = p["bg"] == "wait";
    let mut c = Canvas::new(w, h, grey(if wait { 205 } else { 255 }));
    for t in p["tags"].as_array().into_iter().flatten() {
        let v: Vec<i64> = t.as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
        if v.len() == 4 && (0..250).contains(&v[0]) {
            tag(&mut c, TAG_BITS[v[0] as usize], v[1], v[2], v[3]);
        }
    }
    let band = banner_h(h as i64);
    c.text(BANNER, 0, 0, w as i64, band, band as f32 * 0.6, grey(110));
    if wait {
        c.text("scanning another monitor", 0, 0, w as i64, h as i64, 24f32.max(h as f32 / 14.0), grey(90));
    }
    c
}

/// Draws a calm grey question, like Esc's block prompt or the host's ask after 3 cancels.
pub fn prompt(text: &str, w: usize, h: usize) -> Canvas {
    let mut c = Canvas::new(w, h, grey(205));
    let lines: Vec<&str> = text.lines().collect();
    let lh = (h as i64 / 12).max(30);
    let top = (h as i64 - lh * lines.len() as i64) / 2;
    for (i, l) in lines.iter().enumerate() {
        c.text(l, 0, top + i as i64 * lh, w as i64, lh, 24f32.max(h as f32 / 20.0), grey(60));
    }
    c
}

/// The screen after an Esc: light grey, so there's no black-to-white flash (photosensitive epilepsy), plus what a second Esc does.
pub fn quiet(w: usize, h: usize) -> Canvas {
    let mut c = Canvas::new(w, h, grey(205));
    c.text("Cancelled.  Esc again: block this Frame", 0, 0, w as i64, h as i64, 20f32.max(h as f32 / 30.0), grey(90));
    c
}

/// Draws the pairing key (docs/pairing.md §3): the host's name, the 6 digits at a quarter of the height,
/// the key as three tags under them, the time left and "Esc or tap to cancel". It's white on #0B0B0F.
/// With the host's address, a second strip of four tags (one octet each) goes under the key's, with the
/// address in text beneath it, so a Frame that can't find the host by mDNS can still reach it.
pub fn key_screen(host: &str, key: &str, addr: Option<[u8; 4]>, left_s: u64, question: Option<&str>, w: usize, h: usize) -> Canvas {
    let (wi, hi) = (w as i64, h as i64);
    let mut c = Canvas::new(w, h, rgb(0x0b, 0x0b, 0x0f));
    let white = rgb(255, 255, 255);
    c.text(&format!("Pair {host} with Command Center: look at this screen from the Frame, or type the key"), 0, hi / 30, wi, hi / 12, hi as f32 / 22.0, white);
    c.text(&format!("{} {}", &key[..3], &key[3..]), 0, hi / 8, wi, hi * 3 / 10, hi as f32 / 4.0, white);
    // The key's tags: three of them, two digits each (DICT_4X4_1000 ids 900+), in a white strip with quiet zones.
    // The address strip below has the same tag size, so it needs room for two: h/8 at most.
    let side = (hi / 8).min(wi / 14) / 6 * 6;
    let (gap, q) = (side / 3, side / 6);
    let strip = |c: &mut Canvas, bits: &[u16], y0: i64| {
        let total = bits.len() as i64 * side + (bits.len() as i64 - 1) * gap;
        let x0 = (wi - total) / 2;
        c.rect(x0 - q, y0 - q, total + 2 * q, side + 2 * q, white);
        for (k, b) in bits.iter().enumerate() {
            tag(c, *b, x0 + k as i64 * (side + gap), y0, side);
        }
    };
    let y0 = hi * 43 / 100;
    let digits: Vec<u16> = (0..3).map(|k| KEY_TAG_BITS[key[2 * k..2 * k + 2].parse::<usize>().unwrap_or(0)]).collect();
    strip(&mut c, &digits, y0);
    if let Some(a) = addr {
        let y1 = y0 + side + 2 * q + gap;
        strip(&mut c, &a.map(|o| ADDR_TAG_BITS[o as usize]), y1);
        c.text(&format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]), 0, y1 + side + 2 * q, wi, hi / 20, hi as f32 / 28.0, white);
    }
    let info = match question {
        Some(q) => q.to_owned(),
        None => format!("{}:{:02} left   \u{b7}   Esc or tap to cancel", left_s / 60, left_s % 60),
    };
    c.text(&info, 0, hi * 4 / 5, wi, hi / 10, hi as f32 / 28.0, white);
    c
}

/// Returns the canvas as 8-bit grey in a binary PGM. Checks use it because that's what OpenCV reads.
pub fn to_pgm(c: &Canvas) -> Vec<u8> {
    let mut out = format!("P5\n{} {}\n255\n", c.w, c.h).into_bytes();
    out.extend(c.px.iter().map(|p| ((((p >> 16) & 255) * 30 + ((p >> 8) & 255) * 59 + (p & 255) * 11) / 100) as u8));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a drawn canvas the way the Frame does: cc-scan's own detector and decoders.
    fn read(c: &Canvas) -> (Option<String>, Option<[u8; 4]>) {
        let g = cc_scan::image::Gray { w: c.w, h: c.h, px: to_pgm(c)[format!("P5\n{} {}\n255\n", c.w, c.h).len()..].to_vec() };
        let tags = cc_scan::detect(&g, &cc_scan::dict::DICT_4X4_1000);
        (cc_scan::read_key(&tags), cc_scan::read_addr(&tags))
    }

    #[test]
    fn the_key_screen_decodes_to_its_key_and_address() {
        for (w, h) in [(1920, 1080), (1280, 720), (2560, 1440), (1080, 1920), (1000, 1000)] {
            for (key, addr) in [("123456", [192, 168, 1, 20]), ("000000", [10, 0, 0, 1]), ("999900", [172, 31, 255, 254])] {
                let (k, a) = read(&key_screen("desk", key, Some(addr), 299, None, w, h));
                assert_eq!((k.as_deref(), a), (Some(key), Some(addr)), "{w}x{h}");
            }
        }
    }

    #[test]
    fn no_address_means_no_address_tags() {
        let (k, a) = read(&key_screen("desk", "482917", None, 299, None, 1920, 1080));
        assert_eq!((k.as_deref(), a), (Some("482917"), None));
    }

    #[test]
    fn the_bit_tables_match_cc_scans_dictionary() {
        assert_eq!(&ADDR_TAG_BITS[..], &cc_scan::dict::CODES[cc_scan::ADDR_BASE..cc_scan::ADDR_BASE + 256]);
        assert_eq!(&KEY_TAG_BITS[..], &cc_scan::dict::CODES[cc_scan::KEY_BASE..cc_scan::KEY_BASE + 100]);
        assert_eq!(&TAG_BITS[..], &cc_scan::dict::CODES[..250]);
        assert!(cc_scan::ADDR_BASE + 256 <= 900 && cc_scan::ADDR_BASE >= 250, "clear of align's and the key's ids");
    }
}
