//! SteamVR through OpenVR's C API. The overlay, system and IPC resource tables get fetched once and kept.
use crate::geometry::Mat;
use openvr_sys as sys;
use std::ffi::{CStr, CString};
use std::sync::OnceLock;

pub use crate::ids::SurfaceId as Handle; // the core's id for an overlay; `.0` is SteamVR's handle
pub use sys::VREvent_t;

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
        0 => Ok(crate::vr::Handle(h)),
        e => Err(error_name(e)),
    }
}

/// A tab in SteamVR's dashboard: its page (main) and its thumbnail, which is the tab's icon.
pub fn create_dashboard_overlay(key: &str, name: &str) -> Result<(Handle, Handle), String> {
    let (k, n) = (cstr(key)?, cstr(name)?);
    let (mut main, mut thumb) = (0, 0);
    match call!(ov, CreateDashboardOverlay, k.as_ptr() as *mut _, n.as_ptr() as *mut _, &mut main, &mut thumb) {
        0 => Ok((crate::vr::Handle(main), crate::vr::Handle(thumb))),
        e => Err(error_name(e)),
    }
}

pub fn set_from_file(h: Handle, path: &str) {
    match cstr(path) {
        Ok(p) => {
            call!(ov, SetOverlayFromFile, h.0, p.as_ptr() as *mut _);
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
    call!(ov, SetOverlayTransformAbsolute, h.0, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, &mut t);
}

/// The device behind OpenVR's tracked-device index `i`: 0 is the headset, 64 (one past SteamVR's
/// own, k_unMaxTrackedDeviceCount) is our mouse's ray, and the rest are controllers, ours and
/// other apps' virtual pointers included (`is_real_controller` tells them apart).
pub fn dev(i: u32) -> crate::ids::Dev {
    use crate::ids::{Ctl, Dev};
    match i {
        0 => Dev::Head,
        i if i == sys::k_unMaxTrackedDeviceCount as u32 => Dev::Mouse,
        i => Dev::Controller(Ctl::new(i)),
    }
}

/// One overlay event, as the neutral event the core reads (events.rs): one for one, in order, with
/// SteamVR's own coordinates and nothing made up. Anything that isn't a pointer, the keyboard or the
/// tab's close is `Other`, with SteamVR's number.
pub fn surface_event(e: &VREvent_t) -> crate::events::SurfaceEvent {
    use crate::events::{Button, KeyboardEvent as K, PointerEvent, PointerKind as P, SurfaceEvent as S};
    let m = unsafe { e.data.mouse }; // what the code read for every pointer event, scrolls too
    let button = |b: u32| match b {
        sys::EVRMouseButton_VRMouseButton_Left => Button::Left,
        sys::EVRMouseButton_VRMouseButton_Right => Button::Right,
        sys::EVRMouseButton_VRMouseButton_Middle => Button::Middle,
        b => Button::Other(b),
    };
    let scroll = |smooth| {
        let s = unsafe { e.data.scroll };
        P::Scroll { dx: s.xdelta, dy: s.ydelta, smooth }
    };
    let kind = match e.eventType {
        sys::EVREventType_VREvent_FocusEnter => P::Enter,
        sys::EVREventType_VREvent_MouseMove => P::Move,
        sys::EVREventType_VREvent_MouseButtonDown => P::Down(button(m.button)),
        sys::EVREventType_VREvent_MouseButtonUp => P::Up(button(m.button)),
        sys::EVREventType_VREvent_ScrollDiscrete => scroll(false),
        sys::EVREventType_VREvent_ScrollSmooth => scroll(true),
        sys::EVREventType_VREvent_FocusLeave => P::Leave,
        t => {
            let k = unsafe { e.data.keyboard };
            return match t {
                sys::EVREventType_VREvent_KeyboardCharInput => S::Keyboard(K::Char { user: k.uUserValue, text: typed(&k.cNewInput) }),
                sys::EVREventType_VREvent_KeyboardDone => S::Keyboard(K::Done { user: k.uUserValue }),
                sys::EVREventType_VREvent_KeyboardClosed => S::Keyboard(K::Closed { user: k.uUserValue }),
                sys::EVREventType_VREvent_OverlayClosed => S::Closed,
                t => S::Other(t),
            };
        }
    };
    S::Pointer(PointerEvent { kind, x: m.x, y: m.y, from: dev(e.trackedDeviceIndex), age: e.eventAgeSeconds })
}

/// SteamVR's own number for a pointer event's kind, for the logs that have always printed it.
pub fn openvr_type(k: crate::events::PointerKind) -> u32 {
    use crate::events::PointerKind as P;
    match k {
        P::Enter => sys::EVREventType_VREvent_FocusEnter,
        P::Move => sys::EVREventType_VREvent_MouseMove,
        P::Down(_) => sys::EVREventType_VREvent_MouseButtonDown,
        P::Up(_) => sys::EVREventType_VREvent_MouseButtonUp,
        P::Scroll { smooth: false, .. } => sys::EVREventType_VREvent_ScrollDiscrete,
        P::Scroll { smooth: true, .. } => sys::EVREventType_VREvent_ScrollSmooth,
        P::Leave => sys::EVREventType_VREvent_FocusLeave,
    }
}

/// SteamVR's keyboard's new characters: the bytes up to a NUL (all 8 without one), if they're
/// UTF-8 and not empty.
pub fn typed(input: &[std::ffi::c_char; 8]) -> Option<String> {
    #[allow(clippy::unnecessary_cast)] // c_char is u8 on the Frame (arm64) but i8 on x86 hosts
    let bytes = input.map(|c| c as u8);
    let n = bytes.iter().position(|&b| b == 0).unwrap_or(8);
    String::from_utf8(bytes[..n].to_vec()).ok().filter(|s| !s.is_empty())
}

/// One of SteamVR's own events (PollNextEvent), as the neutral event the core reads (events.rs):
/// `Other` with its type for one main.rs doesn't act on. A relocalization reads the universe now, as main.rs
/// did, to tell the same room (`Relocalized`) from a new one (`RoomChanged`) against `universe`.
/// A button counts only on a real controller, and wearing only on the headset.
pub fn session_event(e: &VREvent_t, universe: u64) -> crate::events::SessionEvent {
    use crate::events::SessionEvent as S;
    match e.eventType {
        sys::EVREventType_VREvent_Quit => S::Quit,
        sys::EVREventType_VREvent_DashboardActivated => S::DashboardShown(true),
        sys::EVREventType_VREvent_DashboardDeactivated => S::DashboardShown(false),
        sys::EVREventType_VREvent_ButtonPress if is_real_controller(e.trackedDeviceIndex) => S::ControllerUsed(crate::ids::Ctl::new(e.trackedDeviceIndex)),
        sys::EVREventType_VREvent_StandingZeroPoseReset => {
            let mut err = 0;
            let now = crate::call!(sys, GetUint64TrackedDeviceProperty, 0, sys::ETrackedDeviceProperty_Prop_CurrentUniverseId_Uint64, &mut err);
            if now == universe { S::Relocalized } else { S::RoomChanged(now) }
        }
        sys::EVREventType_VREvent_TrackedDeviceUserInteractionStarted if e.trackedDeviceIndex == 0 => S::Worn(true),
        sys::EVREventType_VREvent_TrackedDeviceUserInteractionEnded if e.trackedDeviceIndex == 0 => S::Worn(false),
        t => S::Other(t),
    }
}

/// OpenVR's tracked-device index for `d` (the inverse of `dev`).
pub fn index(d: crate::ids::Dev) -> u32 {
    use crate::ids::Dev;
    match d {
        Dev::Head => 0,
        Dev::Mouse => sys::k_unMaxTrackedDeviceCount as u32,
        Dev::Controller(c) => {
            // made only by `dev`, so never the headset's or the mouse's number: those come back as Head
            // or Mouse. A debug-build guard only, not a runtime check: a release build passes 0 or
            // 64 through to SteamVR, as before.
            debug_assert!(c.raw() != 0 && c.raw() != sys::k_unMaxTrackedDeviceCount as u32, "controller {} is the headset's or the mouse's number", c.raw());
            c.raw()
        }
    }
}

/// The headset's pose in standing space, while it's tracked.
pub fn head() -> Option<Mat> {
    let mut pose: sys::TrackedDevicePose_t = Default::default();
    call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, 0.0, &mut pose, 1);
    pose.bPoseIsValid.then_some(pose.mDeviceToAbsoluteTracking.m)
}

/// The display's rate and delay, as `to_photons` last read them and when. They're read again about
/// once a second (a refresh-rate change in SteamVR's settings shows up within that), not on every
/// pose read. The fake clears it, so a test sees its own script's rate.
static DISPLAY: std::sync::Mutex<Option<(std::time::Instant, f32, f32)>> = std::sync::Mutex::new(None);

/// How far ahead of now this frame's picture reaches your eyes: the rest of this frame, plus the
/// display's vsync-to-photons delay. Poses read at that time line up with what's shown, so a
/// carried or docked panel doesn't trail behind the hand (the scan wants poses at now, `head`).
pub fn to_photons() -> f32 {
    let (mut since, mut frame) = (0f32, 0u64);
    if !call!(sys, GetTimeSinceLastVsync, &mut since, &mut frame) {
        return 0.0;
    }
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
        pub poses: Vec<(u32, Mat)>,                         // each device's pose; a device not here isn't tracked
        pub next: u64,                                      // the last overlay handle handed out
        pub calls: Vec<String>,                             // every write made to it, in order (golden.rs)
        pub roles: Vec<(sys::ETrackedControllerRole, u32)>, // which device has which hand role; none is Invalid
        pub connected: Vec<u32>,                            // on but untracked (a device in poses is on and tracked)
        pub types: Vec<(u32, String)>,                      // a device's controller type (cc_pointer's ends in _pointer); none is ""
        pub vsync: Option<(f32, f32, f32)>,                 // seconds since vsync, display Hz, vsync to photons; none is no vsync, so to_photons is 0
        pub pose_times: Vec<f32>,                           // the predicted times poses were asked for, in order
        pub sys_events: VecDeque<sys::VREvent_t>,           // SteamVR's own queue (PollNextEvent), apart from the overlays'
        pub universe: u64,                                  // Prop_CurrentUniverseId_Uint64
    }

    pub static SCRIPT: Mutex<Option<Script>> = Mutex::new(None);
    /// The fake's been put in place: from then on this test process has no real SteamVR, and
    /// Windows::start, Kwin::start and Capture::start refuse to run.
    static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn on() -> bool {
        ON.load(std::sync::atomic::Ordering::Relaxed)
    }

    thread_local! {
        static HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Whether this thread holds the fake (a replay test, between install and its guard's drop).
    /// windows.rs's session inputs go to their test lists then, and to their real paths otherwise,
    /// so an ordinary unit test still runs the real code.
    pub fn here() -> bool {
        HERE.get()
    }

    /// The fake's guard: other fake-SteamVR tests wait for it, and while this thread holds it
    /// `here()` is true.
    pub struct Guard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Drop for Guard {
        fn drop(&mut self) {
            HERE.set(false);
        }
    }
    /// Held by a test for as long as it uses the fake, since there's one SteamVR per process.
    pub static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    fn script<R>(f: impl FnOnce(&mut Script) -> R) -> R {
        f(SCRIPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(Script::default))
    }

    /// A number as the call log (and golden.rs) writes it: 5 decimals and no -0, so the last bits
    /// of a float, which can differ between x86_64 and aarch64, don't count.
    pub fn num(x: f64) -> String {
        let s = format!("{x:.5}");
        if s.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { s.trim_start_matches('-').to_string() } else { s }
    }

    pub fn mat(m: &Mat) -> String {
        let rows: Vec<String> = m.iter().map(|row| row.iter().map(|&v| num(v as f64)).collect::<Vec<_>>().join(" ")).collect();
        format!("[{}]", rows.join("; "))
    }

    fn log(c: String) {
        script(|s| s.calls.push(c));
    }

    /// The writes made since the last time, in order.
    pub fn take_calls() -> Vec<String> {
        script(|s| std::mem::take(&mut s.calls))
    }

    unsafe extern "C" fn poll(_h: sys::VROverlayHandle_t, e: *mut sys::VREvent_t, _size: u32) -> bool {
        script(|s| s.events.pop_front()).map(|ev| unsafe { *e = ev }).is_some()
    }
    unsafe extern "C" fn poll_sys(e: *mut sys::VREvent_t, _size: u32) -> bool {
        script(|s| s.sys_events.pop_front()).map(|ev| unsafe { *e = ev }).is_some()
    }
    unsafe extern "C" fn u64_prop(_d: u32, p: sys::ETrackedDeviceProperty, _e: *mut sys::ETrackedPropertyError) -> u64 {
        if p == sys::ETrackedDeviceProperty_Prop_CurrentUniverseId_Uint64 { script(|s| s.universe) } else { 0 }
    }
    unsafe extern "C" fn create(_k: *mut std::os::raw::c_char, _n: *mut std::os::raw::c_char, h: *mut sys::VROverlayHandle_t) -> sys::EVROverlayError {
        let n = script(|s| {
            s.next += 1;
            s.next
        });
        unsafe { *h = n };
        log(format!("CreateOverlay -> {n}"));
        0
    }
    unsafe extern "C" fn flag(h: sys::VROverlayHandle_t, f: sys::VROverlayFlags, on: bool) -> sys::EVROverlayError {
        log(format!("SetOverlayFlag {h} {f} {on}"));
        0
    }
    unsafe extern "C" fn sort(h: sys::VROverlayHandle_t, o: u32) -> sys::EVROverlayError {
        log(format!("SetOverlaySortOrder {h} {o}"));
        0
    }
    unsafe extern "C" fn mouse_scale(h: sys::VROverlayHandle_t, s: *mut sys::HmdVector2_t) -> sys::EVROverlayError {
        let v = unsafe { (*s).v };
        log(format!("SetOverlayMouseScale {h} {} {}", num(v[0] as f64), num(v[1] as f64)));
        0
    }
    unsafe extern "C" fn input_method(h: sys::VROverlayHandle_t, m: sys::VROverlayInputMethod) -> sys::EVROverlayError {
        log(format!("SetOverlayInputMethod {h} {m}"));
        0
    }
    unsafe extern "C" fn show(h: sys::VROverlayHandle_t) -> sys::EVROverlayError {
        log(format!("ShowOverlay {h}"));
        0
    }
    unsafe extern "C" fn alpha(h: sys::VROverlayHandle_t, a: f32) -> sys::EVROverlayError {
        log(format!("SetOverlayAlpha {h} {}", num(a as f64)));
        0
    }
    unsafe extern "C" fn role_index(r: sys::ETrackedControllerRole) -> u32 {
        script(|s| s.roles.iter().find(|x| x.0 == r).map_or(sys::k_unTrackedDeviceIndexInvalid as u32, |x| x.1))
    }
    // no vsync unless the script has one: to_photons is then 0, so poses are read at now
    unsafe extern "C" fn vsync(since: *mut f32, _frame: *mut u64) -> bool {
        script(|s| s.vsync).map(|v| unsafe { *since = v.0 }).is_some()
    }
    unsafe extern "C" fn float_prop(_d: u32, p: sys::ETrackedDeviceProperty, _e: *mut sys::ETrackedPropertyError) -> f32 {
        let (_, hz, v2p) = script(|s| s.vsync).unwrap_or_default();
        if p == sys::ETrackedDeviceProperty_Prop_DisplayFrequency_Float {
            hz
        } else if p == sys::ETrackedDeviceProperty_Prop_SecondsFromVsyncToPhotons_Float {
            v2p
        } else {
            0.0
        }
    }
    unsafe extern "C" fn width(h: sys::VROverlayHandle_t, w: f32) -> sys::EVROverlayError {
        log(format!("SetOverlayWidthInMeters {h} {}", num(w as f64)));
        0
    }
    unsafe extern "C" fn curvature(h: sys::VROverlayHandle_t, c: f32) -> sys::EVROverlayError {
        log(format!("SetOverlayCurvature {h} {}", num(c as f64)));
        0
    }
    unsafe extern "C" fn texel(h: sys::VROverlayHandle_t, a: f32) -> sys::EVROverlayError {
        log(format!("SetOverlayTexelAspect {h} {}", num(a as f64)));
        0
    }
    unsafe extern "C" fn bounds(h: sys::VROverlayHandle_t, b: *mut sys::VRTextureBounds_t) -> sys::EVROverlayError {
        let b = unsafe { *b };
        log(format!("SetOverlayTextureBounds {h} {} {} {} {}", num(b.uMin as f64), num(b.vMin as f64), num(b.uMax as f64), num(b.vMax as f64)));
        0
    }
    unsafe extern "C" fn absolute(h: sys::VROverlayHandle_t, o: sys::ETrackingUniverseOrigin, m: *mut sys::HmdMatrix34_t) -> sys::EVROverlayError {
        log(format!("SetOverlayTransformAbsolute {h} {o} {}", mat(unsafe { &(*m).m })));
        0
    }
    unsafe extern "C" fn relative(h: sys::VROverlayHandle_t, dev: u32, m: *mut sys::HmdMatrix34_t) -> sys::EVROverlayError {
        log(format!("SetOverlayTransformTrackedDeviceRelative {h} {dev} {}", mat(unsafe { &(*m).m })));
        0
    }
    unsafe extern "C" fn haptic(h: sys::VROverlayHandle_t, secs: f32, freq: f32, amp: f32) -> sys::EVROverlayError {
        log(format!("TriggerLaserMouseHapticVibration {h} {} {} {}", num(secs as f64), num(freq as f64), num(amp as f64)));
        0
    }
    unsafe extern "C" fn hide(h: sys::VROverlayHandle_t) -> sys::EVROverlayError {
        log(format!("HideOverlay {h}"));
        0
    }
    // the poses don't move with `t`: the script's are where each device is at whatever time is asked
    unsafe extern "C" fn poses(_o: sys::ETrackingUniverseOrigin, t: f32, out: *mut sys::TrackedDevicePose_t, n: u32) {
        let out = unsafe { std::slice::from_raw_parts_mut(out, n as usize) };
        script(|s| {
            s.pose_times.push(t);
            for (dev, m) in &s.poses {
                if let Some(p) = out.get_mut(*dev as usize) {
                    p.mDeviceToAbsoluteTracking.m = *m;
                    (p.bPoseIsValid, p.bDeviceIsConnected) = (true, true);
                }
            }
            for dev in &s.connected {
                if let Some(p) = out.get_mut(*dev as usize) {
                    p.bDeviceIsConnected = true;
                }
            }
        });
    }
    // no render model, so laser_pose gives the pose as it is: a trace records the laser itself
    /// As OpenVR does it: the length the value needs, NUL included, whatever the buffer. A buffer
    /// that's too small (or size 0, to ask for the length) gets nothing written and
    /// TrackedProp_BufferTooSmall. Every caller here passes a 64 or 256 byte buffer, none size 0.
    unsafe extern "C" fn string_prop(d: u32, p: sys::ETrackedDeviceProperty, buf: *mut std::os::raw::c_char, size: u32, e: *mut sys::ETrackedPropertyError) -> u32 {
        let v = if p == sys::ETrackedDeviceProperty_Prop_ControllerType_String { script(|s| s.types.iter().find(|x| x.0 == d).map(|x| x.1.clone())) } else { None };
        let v = v.unwrap_or_default();
        let need = v.len() as u32 + 1;
        let err = if size < need {
            sys::ETrackedPropertyError_TrackedProp_BufferTooSmall
        } else {
            unsafe {
                std::ptr::copy_nonoverlapping(v.as_ptr() as *const std::os::raw::c_char, buf, v.len());
                *buf.add(v.len()) = 0;
            }
            sys::ETrackedPropertyError_TrackedProp_Success
        };
        if !e.is_null() {
            unsafe { *e = err };
        }
        need
    }
    unsafe extern "C" fn class(_d: u32) -> sys::ETrackedDeviceClass {
        sys::ETrackedDeviceClass_TrackedDeviceClass_Controller
    }

    /// Puts the fake in place of SteamVR, once for the process, and starts `s` as its script.
    /// Returns the guard that keeps other fake-SteamVR tests out until it drops. VR is a OnceLock,
    /// and no test calls vr::init (it needs vrserver), so the fake is the only thing ever set in
    /// it. The tables it holds are leaked and the functions in them reach the script only through
    /// SCRIPT, a static, so nothing they touch is ever freed, even by a poll after the guard's
    /// gone. Each install swaps the script in under SCRIPT's lock, while holding the guard, so the
    /// last test's events and poses are gone. Use trace::fake_vr, which also clears what a replay
    /// leaves outside the script.
    pub fn install(s: Script) -> Guard {
        let guard = Guard(ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner()));
        ON.store(true, std::sync::atomic::Ordering::Relaxed);
        HERE.set(true);
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
                HideOverlay: Some(hide),
                SetOverlayAlpha: Some(alpha),
                SetOverlayWidthInMeters: Some(width),
                SetOverlayCurvature: Some(curvature),
                SetOverlayTexelAspect: Some(texel),
                SetOverlayTextureBounds: Some(bounds),
                SetOverlayTransformAbsolute: Some(absolute),
                SetOverlayTransformTrackedDeviceRelative: Some(relative),
                TriggerLaserMouseHapticVibration: Some(haptic),
                ..Default::default()
            }),
            sys: leak(sys::VR_IVRSystem_FnTable {
                GetDeviceToAbsoluteTrackingPose: Some(poses),
                GetStringTrackedDeviceProperty: Some(string_prop),
                GetTrackedDeviceClass: Some(class),
                GetTrackedDeviceIndexForControllerRole: Some(role_index),
                GetTimeSinceLastVsync: Some(vsync),
                GetFloatTrackedDeviceProperty: Some(float_prop),
                GetUint64TrackedDeviceProperty: Some(u64_prop),
                PollNextEvent: Some(poll_sys),
                ..Default::default()
            }),
            ipc: leak(Default::default()),
            input: leak(Default::default()),
            rm: leak(Default::default()),
            apps: leak(Default::default()),
            notes: None,
        });
        // a test that panicked holding it mustn't fail the ones after it
        SCRIPT.clear_poison();
        *SCRIPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(s);
        *DISPLAY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None; // the last test's display rate
        guard
    }
}

