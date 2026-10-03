//! Gets window and output pixels from KWin's screencast PipeWire streams into SteamVR.
//!
//! One PipeWire thread holds every stream, and it's the only one that queues buffers. The main
//! thread (the only one calling OpenVR) takes each stream's newest frame from its Feed, imports
//! each DMA-BUF once, and gives buffers back through the PipeWire loop's channel.
//!
//! The format is BGRx, which is DRM's XRGB8888, so gpu.rs already imports it. We offer a LINEAR
//! DMA-BUF first, since that's the only modifier SteamVR takes from us, then shared memory.
use crate::{call, vr};
use openvr_sys as sys;
use pipewire as pw;
use pw::spa;
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

const LINEAR: i64 = 0;
const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

/// A *mut pw_buffer (only ever dereferenced on the PipeWire thread) plus its generation. pw_stream
/// keeps its buffers in a fixed array, so a renegotiation (a resize) hands back the same
/// addresses, and a give-back or frame from before it must not match the new ones.
pub type BufId = (usize, u32);

pub enum Pixels {
    Dmabuf { fd: OwnedFd, offset: u32, stride: u32, modifier: u64 }, // our own dup, since PipeWire closes its fd on remove_buffer
    Mem(Vec<u8>),                                               // RGBA, already copied out
}

pub struct Frame {
    pub buf: BufId,
    pub w: u32,
    pub h: u32,
    pub px: Pixels,
}

#[derive(Default)]
struct Shared {
    newest: Option<Frame>,
    gone: Vec<BufId>,       // removed buffers for the main thread to unref
    bufs: Vec<BufId>,       // live buffers, so a late give-back can't queue a freed one
}

/// One stream's hand-off between the PipeWire thread and the main thread.
#[derive(Default)]
pub struct Feed {
    s: Mutex<Shared>,
    pub frames: AtomicU32, // every frame that arrived, shown or not
    pub quiet: AtomicBool, // a new frame doesn't wake the main loop (for one that isn't taken right away: windows.rs)
    pub change: AtomicU32, // the most of the picture any one frame damaged since last taken, in thousandths (damaged)
}

impl Feed {
    pub fn take(&self) -> Option<Frame> {
        self.s.lock().unwrap().newest.take()
    }
}

enum Cmd {
    Open { key: u64, node: u32, name: String, feed: Arc<Feed>, fps: u32 },
    Close(u64),
    Back(u64, BufId),
    Active(u64, bool),
    Rate(u64, u32),
}

/// The PipeWire thread's mailbox.
pub struct Capture {
    tx: pw::channel::Sender<Cmd>,
}

impl Capture {
    pub fn start() -> Capture {
        let (tx, rx) = pw::channel::channel();
        std::thread::spawn(move || {
            if let Err(e) = pw_thread(rx) {
                eprintln!("capture: PipeWire: {e}");
            }
        });
        Capture { tx }
    }

    /// Streams a node (from KWin's `created`) into a new Feed at up to `fps` frames a second.
    /// KWin holds back the rest. `name` labels its log lines.
    pub fn open(&self, key: u64, node: u32, name: &str, fps: u32) -> Arc<Feed> {
        let feed = Arc::new(Feed::default());
        let _ = self.tx.send(Cmd::Open { key, node, name: name.into(), feed: feed.clone(), fps });
        feed
    }

    /// Paused (false), KWin renders nothing for it; resumed (true), it carries on. The buffers
    /// stay as they were, so there's no renegotiation and nothing gets imported again.
    pub fn set_active(&self, key: u64, on: bool) {
        let _ = self.tx.send(Cmd::Active(key, on));
    }

    /// Sets a new frame-rate cap, like the theater panel's 60. It's offered again, so the stream
/// renegotiates.
    pub fn set_rate(&self, key: u64, fps: u32) {
        let _ = self.tx.send(Cmd::Rate(key, fps));
    }

    pub fn close(&self, key: u64) {
        let _ = self.tx.send(Cmd::Close(key));
    }

    fn give_back(&self, key: u64, buf: BufId) {
        let _ = self.tx.send(Cmd::Back(key, buf));
    }
}

/// What the PipeWire thread keeps per stream. Fields drop in order, the stream then its listener,
/// so remove_buffer still reaches us while the stream goes away.
struct Live {
    stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<Data>,
    feed: Arc<Feed>,
    fps: Rc<Cell<u32>>,
}

