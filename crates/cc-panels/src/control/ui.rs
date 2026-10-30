//! The drawing kit the Machines and Preferences windows share: a Breeze window's chrome,
//! buttons, switches and segmented buttons, tags and typed text, all in units of a Breeze logical
//! pixel (the taskbar's). It also decides where a window opens and uploads its texture.
//!
//! It's free functions, not a framework. Each window keeps its own hits, key and rows.
use crate::geometry::{Pose, STAND_IN_HEAD};
use crate::grab::{self, Look, Paint, TagImg, average, line, over, rbox, tint};
use crate::overlay::Overlays;
use crate::poll::Pointing;
use crate::taskbar::{self, LABEL, TAG_ROOM, TAG_TEXT, text_pixel};
use crate::{call, config, vr};
use openvr_sys as sys;
use std::sync::mpsc;

/// ascii.rgba's characters (assets.rs, `ascii`) in file order, which is printable ASCII.
pub const ASCII: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";

pub const PAD: f64 = 12.0;
pub const TITLE: f64 = 40.0; // the title bar
pub const ROW: f64 = 44.0;
pub const BTN_H: f64 = 28.0;
pub const GAP: f64 = 8.0;
const RADIUS: f64 = 8.0;
pub const SCALE: f64 = 1.5; // texture pixels per unit
pub const MPP: f64 = 0.0006; // calibration knob: metres per unit (matches the taskbar's text size at 0.6 m)
pub const SORT: u32 = 193; // over the taskbar (190, Plasma's 191) and its card (192), under the scan's HUD (195/196)

pub type Px = ([f64; 3], f64);

/// Button k's span (x0, x1), for buttons of widths b right-aligned to x1 and GAP apart.
pub fn right(x1: f64, b: &[f64], k: usize) -> (f64, f64) {
    let x1 = x1 - b[k + 1..].iter().map(|b| b + GAP).sum::<f64>();
    (x1 - b[k], x1)
}

/// Draws a tag as text at the taskbar's LABEL scale, starting at `left` with its middle at cy.
pub fn text(img: Option<&TagImg>, x: f64, y: f64, left: f64, cy: f64) -> Px {
    let Some(img) = img else { return ([0.0; 3], 0.0) };
    let (ox, oy) = (left - TAG_TEXT * LABEL, cy - img.1 as f64 * LABEL / 2.0);
    let (sx, sy, half) = ((x - ox) / LABEL, (y - oy) / LABEL, 0.5 / LABEL);
    if sx < 0.0 || sy < 0.0 || sx > img.0 as f64 || sy > img.1 as f64 {
        return ([0.0; 3], 0.0);
    }
    average(img, sx - half, sy - half, sx + half, sy + half)
}

pub fn text_w(img: Option<&TagImg>) -> f64 {
    img.map_or(0.0, |i| (i.0 as f64 - TAG_ROOM) * LABEL)
}

/// Draws typed text s from ascii.rgba's characters, starting at `left` with its middle at cy
/// (placed like text()'s tags).
pub fn typed(g: Option<&TagImg>, s: &str, x: f64, y: f64, left: f64, cy: f64) -> Px {
    text_pixel(g, ASCII, s, x - left, y - cy + taskbar::ROW / 2.0, typed_w(g, s))
}

pub fn typed_w(g: Option<&TagImg>, s: &str) -> f64 {
    g.map_or(0.0, |g| g.0 as f64 / ASCII.len() as f64 * LABEL * s.chars().count() as f64)
}

