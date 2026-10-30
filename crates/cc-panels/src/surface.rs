//! The surface registry: every overlay Command Center puts somewhere, listed once, with what it
//! is, what its place is relative to, and where that place is saved. Every overlay is made through
//! `create` (or `create_dashboard`) and let go through `destroy`, so the list is
//! complete, and a test fails if code makes one any other way. `surfaces` on the control socket
//! prints it.
//!
//! This is a record of how things are today, not a new way of doing them. Nothing here moves,
//! saves or draws anything. The one-manipulator spec (Planning/SteamVR Window Manipulation.md)
//! builds on it, which is why the anchors and stores are written down as data.
//!
//! What each kind is placed relative to (`Anchor`), where its place is saved (`Store`) and how it
//! is manipulated (`Manip`) today:
//!
//! | Surface | Anchor | Saved in | Manipulated by |
//! | --- | --- | --- | --- |
//! | Remote monitor | Room | spots.home.`<viewer name>` | Grab (card: bar, edges, corners, curve, push), Right Ctrl |
//! | Pop-out | Room | spots.home.`pop-<machine>-<uuid>` | Grab, Right Ctrl |
//! | Window panel | Room | spots.home.`app:<id>` (with its zoom), and win-poses.json by window uuid | Grab (and zoom, stretch), Right Ctrl |
//! | Window popup | Parent: its window | nothing | nothing (it rides its window's panel) |
//! | Card (frame, bar, tab) | Parent: its panel or Extra | nothing | it is the handle for the Grab |
//! | Machines / Preferences / Workspace | Room | spots.home.`machines` / `prefs` / `workspace` | Grab, in the Extra slot |
//! | Taskbar frame | Room (fixed), Head (follow), Device (wrist) | fixed carry: spots.home.`taskbar`; mode, size and curve: settings.json `taskbar`, `taskbar_scale`, `taskbar_curve` | its own Hold (taskbar.rs), fixed mode only for carrying |
//! | Plasma's bar and popups | Parent: the taskbar frame | nothing | nothing (input is forwarded) |
//! | A panel's back | Parent: its panel | nothing | a press carries its panel |
//! | Key strip | Parent: its panel (under its card, while SteamVR's keyboard types to it) | nothing | nothing (it rides its panel) |
//! | Scan HUD strip | Head | nothing | nothing (its skip button is clicked) |
//! | Scan HUD tag outlines | Room (from `hud mark`) | nothing | nothing |
//! | Theater backdrop | Head | nothing | nothing |
//! | Cursor | Room (faces the head each frame) | nothing | nothing |
//! | Laser-mode overlay | Room (far below the head) | nothing | nothing |
//! | Dashboard tab | Dashboard (SteamVR places it) | nothing | nothing |
//!
//! A panel slot is made once and filled with different things over a run, so what one is (a
//! monitor, a window, a pop-out, a popup, or free) is read when it's described, from `PanelNow`.
//! Theater mode isn't a surface of its own: it moves a window panel to a new place that isn't
//! saved (windows.rs `theater`).
//!
//! This file is also built by the cc-win-spike binary (with back.rs), so it uses only vr.rs.
use crate::overlay::Surfaces;
use crate::vr;
use std::fmt;
use std::sync::Mutex;

pub type Handle = vr::Handle;

/// What a registered overlay is, as the code that makes it knows it. A panel slot's picture is
/// `Panel`, and what it shows right now is its `Role`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Panel,
    Card,
    Extra,
    Taskbar,
    Plasma,
    Back,
    HudStrip,
    HudMark,
    Backdrop,
    Cursor,
    LaserMode,
    Dashtab,
    Keys, // the key strip under a panel while SteamVR's keyboard types to it (osk.rs)
}

/// What a surface's place is relative to. Follow mode eases after the head, so it's `Head` even
/// though it's written as a room pose each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    /// Standing space.
    Room,
    /// The headset.
    Head,
    /// A controller (the taskbar's wrist mode).
    Device,
    /// Another surface: its panel or window, or the taskbar's frame.
    Parent(Parent),
    /// SteamVR's dashboard, which places it.
    Dashboard,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Parent {
    Panel,
    Window,
    Extra,
    Taskbar,
}

/// Where a surface's place is saved.
#[derive(Clone, Debug, PartialEq)]
pub enum Store {
    /// The active workspace's "home" spot in home.json, under this name.
    Spot(String),
    /// An entry in a file in ~/.cache/control-center: (file, entry).
    Cache(&'static str, String),
    /// A key in ~/.config/control-center/settings.json.
    Setting(&'static str),
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Store::Spot(k) => write!(f, "spot:{k}"),
            Store::Cache(file, k) => write!(f, "cache:{file}[{k}]"),
            Store::Setting(k) => write!(f, "setting:{k}"),
        }
    }
}

