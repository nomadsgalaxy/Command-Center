//! The VR mirror, read continuously by a thread so the newest frame is always at hand, like
//! scan.py's Camera. The mirror is /dev/video99: SteamVR's v4l2cam forwards the left eye's view
//! with passthrough, RGB3 1920x1080. It uses V4L2 mmap streaming, so frames (~86 a second) are
//! taken and handed back without copying. Only a shot copies one. A failed take reopens the
//! device, and after 10 s without a frame it gives up.
use crate::image::Gray;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Condvar, Mutex};

use std::time::Duration;

pub const DEVICE: &str = "/dev/video99";

// From linux/videodev2.h, 64-bit layouts.
const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const MEMORY_MMAP: u32 = 1;
const NBUFS: u32 = 4; // One is held as the newest, and the writer fills the rest.

#[repr(C)]
#[derive(Default)]
struct RequestBuffers {
    count: u32,
    type_: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    _pad: u32,
    timestamp: [i64; 2],
    timecode: [u32; 4],
    sequence: u32,
    memory: u32,
    m: u64, // The mmap offset.
    length: u32,
    reserved2: u32,
    request_fd: u32,
    _pad2: u32,
}

#[repr(C)]
struct Format {
    type_: u32,
    _pad: u32,
    // v4l2_pix_format, then the rest of the 200-byte union.
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    rest: [u8; 176],
}

const fn ioc(dir: u64, nr: u64, size: usize) -> u64 {
    (dir << 30) | ((size as u64) << 16) | ((b'V' as u64) << 8) | nr
}
const RW: u64 = 3;
const W: u64 = 1;
const VIDIOC_G_FMT: u64 = ioc(RW, 4, std::mem::size_of::<Format>());
const VIDIOC_REQBUFS: u64 = ioc(RW, 8, std::mem::size_of::<RequestBuffers>());
const VIDIOC_QUERYBUF: u64 = ioc(RW, 9, std::mem::size_of::<Buffer>());
const VIDIOC_QBUF: u64 = ioc(RW, 15, std::mem::size_of::<Buffer>());
const VIDIOC_DQBUF: u64 = ioc(RW, 17, std::mem::size_of::<Buffer>());
const VIDIOC_STREAMON: u64 = ioc(W, 18, 4);

fn xioctl<T>(fd: i32, req: u64, arg: &mut T) -> std::io::Result<()> {
    loop {
        if unsafe { libc::ioctl(fd, req as _, arg as *mut T) } == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(e);
        }
    }
}

struct Map {
    ptr: *mut u8,
    len: usize,
}
unsafe impl Send for Map {}

impl Drop for Map {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr as *mut _, self.len) };
    }
}

/// One open, streaming device.
struct Stream {
    _file: File,
    fd: i32,
    maps: Vec<Map>,
    w: usize,
    h: usize,
    stride: usize,
}

impl Stream {
    fn open(path: &str) -> std::io::Result<Stream> {
        // When Stream drops, the file closes, and the kernel stops streaming and frees the buffers.
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let fd = file.as_raw_fd();
        let mut fmt = Format { type_: BUF_TYPE_VIDEO_CAPTURE, _pad: 0, width: 0, height: 0, pixelformat: 0, field: 0, bytesperline: 0, sizeimage: 0, rest: [0; 176] };
        xioctl(fd, VIDIOC_G_FMT, &mut fmt)?;
        if fmt.pixelformat != u32::from_le_bytes(*b"RGB3") {
            return Err(std::io::Error::other(format!("{path}: not RGB3 ({:08x})", fmt.pixelformat)));
        }
        let mut req = RequestBuffers { count: NBUFS, type_: BUF_TYPE_VIDEO_CAPTURE, memory: MEMORY_MMAP, ..Default::default() };
        xioctl(fd, VIDIOC_REQBUFS, &mut req)?;
        let mut maps = Vec::new();
        for i in 0..req.count {
            let mut b = Buffer { index: i, type_: BUF_TYPE_VIDEO_CAPTURE, memory: MEMORY_MMAP, ..Default::default() };
            xioctl(fd, VIDIOC_QUERYBUF, &mut b)?;
            let ptr = unsafe { libc::mmap(std::ptr::null_mut(), b.length as usize, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, b.m as libc::off_t) };
            if ptr == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            maps.push(Map { ptr: ptr as *mut u8, len: b.length as usize });
            xioctl(fd, VIDIOC_QBUF, &mut b)?;
        }
        let mut t = BUF_TYPE_VIDEO_CAPTURE as i32;
        xioctl(fd, VIDIOC_STREAMON, &mut t)?;
        let stride = (fmt.bytesperline as usize).max(fmt.width as usize * 3);
        Ok(Stream { _file: file, fd, maps, w: fmt.width as usize, h: fmt.height as usize, stride })
    }

