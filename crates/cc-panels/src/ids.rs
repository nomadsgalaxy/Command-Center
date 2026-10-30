//! The neutral ids the core uses for what a VR runtime keeps (docs/openxr-plan.md, section 3),
//! apart from backend.rs so cc-win-spike, which builds vr.rs, can use them too.
use std::fmt;

/// One surface: a panel, card, bar, back, cursor or window of ours. On OpenVR it's the overlay
/// handle, and `.0` is what goes to SteamVR. It prints as that number, in logs and in the
/// control socket's dumps, as the bare handle did.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(pub u64);

impl SurfaceId {
    /// No surface: OpenVR's invalid overlay handle, 0, which the code uses for "none yet".
    pub const NONE: SurfaceId = SurfaceId(0);
}

impl fmt::Display for SurfaceId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for SurfaceId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[allow(dead_code)] // which hand is the boundary's (backend.rs Stage::hand); nothing asks yet
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hand {
    Left,
    Right,
}

/// A controller, as the backend knows it. On OpenVR it's the tracked-device index; on OpenXR it's
/// one per hand path. It stays opaque because today's code tells controllers apart by index, not
/// by hand: a laser event carries an index, a drag keeps the index it started with, docking takes
/// "the other controllers" as a set, and a controller can have no hand role at all (Invalid or
/// OptOut) or share one. `Stage::hand` says which hand it is right now, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ctl(pub u32);

/// What can point, be tracked or hold a surface. The room isn't one: a surface in the room is
/// `At::Room` (overlay.rs), and where something can be on a device or in the room it's
/// `Option<Dev>`, None being the room.
/// Trackers, base stations and our own virtual pointer (cc-pointer) aren't `Dev`s either, because
/// nothing uses them as one: `vr::is_real_controller` filters them out today, and the virtual
/// pointer stays inside the OpenVR backend (laser.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dev {
    Head,
    Controller(Ctl),
    /// The mouse's ray: a pointer with a pose but no device behind it (OpenVR's index 64, past
    /// SteamVR's own, `vr::dev`).
    Mouse,
}
