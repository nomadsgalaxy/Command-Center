//! A trace of what cc-panels polls from SteamVR, so a bug only the headset shows can be replayed
//! as a test (docs/vr-harness.md). With ~/.cache/control-center/vr-trace there when cc-panels
//! starts, it writes a new ~/.cache/control-center/vr-trace-<epoch>-<pid>.jsonl each start (so a
//! restart never overwrites the last), private to you (0600), one JSON object a line:
//!   {"tick":"plasma","t":12.5,"dropped":0}     an events() pass starting: seconds since the trace began, events left out so far
//!   {"ev":"plasma","type":300,"dev":3,"age":0,"x":..,"y":..,"button":..,"cursor":..}   a pointer event
//!   {"ev":"plasma","type":305,"dev":3,"age":0,"dx":..,"dy":..,"scale":..,"cursor":..}  a scroll
//!   {"pose":3,"m":[[..],[..],[..]]}            a laser's pose as laser_pose gave it (m null: none)
//!   {"plasma":{...}}, {"taskbar":{...}}        where Plasma's bar and the taskbar's frame are
//! A trace is what you did with the lasers, so only the events a replay needs go in, with only
//! their own fields: pointer moves, presses and releases, focus coming and going, and scrolls.
//! Anything else, keyboard events included, is left out and only counted.
use crate::geometry::Mat;
use openvr_sys as sys;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::Mutex;

/// shortcut: stops writing at 64 MB, a few minutes of lasers at 90 Hz; enough for one bug.
const MAX: u64 = 64 << 20;

struct Out {
    file: std::io::BufWriter<std::fs::File>, // flushed once a pass (tick), so writing never stalls a frame on the disk
    written: u64,
    since: std::time::Instant,   // what the ticks' "t" counts from
    flushed: std::time::Instant, // the last flush, at most once a second
    failed: bool,                // a write or flush failed, and the log has said so once
}

impl Out {
    fn new(file: std::fs::File) -> Out {
        let now = std::time::Instant::now();
        Out { file: std::io::BufWriter::with_capacity(1 << 20, file), written: 0, since: now, flushed: now, failed: false }
    }

    /// A write's or flush's result: the first failure goes to the log, so a trace with a hole in
    /// it never looks whole.
    fn check(&mut self, r: std::io::Result<()>) -> bool {
        if let Err(e) = &r
            && !std::mem::replace(&mut self.failed, true)
        {
            eprintln!("vr-trace: writing it failed ({e}), so it's missing lines from here on");
        }
        r.is_ok()
    }
}

/// A new trace file in `dir`: vr-trace-<epoch>-<pid>.jsonl, or with -2, -3 ... after it if that's
/// taken, made new (never an old one opened again) and 0600, like viewers.conf.
fn create(dir: &std::path::Path) -> std::io::Result<(std::fs::File, std::path::PathBuf)> {
    use std::os::unix::fs::OpenOptionsExt;
    let epoch = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let base = format!("vr-trace-{epoch}-{}", std::process::id());
    for n in 1..100 {
        let path = dir.join(if n == 1 { format!("{base}.jsonl") } else { format!("{base}-{n}.jsonl") });
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free name for it"))
}

/// The trace, if one's being written. Every lock of it takes it even if a panic poisoned it
/// (ffi::lock, or flush's try_lock), so a bug in tracing can't turn into a panic every frame.
static OUT: Mutex<Option<Out>> = Mutex::new(None);

fn out() -> &'static Mutex<Option<Out>> {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let dir = format!("{}/.cache/control-center", crate::config::home_dir());
        if std::path::Path::new(&dir).join("vr-trace").exists() {
            match create(std::path::Path::new(&dir)) {
                Ok((file, path)) => {
                    eprintln!("vr-trace: writing {}", path.display());
                    *crate::ffi::lock(&OUT) = Some(Out::new(file));
                }
                Err(e) => eprintln!("vr-trace: can't make a trace in {dir}: {e}"),
            }
        }
    });
    &OUT
}