/// A Breeze window w × h at (x, y) from its top left: the surface, a title bar in the frame
/// colour with its title, and the line under it. Also returns the distance to its edge for
/// `border`, which is drawn last. None means the point is outside it.
pub fn chrome(p: &Paint, title: Option<&TagImg>, w: f64, h: f64, x: f64, y: f64) -> Option<(Px, f64)> {
    let (t, acc) = (&p.t, &p.acc);
    let d = rbox(x - w / 2.0, y - h / 2.0, w / 2.0 - 1.0, h / 2.0 - 1.0, RADIUS);
    let inside = (0.5 - d).clamp(0.0, 1.0);
    if inside <= 0.0 && d > 1.5 {
        return None;
    }
    let mut out = (t.surface, inside);
    if y < TITLE + 1.0 {
        out = over(out, (t.frame, (TITLE - y + 0.5).clamp(0.0, 1.0) * inside));
        out = over(out, tint(text(title, x, y, PAD + 4.0, TITLE / 2.0), t.text, t.dim, acc.line));
    }
    Some((over(out, (t.border, line(y - TITLE, 0.5) * inside)), d))
}

/// The window's 1 u border with rounded corners, drawn over everything.
pub fn border(p: &Paint, out: Px, d: f64) -> Px {
    over(out, (p.t.border, line(d + 0.5, 0.5)))
}

/// A button's body over (x0, x1), with its middle at cy.
pub fn button(p: &Paint, look: Look, (x0, x1): (f64, f64), x: f64, y: f64, cy: f64) -> Px {
    let d = rbox(x - (x0 + x1) / 2.0, y - cy, (x1 - x0) / 2.0, BTN_H / 2.0, 4.0);
    grab::button(&p.t, &p.acc, look, d, 6.0, 1.0)
}

/// A plain button over (x0, x1) with its label centred. It lights up under a laser.
pub fn labelled(p: &Paint, lit: bool, (x0, x1): (f64, f64), cy: f64, label: Option<&TagImg>, x: f64, y: f64) -> Px {
    if x <= x0 - 7.0 || x >= x1 + 7.0 {
        return ([0.0; 3], 0.0);
    }
    let t = &p.t;
    let out = button(p, if lit { Look::Lit } else { Look::Rest }, (x0, x1), x, y, cy);
    over(out, tint(text(label, x, y, (x0 + x1 - text_w(label)) / 2.0, cy), t.wtext, t.wtext, t.wtext))
}

/// A switch's track from sx with its middle at cy. While it's on, the knob sits right and the
/// track takes the accent.
pub fn switch(p: &Paint, out: Px, x: f64, y: f64, sx: f64, cy: f64, on: bool) -> Px {
    let (t, acc) = (&p.t, &p.acc);
    let track = rbox(x - sx - 11.0, y - cy, 11.0, 7.0, 7.0);
    let out = over(out, if on { (acc.line, (0.5 - track).clamp(0.0, 1.0)) } else { (t.border, line(track + 0.5, 0.5)) });
    let knob = (x - sx - if on { 18.0 } else { 4.0 }).hypot(y - cy) - 4.5;
    over(out, (if on { acc.ink } else { t.wdim }, (0.5 - knob).clamp(0.0, 1.0)))
}

/// Which of n segments over (x0, x1) is at x, if any.
pub fn segment(n: usize, (x0, x1): (f64, f64), x: f64) -> Option<usize> {
    (x0..=x1).contains(&x).then(|| (((x - x0) / (x1 - x0) * n as f64) as usize).min(n - 1))
}

/// A segmented button over (x0, x1), one segment per label. The chosen one is solid in the
/// accent, and the one under a laser lights up (or gets a ring, if it's the chosen one).
pub fn segmented(p: &Paint, labels: &[Option<&TagImg>], chosen: usize, hover: Option<usize>, (x0, x1): (f64, f64), cy: f64, x: f64, y: f64) -> Px {
    if x <= x0 - 7.0 || x >= x1 + 7.0 {
        return ([0.0; 3], 0.0);
    }
    let (t, acc) = (&p.t, &p.acc);
    let mut out = button(p, Look::Rest, (x0, x1), x, y, cy);
    let sw = (x1 - x0) / labels.len() as f64;
    for (k, l) in labels.iter().enumerate() {
        let (s0, s1) = (x0 + k as f64 * sw, x0 + (k + 1) as f64 * sw);
        let look = if k == chosen {
            Look::Carry
        } else if hover == Some(k) {
            Look::Lit
        } else {
            Look::Rest
        };
        if look != Look::Rest {
            let d = rbox(x - (s0 + s1) / 2.0, y - cy, sw / 2.0 - 2.0, BTN_H / 2.0 - 2.0, 3.0);
            out = over(out, grab::button(t, acc, look, d, 2.0, 1.0)); // keeps the glow inside the segment's edge
            if k == chosen && hover == Some(k) {
                out = over(out, (acc.ink, line(d + 2.5, 0.75))); // lit: a ring just inside it
            }
        }
        if x > s0 && x < s1 {
            let ink = if k == chosen { acc.ink } else { t.wtext };
            out = over(out, tint(text(*l, x, y, (s0 + s1 - text_w(*l)) / 2.0, cy), ink, ink, ink));
        }
    }
    out
}

