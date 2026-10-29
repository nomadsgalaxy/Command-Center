//! Command Center's tab in SteamVR's dashboard, next to Steam's, so you can reach it from the
//! same place you launch your 2D games. It's a small control panel with two settings, each
//! applied the moment you click it:
//!   Desktop              [Show|Hide]   the panels, the same as the `show` and `hide` socket commands
//!   Mouse and keyboard   [Command Center|Steam / game]   where they go. "Steam / game" gives the
//!                        keyboards and mice back to the Frame (kvm.rs `set_yielded`, the way a VR
//!                        game does), and "Command Center" takes them again.
//! plus a line saying what the choice does. While a 2D Steam game is playing, the same two choices
//! hand the Desktop back and forth with the game (handoff.rs), and picking this tab does what
//! pressing "Command Center" does. What it shows is who has the input right now, game or not.
//!
//! It's made at start (`CreateDashboardOverlay`, key controlcenter.dashboard) and lives as long as
//! cc-panels does, so unlike the other windows it has no card and no open/close. While the Desktop
//! is closed, `cc-panels --tab` (tab.rs) shows the same tab in its `closed` mode, where Show opens
//! the Desktop. Only one of them holds the key: the tab mode lets go as soon as the Desktop takes
//! its lock, and the Desktop's tab tries again every RETRY until it gets it. Its
//! thumbnail is packaging/command-center.png. It's painted with the Preferences window's kit
//! (ui.rs) and gets laser clicks as mouse events on its overlay, in the page's units.
//!
//! A choice is a press on the `Handoff` (handoff.rs), which the main loop reads with the game
//! state to set kvm's `yielded`. That keeps one writer for it, because a second would be undone on
//! the next frame.
//!
//! SteamVR draws its own control bar under the tab, and its close quits Command Center, the way it
//! quits any overlay app from the dashboard.
//!
//! Not here: handing the mouse and keyboard to Steam by itself when the dashboard merely opens.
//! `dashboard()` is where that would go, since main.rs calls it when the dashboard opens or closes.
use super::ui::{self, BTN_H, PAD, Px, ROW, Rect, SCALE, TITLE, text, typed, typed_w};
use crate::geometry::from_top;
use crate::grab::{Paint, TagImg, over, tint};
use crate::handoff::Handoff;
use crate::taskbar::tex_scale;
use crate::{HIDDEN, call, theme, vr};
use openvr_sys as sys;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// Its labels, drawn by assets.rs at start as tag-ui-<key>.rgba. "dash" is both the title and the
/// first choice of where the mouse and keyboard go.
pub const LABELS: [(&str, &str); 6] =
    [("dash", "Command Center"), ("dashdesktop", "Desktop"), ("dashshow", "Show"), ("dashhide", "Hide"), ("dashinput", "Mouse and keyboard"), ("dashsteam", "Steam / game")];

/// How long the closed tab says it's opening the Desktop after Show.
const OPENING: Duration = Duration::from_secs(60);
/// How often making the tab is tried again while its key is taken (tab.rs letting go of it).
const RETRY: Duration = Duration::from_millis(500);

/// SteamVR's dashboard is open (main.rs tells `dashboard`), so the tab is being looked at.
static OPEN: AtomicBool = AtomicBool::new(false);

/// The dashboard opened or closed. It only keeps the tab from redrawing while nobody can see it.
pub fn dashboard(open: bool) {
    OPEN.store(open, Relaxed);
}

const W: f64 = 600.0; // in units (the taskbar's: a Breeze logical pixel)
const ROWS: usize = 3; // Desktop, Mouse and keyboard, and what that choice does
const H: f64 = TITLE + ROWS as f64 * ROW + PAD;
const SEG: f64 = 150.0; // one segment's width
/// Calibration knob: how wide the page is in the dashboard, in metres. It wants a look in the headset.
const WIDTH_M: f32 = 2.0;
const NAMES: [usize; 2] = [0, 1]; // the rows that are settings, in the order of Ui::names

/// A segment under a laser: its row (0 Desktop, 1 Mouse and keyboard) and which of its two.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Hit(usize, usize);

fn row_y(i: usize) -> f64 {
    TITLE + (i as f64 + 0.5) * ROW
}

/// The span of the two segments, right-aligned.
fn span() -> (f64, f64) {
    (W - PAD - 2.0 * SEG, W - PAD)
}

