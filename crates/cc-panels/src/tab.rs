//! `cc-panels --tab`: Command Center's dashboard tab while the Desktop is closed, so it's always
//! there next to Steam's. SteamVR starts it with itself, because command-center.vrmanifest (next to
//! cc-panels' root: /usr/lib/command-center in the image, the checkout otherwise) says it's a
//! dashboard overlay app and it's set to auto-launch. It's a small overlay-only process: no
//! panels, no mice or keyboards, no laser, no action manifest. Its page is the Desktop's own tab
//! (control/dashtab.rs) in its closed mode, where Show opens the Desktop.
//!
//! The two never fight over the tab's key: a Desktop cc-panels holds an flock on
//! /run/user/<uid>/control-center-desktop.lock for as long as it runs, and this one tries that
//! lock every LOOK. It lets go of the tab the moment the lock's taken, and makes it again once
//! it's free. The Desktop's tab tries every half second until the key's free.
//!
//! The manifest is registered once, at install (`--register-tab`, one OpenVR connection) while
//! SteamVR runs, else by the Desktop's next start (`ensure_registered`, inside the connection it
//! has anyway). `--unregister-tab` takes it out at uninstall.
use crate::control::dashtab::Dashtab;
use crate::{call, config, vr};
use openvr_sys as sys;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often it looks at the Desktop's lock. A shared try and a let-go cost next to nothing.
const LOOK: Duration = Duration::from_millis(250);

/// Our app key in SteamVR, as the manifest says.
pub const APP_KEY: &str = "controlcenter.tab";

/// The manifest's place: beside the root, so the image and a checkout agree on where it is.
pub fn manifest() -> PathBuf {
    crate::root().join("command-center.vrmanifest")
}

/// The Desktop's lock, which a Desktop cc-panels holds for as long as it runs (main.rs). It's in
/// the Frame's own runtime dir, /run/user/<uid>, which this and the Desktop both see; the
/// Desktop's nested session has a runtime dir of its own.
pub fn desktop_lock() -> PathBuf {
    PathBuf::from(format!("/run/user/{}/control-center-desktop.lock", unsafe { libc::getuid() }))
}

/// Takes the Desktop's lock for as long as the file stays open. It waits for it: this tab only
/// ever holds it for the instant it takes to look.
pub fn hold(path: &Path) -> Result<std::fs::File, String> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&f), libc::LOCK_EX) } != 0 {
        return Err(format!("{}: {}", path.display(), std::io::Error::last_os_error()));
    }
    Ok(f)
}

/// Whether a Desktop holds its lock: try it shared without waiting, and let go at once.
fn desktop_running(path: &Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path) else { return false };
    let fd = std::os::fd::AsRawFd::as_raw_fd(&f);
    if unsafe { libc::flock(fd, libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        return true;
    }
    unsafe { libc::flock(fd, libc::LOCK_UN) };
    false
}

pub fn run() -> Result<(), String> {
    vr::init().map_err(|e| format!("SteamVR: {e}"))?;
    // SteamVR started a wrapper (cc-tab, and cc-box's container on a checkout install), not us,
    // so say which app this process is, or it can't tell the tab's app is running.
    let key = CString::new(APP_KEY).unwrap();
    let e = call!(apps, IdentifyApplication, std::process::id(), key.as_ptr() as *mut _);
    if e != 0 {
        eprintln!("tab: SteamVR didn't take our app key ({e}); the tab works anyway");
    }
    let _ = crate::theme::init();
    let assets = format!("{}/.cache/control-center/assets", config::home_dir());
    let tags: Vec<String> = crate::control::dashtab::LABELS.iter().map(|(k, text)| format!("tag=ui-{k},{text},frame,0,0,0")).collect();
    crate::assets::draw(&assets, &crate::theme::font(), &tags); // the Desktop may never have run to draw them
    let mut tab = Dashtab::new(&assets);
    tab.closed = true;
    let mut hand = crate::handoff::Handoff::default();
    let (mut desktop, mut looked, lock) = (false, None::<Instant>, desktop_lock());
    eprintln!("tab: up");
    loop {
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        while call!(sys, PollNextEvent, &mut e, size_of::<vr::VREvent_t>() as u32) {
            match e.eventType {
                sys::EVREventType_VREvent_Quit => {
                    tab.destroy();
                    vr::shutdown();
                    eprintln!("tab: SteamVR is quitting");
                    return Ok(());
                }
                sys::EVREventType_VREvent_DashboardActivated => crate::control::dashtab::dashboard(true),
                sys::EVREventType_VREvent_DashboardDeactivated => crate::control::dashtab::dashboard(false),
                _ => {}
            }
        }
        if looked.is_none_or(|t| t.elapsed() >= LOOK) {
            (desktop, looked) = (desktop_running(&lock), Some(Instant::now()));
        }
        if desktop {
            tab.release();
        } else {
            tab.tick(&mut hand);
        }
        if tab.quit {
            tab.destroy();
            vr::shutdown();
            eprintln!("tab: closed from SteamVR's bar; SteamVR starts it again with itself next time");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(if tab.moving() { 11 } else { 100 }));
    }
}

