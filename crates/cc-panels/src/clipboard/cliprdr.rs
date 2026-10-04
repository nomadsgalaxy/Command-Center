//! The machines' side: FreeRDP's cliprdr channel in each RDP session (rdp.rs attaches it). Each
//! channel is a client to that monitor's krdpserver. We announce with ClientFormatList, krdp
//! announces with ServerFormatList, and either side asks for data by format id.
//!
//! Files (MS-RDPECLIP 3.1.5.4.5): a copy offers FileGroupDescriptorW, a list of names and
//! sizes, and the contents go in ranges over FILECONTENTS requests by list index. We serve the
//! Frame's files that way, pass a machine's requests through to another machine's channel when
//! the files came from there, and fetch them into a staging folder when a Frame app pastes them.
use super::formats::{self, Entry};
use super::{Chan, Data, Fmt, HUB, Want, run};
use crate::{Panel, panel};
use freerdp_sys::*;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CString, c_void};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering::*;
use std::sync::atomic::AtomicU32;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

const RANGE: u32 = 4 << 20; // what we ask a machine for at once (cc-aux's krdp caps it here too)
const MOST: u32 = 8 << 20; // the most we hand back to one request

struct Info {
    panel: usize,
    machine: String,
    files: bool,               // the server said CB_STREAM_FILECLIP_ENABLED
    ids: Vec<(Fmt, u32)>,      // its last format list's ids
    asked: VecDeque<Fmt>,      // our format data requests, oldest first, so we know what an answer is
    relay: Option<usize>,      // its last file paste's source channel, or None for the Frame's
}

static CHANS: Mutex<Vec<Info>> = Mutex::new(Vec::new());
/// The Frame's files as last described to a machine, for its FILECONTENTS requests.
static LOCAL: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// Our FILECONTENTS requests waiting on an answer, by stream id.
static PENDING: Mutex<Option<HashMap<u32, mpsc::Sender<Option<Vec<u8>>>>>> = Mutex::new(None);
static STREAM: AtomicU32 = AtomicU32::new(1);
/// A machine's files fetched for the Frame: the copy (copy) and the top-level paths.
static STAGED: Mutex<Option<(u64, Vec<PathBuf>)>> = Mutex::new(None);

pub fn chans() -> Vec<Chan> {
    CHANS.lock().unwrap().iter().map(|i| Chan { panel: i.panel, machine: i.machine.clone(), files: i.files }).collect()
}

fn with<R>(panel: usize, f: impl FnOnce(&mut Info) -> R) -> Option<R> {
    CHANS.lock().unwrap().iter_mut().find(|i| i.panel == panel).map(f)
}

fn index(c: *mut CliprdrClientContext) -> usize {
    unsafe { (*c).custom as usize - 1 }
}

fn ctx(p: usize) -> Option<*mut CliprdrClientContext> {
    let c = crate::panels().get(p)?.cliprdr.load(Acquire);
    (!c.is_null()).then_some(c)
}

/// The channel came up (rdp.rs, on ChannelConnected).
pub fn attach(p: &Panel, c: *mut CliprdrClientContext) {
    unsafe {
        (*c).custom = (p.index + 1) as *mut c_void;
        (*c).ServerCapabilities = Some(on_server_capabilities);
        (*c).MonitorReady = Some(on_monitor_ready);
        (*c).ServerFormatList = Some(on_server_format_list);
        (*c).ServerFormatListResponse = Some(on_server_format_list_response);
        (*c).ServerFormatDataRequest = Some(on_server_format_data_request);
        (*c).ServerFormatDataResponse = Some(on_server_format_data_response);
        (*c).ServerFileContentsRequest = Some(on_server_file_contents_request);
        (*c).ServerFileContentsResponse = Some(on_server_file_contents_response);
    }
    let machine = crate::config::machine_of(&p.v).to_string();
    let mut all = CHANS.lock().unwrap();
    all.retain(|i| i.panel != p.index);
    all.push(Info { panel: p.index, machine, files: false, ids: Vec::new(), asked: VecDeque::new(), relay: None });
    drop(all);
    p.cliprdr.store(c, Release);
    eprintln!("clipboard: {} channel up", p.v.name);
}

