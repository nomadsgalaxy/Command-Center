//! Loads the built driver .so the way vrserver does (dlopen, HmdDriverFactory), drives it through a
//! fake runtime whose interfaces are hand-written vtables that record every call, then talks to its
//! control socket. No SteamVR involved. It takes ~21 s because the driver's 20 s startup guard is
//! real time, so it's opt-in:
//!   cargo build --release -p cc-pointer && cargo test --release -p cc-pointer -- --ignored
//! CC_POINTER_CXX_SO=<path> also runs the old C++ driver, built with its socket renamed to
//! @ccp_harness_cxx, and requires the two call sequences to be identical.
//! The socket names are private on purpose: a test must never reach the live driver's @cc_pointer.
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Quat {
    w: f64,
    x: f64,
    y: f64,
    z: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
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

type This = *mut Obj;
/// A fake runtime object: its vtable, then (invisible to the driver) the recorder.
#[repr(C)]
struct Obj {
    vtbl: *const c_void,
    rec: *const Rec,
}
#[derive(Default)]
struct Rec {
    calls: Mutex<Vec<String>>,
    next_handle: Mutex<u64>,
    device: Mutex<usize>,
    missing: Option<&'static str>, // an interface GetGenericInterface won't hand out
    objs: Vec<(&'static str, usize)>,
}
fn push(this: This, s: String) {
    unsafe { (*(*this).rec).calls.lock().unwrap().push(s) }
}
unsafe fn cstr(p: *const c_char) -> String {
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}
fn handle(this: This) -> u64 {
    let mut h = unsafe { (*(*this).rec).next_handle.lock().unwrap() };
    *h += 1;
    0x1000 + *h
}

unsafe extern "C" fn trap(this: This) {
    push(this, "UNEXPECTED call".into());
}

#[repr(C)]
struct ContextVtbl {
    get_generic_interface: unsafe extern "C" fn(This, *const c_char, *mut i32) -> *mut c_void,
    get_driver_handle: unsafe extern "C" fn(This) -> u64,
}
unsafe extern "C" fn ctx_get(this: This, name: *const c_char, err: *mut i32) -> *mut c_void {
    let name = unsafe { cstr(name) };
    push(this, format!("GetGenericInterface {name} err_ptr={}", !err.is_null()));
    let rec = unsafe { &*(*this).rec };
    if rec.missing == Some(name.as_str()) {
        if !err.is_null() {
            unsafe { *err = 105 };
        }
        return std::ptr::null_mut();
    }
    let dummy = rec.objs.iter().find(|o| o.0 == "dummy").unwrap().1;
    rec.objs.iter().find(|o| o.0 == name).map_or(dummy, |o| o.1) as *mut c_void
}
unsafe extern "C" fn ctx_handle(this: This) -> u64 {
    push(this, "GetDriverHandle".into());
    1
}
static CONTEXT_VTBL: ContextVtbl = ContextVtbl { get_generic_interface: ctx_get, get_driver_handle: ctx_handle };

#[repr(C)]
struct HostVtbl {
    tracked_device_added: unsafe extern "C" fn(This, *const c_char, i32, *mut c_void) -> bool,
    tracked_device_pose_updated: unsafe extern "C" fn(This, u32, *const DriverPose, u32),
    rest: [unsafe extern "C" fn(This); 10],
}
unsafe extern "C" fn host_added(this: This, serial: *const c_char, class: i32, driver: *mut c_void) -> bool {
    push(this, format!("TrackedDeviceAdded {} class={class}", unsafe { cstr(serial) }));
    unsafe { *(*(*this).rec).device.lock().unwrap() = driver as usize };
    true
}
unsafe extern "C" fn host_pose(this: This, id: u32, pose: *const DriverPose, size: u32) {
    push(this, format!("TrackedDevicePoseUpdated {id} size={size} {:?}", unsafe { *pose }));
}
static HOST_VTBL: HostVtbl =
    HostVtbl { tracked_device_added: host_added, tracked_device_pose_updated: host_pose, rest: [trap; 10] };

#[repr(C)]
struct PropertiesVtbl {
    read_property_batch: unsafe extern "C" fn(This),
    write_property_batch: unsafe extern "C" fn(This, u64, *mut PropertyWrite, u32) -> i32,
    get_prop_error_name_from_enum: unsafe extern "C" fn(This),
    tracked_device_to_property_container: unsafe extern "C" fn(This, u32) -> u64,
}
unsafe extern "C" fn props_write(this: This, c: u64, batch: *mut PropertyWrite, n: u32) -> i32 {
    for w in unsafe { std::slice::from_raw_parts_mut(batch, n as usize) } {
        let b = unsafe { std::slice::from_raw_parts(w.buffer as *const u8, w.buffer_size as usize) };
        let v = match w.tag {
            2 => format!("{}", i32::from_ne_bytes(b.try_into().unwrap())),
            4 => format!("{}", b[0] != 0),
            5 => format!("{:?}", String::from_utf8_lossy(b)),
            _ => format!("{b:?}"),
        };
        push(this, format!("WriteProperty c={c:#x} prop={} type={} tag={} size={} {v}", w.prop, w.write_type, w.tag, w.buffer_size));
        w.error = 0;
    }
    0
}
unsafe extern "C" fn props_container(this: This, id: u32) -> u64 {
    push(this, format!("TrackedDeviceToPropertyContainer {id}"));
    0x100 + id as u64
}
static PROPS_VTBL: PropertiesVtbl = PropertiesVtbl {
    read_property_batch: trap,
    write_property_batch: props_write,
    get_prop_error_name_from_enum: trap,
    tracked_device_to_property_container: props_container,
};

#[repr(C)]
struct InputVtbl {
    create_boolean_component: unsafe extern "C" fn(This, u64, *const c_char, *mut u64) -> i32,
    update_boolean_component: unsafe extern "C" fn(This, u64, bool, f64) -> i32,
    create_scalar_component: unsafe extern "C" fn(This, u64, *const c_char, *mut u64, i32, i32) -> i32,
    update_scalar_component: unsafe extern "C" fn(This, u64, f32, f64) -> i32,
    rest: [unsafe extern "C" fn(This); 3],
}
unsafe extern "C" fn input_create_bool(this: This, c: u64, name: *const c_char, h: *mut u64) -> i32 {
    let v = handle(this);
    push(this, format!("CreateBooleanComponent c={c:#x} {} -> {v:#x}", unsafe { cstr(name) }));
    unsafe { *h = v };
    0
}
unsafe extern "C" fn input_bool(this: This, h: u64, v: bool, t: f64) -> i32 {
    push(this, format!("UpdateBooleanComponent {h:#x} {v} {t}"));
    0
}
unsafe extern "C" fn input_create_scalar(this: This, c: u64, name: *const c_char, h: *mut u64, ty: i32, units: i32) -> i32 {
    let v = handle(this);
    push(this, format!("CreateScalarComponent c={c:#x} {} type={ty} units={units} -> {v:#x}", unsafe { cstr(name) }));
    unsafe { *h = v };
    0
}
unsafe extern "C" fn input_scalar(this: This, h: u64, v: f32, t: f64) -> i32 {
    push(this, format!("UpdateScalarComponent {h:#x} {v} {t}"));
    0
}
static INPUT_VTBL: InputVtbl = InputVtbl {
    create_boolean_component: input_create_bool,
    update_boolean_component: input_bool,
    create_scalar_component: input_create_scalar,
    update_scalar_component: input_scalar,
    rest: [trap; 3],
};

unsafe extern "C" fn log(this: This, msg: *const c_char) {
    push(this, format!("Log {}", unsafe { cstr(msg) }));
}
static LOG_VTBL: [unsafe extern "C" fn(This, *const c_char); 1] = [log];
static DUMMY_VTBL: [unsafe extern "C" fn(This); 16] = [trap; 16];

/// A fake IVRDriverContext and its interfaces, all recording into one leaked Rec.
fn fake_context(missing: Option<&'static str>) -> (&'static Rec, This) {
    let rec: &'static mut Rec = Box::leak(Box::new(Rec { missing, ..Default::default() }));
    let obj = |vtbl: *const c_void| Box::leak(Box::new(Obj { vtbl, rec })) as *mut Obj as usize;
    let objs = vec![
        ("IVRServerDriverHost_006", obj((&HOST_VTBL as *const HostVtbl).cast())),
        ("IVRProperties_001", obj((&PROPS_VTBL as *const PropertiesVtbl).cast())),
        ("IVRDriverInput_003", obj((&INPUT_VTBL as *const InputVtbl).cast())),
        ("IVRDriverLog_001", obj(LOG_VTBL.as_ptr().cast())),
        ("dummy", obj(DUMMY_VTBL.as_ptr().cast())),
    ];
    let ctx = obj((&CONTEXT_VTBL as *const ContextVtbl).cast()) as This;
    rec.objs = objs;
    (rec, ctx)
}

#[repr(C)]
struct ProviderVtbl {
    init: unsafe extern "C" fn(*mut c_void, This) -> i32,
    cleanup: unsafe extern "C" fn(*mut c_void),
    get_interface_versions: unsafe extern "C" fn(*mut c_void) -> *const *const c_char,
    run_frame: unsafe extern "C" fn(*mut c_void),
    should_block_standby_mode: unsafe extern "C" fn(*mut c_void) -> bool,
    enter_standby: unsafe extern "C" fn(*mut c_void),
    leave_standby: unsafe extern "C" fn(*mut c_void),
}
#[repr(C)]
struct DeviceVtbl {
    activate: unsafe extern "C" fn(*mut c_void, u32) -> i32,
    deactivate: unsafe extern "C" fn(*mut c_void),
    enter_standby: unsafe extern "C" fn(*mut c_void),
    get_component: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    debug_request: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_char, u32),
    get_pose: unsafe extern "C" fn(*mut c_void) -> DriverPose,
}
unsafe fn vt<V>(p: *mut c_void) -> &'static V {
    unsafe { &**(p as *const *const V) }
}

