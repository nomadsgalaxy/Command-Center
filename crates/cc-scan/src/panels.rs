//! cc-panels' control socket and the head pose polled from it, like scan.py's ask, head_pose and
//! HeadTrack. The socket is @controlcenter, or CC_PANELS_SOCKET's name. Tests use a private one,
//! because a namespace doesn't hide it.
//! This doesn't use cc-tip, because every run of it is an OpenVR app that spends SteamVR's
//! binding-load budget. align starts from the Desktop, so cc-panels is already there.
use nalgebra::{Matrix3, Rotation3, Vector3};
use std::collections::VecDeque;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub type Pose = [[f64; 4]; 3];

pub fn socket_name() -> String {
    std::env::var("CC_PANELS_SOCKET").unwrap_or_else(|_| "controlcenter".into())
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A datagram socket bound to an abstract name of its own, so the replies come back there.
pub fn socket() -> std::io::Result<UnixDatagram> {
    let name = format!("cc-scan-{}-{}", std::process::id(), NEXT.fetch_add(1, Relaxed));
    UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)
}

pub fn send(s: &UnixDatagram, cmd: &str) -> std::io::Result<usize> {
    s.send_to_addr(cmd.as_bytes(), &SocketAddr::from_abstract_name(socket_name().as_bytes())?)
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Words(Vec<String>),
    /// Nothing's listening there.
    NotRunning,
    /// No answer in time. Its loop can idle slowly.
    Late,
}

/// Sends one request and returns its reply's words.
pub fn ask(cmd: &str, timeout: Duration) -> Reply {
    let Ok(s) = socket() else { return Reply::Late };
    let _ = s.set_read_timeout(Some(timeout));
    match send(&s, cmd) {
        Err(e) if matches!(e.raw_os_error(), Some(libc::ECONNREFUSED) | Some(libc::ENOENT)) => return Reply::NotRunning,
        Err(_) => return Reply::Late,
        Ok(_) => {}
    }
    let mut buf = [0u8; 4096];
    match s.recv(&mut buf) {
        Ok(n) => Reply::Words(String::from_utf8_lossy(&buf[..n]).split_whitespace().map(str::to_owned).collect()),
        Err(_) => Reply::Late,
    }
}

/// Parses "ok <12 numbers> <ms>" as a pose.
fn parse_head(w: &[String]) -> Option<Pose> {
    if w.len() != 14 || w[0] != "ok" {
        return None;
    }
    let v: Vec<f64> = w[1..13].iter().map(|x| x.parse().ok()).collect::<Option<_>>()?;
    Some([[v[0], v[1], v[2], v[3]], [v[4], v[5], v[6], v[7]], [v[8], v[9], v[10], v[11]]])
}

/// The head's pose right now. It's Ok(None) while the head isn't tracked, or if cc-panels answers late three times.
pub fn head_pose() -> Result<Option<Pose>, String> {
    for _ in 0..3 {
        match ask("head", Duration::from_secs(1)) {
            Reply::Late => continue,
            Reply::NotRunning => return Err("the Desktop isn't open: open it first (align runs from it)".into()),
            Reply::Words(w) => {
                if let Some(p) = parse_head(&w) {
                    return Ok(Some(p));
                }
                if w.get(..3).is_some_and(|x| x == ["error", "head", "untracked"]) {
                    return Ok(None);
                }
                return Err(format!("cc-panels answered `head` with: {}", w.join(" ")));
            }
        }
    }
    Ok(None)
}

pub fn mat(p: &Pose) -> (Matrix3<f64>, Vector3<f64>) {
    (Matrix3::new(p[0][0], p[0][1], p[0][2], p[1][0], p[1][1], p[1][2], p[2][0], p[2][1], p[2][2]), Vector3::new(p[0][3], p[1][3], p[2][3]))
}

/// The angle of the rotation a^T b in radians, the same as cv2.Rodrigues' norm.
pub fn turn_angle(a: &Matrix3<f64>, b: &Matrix3<f64>) -> f64 {
    rotvec(&(a.transpose() * b)).norm()
}

/// cv2.Rodrigues from matrix to rotation vector, including its atan2 form and its branch near 180 degrees.
pub fn rotvec(m: &Matrix3<f64>) -> Vector3<f64> {
    let r = Vector3::new(m[(2, 1)] - m[(1, 2)], m[(0, 2)] - m[(2, 0)], m[(1, 0)] - m[(0, 1)]);
    let s = r.norm() * 0.5;
    let c = ((m[(0, 0)] + m[(1, 1)] + m[(2, 2)] - 1.0) * 0.5).clamp(-1.0, 1.0);
    if s < 1e-5 {
        if c > 0.0 {
            return Vector3::zeros();
        }
        let t = |i: usize| ((m[(i, i)] + 1.0) * 0.5).max(0.0).sqrt();
        let (rx, ry, mut rz) = (t(0), t(1) * if m[(0, 1)] < 0.0 { -1.0 } else { 1.0 }, t(2) * if m[(0, 2)] < 0.0 { -1.0 } else { 1.0 });
        if rx.abs() < ry.abs() && rx.abs() < rz.abs() && (m[(1, 2)] > 0.0) != (ry * rz > 0.0) {
            rz = -rz;
        }
        let v = Vector3::new(rx, ry, rz);
        return v * (c.acos() / v.norm());
    }
    r * (s.atan2(c) / (2.0 * s))
}