struct Data {
    name: String,
    feed: Arc<Feed>,
    size: (u32, u32),
    modifier: Option<u64>,
    maps: HashMap<BufId, (*mut libc::c_void, usize, usize)>, // memfd mmaps: base, length and data offset
    told: bool,
    epoch: u32,  // the buffers' generation
    stale: bool, // buffers were removed, so the next add starts a new generation
    fps: Rc<Cell<u32>>, // the frame-rate cap as offered
}

fn pod(v: Object) -> Vec<u8> {
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(v)).unwrap().0.into_inner()
}

fn prop(key: u32, flags: PropertyFlags, value: Value) -> Property {
    Property { key, flags, value }
}

/// The format we offer: BGRx at any size. For a DMA-BUF it carries a modifier, offered with
/// DONT_FIXATE so KWin fixes it, the same as for OBS. For shared memory it has none. `fixed`
/// offers the modifier as one fixed value. The rate is anything up to `fps` frames a second
/// (VideoMaxFramerate), and KWin's screencast waits out the rest, merging their damage.
fn format(modifier: Option<i64>, fixed: bool, fps: u32) -> Vec<u8> {
    use spa::param::format::{FormatProperties as F, MediaSubtype, MediaType};
    let none = PropertyFlags::empty();
    let mut p = vec![
        prop(F::MediaType.as_raw(), none, Value::Id(Id(MediaType::Video.as_raw()))),
        prop(F::MediaSubtype.as_raw(), none, Value::Id(Id(MediaSubtype::Raw.as_raw()))),
        prop(F::VideoFormat.as_raw(), none, Value::Id(Id(spa::param::video::VideoFormat::BGRx.as_raw()))),
    ];
    if let Some(m) = modifier {
        p.push(if fixed {
            prop(F::VideoModifier.as_raw(), PropertyFlags::MANDATORY, Value::Long(m))
        } else {
            let c = Choice(ChoiceFlags::empty(), ChoiceEnum::Enum { default: m, alternatives: vec![m] });
            prop(F::VideoModifier.as_raw(), PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE, Value::Choice(ChoiceValue::Long(c)))
        });
    }
    let size = ChoiceEnum::Range {
        default: Rectangle { width: 1280, height: 800 },
        min: Rectangle { width: 1, height: 1 },
        max: Rectangle { width: 8192, height: 8192 },
    };
    p.push(prop(F::VideoSize.as_raw(), none, Value::Choice(ChoiceValue::Rectangle(Choice(ChoiceFlags::empty(), size)))));
    let rate = ChoiceEnum::Range { default: Fraction { num: 0, denom: 1 }, min: Fraction { num: 0, denom: 1 }, max: Fraction { num: 1000, denom: 1 } };
    p.push(prop(F::VideoFramerate.as_raw(), none, Value::Choice(ChoiceValue::Fraction(Choice(ChoiceFlags::empty(), rate)))));
    let cap = Fraction { num: fps.max(1), denom: 1 };
    let max = ChoiceEnum::Range { default: cap, min: Fraction { num: 1, denom: 1 }, max: cap };
    p.push(prop(F::VideoMaxFramerate.as_raw(), none, Value::Choice(ChoiceValue::Fraction(Choice(ChoiceFlags::empty(), max)))));
    pod(Object { type_: SpaTypes::ObjectParamFormat.as_raw(), id: spa::param::ParamType::EnumFormat.as_raw(), properties: p })
}

fn buffers() -> Vec<u8> {
    let types = (1 << spa::sys::SPA_DATA_DmaBuf) | (1 << spa::sys::SPA_DATA_MemFd);
    let count = ChoiceEnum::Range { default: 4, min: 2, max: 8 };
    let none = PropertyFlags::empty();
    pod(Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            prop(spa::sys::SPA_PARAM_BUFFERS_buffers, none, Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), count)))),
            prop(spa::sys::SPA_PARAM_BUFFERS_dataType, none, Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), ChoiceEnum::Flags { default: types, flags: vec![types] })))),
        ],
    })
}

