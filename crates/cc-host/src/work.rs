//! What paired Frames ask the agent to run (docs/agent.md 3a, 5a; docs/remote-windows.md): monitor
//! sessions, window streams and tag screens. It follows home/agent.py's rules. Sessions get
//! sessions_max, the idle stop by the Frame's own address, adoption after a restart, and stale servers
//! restarted. Windows get their scope, opt-ins and limits. Tag screens are checked and last 60 s each,
//! Esc only cancels, blocking takes a deliberate choice, and there's an hourly cap.
use crate::agent::Agent;
use crate::platform::mono;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const START_S: f64 = 10.0;
pub const WINDOWS_PER_FRAME: u16 = 2;
const TAG_LIFE: f64 = 60.0;
const CANCEL_GAP: f64 = 3.0;
const BLOCK_S: f64 = 600.0;
const ASK_AFTER: usize = 3;
const ASK_WINDOW: f64 = 300.0;

pub struct Sess {
    pub port: u16,
    pub ip: Option<String>,
    pub idle_since: Option<f64>,
    pub adopted: Option<f64>,
    pub fresh: Option<f64>,
    pub uuid: Option<String>,
    pub unit: String,
}

struct Screen {
    child: std::process::Child,
    until: f64,
}

#[derive(Default)]
pub struct Tags {
    holder: Option<String>,
    screens: HashMap<i64, Screen>,
    used: HashMap<String, Vec<(f64, f64)>>,
    cancels: HashMap<String, Vec<f64>>,
    blocked: HashMap<String, f64>,
    last_cancel: HashMap<String, f64>,
}

#[derive(Default)]
pub struct Work {
    pub sessions: HashMap<(String, String), Sess>,
    locks: HashMap<(String, String), Arc<Mutex<()>>>,
    out_sig: Option<String>,
    out_changed: f64,
    asks: HashMap<String, VecDeque<Instant>>,
    pub tags: Tags,
    imu: Option<ImuStream>,
}

/// The one IMU stream a host runs (the controller is one device): who asked, and how to stop it.
struct ImuStream {
    frame: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

fn settings(agent: &Agent) -> Value {
    std::fs::read_to_string(agent.conf.join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}))
}

fn setting(agent: &Agent, key: &str, default: f64) -> f64 {
    settings(agent)[key].as_f64().unwrap_or(default)
}

pub fn valid_uuid(u: &str) -> bool {
    let b = u.as_bytes();
    b.len() == 38 && b[0] == b'{' && b[37] == b'}' && u[1..37].char_indices().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_digit() || ('a'..='f').contains(&c) })
}

impl Agent {
    fn lock_for(&self, key: &(String, String)) -> Arc<Mutex<()>> {
        self.work.lock().unwrap().locks.entry(key.clone()).or_default().clone()
    }

    fn slot(&self, frame: &str) -> Result<u16, String> {
        let t = std::fs::read_to_string(self.conf.join(format!("frames/{frame}.json"))).map_err(|_| "not-paired".to_string())?;
        let v: Value = serde_json::from_str(&t).map_err(|_| "not-paired".to_string())?;
        v["slot"].as_u64().map(|s| s as u16).ok_or_else(|| "not-paired".into())
    }

