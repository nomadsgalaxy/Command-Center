//! cc-panels draws its own images now, the ones panels/assets.py used to make, so there's no
//! Python and no container involved. That's the cursor, each panel's border tag with its machine
//! and monitor (or its app) set like Plasma's own titles in the session's font
//! (docs/plasma-look-design.md), and the glyphs for the taskbar and the Machines window.
//!
//! Tags are masks, so one file works for every colour scheme: R is the name's coverage, G the
//! secondary text's ("Monitor 2"), B the separator dot's and A the max of the three. grab.rs
//! tints them with the theme's colours. The file is width and height (u32 LE), then RGBA
//! (load_tag). The cursor is a PNG.
//! ponytail: no kerning. Plasma's titles are short, and Pillow's basic layout didn't kern either.

use std::io::Write;

const BASE: i32 = 46; // baseline in the 64 px tall tag: caps sit on 15..46, descenders reach 57
const H: usize = 64;
const SIZE: f32 = 44.0;

/// Looks up `family` through fontconfig (fc-match) and falls back to the system's Noto Sans.
fn font(family: &str) -> Option<fontdue::Font> {
    let matched = std::process::Command::new("fc-match").args(["-f", "%{file}", family]).output().ok();
    let matched = matched.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    [matched.as_str(), "/usr/share/fonts/noto/NotoSans-Regular.ttf"].iter().filter(|p| !p.is_empty()).find_map(|p| {
        let bytes = std::fs::read(p).ok()?;
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings { scale: SIZE, ..Default::default() }).ok()
    })
}

fn width(f: &fontdue::Font, text: &str) -> f32 {
    text.chars().map(|c| f.metrics(c, SIZE).advance_width).sum()
}

/// Writes text into one coverage layer (w x H), starting at x on the baseline. It keeps the
/// max with what's already there.
fn text(layer: &mut [u8], w: usize, f: &fontdue::Font, x: f32, s: &str) {
    let mut pen = x;
    for c in s.chars() {
        let (m, bmp) = f.rasterize(c, SIZE);
        let (left, top) = (pen.round() as i32 + m.xmin, BASE - m.ymin - m.height as i32);
        for (j, row) in bmp.chunks(m.width.max(1)).enumerate() {
            for (i, &a) in row.iter().enumerate() {
                let (px, py) = (left + i as i32, top + j as i32);
                if (0..w as i32).contains(&px) && (0..H as i32).contains(&py) {
                    let o = &mut layer[py as usize * w + px as usize];
                    *o = (*o).max(a);
                }
            }
        }
        pen += m.advance_width;
    }
}

/// Packs three layers into one RGBA mask, with A as their max and `pad` px of room all round.
fn mask(layers: [&[u8]; 3], w: usize, pad: usize) -> Vec<u8> {
    let (cw, ch) = (w + 2 * pad, H + 2 * pad);
    let mut out = Vec::with_capacity(8 + cw * ch * 4);
    out.extend((cw as u32).to_le_bytes());
    out.extend((ch as u32).to_le_bytes());
    let mut px = vec![0u8; cw * ch * 4];
    for y in 0..H {
        for x in 0..w {
            let i = y * w + x;
            let (r, g, b) = (layers[0][i], layers[1][i], layers[2][i]);
            let o = ((y + pad) * cw + x + pad) * 4;
            px[o..o + 4].copy_from_slice(&[r, g, b, r.max(g).max(b)]);
        }
    }
    out.extend(px);
    out
}

/// A host's name from reverse DNS without the domain, or the address itself if it has no name.
fn host_name(host: &str) -> String {
    let Ok(ip) = host.parse::<std::net::Ipv4Addr>() else { return host.split('.').next().unwrap_or(host).to_owned() };
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as _;
    sa.sin_addr.s_addr = u32::from_ne_bytes(ip.octets());
    let mut name = [0 as libc::c_char; 256];
    let r = unsafe {
        libc::getnameinfo(&sa as *const _ as *const libc::sockaddr, std::mem::size_of_val(&sa) as _, name.as_mut_ptr(), name.len() as _, std::ptr::null_mut(), 0, libc::NI_NAMEREQD)
    };
    if r != 0 {
        return host.to_owned();
    }
    let full = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
    full.split('.').next().unwrap_or(&full).to_owned()
}

