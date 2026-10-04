//! A camera scan session, like scan.py serve but as a library for cc-home. It reads the mirror,
//! tracks the head and puts the HUD up. It takes shots on request and keeps them with what they saw
//! (shots.json, with the head track next to it for the fit). It also reads the pairing key just by
//! looking at it.
use crate::camera::Camera;
use crate::hud::{self, Hud};
use crate::lag::Lag;
use crate::panels::{self, HeadTrack};
use crate::solve::round;
use crate::{Tag, detect, dict, read_addr, read_key};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A frame is usable while the head moves slower than this. I wanted one instruction, "move your
/// head slowly". The frame's pose is the one at the frame's time.
pub const MAX_TURN: f64 = 12.0; // deg/s. align can set a different one with gate.
pub const MAX_MOVE: f64 = 0.25; // m/s

/// What a request for shots came back with, like scan.py's "ok <x,y,z> <skip|-> <ids>".
#[derive(Debug)]
pub struct Answer {
    /// Where the head was in the last usable shot, in metres.
    pub at: Option<[f64; 3]>,
    /// Whether the HUD's skip button was clicked since the last answer.
    pub skip: bool,
    /// The tag ids read in usable shots.
    pub found: Vec<usize>,
}

pub struct Scan {
    cam: Camera,
    head: HeadTrack,
    lag: Lag,
    hud: Hud,
    path: PathBuf,
    shots: Vec<Value>,
    monitor: Option<String>,
    max_turn: f64,
}

/// Moves a previous scan's recordings (shots, head track, frames) into <stem>-<time>/ before a new
/// one can overwrite them. In one review: the 23:44 align overwrote the head poses a fit was checked on.
pub fn archive(path: &Path) {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("shots").to_owned();
    let dir = path.parent().unwrap_or(Path::new("."));
    if !std::fs::metadata(path).is_ok_and(|m| m.len() > 2) {
        return; // Nothing there, or "[]".
    }
    let t = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let to = dir.join(format!("{stem}-{t}"));
    if std::fs::create_dir_all(&to).is_err() {
        return;
    }
    let ours = |name: &str| name == format!("{stem}.json") || name == format!("{stem}-head.json")
        || name.strip_prefix(&format!("{stem}-")).and_then(|r| r.strip_suffix(".jpg")).is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()));
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ours(&name) {
            let _ = std::fs::rename(e.path(), to.join(&name));
        }
    }
}

fn tags_json(tags: &[Tag]) -> Value {
    Value::Array(tags.iter().map(|t| json!({"id": t.id, "corners": t.corners.iter().map(|c| [round(c.0 as f64, 2), round(c.1 as f64, 2)]).collect::<Vec<_>>(), "side_px": round(t.side_px as f64, 1)})).collect())
}

impl Scan {
    /// Opens a session that writes its shots to `path` (shots.json). The head track goes to
    /// <path>-head.json, and the usable frames' JPEGs go next to them. conf is ~/.config/control-center,
    /// for the camera and the lag. It fails if the Desktop isn't open (cc-panels gives the head pose) or
    /// the mirror can't be read.
    pub fn open(path: &Path, conf: &Path) -> Result<Scan, String> {
        panels::head_pose()?;
        archive(path);
        let cam = Camera::open()?;
        let camera = std::fs::read(conf.join("mirror-camera.json")).ok().and_then(|b| serde_json::from_slice::<Vec<f64>>(&b).ok()).filter(|c| c.len() == 10);
        let folder = path.parent().unwrap_or(Path::new(".")).to_owned();
        Ok(Scan { cam, head: HeadTrack::start(), lag: Lag::new(conf), hud: Hud::start(folder, camera), path: path.to_owned(), shots: vec![], monitor: None, max_turn: MAX_TURN })
    }

    /// Sets the step's text in the headset.
    pub fn say(&self, text: &str) {
        self.hud.state.lock().unwrap().text = text.to_owned();
    }

    /// Sets the tag ids the step wants. They get outlined green as they're read.
    pub fn want(&self, ids: impl IntoIterator<Item = usize>) {
        self.hud.state.lock().unwrap().want = ids.into_iter().collect();
    }

    /// Sets the tags' sides in metres, for the outlines' depth.
    pub fn sizes(&self, sides: impl IntoIterator<Item = (usize, f64)>) {
        self.hud.state.lock().unwrap().sizes.extend(sides);
    }

    /// Sets the HUD button's label. Pairing uses cancel.
    pub fn button(&self, label: &str) {
        self.hud.state.lock().unwrap().button = label.to_owned();
    }

    /// Shows the camera refit's tag board in the room, from the image and "<width m> <12 numbers>".
    pub fn board(&self, img: &crate::image::Gray, place: &str) -> Result<(), String> {
        let args = hud::board_args(self.path.parent().unwrap_or(Path::new(".")), img, place).map_err(|e| e.to_string())?;
        self.hud.state.lock().unwrap().board = Some(args);
        Ok(())
    }

    /// Sets how slowly the head has to turn for a frame to count, in deg/s. It depends on the align mode.
    pub fn gate(&mut self, deg_s: f64) {
        self.max_turn = deg_s;
    }

    /// Sets the monitor being scanned, so its shots are only its own. None means any.
    pub fn monitor(&mut self, name: Option<&str>) {
        self.monitor = name.map(str::to_owned);
    }

