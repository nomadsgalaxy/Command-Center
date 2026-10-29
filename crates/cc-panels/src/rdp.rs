//! One RDP session per panel: krdp on the machine, FreeRDP here, H.264 decoded with FFmpeg.
//! Each session keeps reconnecting for as long as cc-panels runs. Its clipboard channel goes to
//! clipboard/cliprdr.rs.
//!
//! Sound and microphone (docs/audio.md): FreeRDP's rdpsnd and audin channels with its PulseAudio
//! backends, which talk to the Frame's PipeWire through its PulseAudio socket. Only one session per
//! machine opens them (config::carries_audio), or a machine's two monitors would play everything
//! twice and both claim the microphone. The channels are chosen when a session connects, so when
//! the choice moves (the carrying monitor was closed, or came back) the session that gains or
//! loses it reconnects once, right away.
use crate::{Panel, QUIT};
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

/// Runs f on the panel a session's context belongs to. None when there's no context, or its index
/// isn't a panel (both would be a FreeRDP bug), so a callback fails instead of reading past either.
unsafe fn with_panel<R>(c: *mut rdpContext, f: impl FnOnce(&Panel) -> R) -> Option<R> {
    if c.is_null() {
        return None;
    }
    crate::panels().get(unsafe { (*(c as *mut PanelContext)).panel }).map(f)
}

/// The GDI's window and its invalid region, or None if any link on the way there is null, so a
/// callback fails instead of reading through a null pointer.
unsafe fn gdi_window(c: *mut rdpContext) -> Option<(HGDI_WND, *mut rdpGdi)> {
    unsafe {
        if c.is_null() || (*c).gdi.is_null() || (*(*c).gdi).primary.is_null() || (*(*(*c).gdi).primary).hdc.is_null() {
            return None;
        }
        let hwnd = (*(*(*(*c).gdi).primary).hdc).hwnd;
        (!hwnd.is_null() && !(*hwnd).invalid.is_null()).then_some((hwnd, (*c).gdi))
    }
}

unsafe extern "C" fn on_begin_paint(c: *mut rdpContext) -> BOOL {
    crate::ffi::guard("on_begin_paint", 0, || {
        let Some((hwnd, _)) = (unsafe { gdi_window(c) }) else { return 0 };
        let hwnd = unsafe { &mut *hwnd };
        unsafe { (*hwnd.invalid).null = 1 };
        hwnd.ninvalid = 0; // clear its rects like gdi_begin_paint does, or they'd pile up
        1
    })
}

unsafe extern "C" fn on_end_paint(c: *mut rdpContext) -> BOOL {
    crate::ffi::guard("on_end_paint", 0, || {
        let Some((hwnd, gdi)) = (unsafe { gdi_window(c) }) else { return 0 };
        let f = |p: &Panel| {
            let r = unsafe { &*(*hwnd).invalid };
            if r.null != 0 {
                return 1;
            }
            for st in &p.stale {
                st.grow(r.x, r.y, r.x + r.w, r.y + r.h); // neither buffer has this region yet
            }
            // D-045: how much of the picture this update changed, for attention.rs. It sums the rects
            // instead of their bounds, since a caret and a clock in opposite corners are only a little.
            let (hwnd, gdi) = unsafe { (&*hwnd, &*gdi) };
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
        };
        unsafe { with_panel(c, f) }.unwrap_or(0)
    })
}

