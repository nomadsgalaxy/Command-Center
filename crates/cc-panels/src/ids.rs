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
