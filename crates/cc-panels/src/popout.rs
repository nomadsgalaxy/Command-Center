//! Remote windows popped out of a monitor's panel (docs/remote-windows.md, spike 1).
//!
//! `pop <monitor> <uuid>` (control.rs) fills a spare remote slot (main.rs SPARES, same as Add
//! machine) with a viewer of that host window's own krdp server. The agent's `window start`
//! runs on the slot's RDP thread (rdp.rs session), and the panel shows up in front of you near
//! the monitor. After that it's an ordinary remote panel with a card, input and levels.
//!
//! The card's x or `viewer disconnect` stops the window's server and frees the slot. The window
//! closing on the host does the same, after showing "window closed" for a few seconds.
use crate::geometry::{Placement, Pose, angles, panel_matrix};
use crate::kvm::{KVM, Kvm};
use crate::{Panel, QUIT, Source, config, panel, panels};
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

/// W5: the panel pressed last on each machine, as (machine, panel). The window server is click
/// to focus, so an inactive window's first press only activates it and gets swallowed. That's
/// why we track it: a pop-out that was pressed last of its machine's panels counts as active,
/// and a press on any other of them (a monitor, another pop-out) takes its focus away.
/// ponytail: approximate, since we don't see the host's own clicks or focus changes. If it
/// misleads, have the window server send us its real active state.
static FOCUS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
/// Streams that arrived (first, at connect) or changed size, queued for the main loop as
/// (panel, w, h, first).
static SIZED: Mutex<Vec<(usize, u32, u32, bool)>> = Mutex::new(Vec::new());
/// The last error or closed window, for the Machines window's status.
static NEWS: Mutex<Option<String>> = Mutex::new(None);
const IDLE: &str = "-idle"; // tag suffixes: none for the name alone (active), IDLE adds the focus hint, CLOSED
const CLOSED: &str = "-closed";
const CLOSED_FOR: Duration = Duration::from_secs(3);

/// KWin's braced uuid in lower-case hex, which is the form the agent takes.
pub fn uuid_ok(u: &str) -> bool {
    let Some(h) = u.strip_prefix('{').and_then(|u| u.strip_suffix('}')) else { return false };
    h.len() == 36 && h.char_indices().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_digit() || ('a'..='f').contains(&c) })
}

/// A pop-out's name doubles as its spot key. It's the machine plus the window, so the window
/// gets its place back next time.
pub fn name_for(machine: &str, uuid: &str) -> String {
    format!("pop-{machine}-{}", uuid.trim_start_matches('{').trim_end_matches('}'))
}

/// What the card says: the app, and which monitor it came from (by the user's name for it).
pub fn shown_name(app: Option<&str>, monitor: &str) -> String {
    match app {
        Some(a) => format!("{a} (from {monitor})"),
        None => format!("window from {monitor}"),
    }
}

/// The window's app id, out of the agent's `window list` answer (`windows`: [{uuid, app, ...}]).
pub fn app_of(list: &serde_json::Value, uuid: &str) -> Option<String> {
    let w = list["windows"].as_array()?.iter().find(|w| w["uuid"] == uuid)?;
    w["app"].as_str().filter(|a| !a.is_empty()).map(str::to_string)
}

/// In front of you near the source monitor: a fifth of the way from it to your head, facing you
/// and as wide as the monitor. Once the stream's size comes in, tick scales it to the window.
pub fn in_front(src: &Placement, head: [f64; 3]) -> Pose {
    let centre = [0, 1, 2].map(|i| head[i] + (src.c[i] - head[i]) * 0.8);
    let (yaw, pitch) = angles(&[0, 1, 2].map(|i| centre[i] - head[i]));
    Pose { centre, yaw, pitch, width: src.width, ..Default::default() }
}