thread_local! {
    /// True while this thread is the session thread, inside freerdp_check_event_handles, holding p.lock.
    static IN_SESSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

unsafe extern "C" fn on_desktop_resize(c: *mut rdpContext) -> BOOL {
    crate::ffi::guard("on_desktop_resize", 0, || {
        // This normally comes inside check_event_handles, where we already hold the lock (drdynvc is
        // synchronous, see run). If it ever comes from another thread (drdynvc's own, when it's
        // asynchronous), we take the lock here.
        let f = |p: &Panel| {
            let _g = (!IN_SESSION.get()).then(|| crate::ffi::lock(&p.lock));
            let (w, h) = unsafe { desktop(c) };
            if p.v.pop.is_some() {
                crate::popout::resized(p.index, w, h, false); // a popup or the window resized its stream
            }
            if unsafe { (*c).gdi.is_null() } {
                return 0;
            }
            unsafe { gdi_resize((*c).gdi, w, h) }
        };
        unsafe { with_panel(c, f) }.unwrap_or(0)
    })
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
    crate::ffi::guard("on_channel_connected", (), || {
        let e = unsafe { &*e };
        if !is_cliprdr(e.name) {
            return unsafe { freerdp_client_OnChannelConnectedEventHandler(context, e) }; // graphics pipeline to the GDI
        }
        unsafe { with_panel(context as *mut rdpContext, |p| crate::clipboard::cliprdr::attach(p, e.pInterface as *mut CliprdrClientContext)) };
    })
}

unsafe extern "C" fn on_channel_disconnected(context: *mut c_void, e: *const ChannelDisconnectedEventArgs) {
    crate::ffi::guard("on_channel_disconnected", (), || {
        let e = unsafe { &*e };
        if !is_cliprdr(e.name) {
            return unsafe { freerdp_client_OnChannelDisconnectedEventHandler(context, e) };
        }
        unsafe { with_panel(context as *mut rdpContext, |p| crate::clipboard::cliprdr::detach(p, e.pInterface as *mut CliprdrClientContext)) };
    })
}

type Connected = unsafe extern "C" fn(*mut c_void, *const ChannelConnectedEventArgs);
type Disconnected = unsafe extern "C" fn(*mut c_void, *const ChannelDisconnectedEventArgs);

// ------------------------------------------------------------------ the session

unsafe extern "C" fn on_pre_connect(instance: *mut freerdp) -> BOOL {
    crate::ffi::guard("on_pre_connect", 0, || {
        unsafe {
            let c = (*instance).context;
            PubSub_Subscribe((*c).pubSub, c"ChannelConnected".as_ptr(), on_channel_connected as Connected);
            PubSub_Subscribe((*c).pubSub, c"ChannelDisconnected".as_ptr(), on_channel_disconnected as Disconnected);
            freerdp_settings_set_uint32((*c).settings, FreeRDP_Settings_Keys_UInt32_FreeRDP_OsMajorType, OSMAJORTYPE_UNIX)
        }
    })
}

unsafe extern "C" fn on_post_connect(instance: *mut freerdp) -> BOOL {
    crate::ffi::guard("on_post_connect", 0, || {
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
    })
}

unsafe extern "C" fn on_post_disconnect(instance: *mut freerdp) {
    crate::ffi::guard("on_post_disconnect", (), || {
        unsafe {
            let c = (*instance).context;
            PubSub_Unsubscribe((*c).pubSub, c"ChannelConnected".as_ptr(), on_channel_connected as Connected);
            PubSub_Unsubscribe((*c).pubSub, c"ChannelDisconnected".as_ptr(), on_channel_disconnected as Disconnected);
            gdi_free(instance);
        }
    })
}

unsafe extern "C" fn on_client_new(instance: *mut freerdp, _: *mut rdpContext) -> BOOL {
    crate::ffi::guard("on_client_new", 0, || {
        unsafe {
            (*instance).PreConnect = Some(on_pre_connect);
            (*instance).PostConnect = Some(on_post_connect);
            (*instance).PostDisconnect = Some(on_post_disconnect);
        }
        1
    })
}

/// A paired machine's certificate that doesn't match its pin. We refuse it (0) and never ask.
unsafe extern "C" fn on_other_cert(instance: *mut freerdp, _: *const BYTE, _: usize, _: *const std::os::raw::c_char, _: UINT16, _: DWORD) -> std::os::raw::c_int {
    crate::ffi::guard("on_other_cert", 0, || {
        let name = unsafe { with_panel((*instance).context, |p| p.v.name.clone()) }.unwrap_or_default();
        eprintln!("{name}: the host's certificate changed; pair again");
        0
    })
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
/// Panel::host_health: the host's agent says its server for this monitor is fine (or hasn't said),
/// keeps restarting, or gave up (docs/agent.md, status's health).
pub const HOST_OK: u8 = 0;
pub const HOST_DEGRADED: u8 = 1;
pub const HOST_DOWN: u8 = 2;

/// What status's health says for this monitor's server, as a HOST_* value.
pub fn host_health(status: &serde_json::Value, index: u32) -> u8 {
    let mine = status["health"].as_array().into_iter().flatten().find(|h| h["index"].as_u64() == Some(index as u64));
    match mine.and_then(|h| h["health"].as_str()) {
        Some("down") => HOST_DOWN,
        Some("degraded") => HOST_DEGRADED,
        _ => HOST_OK,
    }
}

/// After a failed start, asks the agent why, at most every 30 s, so the machines window can say the
/// host's server is broken instead of the panel retrying quietly.
fn ask_health(p: &Panel, last: &mut Option<std::time::Instant>) {
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
        return;
    }
    *last = Some(std::time::Instant::now());
    if let Ok(st) = agent(crate::config::machine_of(&p.v), "status", serde_json::json!({})) {
        let h = host_health(&st, (p.v.port - 3400) % 10);
        if p.host_health.swap(h, Relaxed) != h && h != HOST_OK {
            eprintln!("{}: its host says its server is {}", p.v.name, if h == HOST_DOWN { "down (it gave up restarting)" } else { "restarting again and again" });
        }
    }
}

/// How long a panel waits before asking again when its host says its server gave up ("down").
/// Not every few seconds: each start the host lets through would fail again and alert again. Five
/// minutes is also about when someone may have fixed it. Connect asks at once (Panel::retry).
pub const DOWN_RETRY: Duration = Duration::from_secs(300);

/// Whether a panel whose host server is down keeps waiting after `waited`.
pub fn keep_waiting(waited: Duration, live: bool, retry: bool) -> bool {
    live && !retry && waited < DOWN_RETRY
}

fn session(p: &Panel, start: bool) -> Result<u32, String> {
    let v: &crate::config::Viewer = &p.v;
    let verb = if start { "start" } else { "stop" };
    // A window popping out is always someone asking, and so is Connect.
    let force = start && (v.pop.is_some() || p.retry.swap(false, Relaxed));
    let machine = crate::config::machine_of(v);
    let (cmd, args) = match &v.pop {
        // its source monitor is on the shared login, so it's not paired (cc-home's window)
        Some((from, _)) if crate::panels().iter().any(|q| q.v.name == *from && q.v.port < 3410) => return Err("not-paired".into()),
        Some((_, uuid)) => ("window", serde_json::json!({"op": verb, "uuid": uuid, "force": force})),
        None if v.port < 3410 || cc_proto::agent::trusted(std::path::Path::new(&crate::config::config("")), machine).is_err() => return Ok(v.port),
        None => ("session", serde_json::json!({"op": verb, "index": (v.port - 3400) % 10, "force": force})),
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

/// Points libpulse at the Frame's PipeWire (its PulseAudio socket). The Desktop session has its own
/// XDG_RUNTIME_DIR, where the session setup links that socket (session.rs), but a pinned
/// PULSE_SERVER doesn't depend on that. Keeps one the user set. Called once, before any thread.
/// With ~/.cache/control-center/audio-debug there, FreeRDP's rdpsnd logs every chunk it plays,
/// drops ("Buffer overrun") or runs short of ("Buffer underrun") (docs/audio.md).
pub fn audio_env() {
    let home = std::env::var("HOME").unwrap_or_default();
    if std::env::var_os("WLOG_FILTER").is_none() && std::path::Path::new(&home).join(".cache/control-center/audio-debug").exists() {
        unsafe { std::env::set_var("WLOG_FILTER", "com.freerdp.channels.rdpsnd.client:DEBUG") };
    }
    if std::env::var_os("PULSE_SERVER").is_some() {
        return;
    }
    let socket = format!("/run/user/{}/pulse/native", unsafe { libc::getuid() });
    if std::path::Path::new(&socket).exists() {
        unsafe { std::env::set_var("PULSE_SERVER", format!("unix:{socket}")) };
    }
}

/// Whether this panel's session should carry its machine's audio right now.
fn carries_audio(p: &Panel) -> bool {
    let all: Vec<&Panel> = crate::panels().iter().filter(|q| q.used() && matches!(q.src, crate::Source::Rdp)).collect();
    let peers: Vec<(&crate::config::Viewer, bool)> = all.iter().map(|q| (&*q.v, q.live() && !q.down.load(Relaxed))).collect();
    crate::config::carries_audio(&p.v, &peers)
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
    let mut asked = None; // when it last asked the host about its server's health
    let mut moved = false; // the last session ended because audio moved, so no pause before the next
    while !QUIT.load(Relaxed) && p.live() {
        let port = match session(p, true) {
            Ok(n) => {
                (said, changed) = (false, false);
                p.host_health.store(HOST_OK, Relaxed);
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
            // Its host's server gave up. Asking every few seconds would restart it and fail again,
            // so wait (DOWN_RETRY, or until Connect), showing it as down meanwhile.
            Err(why) if why == "down" && !pop => {
                if p.host_health.swap(HOST_DOWN, Relaxed) != HOST_DOWN {
                    eprintln!("{}: its host's server gave up; asking again in {} min, or at Connect", p.v.name, DOWN_RETRY.as_secs() / 60);
                }
                let t0 = std::time::Instant::now();
                while !QUIT.load(Relaxed) && keep_waiting(t0.elapsed(), p.live(), p.retry.load(Relaxed)) {
                    std::thread::sleep(Duration::from_secs(1));
                }
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
                ask_health(p, &mut asked);
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
        let audio = carries_audio(p) && !pop;
        p.audio.store(audio, Relaxed);
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
            // The host's sound plays here (rdpsnd) and the Frame's microphone goes to it (audin), on
            // the one session per machine that carries them. FreeRDP's default backend is PulseAudio.
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_AudioPlayback, audio);
            b(FreeRDP_Settings_Keys_Bool_FreeRDP_AudioCapture, audio);
            // With no latency, rdpsnd drops any chunk that arrives behind more than two others
            // ("Buffer overrun ... dropping"), and sound waits behind the picture on this thread, so
            // it came in bursts and chopped. 200 ms is its room, and PulseAudio's buffer. A session
            // without sound still gets rdpsnd, since network auto-detect turns device redirection on
            // and FreeRDP then adds its fake player, so that player only takes 8 kHz: krdp sends 48 kHz
            // or nothing, and this way it sends nothing (it streamed every session's copy before).
            let snd: &[&str] = if audio { &["rdpsnd", "sys:pulse", "latency:200"] } else { &["rdpsnd", "sys:fake", "rate:8000"] };
            let args: Vec<CString> = snd.iter().map(|a| cstr(a)).collect();
            let argv: Vec<*const std::os::raw::c_char> = args.iter().map(|a| a.as_ptr()).collect();
            freerdp_client_add_static_channel(s, argv.len(), argv.as_ptr());
            freerdp_client_add_dynamic_channel(s, argv.len(), argv.as_ptr());
            // Static channels (the clipboard) on this thread. H.264's colour conversion ignores
            // this flag, so install.sh patches FreeRDP's h264.c to run it here too instead of on
            // WinPR's 8-thread pool (3.5-4 ms less CPU a 3840x1080 frame, ~5 ms more latency; efficiency-plan.md 2a)
            n(FreeRDP_Settings_Keys_UInt32_FreeRDP_ThreadingFlags, THREADING_FLAGS_DISABLE_THREADS);
            p.rdp.store(c, Release);
            crate::ffi::new_session();
            if freerdp_client_start(c) == 0 && freerdp_connect((*c).instance) != 0 {
                p.connected.store(true, Release);
                p.down.store(false, Relaxed);
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
                    if carries_audio(p) != audio {
                        eprintln!("{}: audio moves {}, reconnecting", p.v.name, if audio { "off it" } else { "onto it" });
                        moved = true;
                        break;
                    }
                    let ms = crate::gpu::wait(p).as_millis() as u32;
                    if count == 0 || WaitForMultipleObjects(count, handles.as_ptr(), 0, ms) == WAIT_FAILED {
                        break;
                    }
                    if !wake.is_null() {
                        ResetEvent(wake); // any poke from here on wakes the next wait
                    }
                    let _g = crate::ffi::lock(&p.lock);
                    IN_SESSION.set(true);
                    let ok = freerdp_check_event_handles(c);
                    IN_SESSION.set(false);
                    if ok == 0 {
                        break;
                    }
                    if crate::ffi::session_failed() {
                        eprintln!("{}: a callback panicked, so this session ends and reconnects", p.v.name);
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
                p.down.store(true, Relaxed);
                eprintln!("{}: can't connect (0x{:08x})", p.v.name, freerdp_get_last_error(c));
            }
            detach(p);
            freerdp_client_stop(c);
            freerdp_client_context_free(c);
        }
        if pop {
            break; // a window's session ending means the window (or its server) closed, so no reconnect
        }
        if !std::mem::take(&mut moved) {
            pause();
        }
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
    let _g = crate::ffi::lock(&p.lock);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A panel whose host server is down waits five minutes, or until Connect, or until it's closed.
    #[test]
    fn a_down_host_waits_for_connect_or_five_minutes() {
        let s = Duration::from_secs;
        assert!(keep_waiting(s(10), true, false));
        assert!(keep_waiting(s(299), true, false));
        assert!(!keep_waiting(s(300), true, false), "then it asks again");
        assert!(!keep_waiting(s(10), true, true), "Connect asks at once");
        assert!(!keep_waiting(s(10), false, false), "closed: stop");
    }

    /// The agent's status says which monitor's server is broken, and only that panel shows it.
    #[test]
    fn reads_its_own_monitors_health() {
        let st = serde_json::json!({"ok": true, "health": [
            {"index": 0, "health": "ok"}, {"index": 1, "health": "down", "restarts": 5}, {"index": 2, "health": "degraded"}]});
        assert_eq!((host_health(&st, 0), host_health(&st, 1), host_health(&st, 2)), (HOST_OK, HOST_DOWN, HOST_DEGRADED));
        assert_eq!(host_health(&st, 3), HOST_OK, "a monitor it doesn't list");
        assert_eq!(host_health(&serde_json::json!({"ok": true}), 0), HOST_OK, "an older agent says nothing");
    }
}
