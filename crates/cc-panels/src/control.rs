//! The control socket: datagrams on @controlcenter, with replies going back to the sender.
//!   panels    -> "ok", then one line per shown panel:
//!                <name> cx cy cz  xx xy xz  zx zy zz  width height curve
//!                (a window's name is its spot key, app:<desktop file name>)
//!   place <name> cx cy cz yaw pitch roll width curve [height [vcurve]]   (height 0 takes it from
//!                the picture; vcurve > 0 curves it top to bottom with that radius and ignores curve)
//!   place <name> cx cy cz yaw pitch roll width curve   (cc-home's spot format. The name
//!                `taskbar` places the taskbar while it's fixed; `machines`, `prefs` and `workspace` place those windows while they're open)
//!   windows   -> "ok", then one line per Frame window (windows.rs)
//!   hide | show
//!   summon    brings every panel back in sight: nothing minimized (in KWin too), theater
//!             mode off, nothing hidden
//!   viewer connect <name>   connects a remote that wasn't connected at start (autoconnect)
//!   viewer disconnect <name>   ends a remote's session (connect starts it again)
//!   machines show|hide   the Machines window (control/machines.rs), same as its taskbar chip
//!   prefs show|hide   the Preferences window (control/prefs.rs), same as the taskbar's gear
//!   workspace show|hide   the Workspace window (control/workspace.rs), same as its taskbar chip
//!                (opening one closes the others, since they take turns at grab.rs's Extra slot)
//!   workspace reload   re-reads the active workspace from home.json (after cc-home workspace use,
//!                load, machines): connects only its machines and moves every panel to its spots
//!   universe  -> "ok <id>": the room SteamVR is tracking (0 if none yet), for cc-home workspace save-as
//!   head      -> "ok r00 r01 r02 r03 r10 ... r23 <ms>": the headset's 3x4 standing-space pose, row by row
//!             (the 3x4 matrix cc-tip head printed), then milliseconds since cc-panels started; "error head untracked"
//!             while it isn't tracked. There's no OpenVR connection per call, which matters since
//!             the camera scan reads it twice per frame.
//!   tip [left|right|any]   -> "ok x y z hand": a Frame controller's tip right now, in standing space
//!             (cc-home calibrate samples it over half a second. It's what cc-tip did, minus the
//!             OpenVR connection per run)
//!   devices   -> "ok" and a line per tracked device: class, hand role, connected, controller
//!             type, render model (what cc-roles printed)
//!   hud <rgba file> <w> <h> [skip=x0,y0,x1,y1]   the camera scan's status strip (control/hud.rs),
//!             head-locked in the lower third: shows or refreshes it with that raw RGBA picture. A
//!             click on its skip button (pixels) answers "ok skip" on the next refresh
//!   hud mark <rgba file> <w> <h> <width m> <3x4 row-major>   the scan's outlines on the tags,
//!             placed in standing space
//!   hud hide  hides both
//!   taskbar [fixed|follow|wrist]   sets the taskbar's placement, now and in settings.json
//!                (with no argument: "ok <placement>")
//!   quit      closes Desktop, same as the taskbar's power chip or Right Ctrl + Esc. cc-panels ends
//!                and the desktop session stops (session/cc-rest; programs named in `rest_nice` keep running at nice 10).
//!                The next open restores your apps but not their contents (cc-home hibernate)
//!   pop <monitor> <{uuid}>   puts that remote's host window in its own panel (popout.rs; `cc-home
//!                machine window pop`): "ok popping <name>". Errors come later, in the log
//!                and the Machines window's status
pub mod hud;
pub mod machines;
pub mod prefs;
pub mod ui;
pub mod workspace;

const HUD_USAGE: &str = "error usage: hud <rgba file> <w> <h> [skip=x0,y0,x1,y1] | hud mark <rgba file> <w> <h> <width> <12 numbers> | hud hide";

use crate::geometry::{Pose, panel_matrix};
use crate::kvm::KVM;
use crate::{HIDDEN, Source, call, panels, taskbar};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static CONNECT: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new()); // `viewer connect` requests, for main.rs

