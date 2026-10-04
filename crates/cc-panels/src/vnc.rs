//! One VNC session per panel (D-049, docs/vnc.md): libvncclient here, and any RFB server on the
//! machine (macOS Screen Sharing, wayvnc, TightVNC, x11vnc, TigerVNC). It sits beside rdp.rs and
//! uses the same panel, GPU buffers, wake event and reconnect loop, so the rest of cc-panels sees
//! one remote either way. viewers.conf says which with `proto=vnc`.
//!
//! RFB is pull-based: the server only sends a picture after we ask for one. libvncclient asks
//! again after every update by itself, so our build patches that out (ccPacedRequests, cc-home
//! install.rs). We ask only once the last picture is on its way to the GPU and the panel is in
//! view, so a paused or hidden panel costs the host nothing, which krdp can't do.
//!
//! Security (review B2, B3): without `tls=no` the only way in is VeNCrypt with an X509
//! certificate, the password goes out only inside that TLS session, and the certificate has to
//! match the pin (pin= on the line, or the paired machine's). `tls=no` also allows VNC's own
//! password check and Apple's (ARD), which macOS and TightVNC need for now.
#![allow(non_upper_case_globals)] // libvncclient's rfb* constants, matched as patterns
use crate::{Panel, QUIT, config};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::Ordering::*;
use std::time::Duration;
use vncclient_sys::*;

/// What a session needs to connect. The password and pin get re-read on every try, like RDP's.
#[derive(Clone, Default)]
pub struct Opts {
    pub host: String,
    pub port: u32,
    pub user: String,
    pub password: String,
    /// The certificate's SHA-256. None takes any certificate (and logs its fingerprint for pin=).
    pub pin: Option<[u8; 32]>,
    pub insecure: bool,
}

/// A pin in hex. Anything that isn't 64 hex digits matches no certificate (fails closed).
pub fn pin_bytes(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    if hex.len() == 64 {
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0);
        }
    }
    out
}

/// One input event from the main loop, queued for the session thread, since libvncclient isn't
/// thread-safe. The pointer is in 0..1 of the panel, so a framebuffer that isn't the size in
/// viewers.conf still gets the right pixel.
#[derive(Clone, Copy, Debug)]
pub enum Ev {
    Mouse(u32, f64, f64),
    Wheel(bool, f64),
    Key(u16, i32),
}

/// The soft cursor (the cursor pseudo-encodings): the server sends its shape, and we draw it
/// into the framebuffer at the pointer, keeping the pixels under it to put back.
#[derive(Default)]
struct Cursor {
    w: i32,
    h: i32,
    hot: (i32, i32),
    px: Vec<u32>,
    mask: Vec<u8>,
    at: (i32, i32),
    drawn: Option<[i32; 4]>, // where it is in the framebuffer, clipped
    under: Vec<u32>,
}

/// The session's state, which libvncclient's callbacks reach through its client data.
#[derive(Default)]
struct State {
    opts: Opts,
    damage: Option<[i32; 4]>, // changed since the last take: x0, y0, x1, y1
    area: i64,                // this update's rects added up
    change: u32,              // D-045: the most one update changed since the last take, in thousandths
    updates: u32,             // finished updates since the last take
    asked: bool,              // a FramebufferUpdateRequest is out
    resized: bool,
    cursor: Cursor,
    refused: Option<&'static str>, // why the credential guard said no
    malloc_fb: MallocFrameBufferProc,
}

static TAG: u8 = 0;

unsafe fn state(cl: *mut rfbClient) -> &'static mut State {
    unsafe { &mut *(rfbClientGetClientData(cl, &TAG as *const u8 as *mut c_void) as *mut State) }
}

impl State {
    fn grow(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let d = self.damage.get_or_insert([x0, y0, x1, y1]);
        *d = [d[0].min(x0), d[1].min(y0), d[2].max(x1), d[3].max(y1)];
    }
}

/// TLS is up and the password goes inside it (VeNCrypt X509 with VNC's password or a user and password).
unsafe fn secure(cl: *mut rfbClient) -> bool {
    unsafe { !(*cl).tlsSession.is_null() && matches!((*cl).subAuthScheme, rfbVeNCryptX509VNC | rfbVeNCryptX509Plain) }
}

/// The credential guard (review B2): VeNCrypt lets a server pick an unencrypted sub-type after
/// we chose it, so the password goes out only when it's safe, whatever was negotiated.
unsafe fn may_send(cl: *mut rfbClient) -> bool {
    let st = unsafe { state(cl) };
    let ok = st.opts.insecure || unsafe { secure(cl) };
    if !ok {
        st.refused = Some("the host asked for the password without TLS (tls=no allows that)");
    }
    ok
}

