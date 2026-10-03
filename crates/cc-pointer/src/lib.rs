//! cc_pointer is an invisible SteamVR controller that cc-panels aims along its own mouse ray, so
//! SteamVR's laser can reach UI we don't own (the dashboard, Steam menus, other overlays). It's a
//! dumb shim on purpose: all the logic lives in cc-panels (laser.rs). See docs/laser-pointer-design.md.
//!
//! Control socket: abstract unix datagram "@cc_pointer", same uid only (SCM_CREDENTIALS):
//!   `L <seq> <L|R> px py pz qw qx qy qz <btnmask> <sx> <sy>`
//!       a lease: the full state, pose in raw (uncalibrated) space, a finished quaternion.
//!       btnmask: 1 trigger, 2 b, 4 x, 8 a, 16 system, 32 joystick click; sx sy -1..1.
//!   `H`   hide now.
//! The driver only keeps the last lease and when it came, so a lost packet can't strand a button or
//! a hand role. cc-panels re-sends it every 20 ms, and 300 ms without one is a watchdog drop, so
//! kill -9, SIGSTOP or a hung render loop all give the hand back.
//! CC_POINTER_SOCKET=<name> listens on @<name> instead. That's for tests only, so they never reach
//! the live driver (namespaces don't isolate abstract sockets on the Frame).
//!
//! The OpenVR driver API is C++ classes, so this speaks their vtables directly: #[repr(C)] tables
//! of extern "C" fns with `this` first, which is the Itanium ABI on aarch64/x86_64 Linux. The
//! versions match the openvr_driver.h it was written against (SteamVR 2.1 header, see
//! INTERFACE_VERSIONS). None of the interfaces have virtual destructors. Nothing here can panic,
//! because a panic across extern "C" aborts and takes vrserver down with it.
#![allow(non_snake_case)]

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering::SeqCst};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;

// --- openvr_driver.h -------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct Quat {
    w: f64,
    x: f64,
    y: f64,
    z: f64,
}

/// vr::DriverPose_t (natural alignment: it's outside the header's #pragma pack regions).
#[repr(C)]
#[derive(Clone, Copy)]
struct DriverPose {
    pose_time_offset: f64,
    q_world_from_driver_rotation: Quat,
    vec_world_from_driver_translation: [f64; 3],
    q_driver_from_head_rotation: Quat,
    vec_driver_from_head_translation: [f64; 3],
    vec_position: [f64; 3],
    vec_velocity: [f64; 3],
    vec_acceleration: [f64; 3],
    q_rotation: Quat,
    vec_angular_velocity: [f64; 3],
    vec_angular_acceleration: [f64; 3],
    result: i32,
    pose_is_valid: bool,
    will_drift_in_yaw: bool,
    should_apply_head_model: bool,
    device_is_connected: bool,
}
const _: () = assert!(size_of::<DriverPose>() == 280);
const POSE0: DriverPose = unsafe { std::mem::zeroed() };

/// vr::PropertyWrite_t
#[repr(C)]
struct PropertyWrite {
    prop: i32,
    write_type: i32,
    set_error: i32,
    buffer: *const c_void,
    buffer_size: u32,
    tag: u32,
    error: i32,
}

type This = *mut c_void;

