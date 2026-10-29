//! The Frame's performance profile, through steamos-manager on the user bus
//! (com.steampowered.SteamOSManager1, PerformanceProfile1.PerformanceProfile: "Performance",
//! "Balanced" or "Powersave"; there's no LowPowerMode on the Frame). While the Desktop is closed or
//! you've taken the headset off, and no game is playing, it goes to Powersave, and back to the
//! profile it found once that's over. Never during a game, VR or 2D: that costs frame rate, and
//! Steam sets its own profile per game.
//!
//! It doesn't fight anyone: if the profile changes while it's in Powersave (Steam, a game, you in
//! the Quick Access menu), it lets go and won't put anything back, until the next idle spell. The
//! profile it found is kept in ~/.cache/control-center/power-profile too, so a crash or a kill
//! can't leave the Frame in Powersave: the next start puts it back. With no steamos-manager it does
//! nothing. settings.json `"power_saving": false` turns it off.
//!
//! The Desktop sets `want` from the headset (main.rs), the tab's process from the Desktop being
//! closed (tab.rs); each keeps its own found profile, and the tab's only wants Powersave while the
//! Desktop is closed, so they take turns. A thread (cc-power) does the D-Bus calls, every POLL.
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

const LOW: &str = "Powersave";
const POLL: Duration = Duration::from_secs(2);
/// After a set steamos-manager refused: wait this long before the next try, then the next, then
/// the last one for good.
const BACKOFF: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(30), Duration::from_secs(300)];

/// How long to wait after `fails` refused sets in a row (0: not at all).
fn backoff(fails: usize) -> Duration {
    fails.checked_sub(1).map_or(Duration::ZERO, |i| BACKOFF[i.min(BACKOFF.len() - 1)])
}
const IFACE: &str = "com.steampowered.SteamOSManager1.PerformanceProfile1";

static WANT: AtomicBool = AtomicBool::new(false);
static GAME: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
/// Wakes the thread the moment `want` or `game` changes, so a headset put back on or a game
/// starting gets its profile back straight away, not up to POLL later.
static WAKE: (std::sync::Mutex<bool>, std::sync::Condvar) = (std::sync::Mutex::new(false), std::sync::Condvar::new());

fn wake() {
    *WAKE.0.lock().unwrap() = true;
    WAKE.1.notify_one();
}
/// Which process this is ("desktop" or "tab"), for its own copy of the found profile: the two
/// take turns, the tab's only while the Desktop is closed.
static ROLE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Nobody's using the Desktop (it's closed, or the headset is off). Called as that changes.
pub fn want(idle: bool) {
    if WANT.swap(idle, Relaxed) != idle {
        wake();
    }
}

/// A game is playing (VR or 2D), so no Powersave whatever `want` says.
pub fn game(on: bool) {
    if GAME.swap(on, Relaxed) != on {
        wake();
    }
}

/// What the profile should become now, if anything.
#[derive(Debug, PartialEq)]
pub enum Act {
    Set(String),
    Nothing,
}

/// The rule, with what it remembers: the profile it found (to put back), and whether it let go.
#[derive(Default, Debug, PartialEq)]
pub struct Saver {
    pub found: Option<String>,
    backed_off: bool, // someone else changed it during this idle spell: hands off until it ends
}

impl Saver {
    /// One look: `low` is whether Powersave is wanted, `now` the profile as it is. It says what to
    /// set; `commit` records it once the set has gone through.
    pub fn step(&mut self, low: bool, now: &str) -> Act {
        match (&self.found, low) {
            (None, true) if self.backed_off || now == LOW => Act::Nothing, // let go, or already low (nothing of ours to undo)
            (None, true) => Act::Set(LOW.into()),
            (Some(_), true) if now != LOW => {
                (self.found, self.backed_off) = (None, true); // changed under us: theirs now
                Act::Nothing
            }
            (Some(_), true) => Act::Nothing,
            (Some(was), false) if now == LOW => Act::Set(was.clone()),
            (Some(_), false) => {
                self.found = None; // changed meanwhile: leave theirs
                Act::Nothing
            }
            (None, false) => {
                self.backed_off = false;
                Act::Nothing
            }
        }
    }