/// Asks for each frame's damage (SPA_META_VideoDamage, up to 16 rects; KWin sends their bounds
/// when there are more). attention.rs's quiet uses it (D-045).
fn damage_meta() -> Vec<u8> {
    let one = size_of::<spa::sys::spa_meta_region>() as i32;
    let size = ChoiceEnum::Range { default: one * 16, min: one, max: one * 16 };
    let none = PropertyFlags::empty();
    pod(Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            prop(spa::sys::SPA_PARAM_META_type, none, Value::Id(Id(spa::sys::SPA_META_VideoDamage))),
            prop(spa::sys::SPA_PARAM_META_size, none, Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), size)))),
        ],
    })
}

/// How much of a w x h picture a buffer's frame damaged, in thousandths, from its damage rects.
/// When it has none it counts as all of it, and we log that once.
fn damaged(buf: &spa::sys::spa_buffer, (w, h): (u32, u32)) -> u32 {
    let metas = if buf.metas.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(buf.metas, buf.n_metas as usize) } };
    let Some(m) = metas.iter().find(|m| m.type_ == spa::sys::SPA_META_VideoDamage && !m.data.is_null()) else {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Relaxed) {
            eprintln!("capture: no damage metadata: a window's every frame counts as all changed (attention.rs)");
        }
        return 1000;
    };
    let n = m.size as usize / size_of::<spa::sys::spa_meta_region>();
    let rects = unsafe { std::slice::from_raw_parts(m.data as *const spa::sys::spa_meta_region, n) };
    let area: u64 = rects.iter().map(|r| r.region.size).take_while(|s| s.width > 0 && s.height > 0).map(|s| s.width as u64 * s.height as u64).sum();
    (area * 1000 / (w as u64 * h as u64).max(1)).min(1000) as u32
}

fn update(stream: &pw::stream::Stream, params: &[Vec<u8>]) {
    let mut p: Vec<&Pod> = params.iter().map(|b| Pod::from_bytes(b).unwrap()).collect();
    if let Err(e) = stream.update_params(&mut p) {
        eprintln!("capture: update_params: {e}");
    }
}

/// A negotiated format: the size, plus the modifier if it's a DMA-BUF one. None while the
/// modifier still needs fixing.
fn negotiated(param: &Pod) -> Option<((u32, u32), Option<u64>)> {
    let mut info = spa::param::video::VideoInfoRaw::new();
    info.parse(param).ok()?;
    use spa::param::video::VideoFlags;
    if info.flags().bits() & spa::sys::SPA_VIDEO_FLAG_MODIFIER_FIXATION_REQUIRED != 0 {
        return None;
    }
    let s = info.size();
    Some(((s.width, s.height), info.flags().contains(VideoFlags::MODIFIER).then(|| info.modifier())))
}

fn pw_thread(rx: pw::channel::Receiver<Cmd>) -> Result<(), pw::Error> {
    pw::init();
    let ml = pw::main_loop::MainLoopRc::new(None)?;
    let ctx = pw::context::ContextRc::new(&ml, None)?;
    let core = ctx.connect_rc(None)?;
    let live: Rc<RefCell<HashMap<u64, Live>>> = Rc::default();
    let _rx = rx.attach(ml.loop_(), {
        let live = live.clone();
        move |cmd| match cmd {
            Cmd::Open { key, node, name, feed, fps } => match open(&core, node, &name, feed, fps) {
                Ok(l) => drop(live.borrow_mut().insert(key, l)),
                Err(e) => eprintln!("{name}: can't stream node {node}: {e}"),
            },
            Cmd::Close(key) => drop(live.borrow_mut().remove(&key)),
            Cmd::Active(key, on) => {
                if let Some(Err(e)) = live.borrow().get(&key).map(|l| l.stream.set_active(on)) {
                    eprintln!("capture: set_active: {e}");
                }
            }
            Cmd::Rate(key, fps) => {
                if let Some(l) = live.borrow().get(&key).filter(|l| l.fps.get() != fps) {
                    l.fps.set(fps);
                    update(&l.stream, &[format(Some(LINEAR), false, fps), format(None, false, fps)]);
                }
            }
            Cmd::Back(key, buf) => {
                if let Some(l) = live.borrow().get(&key)
                    && l.feed.s.lock().unwrap().bufs.contains(&buf)
                {
                    unsafe { l.stream.queue_raw_buffer(buf.0 as *mut pw::sys::pw_buffer) };
                }
            }
        }
    });
    ml.run();
    Ok(())
}