/// Whether a trace is being written. Cheap, so callers can skip building a line.
pub fn on() -> bool {
    crate::ffi::lock(out()).as_ref().is_some_and(|o| o.written < MAX)
}

fn write(v: Value) {
    let mut o = crate::ffi::lock(out());
    let Some(o) = o.as_mut().filter(|o| o.written < MAX) else { return };
    let line = format!("{v}\n");
    let r = o.file.write_all(line.as_bytes());
    if o.check(r) {
        o.written += line.len() as u64;
        if o.written >= MAX {
            let r = o.file.flush();
            o.check(r);
            eprintln!("vr-trace: it's {} MB, so that's all it writes", MAX >> 20);
        }
    }
}

/// Writes what's buffered to the disk, as best it can: a failure is logged once (Out::check), and
/// if the lock's held it gives up rather than wait. It's called on every way cc-panels ends: GiveBack's drop
/// (a normal quit, closing the Desktop, SteamVR's Quit, and SIGTERM or SIGINT, which only set
/// QUIT so main ends through it), the panic hook, and the exits after the cards' or the UI's
/// thread panicked. It never waits on the lock, since a panic can come while it's held, and
/// it never starts a trace.
pub fn flush() {
    let mut o = match OUT.try_lock() {
        Ok(o) => o,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    if let Some(o) = o.as_mut() {
        let r = o.file.flush();
        o.check(r);
    }
}

/// What an event type carries into a trace, if it goes in at all.
enum Kind {
    Pointer, // a move, press or release: where, which button
    Focus,   // the laser came onto the overlay or left it: nothing else
    Scroll,
}

fn kind(t: u32) -> Option<Kind> {
    match t {
        sys::EVREventType_VREvent_MouseMove | sys::EVREventType_VREvent_MouseButtonDown | sys::EVREventType_VREvent_MouseButtonUp => Some(Kind::Pointer),
        sys::EVREventType_VREvent_FocusEnter | sys::EVREventType_VREvent_FocusLeave => Some(Kind::Focus),
        sys::EVREventType_VREvent_ScrollDiscrete | sys::EVREventType_VREvent_ScrollSmooth => Some(Kind::Scroll),
        _ => None,
    }
}

/// Events left out of the trace so far (kind() is None).
static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The line for one polled event, with only its own fields; None for one that stays out.
pub fn event_line(who: &str, e: &sys::VREvent_t) -> Option<Value> {
    let mut v = json!({"ev": who, "type": e.eventType, "dev": e.trackedDeviceIndex, "age": e.eventAgeSeconds});
    match kind(e.eventType)? {
        Kind::Pointer => {
            let m = unsafe { e.data.mouse };
            (v["x"], v["y"], v["button"], v["cursor"]) = (json!(m.x), json!(m.y), json!(m.button), json!(m.cursorIndex));
        }
        Kind::Focus => {}
        Kind::Scroll => {
            let s = unsafe { e.data.scroll };
            (v["dx"], v["dy"], v["scale"], v["cursor"]) = (json!(s.xdelta), json!(s.ydelta), json!(s.viewportscale), json!(s.cursorIndex));
        }
    }
    Some(v)
}

/// An event back from its line.
#[cfg(test)]
pub fn event_from(v: &Value) -> Option<sys::VREvent_t> {
    let t = v["type"].as_u64()? as u32;
    let f = |k: &str| v[k].as_f64().unwrap_or(0.0) as f32;
    let u = |k: &str| v[k].as_u64().unwrap_or(0) as u32;
    let mut e = sys::VREvent_t { eventType: t, trackedDeviceIndex: v["dev"].as_u64()? as u32, eventAgeSeconds: f("age"), ..Default::default() };
    match kind(t)? {
        Kind::Pointer => e.data.mouse = sys::VREvent_Mouse_t { x: f("x"), y: f("y"), button: u("button"), cursorIndex: u("cursor") },
        Kind::Focus => {}
        Kind::Scroll => e.data.scroll = sys::VREvent_Scroll_t { xdelta: f("dx"), ydelta: f("dy"), unused: 0, viewportscale: f("scale"), cursorIndex: u("cursor") },
    }
    Some(e)
}

pub fn pose_line(dev: u32, m: Option<&Mat>) -> Value {
    json!({"pose": dev, "m": m})
}

#[cfg(test)]
pub fn pose_from(v: &Value) -> Option<(u32, Option<Mat>)> {
    let dev = v["pose"].as_u64()? as u32;
    Some((dev, serde_json::from_value(v["m"].clone()).ok()))
}

/// SteamVR's next event on overlay `h`, written to the trace as `who`'s.
pub fn poll(who: &str, h: crate::vr::Handle, e: &mut sys::VREvent_t) -> bool {
    let got = crate::call!(ov, PollNextOverlayEvent, h, e, size_of::<sys::VREvent_t>() as u32);
    if got {
        event(who, e);
    }
    got
}

/// A device's laser (vr::laser_pose), written to the trace.
pub fn laser_pose(dev: u32) -> Option<Mat> {
    let m = crate::vr::laser_pose(dev);
    pose(dev, m.as_ref());
    m
}

/// A pass starting, with the seconds since the trace began. What's buffered goes to the disk at
/// most once a second, so the main loop writes about one block a second, not one a pass.
pub fn tick(who: &str) {
    let t = {
        let mut o = crate::ffi::lock(out());
        let Some(o) = o.as_mut().filter(|o| o.written < MAX) else { return };
        if o.flushed.elapsed() >= std::time::Duration::from_secs(1) {
            let r = o.file.flush();
            o.check(r);
            o.flushed = std::time::Instant::now();
        }
        o.since.elapsed().as_secs_f64()
    };
    write(json!({ "tick": who, "t": t, "dropped": DROPPED.load(std::sync::atomic::Ordering::Relaxed) }));
}

pub fn event(who: &str, e: &sys::VREvent_t) {
    if on() {
        match event_line(who, e) {
            Some(v) => write(v),
            None => drop(DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
        }
    }
}

pub fn pose(dev: u32, m: Option<&Mat>) {
    if on() {
        write(pose_line(dev, m));
    }
}

/// Any other state a replay needs, under its own key (plasmabar.rs's placement).
pub fn state(key: &str, v: Value) {
    if on() {
        write(json!({ key: v }));
    }
}

/// `state`, but only when it's changed since `key`'s last line (the taskbar's, made every frame).
pub fn changed(key: &str, v: Value) {
    static LAST: Mutex<Vec<(String, Value)>> = Mutex::new(Vec::new());
    let mut last = LAST.lock().unwrap();
    if last.iter().any(|(k, l)| k == key && *l == v) {
        return;
    }
    last.retain(|(k, _)| k != key);
    last.push((key.into(), v.clone()));
    drop(last);
    state(key, v);
}

/// The fake SteamVR (vr::fake) with a fresh script, and everything a replay leaves behind cleared:
/// Plasma's crop, Kvm's placements of both bars and what shell_mouse was asked to send. vr.rs
/// can't do this itself, since cc-win-spike builds it without those modules. The guard keeps
/// other fake-SteamVR tests out until it drops (docs/vr-harness.md).
#[cfg(test)]
pub fn fake_vr() -> crate::vr::fake::Guard {
    let guard = crate::vr::fake::install(crate::vr::fake::Script::default());
    crate::plasmabar::reset_for_test();
    let mut k = crate::kvm::lock();
    (k.plasma, k.bar) = (None, None);
    drop(k);
    crate::windows::SHELL_SENT.lock().unwrap().clear();
    crate::windows::SHELL_OTHER.lock().unwrap().clear();
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replay_cant_start_kwin_capture_or_a_session_or_send_real_input() {
        let _vr = fake_vr();
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let kwin = std::panic::catch_unwind(crate::kwin::Kwin::start).is_err();
        let cap = std::panic::catch_unwind(crate::capture::Capture::start).is_err();
        let win = std::panic::catch_unwind(|| crate::windows::Windows::start(0)).is_err();
        std::panic::set_hook(quiet);
        assert!(kwin && cap && win, "KWin {kwin}, capture {cap}, the session {win}");
        crate::windows::shell_mouse(freerdp_sys::PTR_FLAGS_MOVE, Some((1.0, 2.0)));
        assert_eq!(*crate::windows::SHELL_SENT.lock().unwrap(), [(freerdp_sys::PTR_FLAGS_MOVE, Some((1.0, 2.0)))], "it went to the test's list, and only there");
    }

    #[test]
    fn flush_writes_what_was_buffered() {
        let _vr = fake_vr(); // the replays write through OUT too, so not while one runs
        let path = std::env::temp_dir().join(format!("cc-trace-flush-{}.jsonl", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        *crate::ffi::lock(&OUT) = Some(Out::new(file));
        state("plasma", json!({"crop": [1, 2, 3, 4]}));
        pose(3, None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "", "still in the buffer");
        flush();
        let text = std::fs::read_to_string(&path).unwrap();
        *crate::ffi::lock(&OUT) = None;
        let _ = std::fs::remove_file(&path);
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.starts_with(r#"{"plasma":{"crop":[1,2,3,4]}}"#), "{text}");
        flush(); // with no trace: nothing, and no panic
    }

    #[test]
    fn each_start_makes_a_new_private_trace_and_never_reuses_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("cc-trace-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (_a, first) = create(&dir).unwrap();
        std::fs::write(&first, "a restart's predecessor\n").unwrap();
        let (_b, second) = create(&dir).unwrap(); // the same second and pid: a new name, not the old file
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "a restart's predecessor\n", "never truncated");
        for p in [&first, &second] {
            assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600, "{}", p.display());
            assert!(p.file_name().unwrap().to_str().unwrap().starts_with("vr-trace-"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_is_logged_once_and_not_claimed() {
        let mut o = Out::new(std::fs::File::open("/dev/null").unwrap());
        assert!(!o.check(Err(std::io::Error::other("disk full"))) && o.failed);
        assert!(!o.check(Err(std::io::Error::other("again"))), "still failing, said once");
        assert!(o.check(Ok(())));
    }

    #[test]
    fn only_the_thread_holding_the_fake_gets_the_test_inputs() {
        let g = fake_vr();
        assert!(crate::vr::fake::here());
        assert!(!std::thread::spawn(crate::vr::fake::here).join().unwrap(), "another test's thread takes the real path");
        drop(g);
        assert!(!crate::vr::fake::here(), "and so does this one once the guard's gone");
    }

    #[test]
    fn a_trace_poisoned_by_a_panic_still_writes() {
        let _vr = fake_vr();
        let path = std::env::temp_dir().join(format!("cc-trace-poison-{}.jsonl", std::process::id()));
        *crate::ffi::lock(&OUT) = Some(Out::new(std::fs::File::create(&path).unwrap()));
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::thread::spawn(|| {
            let _held = OUT.lock();
            panic!("a tracing bug, with the lock held");
        })
        .join();
        std::panic::set_hook(quiet);
        assert!(OUT.is_poisoned());
        assert!(on(), "still on");
        state("plasma", json!({"crop": [1, 2, 3, 4]}));
        tick("plasma");
        flush();
        let text = std::fs::read_to_string(&path).unwrap();
        *crate::ffi::lock(&OUT) = None;
        OUT.clear_poison();
        let _ = std::fs::remove_file(&path);
        assert_eq!(text.lines().count(), 2, "{text}");
    }

    #[test]
    fn each_install_gets_its_own_script() {
        let ev = |t: u32| sys::VREvent_t { eventType: t, ..Default::default() };
        let poll = || {
            let mut e: sys::VREvent_t = Default::default();
            crate::call!(ov, PollNextOverlayEvent, 1, &mut e, size_of::<sys::VREvent_t>() as u32).then_some(e.eventType)
        };
        let first = crate::vr::fake::Script { events: [ev(300), ev(301)].into(), ..Default::default() };
        let guard = crate::vr::fake::install(first);
        assert_eq!(poll(), Some(300)); // one left behind on purpose
        drop(guard);
        let second = crate::vr::fake::Script { events: [ev(305)].into(), ..Default::default() };
        let _guard = crate::vr::fake::install(second);
        assert_eq!((poll(), poll()), (Some(305), None), "the second script, and nothing from the first");
    }

    #[test]
    fn a_replayed_pose_is_the_laser_it_recorded() {
        let _vr = fake_vr();
        let laser: Mat = [[0.0, 0.0, 1.0, 0.12], [0.0, 1.0, 0.0, 1.05], [-1.0, 0.0, 0.0, -0.31]]; // tip offset and all
        let (dev, m) = pose_from(&pose_line(5, Some(&laser))).unwrap();
        crate::vr::fake::SCRIPT.lock().unwrap().get_or_insert_with(Default::default).poses.push((dev, m.unwrap()));
        assert_eq!(crate::vr::laser_pose(5), Some(laser), "laser_pose adds nothing to it under the fake");
        assert_eq!(crate::vr::laser_pose(6), None, "a device the trace had no pose for");
    }

    #[test]
    fn a_replay_reaches_no_real_input() {
        let _vr = fake_vr();
        crate::windows::wheel(false, 1.0);
        crate::windows::leave();
        crate::windows::shell_outside();
        assert_eq!(*crate::windows::SHELL_OTHER.lock().unwrap(), ["wheel", "leave", "outside"], "each went to the test's list and returned");
    }

    #[test]
    fn an_event_and_a_pose_come_back_from_their_lines_as_they_went() {
        let mut e: sys::VREvent_t = Default::default();
        (e.eventType, e.trackedDeviceIndex, e.eventAgeSeconds) = (sys::EVREventType_VREvent_MouseButtonDown, 3, 0.25);
        e.data.mouse = sys::VREvent_Mouse_t { x: 0.31, y: 0.62, button: sys::EVRMouseButton_VRMouseButton_Right, cursorIndex: 1 };
        let line: Value = serde_json::from_str(&event_line("taskbar", &e).unwrap().to_string()).unwrap();
        assert!(line.get("data").is_none(), "no raw union: {line}");
        let back = event_from(&line).unwrap();
        assert_eq!((back.eventType, back.trackedDeviceIndex, back.eventAgeSeconds), (e.eventType, 3, 0.25));
        let (m, n) = (unsafe { back.data.mouse }, unsafe { e.data.mouse });
        assert_eq!((m.x, m.y, m.button, m.cursorIndex), (n.x, n.y, n.button, n.cursorIndex));
        // a scroll keeps its deltas
        let scroll = sys::VREvent_Scroll_t { xdelta: 0.0, ydelta: -1.0, unused: 7, viewportscale: 1.0, cursorIndex: 0 };
        let s = sys::VREvent_t { eventType: sys::EVREventType_VREvent_ScrollDiscrete, data: sys::VREvent_Data_t { scroll }, ..Default::default() };
        let back = event_from(&event_line("plasma", &s).unwrap()).unwrap();
        assert_eq!(unsafe { back.data.scroll.ydelta }, -1.0);
        // a key, or anything a replay doesn't need, never goes in
        for t in [sys::EVREventType_VREvent_KeyboardCharInput, sys::EVREventType_VREvent_ButtonPress, sys::EVREventType_VREvent_OverlayFocusChanged] {
            let k = sys::VREvent_t { eventType: t, ..Default::default() };
            assert!(event_line("osk", &k).is_none(), "type {t}");
        }
        let pose: Mat = [[1.0, 0.0, 0.0, 0.1], [0.0, 1.0, 0.0, 1.2], [0.0, 0.0, 1.0, -0.4]];
        let line: Value = serde_json::from_str(&pose_line(3, Some(&pose)).to_string()).unwrap();
        assert_eq!(pose_from(&line), Some((3, Some(pose))));
        assert_eq!(pose_from(&pose_line(4, None)), Some((4, None)), "untracked");
    }
}
