//! The Workspace window (docs/workspaces.md), the one the taskbar's Workspace chip opens. It
//! has a row per saved workspace (home.json, cc_proto::conf), Temporary first. Each row has a
//! dot (solid for the active one), the name, where it belongs (this room, another room, or any
//! room for Temporary, plus how many panels its layout places), and these buttons:
//!   [Use]      makes it the active one now: only its machines connected, every panel to its
//!              spots (cc-home workspace use, live). A workspace bound to another room shows
//!              [Load] instead, for travelling: it puts that layout in front of you, in
//!              Temporary (conf::load_workspace). The saved one never changes, and its own room
//!              brings it back the way it was.
//!   [Machines] opens its Machines page (control/machines.rs), scoped to it, to pick which
//!              machines are in it
//!   [Rename]   turns the row into a field (SteamVR's keyboard or a physical one); [Save] or
//!              Enter saves
//!   [x]        deletes it on a second click. It never deletes the active one; switch first.
//! Under the rows is [Save as workspace]: you type a name, and it makes a new dedicated workspace
//! from the current layout and machines, bound to this room, and makes it active
//! (conf::save_workspace_as).
//! A status line says how it went. The chip and `workspace show|hide` open and close it. It
//! shares grab.rs's Extra slot with the Machines window and Preferences, taking turns
//! (control::show): it's made when it opens, destroyed when it closes, and placed the same way
//! they are (ui::spot, spots.home.workspace).
//! Its labels are assets' tags (LABELS); typed text uses ascii.rgba.
//! ponytail: the rows are read when it opens and after its own changes (a cc-home change shows
//! on the next opening); 8 workspaces listed at most.
use super::ui::{self, BTN_H, Edit, GAP, MPP, PAD, Px, ROW, Rect, SCALE, SORT, TITLE, typed, typed_w};
use crate::geometry::{Placement, panel_matrix};
use crate::grab::{self, Look, Paint, TagImg, line, over, tint};
use crate::kvm::KVM;
use crate::taskbar::{Taskbar, tex_scale};
use crate::windows::Windows;
use crate::{HIDDEN, Source, call, config, panels, theme, vr};
use cc_proto::conf::{self, Json, TEMPORARY};
use openvr_sys as sys;
use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Whether it's wanted open. The chip, `workspace show|hide`, the Machines page's Back and its card's close set it.
pub static OPEN: AtomicBool = AtomicBool::new(false);

/// Its labels, drawn at startup as tag-ui-<key>.rgba (so no commas). The last two belong to the
/// Machines page, and "workspace" is the taskbar chip's.
pub const LABELS: [(&str, &str); 10] = [
    ("workspace", "Workspace"),
    ("use", "Use"),
    ("load", "Load for travel"),
    ("active", "Active"),
    ("wsmachines", "Machines"),
    ("rename", "Rename"),
    ("saveas", "Save as workspace"),
    ("wssave", "Save"),
    ("inworkspace", "In workspace"),
    ("back", "Back"),
];

const W: f64 = 880.0; // units, same as the Machines window
const NAME: f64 = PAD + 24.0; // the name's column (the dot sits before it)
const DETAIL: f64 = 230.0; // where it belongs
const BUTTONS: [f64; 4] = [110.0, 110.0, 100.0, 28.0]; // Use/Load, Machines, Rename, x
const EDIT: [f64; 2] = [104.0, 28.0]; // a field's Save, x (cancel)
const NEW: f64 = 190.0; // [Save as workspace]
const MAX: usize = 8; // rows listed
const OSK: u64 = 7; // the user value we hand SteamVR's keyboard
const UI_KEYS: [&str; 4] = ["taskbar", "machines", "prefs", "workspace"]; // windows, not panels

/// What's under a laser.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Use(usize), // a row's Use, or Load for travel
    Machines(usize),
    Rename(usize),
    Delete(usize),
    SaveAs, // [Save as workspace]
    Field,  // the field being typed in (a renamed row's, or the foot's)
    Save,
    Unedit, // its x
}

/// A row as drawn.
#[derive(Clone, PartialEq, Debug)]
struct Row {
    name: String,   // its key in home.json
    title: String,  // what you see (Temporary)
    detail: String, // where it belongs, how many panels
    active: bool,
    load: bool, // bound to another room, so Load for travel instead of Use
}

/// A name being typed, for a row's rename or (None) for Save as.
#[derive(Clone, PartialEq, Debug)]
struct Typing {
    row: Option<usize>,
    text: String,
    editing: bool, // the keyboards'
}

/// What it's drawn from. When this changes, it gets drawn again.
#[derive(Clone, PartialEq, Debug)]
struct Key {
    rows: Vec<Row>,
    hover: Option<Hit>,
    armed: Option<usize>, // the row whose x was clicked once
    typing: Option<Typing>,
    status: String,
    theme: u32,
}

