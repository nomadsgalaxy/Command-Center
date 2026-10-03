//! The headset's eye tracker, done the supported way: an eyetracking action bound to
//! /user/head/eyetracking (see actions/), read with GetEyeTrackingDataRelativeToNow. I only use
//! it to tell which panel you're looking at, so its few degrees of error don't matter much.
use crate::geometry::V3;
use crate::{call, root};
use openvr_sys as sys;
use std::ffi::CString;

pub struct Gaze {
    action: sys::VRActionHandle_t,
    set: sys::VRActionSetHandle_t,
}

impl Gaze {
    pub fn new() -> Option<Gaze> {
        let manifest = root().join("crates/cc-panels/actions/cc_panels_actions.json");
        let m = CString::new(manifest.to_string_lossy().as_bytes()).ok()?;
        let e = call!(input, SetActionManifestPath, m.as_ptr() as *mut _);
        let (mut action, mut set) = (0, 0);
        call!(input, GetActionHandle, c"/actions/gaze/in/gaze".as_ptr() as *mut _, &mut action);
        call!(input, GetActionSetHandle, c"/actions/gaze".as_ptr() as *mut _, &mut set);
        eprintln!("gaze: action manifest {}: error {e}", manifest.display());
        (e == 0 && action != 0).then_some(Gaze { action, set })
    }

    /// Where you're looking: an origin and a unit direction in standing space, if the
    /// tracker has a valid sample right now.
    pub fn sample(&self) -> Option<(V3, V3)> {
        let mut active: sys::VRActiveActionSet_t = unsafe { std::mem::zeroed() };
        active.ulActionSet = self.set;
        active.nPriority = sys::k_nActionSetOverlayGlobalPriorityMin;
        call!(input, UpdateActionState, &mut active, size_of::<sys::VRActiveActionSet_t>() as u32, 1);
        let mut e: sys::VREyeTrackingData_t = unsafe { std::mem::zeroed() };
        let err = call!(
            input,
            GetEyeTrackingDataRelativeToNow,
            self.action,
            sys::ETrackingUniverseOrigin_TrackingUniverseStanding,
            0.0,
            &mut e,
            size_of::<sys::VREyeTrackingData_t>() as u32
        );
        if err != 0 || !e.bActive || !e.bValid {
            return None;
        }
        let o = e.vGazeOrigin.v.map(|x| x as f64);
        let t = e.vGazeTarget.v.map(|x| x as f64);
        let d = [t[0] - o[0], t[1] - o[1], t[2] - o[2]];
        let n = crate::geometry::norm(&d);
        (n > 1e-6).then(|| (o, d.map(|x| x / n)))
    }
}
