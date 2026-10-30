//! SteamVR's keyboard, for typing to a remote from a controller. The keyboard button on a live
//! panel's card (grab.rs `Part::Keys`) opens it for that panel in minimal mode, so each character
//! comes the moment it's typed (VREvent_KeyboardCharInput), and kvm.rs `type_text` sends it on: as
//! the character itself where the panel takes that (VNC keysyms, RDP Unicode input), so the host's
//! layout can't change it, else as a US keyboard's keys.
//!
//! While it's open, a key strip of ours sits under the panel's card with the keys it lacks: Esc,
//! Tab, the arrows, and sticky Ctrl, Alt and Super that apply to the next key or laser click
//! (kvm.rs `tap`, `click_mods`). It also says when a character couldn't be typed, without ever
//! saying which, since it may be a password, and, before you type, when the panel only takes a US
//! keyboard's keys. Characters go as themselves where they can (kvm.rs `route`).
//!
//! A physical key pressed while it's open closes it, since you've gone back to the real one. So
//! does the panel disconnecting, and a dropped session lets go of anything held. Closing lets go of every modifier we pressed. The Machines and
//! Workspace windows open the same keyboard for their own fields and call `forget` first, so a
//! physical key never closes theirs by mistake.
use crate::control::ui::{self, BTN_H, GAP, MPP, PAD, Px, SORT};
use crate::events::{KeyboardEvent, PointerEvent, PointerKind, SurfaceEvent};
use crate::grab::{Look, Paint, TagImg, line, over, rbox, tint};
use crate::{Panel, call, panel, theme, vr};
use openvr_sys as sys;
use std::ffi::CString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// SteamVR hands this back with every keyboard event, plus the panel's number, which tells a
/// panel's keyboard apart from a Command Center window's.
const USER: u64 = 0x4343_4b42 << 32; // "CCKB"

/// The panel the keyboard is open for, and when it opened.
static OPEN: Mutex<Option<(usize, Instant)>> = Mutex::new(None);
/// Characters it couldn't type lately, and when the last one was, for the strip's line.
static MISSED: Mutex<(usize, Option<Instant>)> = Mutex::new((0, None));
const MISSED_FOR: Duration = Duration::from_secs(5); // how long the strip says so

/// n characters couldn't be typed: the strip says so for MISSED_FOR. Any thread (a VNC session's
/// send that failed counts too).
pub fn missed(n: usize) {
    let mut m = crate::ffi::lock(&MISSED);
    *m = (if m.1.is_some_and(|t| t.elapsed() < MISSED_FOR) { m.0 } else { 0 } + n, Some(Instant::now()));
}

/// Whether the panel keyboard is open for panel i.
pub fn open_for(i: usize) -> bool {
    OPEN.lock().unwrap().is_some_and(|(j, _)| j == i)
}

/// A physical key went down after it opened (`at`). A key before it opened doesn't count: you
/// typed on the real keyboard, then asked for this one.
fn real_key_since(key_at: Option<Instant>, at: Instant) -> bool {
    key_at.is_some_and(|k| k > at)
}

/// It closed, however it did: no sticky modifier left, none held down.
fn closed() {
    if OPEN.lock().unwrap().take().is_some() {
        let mut k = crate::kvm::lock();
        k.latched = [false; 3];
        k.release_synth();
    }
}

fn show(p: &Panel) {
    // said before you type, not after: a panel that can't take characters gets a US keyboard's keys
    let us = if p.text_mode() == crate::kvm::TextMode::UsKeys { " as a US keyboard" } else { "" };
    let hint = CString::new(format!("Typing to {}{us}", p.v.name)).unwrap_or_default();
    let mode = sys::EGamepadTextInputMode_k_EGamepadTextInputModeNormal;
    let lines = sys::EGamepadTextInputLineMode_k_EGamepadTextInputLineModeSingleLine;
    let flags = sys::EKeyboardFlags_KeyboardFlag_Minimal;
    if !crate::kvm::lock().popup_ok(true) {
        return; // never: you asked (here for the record, since the fields ask with false)
    }
    let e = call!(ov, ShowKeyboardForOverlay, p.overlay.0, mode, lines, flags, hint.as_ptr() as *mut _, 1024, c"".as_ptr() as *mut _, USER | p.index as u64);
    if e == 0 {
        closed(); // another panel's, if it was open there
        *OPEN.lock().unwrap() = Some((p.index, Instant::now()));
        crate::kvm::lock().type_to(p.index);
        eprintln!("{}: SteamVR keyboard open", p.v.name);
    } else {
        eprintln!("{}: no SteamVR keyboard ({})", p.v.name, vr::error_name(e));
    }
}

