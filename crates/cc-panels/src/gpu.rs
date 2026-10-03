//! GPU buffers for the panels. Each one is a linear GBM buffer in the stream's byte order
//! (B, G, R, then an ignored byte, which is DRM's XRGB8888), imported into SteamVR once.
//! We go this way because ordinary SteamVR texture uploads on the Frame refuse anything over
//! ~1920x1080, and imported buffers have no limit.
use crate::{Panel, call, vr};
use openvr_sys as sys;
use std::os::raw::{c_int, c_void};

/// Every thread shares one GBM device (one gallium screen and context), and it isn't
/// thread-safe, so each call holds this lock. Live, two RDP threads mapping at once crashed in
/// libgallium. The copy itself runs between map and unmap without the lock.
static GBM: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gbm<T>(f: impl FnOnce() -> T) -> T {
    let _g = GBM.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

#[repr(C)]
pub struct GbmDevice([u8; 0]);
#[repr(C)]
pub struct GbmBo([u8; 0]);

#[link(name = "gbm")]
unsafe extern "C" {
    fn gbm_create_device(fd: c_int) -> *mut GbmDevice;
    fn gbm_bo_create(dev: *mut GbmDevice, w: u32, h: u32, format: u32, flags: u32) -> *mut GbmBo;
    fn gbm_bo_destroy(bo: *mut GbmBo);
    fn gbm_bo_get_modifier(bo: *mut GbmBo) -> u64;
    fn gbm_bo_get_offset(bo: *mut GbmBo, plane: c_int) -> u32;
    fn gbm_bo_get_stride(bo: *mut GbmBo) -> u32;
    fn gbm_bo_get_fd(bo: *mut GbmBo) -> c_int;
    fn gbm_bo_map(bo: *mut GbmBo, x: u32, y: u32, w: u32, h: u32, flags: u32, stride: *mut u32, data: *mut *mut c_void) -> *mut c_void;
    fn gbm_bo_unmap(bo: *mut GbmBo, data: *mut c_void);
}

const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const ABGR8888: u32 = u32::from_le_bytes(*b"AB24"); // bytes R, G, B, A: SetOverlayRaw's RGBA as is, so alpha survives
const USE_RENDERING: u32 = 1 << 2;
const USE_LINEAR: u32 = 1 << 4;
const TRANSFER_WRITE: u32 = 1 << 1;

/// Copies a w×h block of 4-byte pixels from src into dst, either as is or turned a quarter
/// turn. Turned, the block's (x, y) lands at dst's row x, column h-1-y, so dst is h wide and
/// w tall.
/// dst is mapped GPU memory (uncached, write-combined), so it has to be written in long runs.
/// That's why the turn happens in 32-pixel tiles into a cached buffer first, and dst gets whole
/// rows in order. Live, writing pixel by pixel straight into it made each desk-portrait upload
/// take 80-300 ms.
pub unsafe fn copy_rect(src: *const u8, src_stride: usize, dst: *mut u8, dst_stride: usize, w: usize, h: usize, turned: bool) {
    if !turned {
        for y in 0..h {
            unsafe { std::ptr::copy_nonoverlapping(src.add(y * src_stride), dst.add(y * dst_stride), w * 4) };
        }
        return;
    }
    const T: usize = 32;
    // dst's w rows of h pixels, cached. It's kept between calls because a 1440x2560 picture is
    // 14.7 MB and zeroing a fresh one each frame cost as much as the turn. Every pixel gets
    // written before it's read, so stale contents never show.
    thread_local!(static OUT: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) });
    OUT.with_borrow_mut(|out| {
        if out.len() < w * h {
            out.resize(w * h, 0);
        }
        let o = out.as_mut_ptr();
        for y0 in (0..h).step_by(T) {
            for x0 in (0..w).step_by(T) {
                for y in y0..(y0 + T).min(h) {
                    let row = unsafe { src.add(y * src_stride) } as *const u32;
                    let col = h - 1 - y;
                    for x in x0..(x0 + T).min(w) {
                        unsafe { *o.add(x * h + col) = row.add(x).read_unaligned() };
                    }
                }
            }
        }
        for x in 0..w {
            unsafe { std::ptr::copy_nonoverlapping(o.add(x * h) as *const u8, dst.add(x * dst_stride), h * 4) };
        }
    });
}

