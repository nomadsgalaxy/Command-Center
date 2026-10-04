//! The Frame session's side: its clipboard through wlr-data-control. KWin 6.2 has
//! zwlr_data_control_manager_v1 (v2) for any client, with no focus needed and no grant, so this
//! is its own plain connection next to session.rs's. (ext-data-control-v1 replaces it in later
//! KWins; it's the same protocol under a new name.)
//!
//! A selection that isn't ours is a Frame copy: the hub gets its formats and reads one only
//! when something pastes it. Ours is a machine's copy we put there, and a Frame app pasting it
//! asks our source for it.
use super::formats::{from_mime, from_mimes, mimes, to_uri_list};
use super::{Data, Fmt, HUB, Want, cliprdr, run};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop, event_created_child};

pub mod proto {
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/wlr-data-control-unstable-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/wlr-data-control-unstable-v1.xml");
}
use proto::zwlr_data_control_device_v1::{self as device, ZwlrDataControlDeviceV1};
use proto::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use proto::zwlr_data_control_offer_v1::{self as offer, ZwlrDataControlOfferV1 as Offer};
use proto::zwlr_data_control_source_v1::{self as source, ZwlrDataControlSourceV1 as Source};

const MOST: usize = 256 << 20; // the biggest thing we read from a Frame app (a screenshot is ~30 MB)
const WAIT: Duration = Duration::from_secs(10); // how long a Frame app gets to hand it over

struct Frame {
    conn: Connection,
    qh: QueueHandle<St>,
    mgr: ZwlrDataControlManagerV1,
    dev: ZwlrDataControlDeviceV1,
    offer: Option<Offer>,  // the current selection, when it's someone else's
    ours: Option<Source>,  // our selection, until KWin cancels it (someone copied)
    starting: bool,        // the selection KWin sends on connecting was there before us, so it's not a copy
}

static FRAME: Mutex<Option<Frame>> = Mutex::new(None);
/// Frame apps' pipes waiting on a paste, by token.
static PIPES: Mutex<Vec<(u64, OwnedFd)>> = Mutex::new(Vec::new());
static TOKEN: AtomicU64 = AtomicU64::new(1);

struct St;