/// A physical key pressed in a focused text field (kvm.rs focus_field, field_keys).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edit {
    Char(char),
    Back,
    Next,   // Tab
    Submit, // Enter
    Leave,  // Escape
}

/// evdev codes 2 (1) to 53 (/) on a US keyboard, unshifted and shifted. 0 means it isn't a
/// character.
const US: [&[u8; 52]; 2] = [b"1234567890-=\0\0qwertyuiop[]\0\0asdfghjkl;'`\0\\zxcvbnm,./", b"!@#$%^&*()_+\0\0QWERTYUIOP{}\0\0ASDFGHJKL:\"~\0|ZXCVBNM<>?"];

/// Turns an evdev key (value 1 is down, 2 is held, i.e. its repeat) into a field edit.
/// Characters and Backspace repeat.
pub fn edit(code: u16, value: i32, shift: bool) -> Option<Edit> {
    if value == 0 {
        return None;
    }
    let c = match code {
        57 => b' ',
        2..=53 => US[shift as usize][code as usize - 2],
        _ => 0,
    };
    match code {
        _ if c != 0 => Some(Edit::Char(c as char)),
        14 => Some(Edit::Back),
        _ if value != 1 => None,
        15 => Some(Edit::Next),
        28 | 96 => Some(Edit::Submit), // plus the keypad's Enter
        1 => Some(Edit::Leave),
        _ => None,
    }
}

/// The other way: the evdev key (and Shift) that types c on a US keyboard. "\b" is Backspace,
/// "\n" Enter and "\t" Tab, as SteamVR's keyboard sends them.
pub fn key_for(c: char) -> Option<(u16, bool)> {
    match c {
        ' ' => return Some((57, false)),
        '\u{8}' => return Some((14, false)),
        '\n' | '\r' => return Some((28, false)),
        '\t' => return Some((15, false)),
        _ => {}
    }
    let b = u8::try_from(c).ok().filter(|b| *b != 0)?;
    (0..2).find_map(|shift| US[shift].iter().position(|&k| k == b).map(|i| (i as u16 + 2, shift == 1)))
}

pub type Rect = (f64, f64, f64, f64); // x0, y0, x1, y1 in units from the top left

/// Redraws rect (x0, y0, x1, y1) in buf (tw x th at s texture pixels per unit), pixel by pixel.
pub fn patch(buf: &mut [u8], tw: usize, th: usize, s: f64, (x0, y0, x1, y1): Rect, pixel: impl Fn(f64, f64) -> Px) {
    let lo = |u: f64| (u * s).max(0.0) as usize;
    let hi = |u: f64, n: usize| ((u * s).ceil() as usize).min(n);
    for y in lo(y0)..hi(y1, th) {
        for x in lo(x0)..hi(x1, tw) {
            let (c, a) = pixel((x as f64 + 0.5) / s, (y as f64 + 0.5) / s);
            buf[(y * tw + x) * 4..][..4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, (a.clamp(0.0, 1.0) * 255.0) as u8]);
        }
    }
}

/// Where a window `name` w metres wide opens: where it was last put down in this room
/// (spots.home.<name> of this room's workspace), else in front of you (`fresh`).
pub fn spot(name: &str, w: f64) -> Pose {
    config::home_pose(name, None).unwrap_or_else(|| fresh(&vr::head().unwrap_or(STAND_IN_HEAD), w))
}