fn open(core: &pw::core::CoreRc, node: u32, name: &str, feed: Arc<Feed>, fps: u32) -> Result<Live, pw::Error> {
    use pw::properties::properties;
    let stream = pw::stream::StreamRc::new(
        core.clone(),
        &format!("cc-{name}"),
        properties! { *pw::keys::MEDIA_TYPE => "Video", *pw::keys::MEDIA_CATEGORY => "Capture", *pw::keys::MEDIA_ROLE => "Screen" },
    )?;
    let fps = Rc::new(Cell::new(fps));
    let data = Data { name: name.into(), feed: feed.clone(), size: (0, 0), modifier: None, maps: HashMap::new(), told: false, epoch: 0, stale: false, fps: fps.clone() };
    let listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|s, d, id, param| {
            let Some(param) = param.filter(|_| id == spa::param::ParamType::Format.as_raw()) else { return };
            match negotiated(param) {
                // KWin usually fixes the modifier itself. If it asks us, LINEAR is the only one we have
                None => update(s, &[format(Some(LINEAR), true, d.fps.get()), format(None, false, d.fps.get())]),
                Some((size, modifier)) => {
                    (d.size, d.modifier, d.told) = (size, modifier, false);
                    update(s, &[buffers(), damage_meta()]);
                }
            }
        })
        .add_buffer(|_, d, b| {
            if std::mem::take(&mut d.stale) {
                d.epoch += 1;
            }
            let id = (b as usize, d.epoch);
            let buf = unsafe { &*(*b).buffer };
            if buf.n_datas < 1 {
                return;
            }
            let data = unsafe { &*buf.datas };
            let dmabuf = data.type_ == spa::sys::SPA_DATA_DmaBuf && buf.n_datas == 1;
            if data.type_ == spa::sys::SPA_DATA_MemFd {
                let len = (data.mapoffset + data.maxsize) as usize;
                let base = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, data.fd as i32, 0) };
                if base != libc::MAP_FAILED {
                    d.maps.insert(id, (base, len, data.mapoffset as usize));
                }
            }
            if !d.told {
                d.told = true;
                let (w, h) = d.size;
                match (dmabuf, d.modifier) {
                    (true, Some(m)) => eprintln!("{}: format {w}x{h} mod {m:#x} dmabuf", d.name),
                    (false, _) if d.maps.contains_key(&id) => eprintln!("{}: format {w}x{h} memfd", d.name),
                    _ => eprintln!("{}: format {w}x{h}: unusable buffers (type {}, {} planes)", d.name, data.type_, buf.n_datas),
                }
            }
            d.feed.s.lock().unwrap().bufs.push(id);
        })
        .remove_buffer(|_, d, b| {
            let id = (b as usize, d.epoch);
            d.stale = true;
            if let Some((base, len, _)) = d.maps.remove(&id) {
                unsafe { libc::munmap(base, len) };
            }
            let mut s = d.feed.s.lock().unwrap();
            s.bufs.retain(|&x| x != id);
            if s.newest.as_ref().is_some_and(|f| f.buf == id) {
                s.newest = None;
            }
            s.gone.push(id);
        })
        .process(|s, d| {
            // only the newest frame matters, so older ones go straight back
            let mut b: *mut pw::sys::pw_buffer = std::ptr::null_mut();
            loop {
                let n = unsafe { s.dequeue_raw_buffer() };
                if n.is_null() {
                    break;
                }
                d.feed.change.fetch_max(damaged(unsafe { &*(*n).buffer }, d.size), Relaxed);
                if !b.is_null() {
                    unsafe { s.queue_raw_buffer(b) };
                }
                b = n;
            }
            if b.is_null() {
                return;
            }
            let buf = unsafe { &*(*b).buffer };
            let data = unsafe { &*buf.datas };
            let chunk = unsafe { &*data.chunk };
            let (w, h) = d.size;
            if buf.n_datas < 1 || chunk.flags & spa::sys::SPA_CHUNK_FLAG_CORRUPTED as i32 != 0 || w == 0 {
                unsafe { s.queue_raw_buffer(b) };
                return;
            }
            let stride = if chunk.stride > 0 { chunk.stride as u32 } else { w * 4 };
            let id = (b as usize, d.epoch);
            let px = if let Some(&(base, _, off)) = d.maps.get(&id) {
                // ponytail: whole-frame copy, swizzled for SetOverlayRaw; VideoDamage rows into gpu.rs buffers in S1
                let src = unsafe { (base as *const u8).add(off + chunk.offset as usize) };
                let mut rgba = vec![0u8; (w * h * 4) as usize];
                for y in 0..h as usize {
                    let row = unsafe { std::slice::from_raw_parts(src.add(y * stride as usize), w as usize * 4) };
                    for (o, i) in rgba[y * w as usize * 4..][..w as usize * 4].chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                        o.copy_from_slice(&[i[2], i[1], i[0], 255]);
                    }
                }
                unsafe { s.queue_raw_buffer(b) };
                Pixels::Mem(rgba)
            } else if data.type_ == spa::sys::SPA_DATA_DmaBuf
                && let Ok(fd) = unsafe { BorrowedFd::borrow_raw(data.fd as i32) }.try_clone_to_owned()
            {
                Pixels::Dmabuf { fd, offset: chunk.offset, stride, modifier: d.modifier.unwrap_or(LINEAR as u64) }
            } else {
                unsafe { s.queue_raw_buffer(b) };
                return;
            };
            d.feed.frames.fetch_add(1, Relaxed);
            let old = d.feed.s.lock().unwrap().newest.replace(Frame { buf: id, w, h, px });
            if let Some(Frame { buf, px: Pixels::Dmabuf { .. }, .. }) = old {
                unsafe { s.queue_raw_buffer(buf.0 as *mut pw::sys::pw_buffer) }; // never shown
            }
            if !d.feed.quiet.load(Relaxed) {
                crate::vr::wake();
            }
        })
        .register()?;
    let mut params = [format(Some(LINEAR), false, fps.get()), format(None, false, fps.get())];
    let mut p: Vec<&Pod> = params.iter_mut().map(|b| Pod::from_bytes(b).unwrap()).collect();
    stream.connect(spa::utils::Direction::Input, Some(node), pw::stream::StreamFlags::AUTOCONNECT, &mut p)?;
    Ok(Live { stream, _listener: listener, feed, fps })
}