fn row_y(i: usize) -> f64 {
    TITLE + (i as f64 + 0.5) * ROW
}

fn height(n: usize) -> f64 {
    TITLE + (n + 1) as f64 * ROW + PAD
}

fn right(b: &[f64], k: usize) -> (f64, f64) {
    ui::right(W - PAD, b, k)
}

/// The field being typed in on row i (row n is the foot).
fn field_x(foot: bool) -> (f64, f64) {
    (if foot { PAD } else { NAME - 4.0 }, right(&EDIT, 0).0 - GAP)
}

/// What's at (x, y), in units from the top left, with n rows and `typing` on row t (n is the foot).
fn hit(n: usize, typing: Option<usize>, x: f64, y: f64) -> Option<Hit> {
    if y < TITLE {
        return None;
    }
    let i = ((y - TITLE) / ROW) as usize;
    if i > n || (y - row_y(i)).abs() > BTN_H / 2.0 {
        return None;
    }
    let on = |(x0, x1): (f64, f64)| (x0..=x1).contains(&x);
    if typing == Some(i) {
        return [(field_x(i == n), Hit::Field), (right(&EDIT, 0), Hit::Save), (right(&EDIT, 1), Hit::Unedit)].into_iter().find(|(s, _)| on(*s)).map(|(_, h)| h);
    }
    if i == n {
        return on((PAD, PAD + NEW)).then_some(Hit::SaveAs);
    }
    [Hit::Use(i), Hit::Machines(i), Hit::Rename(i), Hit::Delete(i)].into_iter().enumerate().find(|(k, _)| on(right(&BUTTONS, *k))).map(|(_, h)| h)
}

/// How far hit a reaches: its button plus some room around it.
fn rect(n: usize, a: Hit) -> Rect {
    let ((x0, x1), i) = match a {
        Hit::Use(i) => (right(&BUTTONS, 0), i),
        Hit::Machines(i) => (right(&BUTTONS, 1), i),
        Hit::Rename(i) => (right(&BUTTONS, 2), i),
        Hit::Delete(i) => (right(&BUTTONS, 3), i),
        Hit::SaveAs => ((PAD, PAD + NEW), n),
        // the field's whole row, whichever row is being typed in
        Hit::Field | Hit::Save | Hit::Unedit => ((0.0, W), n.min(MAX)),
    };
    let cy = row_y(i);
    (x0 - 8.0, cy - ROW / 2.0, x1 + 8.0, cy + ROW / 2.0)
}

fn whole_row(i: usize) -> Rect {
    (0.0, row_y(i) - ROW / 2.0, W, row_y(i) + ROW / 2.0)
}

/// What a laser at (x, y) is on: a button, or else the one it was on while it's still near it.
fn held(was: Option<Hit>, n: usize, typing: Option<usize>, x: f64, y: f64) -> Option<Hit> {
    let still = |a: Hit| match a {
        Hit::Use(i) | Hit::Machines(i) | Hit::Rename(i) | Hit::Delete(i) => i < n && typing != Some(i),
        Hit::SaveAs => typing != Some(n),
        _ => false, // the field's only count where hit() finds them
    };
    hit(n, typing, x, y).or(was.filter(|&a| still(a) && {
        let (x0, y0, x1, y1) = rect(n, a);
        (x0..=x1).contains(&x) && (y0..=y1).contains(&y)
    }))
}

/// The row being typed on (n is the foot).
fn typing_row(t: Option<&Typing>, n: usize) -> Option<usize> {
    t.map(|t| t.row.unwrap_or(n))
}

/// The part to redraw going from a to b; None means all of it.
fn dirty(a: &Key, b: &Key) -> Option<Vec<Rect>> {
    let n = b.rows.len();
    if a.theme != b.theme || a.rows.len() != n {
        return None;
    }
    let mut r: Vec<Rect> = Vec::new();
    if a.hover != b.hover {
        let ta = typing_row(a.typing.as_ref(), n);
        let tb = typing_row(b.typing.as_ref(), n);
        for (h, t) in [(a.hover, ta), (b.hover, tb)] {
            match h {
                Some(Hit::Field | Hit::Save | Hit::Unedit) => r.extend(t.map(whole_row)),
                Some(h) => r.push(rect(n, h)),
                None => {}
            }
        }
    }
    r.extend((0..n).filter(|&i| a.rows[i] != b.rows[i]).map(whole_row));
    if a.armed != b.armed {
        r.extend([a.armed, b.armed].into_iter().flatten().map(whole_row));
    }
    if a.typing != b.typing {
        r.extend([typing_row(a.typing.as_ref(), n), typing_row(b.typing.as_ref(), n)].into_iter().flatten().map(whole_row));
    }
    if a.status != b.status {
        r.push(whole_row(n));
    }
    Some(r)
}