/// What's at (x, y), in units from the top left.
fn hit(x: f64, y: f64) -> Option<Hit> {
    if y < TITLE {
        return None;
    }
    let i = ((y - TITLE) / ROW) as usize;
    if !NAMES.contains(&i) || (y - row_y(i)).abs() > BTN_H / 2.0 {
        return None;
    }
    ui::segment(2, span(), x).map(|s| Hit(i, s))
}

/// The area hit a's look covers: its control and the space around it.
fn rect(Hit(i, _): Hit) -> Rect {
    let (x0, x1) = span();
    (x0 - 8.0, row_y(i) - ROW / 2.0, x1 + 8.0, row_y(i) + ROW / 2.0)
}

/// All of row i.
fn whole_row(i: usize) -> Rect {
    (0.0, row_y(i) - ROW / 2.0, W, row_y(i) + ROW / 2.0)
}

/// The areas to redraw going from a to b. None means redraw all of it.
fn dirty(a: &Key, b: &Key) -> Option<Vec<Rect>> {
    if a.theme != b.theme || a.closed != b.closed || a.opening != b.opening {
        return None;
    }
    let mut r: Vec<Rect> = if a.hover != b.hover { [a.hover, b.hover].into_iter().flatten().map(rect).collect() } else { Vec::new() };
    if a.shown != b.shown {
        r.push(whole_row(0));
    }
    if a.steam != b.steam {
        r.extend([whole_row(1), whole_row(2)]); // the choice, and the line about it
    }
    Some(r)
}

/// What it's drawn from. When this changes, it gets redrawn.
#[derive(Clone, PartialEq, Debug)]
struct Key {
    shown: bool, // the Desktop's panels
    steam: bool,
    hover: Option<Hit>,
    theme: u32,
    closed: bool,  // the Desktop isn't running (tab.rs): Show opens it, and the input row isn't there
    opening: bool, // ...and Show was pressed
}

/// Its labels, plus ascii.rgba for the line about the choice.
#[derive(Default)]
struct Ui {
    title: Option<TagImg>,
    names: [Option<TagImg>; 2],
    segs: [[Option<TagImg>; 2]; 2],
    ascii: Option<TagImg>,
}

struct Scene<'a> {
    k: &'a Key,
    p: Paint,
    ui: &'a Ui,
}

impl Scene<'_> {
    /// Row i: a setting (its name and its two choices) or the line about the choice.
    fn row(&self, i: usize, x: f64, y: f64) -> Px {
        let (t, k, ui, cy) = (&self.p.t, self.k, self.ui, row_y(i));
        let ink = |px: Px, c| tint(px, c, c, c);
        if k.closed && i == 1 {
            return ([0.0; 3], 0.0); // nothing to choose with no Desktop
        }
        if i == 2 {
            let line = match (k.closed, k.opening, k.steam) {
                (true, true, _) => "Opening the Desktop...",
                (true, false, _) => "The Desktop is closed. Show opens it.",
                (false, _, true) => "Your mouse and keyboard go to Steam and your game.",
                (false, _, false) => "Your mouse and keyboard work Command Center.",
            };
            let left = (PAD + 4.0).min(W - PAD - typed_w(ui.ascii.as_ref(), line)); // a long one shows its end
            return if x < W - PAD { ink(typed(ui.ascii.as_ref(), line, x, y, left, cy), t.wdim) } else { ([0.0; 3], 0.0) };
        }
        let chosen = if i == 0 { !k.shown as usize } else { k.steam as usize };
        let hover = match k.hover {
            Some(Hit(r, s)) if r == i => Some(s),
            _ => None,
        };
        let labels: Vec<_> = ui.segs[i].iter().map(Option::as_ref).collect();
        let control = ui::segmented(&self.p, &labels, chosen, hover, span(), cy, x, y);
        let name = if x < span().0 - ui::GAP { ink(text(ui.names[i].as_ref(), x, y, PAD + 16.0, cy), t.wtext) } else { ([0.0; 3], 0.0) };
        over(name, control)
    }

    /// One pixel at (x, y), in units from the top left: a Breeze window holding the rows.
    fn pixel(&self, x: f64, y: f64) -> Px {
        let Some((mut out, d)) = ui::chrome(&self.p, self.ui.title.as_ref(), W, H, x, y) else { return ([0.0; 3], 0.0) };
        let i = ((y - TITLE) / ROW).floor();
        if i >= 0.0 && (i as usize) < ROWS {
            out = over(out, self.row(i as usize, x, y));
        }
        ui::border(&self.p, out, d)
    }
}

