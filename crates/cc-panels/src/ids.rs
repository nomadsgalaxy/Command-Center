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
///
/// Only a backend's conversion makes one (`vr::dev`, and the tests' fakes), so its id is always one
/// the backend gave out: `Ctl::new` is crate-private and the id is read with `raw`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ctl(u32);

impl Ctl {
    /// The backend's controller `id`. On OpenVR that's a tracked-device index, never 0 (the
    /// headset) or 64 (the mouse): those are `Dev::Head` and `Dev::Mouse`, and `vr::index` checks.
    pub(crate) const fn new(id: u32) -> Ctl {
        Ctl(id)
    }

    pub fn raw(self) -> u32 {
        self.0
    }
}

/// What can point, be tracked or hold a surface. The room isn't one: a surface in the room is
/// `At::Room` (overlay.rs), and where something can be on a device or in the room it's
/// `Option<Dev>`, None being the room.
/// Trackers, base stations and our own virtual pointer (cc-pointer) aren't `Dev`s either, because
/// nothing uses them as one: `vr::is_real_controller` filters them out today, and the virtual
/// pointer stays inside the OpenVR backend (laser.rs).
///
/// On OpenVR every index converts to a Dev and back to itself (`vr::dev`, `vr::index`), but a Dev
/// only comes back as itself if it's canonical: the headset is `Head` and the mouse `Mouse`, never
/// a controller numbered 0 or 64, which would come back as `Head` or `Mouse`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dev {
    Head,
    Controller(Ctl),
    /// The mouse's ray: a pointer with a pose but no device behind it (OpenVR's index 64, past
    /// SteamVR's own, `vr::dev`).
    Mouse,
}
