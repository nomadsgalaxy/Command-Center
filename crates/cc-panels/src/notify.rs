//! Notifications in the headset: pairing results, errors, and a computer's screen sharing going
//! down or coming back. They go to org.freedesktop.Notifications on the Frame's main user bus
//! (/run/user/<uid>/bus), which Steam's steam_notif_daemon owns. That's not the desktop session's
//! own bus, the one cc-panels and Plasma use, so the address is spelled out here. If that call
//! fails, the text shows as SteamVR's own toast instead (IVRNotifications on the dashboard tab).
//!
//! `send` never blocks: a thread (cc-notify) does the D-Bus calls. Each subject (a monitor's
//! health, a pairing) says something at most once every GAP, and a newer message waiting out its
//! GAP replaces the older one, so a flapping host ends on what's true now. On top of that, no more
//! than BURST go out in a minute. settings.json `"notifications": false` turns them all off.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const GAP: Duration = Duration::from_secs(30); // one subject at most this often
const BURST: usize = 4; // ...and all of them at most this many a minute
const WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    subject: String,
    text: String,
}

/// Where notes go. The real one is the user bus; tests use a fake.
pub trait Bus {
    fn notify(&mut self, text: &str) -> Result<(), String>;
}

/// The Frame's main user bus, connected on first use and again after a failure.
#[derive(Default)]
struct UserBus(Option<zbus::blocking::Connection>);

impl Bus for UserBus {
    fn notify(&mut self, text: &str) -> Result<(), String> {
        if self.0.is_none() {
            let addr = format!("unix:path=/run/user/{}/bus", unsafe { libc::getuid() });
            self.0 = Some(zbus::blocking::connection::Builder::address(addr.as_str()).and_then(|b| b.build()).map_err(|e| format!("{addr}: {e}"))?);
        }
        let c = self.0.as_ref().unwrap();
        let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
        let args = ("Command Center", 0u32, "", text, "", Vec::<&str>::new(), hints, -1i32);
        let r = c.call_method(Some("org.freedesktop.Notifications"), "/org/freedesktop/Notifications", Some("org.freedesktop.Notifications"), "Notify", &args);
        r.map(|_| ()).map_err(|e| {
            self.0 = None; // a fresh connection next time
            e.to_string()
        })
    }
}

/// Who may say what, when (the module header).
#[derive(Default)]
struct Limit {
    subjects: HashMap<String, (Instant, String, Option<String>)>, // last sent, its text, and a newer one waiting
    sent: VecDeque<Instant>,
}

impl Limit {
    fn room(&mut self, now: Instant) -> bool {
        while self.sent.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            self.sent.pop_front();
        }
        self.sent.len() < BURST
    }

    /// A new note: returns its text if it goes out now. Otherwise it waits (`due`), or it's no news.
    fn offer(&mut self, now: Instant, n: Note) -> Option<String> {
        let room = self.room(now);
        match self.subjects.get_mut(&n.subject) {
            Some((_, last, wait)) if *last == n.text => {
                *wait = None; // back to what it last said
                None
            }
            Some((at, _, wait)) if now.duration_since(*at) < GAP || !room => {
                *wait = Some(n.text);
                None
            }
            None if !room => {
                self.subjects.insert(n.subject, (now.checked_sub(GAP).unwrap_or(now), String::new(), Some(n.text)));
                None
            }
            _ => {
                self.subjects.insert(n.subject, (now, n.text.clone(), None));
                self.sent.push_back(now);
                Some(n.text)
            }
        }
    }

    /// Waiting notes whose GAP is up, while there's room.
    fn due(&mut self, now: Instant) -> Vec<String> {
        self.room(now); // forgets what's older than WINDOW
        let mut out = Vec::new();
        for (at, last, wait) in self.subjects.values_mut() {
            if wait.is_some() && now.duration_since(*at) >= GAP && self.sent.len() < BURST {
                let t = wait.take().unwrap();
                (*at, *last) = (now, t.clone());
                self.sent.push_back(now);
                out.push(t);
            }
        }
        out
    }
}

static TX: Mutex<Option<Sender<Note>>> = Mutex::new(None);
/// Notes the bus refused, for the main loop to show as SteamVR toasts (`tick`).
static TOASTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Says text in the headset, unless settings.json turns notifications off. subject groups the
/// notes that replace each other (a monitor's health).
pub fn send(subject: &str, text: &str) {
    if crate::config::settings()["notifications"] == false {
        return;
    }
    let mut tx = TX.lock().unwrap();
    if tx.is_none() {
        let (t, rx) = channel();
        let started = std::thread::Builder::new().name("cc-notify".into()).spawn(move || run(rx, &mut UserBus::default()));
        if let Err(e) = started {
            eprintln!("notify: no thread ({e}): {text}");
            return;
        }
        *tx = Some(t);
    }
    let _ = tx.as_ref().unwrap().send(Note { subject: subject.into(), text: text.into() });
}