fn strdup(s: &str) -> *mut c_char {
    let c = CString::new(s).unwrap_or_default();
    unsafe { libc::strdup(c.as_ptr()) }
}

unsafe extern "C" fn get_password(cl: *mut rfbClient) -> *mut c_char {
    if !unsafe { may_send(cl) } {
        return std::ptr::null_mut();
    }
    strdup(&unsafe { state(cl) }.opts.password) // libvncclient frees it
}

unsafe extern "C" fn get_credential(cl: *mut rfbClient, kind: c_int) -> *mut rfbCredential {
    let st = unsafe { state(cl) };
    let c = unsafe { libc::calloc(1, size_of::<rfbCredential>()) as *mut rfbCredential };
    if c.is_null() {
        return c;
    }
    if kind as u32 == rfbCredentialTypeX509 {
        // No CA file: the certificate is checked against the pin (an unpinned one goes to
        // on_certificate). A pinned one that's also valid for the system CAs passes without
        // the pin, which a LAN host's self-signed certificate never is.
        if let Some(pin) = st.opts.pin {
            let fp = unsafe { libc::malloc(32) as *mut u8 };
            if !fp.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(pin.as_ptr(), fp, 32) };
            }
            unsafe { (*c).x509Credential.x509ExpectedFingerprint = fp };
        }
        return c;
    }
    if !unsafe { may_send(cl) } {
        unsafe { libc::free(c as *mut c_void) };
        return std::ptr::null_mut();
    }
    unsafe {
        (*c).userCredential.username = strdup(&st.opts.user);
        (*c).userCredential.password = strdup(&st.opts.password);
    }
    c
}

/// The certificate isn't the pinned one, or there's no pin. With a pin that's a refusal. Without
/// one it's trust on first use, like an unpaired RDP machine, and the log says how to pin it.
unsafe extern "C" fn on_certificate(cl: *mut rfbClient, subject: *const c_char, _: libc::time_t, _: libc::time_t, fp: *const u8, len: usize) -> rfbBool {
    let st = unsafe { state(cl) };
    let hex: String = unsafe { std::slice::from_raw_parts(fp, len) }.iter().map(|b| format!("{b:02x}")).collect();
    let subject = if subject.is_null() { String::new() } else { unsafe { CStr::from_ptr(subject) }.to_string_lossy().into_owned() };
    if st.opts.pin.is_some() {
        eprintln!("vnc {}: the host's certificate changed ({subject}, sha256 {hex}); not connecting", st.opts.host);
        st.refused = Some("the host's certificate isn't the pinned one");
        return 0;
    }
    eprintln!("vnc {}: certificate {subject}, sha256 {hex}, not pinned; add pin={hex} to its viewers.conf line", st.opts.host);
    1
}

unsafe extern "C" fn malloc_fb(cl: *mut rfbClient) -> rfbBool {
    let st = unsafe { state(cl) };
    st.cursor.drawn = None; // the old buffer (and the cursor in it) is gone
    st.resized = true;
    let ok = st.malloc_fb.map_or(0, |f| unsafe { f(cl) });
    let (w, h) = unsafe { ((*cl).width, (*cl).height) };
    st.grow(0, 0, w, h);
    ok
}

unsafe extern "C" fn got_update(cl: *mut rfbClient, x: c_int, y: c_int, w: c_int, h: c_int) {
    let st = unsafe { state(cl) };
    st.grow(x, y, x + w, y + h);
    st.area += w.max(0) as i64 * h.max(0) as i64;
}

unsafe extern "C" fn finished_update(cl: *mut rfbClient) {
    let st = unsafe { state(cl) };
    let all = unsafe { ((*cl).width.max(1) as i64) * ((*cl).height.max(1) as i64) };
    st.change = st.change.max((st.area * 1000 / all).min(1000) as u32);
    st.area = 0;
    st.updates += 1;
    // Any update answers the request, even one with only the desktop size or the cursor in it
    // (TigerVNC's first answer), so the next request goes out at once if the panel wants one.
    st.asked = false;
}

unsafe extern "C" fn cursor_shape(cl: *mut rfbClient, xhot: c_int, yhot: c_int, w: c_int, h: c_int, bpp: c_int) {
    let st = unsafe { state(cl) };
    unsafe { restore(cl, st) };
    let n = (w.max(0) * h.max(0)) as usize;
    let (src, mask) = unsafe { ((*cl).rcSource, (*cl).rcMask) };
    if bpp != 4 || n == 0 || src.is_null() || mask.is_null() {
        st.cursor.w = 0; // an empty shape hides it
        return;
    }
    let c = &mut st.cursor;
    (c.w, c.h, c.hot) = (w, h, (xhot, yhot));
    c.px = unsafe { std::slice::from_raw_parts(src as *const u32, n) }.to_vec();
    c.mask = unsafe { std::slice::from_raw_parts(mask, n) }.to_vec();
    unsafe { draw(cl, st) };
}

