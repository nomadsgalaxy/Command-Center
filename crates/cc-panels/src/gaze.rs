//! The headset's eye tracker, done the supported way: an eyetracking action bound to
//! /user/head/eyetracking (see actions/), read with GetEyeTrackingDataRelativeToNow. I only use
//! it to tell which panel you're looking at, so its few degrees of error don't matter much.
use crate::geometry::V3;
use crate::{call, root};
use openvr_sys as sys;
use std::ffi::CString;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Samples that stay invalid this long after valid ones get logged once. The headset coming off
/// does it too, so this only says "look here", not that anything broke.
const LOST_AFTER: Duration = Duration::from_secs(5);

pub struct Gaze {
    action: sys::VRActionHandle_t,
    set: sys::VRActionSetHandle_t,
    health: Mutex<Health>,
}

/// Whether the tracker is still giving valid samples, so a live test can tell if priority 0 stopped it.
#[derive(Default)]
struct Health {
    last_valid: Option<Instant>,
    warned: bool,
}

impl Health {
    /// Notes one sample. Gives a line to log when valid samples have stopped, once, and another
    /// when they start again.
    fn note(&mut self, valid: bool, now: Instant) -> Option<&'static str> {
        if valid {
            self.last_valid = Some(now);
            return std::mem::take(&mut self.warned).then_some("gaze: valid eye samples again");
        }
        let lost = self.last_valid.is_some_and(|t| now.duration_since(t) >= LOST_AFTER);
        (lost && !std::mem::replace(&mut self.warned, true)).then_some("gaze: no valid eye samples for 5 s after valid ones (the headset off, or the action set's priority 0 not getting the tracker)")
    }
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
        (e == 0 && action != 0).then_some(Gaze { action, set, health: Mutex::new(Health::default()) })
    }

    /// Lets go of the action set while a VR game runs. It's active at priority 0, which takes
    /// nothing from the scene app (global overlay priority would, but only with SteamVR's
    /// experimental "overlay input overrides", which is off), so this is just tidiness.
    pub fn release(&self) {
        call!(input, UpdateActionState, std::ptr::null_mut(), size_of::<sys::VRActiveActionSet_t>() as u32, 0);
    }

    /// Where you're looking: an origin and a unit direction in standing space, if the
    /// tracker has a valid sample right now.
    pub fn sample(&self) -> Option<(V3, V3)> {
        let mut active: sys::VRActiveActionSet_t = unsafe { std::mem::zeroed() };
        active.ulActionSet = self.set;
        active.nPriority = 0; // not the global overlay priority: that needs an experimental SteamVR setting and never took the controllers
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
        let valid = err == 0 && e.bActive && e.bValid;
        if let Some(line) = self.health.lock().unwrap().note(valid, Instant::now()) {
            eprintln!("{line}");
        }
        if !valid {
            return None;
        }
        let o = e.vGazeOrigin.v.map(|x| x as f64);
        let t = e.vGazeTarget.v.map(|x| x as f64);
        let d = [t[0] - o[0], t[1] - o[1], t[2] - o[2]];
        let n = crate::geometry::norm(&d);
        (n > 1e-6).then(|| (o, d.map(|x| x / n)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_says_once_when_valid_samples_stop_and_again_when_they_return() {
        let (mut h, t0) = (Health::default(), Instant::now());
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert_eq!(h.note(false, at(0)), None, "never valid: nothing to lose");
        assert_eq!(h.note(true, at(1)), None);
        assert_eq!(h.note(false, at(2)), None, "just a blink");
        assert_eq!(h.note(false, at(5)), None, "4 s");
        assert!(h.note(false, at(6)).is_some_and(|l| l.contains("no valid eye samples")), "5 s: said");
        assert_eq!(h.note(false, at(30)), None, "once");
        assert!(h.note(true, at(31)).is_some_and(|l| l.contains("again")), "back");
        assert_eq!(h.note(true, at(32)), None);
        assert!(h.note(false, at(60)).is_some(), "and a second loss is said too");
    }
}
