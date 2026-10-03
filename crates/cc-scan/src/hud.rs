//! The scan's guidance in the headset. It's drawn here and shown by cc-panels, like scan.py's Hud,
//! and it's my design. There are green outlines on the tags being read, in the room where they are
//! (`hud mark` places a sheet at the eye pose of the frame they were read in, at their depth). There's
//! also a status strip (`hud`) with the step and "n tags read" or "slower". A laser click on its
//! button skips the step.
//! There's no camera picture, because the mirror sees the overlays too and would read a picture of
//! tags again. It stays calm: the status changes at most every 0.6 s, and it only uses green and
//! amber, no red, because of photosensitive epilepsy.
use crate::panels;
use nalgebra::{Matrix4, Vector3};
use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static FONT_BYTES: &[u8] = include_bytes!("../../../third_party/fonts/AtkinsonHyperlegibleMono[wght].ttf");

pub const STEADY: f64 = 0.6;
pub const HOLD: f64 = 0.6;
pub const SKIP_BUTTON: (usize, usize, usize, usize) = (700, 54, 890, 94);
const STRIP: (usize, usize) = (900, 100);
const SHEET: (usize, usize) = (960, 540);

/// What the scan tells the HUD. The HUD tells the scan one thing back: skip.
pub struct State {
    pub text: String,
    pub want: HashSet<usize>,
    pub sizes: HashMap<usize, f64>,
    pub button: String,
    /// The camera refit's board, as its `hud mark` arguments.
    pub board: Option<String>,
    pub skip: bool,
    pub on: bool,
    /// The last shot, for its outlines: (still, head pose, tags (id, corners px)). The HUD takes it.
    pub shot: Option<(bool, Option<panels::Pose>, Vec<(usize, [(f32, f32); 4])>)>,
    pub still: bool,
}

impl Default for State {
    fn default() -> State {
        State { text: String::new(), want: HashSet::new(), sizes: HashMap::new(), button: "skip step".into(), board: None, skip: false, on: true, shot: None, still: false }
    }
}

struct Rgba {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Rgba {
    fn new(w: usize, h: usize, fill: [u8; 4]) -> Rgba {
        Rgba { w, h, px: fill.repeat(w * h) }
    }

    fn rect(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 4], filled: bool) {
        for y in y0..=y1.min(self.h - 1) {
            for x in x0..=x1.min(self.w - 1) {
                if filled || y == y0 || y == y1 || x == x0 || x == x1 {
                    self.px[(y * self.w + x) * 4..(y * self.w + x) * 4 + 4].copy_from_slice(&c);
                }
            }
        }
    }

    fn blend(&mut self, x: i64, y: i64, c: [u8; 4], a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        for k in 0..3 {
            self.px[i + k] = (c[k] as f32 * a + self.px[i + k] as f32 * (1.0 - a)) as u8;
        }
        self.px[i + 3] = (c[3] as f32 * a + self.px[i + 3] as f32 * (1.0 - a)).max(self.px[i + 3] as f32) as u8;
    }

    /// Draws text with its baseline at (x, y), `size` px, shrunk to fit `max_w`.
    fn text(&mut self, font: &fontdue::Font, s: &str, x: f32, y: f32, size: f32, max_w: f32, c: [u8; 4]) {
        let width = |sz: f32| s.chars().map(|ch| font.metrics(ch, sz).advance_width).sum::<f32>();
        let size = if width(size) > max_w { size * max_w / width(size) } else { size };
        let mut pen = x;
        for ch in s.chars() {
            let (m, bm) = font.rasterize(ch, size);
            let (gx, gy) = (pen.round() as i64 + m.xmin as i64, (y - m.height as f32 - m.ymin as f32).round() as i64);
            for r in 0..m.height {
                for col in 0..m.width {
                    let a = bm[r * m.width + col] as f32 / 255.0;
                    if a > 0.0 {
                        self.blend(gx + col as i64, gy + r as i64, c, a);
                    }
                }
            }
            pen += m.advance_width;
        }
    }