/// Before libvncclient writes this area, the cursor comes out of it.
unsafe extern "C" fn lock_area(cl: *mut rfbClient, x: c_int, y: c_int, w: c_int, h: c_int) {
    let st = unsafe { state(cl) };
    if let Some(d) = st.cursor.drawn
        && x < d[2] && x + w > d[0] && y < d[3] && y + h > d[1]
    {
        unsafe { restore(cl, st) };
    }
}

/// After it wrote a rect, the cursor goes back in.
unsafe extern "C" fn unlock(cl: *mut rfbClient) {
    unsafe { draw(cl, state(cl)) };
}

/// The server moved the pointer (PointerPos).
unsafe extern "C" fn cursor_pos(cl: *mut rfbClient, x: c_int, y: c_int) -> rfbBool {
    unsafe { move_cursor(cl, x, y) };
    1
}

unsafe fn move_cursor(cl: *mut rfbClient, x: i32, y: i32) {
    let st = unsafe { state(cl) };
    if st.cursor.at == (x, y) {
        return;
    }
    unsafe { restore(cl, st) };
    st.cursor.at = (x, y);
    unsafe { draw(cl, st) };
}

unsafe fn restore(cl: *mut rfbClient, st: &mut State) {
    let Some([x0, y0, x1, y1]) = st.cursor.drawn.take() else { return };
    let (fb, w) = unsafe { ((*cl).frameBuffer as *mut u32, (*cl).width) };
    let mut i = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            unsafe { *fb.add((y * w + x) as usize) = st.cursor.under[i] };
            i += 1;
        }
    }
    st.grow(x0, y0, x1, y1);
}

unsafe fn draw(cl: *mut rfbClient, st: &mut State) {
    let c = &mut st.cursor;
    let (fb, w, h) = unsafe { ((*cl).frameBuffer as *mut u32, (*cl).width, (*cl).height) };
    if c.drawn.is_some() || c.w == 0 || fb.is_null() {
        return;
    }
    let (ox, oy) = (c.at.0 - c.hot.0, c.at.1 - c.hot.1);
    let r = [ox.max(0), oy.max(0), (ox + c.w).min(w), (oy + c.h).min(h)];
    if r[2] <= r[0] || r[3] <= r[1] {
        return;
    }
    c.under.clear();
    for y in r[1]..r[3] {
        for x in r[0]..r[2] {
            let at = unsafe { fb.add((y * w + x) as usize) };
            c.under.push(unsafe { *at });
            let k = ((y - oy) * c.w + (x - ox)) as usize;
            if c.mask[k] != 0 {
                unsafe { *at = c.px[k] };
            }
        }
    }
    c.drawn = Some(r);
    st.grow(r[0], r[1], r[2], r[3]);
}

/// One connection to an RFB server.
pub struct Session {
    cl: *mut rfbClient,
    mask: i32,          // the buttons held
    at: (i32, i32),     // where we last put the pointer
    notches: [f64; 2],  // wheel turns not yet a whole click (vertical, horizontal)
    keys: HashMap<u16, u32>, // the keysym each held key went down as
}

unsafe impl Send for Session {}