#[repr(C)]
struct ContextVtbl {
    get_generic_interface: unsafe extern "C" fn(This, *const c_char, *mut i32) -> *mut c_void,
    get_driver_handle: unsafe extern "C" fn(This) -> u64,
}
/// IVRServerDriverHost_006, the slots we call (the rest follow them).
#[repr(C)]
struct HostVtbl {
    tracked_device_added: unsafe extern "C" fn(This, *const c_char, i32, This) -> bool,
    tracked_device_pose_updated: unsafe extern "C" fn(This, u32, *const DriverPose, u32),
}
/// IVRProperties_001
#[repr(C)]
struct PropertiesVtbl {
    read_property_batch: unsafe extern "C" fn(This, u64, *mut c_void, u32) -> i32,
    write_property_batch: unsafe extern "C" fn(This, u64, *mut PropertyWrite, u32) -> i32,
    get_prop_error_name_from_enum: unsafe extern "C" fn(This, i32) -> *const c_char,
    tracked_device_to_property_container: unsafe extern "C" fn(This, u32) -> u64,
}
/// IVRDriverInput_003, the slots we call (haptic and skeleton follow them).
#[repr(C)]
struct InputVtbl {
    create_boolean_component: unsafe extern "C" fn(This, u64, *const c_char, *mut u64) -> i32,
    update_boolean_component: unsafe extern "C" fn(This, u64, bool, f64) -> i32,
    create_scalar_component: unsafe extern "C" fn(This, u64, *const c_char, *mut u64, i32, i32) -> i32,
    update_scalar_component: unsafe extern "C" fn(This, u64, f32, f64) -> i32,
}
/// IVRDriverLog_001
#[repr(C)]
struct LogVtbl {
    log: unsafe extern "C" fn(This, *const c_char),
}
/// IServerTrackedDeviceProvider_004: what we implement for SteamVR.
#[repr(C)]
struct ProviderVtbl {
    init: unsafe extern "C" fn(This, This) -> i32,
    cleanup: unsafe extern "C" fn(This),
    get_interface_versions: unsafe extern "C" fn(This) -> *const *const c_char,
    run_frame: unsafe extern "C" fn(This),
    should_block_standby_mode: unsafe extern "C" fn(This) -> bool,
    enter_standby: unsafe extern "C" fn(This),
    leave_standby: unsafe extern "C" fn(This),
}
/// ITrackedDeviceServerDriver_005
#[repr(C)]
struct DeviceVtbl {
    activate: unsafe extern "C" fn(This, u32) -> i32,
    deactivate: unsafe extern "C" fn(This),
    enter_standby: unsafe extern "C" fn(This),
    get_component: unsafe extern "C" fn(This, *const c_char) -> *mut c_void,
    debug_request: unsafe extern "C" fn(This, *const c_char, *mut c_char, u32),
    get_pose: unsafe extern "C" fn(This) -> DriverPose,
}

/// A C++ object as the other side sees it: a vtable pointer and nothing else we need.
#[repr(C)]
struct Obj<V: 'static> {
    vtbl: &'static V,
}

/// The vtable of a runtime object (non-null).
unsafe fn vt<V>(p: This) -> &'static V {
    unsafe { &**(p as *const *const V) }
}

const INIT_OK: i32 = 0;
const INIT_INTERFACE_NOT_FOUND: i32 = 105;
const DEVICE_CLASS_CONTROLLER: i32 = 2;
const ROLE_LEFT: i32 = 1;
const ROLE_RIGHT: i32 = 2;
const ROLE_OPT_OUT: i32 = 3;
const TRACKING_UNINITIALIZED: i32 = 1;
const TRACKING_RUNNING_OK: i32 = 200;
const INVALID_DEVICE: u32 = 0xFFFF_FFFF;
const SCALAR_ABSOLUTE: i32 = 0;
const SCALAR_NORMALIZED_TWO_SIDED: i32 = 1;
const TAG_INT32: u32 = 2;
const TAG_BOOL: u32 = 4;
const TAG_STRING: u32 = 5;
const PROP_MODEL_NUMBER: i32 = 1001;
const PROP_RENDER_MODEL_NAME: i32 = 1003;
const PROP_MANUFACTURER_NAME: i32 = 1005;
const PROP_DEVICE_CLASS: i32 = 1029;
const PROP_DRIVER_VERSION: i32 = 1031;
const PROP_INPUT_PROFILE_PATH: i32 = 1037;
const PROP_NEVER_TRACKED: i32 = 1038;
const PROP_CONTROLLER_ROLE_HINT: i32 = 3007;
const PROP_CONTROLLER_TYPE: i32 = 7000;
const PROP_HAND_SELECTION_PRIORITY: i32 = 7002;

const PROVIDER_VERSION: &CStr = c"IServerTrackedDeviceProvider_004";

/// vr::k_InterfaceVersions: every interface version the header we match was built from.
struct Versions([*const c_char; 12]);
unsafe impl Sync for Versions {}
static INTERFACE_VERSIONS: Versions = Versions([
    c"IVRSettings_003".as_ptr(),
    c"ITrackedDeviceServerDriver_005".as_ptr(),
    c"IVRDisplayComponent_003".as_ptr(),
    c"IVRDriverDirectModeComponent_008".as_ptr(),
    c"IVRCameraComponent_003".as_ptr(),
    PROVIDER_VERSION.as_ptr(),
    c"IVRWatchdogProvider_001".as_ptr(),
    c"IVRVirtualDisplay_002".as_ptr(),
    c"IVRDriverManager_001".as_ptr(),
    c"IVRResources_001".as_ptr(),
    c"IVRCompositorPluginProvider_001".as_ptr(),
    std::ptr::null(),
]);