/// The rows for home.json's workspaces, Temporary first. `universe` is the current room (0: none).
fn rows_of(data: &Json, universe: u64) -> Vec<Row> {
    let active = conf::active_workspace(data);
    let mut ws: Vec<&(String, Json)> = data.at("workspaces").items().iter().filter(|(_, w)| matches!(w, Json::Obj(_))).collect();
    ws.sort_by_key(|(n, _)| n != TEMPORARY); // stable, so file order after it
    ws.into_iter()
        .take(MAX)
        .map(|(name, w)| {
            let room = w.at("universe").str().unwrap_or("");
            let here = universe != 0 && room == universe.to_string();
            let mut d = vec![match () {
                _ if name == TEMPORARY => "for travelling".to_string(),
                _ if here => "this room".into(),
                _ if room.is_empty() => "no room yet".into(),
                _ => format!("room ..{}", &room[room.len().saturating_sub(4)..]),
            }];
            let n = w.at("spots").at("home").items().iter().filter(|(k, _)| !UI_KEYS.contains(&k.as_str())).count();
            d.push(format!("{n} panel{}", if n == 1 { "" } else { "s" }));
            if let Some(from) = w.at("loaded").str() {
                d.push(format!("from {from}"));
            }
            if let Some(m) = conf::workspace_machines(data, name) {
                d.push(format!("{} machine{}", m.len(), if m.len() == 1 { "" } else { "s" }));
            }
            Row {
                name: name.clone(),
                title: conf::workspace_title(name).into(),
                detail: d.join(", "),
                active: *name == active,
                load: universe != 0 && !room.is_empty() && !here && name != TEMPORARY,
            }
        })
        .collect()
}

/// Loads its labels and ascii.rgba.
#[derive(Default)]
struct Ui {
    title: Option<TagImg>,
    use_: Option<TagImg>,
    load: Option<TagImg>,
    active: Option<TagImg>,
    machines: Option<TagImg>,
    rename: Option<TagImg>,
    save_as: Option<TagImg>,
    save: Option<TagImg>,
    ascii: Option<TagImg>,
}

struct Scene<'a> {
    k: &'a Key,
    p: Paint,
    ui: &'a Ui,
}

