//! One RDP session per panel: krdp on the machine, FreeRDP here, H.264 decoded with FFmpeg.
//! Each session keeps reconnecting for as long as cc-panels runs. Its clipboard channel goes to
//! clipboard/cliprdr.rs.
use crate::{Panel, QUIT, panel};
use freerdp_sys::*;
use std::ffi::{CStr, CString, c_void};
use std::sync::atomic::Ordering::*;
use std::time::Duration;

/// FreeRDP's client context with the panel it belongs to.
#[repr(C)]
struct PanelContext {
    common: rdpClientContext,
    panel: usize,
}

unsafe fn panel_of(c: *mut rdpContext) -> &'static Panel {
    panel(unsafe { (*(c as *mut PanelContext)).panel })
}

unsafe extern "C" fn on_begin_paint(c: *mut rdpContext) -> BOOL {
    let hwnd = unsafe { &mut *(*(*(*(*c).gdi).primary).hdc).hwnd };
    unsafe { (*hwnd.invalid).null = 1 };
    hwnd.ninvalid = 0; // clear its rects like gdi_begin_paint does, or they'd pile up
    1
}

unsafe extern "C" fn on_end_paint(c: *mut rdpContext) -> BOOL {
    let p = unsafe { panel_of(c) };
    let r = unsafe { &*(*(*(*(*(*c).gdi).primary).hdc).hwnd).invalid };
    if r.null != 0 {
        return 1;
    }
    for st in &p.stale {
        st.grow(r.x, r.y, r.x + r.w, r.y + r.h); // neither buffer has this region yet
    }
    // D-045: how much of the picture this update changed, for attention.rs. It sums the rects
    // instead of their bounds, since a caret and a clock in opposite corners are only a little.
    let (hwnd, gdi) = unsafe { (&*(*(*(*(*c).gdi).primary).hdc).hwnd, &*(*c).gdi) };
    let n = if hwnd.cinvalid.is_null() { 0 } else { hwnd.ninvalid.max(0) as usize };
    let rects = if n == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(hwnd.cinvalid, n) } };
    let area: i64 = rects.iter().map(|r| r.w.max(0) as i64 * r.h.max(0) as i64).sum();
    let all = (gdi.width.max(1) as i64 * gdi.height.max(1) as i64).max(1);
    p.change.fetch_max((area * 1000 / all).min(1000) as u32, Relaxed);
    p.dirty.store(true, Release); // goes to the GPU once the decode is done (run, gpu::write)
    if !IN_SESSION.get() {
        // Shouldn't happen with SynchronousDynamicChannels, but if it does, the session thread may be asleep, so wake it
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, Relaxed) {
            eprintln!("{}: painted outside check_event_handles (once)", p.v.name);
        }
        poke(p);
    }
    1
}