/// One overlay's view of a Feed, on the main thread. It keeps SteamVR handles per imported
/// buffer, and holds the shown buffer plus the one before it back from PipeWire, because SteamVR
/// may still be sampling it. Older ones go back.
#[derive(Default)]
pub struct Shown {
    handles: HashMap<BufId, sys::SharedTextureHandle_t>,
    held: [Option<BufId>; 2],
    pub size: (u32, u32),
    pub shown: u32, // frames put on the overlay
    imported: bool,
}

impl Shown {
    /// Puts the newest frame on `overlay`, if there is one and its rendering is done.
    /// Returns whether it went up.
    pub fn tick(&mut self, cap: &Capture, key: u64, feed: &Feed, overlay: vr::Handle, name: &str) -> bool {
        let gone = std::mem::take(&mut feed.s.lock().unwrap().gone);
        for b in gone {
            if let Some(h) = self.handles.remove(&b) {
                call!(ipc, UnrefResource, h);
            }
            self.held.iter_mut().filter(|x| **x == Some(b)).for_each(|x| *x = None);
        }
        let Some(f) = feed.take() else { return false };
        let ok = match &f.px {
            Pixels::Mem(rgba) => {
                // ponytail: SetOverlayRaw refuses past ~1920x1080; gpu.rs Buffers::upload(src, stride) in S1
                crate::back::shown(overlay, 0); // no handle, so its back is a plain sheet
                vr::timed(|| format!("{name}: SetOverlayRaw {}x{}", f.w, f.h), || call!(ov, SetOverlayRaw, overlay, rgba.as_ptr() as *mut _, f.w, f.h, 4)) == 0
            }
            Pixels::Dmabuf { fd, offset, stride, modifier } => {
                let fd = std::os::fd::AsRawFd::as_raw_fd(fd);
                let h = match self.handles.get(&f.buf) {
                    Some(&h) => h,
                    None => {
                        let h = vr::timed(|| format!("{name}: ImportDmabuf {}x{}", f.w, f.h), || import(fd, f.w, f.h, *offset, *stride, *modifier));
                        if !self.imported || h.is_none() {
                            eprintln!("{name}: import {}", if h.is_some() { "ok" } else { "fail" });
                            self.imported = true;
                        }
                        let Some(h) = h else {
                            cap.give_back(key, f.buf);
                            return false;
                        };
                        // removed in the meantime and its gone entry may already be handled, so don't cache
                        if !feed.s.lock().unwrap().bufs.contains(&f.buf) {
                            call!(ipc, UnrefResource, h);
                            return false;
                        }
                        self.handles.insert(f.buf, h);
                        h
                    }
                };
                // the producer's implicit fence: it's readable once rendering is done
                let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                if unsafe { libc::poll(&mut pfd, 1, 0) } <= 0 {
                    let mut s = feed.s.lock().unwrap();
                    match s.newest {
                        None if s.bufs.contains(&f.buf) => s.newest = Some(f),
                        None => {} // removed in the meantime, so PipeWire has it back
                        Some(_) => cap.give_back(key, f.buf), // a newer one came in the meantime
                    }
                    return false;
                }
                let mut h = h;
                let mut t = sys::Texture_t {
                    handle: &mut h as *mut _ as *mut std::ffi::c_void,
                    eType: sys::ETextureType_TextureType_SharedTextureHandle,
                    eColorSpace: sys::EColorSpace_ColorSpace_Gamma,
                };
                let ok = call!(ov, SetOverlayTexture, overlay, &mut t) == 0;
                if ok {
                    crate::back::shown(overlay, h); // its back too, if it has one
                }
                if self.held[1] != Some(f.buf) {
                    if let Some(old) = self.held[0].take() {
                        cap.give_back(key, old);
                    }
                    self.held = [self.held[1], Some(f.buf)];
                }
                ok
            }
        };
        self.size = (f.w, f.h);
        self.shown += ok as u32;
        ok
    }