impl Scene<'_> {
    fn labelled(&self, span: (f64, f64), cy: f64, label: Option<&TagImg>, hit: Hit, x: f64, y: f64) -> Px {
        ui::labelled(&self.p, self.k.hover == Some(hit), span, cy, label, x, y)
    }

    /// An x button over span, in the accent color when armed.
    fn x_button(&self, span: (f64, f64), hit: Hit, armed: bool, off: bool, x: f64, y: f64, cy: f64) -> Px {
        let (t, g) = (&self.p.t, self.ui.ascii.as_ref());
        if x <= span.0 - 7.0 || x >= span.1 + 7.0 {
            return ([0.0; 3], 0.0);
        }
        let look = if armed { Look::Carry } else if self.k.hover == Some(hit) && !off { Look::Lit } else { Look::Rest };
        let ink = if off { t.wdim } else if armed { self.p.acc.ink } else { t.wtext };
        let out = over(ui::button(&self.p, look, span, x, y, cy), tint(typed(g, "x", x, y, (span.0 + span.1 - typed_w(g, "x")) / 2.0, cy), ink, ink, ink));
        if off { (out.0, out.1 * 0.6) } else { out }
    }

    /// The field being typed in, with [Save] and x, on the row at cy.
    fn typing(&self, t: &Typing, foot: bool, x: f64, y: f64, cy: f64) -> Px {
        let (th, g) = (&self.p.t, self.ui.ascii.as_ref());
        let mut out = ([0.0; 3], 0.0);
        let (x0, x1) = field_x(foot);
        if x > x0 - 7.0 && x < x1 + 7.0 {
            let lit = t.editing || self.k.hover == Some(Hit::Field);
            out = over(out, ui::button(&self.p, if lit { Look::Lit } else { Look::Rest }, (x0, x1), x, y, cy));
            let (s, ink) = if t.text.is_empty() { ("Name the workspace", th.wdim) } else { (t.text.as_str(), th.wtext) };
            let left = (x0 + 8.0).min(x1 - 8.0 - typed_w(g, s)); // a long one shows its end
            if x > x0 + 4.0 && x < x1 - 4.0 {
                out = over(out, tint(typed(g, s, x, y, left, cy), ink, ink, ink));
            }
        }
        out = over(out, self.labelled(right(&EDIT, 0), cy, self.ui.save.as_ref(), Hit::Save, x, y));
        over(out, self.x_button(right(&EDIT, 1), Hit::Unedit, false, false, x, y, cy))
    }

    /// Row i: its dot, name, where it belongs and its buttons (or the field when it's being renamed).
    fn row(&self, i: usize, x: f64, y: f64) -> Px {
        let (t, acc, g, r, cy) = (&self.p.t, &self.p.acc, self.ui.ascii.as_ref(), &self.k.rows[i], row_y(i));
        let dd = (x - PAD - 10.0).hypot(y - cy) - 5.0;
        let mut out = if r.active { (acc.line, (0.5 - dd).clamp(0.0, 1.0)) } else { (t.wdim, line(dd + 0.75, 0.75)) };
        if let Some(tp) = self.k.typing.as_ref().filter(|tp| tp.row == Some(i)) {
            return over(out, self.typing(tp, false, x, y, cy));
        }
        if x < DETAIL - GAP {
            let fade = if NAME + typed_w(g, &r.title) > DETAIL - GAP { ((DETAIL - GAP - x) / 24.0).min(1.0) } else { 1.0 };
            let (c, a) = tint(typed(g, &r.title, x, y, NAME, cy), t.wtext, t.wtext, t.wtext);
            out = over(out, (c, a * fade));
        } else if x < right(&BUTTONS, 0).0 - GAP {
            let end = right(&BUTTONS, 0).0 - GAP;
            let fade = if DETAIL + typed_w(g, &r.detail) > end { ((end - x) / 24.0).min(1.0) } else { 1.0 };
            let (c, a) = tint(typed(g, &r.detail, x, y, DETAIL, cy), t.wdim, t.wdim, t.wdim);
            out = over(out, (c, a * fade));
        }
        let first = if r.active { &self.ui.active } else if r.load { &self.ui.load } else { &self.ui.use_ };
        let b0 = right(&BUTTONS, 0);
        if x > b0.0 - 7.0 && x < b0.1 + 7.0 {
            let b = self.labelled(b0, cy, first.as_ref(), Hit::Use(i), x, y);
            out = over(out, if r.active { (b.0, b.1 * 0.6) } else { b }); // already active, so greyed out
        }
        out = over(out, self.labelled(right(&BUTTONS, 1), cy, self.ui.machines.as_ref(), Hit::Machines(i), x, y));
        let rn = self.labelled(right(&BUTTONS, 2), cy, self.ui.rename.as_ref(), Hit::Rename(i), x, y);
        out = over(out, if r.name == TEMPORARY { (rn.0, rn.1 * 0.6) } else { rn });
        over(out, self.x_button(right(&BUTTONS, 3), Hit::Delete(i), self.k.armed == Some(i), r.active, x, y, cy))
    }

    /// The foot: [Save as workspace] and the status, or the Save as field.
    fn foot(&self, x: f64, y: f64) -> Px {
        let (t, g, n) = (&self.p.t, self.ui.ascii.as_ref(), self.k.rows.len());
        let cy = row_y(n);
        if let Some(tp) = self.k.typing.as_ref().filter(|tp| tp.row.is_none()) {
            return self.typing(tp, true, x, y, cy);
        }
        let mut out = self.labelled((PAD, PAD + NEW), cy, self.ui.save_as.as_ref(), Hit::SaveAs, x, y);
        let (s0, s1) = (PAD + NEW + 2.0 * GAP, W - PAD);
        if x > s0 && x < s1 {
            let left = s0.min(s1 - typed_w(g, &self.k.status)); // a long one shows its end
            out = over(out, tint(typed(g, &self.k.status, x, y, left, cy), t.wtext, t.wtext, t.wtext));
        }
        out
    }

    fn pixel(&self, x: f64, y: f64) -> Px {
        let n = self.k.rows.len();
        let Some((mut out, d)) = ui::chrome(&self.p, self.ui.title.as_ref(), W, height(n), x, y) else { return ([0.0; 3], 0.0) };
        let i = ((y - TITLE) / ROW).floor();
        if i >= 0.0 && (i as usize) < n {
            out = over(out, self.row(i as usize, x, y));
        } else if i >= 0.0 && i as usize == n {
            out = over(out, self.foot(x, y));
        }
        ui::border(&self.p, out, d)
    }
}

/// Applies the active workspace again, after a Use, a Load, cc-home's `workspace reload`, or a
/// machine going in or out of the active one. Only its machines stay connected (its others
/// marked autoconnect=yes connect now, the rest disconnect), then every panel, the taskbar and
/// the windows go to its spots.
pub fn switched(grab: &mut grab::Grab, windows: &mut Windows, bar: &mut Taskbar) {
    config::refresh_members();
    for p in panels().iter().filter(|p| matches!(p.src, Source::Rdp) && p.used() && p.v.pop.is_none()) {
        match (config::member(&p.v), p.live()) {
            (false, true) => crate::disconnect(p),
            (true, false) if p.v.auto => {
                crate::place_remote(p);
                super::connect_later(p.index);
            }
            _ => {}
        }
    }
    super::machines::recall(grab, windows, bar);
    let active = config::workspaces().map(|d| conf::active_workspace(&d)).unwrap_or_default();
    eprintln!("workspace: {active} (switched)");
}

