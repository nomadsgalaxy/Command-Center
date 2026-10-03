//! The desktop session we show windows from (cc-desktop, Plasma in a headless KWin). We find it through its plasmashell and talk to it over Wayland. KWin's
//! screencast gives each window (or output) a PipeWire stream, and fake_input clicks and types
//! into it.
//!
//! Heads up: both are restricted globals. KWin only advertises them to a client named by its
//! .desktop grant (org.controlcenter.panels.desktop), matched by security-context app id or by
//! exe path.
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use wayland_client::globals::{Global, GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};

pub mod proto {
    pub mod screencast {
        use wayland_client;
        use wayland_client::protocol::*;
        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/zkde-screencast-unstable-v1.xml");
        }
        use self::__interfaces::*;
        wayland_scanner::generate_client_code!("protocols/zkde-screencast-unstable-v1.xml");
    }
    pub mod fake_input {
        use wayland_client;
        pub mod __interfaces {
            wayland_scanner::generate_interfaces!("protocols/fake-input.xml");
        }
        use self::__interfaces::*;
        wayland_scanner::generate_client_code!("protocols/fake-input.xml");
    }
    pub mod security_context {
        use wayland_client;
        pub mod __interfaces {
            wayland_scanner::generate_interfaces!("protocols/security-context-v1.xml");
        }
        use self::__interfaces::*;
        wayland_scanner::generate_client_code!("protocols/security-context-v1.xml");
    }
}
use proto::fake_input::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;
use proto::screencast::zkde_screencast_stream_unstable_v1::{self as zstream, ZkdeScreencastStreamUnstableV1};
use proto::screencast::zkde_screencast_unstable_v1::ZkdeScreencastUnstableV1;
use proto::security_context::wp_security_context_manager_v1::WpSecurityContextManagerV1;
use proto::security_context::wp_security_context_v1::WpSecurityContextV1;

pub const DESKTOP: &str = "/run/user/1000/cc-desktop"; // session/cc-desktop's runtime dir
pub const APP_ID: &str = "org.controlcenter.panels"; // the grant's desktop id
const POINTER_HIDDEN: u32 = 1; // our laser draws its own dot
const POINTER_EMBEDDED: u32 = 2; // KWin draws its cursor into the stream
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// The environment the session's own processes see. We read it from its plasmashell and never
/// set it on our process, because set_var with many threads running is undefined.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub runtime: String,
    pub wayland: PathBuf, // the socket, absolute
    pub dbus: String,
    pub display: String,
    pub xauthority: String,
}

impl Env {
    /// The plasmashell whose XDG_RUNTIME_DIR is `runtime`. With PidMode=host we see the host's processes.
    pub fn discover(runtime: &str) -> Option<Env> {
        for d in std::fs::read_dir("/proc").ok()?.flatten() {
            let p = d.path();
            if std::fs::read_to_string(p.join("comm")).is_ok_and(|c| c.trim() == "plasmashell") {
                let Ok(raw) = std::fs::read(p.join("environ")) else { continue };
                let vars: Vec<(&str, &str)> =
                    raw.split(|&b| b == 0).filter_map(|kv| std::str::from_utf8(kv).ok()?.split_once('=')).collect();
                let get = |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()).unwrap_or_default();
                if get("XDG_RUNTIME_DIR") == runtime {
                    return Env::from(get);
                }
            }
        }
        // Inside cc-box we can't read another process's environ, so the cc-panels script writes
        // the session's variables here instead and keeps them fresh.
        let home = std::env::var("HOME").unwrap_or_default();
        let text = std::fs::read_to_string(format!("{home}/.cache/control-center/desktop-session.env")).ok()?;
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).unwrap_or_default().to_string();
        (get("XDG_RUNTIME_DIR") == runtime).then(|| Env::from(get)).flatten()
    }

    /// Our own environment, for when we were started inside the session or handed its variables.
    pub fn from_process() -> Option<Env> {
        Env::from(|k| std::env::var(k).unwrap_or_default())
    }

    fn from(get: impl Fn(&str) -> String) -> Option<Env> {
        let (runtime, wl) = (get("XDG_RUNTIME_DIR"), get("WAYLAND_DISPLAY"));
        if wl.is_empty() {
            return None;
        }
        let wayland = if wl.starts_with('/') { PathBuf::from(&wl) } else { PathBuf::from(&runtime).join(&wl) };
        Some(Env { runtime, wayland, dbus: get("DBUS_SESSION_BUS_ADDRESS"), display: get("DISPLAY"), xauthority: get("XAUTHORITY") })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grant {
    SecurityContext,
    ExePath,
    Refused,
}

/// A wl_output: its name (WL-0, Virtual-…) and its logical place in the session.
#[derive(Clone, Debug)]
pub struct Output {
    pub global: u32,
    pub wl: wl_output::WlOutput,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: i32, // mode pixels
    pub h: i32,
    pub scale: i32,
}

pub enum StreamEvent {
    Created(u32), // the PipeWire node
    Failed(String),
    Closed,
}

/// A screencast stream. Dropping it tells KWin to stop rendering for it.
pub struct Stream {
    proxy: ZkdeScreencastStreamUnstableV1,
    conn: Connection,
    pub events: mpsc::Receiver<StreamEvent>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.proxy.close();
        let _ = self.conn.flush();
    }
}

