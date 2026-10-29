//! A trace of what cc-panels polls from SteamVR, so a bug only the headset shows can be replayed
//! as a test (docs/vr-harness.md). With ~/.cache/control-center/vr-trace there when cc-panels
//! starts, it writes ~/.cache/control-center/vr-trace.jsonl, one JSON object a line:
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
    since: std::time::Instant, // what the ticks' "t" counts from
}

static OUT: Mutex<Option<Out>> = Mutex::new(None);

fn out() -> &'static Mutex<Option<Out>> {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let dir = format!("{}/.cache/control-center", crate::config::home_dir());
        if std::path::Path::new(&dir).join("vr-trace").exists() {
            match std::fs::File::create(format!("{dir}/vr-trace.jsonl")) {
                Ok(file) => {
                    eprintln!("vr-trace: writing {dir}/vr-trace.jsonl");
                    *OUT.lock().unwrap() = Some(Out { file: std::io::BufWriter::with_capacity(1 << 20, file), written: 0, since: std::time::Instant::now() });
                }
                Err(e) => eprintln!("vr-trace: {dir}/vr-trace.jsonl: {e}"),
            }
        }
    });
    &OUT
}

/// Whether a trace is being written. Cheap, so callers can skip building a line.
pub fn on() -> bool {
    out().lock().unwrap().as_ref().is_some_and(|o| o.written < MAX)
}

fn write(v: Value) {
    let mut o = out().lock().unwrap();
    let Some(o) = o.as_mut().filter(|o| o.written < MAX) else { return };
    let line = format!("{v}\n");
    if o.file.write_all(line.as_bytes()).is_ok() {
        o.written += line.len() as u64;
        if o.written >= MAX {
            let _ = o.file.flush();
            eprintln!("vr-trace: it's {} MB, so that's all it writes", MAX >> 20);
        }
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

/// A pass starting: the last one's lines go to the disk, then this one's line, with the seconds
/// since the trace began.
pub fn tick(who: &str) {
    let t = {
        let mut o = out().lock().unwrap();
        let Some(o) = o.as_mut().filter(|o| o.written < MAX) else { return };
        let _ = o.file.flush();
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
pub fn fake_vr() -> std::sync::MutexGuard<'static, ()> {
    let guard = crate::vr::fake::install(crate::vr::fake::Script::default());
    crate::plasmabar::reset_for_test();
    let mut k = crate::kvm::lock();
    (k.plasma, k.bar) = (None, None);
    drop(k);
    crate::windows::SHELL_SENT.lock().unwrap().clear();
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