pub struct Workspace {
    ov: Option<vr::Handle>,
    shown: bool,
    ui: Arc<Ui>,
    rows: Vec<Row>,
    n: usize, // the rows it's shaped for (grab.rs reshape_extra)
    hover: Option<Hit>,
    armed: Option<usize>,
    typing: Option<Typing>,
    osk: bool, // SteamVR's keyboard is open for the field
    status: String,
    reload: bool, // run switched() on the tick, since act has no grab
    painter: ui::Painter<Key>,
}

impl Workspace {
    pub fn new(assets: &str) -> Workspace {
        let load = |k: &str| crate::load_tag(&format!("{assets}/tag-ui-{k}.rgba"));
        Workspace {
            ov: None,
            shown: false,
            ui: Arc::new(Ui {
                title: load("workspace"),
                use_: load("use"),
                load: load("load"),
                active: load("active"),
                machines: load("wsmachines"),
                rename: load("rename"),
                save_as: load("saveas"),
                save: load("wssave"),
                ascii: crate::load_tag(&format!("{assets}/ascii.rgba")),
            }),
            rows: Vec::new(),
            n: 0,
            hover: None,
            armed: None,
            typing: None,
            osk: false,
            status: String::new(),
            reload: false,
            painter: ui::Painter::default(),
        }
    }

    /// Under a laser, so the loop keeps the display's rate.
    pub fn moving(&self) -> bool {
        self.hover.is_some()
    }

    fn refresh(&mut self) {
        match config::workspaces() {
            Ok(d) => self.rows = rows_of(&d, config::UNIVERSE.load(Relaxed)),
            Err(e) => self.status = e,
        }
    }

    pub fn tick(&mut self, grab: &mut grab::Grab, windows: &mut Windows, bar: &mut Taskbar) {
        if std::mem::take(&mut self.reload) {
            switched(grab, windows, bar);
        }
        let open = OPEN.load(Relaxed);
        if open && self.ov.is_none() {
            self.open(grab);
        } else if !open && let Some(h) = self.ov.take() {
            self.hide_keyboard();
            grab.close_extra("workspace");
            crate::gpu::forget(h);
            call!(ov, DestroyOverlay, h);
            (self.shown, self.hover, self.painter, self.typing, self.armed) = (false, None, ui::Painter::default(), None, None);
            eprintln!("workspace: closed");
        }
        let Some(h) = self.ov else { return };
        self.events(h, grab, windows);
        self.keyboard();
        if let Some(n) = self.painter.drawn().map(|k| k.rows.len())
            && n != self.n
        {
            self.n = n;
            grab.reshape_extra(height(n) / W);
        }
        let show = !HIDDEN.load(Relaxed) && !bar.game();
        if show != self.shown {
            self.shown = show;
            if !show {
                self.hide_keyboard();
            }
            grab.set_extra_shown(show);
            if show { call!(ov, ShowOverlay, h) } else { call!(ov, HideOverlay, h) };
        }
        if show {
            self.paint(h);
        }
    }

    fn open(&mut self, grab: &mut grab::Grab) {
        if grab.extra_taken() {
            return; // another window still has the slot; it closes on its tick and this one opens on the next
        }
        let h = match vr::create_overlay("controlcenter.workspace", "Command Center workspace") {
            Ok(h) => h,
            Err(e) => {
                eprintln!("workspace: no overlay: {e}");
                OPEN.store(false, Relaxed);
                return;
            }
        };
        call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        call!(ov, SetOverlaySortOrder, h, SORT);
        self.refresh();
        self.n = self.rows.len();
        let pose = ui::spot("workspace", W * MPP);
        let pl = Placement::from_matrix(&panel_matrix(&pose), pose.width, height(self.n) / W, pose.curve);
        grab.open_extra("workspace", h, SORT - 1, theme::CYAN, pl, &OPEN);
        self.ov = Some(h);
        eprintln!("workspace: opened");
    }