pub struct State {
    outputs: Arc<Mutex<Vec<Output>>>,
}

/// One connection to the session. Any thread can make requests (and then flush), but only the
/// session thread dispatches.
pub struct Session {
    pub conn: Connection,
    qh: QueueHandle<State>,
    pub grant: Grant,
    pub screencast: Option<ZkdeScreencastUnstableV1>,
    pub fake: Option<OrgKdeKwinFakeInput>,
    pub outputs: Arc<Mutex<Vec<Output>>>,
    pub globals: Vec<Global>, // what the session offered us
    _close: Option<OwnedFd>,  // the security context's listener stays alive while we hold this
}

impl Session {
    fn stream(&self, open: impl FnOnce(&ZkdeScreencastUnstableV1, mpsc::Sender<StreamEvent>) -> ZkdeScreencastStreamUnstableV1) -> Option<Stream> {
        let sc = self.screencast.as_ref()?;
        let (tx, events) = mpsc::channel();
        let proxy = open(sc, tx);
        let _ = self.conn.flush();
        Some(Stream { proxy, conn: self.conn.clone(), events })
    }

    /// Streams one window by its KWin internalId, at its clientGeometry × buffer scale.
    /// `cursor` has KWin draw its cursor in. That's for Plasma's bar: a controller's laser there
    /// gets no dot from us, so the session's own cursor shows where it points.
    pub fn stream_window(&self, uuid: &str, cursor: bool) -> Option<Stream> {
        let pointer = if cursor { POINTER_EMBEDDED } else { POINTER_HIDDEN };
        self.stream(|sc, tx| sc.stream_window(uuid.into(), pointer, &self.qh, tx))
    }

    pub fn stream_output(&self, out: &wl_output::WlOutput, cursor: bool) -> Option<Stream> {
        let pointer = if cursor { POINTER_EMBEDDED } else { POINTER_HIDDEN };
        self.stream(|sc, tx| sc.stream_output(out, pointer, &self.qh, tx))
    }

    /// A new output that only exists while it's streamed (desktop mode's monitor 2, 3, …). It
    /// shows up as a wl_output once KWin has made it.
    pub fn stream_virtual(&self, name: &str, w: i32, h: i32) -> Option<Stream> {
        self.stream(|sc, tx| sc.stream_virtual_output(name.into(), w, h, 1.0, POINTER_HIDDEN, &self.qh, tx))
    }

    pub fn output(&self, name: &str) -> Option<Output> {
        self.outputs.lock().unwrap().iter().find(|o| o.name == name).cloned()
    }

    /// Moves the pointer to a point in the session's global (logical) coordinates.
    pub fn pointer_to(&self, x: f64, y: f64) {
        if let Some(f) = &self.fake {
            f.pointer_motion_absolute(x, y);
            let _ = self.conn.flush();
        }
    }

    pub fn button(&self, code: u32, down: bool) {
        if let Some(f) = &self.fake {
            f.button(code, down as u32);
            let _ = self.conn.flush();
        }
    }

    /// Sends an evdev key to the focused window through KWin's xkb, so its kxkbrc layout applies.
    pub fn key(&self, code: u32, down: bool) {
        if let Some(f) = &self.fake {
            f.keyboard_key(code, down as u32);
            let _ = self.conn.flush();
        }
    }