/// The thread: connects to the session, dispatches until it goes away, and tries again every 5 s.
pub fn thread() {
    let mut last = String::new();
    while !crate::QUIT.load(Relaxed) {
        if let Err(e) = connect() {
            if e != last {
                eprintln!("clipboard: Frame session: {e}");
            }
            last = e;
        } else {
            last.clear();
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn connect() -> Result<(), String> {
    let env = crate::session::Env::discover(crate::session::DESKTOP).ok_or("no session")?;
    serve(&env.wayland)
}

fn serve(path: &std::path::Path) -> Result<(), String> {
    let sock = UnixStream::connect(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let conn = Connection::from_socket(sock).map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<St>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|_| "no seat")?;
    let mgr: ZwlrDataControlManagerV1 = globals.bind(&qh, 1..=2, ()).map_err(|_| "no zwlr_data_control_manager_v1")?;
    let dev = mgr.get_data_device(&seat, &qh, ());
    *FRAME.lock().unwrap() = Some(Frame { conn: conn.clone(), qh, mgr, dev, offer: None, ours: None, starting: true });
    let mut st = St;
    let r = queue.roundtrip(&mut st); // takes in the selection that was already there
    if let Some(f) = FRAME.lock().unwrap().as_mut() {
        f.starting = false;
    }
    eprintln!("clipboard: Frame session up");
    if r.is_ok() {
        while queue.blocking_dispatch(&mut st).is_ok() {}
    }
    *FRAME.lock().unwrap() = None;
    Err("connection lost".into())
}

fn offered(o: &Offer) -> Vec<String> {
    o.data::<Mutex<Vec<String>>>().map(|m| m.lock().unwrap().clone()).unwrap_or_default()
}

fn selection(id: Option<Offer>) {
    let mut g = FRAME.lock().unwrap();
    let Some(f) = g.as_mut() else { return };
    if let Some(old) = f.offer.take() {
        old.destroy();
    }
    f.offer = id;
    if f.starting || f.ours.is_some() {
        return; // there before us, or ours
    }
    let types = f.offer.as_ref().map(offered).unwrap_or_default();
    drop(g);
    let fmts: Vec<Fmt> = from_mimes(&types).into_iter().map(|(f, _)| f).collect();
    if fmts.is_empty() {
        return;
    }
    eprintln!("clipboard: the Frame session copied {fmts:?}");
    let chans = cliprdr::chans();
    let acts = HUB.lock().unwrap().frame_announce(fmts, &chans, Instant::now());
    run(acts);
}

// ------------------------------------------------------------------ the hub's actions

/// Makes a machine's copy the Frame session's selection.
pub fn offer(fmts: &[Fmt]) {
    let mut g = FRAME.lock().unwrap();
    let Some(f) = g.as_mut() else { return };
    let src = f.mgr.create_data_source(&f.qh, ());
    for m in fmts.iter().flat_map(|f| mimes(*f)) {
        src.offer(m.to_string());
    }
    f.dev.set_selection(Some(&src));
    if let Some(old) = f.ours.replace(src) {
        old.destroy();
    }
    let _ = f.conn.flush();
}

/// Reads a format from the Frame selection, off this thread; it goes to the hub as frame_data.
pub fn fetch(copy: u64, fmt: Fmt) {
    let g = FRAME.lock().unwrap();
    let target = g.as_ref().and_then(|f| {
        let o = f.offer.as_ref()?;
        let types = offered(o);
        let mime = from_mimes(&types).into_iter().find(|(x, _)| *x == fmt)?.1.to_string();
        Some((o.clone(), mime, f.conn.clone()))
    });
    let (Some((o, mime, conn)), Some((r, w))) = (target, pipe()) else {
        drop(g);
        let acts = HUB.lock().unwrap().frame_data(copy, fmt, None);
        return run(acts);
    };
    o.receive(mime, w.as_fd());
    let _ = conn.flush();
    drop((w, g)); // ours closed, so the app's close ends the read
    std::thread::spawn(move || {
        let data = read_all(r);
        let acts = HUB.lock().unwrap().frame_data(copy, fmt, data); // not held while they run
        run(acts);
    });
}

/// Answers a Frame app's paste. Files from a machine get fetched first (cliprdr::download).
pub fn give(token: u64, fmt: Fmt, data: Data, src: Option<usize>, copy: u64) {
    let fd = {
        let mut p = PIPES.lock().unwrap();
        let i = p.iter().position(|(t, _)| *t == token);
        i.map(|i| p.remove(i).1)
    };
    let Some(fd) = fd else { return };
    std::thread::spawn(move || {
        let bytes = match (fmt, src, data) {
            (Fmt::Files, Some(src), Some(d)) => cliprdr::download(src, &d, copy).map(|tops| to_uri_list(&tops)),
            (_, _, d) => d.map(|d| d.to_vec()),
        };
        if let Some(b) = bytes {
            let _ = std::fs::File::from(fd).write_all(&b); // a closed pipe is the app giving up; fine
        }
    });
}

fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    (unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == 0).then(|| unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Everything until the app closes its end, unless it takes longer than WAIT or sends more than MOST.
fn read_all(fd: OwnedFd) -> Option<Vec<u8>> {
    let end = Instant::now() + WAIT;
    let mut f = std::fs::File::from(fd);
    let (mut out, mut buf) = (Vec::new(), vec![0u8; 1 << 16]);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as i32;
        let mut p = libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        if left == 0 || unsafe { libc::poll(&mut p, 1, left) } <= 0 {
            return None;
        }
        match f.read(&mut buf) {
            Ok(0) => return Some(out),
            Ok(n) if out.len() + n <= MOST => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
}

// ------------------------------------------------------------------ Wayland events

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for St {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for St {
    fn event(_: &mut Self, _: &ZwlrDataControlDeviceV1, e: device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            device::Event::Selection { id } => selection(id),
            device::Event::PrimarySelection { id: Some(o) } => o.destroy(), // middle-click paste stays the Frame's own
            device::Event::Finished => *FRAME.lock().unwrap() = None,
            _ => {}
        }
    }

    event_created_child!(St, ZwlrDataControlDeviceV1, [
        device::EVT_DATA_OFFER_OPCODE => (Offer, Mutex::new(Vec::<String>::new())),
    ]);
}

impl Dispatch<Offer, Mutex<Vec<String>>> for St {
    fn event(_: &mut Self, _: &Offer, e: offer::Event, types: &Mutex<Vec<String>>, _: &Connection, _: &QueueHandle<Self>) {
        let offer::Event::Offer { mime_type } = e;
        types.lock().unwrap().push(mime_type);
    }
}

impl Dispatch<Source, ()> for St {
    fn event(_: &mut Self, src: &Source, e: source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            source::Event::Send { mime_type, fd } => {
                let Some(fmt) = from_mime(&mime_type) else { return };
                let token = TOKEN.fetch_add(1, Relaxed);
                PIPES.lock().unwrap().push((token, fd));
                let acts = HUB.lock().unwrap().want(Want::Frame(token), fmt);
                run(acts);
            }
            source::Event::Cancelled => {
                let mut g = FRAME.lock().unwrap();
                if let Some(f) = g.as_mut().filter(|f| f.ours.as_ref() == Some(src)) {
                    f.ours = None; // someone copied, so the next selection is theirs
                }
                src.destroy();
            }
        }
    }
}

delegate_noop!(St: ignore WlSeat);
delegate_noop!(St: ZwlrDataControlManagerV1);

#[cfg(test)]
mod tests {
    //! Against a private, headless KWin, never the live session's:
    //!   d=~/.cache/cc-cliptest; mkdir -p -m700 $d
    //!   XDG_RUNTIME_DIR=$d dbus-run-session kwin_wayland --virtual --socket kwin --no-lockscreen &
    //!   CC_CLIP_TEST_WAYLAND=$d/kwin cargo test -p cc-panels frame_clipboard -- --ignored
    use super::*;

    /// A second client playing a Frame app, with its own data-control device.
    struct App {
        offer: Option<Offer>,
        text: Option<Vec<u8>>,
        sent: bool,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<ZwlrDataControlDeviceV1, ()> for App {
        fn event(app: &mut Self, _: &ZwlrDataControlDeviceV1, e: device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let device::Event::Selection { id } = e {
                app.offer = id;
            }
        }
        event_created_child!(App, ZwlrDataControlDeviceV1, [device::EVT_DATA_OFFER_OPCODE => (Offer, Mutex::new(Vec::<String>::new()))]);
    }
    impl Dispatch<Offer, Mutex<Vec<String>>> for App {
        fn event(_: &mut Self, _: &Offer, e: offer::Event, types: &Mutex<Vec<String>>, _: &Connection, _: &QueueHandle<Self>) {
            let offer::Event::Offer { mime_type } = e;
            types.lock().unwrap().push(mime_type);
        }
    }
    impl Dispatch<Source, ()> for App {
        fn event(app: &mut Self, _: &Source, e: source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let source::Event::Send { fd, .. } = e {
                let _ = std::fs::File::from(fd).write_all(app.text.as_deref().unwrap_or_default());
                app.sent = true;
            }
        }
    }
    delegate_noop!(App: ignore WlSeat);
    delegate_noop!(App: ZwlrDataControlManagerV1);

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore]
    fn frame_clipboard_both_ways() {
        let path = std::path::PathBuf::from(std::env::var("CC_CLIP_TEST_WAYLAND").expect("CC_CLIP_TEST_WAYLAND: a private KWin's socket"));
        let p = path.clone();
        std::thread::spawn(move || serve(&p));
        wait_for("our connection", || FRAME.lock().unwrap().as_ref().is_some_and(|f| !f.starting));

        let conn = Connection::from_socket(UnixStream::connect(&path).unwrap()).unwrap();
        let (globals, mut q) = registry_queue_init::<App>(&conn).unwrap();
        let qh = q.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
        let mgr: ZwlrDataControlManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
        let dev = mgr.get_data_device(&seat, &qh, ());
        let mut app = App { offer: None, text: Some(b"from a Frame app".to_vec()), sent: false };
        q.roundtrip(&mut app).unwrap();

        // the app copies: the hub hears of it, and reads it only once something pastes
        let src = mgr.create_data_source(&qh, ());
        src.offer("text/plain;charset=utf-8".into());
        src.offer("text/html".into());
        dev.set_selection(Some(&src));
        q.roundtrip(&mut app).unwrap();
        wait_for("the Frame copy", || HUB.lock().unwrap().owner() == Some(&super::super::Owner::Frame));
        let copy = HUB.lock().unwrap().copy();
        let acts = HUB.lock().unwrap().want(Want::Rdp(0), Fmt::Text);
        assert_eq!(acts, vec![super::super::Act::FetchFrame(copy, Fmt::Text)]);
        let pump = std::thread::spawn(move || {
            while !app.sent {
                q.blocking_dispatch(&mut app).unwrap();
            }
            (q, app)
        });
        fetch(copy, Fmt::Text);
        let (mut q, mut app) = pump.join().unwrap();
        wait_for("the text read", || HUB.lock().unwrap().cached(Fmt::Text).is_some());
        assert_eq!(HUB.lock().unwrap().cached(Fmt::Text).as_deref(), Some(&b"from a Frame app"[..]));

        // a machine copies: it becomes the Frame selection, and the app pastes it from us
        let t = Instant::now();
        let acts = HUB.lock().unwrap().remote_announce(0, "desktop", vec![Fmt::Text], &[], t);
        assert_eq!(acts, vec![super::super::Act::FetchRdp(0, Fmt::Text)]);
        let acts = HUB.lock().unwrap().remote_data(0, Some(b"from the desktop".to_vec()), &[], t);
        run(acts); // OfferFrame
        q.roundtrip(&mut app).unwrap();
        let o = app.offer.clone().expect("our selection");
        assert!(offered(&o).contains(&"text/plain;charset=utf-8".to_string()));
        let (r, w) = pipe().unwrap();
        o.receive("text/plain;charset=utf-8".into(), w.as_fd());
        conn.flush().unwrap();
        drop(w);
        assert_eq!(read_all(r).as_deref(), Some(&b"from the desktop"[..]));
        // and our own selection didn't come back to the hub as a Frame copy
        assert_eq!(HUB.lock().unwrap().owner(), Some(&super::super::Owner::Machine("desktop".into())));
    }
}