// --- the driver context (VR_INIT_SERVER_DRIVER_CONTEXT and the VRxxx() accessors) -------------

static CONTEXT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
const HOST: usize = 0;
const PROPS: usize = 2;
const LOG: usize = 3;
const INPUT: usize = 6;
/// InitServer fetches the first six in this order and fails if one is missing. Input is fetched lazily.
const IFACES: [&CStr; 7] = [
    c"IVRServerDriverHost_006",
    c"IVRSettings_003",
    c"IVRProperties_001",
    c"IVRDriverLog_001",
    c"IVRDriverManager_001",
    c"IVRResources_001",
    c"IVRDriverInput_003",
];
static SLOTS: [AtomicPtr<c_void>; 7] = [const { AtomicPtr::new(std::ptr::null_mut()) }; 7];

/// The runtime's interface, fetched once (null if there's none).
fn iface(i: usize) -> This {
    let p = SLOTS[i].load(SeqCst);
    if !p.is_null() {
        return p;
    }
    let ctx = CONTEXT.load(SeqCst);
    if ctx.is_null() {
        return p;
    }
    let mut err = 0;
    let p = unsafe { (vt::<ContextVtbl>(ctx).get_generic_interface)(ctx, IFACES[i].as_ptr(), &mut err) };
    SLOTS[i].store(p, SeqCst);
    p
}

fn clear_context() {
    for s in &SLOTS {
        s.store(std::ptr::null_mut(), SeqCst);
    }
}

fn log(msg: &CStr) {
    let p = iface(LOG);
    if !p.is_null() {
        unsafe { (vt::<LogVtbl>(p).log)(p, msg.as_ptr()) }
    }
}

/// CVRPropertyHelpers::SetProperty: a batch of one.
fn set_prop(container: u64, prop: i32, buffer: *const c_void, buffer_size: u32, tag: u32) {
    let p = iface(PROPS);
    if p.is_null() {
        return;
    }
    let mut w = PropertyWrite { prop, write_type: 0, set_error: 0, buffer, buffer_size, tag, error: 0 };
    unsafe { (vt::<PropertiesVtbl>(p).write_property_batch)(p, container, &mut w, 1) };
}
fn set_string(c: u64, prop: i32, v: &CStr) {
    set_prop(c, prop, v.as_ptr().cast(), v.to_bytes_with_nul().len() as u32, TAG_STRING);
}
fn set_i32(c: u64, prop: i32, v: i32) {
    set_prop(c, prop, (&v as *const i32).cast(), 4, TAG_INT32);
}
fn set_bool(c: u64, prop: i32, v: bool) {
    set_prop(c, prop, (&v as *const bool).cast(), 1, TAG_BOOL);
}

/// A poisoned lock still hands out its state (it can't happen anyway, since nothing panics).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> i64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec * 1000 + t.tv_nsec / 1_000_000
}

// --- the lease and the arbiter (pure) ---------------------------------------------------------

const BUTTON_PATHS: [&CStr; 6] = [
    c"/input/trigger/click",
    c"/input/b/click",
    c"/input/x/click",
    c"/input/a/click",
    c"/input/system/click",
    c"/input/joystick/click",
]; // btnmask bit i

#[derive(Clone, Copy, Debug, PartialEq)]
struct Lease {
    at: i64,
    hidden: bool, // "H" since the last lease
    hand: i32,
    pose: [f32; 7], // px py pz qw qx qy qz
    buttons: u32,
    sx: f32,
    sy: f32,
}
const LEASE0: Lease =
    Lease { at: 0, hidden: true, hand: ROLE_RIGHT, pose: [0., 0., 0., 1., 0., 0., 0.], buttons: 0, sx: 0., sy: 0. };

impl Lease {
    fn finite(&self) -> bool {
        self.pose.iter().all(|v| v.is_finite()) && self.sx.is_finite() && self.sy.is_finite()
    }
}

