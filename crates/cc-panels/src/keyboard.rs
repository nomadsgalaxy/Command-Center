//! SteamVR's keyboard: the `Keyboard` boundary (docs/openxr-plan.md, phase 0 step 5) and `Live`,
//! its OpenVR side, the only code here that touches OpenVR. osk.rs (a panel's typing),
//! control/workspace.rs and control/machines.rs (their fields) open and read it through it.
use std::ffi::CString;

use crate::call;
use crate::ids::SurfaceId;
use crate::texture::Refused;
use openvr_sys as sys;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardMode {
    Normal,
    #[allow(dead_code)] // the design's; no field asks for one yet
    Password,
    /// The key strip's minimal mode (osk.rs).
    Minimal,
}

/// The runtime's keyboard (SteamVR's, osk.rs, workspace.rs, machines.rs). Its events come in
/// through `Pointing::next` and `Session` as SteamVR sends them now.
pub trait Keyboard {
    /// Opens it on s for up to `max` characters, starting with `initial`; its events carry `user`.
    /// Refused when the runtime has none (OpenXR, later), and the core opens its own.
    #[allow(clippy::too_many_arguments)] // ShowKeyboardForOverlay's own, with SurfaceId for the overlay
    fn open(&mut self, s: SurfaceId, mode: KeyboardMode, multiline: bool, hint: &str, initial: &str, max: u32, user: u64) -> Result<(), Refused>;
    /// What's typed so far, as the runtime hands it over: a 256-byte buffer, ended by a 0 if it's
    /// shorter. The callers each read it their own way (machines.rs, workspace.rs).
    fn text(&self) -> [u8; 256];
    fn close(&mut self);
}

/// SteamVR, for real.
pub struct Live;

impl Keyboard for Live {
    fn open(&mut self, s: SurfaceId, mode: KeyboardMode, multiline: bool, hint: &str, initial: &str, max: u32, user: u64) -> Result<(), Refused> {
        let (hint, initial) = (CString::new(hint).unwrap_or_default(), CString::new(initial).unwrap_or_default());
        let input = match mode {
            KeyboardMode::Password => sys::EGamepadTextInputMode_k_EGamepadTextInputModePassword,
            KeyboardMode::Normal | KeyboardMode::Minimal => sys::EGamepadTextInputMode_k_EGamepadTextInputModeNormal,
        };
        let lines = if multiline { sys::EGamepadTextInputLineMode_k_EGamepadTextInputLineModeMultipleLines } else { sys::EGamepadTextInputLineMode_k_EGamepadTextInputLineModeSingleLine };
        let flags = if mode == KeyboardMode::Minimal { sys::EKeyboardFlags_KeyboardFlag_Minimal } else { 0 };
        match call!(ov, ShowKeyboardForOverlay, s.0, input, lines, flags, hint.as_ptr() as *mut _, max, initial.as_ptr() as *mut _, user) {
            0 => Ok(()),
            e => Err(Refused(e)),
        }
    }

    fn text(&self) -> [u8; 256] {
        let mut buf = [0u8; 256];
        call!(ov, GetKeyboardText, buf.as_mut_ptr() as *mut _, buf.len() as u32);
        buf
    }

    fn close(&mut self) {
        call!(ov, HideKeyboard);
    }
}