/// Adds the manifest and turns on its auto-launch, in the connection `apps` is from. The
/// auto-launch is only set here, when it's first added, so turning it off in SteamVR sticks.
fn add(path: &Path) -> Result<(), String> {
    let (p, key) = (CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?, CString::new(APP_KEY).unwrap());
    match call!(apps, AddApplicationManifest, p.as_ptr() as *mut _, false) {
        0 => {}
        e => return Err(format!("SteamVR didn't add {} ({e})", path.display())),
    }
    match call!(apps, SetApplicationAutoLaunch, key.as_ptr() as *mut _, true) {
        0 => Ok(()),
        e => Err(format!("SteamVR added the tab but won't start it with itself ({e})")),
    }
}

/// At the Desktop's start, inside its own connection: if SteamVR lost our manifest (a steamreset,
/// or an install while SteamVR was off), it goes back in.
pub fn ensure_registered() {
    let path = manifest();
    let key = CString::new(APP_KEY).unwrap();
    if !path.is_file() || call!(apps, IsApplicationInstalled, key.as_ptr() as *mut _) {
        return;
    }
    match add(&path) {
        Ok(()) => eprintln!("tab: registered {} with SteamVR, so the tab shows while the Desktop is closed", path.display()),
        Err(e) => eprintln!("tab: {e}"),
    }
}

/// `--register-tab` and `--unregister-tab` (cc-home install): one connection as a utility app.
pub fn register(on: bool) -> Result<(), String> {
    vr::init_as(sys::EVRApplicationType_VRApplication_Utility).map_err(|e| format!("SteamVR: {e}"))?;
    let path = manifest();
    let r = if on {
        add(&path)
    } else {
        let p = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        match call!(apps, RemoveApplicationManifest, p.as_ptr() as *mut _) {
            0 => Ok(()),
            e => Err(format!("SteamVR didn't remove {} ({e})", path.display())),
        }
    };
    vr::shutdown();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_starting_desktop_waits_for_the_lock_instead_of_giving_up() {
        let dir = std::env::temp_dir().join(format!("cc-tab-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control-center-desktop.lock");
        let held = hold(&path).unwrap(); // the update helper, mid-refresh
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || tx.send(hold(&p).is_ok()).unwrap());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "it's waiting, not starting and not giving up");
        drop(held);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true), "and it starts once the lock is free");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_desktops_lock_says_whether_it_runs() {
        let dir = std::env::temp_dir().join(format!("cc-tab-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control-center-desktop.lock");
        assert!(!desktop_running(&path), "nobody holds it");
        let held = hold(&path).unwrap();
        assert!(desktop_running(&path), "a Desktop holds it");
        assert!(desktop_running(&path), "and looking doesn't take it from the Desktop");
        drop(held);
        assert!(!desktop_running(&path), "the Desktop's gone");
        let again = hold(&path).unwrap(); // a look let go of its try, so a new Desktop gets it straight away
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