/// The connection state, worked out purely from the last lease and the clock. The device applies it.
#[derive(Clone, Copy)]
struct Arbiter {
    init: i64, // ms
    last_connect: i64,
    backoff_until: i64,
    hinted: bool, // the role hint names `hand`
    connected: bool,
    hand: i32,
}
const ARBITER0: Arbiter =
    Arbiter { init: 0, last_connect: 0, backoff_until: 0, hinted: false, connected: false, hand: ROLE_RIGHT };

impl Arbiter {
    fn start(&mut self, now: i64) {
        self.init = now;
        self.last_connect = now;
        self.backoff_until = now;
    }

    fn step(&mut self, now: i64, l: &Lease) {
        let alive = !l.hidden && now - l.at < 300;
        // 20 s startup guard: holding a hand while the Steam UI loads left it stuck loading.
        // The hand stays locked while hinted or connected, because laser.rs's pick flips as real
        // controllers come and go, and re-hinting in place is an uncounted binding load. A switch releases
        // (no back-off), and the new hand goes through the limiter and a fresh hint frame.
        let switched = (self.hinted || self.connected) && l.hand != self.hand;
        let wanted = alive && now - self.init >= 20000 && l.finite() && now >= self.backoff_until && !switched;
        if !wanted {
            if self.connected && !l.hidden && !alive {
                self.backoff_until = now + 2000; // watchdog drop
            }
            self.hinted = false;
            self.connected = false;
            return;
        }
        self.hand = l.hand;
        if self.connected {
            return;
        }
        // Every connect is a binding load and SteamVR caps those ("Too many binding loads"), so I
        // allow at most one per 10 s. Hint this frame, connect the next.
        if !self.hinted {
            self.hinted = now - self.last_connect >= 10000; // init counts: the 20 s guard covers it
            return;
        }
        self.connected = true;
        self.last_connect = now;
    }
}

struct Shared {
    lease: Lease,
    latched: u32, // every bit seen pressed since the last frame, so a tap lasts a frame
}
static SHARED: Mutex<Shared> = Mutex::new(Shared { lease: LEASE0, latched: 0 });

/// The C++ sscanf("L %u %c %f %f %f %f %f %f %f %u %f %f%n") and its end check, done by tokens:
/// whitespace-separated numbers, one char for the hand, and nothing after but an optional newline.
fn parse_lease(msg: &str) -> Option<Lease> {
    fn token<'a>(rest: &mut &'a str) -> Option<&'a str> {
        let t = rest.trim_start();
        let n = t.find(|c: char| c.is_ascii_whitespace()).unwrap_or(t.len());
        *rest = &t[n..];
        Some(&t[..n]).filter(|t| !t.is_empty())
    }
    fn num<T: std::str::FromStr>(rest: &mut &str) -> Option<T> {
        token(rest)?.parse().ok()
    }
    let r = &mut msg.strip_prefix('L')?;
    num::<i64>(r)?; // seq (%u: a sign is fine)
    let mut t = r.trim_start().chars();
    let hand = match t.next()? {
        'L' => ROLE_LEFT,
        'R' => ROLE_RIGHT,
        _ => return None,
    };
    *r = t.as_str();
    let mut l = LEASE0;
    for v in &mut l.pose {
        *v = num(r)?;
    }
    l.buttons = num::<i64>(r)? as u32 & ((1 << BUTTON_PATHS.len()) - 1); // %u wraps a minus sign
    l.sx = num(r)?;
    l.sy = num(r)?;
    if !(r.is_empty() || r.starts_with('\n')) {
        return None;
    }
    l.hand = hand;
    let clamp = |v: f32| if v < -1. { -1. } else if v > 1. { 1. } else { v }; // NaN passes; finite() rejects it
    (l.sx, l.sy) = (clamp(l.sx), clamp(l.sy));
    l.hidden = false;
    Some(l)
}

/// Parses one datagram (up to its first NUL, like the C string it used to be) into `s`. Returns
/// false and leaves the state alone on anything malformed.
fn handle(s: &Mutex<Shared>, msg: &[u8], now: i64) -> bool {
    let msg = &msg[..msg.iter().position(|&b| b == 0).unwrap_or(msg.len())];
    if msg == b"H" || msg.starts_with(b"H\n") {
        lock(s).lease.hidden = true;
        return true;
    }
    let Some(mut l) = std::str::from_utf8(msg).ok().and_then(parse_lease) else { return false };
    l.at = now;
    let mut s = lock(s);
    s.lease = l;
    s.latched |= l.buttons;
    true
}