/// A window opened the keyboard for its own field, so it isn't the panel's anymore.
pub fn forget() {
    closed();
}

/// Handles e if it's the panel keyboard's, from p's overlay: true if it was. A laser click on the
/// panel while it's open carries the sticky modifiers (false: the click goes on as usual), and the
/// laser leaving the panel lets go of any it held.
pub fn event(p: &Panel, e: &SurfaceEvent) -> bool {
    let ours = |user: &u64| *user == USER | p.index as u64;
    match e {
        SurfaceEvent::Keyboard(KeyboardEvent::Char { user, text }) if ours(user) => {
            if let Some(text) = text {
                let n = crate::kvm::lock().type_text(p.index, text);
                if n > 0 {
                    missed(n);
                    eprintln!("{}: {n} character(s) from SteamVR's keyboard couldn't be typed", p.v.name);
                }
            }
        }
        SurfaceEvent::Keyboard(KeyboardEvent::Done { user } | KeyboardEvent::Closed { user }) if ours(user) => {
            if open_for(p.index) {
                closed();
            }
        }
        SurfaceEvent::Pointer(PointerEvent { kind: k @ (PointerKind::Down(_) | PointerKind::Up(_)), .. }) if open_for(p.index) => {
            crate::kvm::lock().click_mods(p.index, matches!(k, PointerKind::Down(_)));
            return false;
        }
        SurfaceEvent::Pointer(PointerEvent { kind: PointerKind::Leave, .. }) if open_for(p.index) => {
            crate::kvm::lock().release_synth();
            return false;
        }
        _ => return false,
    }
    true
}

// ------------------------------------------------------------------ the key strip

/// The strip's buttons, left to right: what's on them, and the key they send. The last three
/// are the sticky modifiers (kvm.rs MODS), 0 here.
const KEYS: [(&str, u16); 9] = [("Esc", 1), ("Tab", 15), ("<", 105), ("^", 103), ("v", 108), (">", 106), ("Ctrl", 0), ("Alt", 0), ("Super", 0)];
const BTN: f64 = 52.0; // a button's width, in ui.rs units
const W: f64 = 2.0 * PAD + KEYS.len() as f64 * BTN + (KEYS.len() - 1) as f64 * GAP;
const CY: f64 = PAD + BTN_H / 2.0; // the buttons' middle
const LINE_Y: f64 = CY + BTN_H / 2.0 + 14.0; // the line about characters it couldn't type
const H: f64 = LINE_Y + 10.0 + PAD;

fn span(b: usize) -> (f64, f64) {
    let x0 = PAD + b as f64 * (BTN + GAP);
    (x0, x0 + BTN)
}

/// The button at (x, y), in units from the top left.
fn hit(x: f64, y: f64) -> Option<usize> {
    ((y - CY).abs() <= BTN_H / 2.0).then(|| (0..KEYS.len()).find(|&b| (span(b).0..=span(b).1).contains(&x))).flatten()
}

/// What it's drawn from.
#[derive(Clone, PartialEq, Debug)]
struct Key {
    hover: Option<usize>,
    latched: [bool; 3],
    missed: usize,
    us: bool, // the panel only takes a US keyboard's keys (Panel::text_mode)
    theme: u32,
}