/// A card's border tag. It's the name (a remote's hostname, or the given text when the port is
/// "frame"), plus " · Monitor n+1" for krdp's port 3400 + n.
fn tag(f: &fontdue::Font, host: &str, port: &str) -> Vec<u8> {
    let num: Option<u32> = port.parse().ok();
    let name = if num.is_some() { host_name(host) } else { host.to_owned() };
    let mon = num.filter(|p| (3400..3500).contains(p)).map(|p| p - 3400 + 1);
    let gap = 20.0;
    let second = mon.map(|m| format!("Monitor {m}")).unwrap_or_default();
    let end = 4.0 + width(f, &name);
    let w = (end + if mon.is_some() { 2.0 * gap + width(f, &second) } else { 0.0 } + 8.0) as usize;
    let mut layers = [vec![0u8; w * H], vec![0u8; w * H], vec![0u8; w * H]];
    text(&mut layers[0], w, f, 4.0, &name);
    if mon.is_some() {
        let (cx, cy, r) = (end + gap, (BASE - 12) as f32, 4.0f32); // middle of the x-height
        for y in 0..H {
            for x in 0..w {
                if (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2) <= r * r {
                    layers[2][y * w + x] = 255;
                }
            }
        }
        text(&mut layers[1], w, f, cx + gap, &second);
    }
    mask([&layers[0], &layers[1], &layers[2]], w, 32) // the padding grab.rs's legend() expects
}

/// Centres each character in a cell as wide as the widest one, so the clock doesn't jiggle as
/// digits change. R only.
fn glyphs(f: &fontdue::Font, chars: &str) -> Vec<u8> {
    let cell = chars.chars().map(|c| f.metrics(c, SIZE).advance_width).fold(0.0, f32::max) as usize + 6;
    let w = cell * chars.chars().count();
    let mut ink = vec![0u8; w * H];
    for (k, c) in chars.chars().enumerate() {
        let adv = f.metrics(c, SIZE).advance_width;
        text(&mut ink, w, f, (k * cell) as f32 + (cell as f32 - adv) / 2.0, &c.to_string());
    }
    let none = vec![0u8; w * H];
    mask([&ink, &none, &none], w, 0)
}

/// A dot, because it reads as a solid object over any content. It's a Starlight disc
/// with a Deep Space rim, nearly opaque, 4x4 supersampled for a soft edge. RGBA, size x size.
fn cursor(size: usize) -> Vec<u8> {
    const STARLIGHT: [f32; 3] = [232.0, 236.0, 255.0];
    const DEEP_SPACE: [f32; 3] = [5.0, 6.0, 15.0];
    let c = size as f32 / 2.0;
    let (outer, rim) = (c * 0.84, size as f32 * 0.1);
    let mut px = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let (mut col, mut cov) = ([0.0f32; 3], 0.0f32);
            for s in 0..16 {
                let (sx, sy) = (x as f32 + (s % 4) as f32 / 4.0 + 0.125, y as f32 + (s / 4) as f32 / 4.0 + 0.125);
                let d = ((sx - c).powi(2) + (sy - c).powi(2)).sqrt();
                if d <= outer {
                    let from = if d > outer - rim { DEEP_SPACE } else { STARLIGHT };
                    (0..3).for_each(|k| col[k] += from[k]);
                    cov += 1.0;
                }
            }
            if cov > 0.0 {
                let o = (y * size + x) * 4;
                px[o..o + 4].copy_from_slice(&[(col[0] / cov) as u8, (col[1] / cov) as u8, (col[2] / cov) as u8, (235.0 * cov / 16.0) as u8]);
            }
        }
    }
    px
}

