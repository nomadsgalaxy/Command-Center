//! The session's KWin, over its own D-Bus. Our script panels/cc-windows.js reports every
//! window to us (`Event(json)`) and long-polls us for commands (`Next()`, always answered
//! within 10 s, "" when there's nothing), because a KWin script can only call out. We own
//! org.controlcenter.Panels on that bus. Each connect reloads the script, which re-sends
//! every window.
//! Anything the script changes (border, geometry, all desktops) it saves first. windows.rs
//! keeps those in kwin-restore.json, and `restore` puts them back with a one-shot script
//! (panels/cc-restore.js) on every exit, after a crash (`cc-panels --restore`, from the
//! wrapper) and on the next start.
use crate::{QUIT, config, root, session};
use serde_json::{Value, json};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, mpsc};
use std::task::Poll;
use std::time::{Duration, Instant};

const NAME: &str = "org.controlcenter.Panels";
const SCRIPT: &str = "cc-windows";

/// What the script saved, by window uuid, while it runs.
pub fn restore_file() -> String {
    config::cache("kwin-restore.json")
}

pub type Queue = Arc<Mutex<Vec<Value>>>;

/// A command got queued, so the long poll answers now (`kick` after each push).
static QUEUED: event_listener::Event = event_listener::Event::new();

pub fn kick() {
    QUEUED.notify(1);
}

struct Panels {
    events: mpsc::Sender<Value>,
    queue: Queue,
}

#[zbus::interface(name = "org.controlcenter.Panels")]
impl Panels {
    fn event(&self, json: &str) {
        match serde_json::from_str(json) {
            Ok(v) => {
                let _ = self.events.send(v);
                crate::vr::wake(); // so the main loop takes it now, not on its next idle tick
            }
            Err(e) => eprintln!("kwin: bad event {json:?}: {e}"),
        }
    }

    /// The commands queued since the last call, as a JSON array, or "" after 10 s without any.
    /// It sleeps until a `kick` instead of polling every 20 ms, which saves ~50 wakes a second.
    async fn next(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut queued = QUEUED.listen(); // listen before looking, so a push from here on still wakes it
            let cmds = std::mem::take(&mut *self.queue.lock().unwrap());
            if !cmds.is_empty() {
                return Value::Array(cmds).to_string();
            }
            let mut timer = async_io::Timer::at(deadline);
            let timed_out = std::future::poll_fn(|cx| match std::pin::Pin::new(&mut queued).poll(cx) {
                Poll::Ready(()) => Poll::Ready(false),
                Poll::Pending => std::pin::Pin::new(&mut timer).poll(cx).map(|_| true),
            })
            .await;
            if timed_out {
                return String::new();
            }
        }
    }
}

/// The script's side, for the main loop: its events, and our commands to it.
pub struct Kwin {
    pub events: mpsc::Receiver<Value>,
    queue: Queue,
}

impl Kwin {
    pub fn start() -> Kwin {
        let (tx, events) = mpsc::channel();
        let queue = Queue::default();
        let q = queue.clone();
        let _ = std::thread::Builder::new().name("cc-kwin".into()).spawn(move || run(tx, q));
        Kwin { events, queue }
    }

    pub fn send(&self, cmd: Value) {
        self.queue.lock().unwrap().push(cmd);
        kick();
    }

    /// For commands from other threads (the input loop's raises).
    pub fn queue(&self) -> Queue {
        self.queue.clone()
    }
}

fn env() -> Option<session::Env> {
    session::Env::discover(session::DESKTOP).filter(|e| !e.dbus.is_empty())
}

fn bus(e: &session::Env, name: Option<Panels>) -> zbus::Result<zbus::blocking::Connection> {
    let b = zbus::blocking::connection::Builder::address(e.dbus.as_str())?;
    match name {
        Some(p) => b.name(NAME)?.serve_at("/", p)?.build(),
        None => b.build(),
    }
}

fn unload(bus: &zbus::blocking::Connection, name: &str) {
    let _ = bus.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &(name,));
}