pub struct Dashtab {
    ov: Option<(vr::Handle, vr::Handle)>, // the page and its thumbnail
    retry: Option<Instant>,               // when to try making it again, after a try that failed
    failed: bool,                         // the last try failed, so the next failure isn't logged again
    pub closed: bool,                     // tab.rs's mode: the Desktop isn't running
    opening: Option<Instant>,             // Show pressed in that mode
    pub quit: bool,                       // ...and SteamVR's close pressed on it, so tab.rs ends

    ui: Arc<Ui>,
    hover: Option<Hit>,
    painter: ui::Painter<Key>,
}

impl Dashtab {
    pub fn new(assets: &str) -> Dashtab {
        let load = |k: &str| crate::load_tag(&format!("{assets}/tag-ui-{k}.rgba"));
        Dashtab {
            ov: None,
            retry: None,
            failed: false,
            closed: false,
            opening: None,
            quit: false,
            ui: Arc::new(Ui {
                title: load("dash"),
                names: ["dashdesktop", "dashinput"].map(load),
                segs: [["dashshow", "dashhide"].map(load), ["dash", "dashsteam"].map(load)],
                ascii: crate::load_tag(&format!("{assets}/ascii.rgba")),
            }),
            hover: None,
            painter: ui::Painter::default(),
        }
    }

    /// True under a laser, so the loop keeps the display's rate.
    pub fn moving(&self) -> bool {
        self.hover.is_some()
    }

    /// Our tab is the dashboard's active one: the dashboard is open and this is the page showing.
    pub fn selected(&self) -> bool {
        OPEN.load(Relaxed) && self.ov.is_some_and(|(h, _)| call!(ov, IsActiveDashboardOverlay, h))
    }

    /// The tab's page overlay, once it's made (notify.rs hangs SteamVR's toasts off it).
    pub fn page(&self) -> Option<vr::Handle> {
        self.ov.map(|(h, _)| h)
    }

    /// Every frame: makes the tab the first time, then handles its events and draws it.
    pub fn tick(&mut self, hand: &mut Handoff) {
        if self.ov.is_none() && self.retry.is_none_or(|t| Instant::now() >= t) {
            self.retry = Some(Instant::now() + RETRY);
            self.open();
        }
        let Some((h, _)) = self.ov else { return };
        // asked every frame, not only told by the Activated event: the Desktop is usually started
        // from the dashboard, so it's already open when we start, and that event never comes
        OPEN.store(call!(ov, IsDashboardVisible), Relaxed);
        self.events(h, hand);
        // drawn once at the start, and after that only while the dashboard is open
        if self.painter.drawn().is_none() || OPEN.load(Relaxed) {
            self.paint(h, hand.yielded());
        }
    }

