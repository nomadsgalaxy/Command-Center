//! The SteamVR laser off our panels (docs/laser-pointer-design.md). kvm's ray aims cc_pointer,
//! an invisible controller our driver shows while we lease it, so SteamVR's own laser can reach
//! UI we don't own. These are the pure parts: raw-space poses, the lease protocol, which hand to
//! take, and the claim state machine. They're all driven by explicit inputs so the tests don't
//! need VR.
use crate::geometry::{Mat, V3, cross, norm};
use std::time::{Duration, Instant};

/// Lease button bits, the way the cc_pointer driver (crates/cc-pointer) reads them. trigger / b / x are the
/// left / right / middle clicks, a only claims the laser (switchlaserhand), and system toggles the dashboard.
pub const TRIGGER: u32 = 1;
pub const B: u32 = 2;
pub const X: u32 = 4;
pub const A: u32 = 8;
pub const SYSTEM: u32 = 16;
#[allow(dead_code)] // the laser's back button; nothing maps a mouse button to it yet
pub const JOYSTICK: u32 = 32;

/// Hide now, so the driver disconnects without waiting for its 300 ms watchdog.
pub const HIDE: &str = "H";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hand {
    Left,
    Right,
}

/// One lease datagram. It carries the full state, so a lost packet strands nothing. Pose is in raw space.
pub fn lease(seq: u32, hand: Hand, p: &V3, q: &[f64; 4], buttons: u32, scroll: (f64, f64)) -> String {
    let h = if hand == Hand::Left { 'L' } else { 'R' };
    format!(
        "L {seq} {h} {:.5} {:.5} {:.5} {:.6} {:.6} {:.6} {:.6} {buttons} {:.3} {:.3}",
        p[0], p[1], p[2], q[0], q[1], q[2], q[3], scroll.0, scroll.1
    )
}

/// The hand to lease, or none. It's the non-dominant hand (never the `dominant_hand` setting's),
/// and only while the mouse is in use and no real controller holds it. We tried borrowing a
/// held hand in stage 3, and SteamVR stripped both real controllers' roles.
pub fn pick_hand(mouse_in_use: bool, dominant: Hand, other_hand_free: bool) -> Option<Hand> {
    (mouse_in_use && other_hand_free).then_some(if dominant == Hand::Right { Hand::Left } else { Hand::Right })
}

fn col(m: &Mat, j: usize) -> V3 {
    [0, 1, 2].map(|i| m[i][j] as f64)
}

fn rot(m: &Mat, v: &V3) -> V3 {
    [0, 1, 2].map(|i| (0..3).map(|j| m[i][j] as f64 * v[j]).sum())
}

fn rot_inv(m: &Mat, v: &V3) -> V3 {
    [0, 1, 2].map(|j| (0..3).map(|i| m[i][j] as f64 * v[i]).sum())
}

fn unit(v: &V3) -> V3 {
    let n = norm(v);
    if n > 1e-9 { v.map(|x| x / n) } else { [0.0, 0.0, -1.0] }
}

/// Takes a standing-space direction into raw space using the HMD's pose in both (S standing,
/// R raw): R·S⁻¹·v.
pub fn to_raw_dir(s: &Mat, r: &Mat, v: &V3) -> V3 {
    rot(r, &rot_inv(s, v))
}

/// Takes a standing-space point into raw space through the head: R·S⁻¹·p.
pub fn to_raw_point(s: &Mat, r: &Mat, p: &V3) -> V3 {
    let (sp, rp) = (col(s, 3), col(r, 3));
    let d = to_raw_dir(s, r, &[p[0] - sp[0], p[1] - sp[1], p[2] - sp[2]]);
    [rp[0] + d[0], rp[1] + d[1], rp[2] + d[2]]
}