    /// Scrolls in wl_pointer units (15 per wheel notch), positive down or right.
    pub fn axis(&self, horizontal: bool, value: f64) {
        if let Some(f) = &self.fake {
            f.axis(horizontal as u32, value);
            let _ = self.conn.flush();
        }
    }
}

/// Converts stream pixels from an overlay's mouse (bottom-left origin) to session coordinates.
/// `origin` is the streamed thing's top-left and `px` its stream pixels per logical unit.
pub fn to_global(origin: (i32, i32), px: f64, h: f64, mx: f64, my: f64) -> (f64, f64) {
    (origin.0 as f64 + mx / px, origin.1 as f64 + (h - my) / px)
}

fn bind_output(reg: &wl_registry::WlRegistry, qh: &QueueHandle<State>, outputs: &Mutex<Vec<Output>>, global: u32, version: u32) {
    let wl = reg.bind::<wl_output::WlOutput, _, _>(global, version.min(4), qh, global);
    outputs.lock().unwrap().push(Output { global, wl, name: String::new(), x: 0, y: 0, w: 0, h: 0, scale: 1 });
}

fn find(globals: &GlobalList, name: &str) -> Option<u32> {
    globals.contents().with_list(|l| l.iter().find(|g| g.interface == name).map(|g| g.version))
}