/// The channel went away (rdp.rs, on ChannelDisconnected).
pub fn detach(p: &Panel, c: *mut CliprdrClientContext) {
    p.cliprdr.store(std::ptr::null_mut(), Release);
    unsafe { (*c).custom = std::ptr::null_mut() };
    let before = chans();
    CHANS.lock().unwrap().retain(|i| i.panel != p.index);
    let acts = HUB.lock().unwrap().channel_down(p.index, &before);
    run(acts);
}

unsafe extern "C" fn on_server_capabilities(c: *mut CliprdrClientContext, caps: *const CLIPRDR_CAPABILITIES) -> UINT {
    let caps = unsafe { &*caps };
    if caps.cCapabilitiesSets > 0 && !caps.capabilitySets.is_null() {
        let set = caps.capabilitySets; // FreeRDP hands over the general set, the only one there is
        if unsafe { (*set).capabilitySetType } as u32 == CB_CAPSTYPE_GENERAL {
            let flags = unsafe { (*(set as *const CLIPRDR_GENERAL_CAPABILITY_SET)).generalFlags };
            with(index(c), |i| i.files = flags & CB_STREAM_FILECLIP_ENABLED != 0);
        }
    }
    0
}

unsafe extern "C" fn on_monitor_ready(c: *mut CliprdrClientContext, _: *const CLIPRDR_MONITOR_READY) -> UINT {
    let mut general = CLIPRDR_GENERAL_CAPABILITY_SET {
        capabilitySetType: CB_CAPSTYPE_GENERAL as u16,
        capabilitySetLength: CB_CAPSTYPE_GENERAL_LEN as u16,
        version: CB_CAPS_VERSION_2,
        // files by stream, without local paths; we never lock (we keep each copy's file list ourselves)
        generalFlags: CB_USE_LONG_FORMAT_NAMES | CB_STREAM_FILECLIP_ENABLED | CB_FILECLIP_NO_FILE_PATHS | CB_CAN_LOCK_CLIPDATA,
    };
    let mut caps = CLIPRDR_CAPABILITIES::default();
    caps.common.msgType = CliprdrMsgType_CB_CLIP_CAPS as u16;
    caps.cCapabilitiesSets = 1;
    caps.capabilitySets = &mut general as *mut _ as *mut CLIPRDR_CAPABILITY_SET;
    let rc = unsafe { ((*c).ClientCapabilities.unwrap())(c, &caps) };
    if rc != 0 {
        return rc;
    }
    let me = chans().into_iter().find(|ch| ch.panel == index(c));
    if let Some(me) = me {
        let acts = HUB.lock().unwrap().channel_up(&me, Instant::now()); // a machine that (re)connects gets what's been copied
        run(acts);
    }
    0
}

unsafe extern "C" fn on_server_format_list_response(_: *mut CliprdrClientContext, _: *const CLIPRDR_FORMAT_LIST_RESPONSE) -> UINT {
    0
}

unsafe extern "C" fn on_server_format_list(c: *mut CliprdrClientContext, list: *const CLIPRDR_FORMAT_LIST) -> UINT {
    let mut ok = CLIPRDR_FORMAT_LIST_RESPONSE::default();
    ok.common.msgType = CliprdrMsgType_CB_FORMAT_LIST_RESPONSE as u16;
    ok.common.msgFlags = CB_RESPONSE_OK as u16;
    let rc = unsafe { ((*c).ClientFormatListResponse.unwrap())(c, &ok) };
    if rc != 0 {
        return rc;
    }
    let list = unsafe { &*list };
    let raw = if list.formats.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(list.formats, list.numFormats as usize) } };
    let named: Vec<(u32, Option<String>)> = raw.iter().map(|f| {
        let name = (!f.formatName.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(f.formatName) }.to_string_lossy().into_owned());
        (f.formatId, name)
    }).collect();
    let picked = formats::pick(&named);
    let p = index(c);
    let Some(machine) = with(p, |i| {
        i.ids = picked.clone();
        i.machine.clone()
    }) else { return 0 };
    let fmts: Vec<Fmt> = picked.iter().map(|(f, _)| *f).collect();
    eprintln!("clipboard: {} announces {fmts:?}", panel(p).v.name);
    let all = chans();
    let acts = HUB.lock().unwrap().remote_announce(p, &machine, fmts, &all, Instant::now());
    run(acts);
    0
}