// --- the device -------------------------------------------------------------------------------

struct Device {
    arb: Arbiter,
    object_id: u32,
    container: u64,
    hint: i32, // -1: nothing set yet
    pose: DriverPose,
    buttons: [u64; 6],
    sx: u64,
    sy: u64,
}
// Host calls happen with this unlocked, because SteamVR may call back into the device from them.
static DEVICE: Mutex<Device> = Mutex::new(Device {
    arb: ARBITER0,
    object_id: INVALID_DEVICE,
    container: 0,
    hint: -1,
    pose: POSE0,
    buttons: [0; 6],
    sx: 0,
    sy: 0,
});

static DEVICE_VTBL: DeviceVtbl = DeviceVtbl {
    activate: device_activate,
    deactivate: device_deactivate,
    enter_standby: noop,
    get_component: device_get_component,
    debug_request: device_debug_request,
    get_pose: device_get_pose,
};
static DEVICE_OBJ: Obj<DeviceVtbl> = Obj { vtbl: &DEVICE_VTBL };

unsafe extern "C" fn noop(_: This) {}

/// Always the lowest priority: we only ever take a free hand, and a real controller that wakes up
/// takes its hand back. Stage 3 (2026-10-01) showed why: hinting the left hand at a high priority
/// while the left controller was on stripped BOTH real controllers of their roles, and the right
/// one stayed role-less until someone pressed a button on it.
fn set_hint(role: i32) {
    let c = {
        let mut d = lock(&DEVICE);
        if d.hint == role {
            return;
        }
        d.hint = role;
        d.container
    };
    set_i32(c, PROP_HAND_SELECTION_PRIORITY, -1_000_000);
    set_i32(c, PROP_CONTROLLER_ROLE_HINT, role);
}

unsafe extern "C" fn device_activate(_: This, object_id: u32) -> i32 {
    let props = iface(PROPS);
    let c = if props.is_null() {
        0
    } else {
        unsafe { (vt::<PropertiesVtbl>(props).tracked_device_to_property_container)(props, object_id) }
    };
    {
        let mut d = lock(&DEVICE);
        d.object_id = object_id;
        d.container = c;
    }
    set_string(c, PROP_MODEL_NUMBER, c"cc_pointer");
    set_string(c, PROP_MANUFACTURER_NAME, c"Command Center");
    set_string(c, PROP_CONTROLLER_TYPE, c"cc_pointer");
    set_string(c, PROP_DRIVER_VERSION, c"ccp/1"); // cc-panels' protocol gate
    set_string(c, PROP_INPUT_PROFILE_PATH, c"{cc_pointer}/input/cc_pointer_profile.json");
    set_string(c, PROP_RENDER_MODEL_NAME, c"{cc_pointer}/rendermodels/cc_pointer_invisible");
    set_i32(c, PROP_DEVICE_CLASS, DEVICE_CLASS_CONTROLLER);
    set_bool(c, PROP_NEVER_TRACKED, false);
    set_hint(ROLE_OPT_OUT); // disconnected is always OptOut

    let input = iface(INPUT);
    if !input.is_null() {
        let f = unsafe { vt::<InputVtbl>(input) };
        let mut buttons = [0u64; 6];
        let (mut sx, mut sy) = (0u64, 0u64);
        for (path, h) in BUTTON_PATHS.iter().zip(&mut buttons) {
            unsafe { (f.create_boolean_component)(input, c, path.as_ptr(), h) };
        }
        let (abs, two) = (SCALAR_ABSOLUTE, SCALAR_NORMALIZED_TWO_SIDED);
        unsafe { (f.create_scalar_component)(input, c, c"/input/joystick/x".as_ptr(), &mut sx, abs, two) };
        unsafe { (f.create_scalar_component)(input, c, c"/input/joystick/y".as_ptr(), &mut sy, abs, two) };
        let mut d = lock(&DEVICE);
        (d.buttons, d.sx, d.sy) = (buttons, sx, sy);
    }
    log(c"cc_pointer: activated (ccp/1)");
    INIT_OK
}

