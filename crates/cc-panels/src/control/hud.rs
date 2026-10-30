//! The camera scan's guidance in the headset, laid out the way I designed it. There are
//! two pieces:
//!   - a status strip in the lower third of the view, showing the step and whether you're holding
//!     still. A laser click on it means "skip this step", and the next refresh answers `ok skip`;
//!   - a transparent sheet with green outlines on the tags being read, placed in the room where
//!     the tags actually are.
//! Both are raw RGBA pictures read from a file, since SteamVR caches SetOverlayFromFile by path.
//! `hud hide` destroys them, and so does `tick` once one stops being refreshed (a scan that
//! died). The next `hud` makes them again.
use crate::geometry::from_top;
use crate::overlay::{At, Live, Place, apply};
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
        Kind::Status => crate::surface::create(crate::surface::Kind::HudStrip, None, "controlcenter.hud", "Command Center scan")?,
        Kind::Mark => crate::surface::create(crate::surface::Kind::HudMark, None, "controlcenter.hud.mark", "Command Center scan tags")?,
    };
    call!(ov, SetOverlayAlpha, h.0, 1.0); // the picture carries its own transparency
    if kind == Kind::Status {
        apply(&mut Live, h, &Place::new(At::Device(crate::ids::Dev::Head, status_pose().m), 0.5));
        call!(ov, SetOverlaySortOrder, h.0, 196); // over the mark, under the cursor (200)
        // clickable like the cards (grab.rs create), so a laser click can skip the scan's step
        call!(ov, SetOverlayInputMethod, h.0, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, h.0, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
    } else {
        call!(ov, SetOverlaySortOrder, h.0, 195); // above panels (10+), the taskbar (190/191) and theater
    }
    Ok(h)
}

/// Reads a HUD picture of exactly `len` bytes. Anyone on this machine can send `hud <path>` to the
/// control socket, and this runs on the main loop, so the path has to be a plain file inside our
/// own cache folder (where cc-home's scans write them), of exactly that size, and no more than that
/// is ever read: /dev/zero, a FIFO or a huge file elsewhere can't stall or fill the Desktop
/// (RESEXHAUST-001).
fn sheet(cache: &std::path::Path, path: &std::path::Path, len: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let real = path.canonicalize().map_err(|e| e.to_string())?;
    if !cache.canonicalize().is_ok_and(|c| real.starts_with(c)) {
        return Err(format!("not in {}", cache.display()));
    }
    // non-blocking, so opening a FIFO that got there anyway can't stall the main loop
    let f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW).open(&real).map_err(|e| e.to_string())?;
    let m = f.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.len() != len {
        return Err(format!("{} bytes, not a {len}-byte RGBA file", m.len()));
    }
    let mut px = Vec::with_capacity(len as usize);
    f.take(len + 1).read_to_end(&mut px).map_err(|e| e.to_string())?;
    if px.len() as u64 != len {
        return Err(format!("{} bytes, not {len}", px.len()));
    }
    Ok(px)
}

/// Shows or refreshes one piece from a raw RGBA picture file, w x h. The mark also gets its width
/// in metres and its place in standing space. The strip gets its skip button's rectangle, and
/// only a click inside it skips, because live a laser just resting across the strip skipped by
/// accident.
pub fn show(kind: Kind, path: &str, w: u32, h: u32, place: Option<(f32, crate::geometry::Mat)>, skip: Option<[f32; 4]>) -> String {
    if w == 0 || h == 0 || w > 1920 || h > 1920 || w as u64 * h as u64 > 1_500_000 {
        return format!("error hud {w}x{h}: 1 to 1920 px a side, 1.5 MP at most");
    }
    let cache = std::path::PathBuf::from(format!("{}/.cache/control-center", crate::config::home_dir()));
    let px = match sheet(&cache, std::path::Path::new(path), (w * h * 4) as u64) {
        Ok(px) => px,
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
        apply(&mut Live, hd.overlay, &Place::new(At::Room(m), width as f64));
    }
    if kind == Kind::Status && hd.height != h as f32 {
        let mut scale = sys::HmdVector2_t { v: [w as f32, h as f32] }; // so mouse events arrive in the picture's pixels
        call!(ov, SetOverlayMouseScale, hd.overlay.0, &mut scale);
        hd.height = h as f32;
    }
    hd.skip = skip;
    if hd.shown.is_none() {
        call!(ov, ShowOverlay, hd.overlay.0);
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
            crate::surface::destroy(h.overlay);
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
        let mut e: vr::VREvent_t = Default::default();
        while call!(ov, PollNextOverlayEvent, h.overlay.0, &mut e, size_of::<vr::VREvent_t>() as u32) {
            let m = unsafe { e.data.mouse }; // pixels from the bottom left
            if e.eventType == sys::EVREventType_VREvent_MouseButtonDown && h.skip.is_some_and(|r| on_button(&r, m.x, from_top(h.height, m.y))) {
                SKIP.store(true, Relaxed);
            }
        }
    }
    for slot in huds.iter_mut() {
        if slot.as_ref().is_some_and(|h| h.shown.is_none_or(|t| t.elapsed() > STALE)) {
            let o = slot.take().unwrap().overlay;
            crate::gpu::forget(o);
            crate::surface::destroy(o); // the scan died, so free it rather than just hide it
        }
    }
}

#[cfg(test)]
mod tests {
    /// Only an exactly sized plain file inside the cache folder is read: not a file outside it, not
    /// one reached by a symlink out of it, not a FIFO or a device, and not a wrong size.
    #[test]
    fn hud_reads_only_its_own_pictures() {
        use std::os::unix::fs::symlink;
        let d = std::env::temp_dir().join(format!("cc-panels-hudread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let cache = d.join("cache");
        std::fs::create_dir_all(cache.join("scan")).unwrap();
        std::fs::write(cache.join("scan/hud.rgba"), [7u8; 16]).unwrap();
        std::fs::write(d.join("outside.rgba"), [7u8; 16]).unwrap();
        symlink(d.join("outside.rgba"), cache.join("scan/link.rgba")).unwrap();
        symlink("/dev/zero", cache.join("scan/zero.rgba")).unwrap();
        let fifo = std::ffi::CString::new(cache.join("scan/fifo.rgba").to_string_lossy().into_owned()).unwrap();
        unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) };
        let read = |p: &str, n| super::sheet(&cache, &cache.join(p), n);
        assert_eq!(read("scan/hud.rgba", 16), Ok(vec![7u8; 16]));
        assert!(read("scan/hud.rgba", 12).is_err(), "the wrong size");
        assert!(super::sheet(&cache, &d.join("outside.rgba"), 16).is_err(), "outside the cache");
        assert!(read("scan/link.rgba", 16).is_err(), "a symlink out of it");
        assert!(read("scan/zero.rgba", 16).is_err(), "/dev/zero");
        assert!(read("scan/fifo.rgba", 16).is_err(), "a FIFO, without blocking");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_the_button_counts() {
        let r = [700.0, 54.0, 890.0, 94.0];
        assert!(super::on_button(&r, 800.0, 70.0));
        assert!(!super::on_button(&r, 100.0, 70.0) && !super::on_button(&r, 800.0, 20.0));
    }
}