fn pixel(k: &Key, p: &Paint, ascii: Option<&TagImg>, x: f64, y: f64) -> Px {
    let t = &p.t;
    let d = rbox(x - W / 2.0, y - H / 2.0, W / 2.0 - 1.0, H / 2.0 - 1.0, 8.0);
    let inside = (0.5 - d).clamp(0.0, 1.0);
    if inside <= 0.0 && d > 1.5 {
        return ([0.0; 3], 0.0);
    }
    let mut out = (t.surface, inside);
    for (b, (label, _)) in KEYS.iter().enumerate() {
        let (x0, x1) = span(b);
        if x < x0 - 7.0 || x > x1 + 7.0 {
            continue;
        }
        let sticky = b >= KEYS.len() - 3 && k.latched[b + 3 - KEYS.len()];
        let look = if sticky {
            Look::Carry
        } else if k.hover == Some(b) {
            Look::Lit
        } else {
            Look::Rest
        };
        out = over(out, ui::button(p, look, (x0, x1), x, y, CY));
        let ink = if sticky { p.acc.ink } else { t.wtext };
        out = over(out, tint(ui::typed(ascii, label, x, y, (x0 + x1 - ui::typed_w(ascii, label)) / 2.0, CY), ink, ink, ink));
    }
    let note = match (k.missed, k.us) {
        (0, false) => None,
        (0, true) => Some("Typing as a US keyboard".to_string()),
        (n, _) => Some(format!("Couldn't type {n} character{}", if n == 1 { "" } else { "s" })),
    };
    if let Some(s) = note {
        out = over(out, tint(ui::typed(ascii, &s, x, y, PAD + 4.0, LINE_Y), t.wdim, t.wdim, t.wdim));
    }
    over(out, (t.border, line(d + 0.5, 0.5)))
}

/// The strip, made while the panel keyboard is open and gone when it closes. The main loop owns it.
pub struct Keys {
    ov: Option<(vr::Handle, usize)>, // the strip and its panel
    ascii: Arc<Option<TagImg>>,
    hover: Option<usize>,
    painter: ui::Painter<Key>,
}

impl Keys {
    pub fn new(assets: &str) -> Keys {
        Keys { ov: None, ascii: Arc::new(crate::load_tag(&format!("{assets}/ascii.rgba"))), hover: None, painter: ui::Painter::default() }
    }

    /// True under a laser, so the loop keeps the display's rate.
    pub fn moving(&self) -> bool {
        self.hover.is_some()
    }

    /// Every frame: opens the keyboard for any panel whose button was pressed, closes it when a
    /// physical key goes down after it opened or the panel's session ends, and keeps the strip
    /// under the panel it's open for.
    pub fn tick(&mut self, grab: &mut crate::grab::Grab) {
        for i in grab.take_keyboards() {
            show(panel(i));
        }
        let open = *OPEN.lock().unwrap();
        if let Some((i, at)) = open {
            let p = panel(i);
            if !p.connected.load(std::sync::atomic::Ordering::Acquire) {
                crate::kvm::lock().release_synth(); // its session dropped: nothing stays held for the next one
            }
            let why = if real_key_since(crate::kvm::lock().key_at, at) {
                Some("for the physical one")
            } else if !p.live() {
                Some("as its panel disconnected")
            } else {
                None
            };
            if let Some(why) = why {
                call!(ov, HideKeyboard);
                closed();
                eprintln!("{}: SteamVR keyboard closed {why}", p.v.name);
            }
        }
        match (*OPEN.lock().unwrap(), self.ov) {
            (Some((i, _)), Some((_, j))) if i == j => {}
            (want, _) => {
                self.close();
                if let Some((i, _)) = want {
                    self.open(i);
                }
            }
        }
        let Some((h, i)) = self.ov else { return };
        self.events(h, i);
        if let Some(pl) = crate::kvm::lock().place.get(i) {
            let drop = pl.height / 2.0 + crate::grab::card_below(pl) + H * MPP / 2.0;
            let mut m = pl.matrix();
            for (r, row) in m.iter_mut().enumerate() {
                row[3] -= (pl.y[r] * drop - pl.z[r] * 0.005) as f32; // under the card, a hair in front
            }
            vr::place(h, &m);
        }
        if crate::HIDDEN.load(std::sync::atomic::Ordering::Relaxed) {
            call!(ov, HideOverlay, h.0)
        } else {
            call!(ov, ShowOverlay, h.0)
        };
        self.paint(h, panel(i).accent);
    }