unsafe extern "C" fn device_deactivate(_: This) {
    lock(&DEVICE).object_id = INVALID_DEVICE;
}

unsafe extern "C" fn device_get_component(_: This, _: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}

unsafe extern "C" fn device_debug_request(_: This, _: *const c_char, response: *mut c_char, size: u32) {
    if size != 0 && !response.is_null() {
        unsafe { *response = 0 };
    }
}

unsafe extern "C" fn device_get_pose(_: This) -> DriverPose {
    lock(&DEVICE).pose
}

fn device_run_frame() {
    let mut d = lock(&DEVICE);
    if d.object_id == INVALID_DEVICE {
        return;
    }
    let (l, latched) = {
        let mut s = lock(&SHARED);
        let r = (s.lease, s.latched);
        s.latched = 0;
        r
    };
    let was = d.arb.connected;
    d.arb.step(now_ms(), &l);
    let on = d.arb.connected;
    let hint = if d.arb.hinted { d.arb.hand } else { ROLE_OPT_OUT };

    let mut pose = POSE0;
    pose.q_world_from_driver_rotation.w = 1.; // driver space = raw space
    pose.q_driver_from_head_rotation.w = 1.;
    if on {
        let p = l.pose.map(f64::from);
        pose.vec_position = [p[0], p[1], p[2]];
        pose.q_rotation = Quat { w: p[3], x: p[4], y: p[5], z: p[6] };
    } else {
        pose.q_rotation.w = 1.;
    }
    pose.pose_is_valid = on;
    pose.result = if on { TRACKING_RUNNING_OK } else { TRACKING_UNINITIALIZED };
    pose.device_is_connected = on;
    d.pose = pose;
    let (id, buttons, sx, sy) = (d.object_id, d.buttons, d.sx, d.sy);
    drop(d);

    if on != was {
        log(if on { c"cc_pointer: connected" } else { c"cc_pointer: released" });
    }
    // Hint before connecting, and OptOut in the same frame as disconnecting, because SteamVR keeps a
    // hand reserved for a disconnected device that still hints it.
    set_hint(hint);
    let host = iface(HOST);
    if !host.is_null() {
        unsafe { (vt::<HostVtbl>(host).tracked_device_pose_updated)(host, id, &pose, size_of::<DriverPose>() as u32) };
    }

    // Disconnected means every input zero, in the same frame.
    let mask = if on { l.buttons | latched } else { 0 };
    let input = iface(INPUT);
    if input.is_null() {
        return;
    }
    let f = unsafe { vt::<InputVtbl>(input) };
    for (i, h) in buttons.iter().enumerate() {
        unsafe { (f.update_boolean_component)(input, *h, (mask >> i) & 1 != 0, 0.) };
    }
    unsafe { (f.update_scalar_component)(input, sx, if on { l.sx } else { 0. }, 0.) };
    unsafe { (f.update_scalar_component)(input, sy, if on { l.sy } else { 0. }, 0.) };
}

// --- the provider -----------------------------------------------------------------------------

static RUNNING: AtomicBool = AtomicBool::new(false);
static LISTENER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

static PROVIDER_VTBL: ProviderVtbl = ProviderVtbl {
    init: provider_init,
    cleanup: provider_cleanup,
    get_interface_versions: provider_get_interface_versions,
    run_frame: provider_run_frame,
    should_block_standby_mode: provider_should_block_standby_mode,
    enter_standby: noop,
    leave_standby: noop,
};
static PROVIDER: Obj<ProviderVtbl> = Obj { vtbl: &PROVIDER_VTBL };

unsafe extern "C" fn provider_init(_: This, context: This) -> i32 {
    CONTEXT.store(context, SeqCst);
    clear_context();
    if (0..6).any(|i| iface(i).is_null()) {
        return INIT_INTERFACE_NOT_FOUND;
    }
    lock(&DEVICE).arb.start(now_ms());
    let host = iface(HOST);
    let device = &DEVICE_OBJ as *const Obj<DeviceVtbl> as This;
    unsafe { (vt::<HostVtbl>(host).tracked_device_added)(host, c"cc_pointer_0".as_ptr(), DEVICE_CLASS_CONTROLLER, device) };
    RUNNING.store(true, SeqCst);
    *lock(&LISTENER) = std::thread::Builder::new().spawn(listen).ok();
    INIT_OK
}