/// The one device. main.rs makes it, and the UI overlays' buffers use it too (show_raw).
static DEV: std::sync::atomic::AtomicPtr<GbmDevice> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

pub fn device() -> Option<*mut GbmDevice> {
    let fd = unsafe { libc::open(c"/dev/dri/renderD128".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    let dev = if fd >= 0 { unsafe { gbm_create_device(fd) } } else { std::ptr::null_mut() };
    DEV.store(dev, std::sync::atomic::Ordering::Relaxed);
    (!dev.is_null()).then_some(dev)
}

/// A panel's two buffers, shared by its RDP thread and the main loop through Panel::gpu. The
/// RDP thread writes the spare one right after a decode (write). The main loop shows it
/// (present) and makes the buffers, since OpenVR calls, ImportDmabuf included, stay on the
/// main thread.
pub struct Shared {
    bo: [*mut GbmBo; 2],
    have: (i32, i32, bool), // the buffers' size, and whether they're turned (a vertically curved panel)
    want: (i32, i32, bool), // what the picture needs, as the RDP thread last saw it
    shown: usize,           // the buffer SteamVR shows; the other is the spare
    ready: bool,            // the spare holds a newer picture
    held: bool,             // the spare was on screen until this tick's flip; SteamVR may still read it until its next frame, so we don't write it before the next tick
    last: Option<std::time::Instant>, // the last write; a peripheral panel only gets a few a second
}

// The buffers are only touched under Panel::gpu.
unsafe impl Send for Shared {}

impl Default for Shared {
    fn default() -> Self {
        Shared { bo: [std::ptr::null_mut(); 2], have: (0, 0, false), want: (0, 0, false), shown: 0, ready: false, held: false, last: None }
    }
}

impl Shared {
    fn spare(&self) -> usize {
        self.shown ^ 1
    }

    /// Main loop: the buffer to show now, if the spare has a newer picture.
    fn flip(&mut self) -> Option<usize> {
        if !std::mem::take(&mut self.ready) {
            return None;
        }
        (self.shown, self.held) = (self.shown ^ 1, true);
        Some(self.shown)
    }

    /// Main loop, after SteamVR refused the flip. It's still showing the old buffer, so we
    /// leave that one alone and try the new one again next tick.
    fn unflip(&mut self) {
        (self.shown, self.ready, self.held) = (self.shown ^ 1, true, false);
    }

    /// Main loop, a tick after a flip: the old buffer can be written again. True if it was held.
    fn next_tick(&mut self) -> bool {
        !self.ready && std::mem::take(&mut self.held)
    }

    /// RDP thread: the buffer it can write now, if any. It has to be made at the picture's
    /// size and not just taken off the screen.
    fn writable(&self) -> Option<usize> {
        (self.have == self.want && !self.held).then(|| self.spare())
    }
}

/// The main loop's side of a panel's buffers: SteamVR's handles for them.
#[derive(Default)]
pub struct Buffers {
    handle: [sys::SharedTextureHandle_t; 2],
    pub uploads: u32, // since the last status line
    tried: Option<std::time::Instant>, // the last failed make, so we retry once a second instead of every tick
}

impl Buffers {
    fn make(&mut self, dev: *mut GbmDevice, name: &str, s: &mut Shared, format: u32) -> bool {
        self.free(s);
        let (w, h, _) = s.want;
        let _g = GBM.lock().unwrap_or_else(|e| e.into_inner());
        for k in 0..2 {
            s.bo[k] = unsafe { gbm_bo_create(dev, w as u32, h as u32, format, USE_LINEAR | USE_RENDERING) };
            if s.bo[k].is_null() {
                eprintln!("{name}: no {w}x{h} GPU buffer");
                return false;
            }
            let mut a: sys::DmabufAttributes_t = unsafe { std::mem::zeroed() };
            unsafe {
                a.unWidth = w as u32;
                a.unHeight = h as u32;
                (a.unDepth, a.unMipLevels, a.unArrayLayers, a.unSampleCount) = (1, 1, 1, 1);
                a.unFormat = format; // XRGB8888 for panels, since they're always opaque
                a.ulModifier = gbm_bo_get_modifier(s.bo[k]);
                a.unPlaneCount = 1;
                a.plane[0].unOffset = gbm_bo_get_offset(s.bo[k], 0);
                a.plane[0].unStride = gbm_bo_get_stride(s.bo[k]);
                a.plane[0].nFd = gbm_bo_get_fd(s.bo[k]);
            }
            let ok = vr::timed(|| format!("{name}: ImportDmabuf {w}x{h}"), || call!(ipc, ImportDmabuf, sys::EVRApplicationType_VRApplication_Overlay, &mut a, &mut self.handle[k]));
            unsafe { libc::close(a.plane[0].nFd) };
            if !ok {
                eprintln!("{name}: SteamVR won't import a {w}x{h} buffer");
                return false;
            }
        }
        s.have = s.want;
        true
    }

    pub fn free(&mut self, s: &mut Shared) {
        for k in 0..2 {
            if self.handle[k] != 0 {
                call!(ipc, UnrefResource, self.handle[k]);
                self.handle[k] = 0;
            }
            if !s.bo[k].is_null() {
                gbm(|| unsafe { gbm_bo_destroy(s.bo[k]) });
                s.bo[k] = std::ptr::null_mut();
            }
        }
        (s.have, s.ready, s.held) = ((0, 0, false), false, false);
    }

    /// Main loop: shows the spare buffer once the RDP thread has written it (write), and makes
    /// new buffers when the picture needs them. It never waits on the RDP thread.
    pub fn present(&mut self, dev: *mut GbmDevice, p: &Panel) {
        let Ok(mut s) = p.gpu.try_lock() else { return }; // mid-write, so next tick
        if s.next_tick() && p.dirty.load(std::sync::atomic::Ordering::Acquire) {
            crate::rdp::poke(p); // its RDP thread was waiting for this (wait)
        }
        if s.want != s.have && s.want.0 > 0 {
            if self.tried.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
                return;
            }
            if !self.make(dev, &p.v.name, &mut s, XRGB8888) {
                self.tried = Some(std::time::Instant::now());
                return;
            }
            self.tried = None;
            let (w, h, turned) = s.want;
            let (w, h) = if turned { (h, w) } else { (w, h) };
            for st in &p.stale {
                st.grow(0, 0, w, h); // new buffers, so all of it is stale
            }
            p.dirty.store(true, std::sync::atomic::Ordering::Release); // the RDP thread writes it when it's due
            crate::rdp::poke(p); // it was waiting for them (wait)
            return;
        }
        let Some(k) = s.flip() else { return };
        // Still under the lock. SteamVR may read the old buffer until its next frame, so the RDP
        // thread leaves it alone until the next tick (held).
        let mut t = sys::Texture_t {
            handle: &mut self.handle[k] as *mut _ as *mut c_void,
            eType: sys::ETextureType_TextureType_SharedTextureHandle,
            eColorSpace: sys::EColorSpace_ColorSpace_Gamma,
        };
        match call!(ov, SetOverlayTexture, p.overlay, &mut t) {
            0 => {
                self.uploads += 1;
                crate::back::shown(p.overlay, self.handle[k]); // its back too, if it has one
            }
            e => {
                s.unflip();
                eprintln!("{}: upload failed: {}", p.v.name, vr::error_name(e));
            }
        }
    }
}

/// A UI overlay's two buffers (Command Center's windows, cards, taskbar, HUD). They're shown
/// like a panel's but written on the main loop. pending is a picture that came in while the
/// spare was held.
#[derive(Default)]
struct Raw {
    s: Shared,
    b: Buffers,
    pending: Option<(Vec<u8>, usize, usize)>,
    old: Vec<(Shared, Buffers)>, // the pair a resize replaced; SteamVR may still be showing one, so they're freed next tick (raw_tick)
}

static RAW: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<vr::Handle, Raw>>> = std::sync::LazyLock::new(Default::default);

/// Main loop: puts h's RGBA picture (w x ht) into its spare buffer and shows it with
/// SetOverlayTexture, the same way panels do, since panels never flicker. I suspect, but haven't
/// confirmed, that SetOverlayRaw blanks the overlay for a frame per call (openvr#772), which
/// would be a blink at every hover change.
/// Returns false when there are no buffers (or SteamVR refused them; we retry a second later),
/// and the caller falls back to SetOverlayRaw.
/// ponytail: the whole picture is copied (1-5 MB, write-combined); patch rects if it shows in slow ticks.
pub fn show_raw(h: vr::Handle, px: &[u8], w: usize, ht: usize) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let dev = DEV.load(Relaxed);
    if dev.is_null() || w == 0 || ht == 0 || px.len() < w * ht * 4 {
        return false;
    }
    let mut m = RAW.lock().unwrap_or_else(|e| e.into_inner());
    let r = m.entry(h).or_default();
    r.s.want = (w as i32, ht as i32, false);
    if r.s.have != r.s.want {
        if r.b.tried.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
            return false; // failed just now, so SetOverlayRaw until the retry
        }
        if r.s.have.0 > 0 {
            // A resize. The pair on screen stays alive until SteamVR has taken the new one (raw_tick).
            let (s, b) = (std::mem::take(&mut r.s), std::mem::take(&mut r.b));
            r.s.want = s.want;
            r.old.push((s, b));
        }
        if !r.b.make(dev, &format!("overlay {h}"), &mut r.s, ABGR8888) {
            r.b.free(&mut r.s);
            r.b.tried = Some(std::time::Instant::now());
            return false;
        }
        r.b.tried = None;
    }
    let Some(k) = r.s.writable() else {
        r.pending = Some((px.to_vec(), w, ht)); // the spare was on screen this tick, so it's written next tick (raw_tick)
        crate::vr::wake(); // and we want that tick now, not an idle tick later
        return true;
    };
    r.pending = None;
    let (mut stride, mut data) = (0u32, std::ptr::null_mut());
    let dst = gbm(|| unsafe { gbm_bo_map(r.s.bo[k], 0, 0, w as u32, ht as u32, TRANSFER_WRITE, &mut stride, &mut data) });
    if dst.is_null() {
        return false;
    }
    unsafe { copy_rect(px.as_ptr(), w * 4, dst as *mut u8, stride as usize, w, ht, false) };
    gbm(|| unsafe { gbm_bo_unmap(r.s.bo[k], data) });
    let mut t = sys::Texture_t {
        handle: &mut r.b.handle[k] as *mut _ as *mut c_void,
        eType: sys::ETextureType_TextureType_SharedTextureHandle,
        eColorSpace: sys::EColorSpace_ColorSpace_Gamma,
    };
    match call!(ov, SetOverlayTexture, h, &mut t) {
        0 => {
            (r.s.shown, r.s.held) = (k, true); // SteamVR may read the old one until its next frame
            true
        }
        e => {
            eprintln!("overlay {h}: upload failed: {}", vr::error_name(e));
            false
        }
    }
}