impl Session {
    pub fn connect(opts: &Opts) -> Result<Session, String> {
        unsafe {
            let cl = rfbGetClient(8, 3, 4);
            if cl.is_null() {
                return Err("out of memory".into());
            }
            let mut st = Box::new(State { opts: opts.clone(), asked: true, malloc_fb: (*cl).MallocFrameBuffer, ..Default::default() });
            // BGRX in memory, the GPU buffers' XRGB8888, so no conversion
            let f = &mut (*cl).format;
            (f.redShift, f.greenShift, f.blueShift, f.bigEndian) = (16, 8, 0, 0);
            let a = &mut (*cl).appData;
            a.encodingsString = c"tight zrle copyrect".as_ptr(); // review B4: no other decoders
            (a.enableJPEG, a.qualityLevel, a.compressLevel, a.useRemoteCursor) = (1, 7, 1, 1);
            libc::free((*cl).serverHost as *mut c_void);
            (*cl).serverHost = strdup(&opts.host);
            (*cl).serverPort = opts.port as c_int;
            ((*cl).connectTimeout, (*cl).readTimeout) = (10, 30);
            (*cl).canHandleNewFBSize = 1;
            (*cl).ccPacedRequests = 1;
            let schemes: &[u32] = if opts.insecure { &[rfbVeNCrypt, rfbVncAuth, rfbARD] } else { &[rfbVeNCrypt] };
            SetClientAuthSchemes(cl, schemes.as_ptr(), schemes.len() as c_int);
            (*cl).GetPassword = Some(get_password);
            (*cl).GetCredential = Some(get_credential);
            (*cl).GetX509CertFingerprintMismatchDecision = Some(on_certificate);
            (*cl).MallocFrameBuffer = Some(malloc_fb);
            (*cl).GotFrameBufferUpdate = Some(got_update);
            (*cl).FinishedFrameBufferUpdate = Some(finished_update);
            (*cl).GotCursorShape = Some(cursor_shape);
            (*cl).SoftCursorLockArea = Some(lock_area);
            (*cl).SoftCursorUnlockScreen = Some(unlock);
            (*cl).HandleCursorPos = Some(cursor_pos);
            rfbClientSetClientData(cl, &TAG as *const u8 as *mut c_void, &mut *st as *mut State as *mut c_void);
            let s = Session { cl, mask: 0, at: (0, 0), notches: [0.0; 2], keys: HashMap::new() };
            std::mem::forget(st); // Drop frees it
            if rfbClientConnect(cl) == 0 {
                return Err("can't reach it".into());
            }
            if rfbClientInitialise(cl) == 0 {
                return Err(state(cl).refused.unwrap_or("refused (the password, or no security type we allow)").into());
            }
            // Nothing goes to a session without TLS unless tls=no said so, and never to one that
            // didn't check a password, since a fake desktop would get every keystroke.
            let none = (*cl).authScheme == rfbNoAuth || matches!((*cl).subAuthScheme, rfbNoAuth | rfbVeNCryptTLSNone | rfbVeNCryptX509None);
            if none {
                return Err("the host asked for no password".into());
            }
            if !opts.insecure && !secure(cl) {
                return Err("no TLS (tls=no allows that)".into());
            }
            Ok(s)
        }
    }

    /// How it's secured, for the log.
    pub fn security(&self) -> String {
        let (a, sub, tls) = unsafe { ((*self.cl).authScheme, (*self.cl).subAuthScheme, !(*self.cl).tlsSession.is_null()) };
        let name = match (a, sub) {
            (_, rfbVeNCryptX509VNC) => "VeNCrypt X509 + VNC password",
            (_, rfbVeNCryptX509Plain) => "VeNCrypt X509 + user and password",
            (rfbARD, _) => "Apple (ARD), no TLS",
            (rfbVncAuth, _) => "VNC password, no TLS",
            _ => "other",
        };
        format!("{name}{}", if tls { ", TLS" } else { "" })
    }

    pub fn size(&self) -> (i32, i32) {
        unsafe { ((*self.cl).width, (*self.cl).height) }
    }

    /// The picture: BGRX rows, width * 4 bytes apart.
    pub fn frame(&self) -> (*const u8, usize, i32, i32) {
        let (w, h) = self.size();
        (unsafe { (*self.cl).frameBuffer }, w.max(0) as usize * 4, w, h)
    }

    /// A request is out and no update has answered it yet.
    pub fn asked(&self) -> bool {
        unsafe { state(self.cl) }.asked
    }

    /// Asks for whatever changed since the last picture.
    pub fn request(&mut self) -> bool {
        unsafe { state(self.cl) }.asked = true;
        unsafe { SendIncrementalFramebufferUpdateRequest(self.cl) != 0 }
    }

    /// Bytes already read and waiting (libvncclient's buffer or a TLS record), which poll can't
    /// see (review C1).
    fn pending(&self) -> bool {
        let tls = unsafe { (*self.cl).tlsSession };
        unsafe { (*self.cl).buffered > 0 || (!tls.is_null() && SSL_pending(tls) > 0) }
    }

    /// Waits for the server or a poke (`wake`, an fd, or -1). True when there's something to read.
    pub fn wait(&self, wake: c_int, timeout: Duration) -> bool {
        if self.pending() {
            return true;
        }
        let mut fds = [libc::pollfd { fd: unsafe { (*self.cl).sock }, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 }];
        let n = if wake >= 0 { 2 } else { 1 };
        let r = unsafe { libc::poll(fds.as_mut_ptr(), n, timeout.as_millis().min(i32::MAX as u128) as c_int) };
        r > 0 && fds[0].revents != 0
    }

    /// Reads and decodes one server message. False when the connection's gone.
    pub fn handle(&mut self) -> bool {
        unsafe { HandleRFBServerMessage(self.cl) != 0 }
    }

    /// What changed since the last take: the region, D-045's change, and how many updates ended.
    pub fn take(&mut self) -> (Option<[i32; 4]>, u32, u32) {
        let st = unsafe { state(self.cl) };
        (st.damage.take(), std::mem::take(&mut st.change), std::mem::take(&mut st.updates))
    }

