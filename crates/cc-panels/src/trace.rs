//! A trace of what cc-panels polls from SteamVR, so a bug only the headset shows can be replayed
//! as a test (docs/vr-harness.md). With ~/.cache/control-center/vr-trace there when cc-panels
//! starts, it writes ~/.cache/control-center/vr-trace.jsonl, one JSON object a line:
//!   {"tick":"plasma"}                          an events() pass starting
//!   {"ev":"plasma","type":..,"dev":..,"age":..,"data":[6 u64],"x":..,"y":..}   one polled event
//!   {"pose":3,"m":[[..],[..],[..]]}            a laser's pose as laser_pose gave it (m null: none)
//!   {"plasma":{...}}                           Plasma's overlay placed: crop, panel, bounds, world, ...
//! The data is the event's union as it came, so nothing's lost; x and y are only for reading.
use crate::geometry::Mat;
use openvr_sys as sys;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::Mutex;

/// shortcut: stops writing at 64 MB, a few minutes of lasers at 90 Hz; enough for one bug.
const MAX: u64 = 64 << 20;

struct Out {
    file: std::fs::File,
    written: u64,
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
                    *OUT.lock().unwrap() = Some(Out { file, written: 0 });
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
    }
}

/// The line for one polled event.
pub fn event_line(who: &str, e: &sys::VREvent_t) -> Value {
    let r = unsafe { e.data.reserved };
    let m = unsafe { e.data.mouse };
    json!({"ev": who, "type": e.eventType, "dev": e.trackedDeviceIndex, "age": e.eventAgeSeconds,
           "data": [r.reserved0, r.reserved1, r.reserved2, r.reserved3, r.reserved4, r.reserved5], "x": m.x, "y": m.y})
}

/// An event back from its line.
#[cfg(test)]
pub fn event_from(v: &Value) -> Option<sys::VREvent_t> {
    let d: Vec<u64> = v["data"].as_array()?.iter().filter_map(Value::as_u64).collect();
    let [r0, r1, r2, r3, r4, r5] = d[..] else { return None };
    let reserved = sys::VREvent_Reserved_t { reserved0: r0, reserved1: r1, reserved2: r2, reserved3: r3, reserved4: r4, reserved5: r5 };
    Some(sys::VREvent_t {
        eventType: v["type"].as_u64()? as u32,
        trackedDeviceIndex: v["dev"].as_u64()? as u32,
        eventAgeSeconds: v["age"].as_f64().unwrap_or(0.0) as f32,
        data: sys::VREvent_Data_t { reserved },
    })
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

pub fn tick(who: &str) {
    if on() {
        write(json!({ "tick": who }));
    }
}

pub fn event(who: &str, e: &sys::VREvent_t) {
    if on() {
        write(event_line(who, e));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_and_a_pose_come_back_from_their_lines_as_they_went() {
        let mut e: sys::VREvent_t = Default::default();
        (e.eventType, e.trackedDeviceIndex, e.eventAgeSeconds) = (sys::EVREventType_VREvent_MouseButtonDown, 3, 0.25);
        e.data.mouse = sys::VREvent_Mouse_t { x: 0.31, y: 0.62, button: sys::EVRMouseButton_VRMouseButton_Right, cursorIndex: 1 };
        let line: Value = serde_json::from_str(&event_line("taskbar", &e).to_string()).unwrap();
        let back = event_from(&line).unwrap();
        assert_eq!((back.eventType, back.trackedDeviceIndex, back.eventAgeSeconds), (e.eventType, 3, 0.25));
        let (m, n) = (unsafe { back.data.mouse }, unsafe { e.data.mouse });
        assert_eq!((m.x, m.y, m.button, m.cursorIndex), (n.x, n.y, n.button, n.cursorIndex), "the whole union, bit for bit");
        let pose: Mat = [[1.0, 0.0, 0.0, 0.1], [0.0, 1.0, 0.0, 1.2], [0.0, 0.0, 1.0, -0.4]];
        let line: Value = serde_json::from_str(&pose_line(3, Some(&pose)).to_string()).unwrap();
        assert_eq!(pose_from(&line), Some((3, Some(pose))));
        assert_eq!(pose_from(&pose_line(4, None)), Some((4, None)), "untracked");
    }
}