/// Main loop, once a tick: the buffers shown last tick can be written again, so the pictures
/// that were waiting on them go up.
pub fn raw_tick() {
    let pending: Vec<_> = {
        let mut m = RAW.lock().unwrap_or_else(|e| e.into_inner());
        m.iter_mut()
            .filter_map(|(&h, r)| {
                r.s.next_tick();
                for (mut s, mut b) in r.old.drain(..) {
                    b.free(&mut s);
                }
                r.pending.take().map(|p| (h, p))
            })
            .collect()
    };
    for (h, (px, w, ht)) in pending {
        crate::grab::set_raw(h, &px, w, ht);
    }
}

/// h is being destroyed, so its buffers go too.
pub fn forget(h: vr::Handle) {
    let r = RAW.lock().unwrap_or_else(|e| e.into_inner()).remove(&h);
    if let Some(mut r) = r {
        r.b.free(&mut r.s);
        for (mut s, mut b) in r.old {
            b.free(&mut s);
        }
    }
}

/// RDP thread, right after a decode, holding p.lock so the GDI can't change or go away. Copies
/// what changed in the picture into the spare buffer, and the main loop shows it (present).
/// It skips this while the panel is hidden, away (minimized, theater) or paused, and only does
/// a few a second while it's peripheral (attention.rs). Whatever changed in the meantime goes
/// up when it's due (wait).
pub fn write(p: &Panel, gdi: &freerdp_sys::rdpGdi) {
    use std::sync::atomic::Ordering::*;
    if !p.dirty.load(Acquire) || crate::HIDDEN.load(Relaxed) || p.away() {
        return; // stays dirty, and its stale regions keep growing
    }
    let now = std::time::Instant::now();
    let mut s = p.gpu.lock().unwrap(); // the main loop only holds it to show or make buffers
    if !crate::attention::due(p.level(), crate::attention::PERIPHERAL, s.last, now) {
        return;
    }
    let (w, h, turned) = (gdi.width, gdi.height, p.vert());
    let want = if turned { (h, w, true) } else { (w, h, false) };
    if std::mem::replace(&mut s.want, want) != want {
        crate::vr::wake(); // the main loop makes them, then pokes this thread (present)
    }
    let Some(k) = s.writable() else { return }; // being made, or just taken off the screen, so it stays dirty
    p.dirty.store(false, Release);
    let (x0, y0, x1, y1) = p.stale[k].take(); // only what changed since this buffer was last written
    let (x0, y0, x1, y1) = (x0.max(0), y0.max(0), x1.min(w), y1.min(h));
    if x1 > x0 && y1 > y0 {
        // Turned, the picture's (x, y) is stored at column h-1-y of row x.
        let (mx, my, mw, mh) = if turned { (h - y1, x0, y1 - y0, x1 - x0) } else { (x0, y0, x1 - x0, y1 - y0) };
        let (mut stride, mut data) = (0u32, std::ptr::null_mut());
        let tm = std::time::Instant::now();
        let dst = gbm(|| unsafe { gbm_bo_map(s.bo[k], mx as u32, my as u32, mw as u32, mh as u32, TRANSFER_WRITE, &mut stride, &mut data) });
        let map_ms = tm.elapsed().as_millis();
        if dst.is_null() {
            p.stale[k].grow(x0, y0, x1, y1); // still to do
            p.dirty.store(true, Release);
            return;
        }
        let src = unsafe { gdi.primary_buffer.add(y0 as usize * gdi.stride as usize + x0 as usize * 4) };
        let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let t = std::time::Instant::now();
        unsafe { copy_rect(src, gdi.stride as usize, dst as *mut u8, stride as usize, cw, ch, turned) };
        let ms = t.elapsed().as_millis();
        let tu = std::time::Instant::now();
        gbm(|| unsafe { gbm_bo_unmap(s.bo[k], data) });
        let unmap_ms = tu.elapsed().as_millis();
        if ms + map_ms + unmap_ms >= 10 {
            eprintln!("slow {}: map {map_ms} copy {ms} unmap {unmap_ms} ms ({cw}x{ch}{}, RDP thread)", p.v.name, if turned { " turned" } else { "" });
        }
    }
    (s.ready, s.last) = (true, Some(now));
    drop(s);
    crate::vr::wake(); // the main loop shows it next tick
}