/// `pop <monitor> <uuid>`: a spare slot takes the window, gets placed, and connects on its own
/// RDP thread (main.rs, via control's connect_later). Server errors show up later, in failed.
pub fn pop(monitor: &str, uuid: &str) -> String {
    if !uuid_ok(uuid) {
        return format!("error bad uuid {uuid}: braced lower-case hex, as `cc-home machine window list` gives");
    }
    let Some(src) = panels().iter().find(|p| matches!(p.src, Source::Rdp) && p.used() && p.v.pop.is_none() && p.v.name == monitor) else {
        return format!("error no remote {monitor}");
    };
    let name = name_for(config::machine_of(&src.v), uuid);
    if panels().iter().any(|p| p.used() && p.v.name == name) {
        return format!("error that window is popped out already ({name})");
    }
    let Some(p) = panels().iter().find(|p| matches!(p.src, Source::Rdp) && p.v.name.is_empty()) else {
        return "error no free slot: close a pop-out, or restart cc-panels".into();
    };
    // the source machine's login, password and pin, and its size until the stream's size arrives
    let v = config::Viewer { name: name.clone(), port: 0, screen: 0, auto: false, label: String::new(), pop: Some((monitor.into(), uuid.into())), ..src.v.clone() };
    if !p.fill(v) {
        return "error that slot was taken".into();
    }
    {
        // the monitor was pressed last, so the window is likely the one clicked there: call it active
        let mut f = FOCUS.lock().unwrap();
        f.retain(|e| e.1 != p.index);
        f.iter_mut().filter(|e| e.1 == src.index).for_each(|e| e.1 = p.index);
    }
    {
        let mut k = KVM.lock().unwrap();
        let pose = config::home_pose(&name, None).unwrap_or_else(|| in_front(&k.place[src.index], crate::vr::head_position()));
        p.set_vert(pose.vert);
        k.set_pose(p.index, &panel_matrix(&pose), pose.width, pose.curve);
    }
    crate::control::connect_later(p.index);
    eprintln!("{name}: popping out {uuid} from {monitor}");
    format!("ok popping {name}")
}

/// Runs on its RDP thread before the session starts: gets the name from the host's window list
/// and draws the tags.
pub fn prepare(p: &Panel) {
    let v: &config::Viewer = &p.v;
    let Some((from, uuid)) = &v.pop else { return };
    let list = crate::rdp::agent(config::machine_of(v), "window", serde_json::json!({"op": "list"})).unwrap_or_default();
    let app = app_of(&list, uuid);
    let all = config::viewers(&[]);
    let monitor = panels().iter().find(|q| q.used() && q.v.name == *from).map_or_else(|| from.clone(), |q| config::display(&q.v, &all));
    let shown = shown_name(app.as_deref(), &monitor).replace(',', "\u{201a}"); // assets.rs splits its fields on commas
    let [r, g, b] = crate::grab::REMOTE.map(|c| c as u8);
    let tag = |suffix: &str, text: &str| format!("tag={}{suffix},{text},frame,{r},{g},{b}", v.name);
    crate::assets::draw(
        &config::cache("assets"),
        &crate::theme::font(),
        &[tag("", &shown), tag(IDLE, &format!("{shown} \u{b7} inactive: click to focus")), tag(CLOSED, &format!("{shown} \u{b7} window closed"))],
    );
    show_focus(p);
    eprintln!("{}: {shown}", v.name);
}

/// Sets the card's tag: "" (active), IDLE or CLOSED.
fn show(p: &Panel, which: &str) {
    p.set_tag(crate::load_tag(&format!("{}/tag-{}{which}.rgba", config::cache("assets"), p.v.name)).as_ref());
}

/// Sets the card's tag for W5's focus: the name alone when active, otherwise the hint.
fn show_focus(p: &Panel) {
    show(p, if FOCUS.lock().unwrap().iter().any(|e| e.1 == p.index) { "" } else { IDLE });
}

