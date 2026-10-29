//! SteamVR through OpenVR's C API. The overlay, system and IPC resource tables get fetched once and kept.
use crate::geometry::Mat;
use openvr_sys as sys;
use std::ffi::{CStr, CString};
use std::sync::OnceLock;

pub use sys::{VREvent_t, VROverlayHandle_t as Handle};

pub struct Vr {
    pub ov: &'static sys::VR_IVROverlay_FnTable,
    pub sys: &'static sys::VR_IVRSystem_FnTable,
    pub ipc: &'static sys::VR_IVRIPCResourceManagerClient_FnTable,
    pub input: &'static sys::VR_IVRInput_FnTable,
    pub rm: &'static sys::VR_IVRRenderModels_FnTable,
    pub apps: &'static sys::VR_IVRApplications_FnTable,           // GetCurrentSceneProcessId (taskbar.rs), and the tab's manifest (tab.rs)
    pub notes: Option<&'static sys::VR_IVRNotifications_FnTable>, // SteamVR's toasts (notify.rs); start-up doesn't need it
}

static VR: OnceLock<Vr> = OnceLock::new();

pub fn vr() -> &'static Vr {
    VR.get().expect("SteamVR not initialised")
}

/// One call through a table, e.g. `call!(ov, ShowOverlay, h)`.
#[macro_export]
macro_rules! call {
    ($t:ident, $f:ident $(, $a:expr)* $(,)?) => {
        unsafe { ($crate::vr::vr().$t.$f.unwrap())($($a),*) }
    };
}

fn table<T>(version: &[u8]) -> Result<&'static T, String> {
    let version = CStr::from_bytes_with_nul(version).ok().and_then(|v| v.to_str().ok()).ok_or("a bad interface version")?;
    let name = cstr(&format!("FnTable:{version}"))?;
    let mut err = 0;
    let p = unsafe { sys::VR_GetGenericInterface(name.as_ptr(), &mut err) } as *const T;
    if p.is_null() { Err(format!("no {name:?} ({err})")) } else { Ok(unsafe { &*p }) }
}

pub fn init() -> Result<(), String> {
    init_as(sys::EVRApplicationType_VRApplication_Overlay)
}

/// init as another kind of app (tab.rs registers the manifest as a utility).
pub fn init_as(kind: sys::EVRApplicationType) -> Result<(), String> {
    let mut err = 0;
    unsafe { sys::VR_InitInternal2(&mut err, kind, std::ptr::null()) };
    if err != 0 {
        return Err(unsafe { CStr::from_ptr(sys::VR_GetVRInitErrorAsEnglishDescription(err)) }.to_string_lossy().into());
    }
    let vr = Vr {
        ov: table(sys::IVROverlay_Version)?,
        sys: table(sys::IVRSystem_Version)?,
        ipc: table(sys::IVRIPCResourceManagerClient_Version)?,
        input: table(sys::IVRInput_Version)?,
        rm: table(sys::IVRRenderModels_Version)?,
        apps: table(sys::IVRApplications_Version)?,
        notes: table(sys::IVRNotifications_Version).ok(),
    };
    let _ = VR.set(vr);
    Ok(())
}

pub fn shutdown() {
    unsafe { sys::VR_ShutdownInternal() };
}

/// Runs something that might be slow (a texture import or upload, drawing a card) and logs
/// it if it took over 5 ms (docs/stutter-plan.md fix 0).
pub fn timed<T>(what: impl FnOnce() -> String, f: impl FnOnce() -> T) -> T {
    let t = std::time::Instant::now();
    let r = f();
    if t.elapsed() > std::time::Duration::from_millis(5) {
        eprintln!("slow {}: {} ms", what(), t.elapsed().as_millis());
    }
    r
}