/// Queues a remote that was left out at start (autoconnect) to connect, from `viewer connect` or
/// its taskbar chip.
pub fn connect_later(i: usize) {
    let mut c = CONNECT.lock().unwrap();
    if !c.contains(&i) {
        c.push(i);
    }
}

/// Opens or closes the Machines, Preferences or Workspace window (its OPEN). Opening one closes
/// the others, since they take turns at grab.rs's Extra slot.
pub fn show(open: &'static AtomicBool, on: bool) {
    if on {
        machines::OPEN.store(false, Relaxed);
        prefs::OPEN.store(false, Relaxed);
        workspace::OPEN.store(false, Relaxed);
    }
    open.store(on, Relaxed);
}

/// The remotes `viewer connect` asked for since the last call. main.rs starts their RDP threads.
pub fn to_connect() -> Vec<usize> {
    std::mem::take(&mut *CONNECT.lock().unwrap())
}

/// "ok" + the pose's 12 numbers (row-major 3x4, six decimals like cc-tip prints them) + a
/// monotonic millisecond timestamp, so a client can tell two replies apart in time.
fn head_line(m: &crate::geometry::Mat, ms: u128) -> String {
    let mut out = String::from("ok");
    for v in m.iter().flatten() {
        out += &format!(" {v:.6}");
    }
    format!("{out} {ms}")
}