/// A press on panel i (from Panel::mouse, lasers or the mouse) moves W5's focus on its machine to
/// it. A Frame window has no machine, so it moves nothing. An ended pop-out keeps "window closed".
pub fn pressed(i: usize) {
    let Some(m) = panels().get(i).map(|p| config::machine_of(&p.v).to_string()).filter(|m| !m.is_empty()) else { return };
    let was = {
        let mut f = FOCUS.lock().unwrap();
        match f.iter_mut().find(|e| e.0 == m) {
            Some(e) => std::mem::replace(&mut e.1, i),
            None => {
                f.push((m, i));
                usize::MAX
            }
        }
    };
    if was == i {
        return;
    }
    for j in [was, i] {
        if let Some(p) = panels().get(j).filter(|p| p.v.pop.is_some() && p.connected.load(Relaxed)) {
            show(p, if j == i { "" } else { IDLE });
        }
    }
}

fn say(s: String) {
    eprintln!("popout: {s}");
    *NEWS.lock().unwrap() = Some(s);
}

/// News for the Machines window's status since it last checked.
pub fn news() -> Option<String> {
    NEWS.lock().unwrap().take()
}

/// `window start` was refused (windows-off, not-shared, busy, ...): report it and free the slot.
pub fn failed(p: &Panel, why: &str) {
    let from = p.v.pop.as_ref().map_or(String::new(), |x| x.0.clone());
    say(format!("{from}: can't pop out that window ({why})"));
    free(p);
}

/// Makes the slot a spare again and drops W5's focus from it.
fn free(p: &Panel) {
    FOCUS.lock().unwrap().retain(|e| e.1 != p.index);
    eprintln!("{}: slot freed", p.v.name);
    p.empty();
}