    /// What `step` asked for went through (`ok`), or didn't. Only a Powersave that took makes the
    /// profile it replaced (`was`) ours to put back; one that didn't changes nothing, and isn't
    /// someone else's doing. Putting it back clears it; failing to keeps it, to try again.
    pub fn commit(&mut self, act: &Act, was: &str, ok: bool) {
        match act {
            Act::Set(p) if ok && p == LOW => self.found = Some(was.to_string()),
            Act::Set(_) if ok => self.found = None,
            _ => {}
        }
    }
}

/// steamos-manager's profile property on the Frame's main user bus (/run/user/<uid>/bus; the
/// Desktop's own session bus doesn't have it).
struct Manager(zbus::blocking::Connection);

impl Manager {
    fn connect() -> Option<Manager> {
        let addr = format!("unix:path=/run/user/{}/bus", unsafe { libc::getuid() });
        zbus::blocking::connection::Builder::address(addr.as_str()).and_then(|b| b.build()).ok().map(Manager)
    }

    fn props(&self) -> zbus::Result<zbus::blocking::fdo::PropertiesProxy<'_>> {
        zbus::blocking::fdo::PropertiesProxy::builder(&self.0).destination("com.steampowered.SteamOSManager1")?.path("/com/steampowered/SteamOSManager1")?.build()
    }

    fn get(&self) -> Option<String> {
        let iface = zbus::names::InterfaceName::try_from(IFACE).ok()?;
        let v = self.props().ok()?.get(iface, "PerformanceProfile").ok()?;
        String::try_from(v).ok()
    }

    fn set(&self, profile: &str) -> bool {
        let Ok(iface) = zbus::names::InterfaceName::try_from(IFACE) else { return false };
        self.props().and_then(|p| p.set(iface, "PerformanceProfile", zbus::zvariant::Value::from(profile)).map_err(Into::into)).is_ok()
    }
}

fn found_file() -> String {
    crate::config::cache(&format!("power-profile-{}", ROLE.get().copied().unwrap_or("desktop")))
}

/// Starts the thread for this process (`role`), unless settings.json turns it off.
pub fn start(role: &'static str) {
    let _ = ROLE.set(role);
    if crate::config::settings()["power_saving"] == false {
        return;
    }
    let _ = std::thread::Builder::new().name("cc-power".into()).spawn(run);
}

fn run() {
    let Some(m) = Manager::connect() else { return eprintln!("power: no user bus, so the profile is left alone") };
    if m.get().is_none() {
        return eprintln!("power: no steamos-manager performance profile here, so it's left alone");
    }
    let read = |f: &str| std::fs::read_to_string(f).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    // the tab also takes over what a Desktop that died left set, or a game could start in Powersave
    let desktop = crate::config::cache("power-profile-desktop");
    let dead = ROLE.get() == Some(&"tab") && !crate::tab::desktop_running(&crate::tab::desktop_lock()); // a live Desktop's file is its own
    let adopted = dead.then(|| read(&desktop)).flatten();
    let mut s = Saver { found: read(&found_file()).or(adopted.clone()), ..Default::default() };
    if adopted.is_some() {
        let _ = std::fs::write(found_file(), s.found.as_deref().unwrap_or_default());
        let _ = std::fs::remove_file(&desktop);
    }
    if s.found.is_some() {
        eprintln!("power: a profile we set earlier wasn't put back; it will be now");
    }
    let mut refused = false; // said once that a set failed
    let (mut fails, mut last_fail) = (0usize, std::time::Instant::now());
    while !STOP.load(Relaxed) {
        if let Some(now) = m.get() {
            let low = WANT.load(Relaxed) && !GAME.load(Relaxed);
            let had = s.found.clone();
            let act = s.step(low, &now);
            if let Act::Set(p) = &act
                && last_fail.elapsed() >= backoff(fails)
            {
                let ok = m.set(p);
                (fails, last_fail) = if ok { (0, last_fail) } else { (fails + 1, std::time::Instant::now()) };
                s.commit(&act, &now, ok);
                match ok {
                    true => eprintln!("power: {now} -> {p} ({})", if p == LOW { "nobody's using the Desktop" } else { "back to what it was" }),
                    false if !std::mem::replace(&mut refused, true) => eprintln!("power: steamos-manager wouldn't set {p}; trying again quietly"),
                    false => {}
                }
                refused &= !ok;
            }
            if s.found != had {
                match &s.found {
                    Some(p) => {
                        let _ = std::fs::write(found_file(), p);
                    }
                    None => {
                        let _ = std::fs::remove_file(found_file());
                    }
                }
            }
        }
        // wait_timeout_while loops over spurious wake-ups itself; the next pass reads want and game afresh
        let woken = WAKE.0.lock().unwrap();
        let (mut woken, _) = WAKE.1.wait_timeout_while(woken, POLL, |w| !*w).unwrap();
        *woken = false;
    }
}