/// Runs the whole scenario against one driver .so and returns everything it saw, in order.
fn run(so: &str, sock: &str) -> Vec<String> {
    assert_ne!(sock, "cc_pointer", "never the live driver's socket");
    let mut out = Vec::new();
    let note = |out: &mut Vec<String>, rec: &Rec, s: String| {
        out.append(&mut rec.calls.lock().unwrap());
        out.push(format!("-- {s}"));
    };
    let path = CString::new(so).unwrap();
    let lib = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    assert!(!lib.is_null(), "dlopen {so}: {}", unsafe { cstr(libc::dlerror()) });
    let sym = unsafe { libc::dlsym(lib, c"HmdDriverFactory".as_ptr()) };
    assert!(!sym.is_null(), "{so} exports no HmdDriverFactory");
    let factory: unsafe extern "C" fn(*const c_char, *mut i32) -> *mut c_void = unsafe { std::mem::transmute(sym) };

    let mut rc = -1;
    let none = unsafe { factory(c"IServerTrackedDeviceProvider_003".as_ptr(), &mut rc) };
    out.push(format!("factory(_003) null={} rc={rc}", none.is_null()));
    let p = unsafe { factory(c"IServerTrackedDeviceProvider_004".as_ptr(), std::ptr::null_mut()) };
    assert!(!p.is_null());
    let pv = unsafe { vt::<ProviderVtbl>(p) };
    let mut versions = Vec::new();
    let mut v = unsafe { (pv.get_interface_versions)(p) };
    while !unsafe { *v }.is_null() {
        versions.push(unsafe { cstr(*v) });
        v = unsafe { v.add(1) };
    }
    out.push(format!("GetInterfaceVersions {versions:?}"));

    let (rec, ctx) = fake_context(None);
    let t0 = Instant::now();
    let rc = unsafe { (pv.init)(p, ctx) };
    note(&mut out, rec, format!("Init -> {rc}"));
    let dev = *rec.device.lock().unwrap() as *mut c_void;
    assert!(!dev.is_null());
    let dv = unsafe { vt::<DeviceVtbl>(dev) };
    let rc = unsafe { (dv.activate)(dev, 7) };
    note(&mut out, rec, format!("Activate(7) -> {rc}"));
    let frame = |out: &mut Vec<String>, what: &str| {
        unsafe { (pv.run_frame)(p) };
        note(out, rec, format!("RunFrame ({what})"));
    };
    frame(&mut out, "nothing leased");
    let pose = unsafe { (dv.get_pose)(dev) };
    out.push(format!("GetPose {pose:?}"));
    let comp = unsafe { (dv.get_component)(dev, c"IVRDisplayComponent_003".as_ptr()) };
    let mut resp = [b'x' as c_char; 8];
    unsafe { (dv.debug_request)(dev, c"hello".as_ptr(), resp.as_mut_ptr(), resp.len() as u32) };
    unsafe { (dv.debug_request)(dev, c"hello".as_ptr(), std::ptr::null_mut(), 0) };
    let block = unsafe { (pv.should_block_standby_mode)(p) };
    unsafe { (pv.enter_standby)(p) };
    unsafe { (pv.leave_standby)(p) };
    unsafe { (dv.enter_standby)(dev) };
    note(&mut out, rec, format!("GetComponent null={} DebugRequest resp0={} ShouldBlockStandbyMode={block}", comp.is_null(), resp[0]));

    let tx = UnixDatagram::unbound().unwrap();
    let to = SocketAddr::from_abstract_name(sock.as_bytes()).unwrap();
    let send = |msg: &str| {
        let until = Instant::now() + Duration::from_secs(2); // the listener thread binds a moment after Init
        while let Err(e) = tx.send_to_addr(msg.as_bytes(), &to) {
            assert!(Instant::now() < until, "driver socket: {e}");
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    send("L 1 R 0.1 1.5 -0.2 1 0 0 0 1 0 0");
    frame(&mut out, "lease inside the 20 s startup guard");

    std::thread::sleep(Duration::from_millis(20200).saturating_sub(t0.elapsed()));
    let lease = "L 2 R 0.10000 1.50000 -0.20000 0.900000 0.100000 0.200000 0.300000 9 0.500 -0.250";
    send(lease);
    frame(&mut out, "lease after the guard: hint");
    send(lease);
    frame(&mut out, "connect");
    let pose = unsafe { (dv.get_pose)(dev) };
    out.push(format!("GetPose {pose:?}"));
    send("L 3 R 0.1 1.5 -0.2 0.9 0.1 0.2 0.3 1 7 0\n");
    send("L 4 R 0.1 1.5 -0.2 0.9 0.1 0.2 0.3 0 -0.75 0");
    frame(&mut out, "a tap between frames (latched)");
    send("L 5 R 0.1 1.5 -0.2 0.9 0.1 0.2 0.3 0 0 0");
    send("Hide");
    send("L 6 X 0 0 0 1 0 0 0 0 0 0");
    send("L 7 R 0 0 0 1 0 0 0 0 0 0 junk");
    frame(&mut out, "malformed ignored, tap gone");
    send("H");
    frame(&mut out, "hidden");
    unsafe { (dv.deactivate)(dev) };
    frame(&mut out, "deactivated: no calls");
    unsafe { (pv.cleanup)(p) };
    note(&mut out, rec, "Cleanup".into());
    out.push(format!("socket released: {}", UnixDatagram::bind_addr(&to).is_ok()));

    let (rec, ctx) = fake_context(Some("IVRResources_001"));
    let rc = unsafe { (pv.init)(p, ctx) };
    note(&mut out, rec, format!("Init without IVRResources -> {rc}"));
    out
}

fn so_dir() -> std::path::PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf() // target/<profile>
}

#[test]
#[ignore = "takes 21 s (the driver's startup guard); run with --ignored"]
fn driver_against_fake_runtime() {
    let rust_so = so_dir().join("libdriver_cc_pointer.so");
    assert!(rust_so.exists(), "build it first: cargo build --release -p cc-pointer");
    unsafe { std::env::set_var("CC_POINTER_SOCKET", "ccp_harness_rs") };
    let cxx = std::env::var("CC_POINTER_CXX_SO").ok();
    let cxx = cxx.map(|so| std::thread::spawn(move || run(&so, "ccp_harness_cxx")));
    let rs = run(rust_so.to_str().unwrap(), "ccp_harness_rs");
    let dump = |name: &str, v: &[String]| {
        let f = so_dir().join(format!("cc_pointer_harness_{name}.txt"));
        std::fs::write(&f, v.join("\n") + "\n").unwrap();
        eprintln!("{name}: {} lines in {}", v.len(), f.display());
    };
    dump("rust", &rs);

    let has = |s: &str| assert!(rs.iter().any(|l| l.contains(s)), "missing {s:?}");
    has("factory(_003) null=true rc=105");
    has("TrackedDeviceAdded cc_pointer_0 class=2");
    has("prop=1031 type=0 tag=5 size=6 \"ccp/1\\0\"");
    has("prop=1003 type=0 tag=5 size=47 \"{cc_pointer}/rendermodels/cc_pointer_invisible\\0\"");
    has("prop=3007 type=0 tag=2 size=4 3");
    has("CreateBooleanComponent c=0x107 /input/joystick/click -> 0x1006");
    has("CreateScalarComponent c=0x107 /input/joystick/y type=0 units=1 -> 0x1008");
    has("Log cc_pointer: connected");
    has("prop=3007 type=0 tag=2 size=4 2");
    has("vec_position: [0.10000000149011612, 1.5, -0.20000000298023224]");
    has("UpdateScalarComponent 0x1007 -0.75 0");
    has("Log cc_pointer: released");
    has("socket released: true");
    has("Init without IVRResources -> 105");
    assert!(!rs.iter().any(|l| l.contains("UNEXPECTED")));

    if let Some(cxx) = cxx {
        let cx = cxx.join().unwrap();
        dump("cxx", &cx);
        for (i, (a, b)) in rs.iter().zip(&cx).enumerate() {
            assert_eq!(a, b, "line {} differs (rust vs C++)", i + 1);
        }
        assert_eq!(rs.len(), cx.len());
        eprintln!("Rust and C++ call sequences identical ({} lines)", rs.len());
    }
}