/// A request wakes the main loop from its idle wait (up to IDLE_TICK, main.rs), so replies take a
/// few ms however slowly it's ticking. Live, a scan with every panel hidden was stuck waiting on
/// late replies. A thread waits for the socket to be readable and calls vr::wake (once, until the
/// loop looks).
fn wake_on_request(s: &UnixDatagram) {
    use std::os::fd::AsRawFd;
    let Ok(w) = s.try_clone() else { return };
    let _ = std::thread::Builder::new().name("cc-control-wake".into()).spawn(move || {
        let mut p = libc::pollfd { fd: w.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            if unsafe { libc::poll(&mut p, 1, -1) } > 0 {
                crate::vr::wake();
            }
            // the socket stays readable until the loop drains it (and a failing poll returns right
            // away), so look again a little later instead of spinning
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
}

pub fn open() -> Option<UnixDatagram> {
    START.get_or_init(Instant::now);
    let s = SocketAddr::from_abstract_name(b"controlcenter").and_then(|a| UnixDatagram::bind_addr(&a));
    match s.and_then(|s| s.set_nonblocking(true).map(|_| s)) {
        Ok(s) => {
            wake_on_request(&s);
            Some(s)
        }
        Err(e) => {
            eprintln!("control socket: {e}");
            None
        }
    }
}

pub fn handle(s: &UnixDatagram, grab: &mut crate::grab::Grab, windows: &mut crate::windows::Windows, bar: &mut taskbar::Taskbar) {
    hud::tick(); // once a frame, so a HUD whose scan died goes away
    let mut buf = [0u8; 1024];
    while let Ok((n, from)) = s.recv_from(&mut buf) {
        let reply = command(&String::from_utf8_lossy(&buf[..n]), grab, windows, bar);
        let _ = s.send_to_addr(reply.as_bytes(), &from);
    }
}

pub fn show_panels(show: bool) {
    HIDDEN.store(!show, Relaxed);
    for p in panels() {
        if show && p.live() && !p.away() {
            call!(ov, ShowOverlay, p.overlay);
        } else {
            call!(ov, HideOverlay, p.overlay);
        }
    }
}

pub fn command(cmd: &str, grab: &mut crate::grab::Grab, windows: &mut crate::windows::Windows, bar: &mut taskbar::Taskbar) -> String {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    match words.first().copied() {
        Some("panels") => {
            let k = KVM.lock().unwrap();
            let mut out = String::from("ok");
            for (p, l) in panels().iter().zip(&k.place).filter(|(p, _)| p.live()) {
                out += &format!(
                    "\n{} {:.4} {:.4} {:.4}  {:.5} {:.5} {:.5}  {:.5} {:.5} {:.5}  {:.4} {:.4} {:.3} {:.3}",
                    p.spot_key(), l.c[0], l.c[1], l.c[2], l.x[0], l.x[1], l.x[2], l.z[0], l.z[1], l.z[2], l.width, l.height,
                    if l.vert { 0.0 } else { l.curve }, if l.vert { l.curve } else { 0.0 } // curve, vcurve
                );
            }
            out
        }
        Some("place") => {
            let n: Vec<f64> = words.get(2..).unwrap_or(&[]).iter().filter_map(|w| w.parse().ok().filter(|v: &f64| v.is_finite())).collect();
            if !(8..=10).contains(&n.len()) || n.len() != words.len() - 2 {
                return "error usage: place <name> cx cy cz yaw pitch roll width curve [height [vcurve]]".into();
            }
            let vcurve = n.get(9).copied().filter(|&v| v > 0.0);
            let pose = Pose {
                centre: [n[0], n[1], n[2]], yaw: n[3], pitch: n[4], roll: n[5], width: n[6],
                curve: vcurve.unwrap_or(n[7]), vert: vcurve.is_some(),
            };
            if words[1] == "taskbar" {
                bar.place(&pose);
                return "ok".into();
            }
            if let w @ ("machines" | "prefs" | "workspace") = words[1] {
                grab.place_extra(w, &panel_matrix(&pose), pose.width, pose.curve);
                return "ok".into();
            }
            match panels().iter().find(|p| p.live() && p.spot_key() == words[1]) {
                Some(p) => {
                    grab.cancel(p.index); // a recall beats a carry
                    // a window's height comes from the save (its window follows, windows.rs); a
                    // remote screen's comes from its monitor's shape
                    p.set_vert(pose.vert && matches!(p.src, Source::Rdp)); // set_pose turns the panel for that
                    match (n.get(8).filter(|&&h| h > 0.0), &p.src) {
                        (Some(&h), Source::Window(_)) => KVM.lock().unwrap().set_place(p.index, &panel_matrix(&pose), pose.width, h, pose.curve),
                        _ => KVM.lock().unwrap().set_pose(p.index, &panel_matrix(&pose), pose.width, pose.curve),
                    }
                    if matches!(p.src, Source::Rdp) {
                        bar.refix(); // a spot was applied, so the fixed taskbar goes back under the panels
                    }
                    "ok".into()
                }
                None => format!("error no panel {}", words[1]),
            }
        }
        Some("windows") => windows.list(),
        Some("hud") => match words.get(1..) {
            Some(["hide"]) => hud::hide(),
            Some([path, w, h, skip @ ..]) if skip.len() <= 1 && !path.starts_with("mark") => {
                // skip=x0,y0,x1,y1: the strip's skip button in picture pixels
                let rect: Option<Vec<f32>> = skip.first().and_then(|s| s.strip_prefix("skip=")).map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect());
                match (w.parse(), h.parse(), rect) {
                    (Ok(w), Ok(h), None) => hud::show(hud::Kind::Status, path, w, h, None, None),
                    (Ok(w), Ok(h), Some(r)) if r.len() == 4 => hud::show(hud::Kind::Status, path, w, h, None, Some([r[0], r[1], r[2], r[3]])),
                    _ => HUD_USAGE.into(),
                }
            }
            Some(["mark", path, w, h, rest @ ..]) if rest.len() == 13 => {
                let n: Vec<f32> = rest.iter().filter_map(|v| v.parse().ok().filter(|v: &f32| v.is_finite())).collect();
                match (w.parse(), h.parse(), n.len()) {
                    (Ok(w), Ok(h), 13) => {
                        let m = [[n[1], n[2], n[3], n[4]], [n[5], n[6], n[7], n[8]], [n[9], n[10], n[11], n[12]]];
                        hud::show(hud::Kind::Mark, path, w, h, Some((n[0], m)), None)
                    }
                    _ => HUD_USAGE.into(),
                }
            }
            _ => HUD_USAGE.into(),
        },
        Some("viewer") => match (words.get(1).copied(), words.get(2)) {
            (Some("connect"), Some(name)) => match panels().iter().find(|p| matches!(p.src, Source::Rdp) && p.used() && p.v.name == *name) {
                None => format!("error no remote {name}"),
                Some(p) if p.live() => format!("error {name} is connected already"),
                Some(p) => {
                    connect_later(p.index);
                    format!("ok connecting {name}")
                }
            },
            (Some("disconnect"), Some(name)) => match panels().iter().find(|p| matches!(p.src, Source::Rdp) && p.used() && p.v.name == *name) {
                None => format!("error no remote {name}"),
                Some(p) if !p.live() => format!("error {name} isn't connected"),
                Some(p) => {
                    crate::disconnect(p);
                    format!("ok disconnecting {name}")
                }
            },
            _ => "error usage: viewer connect|disconnect <name>".into(),
        },
        Some("workspace") if words.get(1) == Some(&"reload") => {
            workspace::switched(grab, windows, bar);
            "ok workspace reload".into()
        }
        Some("universe") => format!("ok {}", crate::config::UNIVERSE.load(Relaxed)),
        Some(win @ ("machines" | "prefs" | "workspace")) => match words.get(1).copied() {
            Some(w @ ("show" | "hide")) => {
                if win == "machines" && w == "show" {
                    machines::scope(None); // back to the active workspace's
                }
                show(match win {
                    "prefs" => &prefs::OPEN,
                    "workspace" => &workspace::OPEN,
                    _ => &machines::OPEN,
                }, w == "show");
                format!("ok {win} {w}")
            }
            _ => format!("error usage: {win} show|hide"),
        },
        Some("tip") => match crate::vr::tip(words.get(1).copied().unwrap_or("any")) {
            Some((p, hand)) => format!("ok {:.5} {:.5} {:.5} {hand}", p[0], p[1], p[2]),
            None => "error no such Frame controller tracked".into(),
        },
        Some("devices") => format!("ok\n{}", crate::vr::devices().join("\n")),
        Some("head") => match crate::vr::head() {
            Some(m) => head_line(&m, START.get_or_init(Instant::now).elapsed().as_millis()),
            None => "error head untracked".into(),
        },
        Some(w @ ("hide" | "show")) => {
            show_panels(w == "show");
            format!("ok {w}")
        }
        Some("summon") => {
            windows.summon(grab); // un-minimizes its windows in KWin too
            for p in panels() {
                p.set_minimized(false);
            }
            show_panels(true);
            "ok summon".into()
        }
        Some("taskbar") => match words.get(1) {
            None => format!("ok {}", bar.mode().name()),
            Some(w) => match taskbar::Mode::parse(w) {
                Some(m) => match bar.choose(m) {
                    Ok(()) => format!("ok taskbar {}", m.name()),
                    Err(e) => format!("error taskbar {} now, but {e}; not written", m.name()),
                },
                None => "error usage: taskbar fixed|follow|wrist".into(),
            },
        },
        Some("quit") => {
            crate::close_desktop("control");
            "ok quitting".into()
        }
        Some("pop") => match words.get(1..) {
            Some([monitor, uuid]) => crate::popout::pop(monitor, uuid),
            _ => "error usage: pop <monitor> <{uuid}>".into(),
        },
        _ => "error unknown command".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{head_line, machines, prefs, show};
    use std::sync::atomic::Ordering::Relaxed;

    #[test]
    fn showing_one_window_closes_the_other() {
        show(&machines::OPEN, true);
        show(&prefs::OPEN, true);
        assert!(prefs::OPEN.load(Relaxed) && !machines::OPEN.load(Relaxed));
        show(&machines::OPEN, true);
        assert!(machines::OPEN.load(Relaxed) && !prefs::OPEN.load(Relaxed));
        show(&prefs::OPEN, false);
        assert!(machines::OPEN.load(Relaxed), "closing one leaves the other");
        show(&machines::OPEN, false);
    }

    #[test]
    fn head_reply_is_twelve_row_major_numbers_then_milliseconds() {
        let m = [[1.0, 0.0, 0.0, 0.25], [0.0, 1.0, 0.0, 1.7], [0.0, 0.0, 1.0, -0.5]];
        assert_eq!(
            head_line(&m, 1234),
            "ok 1.000000 0.000000 0.000000 0.250000 0.000000 1.000000 0.000000 1.700000 0.000000 0.000000 1.000000 -0.500000 1234"
        );
    }
}