    /// Takes one frame and the head pose it was seen from, like scan.py's shot. The pose is the one at
    /// the frame's time minus the mirror's lag, and the frame is usable while the head moves slowly.
    fn shot(&mut self, d: &dict::Dict) -> Result<(Vec<u8>, Value, Vec<Tag>), String> {
        let (gray, rgb, taken) = self.cam.frame_after(crate::now())?;
        self.lag.add(taken, &gray, &self.head);
        let tags = detect(&gray, d);
        let at = self.head.at(taken - self.lag.value);
        let (shot, still, head) = match at {
            None => (json!({"head": null, "head_moved": 1.0, "still": false, "tags": tags_json(&tags)}), false, None),
            Some((pose, turn, mv)) => {
                // A wrong lag throws off a moving frame's pose (live, a guessed 60 ms gave a 12 mm fit), so until it's measured only nearly still frames count.
                let still = turn < if self.lag.measured { self.max_turn } else { 5.0 } && mv < MAX_MOVE;
                let p: Vec<Vec<f64>> = pose.iter().map(|r| r.iter().map(|v| round(*v, 6)).collect()).collect();
                (json!({"head": p, "head_moved": round(turn, 2), "still": still, "speed": [round(turn, 1), round(mv, 3)],
                        "taken": round(taken, 4), "lag": round(self.lag.value, 3), "tags": tags_json(&tags)}), still, Some(pose))
            }
        };
        {
            let mut s = self.hud.state.lock().unwrap();
            s.still = still;
            s.shot = Some((still, head, tags.iter().map(|t| (t.id, t.corners)).collect()));
        }
        Ok((rgb, shot, tags))
    }

    fn take_skip(&self) -> bool {
        std::mem::take(&mut self.hud.state.lock().unwrap().skip)
    }

    /// Takes n shots and appends them to the session's. The usable ones are kept with what the camera saw, as a JPEG.
    pub fn shots(&mut self, n: usize) -> Result<Answer, String> {
        let (mut found, mut at) = (vec![], None);
        for _ in 0..n {
            let (rgb, mut s, tags) = self.shot(&dict::DICT_4X4_250)?;
            s["monitor"] = json!(self.monitor);
            if s["still"] == json!(true) {
                let stem = self.path.with_extension("");
                let image = format!("{}-{}.jpg", stem.display(), self.shots.len());
                if jpeg_encoder::Encoder::new_file(&image, 95).and_then(|e| e.encode(&rgb, self.cam.w as u16, self.cam.h as u16, jpeg_encoder::ColorType::Rgb)).is_ok() {
                    s["image"] = json!(image);
                }
                found.extend(tags.iter().map(|t| t.id));
                at = s["head"].as_array().map(|r| [r[0][3].as_f64().unwrap_or(0.0), r[1][3].as_f64().unwrap_or(0.0), r[2][3].as_f64().unwrap_or(0.0)]);
            }
            self.shots.push(s);
        }
        if self.shots.len() % 20 == 0 {
            self.save(); // Save now and then. Everything gets saved at the end.
        }
        found.sort();
        found.dedup();
        Ok(Answer { at, skip: self.take_skip(), found })
    }

    /// Takes one shot for the pairing key's tags and the host's address tags. Returns (the key if three were read,
    /// the address if four were, whether the shot was usable, skip).
    pub fn keyread(&mut self) -> Result<(Option<String>, Option<[u8; 4]>, bool, bool), String> {
        let (_, s, tags) = self.shot(&dict::DICT_4X4_1000)?;
        Ok((read_key(&tags), read_addr(&tags), s["still"] == json!(true), self.take_skip()))
    }

    /// Saves the shots and the head's poses over the scan (<path>-head.json, each [time, 12 numbers]).
    /// The fit uses them to read the pose at any frame's time and find the mirror's lag itself.
    pub fn save(&self) {
        let _ = std::fs::write(&self.path, Value::Array(self.shots.clone()).to_string());
        let track: Vec<Value> = self.head.poses.lock().unwrap().iter().map(|(t, p)| {
            let mut row = vec![json!(round(*t, 4))];
            row.extend(p.iter().flat_map(|r| r.iter().map(|v| json!(round(*v, 6)))));
            Value::Array(row)
        }).collect();
        let head = self.path.with_file_name(format!("{}-head.json", self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("shots")));
        let _ = std::fs::write(head, Value::Array(track).to_string());
    }

    /// Ends the scan: saves everything and takes the HUD down.
    pub fn finish(self) {
        self.save();
        self.hud.hide();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn archives_a_previous_scan() {
        let d = std::env::temp_dir().join(format!("cc-scan-archive-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for f in ["hidden.json", "hidden-head.json", "hidden-0.jpg", "hidden-12.jpg", "hidden-orig.json", "visible.json"] {
            std::fs::write(d.join(f), "[1, 2]").unwrap();
        }
        super::archive(&d.join("hidden.json"));
        let left: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with("hidden-1") || n.ends_with(".json")).collect();
        let sub = std::fs::read_dir(&d).unwrap().flatten().find(|e| e.path().is_dir()).unwrap().path();
        let mut moved: Vec<String> = std::fs::read_dir(&sub).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        moved.sort();
        assert_eq!(moved, ["hidden-0.jpg", "hidden-12.jpg", "hidden-head.json", "hidden.json"], "{left:?}");
        assert!(d.join("hidden-orig.json").exists() && d.join("visible.json").exists());
        std::fs::remove_dir_all(d).unwrap();
    }
}