/// How a person moves, resizes and bends it today.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Manip {
    /// Grab's card (grab.rs): the bar, edges, corners and curve button, and Right Ctrl.
    Grab,
    /// Its own (the taskbar's Hold, taskbar.rs).
    Own,
    /// Pressing it works its panel's (a back).
    ViaParent,
    /// Not at all.
    None,
}

/// What a panel slot is showing right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Monitor,
    Popout,
    Window,
    Popup,
    /// Nothing: a spare slot not filled yet, a removed remote, or a window slot with no window.
    Free,
}

impl Role {
    /// From a slot's facts: a window (and then whether it's a menu popup), else a pop-out of a
    /// host window, else a remote monitor.
    pub fn of(now: &PanelNow) -> Role {
        match (now.used, now.window, now.popup, now.popout) {
            (false, ..) => Role::Free,
            (true, true, true, _) => Role::Popup,
            (true, true, false, _) => Role::Window,
            (true, false, _, true) => Role::Popout,
            (true, false, _, false) => Role::Monitor,
        }
    }
}

/// What a panel slot is right now, as main.rs knows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelNow {
    /// Filled and not removed.
    pub used: bool,
    /// It shows a Frame window (windows.rs), not a remote.
    pub window: bool,
    /// ...and that window is a menu or other popup of another.
    pub popup: bool,
    /// It shows a remote's window popped out (popout.rs).
    pub popout: bool,
    /// Its key in the spots: a viewer's name, or a window's `app:<id>`.
    pub spot: String,
    /// A window's uuid, for win-poses.json.
    pub uuid: Option<String>,
}

/// One registered overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub overlay: Handle,
    /// SteamVR's overlay key (`controlcenter.panel.desk`).
    pub key: String,
    pub kind: Kind,
    /// The panel slot it belongs to: a panel's picture, or its card.
    pub slot: Option<usize>,
    /// Its name: a panel's at the start, or an Extra's spot (`prefs`).
    pub name: String,
    pub anchor: Anchor,
}

/// An entry with everything resolved: what it is now, what it follows, where it saves, how it moves.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    pub entry: Entry,
    /// For a panel slot, what it shows now.
    pub role: Option<Role>,
    pub anchor: Anchor,
    pub store: Vec<Store>,
    pub manip: Manip,
}

impl fmt::Display for Surface {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let what = match self.role {
            Some(r) => format!("{:?}/{r:?}", self.entry.kind),
            None => format!("{:?}", self.entry.kind),
        };
        let store = if self.store.is_empty() { "-".to_string() } else { self.store.iter().map(Store::to_string).collect::<Vec<_>>().join(",") };
        write!(f, "{} {} anchor={:?} store={store} manip={:?}", self.entry.key, what, self.anchor, self.manip)
    }
}