fn run(rx: Receiver<Note>, bus: &mut impl Bus) {
    let mut limit = Limit::default();
    loop {
        let n = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(n) => Some(n),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => return,
        };
        deliver(&mut limit, bus, Instant::now(), n);
    }
}

/// Offers n (if any), sends what's due, and queues as toasts what the bus refused.
fn deliver(limit: &mut Limit, bus: &mut impl Bus, now: Instant, n: Option<Note>) {
    let mut out: Vec<String> = n.and_then(|n| limit.offer(now, n)).into_iter().collect();
    out.extend(limit.due(now));
    for t in out {
        if let Err(e) = bus.notify(&t) {
            eprintln!("notify: the user bus said no ({e}), so it's a SteamVR toast: {t}");
            TOASTS.lock().unwrap().push(t);
        }
    }
}

/// The dashboard tab's overlay, which SteamVR's toasts hang off (0 while there's none).
static DASH: AtomicU64 = AtomicU64::new(0);

/// Every frame, on the main loop (the one thread that calls OpenVR): shows the refused notes as
/// SteamVR's toasts on the dashboard tab's overlay.
pub fn tick(dash: Option<crate::vr::Handle>) {
    DASH.store(dash.map_or(0, |h| h.0), Relaxed);
    let toasts = std::mem::take(&mut *TOASTS.lock().unwrap());
    for t in toasts {
        toast(&t);
    }
}

/// SteamVR's own toast, straight away (the main loop only).
pub fn toast(text: &str) {
    use crate::shell::Shell;
    let h = DASH.load(Relaxed);
    let shown = if h == 0 { Err(None) } else { crate::shell::Live.toast(crate::ids::SurfaceId(h), text) };
    match shown {
        Ok(()) => {}
        Err(None) => eprintln!("notify: no dashboard tab or IVRNotifications, so it's only logged: {text}"),
        Err(Some(e)) => eprintln!("notify: SteamVR's toast failed ({}): {text}", e.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(s: &str, t: &str) -> Note {
        Note { subject: s.into(), text: t.into() }
    }

    #[test]
    fn a_subject_says_each_thing_once_and_ends_on_whats_true() {
        let (mut l, t0) = (Limit::default(), Instant::now());
        assert_eq!(l.offer(t0, note("health a", "a is down")).as_deref(), Some("a is down"));
        assert_eq!(l.offer(t0 + Duration::from_secs(1), note("health a", "a is down")), None, "no news");
        assert_eq!(l.offer(t0 + Duration::from_secs(2), note("health a", "a is back")), None, "inside its gap");
        assert_eq!(l.offer(t0 + Duration::from_secs(3), note("health a", "a is down")), None, "back to what it said: nothing waits");
        assert!(l.due(t0 + GAP).is_empty());
        assert_eq!(l.offer(t0 + Duration::from_secs(4), note("health a", "a is back")), None);
        assert_eq!(l.due(t0 + GAP), vec!["a is back"], "the latest, once its gap is up");
        assert_eq!(l.offer(t0 + GAP, note("health b", "b is down")).as_deref(), Some("b is down"), "another subject doesn't wait");
    }

    #[test]
    fn no_more_than_a_burst_a_minute() {
        let (mut l, t0) = (Limit::default(), Instant::now());
        let sent = (0..6).filter_map(|i| l.offer(t0, note(&format!("s{i}"), "x"))).count();
        assert_eq!(sent, BURST);
        assert!(l.due(t0 + GAP).is_empty(), "the minute isn't up");
        assert!(l.room(t0 + WINDOW));
        assert_eq!(l.due(t0 + WINDOW).len(), 2, "the two that waited");
    }

    struct Fake(Vec<String>, bool);
    impl Bus for Fake {
        fn notify(&mut self, text: &str) -> Result<(), String> {
            self.0.push(text.into());
            if self.1 { Ok(()) } else { Err("no daemon".into()) }
        }
    }

    #[test]
    fn a_refused_note_becomes_a_toast() {
        let (mut l, t0) = (Limit::default(), Instant::now());
        let mut ok = Fake(Vec::new(), true);
        deliver(&mut l, &mut ok, t0, Some(note("pair", "Paired desk")));
        assert_eq!(ok.0, ["Paired desk"]);
        assert!(TOASTS.lock().unwrap().is_empty());
        let mut down = Fake(Vec::new(), false);
        deliver(&mut l, &mut down, t0, Some(note("err", "can't reach desk")));
        assert_eq!(std::mem::take(&mut *TOASTS.lock().unwrap()), ["can't reach desk"]);
    }
}
