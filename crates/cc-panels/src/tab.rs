//! `cc-panels --tab`: Command Center's dashboard tab while the Desktop is closed, so it's always
//! there next to Steam's. SteamVR starts it with itself, because command-center.vrmanifest (next to
//! cc-panels' root: /usr/lib/command-center in the image, the checkout otherwise) says it's a
//! dashboard overlay app and it's set to auto-launch. It's a small overlay-only process: no
//! panels, no mice or keyboards, no laser, no action manifest. Its page is the Desktop's own tab
//! (control/dashtab.rs) in its closed mode, where Show opens the Desktop.
//!
//! The two never fight over the tab's key: this one lets go of it as soon as a Desktop cc-panels
//! runs, and makes it again once that's gone. The Desktop's tab tries every 2 s until it's free.
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

/// Our app key in SteamVR, as the manifest says.
pub const APP_KEY: &str = "controlcenter.tab";

/// The manifest's place: beside the root, so the image and a checkout agree on where it is.
pub fn manifest() -> PathBuf {
    crate::root().join("command-center.vrmanifest")
}

/// Whether a Desktop cc-panels (not one of these) is running, from /proc's command lines.
fn desktop_running() -> bool {
    let me = std::process::id();
    let lines = std::fs::read_dir("/proc").into_iter().flatten().flatten().filter(|e| e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()).is_some_and(|p| p != me));
    lines.filter_map(|e| std::fs::read(e.path().join("cmdline")).ok()).any(|c| is_desktop(&c))
}

/// A NUL-separated command line that's cc-panels running the Desktop: its program is cc-panels
/// and it isn't `--tab` or a one-off (`--assets`, `--register-tab`, `--unregister-tab`).
fn is_desktop(cmdline: &[u8]) -> bool {
    let mut args = cmdline.split(|&b| b == 0).filter(|a| !a.is_empty());
    let Some(prog) = args.next() else { return false };
    Path::new(std::str::from_utf8(prog).unwrap_or("")).file_name().is_some_and(|n| n == "cc-panels")
        && !args.any(|a| a.starts_with(b"--tab") || a.starts_with(b"--assets") || a.ends_with(b"register-tab"))
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
    let (mut desktop, mut looked) = (false, None::<Instant>);
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
        if looked.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            (desktop, looked) = (desktop_running(), Some(Instant::now()));
        }
        if desktop {
            tab.release();
        } else {
            tab.tick(&mut hand);
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
    fn only_a_desktop_cc_panels_counts() {
        assert!(is_desktop(b"/usr/lib/command-center/target/release/cc-panels\0--for\x000\x00"));
        assert!(is_desktop(b"cc-panels\0"));
        assert!(!is_desktop(b"/usr/lib/command-center/target/release/cc-panels\0--tab\0"), "this one");
        assert!(!is_desktop(b"cc-panels\0--register-tab\0"));
        assert!(!is_desktop(b"cc-panels\0--assets\0/x\0Noto Sans\0"));
        assert!(!is_desktop(b"/usr/bin/cc-home\0panels\0"), "cc-home's wrapper isn't it");
        assert!(!is_desktop(b""));
    }
}