/// What a kind is placed relative to, unless it changes (the taskbar's mode: `set_anchor`).
fn anchor_of(kind: Kind, slot: Option<usize>) -> Anchor {
    match kind {
        Kind::Panel | Kind::Extra | Kind::HudMark | Kind::Cursor | Kind::LaserMode | Kind::Taskbar => Anchor::Room,
        Kind::Card if slot.is_some() => Anchor::Parent(Parent::Panel),
        Kind::Card => Anchor::Parent(Parent::Extra),
        Kind::Plasma => Anchor::Parent(Parent::Taskbar),
        Kind::Back | Kind::Keys => Anchor::Parent(Parent::Panel),
        Kind::HudStrip | Kind::Backdrop => Anchor::Head,
        Kind::Dashtab => Anchor::Dashboard,
    }
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Makes an overlay and lists it. `slot` is the panel slot it belongs to, if any, and `name` is
/// the panel's or the Extra's. Every overlay in cc-panels is made through here.
pub fn create(kind: Kind, slot: Option<usize>, key: &str, name: &str) -> Result<Handle, String> {
    let h = crate::overlay::Live.create(key, name)?;
    register(h, kind, slot, key, name);
    Ok(h)
}

/// Makes the dashboard tab: its page, listed here, and its icon, which counts against SteamVR's 128
/// overlays but isn't placed by us, so it isn't listed. (`CreateDashboardOverlay` makes both at once.)
pub fn create_dashboard(key: &str, name: &str) -> Result<(Handle, Handle), String> {
    // OpenVR always has a dashboard (an OpenXR backend says None)
    let (main, thumb) = crate::overlay::Live.create_dashboard(key, name).unwrap_or_else(|| Err("no dashboard".into()))?;
    register(main, Kind::Dashtab, None, key, name);
    Ok((main, thumb))
}

fn register(h: Handle, kind: Kind, slot: Option<usize>, key: &str, name: &str) {
    let mut r = REGISTRY.lock().unwrap();
    r.retain(|e| e.overlay != h); // a handle SteamVR gave out again
    r.push(Entry { overlay: h, key: key.into(), kind, slot, name: name.into(), anchor: anchor_of(kind, slot) });
}

/// Takes the overlay off the list and destroys it. Handle 0 (an overlay that couldn't be made) is
/// left alone.
pub fn destroy(h: Handle) {
    forget(h);
    crate::overlay::Live.destroy(h);
}

/// Takes the overlay off the list.
pub fn forget(h: Handle) {
    REGISTRY.lock().unwrap().retain(|e| e.overlay != h);
}

/// What the surface's place is relative to has changed (the taskbar's mode).
pub fn set_anchor(h: Handle, anchor: Anchor) {
    if let Some(e) = REGISTRY.lock().unwrap().iter_mut().find(|e| e.overlay == h) {
        e.anchor = anchor;
    }
}

/// Everything listed, oldest first.
pub fn entries() -> Vec<Entry> {
    REGISTRY.lock().unwrap().clone()
}

/// Resolves an entry. `now` is a panel slot's facts, for entries with a slot (a panel's picture).
pub fn describe(e: &Entry, now: Option<&PanelNow>) -> Surface {
    let mut role = None;
    let mut anchor = e.anchor;
    let (store, manip) = match e.kind {
        Kind::Panel => {
            let r = now.map_or(Role::Free, Role::of);
            role = Some(r);
            match r {
                Role::Monitor | Role::Popout => (vec![Store::Spot(now.map(|n| n.spot.clone()).unwrap_or_default())], Manip::Grab),
                Role::Window => {
                    let n = now.cloned().unwrap_or_default();
                    let mut s = vec![Store::Spot(n.spot)];
                    s.extend(n.uuid.map(|u| Store::Cache("win-poses.json", u)));
                    (s, Manip::Grab)
                }
                Role::Popup => {
                    anchor = Anchor::Parent(Parent::Window);
                    (vec![], Manip::None)
                }
                Role::Free => (vec![], Manip::None),
            }
        }
        Kind::Extra => (vec![Store::Spot(e.name.clone())], Manip::Grab),
        Kind::Taskbar => (vec![Store::Spot("taskbar".into()), Store::Setting("taskbar"), Store::Setting("taskbar_scale"), Store::Setting("taskbar_curve")], Manip::Own),
        Kind::Back | Kind::Keys => (vec![], Manip::ViaParent),
        Kind::Card | Kind::Plasma | Kind::HudStrip | Kind::HudMark | Kind::Backdrop | Kind::Cursor | Kind::LaserMode | Kind::Dashtab => (vec![], Manip::None),
    };
    Surface { entry: e.clone(), role, anchor, store, manip }
}

/// The `surfaces` command's answer: one line per registered overlay. `now` gives a panel slot's
/// facts.
pub fn report(now: impl Fn(usize) -> Option<PanelNow>) -> String {
    let mut out = String::from("ok");
    for e in entries() {
        let facts = e.slot.filter(|_| e.kind == Kind::Panel).and_then(&now);
        out += &format!("\n{}", describe(&e, facts.as_ref()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, slot: Option<usize>, name: &str) -> Entry {
        Entry { overlay: crate::vr::Handle(1), key: format!("controlcenter.{name}"), kind, slot, name: name.into(), anchor: anchor_of(kind, slot) }
    }

    fn now(used: bool, window: bool, popup: bool, popout: bool, spot: &str, uuid: Option<&str>) -> PanelNow {
        PanelNow { used, window, popup, popout, spot: spot.into(), uuid: uuid.map(String::from) }
    }

    #[test]
    fn a_panel_slot_is_what_it_shows_right_now() {
        let role = |n: &PanelNow| Role::of(n);
        assert_eq!(role(&now(true, false, false, false, "desk", None)), Role::Monitor);
        assert_eq!(role(&now(true, false, false, true, "pop-desk-x", None)), Role::Popout);
        assert_eq!(role(&now(true, true, false, false, "app:org.kde.konsole", None)), Role::Window);
        assert_eq!(role(&now(true, true, true, false, "win-3", None)), Role::Popup);
        assert_eq!(role(&now(false, true, false, false, "win-3", None)), Role::Free, "a window slot with no window");
        assert_eq!(role(&now(false, false, false, false, "", None)), Role::Free, "a spare");
    }

    #[test]
    fn a_monitor_saves_under_its_name_and_a_window_under_its_app_and_its_uuid() {
        let e = entry(Kind::Panel, Some(0), "desk");
        let m = describe(&e, Some(&now(true, false, false, false, "desk", None)));
        assert_eq!((m.role, m.anchor, m.manip), (Some(Role::Monitor), Anchor::Room, Manip::Grab));
        assert_eq!(m.store, vec![Store::Spot("desk".into())]);
        let w = describe(&e, Some(&now(true, true, false, false, "app:org.kde.konsole", Some("{1a2b}"))));
        assert_eq!(w.store, vec![Store::Spot("app:org.kde.konsole".into()), Store::Cache("win-poses.json", "{1a2b}".into())], "windows are remembered twice");
        let p = describe(&e, Some(&now(true, false, false, true, "pop-desk-x", None)));
        assert_eq!((p.role, p.store), (Some(Role::Popout), vec![Store::Spot("pop-desk-x".into())]));
    }

    #[test]
    fn a_popup_rides_its_window_and_saves_nothing() {
        let s = describe(&entry(Kind::Panel, Some(5), "win-5"), Some(&now(true, true, true, false, "win-5", None)));
        assert_eq!((s.role, s.anchor, s.manip), (Some(Role::Popup), Anchor::Parent(Parent::Window), Manip::None));
        assert!(s.store.is_empty());
        let free = describe(&entry(Kind::Panel, Some(6), "win-6"), None);
        assert_eq!((free.role, free.manip), (Some(Role::Free), Manip::None));
    }

    #[test]
    fn the_extra_windows_save_under_their_own_names() {
        for n in ["machines", "prefs", "workspace"] {
            let s = describe(&entry(Kind::Extra, None, n), None);
            assert_eq!((s.anchor, s.manip, s.store), (Anchor::Room, Manip::Grab, vec![Store::Spot(n.into())]));
        }
    }

    #[test]
    fn the_taskbar_registers_like_the_rest_but_keeps_its_own_manipulation_and_stores() {
        let mut e = entry(Kind::Taskbar, None, "taskbar");
        let s = describe(&e, None);
        assert_eq!((s.anchor, s.manip), (Anchor::Room, Manip::Own));
        assert_eq!(s.store, vec![Store::Spot("taskbar".into()), Store::Setting("taskbar"), Store::Setting("taskbar_scale"), Store::Setting("taskbar_curve")]);
        e.anchor = Anchor::Device; // wrist mode
        assert_eq!(describe(&e, None).anchor, Anchor::Device);
    }

    #[test]
    fn everything_derived_from_a_panel_or_the_frame_is_anchored_to_it_and_saves_nothing() {
        for (kind, slot, want, manip) in [
            (Kind::Card, Some(0), Anchor::Parent(Parent::Panel), Manip::None),
            (Kind::Card, None, Anchor::Parent(Parent::Extra), Manip::None),
            (Kind::Plasma, None, Anchor::Parent(Parent::Taskbar), Manip::None),
            (Kind::Back, Some(1), Anchor::Parent(Parent::Panel), Manip::ViaParent),
            (Kind::Keys, Some(1), Anchor::Parent(Parent::Panel), Manip::ViaParent),
            (Kind::HudStrip, None, Anchor::Head, Manip::None),
            (Kind::HudMark, None, Anchor::Room, Manip::None),
            (Kind::Backdrop, None, Anchor::Head, Manip::None),
            (Kind::Cursor, None, Anchor::Room, Manip::None),
            (Kind::LaserMode, None, Anchor::Room, Manip::None),
            (Kind::Dashtab, None, Anchor::Dashboard, Manip::None),
        ] {
            let s = describe(&entry(kind, slot, "x"), None);
            assert_eq!((s.anchor, s.manip), (want, manip), "{kind:?}");
            assert!(s.store.is_empty(), "{kind:?} saves nothing");
        }
    }

    #[test]
    fn a_surface_reads_as_one_line() {
        let s = describe(&entry(Kind::Panel, Some(0), "desk"), Some(&now(true, true, false, false, "app:konsole", Some("{u}"))));
        assert_eq!(s.to_string(), "controlcenter.desk Panel/Window anchor=Room store=spot:app:konsole,cache:win-poses.json[{u}] manip=Grab");
        let t = describe(&entry(Kind::Plasma, None, "plasma"), None);
        assert_eq!(t.to_string(), "controlcenter.plasma Plasma anchor=Parent(Taskbar) store=- manip=None");
    }

    #[test]
    fn registering_lists_once_and_forgetting_takes_it_off() {
        // handles far from anything real, since the registry is shared with the other tests
        let (a, b) = (0xFFFF_0001, 0xFFFF_0002);
        register(crate::vr::Handle(a), Kind::Taskbar, None, "controlcenter.test.a", "a");
        register(crate::vr::Handle(b), Kind::Cursor, None, "controlcenter.test.b", "b");
        register(crate::vr::Handle(a), Kind::Taskbar, None, "controlcenter.test.a", "a"); // a handle given out again replaces its entry
        assert_eq!(entries().iter().filter(|e| e.overlay == crate::vr::Handle(a)).count(), 1);
        set_anchor(crate::vr::Handle(a), Anchor::Head);
        assert_eq!(entries().iter().find(|e| e.overlay == crate::vr::Handle(a)).unwrap().anchor, Anchor::Head);
        let text = report(|_| None);
        assert!(text.contains("controlcenter.test.a Taskbar anchor=Head") && text.contains("controlcenter.test.b Cursor anchor=Room"), "{text}");
        forget(crate::vr::Handle(a));
        forget(crate::vr::Handle(b));
        assert!(!entries().iter().any(|e| e.overlay == crate::vr::Handle(a) || e.overlay == crate::vr::Handle(b)));
    }

    /// The registry only lists what goes through it, so no other code may make an overlay itself.
    /// This reads the sources: a new `vr::create_overlay` or `Live.create` call anywhere but here
    /// (and overlay.rs's Live, which makes them for it) is a bug.
    #[test]
    fn every_overlay_is_made_through_the_registry() {
        fn visit(dir: &std::path::Path, bad: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    // src/bin is the window spike, a separate program with an overlay of its own
                    if p.file_name().is_some_and(|n| n != "bin") {
                        visit(&p, bad);
                    }
                // (boundary.rs only lists these names, for the OpenVR ratchet)
                } else if p.extension().is_some_and(|x| x == "rs") && !matches!(p.file_name().and_then(|n| n.to_str()), Some("vr.rs" | "overlay.rs" | "surface.rs" | "boundary.rs")) {
                    let s = std::fs::read_to_string(&p).unwrap();
                    for (n, l) in s.lines().enumerate() {
                        let code = l.split("//").next().unwrap_or("");
                        // vr's made from two pieces, so this search isn't itself a use for boundary.rs's scan
                        let vr_create = ["vr", "::create_overlay"].concat();
                        if [vr_create.as_str(), "create_dashboard_overlay", "CreateOverlay,", "CreateDashboardOverlay,", "Live.create(", "Live.create_dashboard("].iter().any(|p| code.contains(p)) {
                            bad.push(format!("{}:{}: {}", p.display(), n + 1, l.trim()));
                        }
                    }
                }
            }
        }
        let mut bad = Vec::new();
        visit(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut bad);
        assert!(bad.is_empty(), "overlays made outside surface::create:\n{}", bad.join("\n"));
    }

    /// ...and none destroyed without coming off the list.
    #[test]
    fn every_overlay_is_destroyed_through_the_registry() {
        fn visit(dir: &std::path::Path, bad: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    // src/bin is the window spike, a separate program with an overlay of its own
                    if p.file_name().is_some_and(|n| n != "bin") {
                        visit(&p, bad);
                    }
                } else if p.extension().is_some_and(|x| x == "rs") && !matches!(p.file_name().and_then(|n| n.to_str()), Some("overlay.rs" | "surface.rs" | "boundary.rs")) {
                    let s = std::fs::read_to_string(&p).unwrap();
                    for (n, l) in s.lines().enumerate() {
                        let code = l.split("//").next().unwrap_or("");
                        if code.contains("DestroyOverlay") || code.contains("Live.destroy(") {
                            bad.push(format!("{}:{}: {}", p.display(), n + 1, l.trim()));
                        }
                    }
                }
            }
        }
        let mut bad = Vec::new();
        visit(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut bad);
        assert!(bad.is_empty(), "overlays destroyed outside surface::destroy:\n{}", bad.join("\n"));
    }
}