    fn wait_up(&self, frame: &str, port: u16, unit: &str, event: Value) -> Result<f64, Value> {
        let t0 = mono();
        let mut beat = t0;
        while !self.plat.listening(port) {
            if mono() - t0 > START_S {
                let out = self.plat.systemctl(&["status", "-n", "1", "--no-pager", unit]);
                return Err(json!({"ok": false, "error": "not-ready", "detail": out.trim().lines().last().into_iter().collect::<Vec<_>>()}));
            }
            if mono() - beat >= 1.0 {
                beat = mono();
                self.event(frame, event.clone());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(t0)
    }

    /// session start: starts this Frame's server for that monitor (or restarts a stale one) and answers
    /// once its port listens. There are at most sessions_max on this host.
    pub fn session_start(&self, frame: &str, ip: &str, index: &Value) -> Value {
        let Some(index) = index.as_i64() else { return json!({"ok": false, "error": "no-such-monitor"}) };
        if !self.plat.monitors().iter().any(|m| m["index"] == json!(index)) {
            return json!({"ok": false, "error": "no-such-monitor"});
        }
        let slot = match self.slot(frame) {
            Ok(s) => s,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        let port = 3400 + 10 * slot + index as u16;
        let unit = format!("control-center-frame@{frame}-{index}.service");
        let key = (frame.to_owned(), index.to_string());
        let lock = self.lock_for(&key);
        let _held = lock.lock().unwrap();
        let t0 = mono();
        if self.plat.listening(port) && self.plat.viewers(port, None) == 0 && self.stale(&key, &unit) {
            println!("agent: {frame} session {index}: its server predates the monitors' last change, restarting");
            self.plat.systemctl(&["restart", &unit]);
        }
        if !self.plat.listening(port) {
            let cap = setting(self, "sessions_max", 4.0) as usize;
            let others = self.work.lock().unwrap().sessions.keys().filter(|k| **k != key).count();
            if others >= cap {
                return json!({"ok": false, "error": "busy", "detail": format!("{cap} sessions running on this host")});
            }
            self.plat.systemctl(&["start", &unit]);
            if let Err(e) = self.wait_up(frame, port, &unit, json!({"event": "session", "index": index, "state": "starting"})) {
                return e;
            }
        }
        let ms = ((mono() - t0) * 1000.0).round() as i64;
        self.work.lock().unwrap().sessions.insert(key, Sess { port, ip: Some(ip.to_owned()), idle_since: None, adopted: None, fresh: Some(mono()), uuid: None, unit });
        if ms > 50 {
            println!("agent: {frame} session {index} up in {ms} ms");
        }
        json!({"ok": true, "port": port, "ready": true, "start_ms": ms})
    }

    fn stale(&self, key: &(String, String), unit: &str) -> bool {
        let w = self.work.lock().unwrap();
        match w.sessions.get(key) {
            None => true,
            Some(s) => s.adopted.is_some() || s.fresh.is_none() || self.plat.unit_started(unit) < w.out_changed,
        }
    }

    pub fn session_stop(&self, frame: &str, index: &Value) -> Value {
        let Some(index) = index.as_i64() else { return json!({"ok": false, "error": "no-such-monitor"}) };
        let slot = match self.slot(frame) {
            Ok(s) => s,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        let key = (frame.to_owned(), index.to_string());
        let lock = self.lock_for(&key);
        let _held = lock.lock().unwrap();
        if self.plat.viewers(3400 + 10 * slot + index as u16, None) > 0 {
            return json!({"ok": true, "stopped": false});
        }
        self.plat.systemctl(&["stop", &format!("control-center-frame@{frame}-{index}.service")]);
        self.work.lock().unwrap().sessions.remove(&key);
        json!({"ok": true, "stopped": true})
    }

    fn window_scope(&self) -> Vec<Value> {
        let (wins, desktop, shared) = self.plat.windows();
        let captions = self.conf.join("windows-captions").exists();
        wins.into_iter().filter_map(|w| {
            if w["type"] != json!(0) || w["excludeFromCapture"] == json!(true) || w["minimized"] == json!(true) {
                return None;
            }
            if let (Some(ds), Some(d)) = (w["desktops"].as_array(), &desktop)
                && !ds.is_empty() && !ds.iter().any(|x| x.as_str() == Some(d))
            {
                return None;
            }
            let g = |k: &str| w[k].as_f64().unwrap_or(0.0);
            let (x, y, ww, hh) = (g("x"), g("y"), g("width"), g("height"));
            if !shared.iter().any(|r| x < r[0] + r[2] && r[0] < x + ww && y < r[1] + r[3] && r[1] < y + hh) {
                return None;
            }
            let mut item = json!({"uuid": w["uuid"], "app": w["desktopFile"].as_str().filter(|s| !s.is_empty()).or(w["resourceClass"].as_str()).unwrap_or(""),
                                  "x": x.round() as i64, "y": y.round() as i64, "w": ww.round() as i64, "h": hh.round() as i64});
            if captions {
                item["caption"] = w["caption"].clone();
            }
            Some(item)
        }).collect()
    }

    fn window_allowed(&self, frame: &str) -> Result<(), String> {
        if !self.conf.join("windows-on").exists() {
            return Err("windows-off".into());
        }
        let limit = setting(self, "window_asks_per_min", 20.0) as usize;
        let mut w = self.work.lock().unwrap();
        let q = w.asks.entry(frame.to_owned()).or_default();
        while q.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            q.pop_front();
        }
        if q.len() >= limit {
            return Err("rate-limited".into());
        }
        q.push_back(Instant::now());
        Ok(())
    }

    pub fn window_list(&self, frame: &str) -> Result<Value, String> {
        self.window_allowed(frame)?;
        Ok(json!({"ok": true, "windows": self.window_scope()}))
    }

    pub fn window_start(&self, frame: &str, ip: &str, uuid: &Value) -> Value {
        if let Err(e) = self.window_allowed(frame) {
            return json!({"ok": false, "error": e});
        }
        let Some(uuid) = uuid.as_str().filter(|u| valid_uuid(u)) else { return json!({"ok": false, "error": "bad-uuid"}) };
        let slot = match self.slot(frame) {
            Ok(s) => s,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        let (held, used): (Option<(String, String)>, Vec<String>) = {
            let w = self.work.lock().unwrap();
            let mine: Vec<(&(String, String), &Sess)> = w.sessions.iter().filter(|(k, s)| k.0 == frame && s.uuid.is_some()).collect();
            (mine.iter().find(|(_, s)| s.uuid.as_deref() == Some(uuid)).map(|(k, _)| (*k).clone()), mine.iter().map(|(k, _)| k.1.clone()).collect())
        };
        if held.is_none() && !self.window_scope().iter().any(|w| w["uuid"] == json!(uuid)) {
            return json!({"ok": false, "error": "not-shared"});
        }
        let key = match held {
            Some(k) => k,
            None => {
                let Some(k) = (0..WINDOWS_PER_FRAME).map(|k| format!("w{k}")).find(|k| !used.contains(k)) else {
                    return json!({"ok": false, "error": "busy", "detail": format!("{WINDOWS_PER_FRAME} windows popped out already")});
                };
                let cap = setting(self, "sessions_max", 4.0) as usize;
                if self.work.lock().unwrap().sessions.len() >= cap {
                    return json!({"ok": false, "error": "busy", "detail": format!("{cap} sessions running on this host")});
                }
                let need = setting(self, "window_min_free_mb", 1024.0) as u64;
                if self.plat.memory_mb() < need {
                    return json!({"ok": false, "error": "busy", "detail": format!("under {need} MB of memory free on this host")});
                }
                (frame.to_owned(), k)
            }
        };
        let k: u16 = key.1[1..].parse().unwrap_or(0);
        let (port, unit) = (3405 + 10 * slot + k, format!("control-center-window@{frame}-{k}-{}.service", &uuid[1..37]));
        let lock = self.lock_for(&key);
        let _held = lock.lock().unwrap();
        let t0 = mono();
        if !self.plat.listening(port) {
            self.plat.systemctl(&["start", &unit]);
            if let Err(e) = self.wait_up(frame, port, &unit, json!({"event": "window", "uuid": uuid, "state": "starting"})) {
                return e;
            }
        }
        self.work.lock().unwrap().sessions.insert(key, Sess { port, ip: Some(ip.to_owned()), idle_since: None, adopted: None, fresh: Some(mono()), uuid: Some(uuid.to_owned()), unit });
        let app = self.window_scope().into_iter().find(|w| w["uuid"] == json!(uuid)).and_then(|w| w["app"].as_str().map(str::to_owned)).unwrap_or_else(|| "?".into());
        println!("agent: {frame} window {app} on {port}"); // Log the app id, never the caption.
        json!({"ok": true, "port": port, "ready": true, "start_ms": ((mono() - t0) * 1000.0).round() as i64})
    }

    pub fn window_stop(&self, frame: &str, uuid: &Value) -> Value {
        let key = self.work.lock().unwrap().sessions.iter().find(|(k, s)| k.0 == frame && s.uuid.as_ref().map(|u| json!(u)) == Some(uuid.clone())).map(|(k, _)| k.clone());
        let Some(key) = key else { return json!({"ok": true, "stopped": true}) };
        let lock = self.lock_for(&key);
        let _held = lock.lock().unwrap();
        let (port, unit) = match self.work.lock().unwrap().sessions.get(&key) {
            Some(s) => (s.port, s.unit.clone()),
            None => return json!({"ok": true, "stopped": true}),
        };
        if self.plat.viewers(port, None) > 0 {
            return json!({"ok": true, "stopped": false});
        }
        self.plat.systemctl(&["stop", &unit]);
        self.work.lock().unwrap().sessions.remove(&key);
        json!({"ok": true, "stopped": true})
    }

    /// The session list `status` shows for this Frame.
    pub fn sessions_of(&self, frame: &str) -> Vec<Value> {
        let now = mono();
        let w = self.work.lock().unwrap();
        let mut out: Vec<(String, Value)> = w.sessions.iter().filter(|(k, _)| k.0 == frame).map(|(k, s)| {
            let index = k.1.parse::<i64>().map(|i| json!(i)).unwrap_or(json!(k.1));
            let mut v = json!({"index": index, "port": s.port, "idle_s": s.idle_since.map_or(0, |t| (now - t).round() as i64)});
            if let Some(u) = &s.uuid {
                v["uuid"] = json!(u);
            }
            (k.1.clone(), v)
        }).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.into_iter().map(|(_, v)| v).collect()
    }

    pub fn forget_frame(&self, frame: &str) {
        self.work.lock().unwrap().sessions.retain(|k, _| k.0 != frame);
        self.imu_stop(frame);
    }

    /// imu start [hz] / stop (docs/agent.md, "IMU stream"): streams the Deck's motion samples to this
    /// Frame as `imu` events in batches of about `hz` a second, until stop, or its connection closes.
    /// Answers `no-imu` on a host without a Deck controller and `busy` when another Frame has it.
    pub fn imu(self: &Arc<Self>, frame: &str, req: &Value) -> Result<Value, String> {
        match req["op"].as_str() {
            Some("stop") => Ok(json!({"ok": true, "stopped": self.imu_stop(frame)})),
            Some("start") => {
                let hz = match &req["hz"] {
                    Value::Null => 90,
                    v => v.as_u64().filter(|h| (10..=125).contains(h)).ok_or("bad-rate")? as u32,
                };
                let path = self.plat.deck_imu().ok_or("no-imu")?;
                if self.work.lock().unwrap().imu.as_ref().is_some_and(|s| s.frame != frame) {
                    return Err("busy".into());
                }
                self.imu_stop(frame); // A second start from the same Frame changes the rate.
                let mut dev = crate::imu::Imu::open(&path).map_err(|e| format!("imu-failed: {e}"))?;
                let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (me, f, st) = (self.clone(), frame.to_owned(), stop.clone());
                let thread = std::thread::spawn(move || {
                    let mut n = 0u64;
                    crate::imu::run(&mut dev, hz, &st, |b| {
                        n += 1;
                        me.event(&f, json!({"event": "imu", "n": n, "samples": b.iter().map(|s| s.to_row()).collect::<Vec<_>>()}))
                    });
                    // dev drops here and puts the controller's setting back.
                });
                self.work.lock().unwrap().imu = Some(ImuStream { frame: frame.to_owned(), stop, thread });
                println!("agent: {frame} imu start {hz} Hz");
                use cc_proto::imu::{ACCEL_PER_G, GYRO_PER_DPS, QUAT_ONE};
                Ok(json!({"ok": true, "hz": hz, "sample_hz": crate::imu::DEVICE_HZ, "accel_per_g": ACCEL_PER_G, "gyro_per_dps": GYRO_PER_DPS, "quat_one": QUAT_ONE,
                          "fields": ["seq", "t_us", "ax", "ay", "az", "gx", "gy", "gz", "qw", "qx", "qy", "qz"]}))
            }
            _ => Err("bad-op".into()),
        }
    }

    /// Stops this Frame's IMU stream, waits for the controller's setting to be put back, and says whether there was one.
    pub fn imu_stop(&self, frame: &str) -> bool {
        let mut w = self.work.lock().unwrap();
        if w.imu.as_ref().is_some_and(|s| s.frame == frame) {
            let s = w.imu.take().unwrap();
            drop(w);
            s.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = s.thread.join();
            return true;
        }
        false
    }

    /// After an agent restart, running servers become sessions again, watched from their Frame's last
    /// address. One whose Frame is unknown stops after one idle timeout unless it's claimed.
    pub fn adopt(&self) {
        let out = self.plat.systemctl(&["list-units", "--no-legend", "--state=running", "control-center-frame@*", "control-center-window@*"]);
        for name in out.split_whitespace().filter(|w| w.starts_with("control-center-") && w.ends_with(".service")) {
            let inst = name.trim_end_matches(".service").split_once('@').map(|x| x.1).unwrap_or("");
            let (frame, key, port, uuid) = if name.starts_with("control-center-window@") && inst.len() > 39 {
                let (head, u) = inst.split_at(inst.len() - 36);
                let Some((f, k)) = head.trim_end_matches('-').rsplit_once('-') else { continue };
                let Ok(slot) = self.slot(f) else { continue };
                let k: u16 = k.parse().unwrap_or(0);
                (f.to_owned(), format!("w{k}"), 3405 + 10 * slot + k, Some(format!("{{{u}}}")))
            } else if let Some((f, i)) = inst.rsplit_once('-') {
                let (Ok(slot), Ok(i)) = (self.slot(f), i.parse::<u16>()) else { continue };
                (f.to_owned(), i.to_string(), 3400 + 10 * slot + i, None)
            } else {
                continue;
            };
            let ip = std::fs::read_to_string(self.conf.join(format!("frames/{frame}.last"))).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
            let adopted = if ip.is_none() || uuid.is_some() { Some(mono()) } else { None };
            self.work.lock().unwrap().sessions.insert((frame, key), Sess { port, ip, idle_since: None, adopted, fresh: None, uuid, unit: name.to_owned() });
        }
    }

    pub fn idle_stop(&self) {
        let limit = 60.0 * setting(self, "session_idle_min", 10.0);
        let now = mono();
        let keys: Vec<(String, String)> = self.work.lock().unwrap().sessions.keys().cloned().collect();
        for key in keys {
            let Some((port, ip, adopted, idle, unit)) = self.work.lock().unwrap().sessions.get(&key).map(|s| (s.port, s.ip.clone(), s.adopted, s.idle_since, s.unit.clone())) else { continue };
            if !self.plat.listening(port) {
                self.work.lock().unwrap().sessions.remove(&key);
                continue;
            }
            let unclaimed = adopted.is_some_and(|a| now - a >= limit);
            let watched = !unclaimed && self.plat.viewers(port, ip.as_deref()) > 0;
            let mut w = self.work.lock().unwrap();
            if watched {
                if let Some(s) = w.sessions.get_mut(&key) {
                    s.idle_since = None;
                }
            } else if !unclaimed && idle.is_none() {
                if let Some(s) = w.sessions.get_mut(&key) {
                    s.idle_since = Some(now);
                }
            } else if unclaimed || idle.is_some_and(|t| now - t >= limit) {
                drop(w);
                self.plat.systemctl(&["stop", &unit]);
                self.work.lock().unwrap().sessions.remove(&key);
                println!("agent: {} session {} stopped (idle)", key.0, key.1);
            }
        }
    }

    /// When the outputs change, monitor servers older than the change are stale, and unwatched ones
    /// restart now. A server whose stream failed restarts too, and its client reconnects.
    pub fn watch(&self) {
        let now = mono();
        let sig = self.plat.outputs();
        let changed = {
            let mut w = self.work.lock().unwrap();
            let changed = w.out_sig.as_ref().is_some_and(|s| *s != sig);
            if changed {
                w.out_changed = now;
            }
            w.out_sig = Some(sig);
            changed
        };
        let list: Vec<((String, String), u16, bool, String)> = self.work.lock().unwrap().sessions.iter().map(|(k, s)| (k.clone(), s.port, s.uuid.is_some(), s.unit.clone())).collect();
        for (key, port, window, unit) in list {
            let restart = !window && ((changed && self.plat.viewers(port, None) == 0) || self.plat.stream_failed(&unit));
            if restart {
                println!("agent: {} session {}: restarting its server ({})", key.0, key.1, if changed { "the monitors changed" } else { "its stream failed" });
                self.plat.systemctl(&["restart", &unit]);
                if let Some(s) = self.work.lock().unwrap().sessions.get_mut(&key) {
                    s.fresh = Some(now);
                }
            }
        }
    }

    // -- Tag screens (docs/agent.md 3a).

    pub fn tags(self: &Arc<Self>, frame: &str, req: &Value) -> Result<Value, String> {
        match req["op"].as_str() {
            Some("hide") => {
                if self.work.lock().unwrap().tags.holder.as_deref() == Some(frame) {
                    self.tags_hide(req["index"].as_i64());
                }
                Ok(json!({"ok": true}))
            }
            Some("show") => {
                let index = req["index"].as_i64().ok_or("bad-request")?;
                let mon = self.plat.monitors().into_iter().find(|m| m["index"] == json!(index)).ok_or("no-such-monitor")?;
                let (w, h) = (mon["width"].as_i64().unwrap_or(0), mon["height"].as_i64().unwrap_or(0));
                check_tags(&req["params"], w, h)?;
                Ok(match self.tags_show(frame, index, mon["output"].as_str().unwrap_or(""), [w, h], &req["params"]) {
                    None => json!({"ok": true}),
                    Some(why) => json!({"ok": false, "error": why}),
                })
            }
            _ => Err("bad-request".into()),
        }
    }

    fn tags_show(self: &Arc<Self>, frame: &str, index: i64, output: &str, size: [i64; 2], params: &Value) -> Option<String> {
        let now = mono();
        let cap = 60.0 * setting(self, "tags_cap_min", 5.0);
        let mut w = self.work.lock().unwrap();
        let t = &mut w.tags;
        if t.blocked.get(frame).is_some_and(|until| *until > now) {
            return Some("blocked".into());
        }
        if t.last_cancel.get(frame).is_some_and(|c| now - c < CANCEL_GAP) {
            return Some("wait".into());
        }
        let spent: f64 = t.used.get(frame).map_or(0.0, |u| u.iter().filter(|(_, e)| e.min(now) > now - 3600.0).map(|(s, e)| e.min(now) - s.max(now - 3600.0)).sum());
        if spent >= cap {
            return Some("tags-limit".into());
        }
        if t.holder.as_deref().is_some_and(|h| h != frame) && !t.screens.is_empty() {
            return Some("busy".into());
        }
        t.holder = Some(frame.to_owned());
        let alive = t.screens.get_mut(&index).is_some_and(|s| s.child.try_wait().ok().flatten().is_none());
        if !alive {
            let child = self.plat.tagshow(output)?;
            t.screens.insert(index, Screen { child, until: now + TAG_LIFE });
            let stdout = t.screens.get_mut(&index).and_then(|s| s.child.stdout.take());
            if let Some(out) = stdout {
                let me = self.clone();
                let (f, o) = (frame.to_owned(), output.to_owned());
                std::thread::spawn(move || {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        me.tag_answer(&f, index, &o, output_line(&line));
                    }
                });
            }
        }
        let mut msg = params.clone();
        msg["size"] = json!(size);
        let mut line = serde_json::to_vec(&msg).unwrap_or_default();
        line.push(b'\n');
        let s = t.screens.get_mut(&index)?;
        if s.child.stdin.as_mut().and_then(|i| i.write_all(&line).and_then(|_| i.flush()).ok()).is_none() {
            return Some("tagshow-failed".into());
        }
        s.until = now + TAG_LIFE;
        // Count the time screens are up only once. A show while one is up extends it, since a scan shows many.
        let u = t.used.entry(frame.to_owned()).or_default();
        match u.last_mut() {
            Some(last) if last.1 > now => last.1 = now + TAG_LIFE,
            _ => u.push((now, now + TAG_LIFE)),
        }
        None
    }

    fn tag_answer(&self, frame: &str, index: i64, output: &str, what: &str) {
        let now = mono();
        match what {
            "escaped" => {
                self.event(frame, json!({"event": "escaped", "index": index}));
                self.tags_hide(Some(index));
                let mut w = self.work.lock().unwrap();
                w.tags.last_cancel.insert(frame.to_owned(), now);
                let c = w.tags.cancels.entry(frame.to_owned()).or_default();
                c.retain(|t| now - t < ASK_WINDOW);
                c.push(now);
                if c.len() >= ASK_AFTER {
                    drop(w);
                    let text = format!("{frame} cancelled {ASK_AFTER} times in a few minutes. Block it for 10 minutes?\nEnter = block, Esc = keep allowing");
                    if self.plat.ask(output, &text) == "block" {
                        self.tag_answer(frame, index, output, "block");
                    }
                }
            }
            "block" => {
                let mut w = self.work.lock().unwrap();
                w.tags.blocked.insert(frame.to_owned(), now + BLOCK_S);
                w.tags.cancels.remove(frame);
                drop(w);
                self.tags_hide(None);
                self.event(frame, json!({"event": "blocked", "minutes": (BLOCK_S / 60.0) as i64}));
            }
            _ => {}
        }
    }

    pub fn tags_hide(&self, index: Option<i64>) {
        let mut w = self.work.lock().unwrap();
        let t = &mut w.tags;
        let which: Vec<i64> = match index {
            Some(i) => vec![i],
            None => t.screens.keys().copied().collect(),
        };
        for i in which {
            if let Some(mut s) = t.screens.remove(&i) {
                drop(s.child.stdin.take()); // EOF ends the screen. After an Esc, it may still ask.
                std::thread::spawn(move || s.child.wait());
            }
        }
        if t.screens.is_empty() {
            let now = mono();
            if let Some(last) = t.holder.take().and_then(|f| t.used.get_mut(&f)).and_then(|u| u.last_mut()) {
                last.1 = last.1.min(now); // It's hidden, so the rest of its 60 s isn't spent.
            }
        }
    }

    pub fn tags_expire(&self) {
        let now = mono();
        let gone: Vec<i64> = {
            let mut w = self.work.lock().unwrap();
            w.tags.screens.iter_mut().filter_map(|(i, s)| (s.until < now || s.child.try_wait().ok().flatten().is_some()).then_some(*i)).collect()
        };
        for i in gone {
            self.tags_hide(Some(i));
        }
    }

    pub fn tags_state(&self, frame: &str) -> (Vec<i64>, bool) {
        let w = self.work.lock().unwrap();
        let mine = if w.tags.holder.as_deref() == Some(frame) { w.tags.screens.keys().copied().collect() } else { Vec::new() };
        (mine, w.tags.blocked.get(frame).is_some_and(|u| *u > mono()))
    }

    /// cc-share unlock (a SIGHUP without the lock file) lifts the blocks.
    pub fn unblock_all(&self) {
        self.work.lock().unwrap().tags.blocked.clear();
    }
}

fn output_line(line: &str) -> &str {
    line.trim()
}

/// Checks a tag screen's parameters strictly (docs/agent.md 3a): {bg: white|wait, tags: [[id, x, y, side]...]},
/// with at most 64 tags, ids 0-249, side >= 12, all inside the output, no overlap, and none (counting
/// its quiet zone) in the banner's band.
pub fn check_tags(p: &Value, w: i64, h: i64) -> Result<(), String> {
    let o = p.as_object().ok_or("bad-params")?;
    if o.keys().any(|k| k != "bg" && k != "tags") || !matches!(o.get("bg").and_then(Value::as_str), Some("white" | "wait")) {
        return Err("bad-params".into());
    }
    let tags = match o.get("tags") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err("too-many-tags".into()),
    };
    if tags.len() > 64 {
        return Err("too-many-tags".into());
    }
    let band = 24.max(h / 36);
    let mut boxes: Vec<(i64, i64, i64)> = Vec::new();
    for t in &tags {
        let v: Vec<i64> = t.as_array().filter(|a| a.len() == 4 && a.iter().all(|x| x.is_i64())).map(|a| a.iter().map(|x| x.as_i64().unwrap()).collect()).ok_or("bad-tag")?;
        let (id, x, y, side) = (v[0], v[1], v[2], v[3]);
        let q = side / 4;
        if !(0..250).contains(&id) || side < 12 || x < 0 || y < 0 || x + side > w || y + side > h {
            return Err("tag-outside".into());
        }
        if y - q < band {
            return Err("tag-in-banner".into());
        }
        if boxes.iter().any(|&(bx, by, bs)| x < bx + bs && bx < x + side && y < by + bs && by < y + side) {
            return Err("tags-overlap".into());
        }
        boxes.push((x, y, side));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_checks() {
        let ok = json!({"bg": "white", "tags": [[0, 100, 200, 240], [1, 1500, 200, 240]]});
        assert!(check_tags(&ok, 1920, 1080).is_ok());
        for (bad, why) in [
            (json!({"bg": "white", "tags": vec![json!([0, 0, 0, 12]); 65]}), "too-many-tags"),
            (json!({"bg": "white", "tags": [[0, 1800, 200, 240]]}), "tag-outside"),
            (json!({"bg": "white", "tags": [[0, 100, 200, 240], [1, 200, 300, 240]]}), "tags-overlap"),
            (json!({"bg": "white", "tags": [[0, 100, 10, 240]]}), "tag-in-banner"),
            (json!({"bg": "white", "tags": [[250, 100, 200, 240]]}), "tag-outside"),
            (json!({"bg": "red", "tags": []}), "bad-params"),
            (json!({"bg": "white", "tags": [], "image": "x"}), "bad-params"),
        ] {
            assert_eq!(check_tags(&bad, 1920, 1080), Err(why.into()), "{bad}");
        }
    }

    #[test]
    fn uuids() {
        assert!(valid_uuid("{3f1c2b7e-5a4d-4c1e-9b8a-6d2e1f0a9c3b}"));
        for b in ["3f1c2b7e-5a4d-4c1e-9b8a-6d2e1f0a9c3b", "{../../etc}", &format!("{{{}}}", "a".repeat(36)), "{3F1C2B7E-5A4D-4C1E-9B8A-6D2E1F0A9C3B}"] {
            assert!(!valid_uuid(b), "{b}");
        }
    }
}