/// Quaternion (w, x, y, z) of the rotation whose matrix columns are x, y, z (vrmath.h BasisQuat).
pub fn basis_quat(x: &V3, y: &V3, z: &V3) -> [f64; 4] {
    let m = [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]];
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = 0.5 / (trace + 1.0).sqrt();
        [0.25 / s, (m[2][1] - m[1][2]) * s, (m[0][2] - m[2][0]) * s, (m[1][0] - m[0][1]) * s]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = 2.0 * (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt();
        [(m[2][1] - m[1][2]) / s, 0.25 * s, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s]
    } else if m[1][1] > m[2][2] {
        let s = 2.0 * (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt();
        [(m[0][2] - m[2][0]) / s, (m[0][1] + m[1][0]) / s, 0.25 * s, (m[1][2] + m[2][1]) / s]
    } else {
        let s = 2.0 * (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt();
        [(m[1][0] - m[0][1]) / s, (m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, 0.25 * s]
    }
}

/// A device basis pointing along aim with no roll: -z along aim, x horizontal (AimBasis).
/// aim can't be vertical, and kvm's pitch limit makes sure it isn't.
pub fn aim_basis(aim: &V3) -> (V3, V3, V3) {
    let z = unit(&aim.map(|v| -v));
    let x = unit(&cross(&[0.0, 1.0, 0.0], &z));
    (x, cross(&z, &x), z)
}

/// The leased pose in raw space for a laser from origin along aim (both standing). The basis
/// is built level in standing space and then carried over, so the laser doesn't roll.
pub fn raw_pose(s: &Mat, r: &Mat, origin: &V3, aim: &V3) -> (V3, [f64; 4]) {
    let (x, y, z) = aim_basis(aim);
    let q = basis_quat(&to_raw_dir(s, r, &x), &to_raw_dir(s, r, &y), &to_raw_dir(s, r, &z));
    (to_raw_point(s, r, origin), q)
}

/// Where the laser starts, on the line from the eye to the cursor. On our panel it starts just
/// short of the cursor, so the beam is a stub at the dot. Elsewhere SteamVR does the hit test,
/// so it starts near the eye.
pub fn laser_origin(eye: &V3, cursor: &V3, on_our_panel: bool) -> V3 {
    let v = [cursor[0] - eye[0], cursor[1] - eye[1], cursor[2] - eye[2]];
    let d = norm(&v);
    let t = if on_our_panel { (0.95 * d).min(d - 0.15).max(0.0) } else { 0.25 };
    let u = unit(&v);
    [eye[0] + u[0] * t, eye[1] + u[1] * t, eye[2] + u[2] * t]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Off,
    Claiming,
    Healthy,
    /// Not leasing, so the dot is amber. Our panels still click as usual (R-1).
    Degraded,
}

/// What the caller has to do this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Do {
    /// Press `a` for one latched lease frame: switchlaserhand claims the laser for our device.
    PulseA,
    /// The claim failed. Look for "Too many binding loads" in vrserver.txt, then call `exhausted`.
    ScanLog,
}

const SHOW_TO_PULSE: Duration = Duration::from_millis(600); // 300 ms was too soon on the Frame; the first claim failed
const PULSE_TO_CHECK: Duration = Duration::from_millis(800);
/// Each connect is a binding load, and SteamVR stops loading them past a budget we don't know.
const CONNECT_EVERY: Duration = Duration::from_secs(10);

/// Off → Claiming → Healthy | Degraded. Lease while `leasing()`.
#[derive(Debug)]
pub struct Laser {
    pub state: State,
    due: Instant,
    pulses: u8,
    last_connect: Option<Instant>,
    /// The binding budget is spent for this vrserver, and only restarting it clears that.
    exhausted_pid: Option<u32>,
}

impl Laser {
    pub fn new(now: Instant) -> Self {
        Laser { state: State::Off, due: now, pulses: 0, last_connect: None, exhausted_pid: None }
    }

    pub fn leasing(&self) -> bool {
        matches!(self.state, State::Claiming | State::Healthy)
    }

    /// The mouse woke up off our panels. Start leasing, unless that would spend a binding load
    /// we can't afford.
    pub fn wake(&mut self, now: Instant, vrserver_pid: u32) {
        if self.state == State::Off {
            self.connect(now, vrserver_pid);
        }
    }

    /// Claim, or Degraded if that would spend a binding load we can't afford.
    fn connect(&mut self, now: Instant, vrserver_pid: u32) {
        if self.exhausted_pid.is_some_and(|p| p != vrserver_pid) {
            self.exhausted_pid = None;
        }
        if self.exhausted_pid.is_some() || self.last_connect.is_some_and(|t| now < t + CONNECT_EVERY) {
            self.state = State::Degraded;
            return;
        }
        self.last_connect = Some(now);
        (self.state, self.due, self.pulses) = (State::Claiming, now + SHOW_TO_PULSE, 0);
    }

    /// The mouse went idle or we're stopping, so give the hand back (send HIDE).
    pub fn sleep(&mut self) {
        self.state = State::Off;
    }

    /// `primary_is_ours`: GetPrimaryDashboardDevice() is our device. Only read when a check is due.
    /// Degraded retries as soon as the rate limit allows, so it doesn't have to wait for the mouse to idle.
    pub fn tick(&mut self, now: Instant, vrserver_pid: u32, primary_is_ours: impl FnOnce() -> bool) -> Option<Do> {
        if self.state == State::Degraded {
            self.connect(now, vrserver_pid);
        }
        if self.state != State::Claiming || now < self.due {
            return None;
        }
        if self.pulses > 0 && primary_is_ours() {
            self.state = State::Healthy;
            return None;
        }
        if self.pulses < 2 {
            self.pulses += 1;
            self.due = now + PULSE_TO_CHECK;
            return Some(Do::PulseA);
        }
        self.state = State::Degraded;
        Some(Do::ScanLog)
    }

    /// The log scan found "Too many binding loads", so stay Degraded until vrserver restarts.
    pub fn exhausted(&mut self, vrserver_pid: u32) {
        self.exhausted_pid = Some(vrserver_pid);
    }
}

// ------------------------------------------------------------------ live: the link and the driver

use crate::{call, vr};
use openvr_sys as sys;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Condvar, LazyLock, Mutex};