/// Where every new surface opens (geometry.rs `new_pose`: 1.4 m ahead, level, facing you), w metres wide.
fn fresh(head: &crate::geometry::Mat, w: f64) -> Pose {
    crate::geometry::new_pose(head, w, 0.0, 0.0)
}

/// A window's texture: px (tw x th) for a w x ht window (`size`). `full` means it was drawn whole.
pub struct Tex {
    pub px: Vec<u8>,
    pub tw: usize,
    pub th: usize,
    pub size: (f64, f64),
    pub full: bool,
}

/// Takes the last texture px (tw x th at s texture pixels per unit) and redraws just the rects
/// that changed. It draws the whole thing when they're None or px isn't that size.
pub fn redraw(mut px: Vec<u8>, (tw, th): (usize, usize), s: f64, size: (f64, f64), dirty: Option<Vec<Rect>>, pixel: impl Fn(f64, f64) -> Px) -> Tex {
    let dirty = dirty.filter(|_| px.len() == tw * th * 4);
    let full = dirty.is_none();
    match dirty {
        Some(rects) => rects.into_iter().for_each(|r| patch(&mut px, tw, th, s, r, &pixel)),
        None => px = grab::draw(tw, th, |x, y| pixel(x / s, y / s)),
    }
    Tex { px, tw, th, size, full }
}

/// Runs a window's draws off the main thread, because a whole one is 80-360 ms of pixels and a
/// hover's patch 10-40, and the main loop (lasers, panels) would drop those frames. It does one
/// at a time, then the latest key, and uploads each when it's done.
pub struct Painter<K> {
    job: Option<mpsc::Receiver<(K, Tex)>>,
    drawn: Option<K>,
    px: Vec<u8>, // the texture as uploaded (empty if none or refused); the next draw patches it
}

impl<K> Default for Painter<K> {
    fn default() -> Self {
        Painter { job: None, drawn: None, px: Vec::new() }
    }
}

impl<K: Clone + PartialEq + Send + 'static> Painter<K> {
    /// The key that was last uploaded.
    pub fn drawn(&self) -> Option<&K> {
        self.drawn.as_ref()
    }

    /// Uploads a finished draw to h, then starts drawing key unless it's already drawn or a draw
    /// is under way. make() gives the draw (from the last key drawn and its texture, empty if
    /// none) and is only called when one actually starts.
    pub fn paint<D: FnOnce(Option<K>, Vec<u8>, &K) -> Tex + Send + 'static>(&mut self, h: vr::Handle, key: K, make: impl FnOnce() -> D) {
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok((k, mut t)) => {
                    upload(h, &mut t.px, t.tw, t.th, t.size, t.full);
                    (self.job, self.drawn, self.px) = (None, Some(k), t.px); // even if refused: it's tried again on the next change
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    crate::trace::flush();
                    std::process::exit(101) // it panicked (and logged it), so exit like the cards' painter does
                }
            }
        }
        if self.drawn.as_ref() == Some(&key) {
            return;
        }
        let (draw, was, px) = (make(), self.drawn.clone(), std::mem::take(&mut self.px));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let t = draw(was, px, &key);
            if tx.send((key, t)).is_ok() {
                vr::wake(); // the main loop may be idling, so wake it to upload now
            }
        });
        self.job = Some(rx);
    }
}

/// Uploads texture px (tw x th) to h, a w x ht window. When `full` (drawn whole), it also sets
/// the mouse scale so events come in the window's units. If SteamVR refuses it, px is emptied so
/// it gets drawn whole next time.
pub fn upload(h: vr::Handle, px: &mut Vec<u8>, tw: usize, th: usize, (w, ht): (f64, f64), full: bool) {
    if !grab::set_raw(h, px, tw, th) {
        px.clear();
    } else if full {
        crate::overlay::Live.mouse_scale(h, [w as f32, ht as f32]);
    }
}