/// Loads and starts a script file under `name`, unloading any earlier one of that name first.
fn load(bus: &zbus::blocking::Connection, path: &str, name: &str) -> Result<(), String> {
    unload(bus, name);
    let id: i32 = bus
        .call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "loadScript", &(path, name))
        .and_then(|m| m.body().deserialize())
        .map_err(|e| format!("loadScript {path}: {e}"))?;
    if id < 0 {
        return Err(format!("KWin refused {path}"));
    }
    bus.call_method(Some("org.kde.KWin"), format!("/Scripting/Script{id}").as_str(), Some("org.kde.kwin.Script"), "run", &())
        .map_err(|e| format!("run {path}: {e}"))?;
    Ok(())
}

fn alive(bus: &zbus::blocking::Connection) -> bool {
    bus.call_method(Some("org.kde.KWin"), "/KWin", Some("org.freedesktop.DBus.Peer"), "Ping", &()).is_ok()
}

/// The KWin thread: connect, (re)load the script, watch KWin, and retry every 5 s.
fn run(events: mpsc::Sender<Value>, queue: Queue) {
    let (mut last, mut first) = (String::new(), true);
    while !QUIT.load(Relaxed) {
        let r = env().ok_or_else(|| "no session to show windows from".to_string()).and_then(|e| {
            let bus = bus(&e, Some(Panels { events: events.clone(), queue: queue.clone() })).map_err(|e| format!("session bus: {e}"))?;
            if std::mem::take(&mut first) {
                restore_on(&bus); // a crash left windows changed: put them back first, so they're saved as they really were
            }
            queue.lock().unwrap().clear(); // those were for the old script
            load(&bus, &root().join("panels/cc-windows.js").to_string_lossy(), SCRIPT)?;
            Ok(bus)
        });
        match r {
            Ok(bus) => {
                eprintln!("kwin: {SCRIPT} loaded");
                last.clear();
                while !QUIT.load(Relaxed) && alive(&bus) {
                    std::thread::sleep(Duration::from_secs(1));
                }
                if QUIT.load(Relaxed) {
                    return; // the exit does the restore (main's GiveBack)
                }
                eprintln!("kwin: connection lost");
                let _ = events.send(json!({"e": "lost"}));
            }
            Err(e) => {
                if e != last {
                    eprintln!("kwin: {e}");
                }
                last = e;
            }
        }
        for _ in 0..50 {
            if QUIT.load(Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Takes our script out and puts every window it changed back the way it was.
pub fn restore() {
    let Some(e) = env() else {
        if std::path::Path::new(&restore_file()).exists() {
            eprintln!("kwin: no session to restore windows in; {} kept for next time", restore_file());
        }
        return;
    };
    match bus(&e, None) {
        Ok(bus) => restore_on(&bus),
        Err(err) => eprintln!("kwin: can't restore windows: session bus: {err}"),
    }
}

fn restore_on(bus: &zbus::blocking::Connection) {
    unload(bus, SCRIPT); // first, or it would report (and place) whatever the restore changes
    let Ok(text) = std::fs::read_to_string(restore_file()) else { return };
    let saved: Value = serde_json::from_str(&text).unwrap_or_default(); // re-serialised so only data goes into the script
    let n = saved.as_object().map_or(0, |m| m.len());
    let body = std::fs::read_to_string(root().join("panels/cc-restore.js")).unwrap_or_default();
    let path = config::cache("restore.js");
    let r = std::fs::write(&path, format!("const saved = {saved};\n{body}")).map_err(|e| e.to_string()).and_then(|_| load(bus, &path, "cc-restore"));
    match r {
        Ok(()) => {
            // KWin reads and runs a script on its own time, so give it that before unloading.
            // ponytail: not confirmed; getWindowInfo could check each geometry if this ever misses
            std::thread::sleep(Duration::from_secs(1));
            unload(bus, "cc-restore");
            let _ = std::fs::rename(restore_file(), format!("{}.last", restore_file()));
            eprintln!("kwin: {n} window(s) restored");
        }
        Err(e) => eprintln!("kwin: can't restore windows ({e}); {} kept", restore_file()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_long_poll_answers_when_a_command_is_queued() {
        let (tx, _rx) = mpsc::channel();
        let p = Panels { events: tx, queue: Queue::default() };
        let q = p.queue.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50)); // while it waits
            q.lock().unwrap().push(json!({"c": "raise"}));
            kick();
        });
        let start = Instant::now();
        assert_eq!(async_io::block_on(p.next()), r#"[{"c":"raise"}]"#);
        assert!(start.elapsed() < Duration::from_secs(2), "woken, not timed out: {:?}", start.elapsed());
        t.join().unwrap();
    }
}