unsafe extern "C" fn provider_cleanup(_: This) {
    RUNNING.store(false, SeqCst);
    if let Some(t) = lock(&LISTENER).take() {
        let _ = t.join(); // recv times out every 200 ms
    }
    CONTEXT.store(std::ptr::null_mut(), SeqCst);
    clear_context();
}

unsafe extern "C" fn provider_get_interface_versions(_: This) -> *const *const c_char {
    INTERFACE_VERSIONS.0.as_ptr()
}

unsafe extern "C" fn provider_run_frame(_: This) {
    device_run_frame();
}

unsafe extern "C" fn provider_should_block_standby_mode(_: This) -> bool {
    false
}

fn listen() {
    let name = std::env::var_os("CC_POINTER_SOCKET").unwrap_or_else(|| "cc_pointer".into());
    let name = std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str());
    // SAFETY: plain socket calls on our own fd and stack buffers.
    unsafe {
        let sock = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let ok_name = !name.is_empty() && name.len() < addr.sun_path.len();
        for (d, s) in addr.sun_path[1..].iter_mut().zip(name) {
            *d = *s as c_char; // abstract namespace: a leading NUL
        }
        let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + 1 + name.len()) as libc::socklen_t;
        let on: libc::c_int = 1;
        let tv = libc::timeval { tv_sec: 0, tv_usec: 200_000 };
        let opt = |o, v: *const c_void, n: usize| libc::setsockopt(sock, libc::SOL_SOCKET, o, v, n as libc::socklen_t);
        if sock < 0
            || !ok_name
            || libc::bind(sock, (&addr as *const libc::sockaddr_un).cast(), len) != 0
            || opt(libc::SO_PASSCRED, (&on as *const libc::c_int).cast(), size_of::<libc::c_int>()) != 0
            || opt(libc::SO_RCVTIMEO, (&tv as *const libc::timeval).cast(), size_of::<libc::timeval>()) != 0
        {
            log(c"cc_pointer: cannot open control socket");
            if sock >= 0 {
                libc::close(sock);
            }
            return;
        }
        let uid = libc::getuid();
        while RUNNING.load(SeqCst) {
            let mut buf = [0u8; 255];
            let mut ctrl = [0u64; 8]; // >= CMSG_SPACE(sizeof(ucred)), cmsghdr-aligned
            let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = ctrl.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(size_of::<libc::ucred>() as u32) as usize;
            let n = libc::recvmsg(sock, &mut msg, 0);
            if n <= 0 || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
                continue;
            }
            let c = libc::CMSG_FIRSTHDR(&msg);
            if c.is_null() || (*c).cmsg_level != libc::SOL_SOCKET || (*c).cmsg_type != libc::SCM_CREDENTIALS {
                continue;
            }
            let cred: libc::ucred = std::ptr::read_unaligned(libc::CMSG_DATA(c).cast());
            if cred.uid != uid {
                continue; // another user can't steer our hands
            }
            handle(&SHARED, &buf[..n as usize], now_ms());
        }
        libc::close(sock);
    }
}

/// The driver's entry point: SteamVR asks for the provider by interface version.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HmdDriverFactory(interface_name: *const c_char, return_code: *mut i32) -> *mut c_void {
    if !interface_name.is_null() && unsafe { CStr::from_ptr(interface_name) } == PROVIDER_VERSION {
        return &PROVIDER as *const Obj<ProviderVtbl> as *mut c_void;
    }
    if !return_code.is_null() {
        unsafe { *return_code = INIT_INTERFACE_NOT_FOUND };
    }
    std::ptr::null_mut()
}

#[cfg(test)]
mod tests {
    // Same as the C++ selftest: the arbiter and parser, no SteamVR needed.
    use super::*;