#[cfg(test)]
mod tests {
    /// Every OpenVR index comes back from a Dev as itself, so a pose lookup, a release or a
    /// comparison made with a Dev sees the same number it did with the bare index. That includes
    /// the invalid index (u32::MAX) some events carry: a controller, whose pose lookup finds nothing,
    /// as before. Only 0 is the headset and only 64 (one past SteamVR's) is the mouse.
    #[test]
    fn a_device_index_round_trips_through_dev() {
        use crate::ids::{Ctl, Dev};
        use openvr_sys as sys;
        let invalid = sys::k_unTrackedDeviceIndexInvalid as u32;
        for i in [0, 1, 2, 63, sys::k_unMaxTrackedDeviceCount as u32, 65, invalid] {
            assert_eq!(super::index(super::dev(i)), i, "{i}");
        }
        assert_eq!(super::dev(0), Dev::Head);
        assert_eq!(super::dev(64), Dev::Mouse);
        assert_eq!(super::dev(invalid), Dev::Controller(Ctl::new(invalid)), "not the room, not the head: a lookup that finds nothing");
    }

    /// The other way round, a Dev comes back as itself when it's canonical, which every Dev the
    /// code makes is (from `dev`, or Head and Mouse by name).
    #[test]
    fn a_canonical_dev_round_trips_through_its_index() {
        use crate::ids::{Ctl, Dev};
        for d in [Dev::Head, Dev::Mouse, Dev::Controller(Ctl::new(1)), Dev::Controller(Ctl::new(63)), Dev::Controller(Ctl::new(u32::MAX))] {
            assert_eq!(super::dev(super::index(d)), d, "{d:?}");
        }
    }