/// The laser has the dashboard pointer, so kvm can send clicks off our panels through it.
pub static HEALTHY: AtomicBool = AtomicBool::new(false);

/// What the lease thread sends: the latest pose and buttons from the main loop.
struct Snap {
    hand: Option<Hand>,
    pose: (V3, [f64; 4]),
    at: Instant,
    held: u32,
    pulse: (u32, Instant), // bits held down until then (a, system), long enough for the driver to latch them
    scroll: (f64, Instant),
}

/// A hand is wanted (Beam::tick). The lease thread parks while none is, so this starts it again.
static WANTED: Condvar = Condvar::new();

static SNAP: LazyLock<Mutex<Snap>> = LazyLock::new(|| {
    let now = Instant::now();
    Mutex::new(Snap { hand: None, pose: ([0.0; 3], [1.0, 0.0, 0.0, 0.0]), at: now, held: 0, pulse: (0, now), scroll: (0.0, now) })
});

fn send(msg: &str) {
    if let (Ok(s), Ok(a)) = (UnixDatagram::unbound(), SocketAddr::from_abstract_name(b"cc_pointer")) {
        let _ = s.send_to_addr(msg.as_bytes(), &a);
    }
}

/// Gives the hand back now (startup, exit, panic), so the driver doesn't wait for its watchdog.
pub fn hide() {
    SNAP.lock().unwrap_or_else(|e| e.into_inner()).hand = None;
    send(HIDE);
}

pub fn press(bit: u32, down: bool) {
    let mut s = SNAP.lock().unwrap();
    if down { s.held |= bit } else { s.held &= !bit }
}

pub fn pulse(bit: u32) {
    SNAP.lock().unwrap().pulse = (bit, Instant::now() + Duration::from_millis(60));
}