    /// The framebuffer changed size since the last call.
    pub fn resized(&mut self) -> bool {
        std::mem::take(&mut unsafe { state(self.cl) }.resized)
    }

    /// Sends one input event. False when the connection's gone.
    pub fn send(&mut self, e: Ev) -> bool {
        use freerdp_sys::{PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3, PTR_FLAGS_DOWN};
        let cl = self.cl;
        match e {
            Ev::Mouse(flags, u, v) => {
                let (w, h) = self.size();
                self.at = (((u * w as f64) as i32).clamp(0, (w - 1).max(0)), ((v * h as f64) as i32).clamp(0, (h - 1).max(0)));
                // RDP's buttons 1, 2, 3 are left, right, middle; RFB's mask bits 0, 1, 2 are left, middle, right
                let bit = [(PTR_FLAGS_BUTTON1, 1), (PTR_FLAGS_BUTTON2, 4), (PTR_FLAGS_BUTTON3, 2)].iter().filter(|(f, _)| flags & f != 0).fold(0, |m, (_, b)| m | b);
                if flags & PTR_FLAGS_DOWN != 0 { self.mask |= bit } else { self.mask &= !bit }
                unsafe { move_cursor(cl, self.at.0, self.at.1) };
                unsafe { SendPointerEvent(cl, self.at.0, self.at.1, self.mask) != 0 }
            }
            Ev::Wheel(horizontal, notches) => {
                // whole clicks of buttons 4/5 (up/down) or 6/7 (left/right), the rest kept for the next turn
                let acc = &mut self.notches[horizontal as usize];
                *acc += notches;
                let n = acc.trunc();
                *acc -= n;
                let button = match (horizontal, n > 0.0) {
                    (false, true) => 4,
                    (false, false) => 5,
                    (true, false) => 6,
                    (true, true) => 7,
                };
                let bit = 1 << (button - 1);
                (0..n.abs() as i32).all(|_| unsafe { SendPointerEvent(cl, self.at.0, self.at.1, self.mask | bit) != 0 && SendPointerEvent(cl, self.at.0, self.at.1, self.mask) != 0 })
            }
            Ev::Key(code, value) => {
                let down = value != 0;
                let shift = [42u16, 54].iter().any(|k| self.keys.contains_key(k));
                // a key comes up as the keysym it went down as, even if Shift changed in between
                let sym = if down { *self.keys.entry(code).or_insert_with(|| keysym(code, shift)) } else { self.keys.remove(&code).unwrap_or_else(|| keysym(code, shift)) };
                let q = qnum(code);
                unsafe {
                    if q != 0 && SupportsClient2Server(cl, rfbQemuEvent as c_int) != 0 {
                        SendExtendedKeyEvent(cl, sym, q, down as rfbBool) != 0 // the key itself, so the host's layout applies
                    } else if sym != 0 {
                        SendKeyEvent(cl, sym, down as rfbBool) != 0
                    } else {
                        true
                    }
                }
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let st = state(self.cl) as *mut State;
            let fb = (*self.cl).frameBuffer;
            rfbClientCleanup(self.cl); // closes the socket and TLS; it leaves the framebuffer to us
            libc::free(fb as *mut c_void);
            drop(Box::from_raw(st));
        }
    }
}

/// QEMU's key number for an evdev code: the PC scancode, with an E0-prefixed one as 0x80 | code
/// (review A5; kvm::scancode marks those 0x100).
fn qnum(code: u16) -> u32 {
    match code {
        119 => 0xC6, // Pause
        _ => match crate::kvm::scancode(code) {
            sc if sc & 0x100 != 0 => 0x80 | (sc & 0x7F),
            sc => sc,
        },
    }
}

/// The X keysym for an evdev code on a US layout, for servers without QEMU's extended keys
/// (macOS, TightVNC). Printable keys follow Shift; Caps Lock is left to the host.
fn keysym(code: u16, shift: bool) -> u32 {
    // evdev codes 2-13, 16-27, 30-41, 43-53 and 57: unshifted, shifted
    const ROWS: [(u16, &str); 5] = [(2, "1!2@3#4$5%6^7&8*9(0)-_=+"), (16, "qQwWeErRtTyYuUiIoOpP[{]}"), (30, "aAsSdDfFgGhHjJkKlL;:'\"`~"), (43, "\\|zZxXcCvVbBnNmM,<.>/?"), (57, "  ")];
    for (first, chars) in ROWS {
        let i = code.wrapping_sub(first) as usize;
        if let Some(c) = chars.as_bytes().get(2 * i + shift as usize) {
            return *c as u32; // Latin-1 keysyms are their code points
        }
    }
    match code {
        1 => 0xFF1B,             // Escape
        14 => 0xFF08,            // BackSpace
        15 => 0xFF09,            // Tab
        28 => 0xFF0D,            // Return
        29 => 0xFFE3,            // Control_L
        42 => 0xFFE1,            // Shift_L
        54 => 0xFFE2,            // Shift_R
        55 => 0xFFAA,            // KP_Multiply
        56 => 0xFFE9,            // Alt_L
        58 => 0xFFE5,            // Caps_Lock
        59..=68 => 0xFFBE + (code - 59) as u32, // F1-F10
        69 => 0xFF7F,            // Num_Lock
        70 => 0xFF14,            // Scroll_Lock
        71 => 0xFFB7, 72 => 0xFFB8, 73 => 0xFFB9, 74 => 0xFFAD, // KP 7 8 9 -
        75 => 0xFFB4, 76 => 0xFFB5, 77 => 0xFFB6, 78 => 0xFFAB, // KP 4 5 6 +
        79 => 0xFFB1, 80 => 0xFFB2, 81 => 0xFFB3, 82 => 0xFFB0, 83 => 0xFFAE, // KP 1 2 3 0 .
        87 => 0xFFC8,            // F11
        88 => 0xFFC9,            // F12
        96 => 0xFF8D,            // KP_Enter
        97 => 0xFFE4,            // Control_R
        98 => 0xFFAF,            // KP_Divide
        99 => 0xFF61,            // Print
        100 => 0xFFEA,           // Alt_R
        102 => 0xFF50,           // Home
        103 => 0xFF52,           // Up
        104 => 0xFF55,           // Prior
        105 => 0xFF51,           // Left
        106 => 0xFF53,           // Right
        107 => 0xFF57,           // End
        108 => 0xFF54,           // Down
        109 => 0xFF56,           // Next
        110 => 0xFF63,           // Insert
        111 => 0xFFFF,           // Delete
        119 => 0xFF13,           // Pause
        125 => 0xFFEB,           // Super_L
        126 => 0xFFEC,           // Super_R
        127 => 0xFF67,           // Menu
        _ => 0,
    }
}

// ------------------------------------------------------------------ the panel

/// Input waiting for each panel's session thread.
static INPUT: Mutex<Vec<(usize, Ev)>> = Mutex::new(Vec::new());

fn queue(p: &Panel, e: Ev) {
    if p.connected.load(Acquire) {
        INPUT.lock().unwrap().push((p.index, e));
        crate::rdp::poke(p); // its session thread may be in its wait
    }
}

fn take_input(i: usize) -> Vec<Ev> {
    let mut q = INPUT.lock().unwrap();
    let (mine, rest) = std::mem::take(&mut *q).into_iter().partition(|(j, _)| *j == i);
    *q = rest;
    mine.into_iter().map(|(_, e)| e).collect()
}

/// Pointer input in the panel's pixels, in RDP's flags (Panel::mouse).
pub fn mouse(p: &Panel, flags: u32, x: f64, y: f64) {
    let (w, h) = p.size();
    queue(p, Ev::Mouse(flags, x / w as f64, y / h as f64));
}

/// Wheel notches, positive is up or right (Panel::wheel).
pub fn wheel(p: &Panel, horizontal: bool, notches: f64) {
    queue(p, Ev::Wheel(horizontal, notches));
}

/// An evdev key: 0 up, 1 down, 2 repeat (Panel::key).
pub fn key(p: &Panel, code: u16, value: i32) {
    queue(p, Ev::Key(code, value));
}

/// The panel wants its next picture: the last one is on its way to the GPU, and it's in view
/// and not paused (attention.rs). Until then the server sends nothing.
fn wants(p: &Panel) -> bool {
    !p.dirty.load(Acquire) && p.level() != crate::attention::Level::Paused && !p.away() && !crate::HIDDEN.load(Relaxed)
}

fn thread_cpu() -> Duration {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}

/// Connects, runs the session, and reconnects every 5 s, until cc-panels ends or the panel is
/// disconnected (main.rs disconnect), the same as rdp::run.
pub fn run(p: &'static Panel) {
    unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10) }; // gentle, like RDP's decode
    // libvncclient's own log says a lot on every try; CC_VNC_LOG=1 brings it back
    unsafe { rfbEnableClientLogging = std::env::var_os("CC_VNC_LOG").is_some() as rfbBool };
    // poke's event (rdp::poke), shared with RDP. ponytail: never closed, one per panel, as there
    let mut wake = p.wake.load(Acquire);
    if wake.is_null() {
        wake = unsafe { freerdp_sys::CreateEventA(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        p.wake.store(wake, Release);
    }
    let fd = if wake.is_null() { -1 } else { unsafe { freerdp_sys::GetEventFileDescriptor(wake) } };
    let pause = || {
        for _ in 0..5 {
            if QUIT.load(Relaxed) || !p.live() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    };
    let mut said = String::new(); // each failure once, not on every retry
    while !QUIT.load(Relaxed) && p.live() {
        let v = &p.v;
        let o = v.vnc.clone().unwrap_or_default();
        let pin = if o.pin.is_empty() { config::pin(&v.machine) } else { Some(o.pin.clone()) };
        let opts = Opts { host: v.host.clone(), port: v.port, user: v.user.clone(), password: config::password(&v.machine), pin: pin.map(|h| pin_bytes(&h)), insecure: o.insecure };
        let mut s = match Session::connect(&opts) {
            Ok(s) => s,
            Err(why) => {
                if why != std::mem::replace(&mut said, why.clone()) {
                    eprintln!("{}: can't connect (VNC {}:{}): {why}", v.name, v.host, v.port);
                }
                pause();
                continue;
            }
        };
        said.clear();
        let (w, h) = s.size();
        eprintln!("{}: connected ({w}x{h}, VNC, {})", v.name, s.security());
        if (w as u32, h as u32) != p.size() {
            eprintln!("{}: the host's screen is {w}x{h}, viewers.conf says {}x{}; the pointer's scaled to it", v.name, p.size().0, p.size().1);
        }
        take_input(p.index); // anything typed while it was down is stale
        p.connected.store(true, Release);
        'session: while !QUIT.load(Relaxed) && p.live() {
            if !s.asked() && wants(p) && !s.request() {
                break;
            }
            let ready = s.wait(fd, crate::gpu::wait(p));
            if !wake.is_null() {
                unsafe { freerdp_sys::ResetEvent(wake) }; // any poke from here on wakes the next wait
            }
            for e in take_input(p.index) {
                if !s.send(e) {
                    break 'session;
                }
            }
            let _g = p.lock.lock().unwrap();
            if ready {
                let t = thread_cpu();
                if !s.handle() {
                    break;
                }
                let ms = (thread_cpu() - t).as_millis();
                if ms >= 20 {
                    eprintln!("slow {}: VNC decode {ms} ms CPU", v.name);
                }
            }
            s.resized();
            let (d, change, _) = s.take();
            if let Some([x0, y0, x1, y1]) = d {
                for st in &p.stale {
                    st.grow(x0, y0, x1, y1); // neither GPU buffer has this yet
                }
                p.change.fetch_max(change, Relaxed);
                p.dirty.store(true, Release);
            }
            let (data, stride, w, h) = s.frame();
            if !data.is_null() {
                crate::gpu::write_frame(p, data, stride, w, h); // when it's due, like RDP's
            }
        }
        p.connected.store(false, Release);
        drop(s);
        if !QUIT.load(Relaxed) {
            eprintln!("{}: disconnected", v.name);
        }
        pause();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_us_keysyms_and_qemu_numbers() {
        assert_eq!((keysym(30, false), keysym(30, true)), ('a' as u32, 'A' as u32));
        assert_eq!((keysym(2, true), keysym(13, false), keysym(43, false), keysym(53, true)), ('!' as u32, '=' as u32, '\\' as u32, '?' as u32));
        assert_eq!((keysym(57, false), keysym(28, false), keysym(111, false), keysym(68, false)), (' ' as u32, 0xFF0D, 0xFFFF, 0xFFC7));
        assert_eq!((qnum(30), qnum(103), qnum(97), qnum(119)), (0x1E, 0xC8, 0x9D, 0xC6)); // a, Up (E0 48), Right Ctrl (E0 1D), Pause
    }

    #[test]
    fn pins_fail_closed() {
        let hex = "00ff".repeat(16);
        assert_eq!(pin_bytes(&hex)[..2], [0x00, 0xff]);
        assert_eq!(pin_bytes("abc"), [0; 32], "not a pin, so it matches nothing");
    }

    /// Against tools/vnc-test-server.sh (no headset, no panel):
    ///   tools/vnc-test-server.sh test
    /// It sets CC_VNC_TEST=host:port:password:pin, CC_VNC_TEST_PLAIN (a VNC-password-only
    /// server), CC_VNC_TEST_XEV (xev's log) and DISPLAY for xsetroot.
    #[test]
    #[ignore]
    fn against_the_test_server() {
        let env = std::env::var("CC_VNC_TEST").expect("CC_VNC_TEST=host:port:password:pin (tools/vnc-test-server.sh test)");
        let f: Vec<&str> = env.splitn(4, ':').collect();
        let opts = Opts { host: f[0].into(), port: f[1].parse().unwrap(), user: "cc".into(), password: f[2].into(), pin: Some(pin_bytes(f[3])), insecure: false };

        // 1. security: a wrong pin, and a server without TLS, are refused before the password goes out
        let wrong = Opts { pin: Some([7; 32]), ..opts.clone() };
        let e = Session::connect(&wrong).err().expect("a wrong pin connects");
        eprintln!("wrong pin: {e}");
        if let Ok(plain) = std::env::var("CC_VNC_TEST_PLAIN") {
            let (h, port) = plain.rsplit_once(':').unwrap();
            let p = Opts { host: h.into(), port: port.parse().unwrap(), ..opts.clone() };
            let e = Session::connect(&p).err().expect("a server without TLS got the password");
            eprintln!("no TLS: {e}");
            let s = Session::connect(&Opts { insecure: true, ..p }).expect("tls=no connects to a VNC-password server");
            eprintln!("tls=no: {}", s.security());
        }

        // 2. the first picture
        let mut s = Session::connect(&opts).expect("connect");
        eprintln!("connected: {} {:?}", s.security(), s.size());
        let frame = |s: &mut Session, ms: u64| -> Option<[i32; 4]> {
            let end = std::time::Instant::now() + Duration::from_millis(ms);
            let mut got = None;
            while std::time::Instant::now() < end {
                if s.wait(-1, Duration::from_millis(50)) {
                    assert!(s.handle(), "connection lost");
                }
                let (d, change, n) = s.take();
                got = d.or(got);
                if n > 0 && !s.asked() {
                    if change > 0 {
                        return got; // pixels
                    }
                    assert!(s.request()); // only the desktop size or the cursor, so ask again, as run does
                }
            }
            got
        };
        assert!(frame(&mut s, 5000).is_some(), "no first picture");
        let px = |s: &Session, x: i32, y: i32| -> [u8; 3] {
            let (d, stride, _, _) = s.frame();
            let at = unsafe { d.add(y as usize * stride + x as usize * 4) };
            unsafe { [*at.add(2), *at.add(1), *at] } // RGB from BGRX
        };
        let (w, h) = s.size();
        let corner = px(&s, w - 5, h - 5);
        eprintln!("pixel ({},{}) = {corner:02x?}", w - 5, h - 5);
        assert_eq!(corner, [0x20, 0x80, 0xc0], "the test pattern's background");

        // 3. pull: with no request out, a change on the host sends nothing until we ask
        let display = std::env::var("DISPLAY").unwrap_or_default();
        let set = |c: &str| std::process::Command::new("xsetroot").args(["-display", &display, "-solid", c]).status().unwrap();
        set("#c08020");
        assert!(frame(&mut s, 700).is_none(), "the server sent a picture nobody asked for");
        assert_eq!(px(&s, w - 5, h - 5), [0x20, 0x80, 0xc0], "unchanged until we ask");
        assert!(s.request());
        assert!(frame(&mut s, 3000).is_some(), "no picture after asking");
        let now = px(&s, w - 5, h - 5);
        eprintln!("after asking: {now:02x?}");
        assert_eq!(now, [0xc0, 0x80, 0x20]);
        set("#2080c0");

        // 4. a click and a key into xev's window (100,100 to 300,300 on the host)
        let at = |x: f64, y: f64| (x / w as f64, y / h as f64);
        let (u, v) = at(200.0, 150.0);
        use freerdp_sys::{PTR_FLAGS_BUTTON1, PTR_FLAGS_DOWN, PTR_FLAGS_MOVE};
        for e in [Ev::Mouse(PTR_FLAGS_MOVE, u, v), Ev::Mouse(PTR_FLAGS_BUTTON1 | PTR_FLAGS_DOWN, u, v), Ev::Mouse(PTR_FLAGS_BUTTON1, u, v)] {
            assert!(s.send(e));
        }
        for e in [Ev::Key(42, 1), Ev::Key(30, 1), Ev::Key(30, 0), Ev::Key(42, 0), Ev::Wheel(false, 1.0)] {
            assert!(s.send(e));
        }
        assert!(s.request());
        frame(&mut s, 1500);
        if let Ok(log) = std::env::var("CC_VNC_TEST_XEV") {
            let t = std::fs::read_to_string(&log).unwrap_or_default();
            let press = t.lines().filter(|l| l.contains("ButtonPress") || l.contains("button ") || l.contains("KeyPress") || l.contains("keysym")).collect::<Vec<_>>();
            eprintln!("xev saw:\n{}", press.join("\n"));
            assert!(t.contains("ButtonPress"), "no click in xev's window");
            assert!(t.contains("button 1,") && t.contains("button 4,"), "the left button and the wheel");
            assert!(t.contains("(keysym 0x41, A)"), "Shift+A as A");
        }
        let cursor = &unsafe { state(s.cl) }.cursor;
        eprintln!("cursor shape {}x{} hot {:?}, drawn at {:?}", cursor.w, cursor.h, cursor.hot, cursor.drawn);
    }
}