    /// A controller numbered like the headset isn't one: it would come back as the headset, so
    /// `index` refuses it (in a debug build, which the tests are).
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "the headset's or the mouse's number")]
    fn a_controller_numbered_0_is_refused() {
        super::index(crate::ids::Dev::Controller(crate::ids::Ctl::new(0)));
    }

    /// Each overlay event becomes its neutral event one for one, with SteamVR's own numbers: the
    /// device converted, the mouse data kept for every pointer kind (a scroll's x and y are its
    /// deltas, as the code read them), a button SteamVR doesn't name kept as Other, and anything
    /// else Other by its type.
    #[test]
    fn each_overlay_event_translates_one_for_one() {
        use crate::events::{Button, KeyboardEvent as K, PointerEvent, PointerKind as P, SurfaceEvent as S};
        use crate::ids::{Ctl, Dev};
        use openvr_sys as sys;
        let mouse = |t: u32, b: u32| {
            let mut e: super::VREvent_t = Default::default();
            (e.eventType, e.trackedDeviceIndex, e.eventAgeSeconds) = (t, 3, 0.25);
            e.data.mouse = sys::VREvent_Mouse_t { x: 0.5, y: 0.75, button: b, cursorIndex: 0 };
            e
        };
        let at = |kind| S::Pointer(PointerEvent { kind, x: 0.5, y: 0.75, from: Dev::Controller(Ctl::new(3)), age: 0.25 });
        assert_eq!(super::surface_event(&mouse(sys::EVREventType_VREvent_MouseMove, 0)), at(P::Move));
        assert_eq!(super::surface_event(&mouse(sys::EVREventType_VREvent_MouseButtonDown, sys::EVRMouseButton_VRMouseButton_Left)), at(P::Down(Button::Left)));
        assert_eq!(super::surface_event(&mouse(sys::EVREventType_VREvent_MouseButtonUp, sys::EVRMouseButton_VRMouseButton_Right)), at(P::Up(Button::Right)));
        assert_eq!(super::surface_event(&mouse(sys::EVREventType_VREvent_MouseButtonDown, sys::EVRMouseButton_VRMouseButton_Middle)), at(P::Down(Button::Middle)));
        assert_eq!(super::surface_event(&mouse(sys::EVREventType_VREvent_MouseButtonDown, 8)), at(P::Down(Button::Other(8))));
        assert_eq!(super::surface_event(&mouse(sys::EVREventType_VREvent_FocusEnter, 0)), at(P::Enter));
        assert_eq!(super::surface_event(&mouse(sys::EVREventType_VREvent_FocusLeave, 0)), at(P::Leave));
        let mut s = mouse(sys::EVREventType_VREvent_ScrollDiscrete, 0);
        s.data.scroll = sys::VREvent_Scroll_t { xdelta: 0.0, ydelta: -1.0, unused: 0, viewportscale: 1.0, cursorIndex: 0 };
        assert_eq!(
            super::surface_event(&s),
            S::Pointer(PointerEvent { kind: P::Scroll { dx: 0.0, dy: -1.0, smooth: false }, x: 0.0, y: -1.0, from: Dev::Controller(Ctl::new(3)), age: 0.25 }),
            "a scroll's x and y are its deltas"
        );
        s.eventType = sys::EVREventType_VREvent_ScrollSmooth;
        assert!(matches!(super::surface_event(&s), S::Pointer(PointerEvent { kind: P::Scroll { smooth: true, .. }, .. })));
        let mut k = super::VREvent_t { eventType: sys::EVREventType_VREvent_KeyboardCharInput, ..Default::default() };
        k.data.keyboard.uUserValue = 7;
        for (i, c) in b"ab".iter().enumerate() {
            unsafe { k.data.keyboard.cNewInput[i] = *c as std::ffi::c_char };
        }
        assert_eq!(super::surface_event(&k), S::Keyboard(K::Char { user: 7, text: Some("ab".into()) }));
        k.data.keyboard.cNewInput = [0xffu8 as std::ffi::c_char; 8];
        assert_eq!(super::surface_event(&k), S::Keyboard(K::Char { user: 7, text: None }), "not text");
        k.eventType = sys::EVREventType_VREvent_KeyboardDone;
        assert_eq!(super::surface_event(&k), S::Keyboard(K::Done { user: 7 }));
        k.eventType = sys::EVREventType_VREvent_KeyboardClosed;
        assert_eq!(super::surface_event(&k), S::Keyboard(K::Closed { user: 7 }));
        k.eventType = sys::EVREventType_VREvent_OverlayClosed;
        assert_eq!(super::surface_event(&k), S::Closed);
        k.eventType = sys::EVREventType_VREvent_OverlayFocusChanged;
        assert_eq!(super::surface_event(&k), S::Other(sys::EVREventType_VREvent_OverlayFocusChanged));
    }

    /// A NUL in an overlay's name or a picture's path is an error, not a panic.
    #[test]
    fn a_nul_is_an_error() {
        assert!(super::cstr("controlcenter.panel.desk").is_ok());
        assert!(super::cstr("desk\0wide").is_err());
    }

    /// The fake's string properties answer as OpenVR's do: the length needed, and BufferTooSmall
    /// with nothing written when the buffer's short.
    #[test]
    fn the_fakes_string_property_says_how_long_it_needs() {
        use openvr_sys as sys;
        let _vr = super::fake::install(super::fake::Script { types: vec![(4, "cc_pointer".into())], ..Default::default() });
        let mut err = 0;
        let mut buf = [1 as std::os::raw::c_char; 4];
        let n = crate::call!(sys, GetStringTrackedDeviceProperty, 4, sys::ETrackedDeviceProperty_Prop_ControllerType_String, buf.as_mut_ptr(), 0, &mut err);
        assert_eq!((n, err), (11, sys::ETrackedPropertyError_TrackedProp_BufferTooSmall), "size 0 asks for the length");
        let n = crate::call!(sys, GetStringTrackedDeviceProperty, 4, sys::ETrackedDeviceProperty_Prop_ControllerType_String, buf.as_mut_ptr(), buf.len() as u32, &mut err);
        assert_eq!((n, err, buf[0]), (11, sys::ETrackedPropertyError_TrackedProp_BufferTooSmall, 1), "too short: nothing written");
        assert_eq!(super::string_prop(4, sys::ETrackedDeviceProperty_Prop_ControllerType_String), "cc_pointer");
        assert!(!super::is_real_controller(4) && super::is_real_controller(5), "a _pointer type isn't a real controller");
    }
}