/// A wheel notch: the joystick held over for 80 ms, which SteamVR turns into a scroll step.
pub fn scroll(dir: f64) {
    SNAP.lock().unwrap().scroll = (dir.signum(), Instant::now() + Duration::from_millis(80));
}

/// Runs on its own thread, every 20 ms while leasing, so a slow frame in the main loop can't trip
/// the driver's 300 ms watchdog (each reconnect spends a binding load). If the main loop is stuck
/// for 1 s, it stops. With no hand wanted (HIDE sent) it sleeps until one is (WANTED; 1 s for QUIT).
pub fn lease_thread() {
    let (mut seq, mut sent) = (0u32, false);
    while !crate::QUIT.load(Relaxed) {
        let msg = {
            let s = SNAP.lock().unwrap();
            let now = Instant::now();
            s.hand.filter(|_| s.at.elapsed() < Duration::from_secs(1)).map(|h| {
                let buttons = s.held | if now < s.pulse.1 { s.pulse.0 } else { 0 };
                let sy = if now < s.scroll.1 { s.scroll.0 } else { 0.0 };
                lease(seq, h, &s.pose.0, &s.pose.1, buttons, (0.0, sy))
            })
        };
        match msg {
            Some(m) => {
                seq = seq.wrapping_add(1);
                send(&m);
                sent = true;
            }
            None if sent => {
                send(HIDE);
                sent = false;
            }
            None => {}
        }
        let s = SNAP.lock().unwrap();
        if s.hand.is_none() && !sent {
            drop(WANTED.wait_timeout(s, Duration::from_secs(1)));
            continue;
        }
        drop(s);
        std::thread::sleep(Duration::from_millis(20));
    }
    send(HIDE);
}

/// How the cursor dot should look this frame.
#[derive(PartialEq)]
pub enum Dot {
    Normal,
    Hidden, // off our panels with the laser working; SteamVR draws its own dot there
    Amber,  // the laser isn't working, so clicks off our panels go nowhere
}

/// The main loop's side: whether to lease, the claim, and the pose. `None` until SteamVR has a
/// cc_pointer device (the driver isn't installed, or isn't loaded yet).
pub struct Beam {
    laser: Laser,
    ours: Option<u32>,
    pid: u32,
    refreshed: Option<Instant>,
    lasermode: vr::Handle,
}

/// True if the dominant hand is the left (settings.json's `dominant_hand`, read by Beam::new; Preferences sets it live).
static DOMINANT_LEFT: AtomicBool = AtomicBool::new(false);

pub fn dominant() -> Hand {
    if DOMINANT_LEFT.load(Relaxed) { Hand::Left } else { Hand::Right }
}

pub fn set_dominant(h: Hand) {
    DOMINANT_LEFT.store(h == Hand::Left, Relaxed);
}

fn dominant_hand() -> Hand {
    let text = std::fs::read_to_string(crate::config::config("settings.json")).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    match v["dominant_hand"].as_str() {
        None | Some("right") => Hand::Right,
        Some("left") => Hand::Left,
        Some(other) => {
            eprintln!("settings.json: dominant_hand {other:?} isn't left or right; using right");
            Hand::Right
        }
    }
}

fn vrserver_pid() -> u32 {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .find(|e| std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "vrserver"))
        .and_then(|e| e.file_name().to_str()?.parse().ok())
        .unwrap_or(0)
}

/// SteamVR's binding-load budget is spent for this run ("Too many binding loads").
fn bindings_exhausted() -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(format!("{}/.local/share/Steam/logs/vrserver.txt", crate::config::home_dir())) else { return false };
    let len = f.seek(SeekFrom::End(0)).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)));
    let mut tail = String::new();
    let _ = f.read_to_string(&mut tail);
    tail.contains("Too many binding loads")
}