    #[test]
    fn selftest() {
        let t0 = 1_000_000;
        let at = |ms: i64| t0 + ms;
        let s = Mutex::new(Shared { lease: LEASE0, latched: 0 });
        let mut a = ARBITER0;
        a.start(t0);
        let lease_msg = |ms, msg: &str| assert!(handle(&s, msg.as_bytes(), at(ms)));
        let lease = |ms| lease_msg(ms, "L 1 R 0 1.5 0 1 0 0 0 0 0 0");
        let step = |a: &mut Arbiter, ms| a.step(at(ms), &lock(&s).lease);

        lease(19000);
        step(&mut a, 19100);
        assert!(!a.hinted && !a.connected); // startup guard
        lease(20000);
        step(&mut a, 20000);
        assert!(a.hinted && !a.connected && a.hand == ROLE_RIGHT); // hint first
        step(&mut a, 20010);
        assert!(a.connected); // connect the next frame
        step(&mut a, 20400);
        assert!(!a.hinted && !a.connected); // watchdog
        lease(21000);
        step(&mut a, 21000);
        assert!(!a.hinted); // 2 s back-off
        lease(23000);
        step(&mut a, 23000);
        assert!(!a.hinted); // one connect per 10 s
        lease(30020);
        step(&mut a, 30020);
        step(&mut a, 30030);
        assert!(a.connected);
        lease_msg(30040, "L 2 L 0 1.5 0 1 0 0 0 1 0 0");
        step(&mut a, 30040);
        assert!(!a.hinted && !a.connected); // a hand switch releases, no re-hint in place
        lease_msg(30050, "L 2 L 0 1.5 0 1 0 0 0 1 0 0");
        step(&mut a, 30050);
        assert!(!a.hinted); // and the new hand waits out the 10 s limiter
        lease_msg(40030, "L 2 L 0 1.5 0 1 0 0 0 1 0 0");
        step(&mut a, 40030);
        assert!(a.hinted && a.hand == ROLE_LEFT);
        lease(40040);
        step(&mut a, 40040);
        assert!(!a.hinted && !a.connected); // a switch in the hint frame drops the hint
        lease(40050);
        step(&mut a, 40050);
        assert!(a.hinted && a.hand == ROLE_RIGHT); // fresh hint, no back-off
        step(&mut a, 40060);
        assert!(a.connected);
        assert!(handle(&s, b"H", at(40070)));
        step(&mut a, 40070);
        assert!(!a.connected);
        lease(50070);
        step(&mut a, 50070);
        assert!(a.hinted); // H is no watchdog drop: no back-off
        step(&mut a, 50080);
        lease_msg(50090, "L 3 R nan 0 0 1 0 0 0 0 0 0");
        step(&mut a, 50090);
        assert!(!a.connected); // non-finite pose

        assert!(!handle(&s, b"L 4 X 0 0 0 1 0 0 0 0 0 0", at(0)));
        assert!(!handle(&s, b"L 4 R 0 0 0 1 0 0 0 0 0", at(0)));
        assert!(!handle(&s, b"L 4 R 0 0 0 1 0 0 0 0 0 0 junk", at(0)));
        assert!(!handle(&s, b"Hide", at(0)));
        lock(&s).latched = 0;
        lease_msg(0, "L 5 R 0 0 0 1 0 0 0 1 0 0");
        lease_msg(1, "L 6 R 0 0 0 1 0 0 0 0 0 0");
        assert!(lock(&s).latched == 1 && lock(&s).lease.buttons == 0); // a tap between frames still shows
    }

    #[test]
    fn parser_edges() {
        let l = parse_lease("L 7 R 0.10000 -1.25000 0.33333 1.000000 0.000000 -0.500000 0.250000 73 2.000 -1.000\n").unwrap();
        assert_eq!(l.pose, [0.1, -1.25, 0.33333, 1., 0., -0.5, 0.25]);
        assert_eq!((l.hand, l.buttons, l.sx, l.sy, l.hidden), (ROLE_RIGHT, 73 & 63, 1., -1., false));
        assert!(parse_lease("L 7 L NaN 0 0 1 0 0 0 0 0 0").is_some_and(|l| !l.finite() && l.hand == ROLE_LEFT));
        assert!(parse_lease("L7 R0 0 0 1 0 0 0 0 0 0").is_some()); // sscanf's spaces match none too
        assert!(parse_lease("L 7 R 0 0 0 1 0 0 0 0 0 0 ").is_none()); // only a newline may follow
        assert!(parse_lease("L 7 R 0 0 0 1 0 0 0 0 0 nan").is_some_and(|l| l.sy.is_nan()));
        let s = Mutex::new(Shared { lease: LEASE0, latched: 0 });
        assert!(handle(&s, b"H\0junk", 0) && handle(&s, b"H\n", 0) && !handle(&s, b"", 0));
    }
}