unsafe extern "C" fn on_server_format_data_response(c: *mut CliprdrClientContext, r: *const CLIPRDR_FORMAT_DATA_RESPONSE) -> UINT {
    let r = unsafe { &*r };
    let p = index(c);
    let Some(Some(fmt)) = with(p, |i| i.asked.pop_front()) else { return 0 };
    let ok = r.common.msgFlags as u32 & CB_RESPONSE_OK != 0 && !r.requestedFormatData.is_null();
    let raw = if ok { unsafe { std::slice::from_raw_parts(r.requestedFormatData, r.common.dataLen as usize) } } else { &[][..] };
    let bytes = ok.then(|| from_wire(fmt, raw)).flatten();
    let all = chans();
    let acts = HUB.lock().unwrap().remote_data(p, bytes, &all, Instant::now());
    run(acts);
    0
}

unsafe extern "C" fn on_server_format_data_request(c: *mut CliprdrClientContext, want: *const CLIPRDR_FORMAT_DATA_REQUEST) -> UINT {
    let id = unsafe { (*want).requestedFormatId };
    let p = index(c);
    let fmt = [Fmt::Text, Fmt::Html, Fmt::Image, Fmt::Files].into_iter().find(|f| formats::to_rdp(*f).0 == id);
    let Some(fmt) = fmt else {
        respond(p, None);
        return 0;
    };
    let acts = HUB.lock().unwrap().want(Want::Rdp(p), fmt); // answered now, or once it's fetched
    run(acts);
    0
}

/// A machine's form of a format to ours.
fn from_wire(fmt: Fmt, b: &[u8]) -> Option<Vec<u8>> {
    match fmt {
        Fmt::Text => Some(formats::from_utf16(b).into_bytes()),
        Fmt::Html => formats::from_cf_html(b).map(String::into_bytes),
        Fmt::Image => formats::dib_to_bmp(b),
        Fmt::Files => Some(b.to_vec()), // the descriptor, as it came
    }
}

// ------------------------------------------------------------------ the hub's actions

/// Announces formats on a channel.
pub fn offer(p: usize, fmts: &[Fmt]) {
    let Some(c) = ctx(p) else { return };
    let names: Vec<Option<CString>> = fmts.iter().map(|f| formats::to_rdp(*f).1.map(|n| CString::new(n).unwrap())).collect();
    let mut list: Vec<CLIPRDR_FORMAT> = fmts.iter().zip(&names).map(|(f, n)| CLIPRDR_FORMAT {
        formatId: formats::to_rdp(*f).0,
        formatName: n.as_ref().map_or(std::ptr::null_mut(), |n| n.as_ptr() as *mut _),
    }).collect();
    let mut l = CLIPRDR_FORMAT_LIST::default();
    l.common.msgType = CliprdrMsgType_CB_FORMAT_LIST as u16;
    l.numFormats = list.len() as u32;
    l.formats = list.as_mut_ptr();
    eprintln!("clipboard: offering {fmts:?} to {}", panel(p).v.name);
    unsafe { ((*c).ClientFormatList.unwrap())(c, &l) };
}

/// Asks a channel for a format; the answer comes to on_server_format_data_response.
pub fn fetch(p: usize, fmt: Fmt) {
    let id = with(p, |i| i.ids.iter().find(|(f, _)| *f == fmt).map(|(_, id)| *id)).flatten();
    let (Some(c), Some(id)) = (ctx(p), id) else {
        let all = chans();
        let acts = HUB.lock().unwrap().remote_data(p, None, &all, Instant::now());
        return run(acts);
    };
    with(p, |i| i.asked.push_back(fmt));
    let mut want = CLIPRDR_FORMAT_DATA_REQUEST::default();
    want.common.msgType = CliprdrMsgType_CB_FORMAT_DATA_REQUEST as u16;
    want.requestedFormatId = id;
    unsafe { ((*c).ClientFormatDataRequest.unwrap())(c, &want) };
}