thread_local! {
    /// True while this thread is the session thread, inside freerdp_check_event_handles, holding p.lock.
    static IN_SESSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

unsafe extern "C" fn on_desktop_resize(c: *mut rdpContext) -> BOOL {
    // This normally comes inside check_event_handles, where we already hold the lock (drdynvc is
    // synchronous, see run). If it ever comes from another thread (drdynvc's own, when it's
    // asynchronous), we take the lock here.
    let p = unsafe { panel_of(c) };
    let _g = (!IN_SESSION.get()).then(|| p.lock.lock().unwrap());
    let (w, h) = unsafe { desktop(c) };
    if p.v.pop.is_some() {
        crate::popout::resized(p.index, w, h, false); // a popup or the window resized its stream
    }
    unsafe { gdi_resize((*c).gdi, w, h) }
}

/// The session's desktop size, as the server set it.
unsafe fn desktop(c: *mut rdpContext) -> (u32, u32) {
    unsafe {
        let s = (*c).settings;
        (freerdp_settings_get_uint32(s, FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopWidth), freerdp_settings_get_uint32(s, FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopHeight))
    }
}

// ------------------------------------------------------------------ the clipboard channel (clipboard/cliprdr.rs)

fn is_cliprdr(name: *const std::os::raw::c_char) -> bool {
    unsafe { CStr::from_ptr(name) }.to_bytes_with_nul() == CLIPRDR_SVC_CHANNEL_NAME
}

unsafe extern "C" fn on_channel_connected(context: *mut c_void, e: *const ChannelConnectedEventArgs) {
    let e = unsafe { &*e };
    if !is_cliprdr(e.name) {
        return unsafe { freerdp_client_OnChannelConnectedEventHandler(context, e) }; // graphics pipeline to the GDI
    }
    crate::clipboard::cliprdr::attach(unsafe { panel_of(context as *mut rdpContext) }, e.pInterface as *mut CliprdrClientContext);
}

unsafe extern "C" fn on_channel_disconnected(context: *mut c_void, e: *const ChannelDisconnectedEventArgs) {
    let e = unsafe { &*e };
    if !is_cliprdr(e.name) {
        return unsafe { freerdp_client_OnChannelDisconnectedEventHandler(context, e) };
    }
    crate::clipboard::cliprdr::detach(unsafe { panel_of(context as *mut rdpContext) }, e.pInterface as *mut CliprdrClientContext);
}

type Connected = unsafe extern "C" fn(*mut c_void, *const ChannelConnectedEventArgs);
type Disconnected = unsafe extern "C" fn(*mut c_void, *const ChannelDisconnectedEventArgs);

// ------------------------------------------------------------------ the session

unsafe extern "C" fn on_pre_connect(instance: *mut freerdp) -> BOOL {
    unsafe {
        let c = (*instance).context;
        PubSub_Subscribe((*c).pubSub, c"ChannelConnected".as_ptr(), on_channel_connected as Connected);
        PubSub_Subscribe((*c).pubSub, c"ChannelDisconnected".as_ptr(), on_channel_disconnected as Disconnected);
        freerdp_settings_set_uint32((*c).settings, FreeRDP_Settings_Keys_UInt32_FreeRDP_OsMajorType, OSMAJORTYPE_UNIX)
    }
}

unsafe extern "C" fn on_post_connect(instance: *mut freerdp) -> BOOL {
    unsafe {
        if gdi_init(instance, PIXEL_FORMAT_BGRA32) == 0 {
            return 0; // the server's own byte order, so no per-pixel conversion
        }
        let u = (*(*instance).context).update;
        (*u).BeginPaint = Some(on_begin_paint);
        (*u).EndPaint = Some(on_end_paint);
        (*u).DesktopResize = Some(on_desktop_resize);
    }
    1
}

unsafe extern "C" fn on_post_disconnect(instance: *mut freerdp) {
    unsafe {
        let c = (*instance).context;
        PubSub_Unsubscribe((*c).pubSub, c"ChannelConnected".as_ptr(), on_channel_connected as Connected);
        PubSub_Unsubscribe((*c).pubSub, c"ChannelDisconnected".as_ptr(), on_channel_disconnected as Disconnected);
        gdi_free(instance);
    }
}

unsafe extern "C" fn on_client_new(instance: *mut freerdp, _: *mut rdpContext) -> BOOL {
    unsafe {
        (*instance).PreConnect = Some(on_pre_connect);
        (*instance).PostConnect = Some(on_post_connect);
        (*instance).PostDisconnect = Some(on_post_disconnect);
    }
    1
}

/// A paired machine's certificate that doesn't match its pin. We refuse it (0) and never ask.
unsafe extern "C" fn on_other_cert(instance: *mut freerdp, _: *const BYTE, _: usize, _: *const std::os::raw::c_char, _: UINT16, _: DWORD) -> std::os::raw::c_int {
    eprintln!("{}: the host's certificate changed; pair again", unsafe { panel_of((*instance).context) }.v.name);
    0
}

/// Sends one command to a machine's agent (cc_proto::agent, docs/agent.md) as this Frame, on a
/// fresh connection. Returns its answer, or why not, using cc-home's states: not-paired,
/// host-changed, no-answer, no-agent (unreachable or refusing), or the agent's own error.
pub fn agent(machine: &str, cmd: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    use cc_proto::agent::Error;
    let conf = std::path::PathBuf::from(crate::config::config(""));
    let r = cc_proto::agent::call_once(&conf, machine, cmd, args.as_object().cloned().unwrap_or_default()).map_err(|e| match e {
        Error::HostChanged => "host-changed",
        Error::NotPaired(_) => "not-paired",
        Error::NoAnswer => "no-answer",
        _ => "no-agent",
    })?;
    if r["ok"] != true {
        return Err(r["error"].as_str().unwrap_or("refused").to_string());
    }
    Ok(r)
}

/// Asks the host's agent to start (or stop) this Frame's server for the panel (agent.md §5a),
/// or a popped-out window's own server (popout.rs). Returns the port once it's listening; we
/// wait for that here on the RDP thread, up to 20 s. A slot-0 or unpaired remote answers right
/// away with its own port.
/// Err says why not, as a state: "unknown-command" from an older agent, "no-agent" when it's
/// unreachable, "host-changed". A monitor then falls back to its own port, like before we had
/// sessions (run).
fn session(p: &Panel, start: bool) -> Result<u32, String> {
    let v: &crate::config::Viewer = &p.v;
    let verb = if start { "start" } else { "stop" };
    let machine = crate::config::machine_of(v);
    let (cmd, args) = match &v.pop {
        // its source monitor is on the shared login, so it's not paired (cc-home's window)
        Some((from, _)) if crate::panels().iter().any(|q| q.v.name == *from && q.v.port < 3410) => return Err("not-paired".into()),
        Some((_, uuid)) => ("window", serde_json::json!({"op": verb, "uuid": uuid})),
        None if v.port < 3410 || cc_proto::agent::trusted(std::path::Path::new(&crate::config::config("")), machine).is_err() => return Ok(v.port),
        None => ("session", serde_json::json!({"op": verb, "index": (v.port - 3400) % 10})),
    };
    let r = agent(machine, cmd, args).map_err(|e| if e == "unknown-command" && v.pop.is_some() { "unsupported".into() } else { e });
    match r {
        Ok(r) if start => {
            let n = r["port"].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("no port")?;
            if let Some(ms) = r["start_ms"].as_u64().filter(|&ms| ms != 0) {
                eprintln!("{}: session up on {n} in {ms} ms", v.name);
            }
            Ok(n)
        }
        Ok(_) => Ok(0),
        Err(e) => {
            if !start {
                eprintln!("{}: session stop: {e}", v.name);
            }
            Err(e)
        }
    }
}

/// Connects, runs the session until it ends, and reconnects, until cc-panels ends or the panel
/// gets disconnected (main.rs disconnect, which makes it not live).
pub fn run(p: &'static Panel) {
    // Decoding stays gentle because the headset is rendering VR too, but the main loop shouldn't
    // be (cc-panels runs at nice 0, CC_NICE in the wrapper). FreeRDP's threads start from this
    // one, so they inherit it.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10) };
    let cstr = |s: &str| CString::new(s).unwrap_or_default();
    let (host, user) = (cstr(&p.v.host), cstr(&p.v.user));
    // poke's event, which wakes the session's wait. ponytail: never closed, one per panel for
    // cc-panels' life (a reconnect reuses it)
    let mut wake = p.wake.load(Acquire);
    if wake.is_null() {
        wake = unsafe { CreateEventA(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        p.wake.store(wake, Release);
    }
    let pop = p.v.pop.is_some();
    let mut ever = false; // connected at least once (a pop-out that never did failed, rather than closed)
    if pop {
        crate::popout::prepare(p); // its name and tags, from the host's window list
    }
    let pause = || {
        for _ in 0..5 {
            if QUIT.load(Relaxed) || !p.live() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    };
    let (mut said, mut changed) = (false, false); // log each failure once, not on every retry
    while !QUIT.load(Relaxed) && p.live() {
        let port = match session(p, true) {
            Ok(n) => {
                (said, changed) = (false, false);
                n
            }
            // another key at its address: we never connect unpinned, so not until it's paired again
            Err(why) if why == "host-changed" => {
                if !std::mem::replace(&mut changed, true) {
                    eprintln!("{}: host changed: pair again", p.v.name);
                }
                if pop {
                    return crate::popout::failed(p, &why);
                }
                pause();
                continue;
            }
            Err(why) if pop => {
                if why.contains("answer") {
                    let _ = session(p, false); // no-answer, but the agent may have started it anyway
                }
                return crate::popout::failed(p, &why);
            }
            Err(why) => {
                if !std::mem::replace(&mut said, true) {
                    eprintln!("{}: session start: {why}; its own port {}", p.v.name, p.v.port);
                }
                p.v.port
            }
        };
        let mut ep: RDP_CLIENT_ENTRY_POINTS = unsafe { std::mem::zeroed() };
        ep.Version = RDP_CLIENT_INTERFACE_VERSION;
        ep.Size = size_of::<RDP_CLIENT_ENTRY_POINTS_V1>() as u32;
        ep.ContextSize = size_of::<PanelContext>() as u32;
        ep.ClientNew = Some(on_client_new);
        // re-read on each try, since pairing again changes both
        let (password, pin) = (cstr(&crate::config::password(&p.v.machine)), crate::config::pin(&p.v.machine));
        unsafe {
            let c = freerdp_client_context_new(&ep);
            (*(c as *mut PanelContext)).panel = p.index;
            let s = (*c).settings;
            let b = |k, v: bool| freerdp_settings_set_bool(s, k, v as BOOL);
            let n = |k, v: u32| freerdp_settings_set_uint32(s, k, v);
            freerdp_settings_set_string(s, FreeRDP_Settings_Keys_String_FreeRDP_ServerHostname, host.as_ptr());
            freerdp_settings_set_string(s, FreeRDP_Settings_Keys_String_FreeRDP_Username, user.as_ptr());
            freerdp_settings_set_string(s, FreeRDP_Settings_Keys_String_FreeRDP_Password, password.as_ptr());
            n(FreeRDP_Settings_Keys_UInt32_FreeRDP_ServerPort, port);
            n(FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopWidth, p.v.w);
            n(FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopHeight, p.v.h);
            n(FreeRDP_Settings_Keys_UInt32_FreeRDP_ColorDepth, 32);
            // Both, and the server picks: our krdp 6.7.5 takes TLS and checks the password itself,
            // while SteamOS's krdp 6.4 (a Steam Deck) only takes NLA ("server supports only NLA").
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_NlaSecurity, true);
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_TlsSecurity, true);
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_RdpSecurity, false);
            if let Some(pin) = &pin {
                // paired (H2): only that certificate. FreeRDP accepts it by fingerprint before
                // anything else, and any other one goes to on_other_cert, which refuses it
                let fp = cstr(&format!("sha256:{pin}"));
                freerdp_settings_set_string(s, FreeRDP_Settings_Keys_String_FreeRDP_CertificateAcceptedFingerprints, fp.as_ptr());
                b(FreeRDP_Settings_Keys_Bool_FreeRDP_ExternalCertificateManagement, true);
                (*(*c).instance).VerifyX509Certificate = Some(on_other_cert);
            } else {
                b(FreeRDP_Settings_Keys_Bool_FreeRDP_AutoAcceptCertificate, true); // trust on first use, then insist
            }
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_SupportGraphicsPipeline, true);
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_GfxH264, true);
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_GfxAVC444, false);
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_RemoteFxCodec, false);
            // Decode the graphics pipeline (H.264) inside check_event_handles, on this thread and
            // under p.lock, instead of on drdynvc's own thread, so gpu::write right after it sees the finished frame
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_SynchronousDynamicChannels, true);
            n(FreeRDP_Settings_Keys_UInt32_FreeRDP_ConnectionType, CONNECTION_TYPE_LAN);
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_NetworkAutoDetect, true); // krdp measures round trips
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_RedirectClipboard, true); // one clipboard for every machine
            // Static channels (the clipboard) on this thread. H.264's colour conversion ignores
            // this flag, so install.sh patches FreeRDP's h264.c to run it here too instead of on
            // WinPR's 8-thread pool (3.5-4 ms less CPU a 3840x1080 frame, ~5 ms more latency; efficiency-plan.md 2a)
            n(FreeRDP_Settings_Keys_UInt32_FreeRDP_ThreadingFlags, THREADING_FLAGS_DISABLE_THREADS);
            p.rdp.store(c, Release);
            if freerdp_client_start(c) == 0 && freerdp_connect((*c).instance) != 0 {
                p.connected.store(true, Release);
                ever = true;
                if pop {
                    let (w, h) = desktop(c);
                    crate::popout::resized(p.index, w, h, true); // sized from its stream
                }
                eprintln!(
                    "{}: connected ({}x{})",
                    p.v.name,
                    freerdp_settings_get_uint32(s, FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopWidth),
                    freerdp_settings_get_uint32(s, FreeRDP_Settings_Keys_UInt32_FreeRDP_DesktopHeight)
                );
                let mut handles = [std::ptr::null_mut(); MAXIMUM_WAIT_OBJECTS as usize];
                while !QUIT.load(Relaxed) && p.live() && freerdp_shall_disconnect_context(c) == 0 {
                    // 1 s: a timeout only checks QUIT (input goes out from its own thread), so an
                    // idle session wakes once a second instead of ten times (efficiency-plan.md 5).
                    // Sooner while a decoded picture is waiting to be due (gpu::wait).
                    let mut count = freerdp_get_event_handles(c, handles.as_mut_ptr(), handles.len() as u32 - 1);
                    if count > 0 && !wake.is_null() {
                        handles[count as usize] = wake;
                        count += 1;
                    }
                    let ms = crate::gpu::wait(p).as_millis() as u32;
                    if count == 0 || WaitForMultipleObjects(count, handles.as_ptr(), 0, ms) == WAIT_FAILED {
                        break;
                    }
                    if !wake.is_null() {
                        ResetEvent(wake); // any poke from here on wakes the next wait
                    }
                    let _g = p.lock.lock().unwrap();
                    IN_SESSION.set(true);
                    let ok = freerdp_check_event_handles(c);
                    IN_SESSION.set(false);
                    if ok == 0 {
                        break;
                    }
                    // Still holding the lock, write the decoded picture into the spare GPU buffer
                    // here, off the main loop. Live, doing it there cost 20-50 ms a desk-portrait
                    // frame and 10 ms desk-wide.
                    if !(*c).gdi.is_null() {
                        crate::gpu::write(p, &*(*c).gdi);
                    }
                }
                p.connected.store(false, Release);
                detach(p);
                freerdp_disconnect((*c).instance); // frees the GDI

            } else {
                eprintln!("{}: can't connect (0x{:08x})", p.v.name, freerdp_get_last_error(c));
            }
            detach(p);
            freerdp_client_stop(c);
            freerdp_client_context_free(c);
        }
        if pop {
            break; // a window's session ending means the window (or its server) closed, so no reconnect
        }
        pause();
    }
    // ask the host to stop this Frame's server for it (otherwise its idle stop does). A window's
    // gets stopped even on quitting, since it's its own krdpserver (400-800 MB, counted in
    // sessions_max). 20 s at most.
    if !QUIT.load(Relaxed) || pop {
        let _ = session(p, false);
        eprintln!("{}: disconnected", p.v.name);
    }
    if pop {
        if !ever && p.live() && !QUIT.load(Relaxed) { crate::popout::failed(p, "can't connect") } else { crate::popout::ended(p) }
    }
}