/// The next event on the Extra's window h. SteamVR's come first. Then, with no laser (both
/// controllers on), the mouse's events from kvm's ray, shaped like a laser's would be (a move
/// when its point changes, a leave, its left presses) and from Dev::Mouse. None when there are
/// none.
pub fn next_event(h: vr::Handle) -> Option<crate::events::SurfaceEvent> {
    use crate::events::{Button, PointerEvent, PointerKind, SurfaceEvent};
    use std::sync::Mutex;
    static SENT: Mutex<Option<(f32, f32)>> = Mutex::new(None); // the point we last moved to
    if let Some(ev) = crate::poll::Live.next(h) {
        return Some(ev);
    }
    let mut sent = SENT.lock().unwrap();
    if crate::laser::HEALTHY.load(std::sync::atomic::Ordering::Relaxed) {
        *sent = None; // the laser sends its own events now
        return None;
    }
    let mut k = crate::kvm::lock();
    let mut scale = sys::HmdVector2_t { v: [0.0; 2] };
    call!(ov, GetOverlayMouseScale, h.0, &mut scale);
    let at = k.extra.zip(k.extra_at()).map(|((pl, _), (u, v))| (((u / pl.width + 0.5) * scale.v[0] as f64) as f32, ((v / pl.height + 0.5) * scale.v[1] as f64) as f32));
    let (kind, (x, y)) = if !k.extra_mouse.is_empty() {
        let down = k.extra_mouse.remove(0);
        let kind = if down { PointerKind::Down(Button::Left) } else { PointerKind::Up(Button::Left) };
        (kind, at.or(*sent).unwrap_or_default())
    } else if at.is_some() && at != *sent {
        (PointerKind::Move, at.unwrap())
    } else if at.is_none() && sent.is_some() {
        (PointerKind::Leave, sent.unwrap())
    } else {
        return None;
    };
    *sent = at;
    Some(SurfaceEvent::Pointer(PointerEvent { kind, x, y, from: crate::ids::Dev::Mouse, age: 0.0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn a_window_with_no_spot_here_opens_14_m_ahead_level_and_facing_you() {
        let head: crate::geometry::Mat = [[0.0, 0.0, 1.0, 0.4], [0.0, 1.0, 0.0, 1.6], [-1.0, 0.0, 0.0, -0.3]]; // facing -x
        let p = fresh(&head, 0.36);
        let eye = [0, 1, 2].map(|i| head[i][3] as f64);
        let to = [p.centre[0] - eye[0], p.centre[1] - eye[1], p.centre[2] - eye[2]];
        assert!((crate::geometry::norm(&to) - crate::limits::NEW_AHEAD).abs() < 1e-6 && to[1].abs() < 1e-6 && p.pitch.abs() < 1e-6, "{to:?}");
        assert!((p.yaw - crate::geometry::angles(&to).0).abs() < 1e-6 && p.width == 0.36);
    }

    #[test]
    fn every_printable_ascii_types_back_as_itself() {
        for c in ' '..='~' {
            let (code, shift) = key_for(c).unwrap_or_else(|| panic!("{c:?} has no key"));
            assert_eq!(edit(code, 1, shift), Some(Edit::Char(c)), "{c:?}");
        }
        assert_eq!(key_for('\u{8}').map(|(k, s)| edit(k, 1, s)), Some(Some(Edit::Back)));
        assert_eq!(key_for('\n').map(|(k, s)| edit(k, 1, s)), Some(Some(Edit::Submit)));
        assert_eq!(key_for('é'), None);
    }

    #[test]
    fn typed_text_sits_left_a_cell_a_character() {
        assert_eq!(ASCII, (32u8..127).map(|b| b as char).collect::<String>(), "assets.py's ASCII");
        // an atlas of 10 px cells with only 'A' inked
        let (cw, h) = (10usize, 64usize);
        let mut px = vec![0u8; ASCII.len() * cw * h * 4];
        let a = ASCII.find('A').unwrap();
        for y in 0..h {
            for x in a * cw..(a + 1) * cw {
                px[(y * ASCII.len() * cw + x) * 4..][..4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let g = (ASCII.len() * cw, h, px);
        let cell = cw as f64 * LABEL; // 3 units
        assert_eq!(typed_w(Some(&g), "bA"), 2.0 * cell);
        assert!(typed(Some(&g), "bA", 100.0 + cell * 1.5, 50.0, 100.0, 50.0).1 > 0.9, "A, the second");
        assert_eq!(typed(Some(&g), "bA", 100.0 + cell * 0.5, 50.0, 100.0, 50.0).1, 0.0, "b, uninked");
        assert_eq!(typed(Some(&g), "bA", 100.0 + cell * 2.5, 50.0, 100.0, 50.0).1, 0.0, "past the end");
        assert_eq!(typed(Some(&g), "bA", 100.0 + cell * 1.5, 50.0 + h as f64 * LABEL, 100.0, 50.0).1, 0.0, "under the line");
        assert_eq!(typed(Some(&g), "é", 100.5, 50.0, 100.0, 50.0).1, 0.0, "not in the atlas: nothing");
    }

    #[test]
    fn a_us_keyboard_types_ascii() {
        let typed = |keys: &[(u16, bool)]| {
            keys.iter()
                .filter_map(|&(c, sh)| match edit(c, 1, sh) {
                    Some(Edit::Char(c)) => Some(c),
                    _ => None,
                })
                .collect::<String>()
        };
        assert_eq!(typed(&[(22, false), (31, false), (18, false), (19, false), (3, true), (35, false), (24, false), (31, false), (20, false), (52, false), (2, false)]), "user@host.1");
        assert_eq!(
            typed(&[(2, false), (11, false), (2, true), (11, true), (12, true), (13, false), (39, true), (40, true), (43, false), (43, true), (41, true), (53, true), (57, true)]),
            "10!)_=:\"\\|~? "
        );
        assert_eq!(typed(&[(16, true), (44, false), (50, true)]), "QzM");
        assert_eq!(edit(30, 2, false), Some(Edit::Char('a')), "held: repeats");
        assert_eq!(edit(30, 0, false), None, "up: nothing");
        assert_eq!(edit(14, 2, false), Some(Edit::Back));
        assert_eq!((edit(15, 1, false), edit(28, 1, false), edit(96, 1, false), edit(1, 1, false)), (Some(Edit::Next), Some(Edit::Submit), Some(Edit::Submit), Some(Edit::Leave)));
        assert_eq!((edit(15, 2, false), edit(28, 2, false), edit(1, 2, false)), (None, None, None), "only characters and Backspace repeat");
        assert_eq!((edit(42, 1, false), edit(29, 1, false), edit(103, 1, false), edit(59, 1, false)), (None, None, None, None), "Shift, Ctrl, Up, F1");
    }

    #[test]
    fn segments_split_their_span_and_show_the_chosen_one() {
        let span = (100.0, 400.0);
        assert_eq!(segment(3, span, 101.0), Some(0));
        assert_eq!(segment(3, span, 250.0), Some(1));
        assert_eq!(segment(3, span, 400.0), Some(2), "its right end");
        assert_eq!(segment(3, span, 99.0), None);
        let p = Paint::new(&theme::Theme::default(), theme::VIOLET);
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        let at = |chosen: usize, hover: Option<usize>, x: f64| segmented(&p, &[None, None, None], chosen, hover, span, 50.0, x, 50.0).0;
        assert!(near(at(1, None, 250.0), p.acc.line), "the chosen one: the accent");
        assert!(near(at(1, None, 150.0), p.t.raised), "the others at rest");
        assert!(!near(at(1, Some(0), 150.0), p.t.raised) && !near(at(1, Some(0), 150.0), p.acc.line), "hovered: lit");
        assert!(!near(at(1, Some(1), 204.5), at(1, None, 204.5)), "the chosen one hovered: ringed");
        assert_eq!(segmented(&p, &[None], 0, None, span, 50.0, 50.0, 50.0).1, 0.0, "away from it: nothing");
    }
}