/// Answers a channel's paste. Files from a machine go as its descriptor, and the channel's
/// FILECONTENTS requests get passed on to `src`; the Frame's get described here and served from disk.
pub fn give(p: usize, fmt: Fmt, data: Data, src: Option<usize>) {
    let wire = data.and_then(|d| match fmt {
        Fmt::Text => Some(formats::to_utf16(&String::from_utf8_lossy(&d))),
        Fmt::Html => Some(formats::to_cf_html(&String::from_utf8_lossy(&d))),
        Fmt::Image => formats::bmp_to_dib(&d),
        Fmt::Files if src.is_some() => {
            with(p, |i| i.relay = src);
            Some(d.to_vec())
        }
        Fmt::Files => {
            // ponytail: walks the folders on the RDP thread; a huge tree stalls that monitor while it does
            let entries = formats::walk(&formats::from_uri_list(&d));
            let b = formats::to_descriptor(&entries);
            *LOCAL.lock().unwrap() = entries;
            with(p, |i| i.relay = None);
            Some(b)
        }
    });
    respond(p, wire.as_deref());
}

fn respond(p: usize, data: Option<&[u8]>) {
    let Some(c) = ctx(p) else { return };
    let mut r = CLIPRDR_FORMAT_DATA_RESPONSE::default();
    r.common.msgType = CliprdrMsgType_CB_FORMAT_DATA_RESPONSE as u16;
    match data {
        Some(d) => {
            r.common.msgFlags = CB_RESPONSE_OK as u16;
            r.common.dataLen = d.len() as u32;
            r.requestedFormatData = d.as_ptr();
        }
        None => r.common.msgFlags = CB_RESPONSE_FAIL as u16,
    }
    unsafe { ((*c).ClientFormatDataResponse.unwrap())(c, &r) };
}

// ------------------------------------------------------------------ file contents

unsafe extern "C" fn on_server_file_contents_request(c: *mut CliprdrClientContext, r: *const CLIPRDR_FILE_CONTENTS_REQUEST) -> UINT {
    let r = unsafe { *r };
    let p = index(c);
    let pos = (r.nPositionHigh as u64) << 32 | r.nPositionLow as u64;
    let relay = with(p, |i| i.relay).flatten();
    // off the RDP thread: a relay waits on another machine, and a range is a disk read
    std::thread::spawn(move || {
        let data = match relay {
            Some(src) => contents(src, r.listIndex, r.dwFlags, pos, r.cbRequested.min(MOST)),
            None => local(r.listIndex as usize, r.dwFlags, pos, r.cbRequested.min(MOST)),
        };
        answer(p, r.streamId, data.as_deref());
    });
    0
}

unsafe extern "C" fn on_server_file_contents_response(_: *mut CliprdrClientContext, r: *const CLIPRDR_FILE_CONTENTS_RESPONSE) -> UINT {
    let r = unsafe { &*r };
    let ok = r.common.msgFlags as u32 & CB_RESPONSE_OK != 0 && !r.requestedData.is_null();
    let data = ok.then(|| unsafe { std::slice::from_raw_parts(r.requestedData, r.cbRequested as usize) }.to_vec());
    if let Some(tx) = PENDING.lock().unwrap().as_mut().and_then(|m| m.remove(&r.streamId)) {
        let _ = tx.send(data);
    }
    0
}