    fn open(&mut self) {
        let (h, thumb) = match crate::surface::create_dashboard("controlcenter.dashboard", "Command Center") {
            Ok(h) => h,
            Err(e) => {
                if !std::mem::replace(&mut self.failed, true) {
                    eprintln!("dashboard tab: no overlay yet: {e}; trying again every {} s", RETRY.as_secs()); // the key's in use (tab.rs), or the overlays are all taken
                }
                return;
            }
        };
        self.failed = false;
        let logo = crate::root().join("packaging/command-center.png");
        if logo.exists() {
            vr::set_from_file(thumb, &logo.to_string_lossy());
        } else {
            eprintln!("dashboard tab: no logo at {}, so its icon stays blank", logo.display());
        }
        call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayWidthInMeters, h, WIDTH_M);
        // SteamVR's own bar under the tab, with its close (VREvent_OverlayClosed, in events). It's
        // only drawn for what the dashboard hosts, so the floating windows keep our own bar.
        // settings.json `"steamvr_bar": false` turns it off from the next start.
        let bar = crate::config::settings()["steamvr_bar"].as_bool() != Some(false);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_EnableControlBar, bar);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_EnableControlBarClose, bar);
        self.ov = Some((h, thumb));
        eprintln!("dashboard tab: made");
    }

    /// Laser events: hovering, and presses on a choice.
    fn events(&mut self, h: vr::Handle, hand: &mut Handoff) {
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        let mut again = false;
        while call!(ov, PollNextOverlayEvent, h, &mut e, size_of::<vr::VREvent_t>() as u32) {
            let m = unsafe { e.data.mouse };
            let at = hit(m.x as f64, from_top(H, m.y as f64)); // page units; events count from the bottom left
            match e.eventType {
                sys::EVREventType_VREvent_MouseMove => self.hover = at,
                sys::EVREventType_VREvent_FocusLeave => self.hover = None,
                // SteamVR's close on the tab quits Command Center, the way it quits any overlay app.
                // Only when you could have pressed it: the dashboard showing, on our tab. Any other
                // close (a dashboard restart, say) makes the tab again and leaves the Desktop be.
                sys::EVREventType_VREvent_OverlayClosed if !(call!(ov, IsDashboardVisible) && call!(ov, IsActiveDashboardOverlay, h)) => {
                    eprintln!("dashboard tab: SteamVR closed it while it wasn't showing, so it's made again");
                    again = true;
                }
                sys::EVREventType_VREvent_OverlayClosed if self.closed => self.quit = true, // tab.rs ends
                sys::EVREventType_VREvent_OverlayClosed => crate::close_desktop("the dashboard tab's close"),
                sys::EVREventType_VREvent_MouseButtonDown if m.button == sys::EVRMouseButton_VRMouseButton_Left => match at {
                    Some(Hit(0, 0)) if self.closed => self.open_desktop(),
                    Some(_) if self.closed => {}
                    Some(a) => act(a, hand),
                    None => {}
                },
                _ => {}
            }
        }
        if again {
            self.remake();
        }
    }

    /// Redraws it off the main thread when anything changed, and only the parts that did.
    fn paint(&mut self, h: vr::Handle, steam: bool) {
        let opening = self.opening.is_some_and(|t| t.elapsed() < OPENING);
        let key = Key { shown: !HIDDEN.load(Relaxed) && !self.closed, steam, hover: self.hover, theme: theme::generation(), closed: self.closed, opening };
        self.painter.paint(h, key, || {
            let (ui, p) = (self.ui.clone(), Paint::new(&theme::get(), theme::VIOLET));
            move |was: Option<Key>, px, k: &Key| {
                let sc = Scene { k, p, ui: &ui };
                let s = tex_scale(W, H, SCALE);
                ui::redraw(px, ((W * s) as usize, (H * s) as usize), s, (W, H), was.and_then(|w| dirty(&w, k)), |x, y| sc.pixel(x, y))
            }
        });
    }

    pub fn destroy(&mut self) {
        if let Some((h, thumb)) = self.ov.take() {
            crate::gpu::forget(h);
            crate::surface::destroy(h);
            crate::surface::destroy(thumb);
        }
    }

    /// Lets go of the tab and its key (tab.rs, once the Desktop runs). The next tick makes it again.
    pub fn release(&mut self) {
        if self.ov.is_some() {
            self.remake();
            eprintln!("dashboard tab: handed to the Desktop");
        }
    }

    /// Destroys the tab so the next tick makes it anew.
    fn remake(&mut self) {
        self.destroy();
        (self.hover, self.painter, self.retry, self.opening) = (None, ui::Painter::default(), None, None);
    }

    /// Show in the closed mode: starts the Desktop the way the launcher's tile does (cc-home desktop),
    /// as a systemd unit of its own on the host, so it outlives this process. The unit's fixed name
    /// keeps a second press from starting a second one.
    fn open_desktop(&mut self) {
        if self.opening.is_some_and(|t| t.elapsed() < OPENING) {
            return;
        }
        let cc_home = crate::root().join("cc-home");
        let mut argv: Vec<std::ffi::OsString> = ["systemd-run", "--user", "--collect", "--quiet", "--unit=command-center-open"].map(Into::into).into();
        argv.extend([cc_home.into_os_string(), "desktop".into()]);
        if std::path::Path::new("/run/.containerenv").exists() {
            argv.insert(0, format!("{}/.local/bin/distrobox-host-exec", crate::config::home_dir()).into()); // the container's way to the host (machines.rs on_host)
        }
        let started = std::process::Command::new(&argv[0]).args(&argv[1..]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).spawn();
        match started {
            Ok(_) => {
                self.opening = Some(Instant::now());
                eprintln!("dashboard tab: opening the Desktop");
            }
            Err(e) => eprintln!("dashboard tab: can't open the Desktop: {e}"),
        }
    }
}

