//! What only some runtimes have: SteamVR's dashboard and its toasts. The `Shell` boundary
//! (docs/openxr-plan.md, phase 0 step 5) and `Live`, its OpenVR side, the only code here that
//! touches OpenVR. control/dashtab.rs asks about the dashboard through it, and notify.rs shows
//! its toasts.
use crate::call;
use crate::ids::SurfaceId;

/// What only some runtimes have.
pub trait Shell {
    /// Whether there's a dashboard to put a tab in. Nothing asks yet: on OpenVR there always is.
    #[allow(dead_code)] // OpenXR's (phase 1), which has none
    fn has_dashboard(&self) -> bool;
    fn dashboard_visible(&self) -> bool;
    /// Whether our tab is the one showing.
    fn our_tab_active(&self, main: SurfaceId) -> bool;
    /// SteamVR's toast, hung off the surface `on` (our tab's page), the fallback when D-Bus
    /// notifications aren't there. `Err(None)` when the runtime has no toasts, `Err(Some(..))` when
    /// it wouldn't show this one.
    fn toast(&mut self, on: SurfaceId, text: &str) -> Result<(), Option<NoteRefused>>;
}

/// The runtime wouldn't show a toast: IVRNotifications' own error number. Not `Refused`, whose
/// name comes from the overlay errors, a different numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteRefused(pub u32);

/// SteamVR, for real.
pub struct Live;

impl Shell for Live {
    fn has_dashboard(&self) -> bool {
        true
    }

    fn dashboard_visible(&self) -> bool {
        call!(ov, IsDashboardVisible)
    }

    fn our_tab_active(&self, main: SurfaceId) -> bool {
        call!(ov, IsActiveDashboardOverlay, main.0)
    }

    fn toast(&mut self, on: SurfaceId, text: &str) -> Result<(), Option<NoteRefused>> {
        let Some(n) = crate::vr::vr().notes else { return Err(None) };
        let c = std::ffi::CString::new(text.replace('\0', "")).unwrap_or_default();
        let mut id = 0;
        match unsafe { (n.CreateNotification.unwrap())(on.0, 0, openvr_sys::EVRNotificationType_Transient, c.as_ptr() as *mut _, openvr_sys::EVRNotificationStyle_None, std::ptr::null_mut(), &mut id) }
        {
            0 => Ok(()),
            e => Err(Some(NoteRefused(e))),
        }
    }
}