/// One of the Frame's files described in LOCAL: its size (FILECONTENTS_SIZE) or a range of it.
fn local(i: usize, flags: u32, pos: u64, len: u32) -> Option<Vec<u8>> {
    let e = LOCAL.lock().unwrap().get(i)?.clone();
    if flags & FILECONTENTS_SIZE != 0 {
        return Some(e.size.to_le_bytes().to_vec());
    }
    let mut f = std::fs::File::open(e.local?).ok()?;
    f.seek(SeekFrom::Start(pos)).ok()?;
    let mut buf = Vec::with_capacity(len as usize);
    f.take(len as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn answer(p: usize, stream: u32, data: Option<&[u8]>) {
    let Some(c) = ctx(p) else { return };
    let mut r = CLIPRDR_FILE_CONTENTS_RESPONSE::default();
    r.common.msgType = CliprdrMsgType_CB_FILECONTENTS_RESPONSE as u16;
    r.streamId = stream;
    match data {
        Some(d) => {
            r.common.msgFlags = CB_RESPONSE_OK as u16;
            r.cbRequested = d.len() as u32;
            r.common.dataLen = 4 + d.len() as u32;
            r.requestedData = d.as_ptr();
        }
        None => r.common.msgFlags = CB_RESPONSE_FAIL as u16,
    }
    unsafe { ((*c).ClientFileContentsResponse.unwrap())(c, &r) };
}

/// Asks a machine's channel for a file's size or a range of it, and waits up to 30 s.
fn contents(p: usize, list: u32, flags: u32, pos: u64, len: u32) -> Option<Vec<u8>> {
    let c = ctx(p)?;
    let stream = STREAM.fetch_add(1, Relaxed);
    let (tx, rx) = mpsc::channel();
    PENDING.lock().unwrap().get_or_insert_with(HashMap::new).insert(stream, tx);
    let mut r = CLIPRDR_FILE_CONTENTS_REQUEST::default();
    r.common.msgType = CliprdrMsgType_CB_FILECONTENTS_REQUEST as u16;
    r.streamId = stream;
    r.listIndex = list;
    r.dwFlags = flags;
    r.nPositionLow = pos as u32;
    r.nPositionHigh = (pos >> 32) as u32;
    r.cbRequested = if flags & FILECONTENTS_SIZE != 0 { 8 } else { len };
    let sent = unsafe { ((*c).ClientFileContentsRequest.unwrap())(c, &r) } == 0;
    let got = if sent { rx.recv_timeout(Duration::from_secs(30)).ok().flatten() } else { None };
    PENDING.lock().unwrap().as_mut().map(|m| m.remove(&stream));
    got
}

/// A machine's copied files, fetched into ~/.cache/control-center/clipboard/<copy> for a Frame
/// paste. Returns the top-level paths, for the uri-list. A second paste of the same copy reuses them.
pub fn download(src: usize, descriptor: &[u8], copy: u64) -> Option<Vec<PathBuf>> {
    if let Some((g, tops)) = &*STAGED.lock().unwrap() {
        if *g == copy {
            return Some(tops.clone());
        }
    }
    let entries = formats::from_descriptor(descriptor)?;
    let max_mb = crate::config::settings()["clipboard_max_mb"].as_u64().unwrap_or(2048);
    let total: u64 = entries.iter().filter(|e| !e.dir).map(|e| e.size).sum();
    if total > max_mb << 20 {
        eprintln!("clipboard: {} MB of files is over clipboard_max_mb ({max_mb})", total >> 20);
        return None;
    }
    let base = PathBuf::from(format!("{}/.cache/control-center/clipboard", crate::config::home_dir()));
    // ponytail: only the newest copy's files are kept; the one before goes once this starts
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join(copy.to_string());
    std::fs::create_dir_all(&root).ok()?;
    let started = Instant::now();
    for (i, e) in entries.iter().enumerate() {
        let path = formats::staged(&root, &e.name)?;
        if e.dir {
            std::fs::create_dir_all(&path).ok()?;
            continue;
        }
        std::fs::create_dir_all(path.parent()?).ok()?;
        let mut f = std::fs::File::create(&path).ok()?;
        let mut at = 0u64;
        while at < e.size {
            let want = (e.size - at).min(RANGE as u64) as u32;
            let chunk = contents(src, i as u32, FILECONTENTS_RANGE, at, want).filter(|c| !c.is_empty())?;
            f.write_all(&chunk).ok()?;
            at += chunk.len() as u64;
        }
    }
    eprintln!("clipboard: fetched {} file(s), {} MB, in {:.1} s", entries.len(), total >> 20, started.elapsed().as_secs_f32());
    let tops = formats::tops(&root, &entries);
    *STAGED.lock().unwrap() = Some((copy, tops.clone()));
    Some(tops)
}
