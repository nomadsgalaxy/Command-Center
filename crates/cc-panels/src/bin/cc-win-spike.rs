//! Stage 0 of window panels (docs/window-panels-design2.md §16). It takes one window from the
//! nested session, streams it with KWin's screencast onto a SteamVR overlay, and clicks into it
//! through fake_input.
//!
//!   cc-win-spike [<uuid> <cg_x> <cg_y>] [--output[=WL-2]] [--virtual WxH] [--watch]
//!
//! Whichever of window, --virtual and --output comes first goes on the overlay
//! "controlcenter.spike" (1 m wide, 1.2 m ahead). The others only count frames. --virtual makes
//! a KWin virtual output (desktop mode's monitor 2). --watch logs every windowAdded, using a KWin
//! script that calls back over D-Bus. Ctrl+C ends it.
#![allow(dead_code)]
#[path = "../back.rs"]
mod back; // capture.rs tells it what went up, so it has to be here
#[path = "../capture.rs"]
mod capture;
#[path = "../geometry.rs"]
mod geometry;
#[path = "../session.rs"]
mod session;
#[path = "../vr.rs"]
mod vr;

use openvr_sys as sys;
use session::{Session, StreamEvent};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Relaxed);
}

enum Kind {
    Window { uuid: String, cg: (i32, i32) },
    Output(String),
    Virtual(i32, i32),
}

struct Src {
    label: String,
    kind: Kind,
    stream: Option<session::Stream>,
    feed: Option<Arc<capture::Feed>>,
    asked: Instant,
    first: bool,
    last_frames: u32,
}

const VIRTUAL: &str = "cc-monitor-2";

impl Src {
    /// Where the stream's top-left sits in the session, and its pixels per logical unit.
    fn origin(&self, s: &Session) -> Option<((i32, i32), f64)> {
        match &self.kind {
            Kind::Window { cg, .. } => Some((*cg, 1.0)), // preferredBufferScale is 1 today
            Kind::Output(n) => s.output(n).map(|o| ((o.x, o.y), o.scale as f64)),
            Kind::Virtual(..) => {
                let outs = s.outputs.lock().unwrap();
                outs.iter().find(|o| o.name.contains(VIRTUAL)).map(|o| ((o.x, o.y), o.scale as f64))
            }
        }
    }