fn device_of_type(kind: &str) -> Option<u32> {
    (1..sys::k_unMaxTrackedDeviceCount as u32).find(|&i| {
        let mut buf = [0 as std::os::raw::c_char; 64];
        let mut err = 0;
        call!(sys, GetStringTrackedDeviceProperty, i, sys::ETrackedDeviceProperty_Prop_ControllerType_String, buf.as_mut_ptr(), buf.len() as u32, &mut err);
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_bytes() == kind.as_bytes()
    })
}

impl Beam {
    pub fn new() -> Beam {
        hide(); // clear whatever an earlier run left
        // Keeps SteamVR's laser mouse on while the dashboard is closed, with a trick: an
        // invisible interactive overlay far below you, only shown while our laser works.
        let lasermode = vr::create_overlay("controlcenter.lasermode", "Command Center laser mode").unwrap_or(0);
        let mut px = [0u8; 4 * 4 * 4];
        call!(ov, SetOverlayRaw, lasermode, px.as_mut_ptr() as *mut _, 4, 4, 4);
        call!(ov, SetOverlayWidthInMeters, lasermode, 0.001);
        call!(ov, SetOverlayFlag, lasermode, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        set_dominant(dominant_hand());
        eprintln!("laser: dominant hand {:?}, the mouse's laser uses the other one while it's free", dominant());
        Beam { laser: Laser::new(Instant::now()), ours: None, pid: 0, refreshed: None, lasermode }
    }

    /// One frame. `cursor` is where the dot is, and `on_ours` means it's on one of our panels.
    pub fn tick(&mut self, mouse_awake: bool, cursor: V3, on_ours: bool) -> Dot {
        let now = Instant::now();
        if self.refreshed.is_none_or(|t| now - t > Duration::from_secs(2)) {
            self.refreshed = Some(now);
            // the /proc scan and 63 device queries cost 20-30 ms of the main loop (live), so only
            // do them when vrserver restarted, or while our driver isn't there yet
            let alive = std::fs::read_to_string(format!("/proc/{}/comm", self.pid)).is_ok_and(|c| c.trim() == "vrserver");
            let pid = if alive { self.pid } else { vrserver_pid() };
            let found = if pid == self.pid && self.ours.is_some() { self.ours } else { device_of_type("cc_pointer") };
            self.pid = pid;
            if found != self.ours {
                eprintln!("laser: {}", if found.is_some() { "cc_pointer is there" } else { "no cc_pointer driver: clicks reach our panels only" });
                self.ours = found;
            }
        }
        let Some(ours) = self.ours else { return Dot::Normal };
        let (head, raw) = (vr::head(), vr::head_raw());
        let dominant = dominant();
        let other = if dominant == Hand::Right { sys::ETrackedControllerRole_TrackedControllerRole_LeftHand } else { sys::ETrackedControllerRole_TrackedControllerRole_RightHand };
        let holder = call!(sys, GetTrackedDeviceIndexForControllerRole, other);
        let free = holder == sys::k_unTrackedDeviceIndexInvalid as u32 || holder == ours;
        let want = pick_hand(mouse_awake && head.is_some() && raw.is_some(), dominant, free);
        if want.is_some() && self.laser.state == State::Off {
            self.laser.wake(now, self.pid);
        } else if want.is_none() && self.laser.state != State::Off {
            self.laser.sleep();
            hide();
        }
        match self.laser.tick(now, self.pid, || call!(ov, GetPrimaryDashboardDevice) == ours) {
            Some(Do::PulseA) => pulse(A),
            Some(Do::ScanLog) => {
                eprintln!("laser: SteamVR didn't give it the dashboard pointer");
                if bindings_exhausted() {
                    eprintln!("laser: SteamVR's binding loads are spent (\"Too many binding loads\"): off until SteamVR restarts");
                    self.laser.exhausted(self.pid);
                }
            }
            None => {}
        }
        let healthy = self.laser.state == State::Healthy;
        HEALTHY.store(healthy, Relaxed);
        if let (Some(hand), true, Some(s), Some(r)) = (want, self.laser.leasing(), head, raw) {
            let eye = [s[0][3] as f64, s[1][3] as f64, s[2][3] as f64];
            let origin = laser_origin(&eye, &cursor, on_ours);
            let aim = [cursor[0] - origin[0], cursor[1] - origin[1], cursor[2] - origin[2]];
            let pose = raw_pose(&s, &r, &origin, &aim);
            let mut snap = SNAP.lock().unwrap();
            if snap.hand.is_none() {
                WANTED.notify_one();
            }
            (snap.hand, snap.pose, snap.at) = (Some(hand), pose, now);
        } else if self.laser.state == State::Degraded {
            hide();
        }
        if healthy {
            let below = [[1.0, 0.0, 0.0, eye_x(head)], [0.0, 1.0, 0.0, eye_y(head) - 50.0], [0.0, 0.0, 1.0, eye_z(head)]];
            vr::place(self.lasermode, &below);
            call!(ov, ShowOverlay, self.lasermode);
        } else {
            call!(ov, HideOverlay, self.lasermode);
        }
        match self.laser.state {
            State::Healthy if !on_ours => Dot::Hidden,
            State::Degraded if mouse_awake => Dot::Amber,
            _ => Dot::Normal,
        }
    }
}

fn eye_x(h: Option<Mat>) -> f32 {
    h.map_or(0.0, |m| m[0][3])
}
fn eye_y(h: Option<Mat>) -> f32 {
    h.map_or(1.6, |m| m[1][3])
}
fn eye_z(h: Option<Mat>) -> f32 {
    h.map_or(0.0, |m| m[2][3])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &V3, b: &V3) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    /// A pose matrix: yaw (radians, about +y) then a position.
    fn pose(yaw: f64, p: V3) -> Mat {
        let (c, s) = (yaw.cos(), yaw.sin());
        let r = [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]];
        [0, 1, 2].map(|i| [r[i][0] as f32, r[i][1] as f32, r[i][2] as f32, p[i] as f32])
    }

    fn mul(a: &Mat, b: &Mat) -> Mat {
        let mut m = [[0f32; 4]; 3];
        for i in 0..3 {
            for j in 0..4 {
                m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum::<f32>() + if j == 3 { a[i][3] } else { 0.0 };
            }
        }
        m
    }

    fn apply(m: &Mat, p: &V3) -> V3 {
        let r = rot(m, p);
        [r[0] + m[0][3] as f64, r[1] + m[1][3] as f64, r[2] + m[2][3] as f64]
    }

    /// Rotates v by the quaternion (w, x, y, z).
    fn qrot(q: &[f64; 4], v: &V3) -> V3 {
        let (w, u) = (q[0], [q[1], q[2], q[3]]);
        let t = cross(&u, v).map(|x| 2.0 * x);
        let ut = cross(&u, &t);
        [0, 1, 2].map(|i| v[i] + w * t[i] + ut[i])
    }

    #[test]
    fn standing_to_raw_matches_the_universe_with_a_floor_offset() {
        // Raw is standing turned 40 degrees and dropped 1.6 m (no floor calibration).
        let u = pose(40f64.to_radians(), [0.3, -1.6, 0.2]);
        let s = pose(-25f64.to_radians(), [0.1, 1.6, -0.4]);
        let r = mul(&u, &s);
        assert!(close(&to_raw_point(&s, &r, &col(&s, 3)), &col(&r, 3)));
        for p in [[0.0, 0.0, 0.0], [1.0, 1.2, -2.0], [-0.5, 1.7, 0.8]] {
            assert!(close(&to_raw_point(&s, &r, &p), &apply(&u, &p)), "{p:?}");
            assert!(close(&to_raw_dir(&s, &r, &p), &rot(&u, &p)), "{p:?}");
        }
    }

    #[test]
    fn raw_pose_points_minus_z_along_the_aim_without_roll() {
        let u = pose(1.1, [0.0, -1.6, 0.0]);
        let s = pose(0.4, [0.0, 1.6, 0.0]);
        let r = mul(&u, &s);
        let aim = [0.3, -0.2, -1.0];
        let (o, q) = raw_pose(&s, &r, &[0.0, 1.5, -0.25], &aim);
        assert!(close(&o, &apply(&u, &[0.0, 1.5, -0.25])));
        assert!((q.iter().map(|x| x * x).sum::<f64>() - 1.0).abs() < 1e-6); // f32 poses
        assert!(close(&qrot(&q, &[0.0, 0.0, -1.0]), &rot(&u, &unit(&aim))));
        // No roll: the device's x stays level in standing, so it's level in raw too (u is a yaw).
        assert!(qrot(&q, &[1.0, 0.0, 0.0])[1].abs() < 1e-6);
    }

    #[test]
    fn basis_quat_covers_every_branch() {
        let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        // Identity, then half turns about x, y, z (trace -1 hits each of the other branches).
        for (i, a) in [None, Some(0), Some(1), Some(2)].into_iter().enumerate() {
            let b = axes.map(|v| match a {
                None => v,
                Some(k) => [0, 1, 2].map(|j| if j == k { v[j] } else { -v[j] }),
            });
            let q = basis_quat(&b[0], &b[1], &b[2]);
            for k in 0..3 {
                assert!(close(&qrot(&q, &axes[k]), &b[k]), "case {i} axis {k}: {q:?}");
            }
        }
        for aim in [[0.2, 0.1, -1.0], [-1.0, 0.4, 0.3], [0.0, -0.3, 1.0]] {
            let (x, y, z) = aim_basis(&aim);
            let q = basis_quat(&x, &y, &z);
            assert!(close(&qrot(&q, &[0.0, 0.0, -1.0]), &unit(&aim)));
            assert!(close(&qrot(&q, &[0.0, 1.0, 0.0]), &y));
        }
    }

    #[test]
    fn laser_origin_rule() {
        let eye = [0.0, 1.6, 0.0];
        let at = |d: f64, ours| norm(&{
            let o = laser_origin(&eye, &[0.0, 1.6, -d], ours);
            [o[0] - eye[0], o[1] - eye[1], o[2] - eye[2]]
        });
        assert!((at(2.0, true) - 1.85).abs() < 1e-9); // d - 0.15
        assert!((at(1.0, true) - 0.85).abs() < 1e-9);
        assert!((at(0.2, true) - 0.05).abs() < 1e-9);
        assert!((at(2.0, false) - 0.25).abs() < 1e-9);
        assert!((at(4.0, true) - 3.8).abs() < 1e-9); // 0.95 d, past 3 m
        assert_eq!(at(0.1, true), 0.0); // closer than 0.15, so from the eye
    }

    #[test]
    fn lease_format() {
        let m = lease(7, Hand::Right, &[0.1, -1.25, 0.333333], &[1.0, 0.0, -0.5, 0.25], TRIGGER | A, (0.0, -1.0));
        assert_eq!(m, "L 7 R 0.10000 -1.25000 0.33333 1.000000 0.000000 -0.500000 0.250000 9 0.000 -1.000");
        assert!(lease(0, Hand::Left, &[0.0; 3], &[1.0, 0.0, 0.0, 0.0], 0, (0.0, 0.0)).starts_with("L 0 L "));
        assert_eq!([TRIGGER, B, X, A, SYSTEM, JOYSTICK], [1, 2, 4, 8, 16, 32]);
    }

    #[test]
    fn hand_pick_takes_only_the_free_non_dominant_hand() {
        use Hand::*;
        // Idle mouse: never a lease, even with both hands free.
        assert_eq!(pick_hand(false, Right, true), None);
        assert_eq!(pick_hand(false, Left, true), None);
        // Never the dominant hand.
        assert_eq!(pick_hand(true, Right, true), Some(Left));
        assert_eq!(pick_hand(true, Left, true), Some(Right));
        // Never a hand a real controller holds.
        assert_eq!(pick_hand(true, Right, false), None);
    }

    #[allow(non_snake_case)]
    fn MS(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn claim_ok_is_healthy() {
        let t0 = Instant::now();
        let mut l = Laser::new(t0);
        l.wake(t0, 1);
        assert!(l.leasing() && l.state == State::Claiming);
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE - MS(1), 1, || unreachable!()), None);
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE, 1, || unreachable!()), Some(Do::PulseA));
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE + PULSE_TO_CHECK - MS(1), 1, || unreachable!()), None);
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE + PULSE_TO_CHECK, 1, || true), None);
        assert_eq!(l.state, State::Healthy);
        assert_eq!(l.tick(t0 + MS(5000), 1, || unreachable!()), None);
        l.sleep();
        assert!(!l.leasing());
    }

    #[test]
    fn claim_retried_once_then_degraded() {
        let t0 = Instant::now();
        let mut l = Laser::new(t0);
        l.wake(t0, 1);
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE, 1, || false), Some(Do::PulseA));
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE + PULSE_TO_CHECK, 1, || false), Some(Do::PulseA));
        assert_eq!(l.state, State::Claiming);
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE + PULSE_TO_CHECK * 2, 1, || false), Some(Do::ScanLog));
        assert_eq!(l.state, State::Degraded);
        assert!(!l.leasing());
        assert_eq!(l.tick(t0 + SHOW_TO_PULSE + PULSE_TO_CHECK * 2 + MS(900), 1, || true), None);
        assert_eq!(l.state, State::Degraded);
        // The retry can still succeed.
        let mut l = Laser::new(t0);
        l.wake(t0, 1);
        l.tick(t0 + SHOW_TO_PULSE, 1, || false);
        l.tick(t0 + SHOW_TO_PULSE + PULSE_TO_CHECK, 1, || false);
        l.tick(t0 + SHOW_TO_PULSE + PULSE_TO_CHECK * 2, 1, || true);
        assert_eq!(l.state, State::Healthy);
    }

    #[test]
    fn one_connect_per_10_s() {
        let t0 = Instant::now();
        let mut l = Laser::new(t0);
        l.wake(t0, 1);
        l.sleep();
        l.wake(t0 + MS(9999), 1);
        assert_eq!(l.state, State::Degraded);
        l.sleep();
        l.wake(t0 + MS(10_000), 1);
        assert_eq!(l.state, State::Claiming);
        // A wake while leasing isn't a new connect.
        l.wake(t0 + MS(15_000), 1);
        l.sleep();
        l.wake(t0 + MS(19_999), 1);
        assert_eq!(l.state, State::Degraded);
    }

    #[test]
    fn degraded_retries_without_a_sleep() {
        let t0 = Instant::now();
        let mut l = Laser::new(t0);
        l.wake(t0, 1);
        l.sleep();
        l.wake(t0 + MS(5000), 1);
        assert_eq!(l.state, State::Degraded);
        assert_eq!(l.tick(t0 + MS(9999), 1, || unreachable!()), None);
        assert_eq!(l.state, State::Degraded);
        assert_eq!(l.tick(t0 + MS(10_000), 1, || unreachable!()), None);
        assert_eq!(l.state, State::Claiming);
        // Spent loads stick, even through tick, until the pid changes.
        l.exhausted(1);
        l.state = State::Degraded;
        assert_eq!(l.tick(t0 + MS(60_000), 1, || unreachable!()), None);
        assert_eq!(l.state, State::Degraded);
        l.tick(t0 + MS(60_000), 2, || unreachable!());
        assert_eq!(l.state, State::Claiming);
    }

    #[test]
    fn spent_binding_loads_hold_until_vrserver_restarts() {
        let t0 = Instant::now();
        let mut l = Laser::new(t0);
        l.wake(t0, 1);
        l.exhausted(1);
        l.sleep();
        l.wake(t0 + MS(60_000), 1);
        assert_eq!(l.state, State::Degraded);
        l.sleep();
        l.wake(t0 + MS(120_000), 2);
        assert_eq!(l.state, State::Claiming);
    }
}