    fn events(&mut self, h: vr::Handle, grab: &mut grab::Grab, windows: &mut Windows) {
        let n = self.rows.len();
        let t = typing_row(self.typing.as_ref(), n);
        let drawn = self.painter.drawn().map(|k| k.rows.len());
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        while ui::next_event(h, &mut e) {
            let (m, dev) = (unsafe { e.data.mouse }, e.trackedDeviceIndex);
            // in units from the bottom left; none while the texture is drawn for a different row count
            let at = held(self.hover, n, t, m.x as f64, height(n) - m.y as f64).filter(|_| drawn == Some(n));
            grab.panel_event(grab.slot(), &e, &mut KVM.lock().unwrap());
            match e.eventType {
                sys::EVREventType_VREvent_MouseMove => self.hover = at,
                sys::EVREventType_VREvent_FocusLeave => self.hover = None,
                sys::EVREventType_VREvent_MouseButtonDown if m.button == sys::EVRMouseButton_VRMouseButton_Left => {
                    if vr::is_real_controller(dev) {
                        let mut k = KVM.lock().unwrap();
                        if k.awake {
                            k.set_awake(false); // the last device pressed is primary (R-2)
                        }
                    }
                    match at {
                        Some(a) => self.act(a, h),
                        None => self.hide_keyboard(),
                    }
                }
                sys::EVREventType_VREvent_MouseButtonUp => windows.plasma_release(),
                sys::EVREventType_VREvent_KeyboardCharInput | sys::EVREventType_VREvent_KeyboardDone if unsafe { e.data.keyboard }.uUserValue == OSK => {
                    let mut buf = [0u8; 256];
                    call!(ov, GetKeyboardText, buf.as_mut_ptr() as *mut _, buf.len() as u32);
                    if let Some(tp) = self.typing.as_mut().filter(|_| self.osk) {
                        tp.text = text_of(&buf);
                        if e.eventType == sys::EVREventType_VREvent_KeyboardDone {
                            self.osk = false;
                            self.act(Hit::Save, h); // Enter saves
                        }
                    }
                }
                sys::EVREventType_VREvent_KeyboardClosed if unsafe { e.data.keyboard }.uUserValue == OSK && self.osk => {
                    self.osk = false;
                    self.hide_keyboard();
                }
                _ => {}
            }
        }
    }

    /// Starts typing in the field: SteamVR's keyboard gets its text, and the physical keyboards come here too.
    fn type_in(&mut self, h: vr::Handle) {
        let Some(tp) = self.typing.as_mut() else { return };
        if tp.editing && self.osk {
            return;
        }
        tp.editing = true;
        KVM.lock().unwrap().focus_field("workspace", true);
        let (hint, text) = (CString::new("Workspace name").unwrap_or_default(), CString::new(tp.text.clone()).unwrap_or_default());
        let mode = sys::EGamepadTextInputMode_k_EGamepadTextInputModeNormal;
        let lines = sys::EGamepadTextInputLineMode_k_EGamepadTextInputLineModeSingleLine;
        let e = call!(ov, ShowKeyboardForOverlay, h, mode, lines, 0, hint.as_ptr() as *mut _, 40, text.as_ptr() as *mut _, OSK);
        if e == 0 {
            self.osk = true;
        } else {
            self.status = format!("no keyboard (SteamVR's error {e})");
        }
    }

    /// Leaves the field: closes SteamVR's keyboard (keeping its text) and gives the physical keyboards back.
    fn hide_keyboard(&mut self) {
        if std::mem::take(&mut self.osk) {
            let mut buf = [0u8; 256];
            call!(ov, GetKeyboardText, buf.as_mut_ptr() as *mut _, buf.len() as u32);
            if let Some(tp) = self.typing.as_mut() {
                tp.text = text_of(&buf);
            }
            call!(ov, HideKeyboard);
        }
        if let Some(tp) = self.typing.as_mut() {
            tp.editing = false;
        }
        KVM.lock().unwrap().focus_field("workspace", false);
    }

    /// Physical keyboard keys in the field. Enter saves, Escape cancels.
    fn keyboard(&mut self) {
        if !self.typing.as_ref().is_some_and(|t| t.editing) {
            return;
        }
        let Some(keys) = KVM.lock().unwrap().field_keys("workspace") else { return self.hide_keyboard() };
        for (code, value, shift) in keys {
            let Some(e) = ui::edit(code, value, shift) else { continue };
            if std::mem::take(&mut self.osk) {
                call!(ov, HideKeyboard); // the physical keyboard has it now
            }
            let Some(tp) = self.typing.as_mut() else { return };
            match e {
                Edit::Char(c) if tp.text.chars().count() < 40 => tp.text.push(c),
                Edit::Back => drop(tp.text.pop()),
                Edit::Submit => return self.save(),
                Edit::Leave => {
                    self.hide_keyboard();
                    self.typing = None;
                    return;
                }
                Edit::Char(_) | Edit::Next => {}
            }
        }
    }