/// On the way out: puts the profile back now if we set it (the thread may be mid-sleep).
pub fn stop() {
    STOP.store(true, Relaxed);
    let Some(p) = std::fs::read_to_string(found_file()).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else { return };
    if let Some(m) = Manager::connect()
        && m.get().as_deref() == Some(LOW)
        && m.set(&p)
    {
        eprintln!("power: back to {p} on the way out");
    }
    let _ = std::fs::remove_file(found_file());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_saves_powersaves_and_puts_back_what_it_found() {
        let mut s = Saver::default();
        assert_eq!(s.step(false, "Performance"), Act::Nothing, "in use: nothing");
        let a = s.step(true, "Performance");
        assert_eq!(a, Act::Set(LOW.into()));
        assert_eq!(s.found, None, "not ours until the set goes through");
        s.commit(&a, "Performance", true);
        assert_eq!(s.found.as_deref(), Some("Performance"));
        assert_eq!(s.step(true, LOW), Act::Nothing, "stays");
        let a = s.step(false, LOW);
        assert_eq!(a, Act::Set("Performance".into()), "back to what it was");
        s.commit(&a, LOW, true);
        assert_eq!(s, Saver::default());
    }

    #[test]
    fn it_backs_off_when_someone_else_changes_it() {
        let mut s = Saver::default();
        let a = s.step(true, "Balanced");
        s.commit(&a, "Balanced", true);
        assert_eq!(s.step(true, "Performance"), Act::Nothing, "Steam or you changed it");
        assert_eq!(s.step(true, "Performance"), Act::Nothing, "and it doesn't take it back this spell");
        assert_eq!(s.step(false, "Performance"), Act::Nothing, "nothing to put back");
        assert_eq!(s.step(true, "Performance"), Act::Set(LOW.into()), "the next idle spell is ours again");
        // changed while it's going back: theirs stays
        let mut s = Saver::default();
        let a = s.step(true, "Balanced");
        s.commit(&a, "Balanced", true);
        assert_eq!(s.step(false, "Performance"), Act::Nothing);
        assert_eq!(s.found, None);
    }

    #[test]
    fn a_set_that_fails_isnt_taken_as_someone_elses_change() {
        let mut s = Saver::default();
        let a = s.step(true, "Performance");
        s.commit(&a, "Performance", false); // polkit, or steamos-manager busy
        assert_eq!(s, Saver::default(), "nothing of ours, nothing backed off");
        assert_eq!(s.step(true, "Performance"), Act::Set(LOW.into()), "it tries again next time");
        let a = s.step(true, "Performance");
        s.commit(&a, "Performance", true);
        let back = s.step(false, LOW);
        s.commit(&back, LOW, false);
        assert_eq!(s.found.as_deref(), Some("Performance"), "a failed put-back keeps what to put back");
        assert_eq!(s.step(false, LOW), Act::Set("Performance".into()), "and tries again");
    }

    #[test]
    fn a_refused_set_waits_longer_each_time() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!((backoff(1), backoff(2), backoff(3), backoff(9)), (BACKOFF[0], BACKOFF[1], BACKOFF[2], BACKOFF[2]), "5 s, 30 s, then every 5 minutes");
    }

    #[test]
    fn already_powersave_is_left_alone() {
        let mut s = Saver::default();
        assert_eq!(s.step(true, LOW), Act::Nothing);
        assert_eq!(s.step(false, LOW), Act::Nothing, "it wasn't ours, so it isn't undone");
    }
}