    fn open(&mut self, s: &Session) {
        self.asked = Instant::now();
        self.first = false;
        self.stream = match &self.kind {
            Kind::Window { uuid, .. } => s.stream_window(uuid, false),
            Kind::Output(n) => match s.output(n) {
                Some(o) => s.stream_output(&o.wl, false),
                None => {
                    let names: Vec<_> = s.outputs.lock().unwrap().iter().map(|o| o.name.clone()).collect();
                    eprintln!("{}: no such output (there are {})", self.label, names.join(", "));
                    None
                }
            },
            Kind::Virtual(w, h) => s.stream_virtual(VIRTUAL, *w, *h),
        };
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    "usage: cc-win-spike [<uuid> <cg_x> <cg_y>] [--output[=NAME]] [--virtual WxH] [--watch] [--for MIN]".into()
}

fn run() -> Result<(), String> {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    let (mut srcs, mut watch, mut pos) = (Vec::new(), false, Vec::new());
    let mut minutes = 3.0; // it ends by itself, since in the headset there's no terminal to Ctrl+C

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--watch" => watch = true,
            "--for" => minutes = args.next().and_then(|m| m.parse().ok()).ok_or_else(usage)?,
            "--virtual" => {
                let wh = args.next().ok_or_else(usage)?;
                let (w, h) = wh.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).ok_or_else(usage)?;
                srcs.push(Kind::Virtual(w, h));
            }
            "--output" => srcs.push(Kind::Output("WL-2".into())),
            _ if a.starts_with("--output=") => srcs.push(Kind::Output(a["--output=".len()..].into())),
            _ if a.starts_with("--") => return Err(usage()),
            _ => pos.push(a),
        }
    }
    match pos.as_slice() {
        [] => {}
        [uuid, x, y] => {
            let cg = (x.parse().map_err(|_| usage())?, y.parse().map_err(|_| usage())?);
            srcs.insert(0, Kind::Window { uuid: uuid.clone(), cg });
        }
        _ => return Err(usage()),
    }
    if srcs.is_empty() && !watch {
        return Err(usage());
    }
    srcs.sort_by_key(|k| match k {
        Kind::Window { .. } => 0,
        Kind::Virtual(..) => 1,
        Kind::Output(_) => 2,
    });
    let mut srcs: Vec<Src> = srcs
        .into_iter()
        .map(|kind| Src {
            label: match &kind {
                Kind::Window { uuid, .. } => format!("window {uuid}"),
                Kind::Output(n) => format!("output {n}"),
                Kind::Virtual(w, h) => format!("virtual {w}x{h}"),
            },
            kind,
            stream: None,
            feed: None,
            asked: Instant::now(),
            first: false,
            last_frames: 0,
        })
        .collect();

    // Use Command Center's desktop session, else the one our environment names.
    let find = || session::Env::discover(session::DESKTOP).or_else(session::Env::from_process);
    let env = find().ok_or("no session: no cc-desktop plasmashell, and no WAYLAND_DISPLAY")?;
    eprintln!("session: {} (bus {})", env.wayland.display(), env.dbus);
    let (tx, sessions) = mpsc::channel();
    std::thread::spawn(move || session::run(find, |s| drop(tx.send(s)), &QUIT));
    let mut s = sessions.recv_timeout(Duration::from_secs(10)).map_err(|_| "no window access (see above)")?;
    report(&s);

    let _watch = if watch { Some(Watch::start(&env.dbus)?) } else { None };

    vr::init().map_err(|e| format!("SteamVR: {e}"))?;
    let overlay = vr::create_overlay("controlcenter.spike", "Command Center spike")?;
    call!(ov, SetOverlayWidthInMeters, overlay, 1.0);
    call!(ov, SetOverlayInputMethod, overlay, sys::VROverlayInputMethod_Mouse);
    call!(ov, SetOverlayFlag, overlay, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
    call!(ov, SetOverlayFlag, overlay, sys::VROverlayFlags_IgnoreTextureAlpha, true);
    vr::place(overlay, &ahead(1.2));

    let cap = capture::Capture::start();
    for src in &mut srcs {
        src.open(&s);
    }
    let mut shown = capture::Shown::default();
    let mut scale = (0, 0);
    let mut status = Instant::now();
    let start = Instant::now();
    eprintln!("running for {minutes} min (Ctrl+C ends it sooner)");
    while !QUIT.load(Relaxed) {
        if minutes > 0.0 && start.elapsed().as_secs_f64() > minutes * 60.0 {
            eprintln!("time's up");
            break;
        }
        if let Ok(n) = sessions.try_recv() {
            // reconnected, so everything starts streaming again
            s = n;
            report(&s);
            shown.free();
            for (key, src) in srcs.iter_mut().enumerate() {
                cap.close(key as u64);
                src.feed = None;
                src.open(&s);
            }
        }
        for (key, src) in srcs.iter_mut().enumerate() {
            while let Some(e) = src.stream.as_ref().and_then(|st| st.events.try_recv().ok()) {
                match e {
                    StreamEvent::Created(node) => {
                        eprintln!("{}: node {node}", src.label);
                        src.feed = Some(cap.open(key as u64, node, &src.label, 60)); // at the output's own rate
                    }
                    StreamEvent::Failed(e) => eprintln!("{}: failed: {e}", src.label),
                    StreamEvent::Closed => {
                        eprintln!("{}: closed", src.label);
                        src.stream = None;
                        src.feed = None;
                        cap.close(key as u64);
                    }
                }
            }
            let Some(feed) = &src.feed else { continue };
            let up = if key == 0 { shown.tick(&cap, 0, feed, overlay, &src.label) } else { feed.frames.load(Relaxed) > 0 };
            if up && !src.first {
                src.first = true;
                eprintln!("{}: first frame +{}ms", src.label, src.asked.elapsed().as_millis());
                if key == 0 {
                    call!(ov, ShowOverlay, overlay); // shown on its first frame, so there's no black flash
                }
            }
        }
        if shown.size != scale {
            scale = shown.size;
            let mut v = sys::HmdVector2_t { v: [scale.0 as f32, scale.1 as f32] };
            call!(ov, SetOverlayMouseScale, overlay, &mut v);
        }
        if let Some(src) = srcs.first() {
            mouse(overlay, src, &s, scale.1 as f64);
        }
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        while call!(sys, PollNextEvent, &mut e, size_of::<vr::VREvent_t>() as u32) {
            if e.eventType == sys::EVREventType_VREvent_Quit {
                QUIT.store(true, Relaxed);
            }
        }
        if status.elapsed() > Duration::from_secs(5) {
            let t = status.elapsed().as_secs_f64();
            status = Instant::now();
            for src in &mut srcs {
                let n = src.feed.as_ref().map_or(0, |f| f.frames.load(Relaxed));
                eprintln!("{} frames/s: {:.1}", src.label, (n - src.last_frames.min(n)) as f64 / t);
                src.last_frames = n;
            }
        }
        std::thread::sleep(Duration::from_millis(11));
    }

    for (key, src) in srcs.iter_mut().enumerate() {
        src.stream = None; // closing it makes KWin stop rendering it
        cap.close(key as u64);
    }
    shown.free();
    call!(ov, DestroyOverlay, overlay);
    vr::shutdown();
    Ok(())
}