/// The main loop is the only thread calling OpenVR, and it parks between ticks. Anything it
/// should see soon (input, a new frame, news from KWin) wakes it. This lives here rather than
/// in main.rs because the spike builds capture.rs too.
pub static MAIN: OnceLock<std::thread::Thread> = OnceLock::new();
pub static WOKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn wake() {
    // only once until the loop has looked, so a mouse's 1000 reports a second don't each wake it
    if !WOKEN.swap(true, std::sync::atomic::Ordering::Relaxed)
        && let Some(t) = MAIN.get()
    {
        t.unpark();
    }
}

pub fn error_name(e: sys::EVROverlayError) -> String {
    let name = call!(ov, GetOverlayErrorNameFromEnum, e);
    unsafe { CStr::from_ptr(name) }.to_string_lossy().into()
}

pub fn create_overlay(key: &str, name: &str) -> Result<Handle, String> {
    let (k, n) = (cstr(key)?, cstr(name)?);
    let mut h = 0;
    match call!(ov, CreateOverlay, k.as_ptr() as *mut _, n.as_ptr() as *mut _, &mut h) {
        0 => Ok(h),
        e => Err(error_name(e)),
    }
}

/// A tab in SteamVR's dashboard: its page (main) and its thumbnail, which is the tab's icon.
pub fn create_dashboard_overlay(key: &str, name: &str) -> Result<(Handle, Handle), String> {
    let (k, n) = (cstr(key)?, cstr(name)?);
    let (mut main, mut thumb) = (0, 0);
    match call!(ov, CreateDashboardOverlay, k.as_ptr() as *mut _, n.as_ptr() as *mut _, &mut main, &mut thumb) {
        0 => Ok((main, thumb)),
        e => Err(error_name(e)),
    }
}

pub fn set_from_file(h: Handle, path: &str) {
    match cstr(path) {
        Ok(p) => {
            call!(ov, SetOverlayFromFile, h, p.as_ptr() as *mut _);
        }
        Err(e) => eprintln!("overlay picture: {e}"),
    }
}

/// A C string for OpenVR. Overlay names come from viewers.conf and paths from the disk, so a NUL in
/// one is an error for that overlay, not a panic that takes cc-panels down.
fn cstr(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("{s:?} has a NUL in it"))
}

pub fn place(h: Handle, m: &Mat) {
    let mut t = sys::HmdMatrix34_t { m: *m };
    call!(ov, SetOverlayTransformAbsolute, h, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, &mut t);
}

/// The headset's pose in standing space, while it's tracked.
pub fn head() -> Option<Mat> {
    let mut pose: sys::TrackedDevicePose_t = Default::default();
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, 0.0, &mut pose, 1);
    pose.bPoseIsValid.then_some(pose.mDeviceToAbsoluteTracking.m)
}

/// How far ahead of now this frame's picture reaches your eyes: the rest of this frame, plus the
/// display's vsync-to-photons delay. Poses read at that time line up with what's shown, so a
/// carried or docked panel doesn't trail behind the hand (the scan wants poses at now, `head`).
pub fn to_photons() -> f32 {
    let (mut since, mut frame) = (0f32, 0u64);
    if !call!(sys, GetTimeSinceLastVsync, &mut since, &mut frame) {
        return 0.0;
    }
    // the display's rate and delay, read again about once a second (a refresh-rate change in
    // SteamVR's settings shows up within that), not on every pose read
    static DISPLAY: std::sync::Mutex<Option<(std::time::Instant, f32, f32)>> = std::sync::Mutex::new(None);
    let mut d = DISPLAY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (hz, v2p) = match *d {
        Some((at, hz, v2p)) if at.elapsed() < std::time::Duration::from_secs(1) => (hz, v2p),
        _ => {
            let mut err = 0;
            let hz = call!(sys, GetFloatTrackedDeviceProperty, 0, sys::ETrackedDeviceProperty_Prop_DisplayFrequency_Float, &mut err);
            let v2p = call!(sys, GetFloatTrackedDeviceProperty, 0, sys::ETrackedDeviceProperty_Prop_SecondsFromVsyncToPhotons_Float, &mut err);
            *d = Some((std::time::Instant::now(), hz, v2p));
            (hz, v2p)
        }
    };
    drop(d);
    if hz <= 0.0 { 0.0 } else { (1.0 / hz - since).max(0.0) + v2p.max(0.0) }
}