/// The session is over (rdp.rs run has already told the window's server to stop). If the card's
/// x or `viewer disconnect` ended it, the slot is freed right away. If the host ended it because
/// the window closed, it shows "window closed" for a few seconds first.
pub fn ended(p: &Panel) {
    if QUIT.load(Relaxed) {
        return;
    }
    if p.live() {
        say(format!("{}: window closed", p.v.pop.as_ref().map_or("", |x| x.0.as_str())));
        show(p, CLOSED);
        for _ in 0..CLOSED_FOR.as_millis() / 100 {
            if QUIT.load(Relaxed) || !p.live() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    free(p);
}

/// The stream's size, from its RDP thread: `first` at connect, or later when it changes (a popup,
/// the window resized). The main loop sizes the panel in tick.
pub fn resized(i: usize, w: u32, h: u32, first: bool) {
    SIZED.lock().unwrap().push((i, w, h, first));
    crate::vr::wake();
}

/// Main loop: applies each new stream size to its panel. The first size uses the Frame's own
/// window scale, later ones keep the pixels' size in metres, and a saved spot's width stays.
pub fn tick(k: &mut Kvm) {
    let sized = std::mem::take(&mut *SIZED.lock().unwrap());
    for (i, w, h, first) in sized {
        let p = panel(i);
        if !p.used() || p.v.pop.is_none() {
            continue; // freed in the meantime
        }
        let (w0, _) = p.size();
        p.set_size(w.max(1), h.max(1));
        if first {
            show_focus(p); // pressed() leaves it alone until it's connected
        }
        let pl = k.place[i];
        let keep = first && config::home_pose(&p.v.name, None).is_some();
        // first: as big as the Frame's own windows (window_px_per_m). Using the monitor's pixel
        // size made a window a third of a metre wide (my words: "it's so small")
        let width = if keep {
            pl.width
        } else if first {
            (w.max(1) as f64 / crate::windows::px_per_m_now()).clamp(0.1, 4.0)
        } else {
            (pl.width * w.max(1) as f64 / w0 as f64).clamp(0.1, 4.0)
        };
        // a change grows or shrinks it from its top-left, so the picture stays under the cursor
        let m = crate::windows::resized(&pl, width, width * h.max(1) as f64 / w.max(1) as f64, !first);
        k.set_pose(i, &m, width, pl.curve);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &str = "{0b9a1c2d-3e4f-4a5b-8c6d-7e8f90a1b2c3}";

    #[test]
    fn uuids_are_kwins_braced_lower_case_form() {
        assert!(uuid_ok(U));
        assert!(!uuid_ok(&U[1..U.len() - 1]), "unbraced");
        assert!(!uuid_ok(&U.to_uppercase()), "upper case");
        assert!(!uuid_ok("{0b9a1c2d-3e4f-4a5b-8c6d-7e8f90a1b2c}"), "short");
        assert!(!uuid_ok("{0b9a1c2d3e4f-4a5b-8c6d-7e8f90a1b2c3-}"), "a dash misplaced");
        assert!(!uuid_ok("{0b9a1c2d-3e4f-4a5b-8c6d-7e8f90a1b2g3}"), "not hex");
        assert!(!uuid_ok("{0b9a1c2d-3e4f-4a5b-8c6d-7e8f90a1b2c3};rm"), "trailing");
    }

    #[test]
    fn pop_validates_before_touching_a_slot() {
        assert!(pop("desk-wide", "nope").starts_with("error bad uuid"));
        assert_eq!(pop("desk-wide", U), "error no remote desk-wide", "no such monitor panel");
    }

    #[test]
    fn a_pop_out_is_named_and_kept_by_machine_and_window() {
        assert_eq!(name_for("desk", U), "pop-desk-0b9a1c2d-3e4f-4a5b-8c6d-7e8f90a1b2c3");
        assert_ne!(name_for("laptop", U), name_for("desk", U));
        assert_eq!(shown_name(Some("org.kde.dolphin"), "Desk left"), "org.kde.dolphin (from Desk left)");
        assert_eq!(shown_name(None, "desk-wide"), "window from desk-wide");
    }

    #[test]
    fn the_window_list_gives_its_app() {
        let list = serde_json::json!({"ok": true, "windows": [
            {"uuid": "{11111111-1111-1111-1111-111111111111}", "app": "org.kde.konsole", "x": 0, "y": 0, "w": 800, "h": 600},
            {"uuid": U, "app": "org.kde.dolphin", "x": 10, "y": 20, "w": 1398, "h": 900},
        ]});
        assert_eq!(app_of(&list, U).as_deref(), Some("org.kde.dolphin"));
        assert_eq!(app_of(&list, "{22222222-2222-2222-2222-222222222222}"), None, "not listed");
        assert_eq!(app_of(&serde_json::json!({"windows": [{"uuid": U, "app": ""}]}), U), None, "no app id");
        assert_eq!(app_of(&serde_json::Value::Null, U), None, "no answer");
    }

    #[test]
    fn it_comes_in_front_of_the_monitor_facing_you() {
        let src = Placement::from_matrix(&panel_matrix(&Pose { centre: [1.0, 1.5, -2.0], yaw: -20.0, width: 1.4, ..Default::default() }), 1.4, 0.56, 0.0);
        let head = [0.0, 1.6, 0.0];
        let pose = in_front(&src, head);
        let d = |a: [f64; 3], b: [f64; 3]| crate::geometry::norm(&[a[0] - b[0], a[1] - b[1], a[2] - b[2]]);
        assert!((d(pose.centre, head) - 0.8 * d(src.c, head)).abs() < 1e-9, "a fifth nearer than the monitor");
        let pl = Placement::from_matrix(&panel_matrix(&pose), pose.width, 0.5, 0.0);
        let to_head = [0, 1, 2].map(|i| (head[i] - pl.c[i]) / d(head, pl.c));
        assert!(crate::geometry::dot(&pl.z, &to_head) > 0.999, "faces you: {:?}", pl.z);
        assert_eq!(pose.width, 1.4, "the monitor's width until the stream's size comes");
    }
}