/// cv2.Rodrigues from rotation vector to matrix.
pub fn rotmat(v: &Vector3<f64>) -> Matrix3<f64> {
    Rotation3::new(*v).into_inner()
}

/// Blends two poses at f, then makes the rotation orthonormal again with the SVD (u vt).
pub fn blend(a: &Pose, b: &Pose, f: f64) -> Pose {
    let (ra, ta) = mat(a);
    let (rb, tb) = mat(b);
    let m = ra * (1.0 - f) + rb * f;
    let svd = m.svd(true, true);
    let r = svd.u.unwrap() * svd.v_t.unwrap();
    let t = ta * (1.0 - f) + tb * f;
    let mut out = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = r[(i, j)];
        }
        out[i][3] = t[i];
    }
    out
}

/// The head's pose, polled from cc-panels ~60 times a second and kept for the whole scan so it can
/// be read at any moment in between. That way a frame taken while the head moves gets the pose from
/// its own time.
pub struct HeadTrack {
    pub poses: Arc<Mutex<VecDeque<(f64, Pose)>>>,
    pub dead: Arc<AtomicBool>,
}

impl HeadTrack {
    pub fn start() -> HeadTrack {
        let poses = Arc::new(Mutex::new(VecDeque::with_capacity(20000)));
        let dead = Arc::new(AtomicBool::new(false));
        let (p, d) = (poses.clone(), dead.clone());
        std::thread::spawn(move || {
            let Ok(s) = socket() else {
                d.store(true, Relaxed);
                return;
            };
            let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buf = [0u8; 4096];
            loop {
                let t0 = crate::now();
                match send(&s, "head") {
                    Err(e) if matches!(e.raw_os_error(), Some(libc::ECONNREFUSED) | Some(libc::ENOENT)) => {
                        d.store(true, Relaxed); // No cc-panels, and the scan says so.
                        return;
                    }
                    _ => {}
                }
                let got = s.recv(&mut buf).ok().map(|n| String::from_utf8_lossy(&buf[..n]).split_whitespace().map(str::to_owned).collect::<Vec<_>>());
                let t1 = crate::now();
                if let Some(pose) = got.as_deref().and_then(parse_head) {
                    let mut q = p.lock().unwrap();
                    if q.len() == 20000 {
                        q.pop_front();
                    }
                    q.push_back(((t0 + t1) / 2.0, pose));
                }
                std::thread::sleep(Duration::from_secs_f64((0.016 - (t1 - t0)).max(0.0)));
            }
        });
        HeadTrack { poses, dead }
    }

    /// Returns (pose, turn deg/s, move m/s) at monotonic time t, interpolated. It's None if t isn't
    /// between two polls, because the head was untracked or it hasn't been polled yet.
    pub fn at(&self, t: f64) -> Option<(Pose, f64, f64)> {
        at(&self.poses.lock().unwrap(), t)
    }
}

pub fn at(p: &VecDeque<(f64, Pose)>, t: f64) -> Option<(Pose, f64, f64)> {
    let i = (0..p.len().saturating_sub(1)).find(|&k| p[k].0 <= t && t <= p[k + 1].0)?;
    let ((ta, a), (tb, b)) = (p[i], p[i + 1]);
    let dt = (tb - ta).max(1e-6);
    let f = (t - ta) / dt;
    let (ra, xa) = mat(&a);
    let (rb, xb) = mat(&b);
    Some((blend(&a, &b, f), turn_angle(&ra, &rb).to_degrees() / dt, (xb - xa).norm() / dt))
}

/// Tests that set CC_PANELS_SOCKET hold this, since it's process-wide.
#[cfg(test)]
pub static TEST_ENV: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_a_turn() {
        let rot = |deg: f64| {
            let r = Rotation3::from_axis_angle(&Vector3::y_axis(), deg.to_radians());
            let m = r.matrix();
            [[m[(0, 0)], m[(0, 1)], m[(0, 2)], 0.0], [m[(1, 0)], m[(1, 1)], m[(1, 2)], 1.6], [m[(2, 0)], m[(2, 1)], m[(2, 2)], 0.1 * deg]]
        };
        let q: VecDeque<_> = [(0.0, rot(0.0)), (0.1, rot(2.0))].into_iter().collect();
        let (pose, turn, mv) = at(&q, 0.05).unwrap();
        assert!((turn - 20.0).abs() < 1e-6, "{turn}");
        assert!((mv - 2.0).abs() < 1e-6, "{mv}");
        let (r, _) = mat(&pose);
        let (r1, _) = mat(&rot(1.0));
        assert!(turn_angle(&r, &r1) < 1e-4, "{r} {r1} {}", turn_angle(&r, &r1)); // Halfway, so ~1 degree.
        assert!(at(&q, 0.2).is_none());
        // A private socket name that nothing serves reads as not running.
        let _env = TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("CC_PANELS_SOCKET", format!("cc-scan-test-nobody-{}", std::process::id())) };
        assert_eq!(ask("head", Duration::from_millis(100)), Reply::NotRunning);
    }
}