/// A click. The Desktop's choice is applied now; the input's goes to the hand-off, which main.rs
/// reads on its next frame.
fn act(Hit(row, seg): Hit, hand: &mut Handoff) {
    if row == 0 {
        let show = seg == 0;
        if show != HIDDEN.load(Relaxed) {
            return; // already how it says
        }
        eprintln!("dashboard tab: Desktop {}", if show { "shown" } else { "hidden" });
        super::show_panels(show);
    } else {
        eprintln!("dashboard tab: mouse and keyboard to {}", if seg == 1 { "Steam and the game" } else { "Command Center" });
        hand.press(seg == 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grab;

    fn key(hover: Option<Hit>) -> Key {
        Key { shown: true, steam: false, hover, theme: 0, closed: false, opening: false }
    }

    #[test]
    fn hits() {
        let (x0, x1) = span();
        assert_eq!(hit(x0 + 1.0, row_y(0)), Some(Hit(0, 0)), "Show");
        assert_eq!(hit(x1 - 1.0, row_y(0)), Some(Hit(0, 1)), "Hide");
        assert_eq!(hit(x0 + SEG - 1.0, row_y(1)), Some(Hit(1, 0)), "Command Center, its right edge");
        assert_eq!(hit(x0 + SEG + 1.0, row_y(1) + BTN_H / 2.0 - 1.0), Some(Hit(1, 1)), "Steam / game, its bottom edge");
        assert_eq!(hit(x0 - 1.0, row_y(0)), None, "left of it: the name");
        assert_eq!(hit(PAD + 20.0, row_y(1)), None, "a name");
        assert_eq!(hit(x1 - 1.0, TITLE / 2.0), None, "the title bar");
        assert_eq!(hit(x1 - 1.0, row_y(2)), None, "the line about the choice");
        assert_eq!(hit(x1 - 1.0, row_y(0) + BTN_H / 2.0 + 1.0), None, "between rows");
        assert_eq!(hit(x1 + 1.0, row_y(0)), None, "right of it");
        assert!(PAD + 16.0 + 200.0 < x0 - ui::GAP, "room for the names");
    }

    #[test]
    fn it_draws_the_chosen_choice_in_the_accent() {
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::VIOLET));
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        let seg = |s: usize| span().0 + SEG * (s as f64 + 0.5);
        let k = key(None);
        let sc = Scene { k: &k, p, ui: &ui };
        assert!(near(sc.pixel(seg(0), row_y(0)).0, p.acc.line), "Show: chosen while the panels are shown");
        assert!(near(sc.pixel(seg(1), row_y(0)).0, p.t.raised), "Hide: not");
        assert!(near(sc.pixel(seg(0), row_y(1)).0, p.acc.line), "Command Center: chosen");
        let k = Key { shown: false, steam: true, ..key(None) };
        let sc = Scene { k: &k, p, ui: &ui };
        assert!(near(sc.pixel(seg(1), row_y(0)).0, p.acc.line), "Hide: chosen while they're hidden");
        assert!(near(sc.pixel(seg(1), row_y(1)).0, p.acc.line) && near(sc.pixel(seg(0), row_y(1)).0, p.t.raised), "Steam / game");
        assert_eq!(sc.pixel(0.2, 0.2).1, 0.0, "round corners: clear");
    }

    #[test]
    fn closed_it_offers_only_show() {
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::VIOLET));
        let seg = |s: usize| span().0 + SEG * (s as f64 + 0.5);
        let k = Key { closed: true, shown: false, ..key(None) };
        let sc = Scene { k: &k, p, ui: &ui };
        assert_eq!(sc.row(1, seg(0), row_y(1)).1, 0.0, "no input row with no Desktop");
        assert!(sc.row(0, seg(0), row_y(0)).1 > 0.0, "Show is there");
        assert!(dirty(&key(None), &k).is_none() && dirty(&k, &Key { opening: true, ..k.clone() }).is_none(), "the mode and its line: all of it");
    }

    #[test]
    fn a_patch_draws_as_all_of_it() {
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::VIOLET));
        let s = 1.5;
        let (tw, th) = ((W * s) as usize, (H * s) as usize);
        let h = Some(Hit(0, 0));
        let mut cases: Vec<(Key, Key)> =
            [(None, Some(Hit(0, 1))), (Some(Hit(0, 1)), Some(Hit(1, 0))), (Some(Hit(1, 0)), Some(Hit(1, 1))), (Some(Hit(1, 1)), None)].into_iter().map(|(a, b)| (key(a), key(b))).collect();
        cases.extend([Key { shown: false, ..key(h) }, Key { steam: true, ..key(h) }, Key { shown: false, steam: true, ..key(None) }].map(|b| (key(h), b)));
        let draw = |k: &Key| {
            let sc = Scene { k, p, ui: &ui };
            grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
        };
        for (a, b) in cases {
            let sc = Scene { k: &b, p, ui: &ui };
            let t = ui::redraw(draw(&a), (tw, th), s, (W, H), dirty(&a, &b), |x, y| sc.pixel(x, y));
            assert!(!t.full && t.px == draw(&b), "{a:?} -> {b:?}");
        }
        assert!(dirty(&key(None), &Key { theme: 1, ..key(None) }).is_none(), "a new theme: all of it");
    }
}