/// How long the RDP thread can wait on the network:
///   - a picture that wasn't due yet when it was decoded: until it's due (at least 10 ms apart);
///   - buffers being made or just shown: until the main loop pokes it (present);
///   - hidden or away: 100 ms, so it's back soon after;
///   - otherwise once a second, as before.
pub fn wait(p: &Panel) -> std::time::Duration {
    use std::time::Duration;
    const IDLE: Duration = Duration::from_secs(1);
    if !p.dirty.load(std::sync::atomic::Ordering::Acquire) {
        return IDLE;
    }
    if crate::HIDDEN.load(std::sync::atomic::Ordering::Relaxed) || p.away() {
        return Duration::from_millis(100);
    }
    let s = p.gpu.lock().unwrap();
    if s.writable().is_none() {
        return IDLE;
    }
    crate::attention::until_due(p.level(), crate::attention::PERIPHERAL, s.last, std::time::Instant::now()).map_or(IDLE, |d| d.clamp(Duration::from_millis(10), IDLE))
}

#[cfg(test)]
mod tests {
    /// Dev: cargo test --release -- --ignored turn_speed (turns a full portrait picture)
    #[test]
    #[ignore]
    fn turn_speed() {
        let (w, h) = (1440usize, 2560usize);
        let src = vec![7u8; w * h * 4];
        let mut dst = vec![0u8; w * h * 4];
        for _ in 0..3 {
            let t = std::time::Instant::now();
            unsafe { super::copy_rect(src.as_ptr(), w * 4, dst.as_mut_ptr(), h * 4, w, h, true) };
            eprintln!("turned {w}x{h}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
    }

    #[test]
    fn written_spare_is_shown_once_then_the_other_is_spare() {
        let mut s = super::Shared::default();
        assert_eq!(s.flip(), None, "nothing written");
        let k = s.spare(); // the RDP thread writes it...
        s.ready = true;
        assert_eq!(s.flip(), Some(k), "...the main loop shows it");
        assert_eq!(s.flip(), None, "once");
        assert_eq!(s.spare(), k ^ 1, "the next write goes to the other buffer");
        s.ready = true; // written twice before the main loop looked: same spare, shown once
        let k = s.spare();
        assert_eq!((s.flip(), s.flip()), (Some(k), None));
    }

    #[test]
    fn the_buffer_just_taken_off_the_screen_waits_a_tick() {
        let mut s = super::Shared::default();
        s.ready = true;
        let k = s.flip().unwrap();
        assert_eq!(s.writable(), None, "SteamVR may still read the old one until its next frame");
        assert!(s.next_tick(), "the next tick lets it go...");
        assert_eq!(s.writable(), Some(k ^ 1), "...to be written");
        assert!(!s.next_tick(), "once");
        s.ready = true; // SteamVR refuses the flip: the old buffer stays on and the new one is tried again
        let k = s.flip().unwrap();
        s.unflip();
        assert_eq!((s.writable(), s.flip()), (Some(k), Some(k)), "the one SteamVR refused, never the one on screen");
        s.want = (2, 2, false); // the picture changed size, so nothing is written until the new buffers are made
        s.next_tick();
        assert_eq!(s.writable(), None);
    }

    #[test]
    fn copy_rect_turns_a_quarter_turn() {
        // A 3x2 block of pixels numbered 0..5 (row by row), in a source 4 pixels wide.
        let (w, h) = (3usize, 2usize);
        let mut src = vec![0u8; 4 * 4 * h];
        for y in 0..h {
            for x in 0..w {
                src[y * 16 + x * 4] = (y * w + x) as u8;
            }
        }
        let mut flat = vec![0u8; w * 4 * h];
        unsafe { super::copy_rect(src.as_ptr(), 16, flat.as_mut_ptr(), w * 4, w, h, false) };
        assert_eq!(flat.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5]);
        // Turned: 2 wide, 3 tall; its row x is the block's column x, bottom row first.
        let mut turned = vec![0u8; h * 4 * w];
        unsafe { super::copy_rect(src.as_ptr(), 16, turned.as_mut_ptr(), h * 4, w, h, true) };
        assert_eq!(turned.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [3, 0, 4, 1, 5, 2]);
    }
}