/// The headset's pose when this frame's picture reaches your eyes (`to_photons`), if it's tracked.
pub fn head_predicted() -> Option<Mat> {
    let mut pose: sys::TrackedDevicePose_t = Default::default();
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, to_photons(), &mut pose, 1);
    pose.bPoseIsValid.then_some(pose.mDeviceToAbsoluteTracking.m)
}

/// Whether the headset is being worn, as its proximity sensor says through SteamVR's activity
/// level (the same thing VREvent 103/104 report). None when SteamVR can't tell.
pub fn head_worn() -> Option<bool> {
    match call!(sys, GetTrackedDeviceActivityLevel, 0) {
        sys::EDeviceActivityLevel_k_EDeviceActivityLevel_UserInteraction | sys::EDeviceActivityLevel_k_EDeviceActivityLevel_UserInteraction_Timeout => Some(true),
        sys::EDeviceActivityLevel_k_EDeviceActivityLevel_Idle | sys::EDeviceActivityLevel_k_EDeviceActivityLevel_Standby | sys::EDeviceActivityLevel_k_EDeviceActivityLevel_Idle_Timeout => Some(false),
        _ => None,
    }
}

/// The headset's pose in SteamVR's raw tracking space (drivers report raw poses).
pub fn head_raw() -> Option<Mat> {
    let mut pose: sys::TrackedDevicePose_t = Default::default();
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseRawAndUncalibrated, 0.0, &mut pose, 1);
    pose.bPoseIsValid.then_some(pose.mDeviceToAbsoluteTracking.m)
}

/// Where the headset is, or the last place it was tracked (not the floor origin).
pub fn head_position() -> [f64; 3] {
    static LAST: std::sync::Mutex<[f64; 3]> = std::sync::Mutex::new([0.0, 1.6, 0.0]);
    let mut last = LAST.lock().unwrap();
    if let Some(m) = head() {
        *last = [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64];
    }
    *last
}

/// A real controller, not a virtual pointer (ours, cc_pointer, or another app's). By convention
/// those drivers name their controller type `<something>_pointer`.
pub fn is_real_controller(i: u32) -> bool {
    if crate::call!(sys, GetTrackedDeviceClass, i) != sys::ETrackedDeviceClass_TrackedDeviceClass_Controller {
        return false;
    }
    let mut buf = [0 as std::os::raw::c_char; 64];
    let mut err = 0;
    crate::call!(sys, GetStringTrackedDeviceProperty, i, sys::ETrackedDeviceProperty_Prop_ControllerType_String, buf.as_mut_ptr(), buf.len() as u32, &mut err);
    let t = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
    !t.ends_with("_pointer")
}

/// A device's laser: its pose moved to the render model's "tip", which is where SteamVR's
/// laser starts and which way it points (on the Frame controllers it's angled from the grip).
/// With no tip, like our virtual pointer whose pose already is its laser, it's just the pose.
/// The laser points along -z.
pub fn laser_pose(dev: u32) -> Option<Mat> {
    use std::collections::HashMap;
    static TIPS: std::sync::Mutex<Option<HashMap<String, Mat>>> = std::sync::Mutex::new(None);
    let mut pose: [sys::TrackedDevicePose_t; 64] = std::array::from_fn(|_| Default::default());
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, 0.0, pose.as_mut_ptr(), 64);
    let d = pose.get(dev as usize).filter(|p| p.bPoseIsValid)?.mDeviceToAbsoluteTracking.m;
    let mut buf = [0 as std::os::raw::c_char; 256];
    let mut err = 0;
    call!(sys, GetStringTrackedDeviceProperty, dev, sys::ETrackedDeviceProperty_Prop_RenderModelName_String, buf.as_mut_ptr(), buf.len() as u32, &mut err);
    let model = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    let mut tips = TIPS.lock().unwrap();
    let tips = tips.get_or_insert_with(HashMap::new);
    if !tips.contains_key(&model) && !model.is_empty() {
        let mut mode: sys::RenderModel_ControllerMode_State_t = Default::default();
        let mut state: sys::RenderModel_ComponentState_t = Default::default();
        if call!(rm, GetComponentStateForDevicePath, buf.as_mut_ptr(), sys::k_pch_Controller_Component_Tip.as_ptr() as *mut _, 0, &mut mode, &mut state) {
            tips.insert(model.clone(), state.mTrackingToComponentLocal.m);
        }
    }
    Some(match tips.get(&model) {
        Some(tip) => crate::geometry::mul(&d, tip),
        None => d,
    })
}