/// Gets the grant through a security context: a listening socket KWin accepts on for us, with
/// its clients tagged with our app id. KWin looks the grant up by that app id rather than our exe
/// path, and the app id survives rebuilds and the container's /usr.
fn secured(direct: &Connection, globals: &GlobalList, qh: &QueueHandle<State>, queue: &mut EventQueue<State>, state: &mut State, env: &Env) -> Result<(Connection, OwnedFd), String> {
    let mgr: WpSecurityContextManagerV1 = globals.bind(qh, 1..=1, ()).map_err(|e| e.to_string())?;
    let path = env.wayland.with_file_name(format!("cc-panels-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err("pipe".into());
    }
    let (close_r, close_w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let ctx: WpSecurityContextV1 = mgr.create_listener(listener.as_fd(), close_r.as_fd(), qh, ());
    ctx.set_sandbox_engine("cc".into());
    ctx.set_app_id(APP_ID.into());
    ctx.set_instance_id(std::process::id().to_string());
    ctx.commit();
    ctx.destroy();
    mgr.destroy();
    queue.roundtrip(state).map_err(|e| e.to_string())?; // KWin has its own copy of the socket now
    let sock = UnixStream::connect(&path).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&path);
    drop(listener);
    let _ = direct.flush();
    Ok((Connection::from_socket(sock?).map_err(|e| e.to_string())?, close_w))
}

/// Connects and binds what we use. A refused grant isn't an error: KWin just doesn't advertise
/// the globals, and the Session reports that.
pub fn connect(env: &Env) -> Result<(Session, EventQueue<State>, State), String> {
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let mut state = State { outputs: outputs.clone() };
    let sock = UnixStream::connect(&env.wayland).map_err(|e| format!("{}: {e}", env.wayland.display()))?;
    let direct = Connection::from_socket(sock).map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<State>(&direct).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let offered = globals.contents().clone_list();

    let mut chosen = None;
    if find(&globals, "wp_security_context_manager_v1").is_some() {
        match secured(&direct, &globals, &qh, &mut queue, &mut state, env) {
            Ok((conn, close)) => {
                let (g, q) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
                if find(&g, "zkde_screencast_unstable_v1").is_some() {
                    chosen = Some((conn, g, q, Grant::SecurityContext, Some(close)));
                }
            }
            Err(e) => eprintln!("session: no security context ({e}); trying the exe-path grant"),
        }
    }
    let (conn, globals, mut queue, grant, close) = match chosen {
        Some(c) => c,
        None => {
            let grant = if find(&globals, "zkde_screencast_unstable_v1").is_some() { Grant::ExePath } else { Grant::Refused };
            (direct, globals, queue, grant, None)
        }
    };
    let qh = queue.handle();
    let screencast = globals.bind::<ZkdeScreencastUnstableV1, _, _>(&qh, 1..=3, ()).ok();
    let fake = globals.bind::<OrgKdeKwinFakeInput, _, _>(&qh, 3..=4, ()).ok();
    if let Some(f) = &fake {
        f.authenticate("Command Center panels".into(), "VR panels for the desktop's windows".into()); // without this every event gets dropped
    }
    globals.contents().with_list(|l| {
        for g in l.iter().filter(|g| g.interface == "wl_output") {
            bind_output(globals.registry(), &qh, &outputs, g.name, g.version);
        }
    });
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?; // picks up the outputs' names and places
    Ok((Session { conn, qh, grant, screencast, fake, outputs, globals: offered, _close: close }, queue, state))
}

/// The session thread: connects, hands the session out, dispatches until it goes away and
/// retries every 5 s. `find` locates the session on each try, since it may have restarted
/// somewhere else.
pub fn run(find: impl Fn() -> Option<Env>, mut up: impl FnMut(Arc<Session>), quit: &std::sync::atomic::AtomicBool) {
    let mut last = String::new();
    while !quit.load(std::sync::atomic::Ordering::Relaxed) {
        let r = find().ok_or_else(|| "no session to show windows from".to_string()).and_then(|e| connect(&e));
        match r {
            Ok((s, mut queue, mut state)) => {
                let s = Arc::new(s);
                let refused = s.grant == Grant::Refused;
                if refused && last != "refused" {
                    eprintln!("session: no window access: KWin advertises neither screencast nor fake input to us (the {APP_ID}.desktop grant)");
                }
                last = if refused { "refused".into() } else { String::new() };
                if !refused {
                    up(s.clone());
                    while queue.blocking_dispatch(&mut state).is_ok() {}
                    eprintln!("session: connection lost");
                }
            }
            Err(e) => {
                if e != last {
                    eprintln!("session: {e}");
                }
                last = e;
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(state: &mut Self, reg: &wl_registry::WlRegistry, e: wl_registry::Event, _: &GlobalListContents, _: &Connection, qh: &QueueHandle<Self>) {
        match e {
            wl_registry::Event::Global { name, interface, version } if interface == "wl_output" => {
                bind_output(reg, qh, &state.outputs, name, version)
            }
            wl_registry::Event::GlobalRemove { name } => state.outputs.lock().unwrap().retain(|o| o.global != name),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(state: &mut Self, _: &wl_output::WlOutput, e: wl_output::Event, global: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let mut outs = state.outputs.lock().unwrap();
        let Some(o) = outs.iter_mut().find(|o| o.global == *global) else { return };
        match e {
            wl_output::Event::Geometry { x, y, .. } => (o.x, o.y) = (x, y),
            wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. } if f.contains(wl_output::Mode::Current) => {
                (o.w, o.h) = (width, height)
            }
            wl_output::Event::Scale { factor } => o.scale = factor.max(1),
            wl_output::Event::Name { name } => o.name = name,
            _ => {}
        }
    }
}

impl Dispatch<ZkdeScreencastStreamUnstableV1, mpsc::Sender<StreamEvent>> for State {
    fn event(_: &mut Self, _: &ZkdeScreencastStreamUnstableV1, e: zstream::Event, tx: &mpsc::Sender<StreamEvent>, _: &Connection, _: &QueueHandle<Self>) {
        let _ = tx.send(match e {
            zstream::Event::Created { node } => StreamEvent::Created(node),
            zstream::Event::Failed { error } => StreamEvent::Failed(error),
            zstream::Event::Closed => StreamEvent::Closed,
            _ => return,
        });
    }
}

delegate_noop!(State: ignore ZkdeScreencastUnstableV1);
delegate_noop!(State: ignore OrgKdeKwinFakeInput);
delegate_noop!(State: ignore WpSecurityContextManagerV1);
delegate_noop!(State: ignore WpSecurityContextV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_mouse_maps_to_global() {
        // a 1280x800 window at 3360,740: the overlay's bottom-left is the window's bottom-left
        assert_eq!(to_global((3360, 740), 1.0, 800.0, 0.0, 800.0), (3360.0, 740.0));
        assert_eq!(to_global((3360, 740), 1.0, 800.0, 10.0, 0.0), (3370.0, 1540.0));
        assert_eq!(to_global((0, 0), 2.0, 1600.0, 200.0, 1600.0), (100.0, 0.0)); // scale 2
    }
}