/// Wakes the panel's session thread from its wait (gpu::wait) because there's something to write now.
pub fn poke(p: &Panel) {
    let e = p.wake.load(Acquire);
    if !e.is_null() {
        unsafe { SetEvent(e) };
    }
}

/// Input and suppress reach the session through p.rdp under p.lock, so drop it before it's freed.
fn detach(p: &Panel) {
    let _g = p.lock.lock().unwrap();
    p.rdp.store(std::ptr::null_mut(), Release);
}

/// The session's input, while it's connected.
pub fn input(p: &Panel) -> Option<*mut rdpInput> {
    let c = p.rdp.load(Acquire);
    (p.connected.load(Acquire) && !c.is_null()).then(|| unsafe { (*c).input })
}

pub fn mouse(p: &Panel, flags: u32, x: f64, y: f64) {
    if let Some(i) = input(p) {
        unsafe { freerdp_input_send_mouse_event(i, flags as u16, x as u16, y as u16) };
    }
}

pub fn key(p: &Panel, down: bool, repeat: bool, scancode: u32) {
    if let Some(i) = input(p) {
        unsafe { freerdp_input_send_keyboard_event_ex(i, down as BOOL, repeat as BOOL, scancode) };
    }
}

/// A wheel turn, as a 9-bit two's complement rotation in the flags.
pub fn wheel(p: &Panel, horizontal: bool, rotation: i32) {
    let rot = rotation.clamp(-255, 255) as u16 & 0x1FF;
    let f = if horizontal { PTR_FLAGS_HWHEEL } else { PTR_FLAGS_WHEEL };
    mouse(p, f | rot as u32, 0.0, 0.0);
}
