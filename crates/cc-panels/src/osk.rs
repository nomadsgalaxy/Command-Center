//! SteamVR's keyboard, for typing to a remote from a controller. The keyboard button on a live
//! panel's card (grab.rs `Part::Keys`) opens it for that panel in minimal mode, so each character
//! comes the moment it's typed (VREvent_KeyboardCharInput), and kvm.rs `type_text` sends it on as
//! key presses, the same way a physical keyboard's keys go. It has no Esc, arrows or Ctrl, so those
//! stay on a real keyboard.
//!
//! A physical key pressed while it's open closes it, since you've gone back to the real one. The
//! Machines and Workspace windows open the same keyboard for their own fields and call `forget`
//! first, so a physical key never closes theirs by mistake.
use crate::kvm::KVM;
use crate::{Panel, call, panel, vr};
use openvr_sys as sys;
use std::ffi::CString;
use std::sync::Mutex;
use std::time::Instant;

/// SteamVR hands this back with every keyboard event, plus the panel's number, which tells a
/// panel's keyboard apart from a Command Center window's.
const USER: u64 = 0x4343_4b42 << 32; // "CCKB"

/// The panel the keyboard is open for, and when it opened.
static OPEN: Mutex<Option<(usize, Instant)>> = Mutex::new(None);

/// Every frame: opens the keyboard for any panel whose button was pressed, and closes it when a
/// physical key goes down after it opened.
pub fn tick(grab: &mut crate::grab::Grab) {
    for i in grab.take_keyboards() {
        show(panel(i));
    }
    let mut open = OPEN.lock().unwrap();
    if let Some((i, at)) = *open
        && KVM.lock().unwrap().key_at.is_some_and(|k| k > at)
    {
        call!(ov, HideKeyboard);
        *open = None;
        eprintln!("{}: SteamVR keyboard closed for the physical one", panel(i).v.name);
    }
}

fn show(p: &Panel) {
    let hint = CString::new(format!("Typing to {}", p.v.name)).unwrap_or_default();
    let mode = sys::EGamepadTextInputMode_k_EGamepadTextInputModeNormal;
    let lines = sys::EGamepadTextInputLineMode_k_EGamepadTextInputLineModeSingleLine;
    let flags = sys::EKeyboardFlags_KeyboardFlag_Minimal;
    let e = call!(ov, ShowKeyboardForOverlay, p.overlay, mode, lines, flags, hint.as_ptr() as *mut _, 1024, c"".as_ptr() as *mut _, USER | p.index as u64);
    if e == 0 {
        *OPEN.lock().unwrap() = Some((p.index, Instant::now()));
        KVM.lock().unwrap().type_to(p.index);
        eprintln!("{}: SteamVR keyboard open", p.v.name);
    } else {
        eprintln!("{}: no SteamVR keyboard ({})", p.v.name, vr::error_name(e));
    }
}

/// A window opened the keyboard for its own field, so it isn't the panel's anymore.
pub fn forget() {
    *OPEN.lock().unwrap() = None;
}

/// Handles e if it's the panel keyboard's, from p's overlay: true if it was.
pub fn event(p: &Panel, e: &vr::VREvent_t) -> bool {
    let k = unsafe { e.data.keyboard };
    let ours = k.uUserValue == USER | p.index as u64;
    match e.eventType {
        sys::EVREventType_VREvent_KeyboardCharInput if ours => {
            if let Some(text) = typed(&k.cNewInput) {
                KVM.lock().unwrap().type_text(p.index, &text);
            }
        }
        sys::EVREventType_VREvent_KeyboardDone | sys::EVREventType_VREvent_KeyboardClosed if ours => {
            OPEN.lock().unwrap().take_if(|(i, _)| *i == p.index);
        }
        _ => return false,
    }
    true
}

/// The text in a keyboard event: up to 8 bytes of UTF-8, ended by a 0 if it's shorter.
fn typed(input: &[std::ffi::c_char; 8]) -> Option<String> {
    let bytes = input.map(|c| c as u8);
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(8);
    String::from_utf8(bytes[..n].to_vec()).ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

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