    /// Dequeues the next frame's buffer, blocking up to `wait`. Hand it back with give().
    fn take(&self, wait: Duration) -> std::io::Result<Buffer> {
        let mut p = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
        if !wait.is_zero() && unsafe { libc::poll(&mut p, 1, wait.as_millis() as i32) } <= 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
        }
        let mut b = Buffer { type_: BUF_TYPE_VIDEO_CAPTURE, memory: MEMORY_MMAP, ..Default::default() };
        xioctl(self.fd, VIDIOC_DQBUF, &mut b)?;
        Ok(b)
    }

    fn give(&self, mut b: Buffer) -> std::io::Result<()> {
        xioctl(self.fd, VIDIOC_QBUF, &mut b)
    }

    /// Returns a buffer's frame as packed RGB.
    fn rgb(&self, b: &Buffer) -> Vec<u8> {
        let m = &self.maps[b.index as usize];
        let data = unsafe { std::slice::from_raw_parts(m.ptr, m.len) };
        let row = self.w * 3;
        let mut out = Vec::with_capacity(row * self.h);
        for y in 0..self.h {
            out.extend_from_slice(&data[y * self.stride..y * self.stride + row]);
        }
        out
    }
}

/// The device, plus the newest frame's buffer, which stays dequeued until the next one comes.
struct Shared {
    stream: Stream,
    held: Option<Buffer>,
    taken: f64,
    gone: bool,
}

pub struct Camera {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    pub w: usize,
    pub h: usize,
}

impl Camera {
    pub fn open() -> Result<Camera, String> {
        Camera::open_at(DEVICE)
    }

    pub fn open_at(path: &str) -> Result<Camera, String> {
        let s = Stream::open(path).map_err(|e| format!("can't open {path}: {e}"))?;
        let (w, h) = (s.w, s.h);
        let shared = Arc::new((Mutex::new(Shared { stream: s, held: None, taken: f64::NEG_INFINITY, gone: false }), Condvar::new()));
        let sh = shared.clone();
        let path = path.to_owned();
        std::thread::spawn(move || read(&path, &sh));
        Ok(Camera { shared, w, h })
    }

    /// Returns the first frame taken after monotonic time t as (grey, packed RGB, the time it was
    /// taken). Like scan.py's frame_after, it waits 12 s at most and then fails with "no frame". This
    /// is the only place a frame gets copied.
    pub fn frame_after(&self, t: f64) -> Result<(Gray, Vec<u8>, f64), String> {
        let (lock, cv) = &*self.shared;
        let g = lock.lock().unwrap();
        let (g, timeout) = cv.wait_timeout_while(g, Duration::from_secs(12), |s| !s.gone && !(s.held.is_some() && s.taken >= t)).unwrap();
        if timeout.timed_out() || g.gone {
            return Err(format!("no frame from {DEVICE}"));
        }
        let rgb = g.stream.rgb(g.held.as_ref().unwrap());
        let taken = g.taken;
        drop(g);
        Ok((Gray::from_rgb(self.w, self.h, &rgb), rgb, taken))
    }
}

/// The reader. It dequeues each new frame and hands back the one it held before, without copying anything.
fn read(path: &str, sh: &(Mutex<Shared>, Condvar)) {
    let (lock, cv) = sh;
    let mut last = crate::now();
    loop {
        let fd = lock.lock().unwrap().stream.fd;
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut p, 1, 500) } > 0;
        let mut g = lock.lock().unwrap();
        let got = if ready { g.stream.take(Duration::ZERO) } else { Err(std::io::Error::from(std::io::ErrorKind::TimedOut)) };
        match got {
            Ok(b) => {
                if let Some(old) = g.held.replace(b) {
                    let _ = g.stream.give(old);
                }
                last = crate::now();
                g.taken = last;
                cv.notify_all();
            }
            Err(_) => {
                if crate::now() - last > 10.0 {
                    g.gone = true;
                    cv.notify_all();
                    return;
                }
                if ready {
                    // A failed take, so reopen. This happened live when the mirror device hiccuped mid-scan.
                    drop(g);
                    std::thread::sleep(Duration::from_millis(200));
                    if let Ok(n) = Stream::open(path) {
                        let mut g = lock.lock().unwrap();
                        g.held = None;
                        g.stream = n;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn struct_sizes_match_the_kernel() {
        assert_eq!(std::mem::size_of::<super::Buffer>(), 88);
        assert_eq!(std::mem::size_of::<super::RequestBuffers>(), 20);
        assert_eq!(std::mem::size_of::<super::Format>(), 208);
        assert_eq!(super::VIDIOC_DQBUF, 0xc0585611);
        assert_eq!(super::VIDIOC_QBUF, 0xc058560f);
        assert_eq!(super::VIDIOC_G_FMT, 0xc0d05604);
    }
}
