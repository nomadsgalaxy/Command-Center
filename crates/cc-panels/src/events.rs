//! The neutral events a surface's queue gives (docs/openxr-plan.md, section 3): what the core sees
//! of a laser, the runtime's keyboard and a close, whichever runtime made them. On OpenVR,
//! `vr::surface_event` translates each VREvent_t into one of these, in order and with SteamVR's own
//! coordinates, and never makes one up (phase 0's contract, backend.rs `Pointing`).
//!
//! Apart from backend.rs so cc-win-spike, which builds back.rs and vr.rs, has them too.
use crate::ids::Dev;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    /// Anything else SteamVR sends (its own number). The code treats it as it did before: not
    /// Left, Right or Middle.
    Other(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerKind {
    Enter,
    Move,
    Down(Button),
    Up(Button),
    /// `smooth` is SteamVR's ScrollSmooth against ScrollDiscrete. The panels take both, grab.rs's
    /// cards only discrete, so the difference is kept.
    Scroll {
        dx: f32,
        dy: f32,
        smooth: bool,
    },
    Leave,
}

/// A pointer on a surface. x and y are in the surface's pointer units (what `Overlays::mouse_scale`
/// set: pixels for a panel, metres for a card), from its bottom left, as SteamVR's mouse events are
/// now. So `geometry::surface_point` and `from_top` stay in the core.
///
/// On OpenVR, x and y are the event's mouse data whatever its kind, as the code read them before:
/// for a scroll that's SteamVR's x and y deltas (its scroll data shares the space), and for focus
/// coming or going whatever SteamVR left there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub kind: PointerKind,
    pub x: f32,
    pub y: f32,
    pub from: Dev,
    /// How old the event is, in seconds (VREvent_t's eventAgeSeconds; 0 where a backend doesn't know).
    pub age: f32,
}

/// The keyboard's events. `user` is the value the keyboard was opened with (osk.rs uses it for
/// which panel is typing).
#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardEvent {
    /// What was typed since the last one, as UTF-8 (up to 8 bytes on SteamVR). None when SteamVR's
    /// bytes weren't text, or were empty (osk.rs says so).
    Char { user: u64, text: Option<String> },
    Done { user: u64 },
    Closed { user: u64 },
}

/// What one surface's queue gives (the `PollNextOverlayEvent` loops).
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceEvent {
    Pointer(PointerEvent),
    Keyboard(KeyboardEvent),
    /// SteamVR's close button on the dashboard tab's bar (control/dashtab.rs).
    Closed,
    /// Anything else, by the runtime's own number, so a loop that counts its events counts the same.
    Other(u32),
}