    /// Draws a thin line, anti-aliased by its distance to each pixel.
    fn line(&mut self, a: (f32, f32), b: (f32, f32), c: [u8; 4]) {
        let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) * 2.0).ceil().max(1.0) as usize;
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            let (fx, fy) = (x.floor(), y.floor());
            for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let w = (1.0 - (x - fx - dx).abs()) * (1.0 - (y - fy - dy).abs());
                if w > 0.0 {
                    self.blend((fx + dx) as i64, (fy + dy) as i64, c, w.min(1.0));
                }
            }
        }
    }

    /// Writes raw RGBA in one go, since cc-panels reads it all at once.
    fn write(&self, path: &PathBuf) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &self.px)?;
        std::fs::rename(tmp, path)
    }
}

pub struct Hud {
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

impl Hud {
    /// Starts the HUD's thread, which runs until stopped. camera is [fx, fy, cx, cy, rvec x3, eye offset x3]. With none, there are no outlines.
    pub fn start(folder: PathBuf, camera: Option<Vec<f64>>) -> Hud {
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp) = (state.clone(), stop.clone());
        std::thread::spawn(move || {
            let Ok(sock) = panels::socket() else { return };
            let _ = sock.set_nonblocking(true);
            let font = fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default()).expect("the bundled font");
            let mut run = Run { sock, font, folder, camera, steady: (false, 0.0), drawn: None, drawn_board: 0.0, seen: HashMap::new() };
            while !sp.load(Relaxed) && st.lock().unwrap().on {
                run.answers(&st);
                run.status(&st);
                let board = st.lock().unwrap().board.clone();
                if let Some(b) = board {
                    if crate::now() - run.drawn_board > 1.0 {
                        run.send(&format!("hud mark {b}")); // Keep it up, because cc-panels hides anything that isn't refreshed for 5 s.
                        run.drawn_board = crate::now();
                    }
                } else {
                    let shot = st.lock().unwrap().shot.take(); // The lock is let go before outline takes it.
                    if let Some(shot) = shot {
                        run.outline(&st, shot);
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        Hud { state, stop }
    }

    pub fn hide(&self) {
        self.stop.store(true, Relaxed);
        if self.state.lock().unwrap().on {
            panels::ask("hud hide", Duration::from_secs(1));
        }
    }
}

struct Run {
    sock: UnixDatagram,
    font: fontdue::Font,
    folder: PathBuf,
    camera: Option<Vec<f64>>,
    steady: (bool, f64),
    drawn: Option<((String, bool, usize, String), f64)>,
    drawn_board: f64,
    seen: HashMap<usize, ([(f32, f32); 4], f64, [f64; 3])>,
}

impl Run {
    /// Reads cc-panels' answers without ever waiting on them. "ok skip" means the button was clicked.
    /// A cc-panels without the HUD turns it off, and the scan goes on without it.
    fn answers(&mut self, st: &Mutex<State>) {
        let mut buf = [0u8; 4096];
        while let Ok(n) = self.sock.recv(&mut buf) {
            let w: Vec<&str> = std::str::from_utf8(&buf[..n]).unwrap_or("").split_whitespace().collect();
            if w.starts_with(&["ok", "skip"]) {
                st.lock().unwrap().skip = true;
            } else if w.first() == Some(&"error") && w.contains(&"unknown") {
                st.lock().unwrap().on = false;
            }
        }
    }

    fn send(&self, cmd: &str) {
        let _ = panels::send(&self.sock, cmd); // If it's busy or not running, the next refresh tries again.
    }

    fn status(&mut self, st: &Mutex<State>) {
        let now = crate::now();
        let (text, want, still, button) = {
            let s = st.lock().unwrap();
            (s.text.clone(), s.want.clone(), s.still, s.button.clone())
        };
        if still != self.steady.0 && now - self.steady.1 >= STEADY {
            self.steady = (still, now);
        }
        let n = self.seen.keys().filter(|i| want.contains(i)).count();
        let key = (text.clone(), self.steady.0, n, button.clone());
        if self.drawn.as_ref().is_some_and(|(k, t)| *k == key && now - t < 2.0) {
            return;
        }
        self.drawn = Some((key, now));
        let (w, h) = STRIP;
        let mut img = Rgba::new(w, h, [8, 14, 24, 225]); // Deep Space, a little see-through.
        img.text(&self.font, &text, 16.0, 40.0, 30.0, 870.0, [232, 236, 255, 255]);
        // With no tags wanted (getting ready, or a message), show only the text. Usable frames read in green, too fast in amber.
        if !want.is_empty() {
            let (line, c) = if self.steady.0 { (format!("{n} tags read"), [140, 220, 120, 255]) } else { ("slower".into(), [240, 190, 60, 255]) };
            img.text(&self.font, &line, 16.0, 82.0, 26.0, 600.0, c);
        }
        let (x0, y0, x1, y1) = SKIP_BUTTON;
        img.rect(x0, y0, x1, y1, [34, 44, 60, 255], true);
        img.rect(x0, y0, x1, y1, [170, 180, 190, 255], false);
        img.text(&self.font, &button, x0 as f32 + 20.0, y1 as f32 - 13.0, 20.0, (x1 - x0) as f32 - 40.0, [225, 230, 235, 255]);
        let path = self.folder.join("hud.rgba");
        if img.write(&path).is_ok() {
            self.send(&format!("hud {} {w} {h} skip={x0},{y0},{x1},{y1}", path.display()));
        }
    }

    /// Outlines the tags this step wants, as read in a still shot. The outlines sit just outside their
    /// edges, in their white margin, so the camera still reads them. They go on a transparent sheet at
    /// the tags' depth, placed where the camera's eye was. Outlines read in the last 0.6 s from (almost)
    /// the same place stay, so they don't blink.
    fn outline(&mut self, st: &Mutex<State>, shot: (bool, Option<panels::Pose>, Vec<(usize, [(f32, f32); 4])>)) {
        let (still, head, tags) = shot;
        let (Some(c), Some(head)) = (self.camera.clone(), head) else { return };
        if !still {
            return;
        }
        let (want, sizes) = {
            let s = st.lock().unwrap();
            (s.want.clone(), s.sizes.clone())
        };
        let now = crate::now();
        let at = [head[0][3], head[1][3], head[2][3]];
        for (id, corners) in tags {
            if want.contains(&id) {
                self.seen.insert(id, (corners, now, at));
            }
        }
        let dist = |a: &[f64; 3], b: &[f64; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        self.seen.retain(|_, v| now - v.1 < HOLD && dist(&v.2, &at) < 0.02);
        let depths: Vec<f64> = self.seen.iter().filter_map(|(i, v)| {
            let side = ((v.0[1].0 - v.0[0].0).powi(2) + (v.0[1].1 - v.0[0].1).powi(2)).sqrt().max(1.0) as f64;
            sizes.get(i).map(|s| c[0] * s / side)
        }).collect();
        let d = if depths.is_empty() { 0.9 } else { crate::solve::median(&depths) };
        let (w, h) = SHEET; // The camera's 1920x1080 at half size.
        let mut img = Rgba::new(w, h, [0, 0, 0, 0]);
        for (pts, _, _) in self.seen.values() {
            let mid = (pts.iter().map(|p| p.0).sum::<f32>() / 4.0, pts.iter().map(|p| p.1).sum::<f32>() / 4.0);
            let ring: Vec<(f32, f32)> = pts.iter().map(|p| ((mid.0 + (p.0 - mid.0) * 1.36) / 2.0, (mid.1 + (p.1 - mid.1) * 1.36) / 2.0)).collect();
            for k in 0..4 {
                img.line(ring[k], ring[(k + 1) % 4], [120, 230, 120, 255]); // Thin, because I wanted thin lines.
            }
        }
        // The sheet is centred on the frame's middle, d ahead of the eye the mirror shows.
        let mut eye = Matrix4::identity();
        eye.fixed_view_mut::<3, 3>(0, 0).copy_from(&panels::rotmat(&Vector3::new(c[4], c[5], c[6])));
        eye.fixed_view_mut::<3, 1>(0, 3).copy_from(&Vector3::new(c[7], c[8], c[9]));
        let mut sheet = Matrix4::identity();
        sheet.fixed_view_mut::<3, 1>(0, 3).copy_from(&Vector3::new((960.0 - c[2]) / c[0] * d, -(540.0 - c[3]) / c[1] * d, -d));
        let mut hm = Matrix4::identity();
        for i in 0..3 {
            for j in 0..4 {
                hm[(i, j)] = head[i][j];
            }
        }
        let m = hm * eye * sheet;
        let path = self.folder.join("mark.rgba");
        if img.write(&path).is_ok() {
            let nums: Vec<String> = (0..3).flat_map(|i| (0..4).map(move |j| (i, j))).map(|(i, j)| format!("{:.5}", m[(i, j)])).collect();
            self.send(&format!("hud mark {} {w} {h} {:.4} {}", path.display(), 1920.0 / c[0] * d, nums.join(" ")));
        }
    }
}

/// The camera refit's board for `hud mark`. It writes the board image as raw RGBA next to the
/// scan's files. place is "<width m> <12 numbers>".
pub fn board_args(folder: &std::path::Path, board: &crate::image::Gray, place: &str) -> std::io::Result<String> {
    let raw = folder.join("board.rgba");
    let px: Vec<u8> = board.px.iter().flat_map(|&g| [g, g, g, 255]).collect();
    std::fs::write(&raw, px)?;
    Ok(format!("{} {} {} {place}", raw.display(), board.w, board.h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;

    /// Runs the HUD against a fake cc-panels on a private socket. It checks that the status strip is
    /// written and sent, that a still shot's wanted tags get outlined on a sheet in the room, and that
    /// a skip gets passed back.
    #[test]
    fn draws_and_sends() {
        let _env = panels::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let name = format!("cc-scan-test-hud-{}", std::process::id());
        unsafe { std::env::set_var("CC_PANELS_SOCKET", &name) };
        let fake = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap()).unwrap();
        fake.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let dir = std::env::temp_dir().join(format!("cc-scan-hud-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cam = vec![1078.0, 1075.0, 958.0, 537.0, 0.0, 0.0, 0.0, -0.032, 0.0, 0.0];
        let hud = Hud::start(dir.clone(), Some(cam));
        {
            let mut s = hud.state.lock().unwrap();
            s.text = "look at desk-wide".into();
            s.want = [3usize].into_iter().collect();
            s.sizes.insert(3, 0.1);
            s.still = true;
            let head = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.6], [0.0, 0.0, 1.0, 0.0]];
            s.shot = Some((true, Some(head), vec![(3, [(900.0, 500.0), (1000.0, 500.0), (1000.0, 600.0), (900.0, 600.0)])]));
        }
        let mut buf = [0u8; 4096];
        let (mut strip, mut mark) = (false, false);
        for _ in 0..10 {
            let Ok((n, from)) = fake.recv_from(&mut buf) else { break };
            let msg = String::from_utf8_lossy(&buf[..n]).into_owned();
            if msg.starts_with("hud ") && msg.contains(" 900 100 skip=700,54,890,94") {
                strip = true;
                let _ = fake.send_to_addr(b"ok skip", &from);
            }
            if msg.starts_with("hud mark ") && msg.contains(" 960 540 ") {
                mark = msg.split_whitespace().count() == 18;
            }
            if strip && mark {
                break;
            }
        }
        assert!(strip && mark, "strip {strip} mark {mark}");
        assert_eq!(std::fs::metadata(dir.join("hud.rgba")).unwrap().len(), 900 * 100 * 4);
        assert_eq!(std::fs::metadata(dir.join("mark.rgba")).unwrap().len(), 960 * 540 * 4);
        std::thread::sleep(Duration::from_millis(500));
        assert!(hud.state.lock().unwrap().skip, "the button's click came back");
        hud.stop.store(true, Relaxed);
        let _ = std::fs::remove_dir_all(dir);
    }
}