    /// Save, for the rename or for Save as.
    fn save(&mut self) {
        self.hide_keyboard();
        let Some(tp) = self.typing.clone() else { return };
        let name = tp.text.trim().to_string();
        let r = match tp.row.and_then(|i| self.rows.get(i)) {
            Some(r) => {
                let old = r.name.clone();
                config::edit_workspaces(|d| conf::rename_workspace(d, &old, &name)).map(|_| format!("renamed {old} to {name}"))
            }
            None => config::edit_workspaces(|d| conf::save_workspace_as(d, &name, config::UNIVERSE.load(Relaxed)))
                .map(|_| format!("saved as {name}, bound to this room")),
        };
        match r {
            Ok(s) => {
                eprintln!("workspace: {s}");
                (self.status, self.typing) = (s, None);
            }
            Err(e) => self.status = e, // the field stays open so you can fix it
        }
        self.refresh();
    }

    fn act(&mut self, a: Hit, h: vr::Handle) {
        if !matches!(a, Hit::Field | Hit::Save) {
            self.hide_keyboard();
        }
        let armed = self.armed.take();
        match a {
            Hit::Use(i) if self.rows[i].active => self.status = format!("{} is the active one", self.rows[i].title),
            Hit::Use(i) => {
                let r = self.rows[i].clone();
                let done = if r.load {
                    let (head, yaw) = head_pose();
                    let anchors = self.anchors(&r.name);
                    config::edit_workspaces(|d| conf::load_workspace(d, &r.name, head, yaw, &anchors)).map(|n| format!("loaded {} here: {n} panels, in Temporary ({} stays as it was)", r.title, r.title))
                } else {
                    config::edit_workspaces(|d| conf::use_workspace(d, &r.name)).map(|_| format!("using {}", r.title))
                };
                match done {
                    Ok(s) => {
                        eprintln!("workspace: {s}");
                        (self.status, self.reload) = (s, true);
                    }
                    Err(e) => self.status = e,
                }
                self.refresh();
            }
            Hit::Machines(i) => {
                super::machines::scope(Some(&self.rows[i].name));
                super::show(&super::machines::OPEN, true); // this one closes, and Machines opens on its tick
            }
            Hit::Rename(i) if self.rows[i].name == TEMPORARY => self.status = "Temporary keeps its name: Save as workspace makes it one".into(),
            Hit::Rename(i) => {
                self.typing = Some(Typing { row: Some(i), text: self.rows[i].name.clone(), editing: false });
                self.type_in(h);
            }
            Hit::Delete(i) if self.rows[i].active => self.status = "the active one: use another first".into(),
            Hit::Delete(i) if armed == Some(i) => {
                let name = self.rows[i].name.clone();
                self.status = match config::edit_workspaces(|d| conf::forget_workspace(d, &name)) {
                    Ok(()) => format!("deleted {}", conf::workspace_title(&name)),
                    Err(e) => e,
                };
                eprintln!("workspace: {}", self.status);
                self.refresh();
            }
            Hit::Delete(i) => {
                self.armed = Some(i);
                self.status = format!("x again deletes {}", self.rows[i].title);
            }
            Hit::SaveAs => {
                self.typing = Some(Typing { row: None, text: String::new(), editing: false });
                self.type_in(h);
            }
            Hit::Field => self.type_in(h),
            Hit::Save => self.save(),
            Hit::Unedit => self.typing = None,
        }
    }

    /// The monitors of workspace name's primary machine, which anchor its layout when it's loaded elsewhere.
    fn anchors(&self, name: &str) -> Vec<String> {
        let primary = config::workspaces().ok().and_then(|d| d.at("workspaces").at(name).at("primary").str().map(str::to_owned)).unwrap_or_default();
        panels().iter().filter(|p| matches!(p.src, Source::Rdp) && p.used() && !primary.is_empty() && config::machine_of(&p.v) == primary).map(|p| p.v.name.clone()).collect()
    }

    fn paint(&mut self, h: vr::Handle) {
        let key = Key { rows: self.rows.clone(), hover: self.hover, armed: self.armed, typing: self.typing.clone(), status: self.status.clone(), theme: theme::generation() };
        self.painter.paint(h, key, || {
            let (ui, p) = (self.ui.clone(), Paint::new(&theme::get(), theme::CYAN));
            move |was: Option<Key>, px, k: &Key| {
                let sc = Scene { k, p, ui: &ui };
                let ht = height(k.rows.len());
                let s = tex_scale(W, ht, SCALE);
                ui::redraw(px, ((W * s) as usize, (ht * s) as usize), s, (W, ht), was.and_then(|w| dirty(&w, k)), |x, y| sc.pixel(x, y))
            }
        });
    }

    pub fn destroy(&mut self) {
        if let Some(h) = self.ov.take() {
            self.hide_keyboard();
            crate::gpu::forget(h);
            call!(ov, DestroyOverlay, h);
        }
    }
}

/// Where the head is and which way it faces (yaw 0 looks down -z). If that's unknown, the room's origin.
fn head_pose() -> ([f64; 3], f64) {
    let m = vr::head().unwrap_or([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.6], [0.0, 0.0, 1.0, 0.0]]);
    ([m[0][3] as f64, m[1][3] as f64, m[2][3] as f64], (m[0][2] as f64).atan2(m[2][2] as f64).to_degrees())
}

