//! The camera scan's guidance in the headset, laid out the way I designed it. There are
//! two pieces:
//!   - a status strip in the lower third of the view, showing the step and whether you're holding
//!     still. A laser click on it means "skip this step", and the next refresh answers `ok skip`;
//!   - a transparent sheet with green outlines on the tags being read, placed in the room where
//!     the tags actually are.
//! Both are raw RGBA pictures read from a file, since SteamVR caches SetOverlayFromFile by path.
//! `hud hide` destroys them, and so does `tick` once one stops being refreshed (a scan that
//! died). The next `hud` makes them again.
use crate::{call, vr};
use openvr_sys as sys;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

const STALE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Status, // head-locked, lower third, clickable
    Mark,   // placed in standing space, lasers pass through
}

struct Hud {
    overlay: vr::Handle,
    shown: Option<Instant>, // when it was last refreshed, while it's up
    height: f32,            // picture height in pixels (mouse events count from the bottom)
    skip: Option<[f32; 4]>, // the strip's skip button: x0 y0 x1 y1, pixels from the top left
}

static HUDS: Mutex<[Option<Hud>; 2]> = Mutex::new([None, None]);
static SKIP: AtomicBool = AtomicBool::new(false); // set when the strip is clicked, cleared by the next answer

/// 1 m ahead and 0.6 m down in the headset's frame (31° under the line of sight), tilted 31° up
/// towards the eyes. That puts it below what the mirror camera sees (about 27° down), so it can't
/// hide tags from the scan, but a glance down still reads it. At 20° it could still block tags
/// when I tried it live.
fn status_pose() -> sys::HmdMatrix34_t {
    let (s, c) = 31f32.to_radians().sin_cos();
    sys::HmdMatrix34_t { m: [[1.0, 0.0, 0.0, 0.0], [0.0, c, s, -0.6], [0.0, -s, c, -1.0]] }
}

fn create(kind: Kind) -> Result<vr::Handle, String> {
    let h = match kind {
        Kind::Status => vr::create_overlay("controlcenter.hud", "Command Center scan")?,
        Kind::Mark => vr::create_overlay("controlcenter.hud.mark", "Command Center scan tags")?,
    };
    call!(ov, SetOverlayAlpha, h, 1.0); // the picture carries its own transparency
    if kind == Kind::Status {
        call!(ov, SetOverlayWidthInMeters, h, 0.5);
        call!(ov, SetOverlaySortOrder, h, 196); // over the mark, under the cursor (200)
        let mut t = status_pose();
        call!(ov, SetOverlayTransformTrackedDeviceRelative, h, sys::k_unTrackedDeviceIndex_Hmd as u32, &mut t);
        // clickable like the cards (grab.rs create), so a laser click can skip the scan's step
        call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
    } else {
        call!(ov, SetOverlaySortOrder, h, 195); // above panels (10+), the taskbar (190/191) and theater
    }
    Ok(h)
}

/// Shows or refreshes one piece from a raw RGBA picture file, w x h. The mark also gets its width
/// in metres and its place in standing space. The strip gets its skip button's rectangle, and
/// only a click inside it skips, because live a laser just resting across the strip skipped by
/// accident.
pub fn show(kind: Kind, path: &str, w: u32, h: u32, place: Option<(f32, crate::geometry::Mat)>, skip: Option<[f32; 4]>) -> String {
    if w == 0 || h == 0 || w > 1920 || h > 1920 || w as u64 * h as u64 > 1_500_000 {
        return format!("error hud {w}x{h}: 1 to 1920 px a side, 1.5 MP at most");
    }
    let px = match std::fs::read(path) {
        Ok(px) if px.len() == (w * h * 4) as usize => px,
        Ok(px) => return format!("error hud {path} is {} bytes, not {w}x{h} RGBA", px.len()),
        Err(e) => return format!("error hud {path}: {e}"),
    };
    let mut huds = HUDS.lock().unwrap();
    let slot = &mut huds[kind as usize];
    if slot.is_none() {
        match create(kind) {
            Ok(o) => *slot = Some(Hud { overlay: o, shown: None, height: 0.0, skip: None }),
            Err(e) => return format!("error hud {e}"),
        }
    }
    let hd = slot.as_mut().unwrap();
    if !crate::grab::set_raw(hd.overlay, &px, w as usize, h as usize) {
        return "error hud SteamVR refused the picture".into();
    }
    if let Some((width, m)) = place {
        call!(ov, SetOverlayWidthInMeters, hd.overlay, width);
        vr::place(hd.overlay, &m);
    }
    if kind == Kind::Status && hd.height != h as f32 {
        let mut scale = sys::HmdVector2_t { v: [w as f32, h as f32] }; // so mouse events arrive in the picture's pixels
        call!(ov, SetOverlayMouseScale, hd.overlay, &mut scale);
        hd.height = h as f32;
    }
    hd.skip = skip;
    if hd.shown.is_none() {
        call!(ov, ShowOverlay, hd.overlay);
    }
    hd.shown = Some(Instant::now());
    if SKIP.swap(false, Relaxed) { "ok skip".into() } else { "ok".into() }
}

/// Hides and destroys both, so they don't hold two of SteamVR's 128 overlays between scans. The
/// next `hud` makes them again.
pub fn hide() -> String {
    for slot in HUDS.lock().unwrap().iter_mut() {
        if let Some(h) = slot.take() {
            crate::gpu::forget(h.overlay);
            call!(ov, DestroyOverlay, h.overlay);
        }
    }
    "ok".into()
}

fn on_button(r: &[f32; 4], x: f32, y: f32) -> bool {
    (r[0]..=r[2]).contains(&x) && (r[1]..=r[3]).contains(&y)
}

/// Picks up clicks on the strip's skip button and hides anything nobody refreshed for a while.
/// Call it once a frame.
pub fn tick() {
    let mut huds = HUDS.lock().unwrap();
    if let Some(h) = &huds[Kind::Status as usize] {
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        while call!(ov, PollNextOverlayEvent, h.overlay, &mut e, size_of::<vr::VREvent_t>() as u32) {
            let m = unsafe { e.data.mouse }; // pixels from the bottom left
            if e.eventType == sys::EVREventType_VREvent_MouseButtonDown && h.skip.is_some_and(|r| on_button(&r, m.x, h.height - m.y)) {
                SKIP.store(true, Relaxed);
            }
        }
    }
    for slot in huds.iter_mut() {
        if slot.as_ref().is_some_and(|h| h.shown.is_none_or(|t| t.elapsed() > STALE)) {
            let o = slot.take().unwrap().overlay;
            crate::gpu::forget(o);
            call!(ov, DestroyOverlay, o); // the scan died, so free it rather than just hide it
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_button_counts() {
        let r = [700.0, 54.0, 890.0, 94.0];
        assert!(super::on_button(&r, 800.0, 70.0));
        assert!(!super::on_button(&r, 100.0, 70.0) && !super::on_button(&r, 800.0, 20.0));
    }
}