    pub fn free(&mut self) {
        for (_, h) in self.handles.drain() {
            call!(ipc, UnrefResource, h);
        }
        self.held = [None; 2];
    }
}

fn import(fd: i32, w: u32, h: u32, offset: u32, stride: u32, modifier: u64) -> Option<sys::SharedTextureHandle_t> {
    let mut a: sys::DmabufAttributes_t = unsafe { std::mem::zeroed() };
    a.unWidth = w;
    a.unHeight = h;
    (a.unDepth, a.unMipLevels, a.unArrayLayers, a.unSampleCount) = (1, 1, 1, 1);
    a.unFormat = XRGB8888;
    a.ulModifier = modifier;
    a.unPlaneCount = 1;
    a.plane[0].unOffset = offset;
    a.plane[0].unStride = stride;
    a.plane[0].nFd = fd;
    let mut handle = 0;
    call!(ipc, ImportDmabuf, sys::EVRApplicationType_VRApplication_Overlay, &mut a, &mut handle).then_some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(modifier: Option<i64>, w: u32, h: u32) -> Vec<u8> {
        use spa::param::format::FormatProperties as F;
        let mut o = Object { type_: SpaTypes::ObjectParamFormat.as_raw(), id: spa::param::ParamType::Format.as_raw(), properties: vec![] };
        let none = PropertyFlags::empty();
        o.properties.push(prop(F::MediaType.as_raw(), none, Value::Id(Id(spa::param::format::MediaType::Video.as_raw()))));
        o.properties.push(prop(F::MediaSubtype.as_raw(), none, Value::Id(Id(spa::param::format::MediaSubtype::Raw.as_raw()))));
        o.properties.push(prop(F::VideoFormat.as_raw(), none, Value::Id(Id(spa::param::video::VideoFormat::BGRx.as_raw()))));
        if let Some(m) = modifier {
            o.properties.push(prop(F::VideoModifier.as_raw(), PropertyFlags::MANDATORY, Value::Long(m)));
        }
        o.properties.push(prop(F::VideoSize.as_raw(), none, Value::Rectangle(Rectangle { width: w, height: h })));
        pod(o)
    }

    #[test]
    fn negotiation_reads_size_and_modifier() {
        assert_eq!(negotiated(Pod::from_bytes(&fixed(Some(0), 1280, 800)).unwrap()), Some(((1280, 800), Some(0))));
        assert_eq!(negotiated(Pod::from_bytes(&fixed(None, 640, 480)).unwrap()), Some(((640, 480), None)));
        // our own offer still has the modifier to fix, so wait (or fix it) instead of starting
        assert_eq!(negotiated(Pod::from_bytes(&format(Some(LINEAR), false, 30)).unwrap()), None);
        assert!(Pod::from_bytes(&buffers()).is_some());
    }
}
