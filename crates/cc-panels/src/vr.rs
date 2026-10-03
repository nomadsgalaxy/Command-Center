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
    pub apps: &'static sys::VR_IVRApplications_FnTable, // only for GetCurrentSceneProcessId (taskbar.rs)
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
    let name = CString::new(format!("FnTable:{}", CStr::from_bytes_with_nul(version).unwrap().to_str().unwrap())).unwrap();
    let mut err = 0;
    let p = unsafe { sys::VR_GetGenericInterface(name.as_ptr(), &mut err) } as *const T;
    if p.is_null() { Err(format!("no {name:?} ({err})")) } else { Ok(unsafe { &*p }) }
}

pub fn init() -> Result<(), String> {
    let mut err = 0;
    unsafe { sys::VR_InitInternal2(&mut err, sys::EVRApplicationType_VRApplication_Overlay, std::ptr::null()) };
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
    let (k, n) = (CString::new(key).unwrap(), CString::new(name).unwrap());
    let mut h = 0;
    match call!(ov, CreateOverlay, k.as_ptr() as *mut _, n.as_ptr() as *mut _, &mut h) {
        0 => Ok(h),
        e => Err(error_name(e)),
    }
}

pub fn set_from_file(h: Handle, path: &str) {
    let p = CString::new(path).unwrap();
    call!(ov, SetOverlayFromFile, h, p.as_ptr() as *mut _);
}

pub fn place(h: Handle, m: &Mat) {
    let mut t = sys::HmdMatrix34_t { m: *m };
    call!(ov, SetOverlayTransformAbsolute, h, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, &mut t);
}

/// The headset's pose in standing space, while it's tracked.
pub fn head() -> Option<Mat> {
    let mut pose: sys::TrackedDevicePose_t = unsafe { std::mem::zeroed() };
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, 0.0, &mut pose, 1);
    pose.bPoseIsValid.then_some(pose.mDeviceToAbsoluteTracking.m)
}

/// The headset's pose in SteamVR's raw tracking space (drivers report raw poses).
pub fn head_raw() -> Option<Mat> {
    let mut pose: sys::TrackedDevicePose_t = unsafe { std::mem::zeroed() };
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
    let mut pose: [sys::TrackedDevicePose_t; 64] = unsafe { std::mem::zeroed() };
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, 0.0, pose.as_mut_ptr(), 64);
    let d = pose.get(dev as usize).filter(|p| p.bPoseIsValid)?.mDeviceToAbsoluteTracking.m;
    let mut buf = [0 as std::os::raw::c_char; 256];
    let mut err = 0;
    call!(sys, GetStringTrackedDeviceProperty, dev, sys::ETrackedDeviceProperty_Prop_RenderModelName_String, buf.as_mut_ptr(), buf.len() as u32, &mut err);
    let model = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    let mut tips = TIPS.lock().unwrap();
    let tips = tips.get_or_insert_with(HashMap::new);
    if !tips.contains_key(&model) && !model.is_empty() {
        let mut mode: sys::RenderModel_ControllerMode_State_t = unsafe { std::mem::zeroed() };
        let mut state: sys::RenderModel_ComponentState_t = unsafe { std::mem::zeroed() };
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