fn string_prop(dev: u32, prop: sys::ETrackedDeviceProperty) -> String {
    let mut buf = [0 as std::os::raw::c_char; 256];
    let mut err = 0;
    call!(sys, GetStringTrackedDeviceProperty, dev, prop, buf.as_mut_ptr(), buf.len() as u32, &mut err);
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// What cc-roles printed, for the `devices` control command: each tracked device's class,
/// hand role, connection, controller type and render model, one line each.
pub fn devices() -> Vec<String> {
    const ROLES: [&str; 6] = ["invalid", "left", "right", "opt-out", "treadmill", "stylus"];
    (0..sys::k_unMaxTrackedDeviceCount as u32)
        .filter_map(|i| {
            let class = call!(sys, GetTrackedDeviceClass, i);
            (class != sys::ETrackedDeviceClass_TrackedDeviceClass_Invalid).then(|| {
                let role = call!(sys, GetControllerRoleForTrackedDeviceIndex, i) as usize;
                format!(
                    "device {i} class {class} role {} connected {} type {} model {}",
                    ROLES.get(role).unwrap_or(&"?"),
                    call!(sys, IsTrackedDeviceConnected, i) as u8,
                    string_prop(i, sys::ETrackedDeviceProperty_Prop_ControllerType_String),
                    string_prop(i, sys::ETrackedDeviceProperty_Prop_RenderModelName_String)
                )
            })
        })
        .collect()
}

/// A Frame controller's tip right now (what cc-tip sampled), for `hand` left, right or any:
/// its standing-space position and which hand it is. The render model's name tells us the hand.
pub fn tip(hand: &str) -> Option<([f64; 3], &'static str)> {
    (0..sys::k_unMaxTrackedDeviceCount as u32).find_map(|i| {
        if !is_real_controller(i) || !call!(sys, IsTrackedDeviceConnected, i) {
            return None;
        }
        let model = string_prop(i, sys::ETrackedDeviceProperty_Prop_RenderModelName_String);
        let h = if model.contains("left") { "left" } else { "right" };
        if !model.contains("frame_controller") || (hand != "any" && hand != h) {
            return None;
        }
        let m = laser_pose(i)?;
        Some(([m[0][3] as f64, m[1][3] as f64, m[2][3] as f64], h))
    })
}

/// A fake SteamVR for tests (docs/vr-harness.md): OpenVR's own function tables, filled with
/// functions that answer from a script instead of vrserver, so every `call!` runs unchanged. Only
/// what a replay reaches is filled in; anything else is None, and its `call!` panics naming it.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// What the fake answers with.
    #[derive(Default)]
    pub struct Script {
        pub events: VecDeque<sys::VREvent_t>,
        pub poses: Vec<(u32, Mat)>, // each device's pose; a device not here isn't tracked
        next: u64,                  // the next overlay handle
    }

    pub static SCRIPT: Mutex<Option<Script>> = Mutex::new(None);
    /// Held by a test for as long as it uses the fake, since there's one SteamVR per process.
    pub static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    fn script<R>(f: impl FnOnce(&mut Script) -> R) -> R {
        f(SCRIPT.lock().unwrap().get_or_insert_with(Script::default))
    }

    unsafe extern "C" fn poll(_h: Handle, e: *mut sys::VREvent_t, _size: u32) -> bool {
        script(|s| s.events.pop_front()).map(|ev| unsafe { *e = ev }).is_some()
    }
    unsafe extern "C" fn create(_k: *mut std::os::raw::c_char, _n: *mut std::os::raw::c_char, h: *mut Handle) -> sys::EVROverlayError {
        unsafe {
            *h = script(|s| {
                s.next += 1;
                s.next
            })
        };
        0
    }
    unsafe extern "C" fn flag(_h: Handle, _f: sys::VROverlayFlags, _on: bool) -> sys::EVROverlayError {
        0
    }
    unsafe extern "C" fn sort(_h: Handle, _o: u32) -> sys::EVROverlayError {
        0
    }
    unsafe extern "C" fn mouse_scale(_h: Handle, _s: *mut sys::HmdVector2_t) -> sys::EVROverlayError {
        0
    }
    unsafe extern "C" fn input_method(_h: Handle, _m: sys::VROverlayInputMethod) -> sys::EVROverlayError {
        0
    }
    unsafe extern "C" fn show(_h: Handle) -> sys::EVROverlayError {
        0
    }
    unsafe extern "C" fn poses(_o: sys::ETrackingUniverseOrigin, _t: f32, out: *mut sys::TrackedDevicePose_t, n: u32) {
        let out = unsafe { std::slice::from_raw_parts_mut(out, n as usize) };
        script(|s| {
            for (dev, m) in &s.poses {
                if let Some(p) = out.get_mut(*dev as usize) {
                    p.mDeviceToAbsoluteTracking.m = *m;
                    p.bPoseIsValid = true;
                }
            }
        });
    }
    // no render model, so laser_pose gives the pose as it is: a trace records the laser itself
    unsafe extern "C" fn string_prop(_d: u32, _p: sys::ETrackedDeviceProperty, buf: *mut std::os::raw::c_char, size: u32, _e: *mut sys::ETrackedPropertyError) -> u32 {
        if size > 0 {
            unsafe { *buf = 0 };
        }
        0
    }
    unsafe extern "C" fn class(_d: u32) -> sys::ETrackedDeviceClass {
        sys::ETrackedDeviceClass_TrackedDeviceClass_Controller
    }

    /// Puts the fake in place of SteamVR, once for the process, and starts `s` as its script.
    /// Returns the guard that keeps other fake-SteamVR tests out until it drops.
    pub fn install(s: Script) -> std::sync::MutexGuard<'static, ()> {
        let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        fn leak<T>(t: T) -> &'static T {
            Box::leak(Box::new(t))
        }
        VR.get_or_init(|| Vr {
            ov: leak(sys::VR_IVROverlay_FnTable {
                PollNextOverlayEvent: Some(poll),
                CreateOverlay: Some(create),
                SetOverlayFlag: Some(flag),
                SetOverlaySortOrder: Some(sort),
                SetOverlayMouseScale: Some(mouse_scale),
                SetOverlayInputMethod: Some(input_method),
                ShowOverlay: Some(show),
                HideOverlay: Some(show),
                ..Default::default()
            }),
            sys: leak(sys::VR_IVRSystem_FnTable {
                GetDeviceToAbsoluteTrackingPose: Some(poses),
                GetStringTrackedDeviceProperty: Some(string_prop),
                GetTrackedDeviceClass: Some(class),
                ..Default::default()
            }),
            ipc: leak(Default::default()),
            input: leak(Default::default()),
            rm: leak(Default::default()),
            apps: leak(Default::default()),
            notes: None,
        });
        *SCRIPT.lock().unwrap() = Some(s);
        guard
    }
}

#[cfg(test)]
mod tests {
    /// A NUL in an overlay's name or a picture's path is an error, not a panic.
    #[test]
    fn a_nul_is_an_error() {
        assert!(super::cstr("controlcenter.panel.desk").is_ok());
        assert!(super::cstr("desk\0wide").is_err());
    }
}