fn report(s: &Session) {
    let g: Vec<_> = s.globals.iter().map(|g| format!("{} v{}", g.interface, g.version)).collect();
    eprintln!("globals: {}", g.join(", "));
    eprintln!(
        "grant: {:?}: screencast {}, fake input {}",
        s.grant,
        if s.screencast.is_some() { "bound" } else { "missing" },
        if s.fake.is_some() { "bound" } else { "missing" }
    );
    let o: Vec<_> = s.outputs.lock().unwrap().iter().map(|o| format!("{} {}x{}+{}+{}", o.name, o.w, o.h, o.x, o.y)).collect();
    eprintln!("outputs: {}", o.join(", "));
}

/// 1.2 m ahead of the head, level and facing it.
fn ahead(d: f64) -> geometry::Mat {
    let m = vr::head().unwrap_or([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.6], [0.0, 0.0, 1.0, 0.0]]);
    let f = [-m[0][2] as f64, 0.0, -m[2][2] as f64];
    let n = geometry::norm(&f).max(1e-6);
    let f = [f[0] / n, 0.0, f[2] / n];
    let (yaw, pitch) = geometry::angles(&f);
    let centre = [m[0][3] as f64 + f[0] * d, m[1][3] as f64, m[2][3] as f64 + f[2] * d];
    geometry::panel_matrix(&geometry::Pose { centre, yaw, pitch, width: 1.0, ..Default::default() })
}

/// Passes laser moves and clicks on the overlay through to the streamed thing's place in the
/// session.
fn mouse(overlay: vr::Handle, src: &Src, s: &Session, h: f64) {
    let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
    while call!(ov, PollNextOverlayEvent, overlay, &mut e, size_of::<vr::VREvent_t>() as u32) {
        let Some((origin, px)) = src.origin(s) else { continue };
        let m = unsafe { e.data.mouse };
        let (x, y) = session::to_global(origin, px, h, m.x as f64, m.y as f64);
        let button = match m.button {
            sys::EVRMouseButton_VRMouseButton_Right => session::BTN_RIGHT,
            sys::EVRMouseButton_VRMouseButton_Middle => session::BTN_MIDDLE,
            _ => session::BTN_LEFT,
        };
        match e.eventType {
            sys::EVREventType_VREvent_MouseMove => s.pointer_to(x, y),
            sys::EVREventType_VREvent_MouseButtonDown => {
                s.pointer_to(x, y);
                s.button(button, true);
                eprintln!("click {x:.0},{y:.0}");
            }
            sys::EVREventType_VREvent_MouseButtonUp => {
                s.pointer_to(x, y);
                s.button(button, false);
                eprintln!("release {x:.0},{y:.0}");
            }
            _ => {}
        }
    }
}

/// --watch: a KWin script that reports every windowAdded to us over the session's bus.
struct Watch {
    bus: zbus::blocking::Connection,
}

struct Events;

#[zbus::interface(name = "org.controlcenter.Spike")]
impl Events {
    fn event(&self, json: &str) {
        eprintln!("windowAdded {json}");
    }
}

const SCRIPT: &str = r#"workspace.windowAdded.connect(function (w) {
    const g = w.clientGeometry;
    callDBus("org.controlcenter.Spike", "/", "org.controlcenter.Spike", "Event", JSON.stringify({
        uuid: w.internalId.toString(), class: w.resourceClass, caption: w.caption, type: w.windowType,
        normal: w.normalWindow, dialog: w.dialog, popup: w.popupWindow, menu: w.popupMenu,
        dropdown: w.dropdownMenu, tooltip: w.tooltip, combo: w.comboBox, managed: w.managed,
        transient: w.transient, x: g.x, y: g.y, w: g.width, h: g.height }));
});
"#;

impl Watch {
    fn start(address: &str) -> Result<Watch, String> {
        let e = |e: zbus::Error| format!("--watch: {e}");
        let bus = zbus::blocking::connection::Builder::address(address)
            .map_err(e)?
            .name("org.controlcenter.Spike")
            .map_err(e)?
            .serve_at("/", Events)
            .map_err(e)?
            .build()
            .map_err(e)?;
        let path = format!("{}/.cache/control-center/cc-spike-watch.js", std::env::var("HOME").unwrap_or_default());
        std::fs::write(&path, SCRIPT).map_err(|err| format!("--watch: {path}: {err}"))?;
        let w = Watch { bus };
        w.unload();
        let id: i32 = w
            .bus
            .call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "loadScript", &(path.as_str(), "cc-spike-watch"))
            .and_then(|m| m.body().deserialize())
            .map_err(e)?;
        w.bus.call_method(Some("org.kde.KWin"), format!("/Scripting/Script{id}").as_str(), Some("org.kde.kwin.Script"), "run", &()).map_err(e)?;
        eprintln!("watch: script {id} loaded");
        Ok(w)
    }

    fn unload(&self) {
        let _ = self.bus.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &("cc-spike-watch",));
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.unload();
    }
}