    fn open(&mut self, i: usize) {
        let h = match crate::surface::create(crate::surface::Kind::Keys, Some(i), "controlcenter.keys", "Command Center keys") {
            Ok(h) => h,
            Err(e) => return eprintln!("key strip: no overlay: {e}"),
        };
        call!(ov, SetOverlayInputMethod, h.0, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, h.0, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        call!(ov, SetOverlaySortOrder, h.0, SORT);
        call!(ov, SetOverlayWidthInMeters, h.0, (W * MPP) as f32);
        self.ov = Some((h, i));
    }

    fn close(&mut self) {
        if let Some((h, _)) = self.ov.take() {
            crate::gpu::forget(h);
            crate::surface::destroy(h);
            (self.hover, self.painter) = (None, ui::Painter::default());
        }
    }

    fn events(&mut self, h: vr::Handle, i: usize) {
        let mut e: vr::VREvent_t = Default::default();
        while call!(ov, PollNextOverlayEvent, h.0, &mut e, size_of::<vr::VREvent_t>() as u32) {
            let m = unsafe { e.data.mouse };
            let at = hit(m.x as f64, crate::geometry::from_top(H, m.y as f64)); // events count from the bottom left
            match e.eventType {
                sys::EVREventType_VREvent_MouseMove => self.hover = at,
                sys::EVREventType_VREvent_FocusLeave => self.hover = None,
                sys::EVREventType_VREvent_MouseButtonDown if m.button == sys::EVRMouseButton_VRMouseButton_Left => match at {
                    Some(b) if KEYS[b].1 != 0 => crate::kvm::lock().strip_key(i, KEYS[b].1),
                    Some(b) => {
                        let mut k = crate::kvm::lock();
                        k.latched[b + 3 - KEYS.len()] ^= true;
                    }
                    None => {}
                },
                _ => {}
            }
        }
    }

    fn paint(&mut self, h: vr::Handle, accent: theme::Rgb) {
        let missed = crate::ffi::lock(&MISSED);
        let missed = if missed.1.is_some_and(|t| t.elapsed() < MISSED_FOR) { missed.0 } else { 0 };
        let us = self.ov.is_some_and(|(_, i)| panel(i).text_mode() == crate::kvm::TextMode::UsKeys);
        let key = Key { hover: self.hover, latched: crate::kvm::lock().latched, missed, us, theme: theme::generation() };
        self.painter.paint(h, key, || {
            let (ascii, p) = (self.ascii.clone(), Paint::new(&theme::get(), accent));
            move |_: Option<Key>, px, k: &Key| {
                let s = crate::taskbar::tex_scale(W, H, ui::SCALE);
                ui::redraw(px, ((W * s) as usize, (H * s) as usize), s, (W, H), None, |x, y| pixel(k, &p, (*ascii).as_ref(), x, y))
            }
        });
    }

    pub fn destroy(&mut self) {
        self.close();
    }
}

/// The text in a keyboard event: up to 8 bytes of UTF-8, ended by a 0 if it's shorter.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vr::typed; // the translation decodes SteamVR's keyboard bytes with it

    #[test]
    fn a_real_key_before_the_button_doesnt_close_it() {
        let typed = Instant::now();
        let opened = typed + Duration::from_millis(300); // then the panel's keyboard button
        assert!(!real_key_since(Some(typed), opened), "it stays open");
        assert!(real_key_since(Some(opened + Duration::from_millis(1)), opened), "a key after it closes it");
        assert!(!real_key_since(None, opened));
    }

    #[test]
    fn the_strip_hits_each_key_where_its_drawn() {
        for b in 0..KEYS.len() {
            let (x0, x1) = span(b);
            assert_eq!(hit((x0 + x1) / 2.0, CY), Some(b));
        }
        assert_eq!(hit(span(0).1 + GAP / 2.0, CY), None, "between keys");
        assert_eq!(hit(span(0).0 + 1.0, LINE_Y), None, "the line under them");
        assert!(span(KEYS.len() - 1).1 + PAD <= W + 1e-9);
        assert_eq!(KEYS.iter().filter(|k| k.1 == 0).count(), crate::kvm::MODS.len(), "a toggle per sticky modifier");
    }

    #[test]
    fn a_keyboard_event_reads_up_to_its_zero() {
        let ev = |b: &[u8]| {
            let mut a = [0 as std::ffi::c_char; 8];
            for (d, s) in a.iter_mut().zip(b) {
                *d = *s as std::ffi::c_char;
            }
            a
        };
        assert_eq!(typed(&ev(b"a")).as_deref(), Some("a"));
        assert_eq!(typed(&ev("é".as_bytes())).as_deref(), Some("é"));
        assert_eq!(typed(&ev(b"\x08")).as_deref(), Some("\u{8}"));
        assert_eq!(typed(&ev(b"12345678")).as_deref(), Some("12345678"));
        assert_eq!(typed(&ev(b"")).as_deref(), None);
        assert_eq!(typed(&ev(&[0xC3])).as_deref(), None); // half a character
    }
}