/// The SteamVR keyboard's text, up to the NUL: printable only, 40 characters at most.
fn text_of(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).chars().filter(|c| !c.is_control()).take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Json {
        Json::parse(r#"{"workspace": "home", "workspaces": {
            "home": {"spots": {"home": {"desk": {}, "app:konsole": {}, "taskbar": {}}}, "universe": "8000000000000000002"},
            "office": {"spots": {}, "universe": "222", "machines": ["desk"]},
            "temporary": {"spots": {"home": {"desk": {}}}, "loaded": "home"}}}"#).unwrap()
    }

    #[test]
    fn rows_say_where_each_belongs() {
        let r = rows_of(&data(), 8000000000000000002);
        assert_eq!(r.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["Temporary", "home", "office"], "Temporary first");
        assert_eq!(r[1].detail, "this room, 2 panels");
        assert!(r[1].active && !r[1].load && !r[0].load);
        assert_eq!(r[2].detail, "room ..222, 0 panels, 1 machine");
        assert!(r[2].load, "another room's: Load for travel");
        assert_eq!(r[0].detail, "for travelling, 1 panel, from home");
        let r = rows_of(&data(), 0);
        assert!(r.iter().all(|r| !r.load), "no room known: Use");
    }

    #[test]
    fn hits() {
        let mid = |(x0, x1): (f64, f64)| (x0 + x1) / 2.0;
        assert_eq!(hit(3, None, mid(right(&BUTTONS, 0)), row_y(1)), Some(Hit::Use(1)));
        assert_eq!(hit(3, None, mid(right(&BUTTONS, 1)), row_y(2)), Some(Hit::Machines(2)));
        assert_eq!(hit(3, None, mid(right(&BUTTONS, 2)), row_y(0)), Some(Hit::Rename(0)));
        assert_eq!(hit(3, None, mid(right(&BUTTONS, 3)), row_y(0)), Some(Hit::Delete(0)));
        assert_eq!(hit(3, None, PAD + 10.0, row_y(3)), Some(Hit::SaveAs));
        assert_eq!(hit(3, None, NAME + 10.0, row_y(0)), None, "a name");
        assert_eq!(hit(3, Some(1), NAME + 10.0, row_y(1)), Some(Hit::Field), "renamed: its field");
        assert_eq!(hit(3, Some(1), mid(right(&EDIT, 0)), row_y(1)), Some(Hit::Save));
        assert_eq!(hit(3, Some(3), PAD + 10.0, row_y(3)), Some(Hit::Field), "Save as's field");
        assert_eq!(hit(3, Some(3), mid(right(&EDIT, 1)), row_y(3)), Some(Hit::Unedit));
        assert!(DETAIL + 200.0 < right(&BUTTONS, 0).0, "room for where it belongs");
        let gap = row_y(0) + BTN_H / 2.0 + 2.0;
        assert_eq!(held(Some(Hit::Use(0)), 3, None, mid(right(&BUTTONS, 0)), gap), Some(Hit::Use(0)));
    }

    #[test]
    fn a_patch_draws_as_all_of_it() {
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::CYAN));
        let rows = rows_of(&data(), 8000000000000000002);
        let key = |hover, typing, status: &str| Key { rows: rows.clone(), hover, armed: None, typing, status: status.into(), theme: 0 };
        let s = 1.5;
        let n = rows.len();
        let (tw, th) = ((W * s) as usize, (height(n) * s) as usize);
        let draw = |k: &Key| {
            let sc = Scene { k, p, ui: &ui };
            grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
        };
        let t = |row| Some(Typing { row, text: "flat".into(), editing: true });
        let cases = [
            (key(None, None, ""), key(Some(Hit::Use(1)), None, "")),
            (key(Some(Hit::Use(1)), None, ""), key(Some(Hit::Delete(2)), None, "x again")),
            (key(None, None, ""), key(None, t(Some(1)), "")),
            (key(None, t(Some(1)), ""), key(Some(Hit::Save), t(Some(1)), "")),
            (key(Some(Hit::Field), t(None), ""), key(Some(Hit::SaveAs), None, "saved as flat")),
            (key(None, None, ""), Key { armed: Some(2), ..key(None, None, "x again deletes office") }),
        ];
        for (a, b) in cases {
            let sc = Scene { k: &b, p, ui: &ui };
            let tx = ui::redraw(draw(&a), (tw, th), s, (W, height(n)), dirty(&a, &b), |x, y| sc.pixel(x, y));
            assert!(!tx.full && tx.px == draw(&b), "{a:?} -> {b:?}");
        }
    }
}