/// An RGBA PNG with a stored (uncompressed) zlib stream. It's 16 KB, written once, so
/// compression isn't worth the code.
fn png(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        !data.iter().fold(!0u32, |c, &b| (0..8).fold(c ^ b as u32, |c, _| if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 }))
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let body = [kind, data].concat();
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    }
    let raw: Vec<u8> = rgba.chunks(w * 4).flat_map(|row| std::iter::once(0).chain(row.iter().copied())).collect();
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend((b.len() as u16).to_le_bytes());
        z.extend((!(b.len() as u16)).to_le_bytes());
        z.extend(*b);
    }
    let (a, b) = raw.iter().fold((1u32, 0u32), |(a, b), &x| ((a + x as u32) % 65521, (b + (a + x as u32) % 65521) % 65521));
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend((w as u32).to_be_bytes());
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn write(path: &str, data: &[u8]) {
    let tmp = format!("{path}.new");
    if std::fs::File::create(&tmp).and_then(|mut f| f.write_all(data)).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Does what assets.py's command line did. It writes the cursor and glyphs into dir when they're
/// missing (cc-panels deletes glyphs.rgba when the font changes), then each
/// `tag=<name>,<host or text>,<port or frame>,...`.
pub fn draw(dir: &str, family: &str, tags: &[String]) {
    let _ = std::fs::create_dir_all(dir);
    if !std::path::Path::new(&format!("{dir}/cursor.png")).exists() {
        write(&format!("{dir}/cursor.png"), &png(64, 64, &cursor(64)));
    }
    let Some(f) = font(family) else { return eprintln!("assets: no font for {family}") };
    if !std::path::Path::new(&format!("{dir}/glyphs.rgba")).exists() {
        write(&format!("{dir}/glyphs.rgba"), &glyphs(&f, crate::taskbar::GLYPHS));
    }
    if !std::path::Path::new(&format!("{dir}/ascii.rgba")).exists() {
        let ascii: String = (32u8..127).map(char::from).collect(); // machines.rs expects this exact order
        if let Some(mono) = font("monospace") {
            write(&format!("{dir}/ascii.rgba"), &glyphs(&mono, &ascii));
        }
    }
    for t in tags {
        let mut parts = t.strip_prefix("tag=").unwrap_or(t).split(',');
        if let (Some(name), Some(host), Some(port)) = (parts.next(), parts.next(), parts.next()) {
            write(&format!("{dir}/tag-{name}.rgba"), &tag(&f, host, port));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_well_formed() {
        let p = png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(crc_of_iend(&p), 0xAE42_6082); // the standard IEND CRC
    }

    fn crc_of_iend(p: &[u8]) -> u32 {
        u32::from_be_bytes(p[p.len() - 4..].try_into().unwrap())
    }

    #[test]
    fn tag_masks() {
        let Some(f) = font("Noto Sans") else { return }; // this box has no fonts
        let t = tag(&f, "desktop", "3401");
        let (w, h) = (u32::from_le_bytes(t[0..4].try_into().unwrap()) as usize, u32::from_le_bytes(t[4..8].try_into().unwrap()) as usize);
        assert_eq!((h, t.len()), (H + 64, 8 + w * h * 4));
        let px = &t[8..];
        let any = |k: usize| px.chunks(4).any(|p| p[k] > 0);
        assert!(any(0) && any(1) && any(2), "name, Monitor 2 and the dot");
        assert!(px.chunks(4).all(|p| p[3] == p[0].max(p[1]).max(p[2])));
        let g = glyphs(&f, crate::taskbar::GLYPHS);
        assert_eq!(u32::from_le_bytes(g[4..8].try_into().unwrap()) as usize, H);
    }
}

#[cfg(test)]
#[test]
#[ignore] // to check by eye: CC_ASSETS_OUT=<dir> cargo test -p cc-panels assets_draw -- --ignored
fn assets_draw() {
    let out = std::env::var("CC_ASSETS_OUT").unwrap();
    draw(&out, "Noto Sans", &["tag=desk-wide,198.51.100.10,3401,125,249,255".into(), "tag=app-org.kde.konsole,Konsole,frame,164,139,255".into()]);
}
