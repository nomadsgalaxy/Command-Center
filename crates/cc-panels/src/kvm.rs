//! Our own pointer and keyboard, basically a software KVM. While cc-panels runs, the mice
//! are ours and drive a 3D pointer across the panels. The pointer wakes on deliberate motion
//! and sleeps when it's idle.
//!
//! Typing is click to type. A click on a panel (a remote monitor, a Frame window, Plasma's bar)
//! engages the keyboards and they type there. A click off our panels (empty space, or SteamVR's
//! own UI through the laser), the dashboard opening, or the Desktop hidden or under a game
//! gives them back to the Frame. A clean Right Ctrl tap still hands them over either way, and
//! we watch for that tap either way too, but it's not needed: the Targus folding keyboard I use
//! has no Right Ctrl at all.
//!
//! Two things don't give them back. The pointer sleeping doesn't, because it sleeps after 30 s
//! without the mouse, and a long stretch of typing is exactly that. Looking away doesn't either,
//! even with gaze lock on, because looking down at the keyboard to find a key is looking away,
//! and typing would land on the Frame mid-word.
use crate::config::Viewer;
use crate::geometry::{Placement, angles, direction, norm, rotate};
use crate::{QUIT, call, laser, panel, plasmabar, root, vr, windows};
use freerdp_sys::{PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3, PTR_FLAGS_DOWN, PTR_FLAGS_MOVE};
use std::collections::HashSet;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

// linux/input-event-codes.h
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const KEY_ESC: u16 = 1;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_A: u16 = 30;
const KEY_Z: u16 = 44;
const KEY_SPACE: u16 = 57;
const KEY_F1: u16 = 59;
const KEY_F4: u16 = 62;
const KEY_F12: u16 = 88;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_HOME: u16 = 102;
const KEY_G: u16 = 34;
const KEY_D: u16 = 32;
const BTN_MOUSE: u16 = 0x110;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_JOYSTICK: u16 = 0x120;
const BUS_VIRTUAL: u16 = 6;

/// How long the gaze has to stay on another panel before it takes the pointer.
const GAZE_DWELL: Duration = Duration::from_millis(150);
/// Gaze older than this doesn't count (blinks, the tracker off, the headset off).
pub const GAZE_STALE: Duration = Duration::from_millis(300);

/// Set when a key, a wheel turn or the awake pointer's motion came in since the main loop
/// last looked. It keeps the display at its rate for a while (main.rs Pace).
pub static POKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// While asleep, or after a pause, mouse motion only counts once this much of it comes within
/// WAKE_WINDOW, so a knock on the desk or a jittery sensor doesn't wake it. After IDLE without
/// mouse use the cursor sleeps. Waking and sleeping never touch the keyboard.
const WAKE_COUNTS: f64 = 40.0;
const WAKE_WINDOW: Duration = Duration::from_secs(1);
const RESUME_PAUSE: Duration = Duration::from_millis(1500);
const IDLE: Duration = Duration::from_secs(30);
/// Input this recent makes its panel the one you're working in (D-042: full rate wherever you look).
const WORKING: Duration = Duration::from_secs(2);

/// How far out (m) the cursor floats before it's been on any panel.
const FREE_DISTANCE: f64 = 1.5;
/// Just off a panel, the cursor stays on its surface up to this far (m) past the edge, so you
/// cross the gap between two monitors at their depth.
const EDGE_REACH: f64 = 0.3;
/// A drag on a window panel that leaves it: up to this far (m) past the edge the pointer stays
/// on the window, so a scrollbar or a selection dragged off its edge keeps working. Further
/// out it's off every window, so a browser tab dropped there becomes its own window.
const TEAR_OFF: f64 = 0.08;

// linux/input.h ioctls
const fn ioc(dir: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}
const EVIOCGRAB: u64 = ioc(1, 0x90, 4);
const EVIOCGID: u64 = ioc(2, 0x02, 8);
const fn eviocgname(len: u64) -> u64 {
    ioc(2, 0x06, len)
}
const fn eviocgbit(ev: u64, len: u64) -> u64 {
    ioc(2, 0x20 + ev, len)
}

/// Linux key codes match PC scancodes (set 1) for the main block; the rest are extended keys.
pub fn scancode(code: u16) -> u32 {
    const E: u32 = 0x100; // KBDEXT
    match code {
        96 => 0x1C | E,  // KP Enter
        97 => 0x1D | E,  // Right Ctrl
        98 => 0x35 | E,  // KP /
        99 => 0x37 | E,  // SysRq
        100 => 0x38 | E, // Right Alt
        102 => 0x47 | E, // Home
        103 => 0x48 | E, // Up
        104 => 0x49 | E, // Page Up
        105 => 0x4B | E, // Left
        106 => 0x4D | E, // Right
        107 => 0x4F | E, // End
        108 => 0x50 | E, // Down
        109 => 0x51 | E, // Page Down
        110 => 0x52 | E, // Insert
        111 => 0x53 | E, // Delete
        125 => 0x5B | E, // Left Meta
        126 => 0x5C | E, // Right Meta
        127 => 0x5D | E, // Compose
        c if c > 0 && c <= KEY_F12 => c as u32,
        _ => 0,
    }
}

struct Device {
    path: String,
    name: String,
    file: File,
    mouse_only: bool, // held for as long as we run, since its motion is what wakes the pointer
    grabbed: bool,    // ours right now; events from a device that isn't go to the Frame
    pending: bool,    // grab it once its keys are all up, or a held key would stick on the Frame
}

impl Device {
    fn grab(&mut self, on: bool) -> bool {
        let ok = unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, on as libc::c_int) } == 0;
        self.grabbed = on && ok;
        self.pending = false;
        ok
    }

    /// Whether any key or button is down on it right now (EVIOCGKEY).
    fn keys_down(&self) -> bool {
        let mut b = [0u8; 0x300 / 8];
        (unsafe { libc::ioctl(self.file.as_raw_fd(), ioc(2, 0x18, b.len() as u64) as _, b.as_mut_ptr()) }) >= 0 && b.iter().any(|&x| x != 0)
    }

    /// Grab it now if nothing's held on it, otherwise as soon as nothing is.
    fn grab_when_free(&mut self) {
        if self.keys_down() {
            self.pending = true;
        } else if !self.grab(true) {
            eprintln!("input: {} is held by something else ({})", self.name, std::io::Error::last_os_error());
        }
    }
}

pub struct Kvm {
    pub place: Vec<Placement>, // where each panel is (the pointer's geometry)
    pub ppd: Vec<f64>,         // each panel's pixels per degree from where the head is now
    devices: Vec<Device>,
    pub engaged: bool, // keyboards are ours and type into a remote (click to type, or a Right Ctrl tap)
    pub awake: bool,   // the cursor shows (mouse motion wakes it, IDLE puts it to sleep)
    pub active: usize, // the panel the cursor is on, where mouse input goes
    pub kbd: usize,    // the panel you clicked last, where typing goes
    pub kbd_shell: bool, // ...unless that was Plasma's bar, then typing goes to the session (Kickoff's search)
    pub x: f64, // the cursor in the active panel's pixels (where it last was on a panel)
    pub y: f64,
    // The 3D pointer: a ray from where the head was at the last
    // recenter (anchor), turned by the mouse (yaw, pitch). It lands on the panel it meets as
    // seen from the eye. Between panels the cursor floats FREE_DISTANCE out (free = Some(point)).
    anchor: [f64; 3],
    yaw: f64,
    pitch: f64,
    pub free: Option<[f64; 3]>,
    // The taskbar's whole frame (taskbar.rs) while it's shown. The ray takes it unless a nearer
    // panel covers it, and the cursor floats on it (free). Its left clicks get counted for the
    // taskbar to pick up, never sent through SteamVR's laser.
    pub bar: Option<Placement>,
    pub bar_inset: f64, // its clear laser reach around the frame (m), which isn't the cursor's
    pub on_bar: Option<(f64, f64)>, // where the cursor is on it: u right, v up from its middle (m)
    pub bar_clicks: u32,
    // The Machines or Preferences window (grab.rs Extra) while it's shown, and its card's
    // margins around it (sides, below, above, m). It's nearer than panels and bars, so the ray
    // takes it. The cursor floats on it, and SteamVR's laser, starting just short of it, gives
    // it and its card their events like on a panel's card. That's why clicks need the laser
    // (laser::HEALTHY).
    // ponytail: the laser, not the taskbar's on_bar/bar_clicks, because one path already gives
    // the card's bar, corners, carry and wheel, plus both windows' hover, drags and sliders,
    // their events like a controller's. Doing it the taskbar's way would copy all of that for
    // the mouse. Without the laser (binding loads spent, or a real controller in the off hand so
    // both controllers are on), a left press on a card still carries, resizes and bends
    // (card_mouse, card_wheel), and the windows get laser events made from the ray (ui.rs
    // next_event: extra_at, extra_mouse).
    pub extra: Option<(Placement, [f64; 3])>,
    pub on_extra: bool,
    on_card: bool, // the cursor is on the active panel's card (to_card)
    card_held: bool, // left button pressed on a card or the Extra with no laser, so grab has it
    pub card_mouse: Vec<(usize, bool)>, // ...its presses (slot, or usize::MAX for the Extra) and releases, for grab.rs poll
    pub card_wheel: f64, // ...and the wheel's notches meanwhile (push, bend, zoom)
    extra_held: bool, // left button pressed on the Extra's window with no laser
    pub extra_mouse: Vec<bool>, // ...its presses and releases, for ui.rs next_event
    // Each panel's card margins, same idea (grab.rs): past its picture but on its card, and
    // nearer than the panels behind, the ray takes it (like a pop-out's frame in front of its
    // monitor).
    pub cards: Vec<[f64; 3]>,
    // Plasma's bar (plasmabar.rs), same idea: the cursor floats on it, and its moves, clicks
    // and wheel go to the session at that point.
    pub plasma: Option<Placement>,
    on_plasma: bool,
    shell_buttons: u32, // mouse buttons held there
    plasma_at: (f64, f64), // the cursor's point in the session there, as last sent
    sent: Option<(usize, i32, i32)>, // the last move sent (re-landing every frame only sends changes)
    depth: f64, // how far along the ray the last panel was, so off panels the cursor floats that far out
    // Eye tracking: the panel you're looking at owns the pointer. Mouse motion can't take it
    // past that panel's edges, and looking at another panel moves it there on the next motion.
    // A quick glance (under GAZE_DWELL) or a look between panels changes nothing.
    pub gaze_lock: bool, // off by default so the mouse roams free (turn it on with CC_GAZE=1 or Right Ctrl + G)
    pub focus: Option<usize>,
    gaze_on: Option<(usize, Instant, f64, f64)>, // the panel under the gaze now, since when, and u, v
    gaze_at: Option<Instant>,                    // the last valid sample
    pub sensitivity: f64, // degrees per mouse count
    keys: Vec<u16>,       // keys (evdev codes) held down on the typing panel's machine or window
    field: Option<&'static str>,       // the window whose text field has the keyboards (focus_field)
    field_keys: Vec<(u16, i32, bool)>, // ...and keys typed there since it last checked: code, value, Shift
    field_held: Vec<u16>,              // keys pressed there, so their repeats and release stay out of a remote
    buttons: u32,         // mouse buttons held there
    beam_buttons: u32,    // mouse buttons held through the SteamVR laser (off our panels)
    beam_depth: f64,      // ...and how far out the cursor was then, which it keeps until the release
    dx: f64,              // motion since the last report
    dy: f64,
    last_used: Option<Instant>,       // the mouse's last use while awake, for the idle sleep
    typed: Option<Instant>,          // the last key typed into a panel
    pending: (f64, Option<Instant>), // motion counted toward waking, and since when
    rctrl: Option<Instant>, // when Right Ctrl went down, to spot the tap
    rctrl_clean: bool,
    shift: bool,
    pub moves: u32, // sent since the last status line
    pub clicks: u32,
    pub keystrokes: u32,
    pub wheels: u32,
    // Where the cursor is while a button's held on a window panel, and when. A window that
    // shows up meanwhile or right after (a browser tab dragged out) gets its panel there
    // (windows.rs adopt).
    pub window_drag: Option<([f64; 3], Instant)>,
}

pub static KVM: Mutex<Kvm> = Mutex::new(Kvm {
    place: Vec::new(),
    ppd: Vec::new(),
    devices: Vec::new(),
    engaged: false,
    awake: false,
    active: 0,
    kbd: 0,
    kbd_shell: false,
    x: 0.0,
    y: 0.0,
    anchor: [0.0; 3],
    yaw: 0.0,
    pitch: 0.0,
    free: None,
    bar: None,
    bar_inset: 0.0,
    on_bar: None,
    bar_clicks: 0,
    extra: None,
    on_extra: false,
    on_card: false,
    card_held: false,
    card_mouse: Vec::new(),
    card_wheel: 0.0,
    extra_held: false,
    extra_mouse: Vec::new(),
    cards: Vec::new(),
    plasma: None,
    on_plasma: false,
    shell_buttons: 0,
    plasma_at: (0.0, 0.0),
    sent: None,
    depth: FREE_DISTANCE,
    gaze_lock: false,
    focus: None,
    gaze_on: None,
    gaze_at: None,
    sensitivity: 0.03,
    keys: Vec::new(),
    field: None,
    field_keys: Vec::new(),
    field_held: Vec::new(),
    buttons: 0,
    beam_buttons: 0,
    beam_depth: 1.5,
    dx: 0.0,
    dy: 0.0,
    last_used: None,
    typed: None,
    pending: (0.0, None),
    rctrl: None,
    rctrl_clean: false,
    shift: false,
    moves: 0,
    clicks: 0,
    keystrokes: 0,
    wheels: 0,
    window_drag: None,
});

impl Kvm {
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Let go of the mouse buttons held on the cursor's machine, since it's moving to another panel.
    fn release_buttons(&mut self) {
        for (b, bit) in [(PTR_FLAGS_BUTTON1, laser::TRIGGER), (PTR_FLAGS_BUTTON2, laser::B), (PTR_FLAGS_BUTTON3, laser::X)] {
            if self.beam_buttons & b != 0 {
                laser::press(bit, false);
            }
        }
        self.beam_buttons = 0;
        if std::mem::take(&mut self.card_held) {
            self.card_mouse.push((self.active, false));
        }
        if std::mem::take(&mut self.extra_held) {
            self.extra_mouse.push(false);
        }
        for b in [PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3] {
            if self.shell_buttons & b != 0 {
                windows::shell_mouse(b, None);
            }
        }
        self.shell_buttons = 0;
        for b in [PTR_FLAGS_BUTTON1, PTR_FLAGS_BUTTON2, PTR_FLAGS_BUTTON3] {
            if self.buttons & b != 0 {
                panel(self.active).mouse(b, self.x, self.y);
            }
        }
        self.buttons = 0;
    }

    /// Let go of the keys held on the typing machine, because typing moves or we're releasing.
    pub fn release_keys(&mut self) {
        for code in std::mem::take(&mut self.keys) {
            self.send_key(code, 0);
        }
    }

    /// Send a key to the typing panel, or to the session after a click on Plasma's bar.
    fn send_key(&self, code: u16, value: i32) {
        if self.kbd_shell { windows::key(code, value) } else { panel(self.kbd).key(code, value) }
    }

    /// Typing goes to panel i from now on (you clicked it), and keys held elsewhere get let go.
    pub fn type_to(&mut self, i: usize) {
        self.leave_field();
        if self.kbd != i || self.kbd_shell {
            self.release_keys();
            (self.kbd, self.kbd_shell) = (i, false);
        }
    }

    /// Typing goes to the session's focused window (a click on Plasma's bar, for Kickoff's search).
    pub fn type_to_shell(&mut self) {
        self.leave_field();
        if !self.kbd_shell {
            self.release_keys();
            self.kbd_shell = true;
        }
    }

    fn release_held(&mut self) {
        self.release_buttons();
        self.release_keys();
    }

    /// Give the keyboards to a Command Center window's text field (owner is the window),
    /// engaged or not, or take them back. While it has them, keys queue for field_keys and none
    /// go to a remote. Typing sent to a remote (type_to, type_to_shell) ends it.
    pub fn focus_field(&mut self, owner: &'static str, on: bool) {
        if !on {
            if self.field == Some(owner) {
                self.leave_field();
            }
            return;
        }
        self.release_keys(); // the ones held on the typing panel's machine
        (self.field, self.field_keys) = (Some(owner), Vec::new());
        self.grab_keyboards();
    }

    /// The keys (code, value, Shift) typed into owner's field since the last call, or None if it
    /// doesn't have the keyboards (anymore).
    pub fn field_keys(&mut self, owner: &str) -> Option<Vec<(u16, i32, bool)>> {
        (self.field == Some(owner)).then(|| std::mem::take(&mut self.field_keys))
    }

    fn leave_field(&mut self) {
        if self.field.take().is_some() {
            self.grab_keyboards();
        }
    }

    /// The keyboards are ours when engaged, or when a window's field has them.
    fn keyboards(&self) -> bool {
        self.engaged || self.field.is_some()
    }

    fn grab_keyboards(&mut self) {
        let on = self.keyboards();
        for d in self.devices.iter_mut().filter(|d| !d.mouse_only && (d.grabbed || d.pending) != on) {
            if on {
                d.grab_when_free();
            } else {
                d.grab(false);
            }
        }
    }

    /// Engaged means the keyboards are ours and typing goes to the panel you clicked last.
    /// Released, they're the Frame's (unless a window's field has them). Clicks change it (click
    /// to type, see the module doc), and so does a Right Ctrl tap. `why` goes in the log.
    pub fn set_engaged(&mut self, on: bool, why: &str) {
        if on == self.engaged {
            return;
        }
        if !on {
            self.release_keys();
        }
        self.engaged = on;
        self.grab_keyboards();
        eprintln!("keyboard {} ({why})", if on { "to the remotes" } else { "to the Frame" });
    }

    /// Whether panel i is taking your input right now: it's the cursor's (you used the mouse or
    /// hold a button on it), or the typing panel right after a key. You might be looking at the
    /// keyboard or another panel meanwhile.
    pub fn working(&self, i: usize) -> bool {
        let recent = |t: Option<Instant>| t.is_some_and(|t| t.elapsed() < WORKING);
        (self.awake && self.free.is_none() && self.active == i && (self.buttons != 0 || recent(self.last_used)))
            || (self.engaged && !self.kbd_shell && self.kbd == i && recent(self.typed))
    }

    /// Awake means the cursor shows, straight ahead when it wakes. The mice are ours either way.
    pub fn set_awake(&mut self, on: bool) {
        if on == self.awake {
            return;
        }
        self.awake = on;
        self.pending = (0.0, None);
        (self.dx, self.dy) = (0.0, 0.0); // the motion that woke it shouldn't move it
        if on {
            self.last_used = Some(Instant::now());
            self.rctrl_clean = false; // Right Ctrl held across waking is a chord, not the tap
            self.recenter();
        } else {
            self.release_buttons();
        }
        eprintln!("pointer {}", if on { "awake" } else { "asleep (move the mouse to wake it)" });
    }

    /// Mouse motion. Asleep or after a pause it has to be deliberate, then it moves the cursor.
    fn motion(&mut self) {
        let dormant = !self.awake || self.last_used.is_none_or(|t| t.elapsed() > RESUME_PAUSE);
        if dormant && !count_toward_waking(&mut self.pending, self.dx.abs() + self.dy.abs(), Instant::now()) {
            (self.dx, self.dy) = (0.0, 0.0);
            return;
        }
        if !self.awake {
            return self.set_awake(true);
        }
        self.pending = (0.0, None);
        self.last_used = Some(Instant::now());
        self.mv();
    }

    /// Runs every input-loop tick: the idle sleep, and keyboards waiting for their keys to come up.
    fn tick(&mut self) {
        if self.awake && self.buttons == 0 && self.last_used.is_some_and(|t| t.elapsed() > IDLE) {
            eprintln!("idle {}s", IDLE.as_secs());
            self.set_awake(false);
        }
        if self.keyboards() {
            for d in self.devices.iter_mut().filter(|d| d.pending) {
                d.grab_when_free();
            }
        }
    }

    /// The cursor's point in the room, and whether it's on one of our panels (or the taskbar).
    pub fn cursor(&self) -> ([f64; 3], bool) {
        (self.cursor_point(), self.free.is_none() || self.on_bar.is_some() || self.on_plasma || self.on_extra)
    }

    /// The ray as a device pose (origin at the anchor, -z along it, no roll), so grab.rs can
    /// carry cards on it (MOUSE).
    pub fn ray(&self) -> crate::geometry::Mat {
        let (x, y, z) = laser::aim_basis(&direction(self.yaw, self.pitch));
        [0, 1, 2].map(|r| [x[r] as f32, y[r] as f32, z[r] as f32, self.anchor[r] as f32])
    }

    /// Where the ray meets the Extra's window (not its card), from its middle (m), while the
    /// cursor is on it and no card is held.
    pub fn extra_at(&self) -> Option<(f64, f64)> {
        let (pl, _) = self.extra.filter(|_| self.on_extra && !self.card_held && self.beam_buttons == 0)?;
        let (_, u, v) = pl.hit(&self.anchor, &direction(self.yaw, self.pitch))?;
        (u.abs() <= pl.width / 2.0 && v.abs() <= pl.height / 2.0).then_some((u, v))
    }

    /// The cursor's point in the room: on the active panel, or floating.
    fn cursor_point(&self) -> [f64; 3] {
        if let Some(f) = self.free {
            return f;
        }
        let ((w, h), pl) = (panel(self.active).size(), &self.place[self.active]);
        let m = pl.on_surface((self.x / w as f64 - 0.5) * pl.width, (0.5 - self.y / h as f64) * pl.height, 0.0);
        [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64]
    }

    /// Restart the ray at the head and aim it where the cursor is, so the cursor doesn't jump
    /// after a panel moved under it.
    fn reaim(&mut self) {
        let p = self.cursor_point();
        self.anchor = vr::head_position();
        (self.yaw, self.pitch) = angles(&[p[0] - self.anchor[0], p[1] - self.anchor[1], p[2] - self.anchor[2]]);
    }

    /// Recenter: the ray starts at the head and points the way you're facing, so the
    /// cursor ends up straight ahead (on engaging, and with Right Ctrl + Space).
    pub fn recenter(&mut self) {
        let Some(m) = vr::head() else {
            return self.reaim(); // not tracked right now, so at least start from the last head position
        };
        self.anchor = [m[0][3] as f64, m[1][3] as f64, m[2][3] as f64];
        (self.yaw, self.pitch) = angles(&[-m[0][2] as f64, -m[1][2] as f64, -m[2][2] as f64]);
        self.pitch = self.pitch.clamp(-85.0, 85.0);
        self.land();
    }

    /// The nearest panel a ray meets inside its edges (one it can reach): (panel, distance, u, v).
    fn nearest(&self, o: &[f64; 3], d: &[f64; 3]) -> Option<(usize, f64, f64, f64)> {
        if crate::HIDDEN.load(Relaxed) {
            return None; // hidden panels catch nothing
        }
        self.place
            .iter()
            .enumerate()
            .filter(|&(i, _)| reachable(i))
            .filter_map(|(i, pl)| pl.hit(o, d).filter(|&(_, u, v)| u.abs() <= pl.width / 2.0 && v.abs() <= pl.height / 2.0).map(|(t, u, v)| (i, t, u, v)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Where a ray meets a bar (the taskbar, or Plasma's), if no panel it meets (`hit`) is
    /// nearer: (distance, u, v). The Frame draws by depth, so a nearer panel covers the bar, and
    /// SteamVR's laser sees it that way too.
    /// `inset`: the clear margin at the edges that it doesn't take.
    fn bar_hit(b: Option<Placement>, inset: f64, o: &[f64; 3], d: &[f64; 3], hit: Option<(usize, f64, f64, f64)>) -> Option<(f64, f64, f64)> {
        let b = b?;
        b.hit(o, d).filter(|&(t, u, v)| u.abs() <= b.width / 2.0 - inset && v.abs() <= b.height / 2.0 - inset && hit.is_none_or(|h| h.1 > t))
    }

    /// Where a ray meets the Extra window or its card, if no panel or bar it meets is nearer
    /// (a raised wrist bar, or a fixed one put in front): the distance.
    fn extra_hit(&self, o: &[f64; 3], d: &[f64; 3], hit: Option<(usize, f64, f64, f64)>) -> Option<f64> {
        let (pl, m) = self.extra?;
        Self::card_at(&pl, m, o, d, hit, self.bars(o, d, hit)).map(|h| h.0)
    }

    /// How far along a ray the nearest bar is that no panel it meets (`hit`) covers.
    fn bars(&self, o: &[f64; 3], d: &[f64; 3], hit: Option<(usize, f64, f64, f64)>) -> f64 {
        let bar = |b, inset| Self::bar_hit(b, inset, o, d, hit).map_or(f64::INFINITY, |h| h.0);
        bar(self.bar, self.bar_inset).min(bar(self.plasma, 0.0))
    }

    /// A ray on pl or its card (margins: sides, below, above), if it's nearer than `hit` and `bars`: (t, u, v).
    fn card_at(pl: &Placement, [side, below, above]: [f64; 3], o: &[f64; 3], d: &[f64; 3], hit: Option<(usize, f64, f64, f64)>, bars: f64) -> Option<(f64, f64, f64)> {
        pl.hit(o, d).filter(|&(t, u, v)| u.abs() <= pl.width / 2.0 + side && v >= -pl.height / 2.0 - below && v <= pl.height / 2.0 + above && hit.is_none_or(|h| h.1 > t) && t < bars)
    }

    /// The nearest panel card a ray meets past the picture, if no panel or bar it meets is
    /// nearer: (panel, distance).
    fn card_hit(&self, o: &[f64; 3], d: &[f64; 3], hit: Option<(usize, f64, f64, f64)>) -> Option<(usize, f64)> {
        if crate::HIDDEN.load(Relaxed) {
            return None;
        }
        let bars = self.bars(o, d, hit);
        self.place
            .iter()
            .zip(&self.cards)
            .enumerate()
            .filter(|&(i, _)| reachable(i))
            .filter_map(|(i, (pl, &m))| Self::card_at(pl, m, o, d, hit, bars).filter(|&(_, u, v)| u.abs() > pl.width / 2.0 || v.abs() > pl.height / 2.0).map(|h| (i, h.0)))
            // the card sits 2 mm behind its picture (grab.rs), so a near tie (monitors side by
            // side, scan alignment noise) goes to the panel, the way SteamVR's laser sees it
            .filter(|&(_, t)| hit.is_none_or(|h| h.1 > t + 0.01))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Float the cursor on panel i's card at p, t along the ray. SteamVR's laser gives the card
    /// its events, and i becomes active so its card shows (grab.rs).
    fn to_card(&mut self, i: usize, p: [f64; 3], t: f64) {
        if i != self.active {
            self.release_buttons();
            self.active = i;
        }
        (self.on_card, self.free, self.depth, self.sent) = (true, Some(p), t, None);
        windows::leave();
    }

    /// Float the cursor on the Extra at p, t along the ray. SteamVR's laser gives it the events.
    fn to_extra(&mut self, p: [f64; 3], t: f64) {
        (self.on_extra, self.free, self.depth, self.sent) = (true, Some(p), t, None);
        windows::leave();
    }

    /// A gaze sample (origin, unit direction), or None when there's no valid one.
    pub fn gaze(&mut self, g: Option<([f64; 3], [f64; 3])>) {
        let Some((o, d)) = g else { return };
        let now = Instant::now();
        self.gaze_at = Some(now);
        let hit = self.nearest(&o, &d);
        if Self::bar_hit(self.bar, self.bar_inset, &o, &d, hit).is_some() || Self::bar_hit(self.plasma, 0.0, &o, &d, hit).is_some() || self.extra_hit(&o, &d, hit).is_some()
            // a card in front of another panel counts (a pop-out's frame), the gap between monitors doesn't
            || (hit.is_some() && self.card_hit(&o, &d, hit).is_some())
        {
            (self.focus, self.gaze_on) = (None, None); // looking at a taskbar, the Extra or a card: the cursor's free to go there
            return;
        }
        let Some((i, _, u, v)) = hit else { return }; // between panels: nothing changes
        match self.gaze_on {
            Some((j, since, ..)) if j == i => {
                self.gaze_on = Some((i, since, u, v));
                if now - since >= GAZE_DWELL {
                    self.focus = Some(i);
                }
            }
            _ => self.gaze_on = Some((i, now, u, v)),
        }
    }

    fn gaze_fresh(&self) -> bool {
        self.gaze_at.is_some_and(|t| t.elapsed() < GAZE_STALE)
    }

    /// Aim the ray at a point on a panel (u, v from its middle).
    fn aim_at(&mut self, i: usize, u: f64, v: f64) {
        let m = self.place[i].on_surface(u, v, 0.0);
        let a = self.anchor;
        (self.yaw, self.pitch) = angles(&[m[0][3] as f64 - a[0], m[1][3] as f64 - a[1], m[2][3] as f64 - a[2]]);
    }

    /// Mouse motion turns the ray; the cursor goes where it lands.
    fn mv(&mut self) {
        // Looking at a different panel than the cursor's: the cursor starts from where you look.
        let focus = self.focus.filter(|_| self.gaze_lock && self.gaze_fresh() && self.buttons == 0);
        if let Some(f) = focus.filter(|&f| f != self.active || self.free.is_some()) {
            let (u, v) = match self.gaze_on {
                Some((j, _, u, v)) if j == f => (u, v),
                _ => (0.0, 0.0),
            };
            let pl = &self.place[f];
            let (u, v) = (u.clamp(-pl.width / 2.0, pl.width / 2.0), v.clamp(-pl.height / 2.0, pl.height / 2.0));
            self.anchor = vr::head_position();
            self.aim_at(f, u, v);
        }
        self.yaw -= self.dx * self.sensitivity; // right turns the ray right
        self.pitch = (self.pitch - self.dy * self.sensitivity).clamp(-85.0, 85.0);
        (self.dx, self.dy) = (0.0, 0.0);
        self.land();
    }

    /// Where the ray lands now. This runs every frame, not just on mouse motion, so leaning or
    /// turning your head (or a panel moving) re-lands the cursor on what you see.
    pub fn land(&mut self) {
        (self.on_bar, self.on_plasma) = (None, false);
        if self.beam_buttons != 0 || self.card_held {
            // Held through the laser (a drag on SteamVR's UI, or carrying a panel by its bar),
            // the cursor keeps its distance and lands on nothing until the release (a drag
            // lock). That way the laser's origin rule doesn't jump while the panel moves.
            let (d, a) = (direction(self.yaw, self.pitch), self.anchor);
            self.free = Some([0, 1, 2].map(|i| a[i] + d[i] * self.beam_depth));
            return; // still on the Extra if pressed there (on_extra kept), since its card is the laser's origin
        }
        (self.on_extra, self.on_card) = (false, false);
        let focus = self.focus.filter(|_| self.gaze_lock && self.gaze_fresh() && self.buttons == 0);
        let d = direction(self.yaw, self.pitch);
        let o = self.anchor;
        let point = |t: f64| [o[0] + d[0] * t, o[1] + d[1] * t, o[2] + d[2] * t];
        // A button held on Plasma's bar keeps it (a slider or scrollbar dragged off its edge),
        // the same way a held button keeps a panel below: the ray on its plane, clamped to its
        // edges.
        if self.shell_buttons != 0 {
            if let Some(pl) = self.plasma
                && let Some((t, u, v)) = pl.hit(&o, &d)
                && let Some(at) = plasmabar::at(u.clamp(-pl.width / 2.0, pl.width / 2.0) / pl.width + 0.5, v.clamp(-pl.height / 2.0, pl.height / 2.0) / pl.height + 0.5)
            {
                (self.on_plasma, self.free, self.depth) = (true, Some(point(t)), t);
                self.to_plasma(at);
            }
            return; // hidden meanwhile, or you turned away from it: the pointer stays put
        }
        let mut hit = self.nearest(&o, &d);
        // A held button keeps the panel (dragging off its edge), and so does the gaze (the
        // panel you're looking at): the ray stays on its surface, clamped to its edges, and
        // stops there too, so coming back needs no extra motion.
        // Held onto another monitor of the same machine, or another window panel, it carries on
        // there (drag_to).
        let keep = if self.buttons != 0 {
            let to = |i: usize| (i, (&*panel(i).v, window(i)));
            Some(drag_to(to(self.active), hit.map(|h| to(h.0))))
        } else {
            focus
        };
        // The Extra (Machines, Preferences) is nearer than both bars, so it goes first, same way.
        if keep.is_none()
            && let Some(t) = self.extra_hit(&o, &d, hit)
        {
            return self.to_extra(point(t), t);
        }
        // A panel's card (its frame, bar, tab) in front of the panel behind, same way.
        if keep.is_none()
            && let Some((i, t)) = self.card_hit(&o, &d, hit)
        {
            return self.to_card(i, point(t), t);
        }
        // Plasma's bar sits in front of the taskbar's frame and inside it, so it goes first. The
        // cursor floats on it, except while a button holds a panel or the gaze keeps one
        // (looking at the bar lets go, see gaze()), and its point in the session gets sent when
        // it changes.
        if keep.is_none()
            && let Some(pl) = self.plasma
            && let Some((t, u, v)) = Self::bar_hit(Some(pl), 0.0, &o, &d, hit)
            && let Some(at) = plasmabar::at(u / pl.width + 0.5, v / pl.height + 0.5)
        {
            (self.on_plasma, self.free, self.depth) = (true, Some(point(t)), t);
            self.to_plasma(at);
            return;
        }
        // The taskbar's frame, in front of the panel it's on, same way.
        if keep.is_none()
            && let Some((t, u, v)) = Self::bar_hit(self.bar, self.bar_inset, &o, &d, hit)
        {
            (self.on_bar, self.free, self.depth, self.sent) = (Some((u, v)), Some(point(t)), t, None);
            windows::leave();
            return;
        }
        if let Some(k) = keep.filter(|&k| reachable(k) && hit.is_none_or(|h| h.0 != k)) {
            let pl = self.place[k];
            // Held on a window, the ray doesn't stop at its edge. The Desktop's windows are all
            // one KWin seat, so it lands on another window panel as is (drag_to), and further
            // off it's off every window (drag_off below).
            let held_window = self.buttons != 0 && window(k);
            match pl.hit(&o, &d) {
                Some((t, ru, rv)) if held_window => {
                    hit = held_on_window(&pl, ru, rv).map(|(u, v)| (k, t, u, v));
                }
                Some((t, ru, rv)) => {
                    let (u, v) = (ru.clamp(-pl.width / 2.0, pl.width / 2.0), rv.clamp(-pl.height / 2.0, pl.height / 2.0));
                    // the ray stops at the edge too, so there's no dead zone coming back. Held, it
                    // carries on toward its machine's other monitors (as far as their middles) so
                    // it can cross to one
                    let (mut lo, mut hi) = ([-pl.width / 2.0, -pl.height / 2.0], [pl.width / 2.0, pl.height / 2.0]);
                    for j in (0..self.place.len()).filter(|&j| self.buttons != 0 && j != k && reachable(j) && one_seat((&panel(k).v, false), (&panel(j).v, false))) {
                        let m = self.place[j].on_surface(0.0, 0.0, 0.0);
                        let c = [m[0][3] as f64 - o[0], m[1][3] as f64 - o[1], m[2][3] as f64 - o[2]];
                        if let Some((_, cu, cv)) = pl.hit(&o, &c.map(|x| x / norm(&c))) {
                            (lo, hi) = ([lo[0].min(cu), lo[1].min(cv)], [hi[0].max(cu), hi[1].max(cv)]);
                        }
                    }
                    self.aim_at(k, ru.clamp(lo[0], hi[0]), rv.clamp(lo[1], hi[1]));
                    hit = Some((k, t, u, v));
                }
                None if held_window => hit = None,
                None if keep == focus => {
                    // you turned right away from it (past its curve, or behind), so stay at the cursor
                    let (w, h) = panel(k).size();
                    let (u, v) = ((self.x / w as f64 - 0.5) * pl.width, (0.5 - self.y / h as f64) * pl.height);
                    self.aim_at(k, u, v);
                    hit = Some((k, 0.0, u, v));
                }
                None if self.buttons != 0 => {
                    // dragging and you turned away from it: the cursor stays where it was
                    let (w, h) = panel(k).size();
                    hit = Some((k, self.depth, (self.x / w as f64 - 0.5) * pl.width, (0.5 - self.y / h as f64) * pl.height));
                }
                None => {}
            }
        }
        // Off every panel: on the last one's surface just past its edge, otherwise floating at
        // its depth. It never jumps nearer or further.
        let free = if hit.is_none() {
            let pl = &self.place[self.active];
            let near = pl.hit(&o, &d).filter(|&(_, u, v)| {
                let (du, dv) = ((u.abs() - pl.width / 2.0).max(0.0), (v.abs() - pl.height / 2.0).max(0.0));
                du.hypot(dv) <= EDGE_REACH
            });
            Some(point(near.map_or(self.depth, |h| h.0)))
        } else {
            None
        };
        // Check from the eye too: once the head's moved, a nearer panel can cover the point.
        if self.buttons == 0 && keep.is_none() {
            let p = free.unwrap_or_else(|| point(hit.unwrap().1));
            let eye = vr::head_position();
            let to = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
            let n = norm(&to);
            if n > 1e-6 {
                let to = to.map(|x| x / n);
                let nearer = self.nearest(&eye, &to);
                if let Some(t) = self.extra_hit(&eye, &to, nearer).filter(|&t| t < n - 0.02) {
                    // re-aim the ray at it from the eye, so a press's drag lock holds at the point it hit
                    self.to_extra([0, 1, 2].map(|i| eye[i] + to[i] * t), t);
                    return self.reaim();
                }
                if let Some((j, t)) = self.card_hit(&eye, &to, nearer).filter(|h| h.1 < n - 0.02) {
                    self.to_card(j, [0, 1, 2].map(|i| eye[i] + to[i] * t), t);
                    return self.reaim();
                }
                if let Some(nearer) = nearer.filter(|h| h.1 < n - 0.02) {
                    hit = Some(nearer);
                }
            }
        }
        let Some((i, t, u, v)) = hit else {
            self.free = free;
            self.sent = None; // back on a panel, even at the same pixel, counts as a move
            if self.buttons != 0 && window(self.active) {
                self.window_drag = Some((self.cursor_point(), Instant::now()));
                windows::drag_off(); // held from a window, so now it's off every window
            } else {
                windows::leave();
            }
            return;
        };
        self.depth = t;
        if i != self.active {
            if keep != Some(i) {
                self.release_buttons(); // not when it's carried on (drag_to): the button stays down, it's one seat
            }
            self.active = i;
        }
        let (p, pl) = (panel(i), &self.place[i]);
        let (w, h) = p.size();
        self.free = None;
        self.x = ((u / pl.width + 0.5) * w as f64).clamp(0.0, w as f64 - 1.0);
        self.y = ((0.5 - v / pl.height) * h as f64).clamp(0.0, h as f64 - 1.0);
        if self.buttons != 0 && window(i) {
            self.window_drag = Some((self.cursor_point(), Instant::now()));
        }
        let at = (i, self.x as i32, self.y as i32);
        if self.sent != Some(at) {
            self.sent = Some(at);
            p.mouse(PTR_FLAGS_MOVE, self.x, self.y);
            self.moves += 1;
        }
    }

    /// Move the pointer onto Plasma's bar at a point in the session, sent when its pixel changes.
    fn to_plasma(&mut self, at: (f64, f64)) {
        self.plasma_at = at;
        let px = (usize::MAX, at.0 as i32, at.1 as i32);
        if self.sent != Some(px) {
            self.sent = Some(px);
            windows::shell_mouse(PTR_FLAGS_MOVE, Some(at));
            self.moves += 1;
        }
    }

    /// Where a panel is (its pose, width in metres, curve radius, 0 = flat). Its height comes
    /// from its picture's pixel aspect.
    pub fn set_pose(&mut self, i: usize, m: &crate::geometry::Mat, width: f64, curve: f64) {
        let (w, h) = panel(i).size();
        self.set_place(i, m, width, width * h as f64 / w as f64, curve);
    }

    /// Like set_pose, but with the height given. A window panel stretched by its bottom-right
    /// corner (grab.rs) keeps the dragged size until its window catches up (windows.rs).
    pub fn set_place(&mut self, i: usize, m: &crate::geometry::Mat, width: f64, height: f64, curve: f64) {
        let p = panel(i);
        let pl = Placement { vert: p.vert(), ..Placement::from_matrix(m, width, height / width, curve) };
        self.place[i] = pl;
        // SteamVR overlays only curve about their vertical axis, so a panel curved top to bottom
        // becomes the same panel turned a quarter turn, with its picture rotated (gpu.rs).
        let (o, (w, h)) = if pl.vert { (pl.turned(), (p.size().1, p.size().0)) } else { (pl, p.size()) };
        call!(ov, SetOverlayWidthInMeters, p.overlay, o.width as f32);
        // the picture stretched to that height in the meantime (1 when it's the pixels' own)
        call!(ov, SetOverlayTexelAspect, p.overlay, (o.width * h as f64 / (o.height * w as f64)) as f32);
        let bend = if curve > 0.0 { (o.width / (2.0 * std::f64::consts::PI * curve)).min(1.0) } else { 0.0 };
        call!(ov, SetOverlayCurvature, p.overlay, bend as f32);
        vr::place(p.overlay, &o.matrix());
        if i == self.active && self.free.is_none() && self.awake {
            self.reaim(); // the cursor rides along (gestures, presets, the control socket)
        }
    }

    /// Right Ctrl + mouse swings the panel around your head (yaw, then pitch about the
    /// horizontal axis across the line to it), keeping its distance and turning it with the swing.
    fn swing(&mut self, dx: f64, dy: f64) {
        if self.free.is_some() {
            return; // between panels there's nothing under the cursor to move
        }
        let i = self.active;
        let head = vr::head_position();
        let mut pl = self.place[i];
        let mut v = [0, 1, 2].map(|k| pl.c[k] - head[k]);
        let up = [0.0, 1.0, 0.0];
        let (yaw, pitch) = ((-dx * self.sensitivity).to_radians(), (-dy * self.sensitivity).to_radians());
        let mut across = crate::geometry::cross(&up, &v);
        let n = norm(&across);
        for a in [&mut v, &mut pl.x, &mut pl.y, &mut pl.z] {
            *a = rotate(a, &up, yaw);
        }
        if n > 1e-6 {
            across = rotate(&across.map(|a| a / n), &up, yaw);
            for a in [&mut v, &mut pl.x, &mut pl.y, &mut pl.z] {
                *a = rotate(a, &across, -pitch);
            }
        }
        pl.c = [0, 1, 2].map(|k| head[k] + v[k]);
        self.set_pose(i, &pl.matrix(), pl.width, pl.curve);
    }

    /// Right Ctrl + wheel pushes the panel away or pulls it closer along the line from your head; with Shift, it resizes.
    fn scale(&mut self, notches: i32, resize: bool) {
        if self.free.is_some() {
            return;
        }
        let i = self.active;
        let mut pl = self.place[i];
        let f = 1.06f64.powi(notches);
        if resize {
            // a curved panel can't wrap past half its circle, so its radius grows with it instead
            let width = (pl.width * f).clamp(0.15, 6.0);
            let curve = if pl.curve > 0.0 { pl.curve.max(width / std::f64::consts::PI) } else { 0.0 };
            return self.set_pose(i, &pl.matrix(), width, curve);
        }
        let head = vr::head_position();
        let v = [0, 1, 2].map(|k| pl.c[k] - head[k]);
        let d = norm(&v);
        let next = (d * f).clamp(0.3, 10.0);
        pl.c = [0, 1, 2].map(|k| head[k] + v[k] / (d + 1e-9) * next);
        self.set_pose(i, &pl.matrix(), pl.width, pl.curve);
    }

    /// A key or button. `ours` means its device is grabbed (otherwise the Frame gets it too).
    fn key(&mut self, code: u16, value: i32, ours: bool) {
        // Right Ctrl on its own toggles; used with other keys it's just Right Ctrl.
        let rctrl_down = self.rctrl.is_some();
        if code == KEY_ESC && value == 1 && rctrl_down {
            crate::close_desktop("Right Ctrl + Esc");
            return;
        }
        if code == KEY_RIGHTCTRL && value == 1 {
            (self.rctrl, self.rctrl_clean) = (Some(Instant::now()), true);
        } else if value == 1 {
            self.rctrl_clean = false;
        }
        if code == KEY_RIGHTCTRL && value == 0 {
            let tap = self.rctrl_clean && self.rctrl.is_some_and(|t| t.elapsed() < Duration::from_millis(400));
            self.rctrl = None;
            if tap {
                if self.engaged {
                    // the remote saw Right Ctrl go down, so let it up before letting go
                    self.send_key(code, 0);
                    self.keys.retain(|&k| k != code);
                }
                self.set_engaged(!self.engaged, "Right Ctrl tap");
                return;
            }
        }
        if code == KEY_LEFTSHIFT || code == KEY_RIGHTSHIFT {
            self.shift = value != 0;
        }
        if !ours {
            return; // the Frame's (a keyboard while released, or one not grabbed yet)
        }
        let button = (BTN_MOUSE..BTN_JOYSTICK).contains(&code);
        if button {
            if self.rctrl.is_some() {
                self.rctrl_clean = false;
            }
            if !self.awake {
                if value == 1 {
                    self.set_awake(true); // a click wakes it, and only wakes it
                }
                return;
            }
            self.last_used = Some(Instant::now());
        } else if !self.keyboards() {
            return; // a mouse's own keys go to the Frame while it has the typing
        }
        // Right Ctrl + Home sends every panel home; + F1..F4 recalls that preset; + Shift saves it.
        let spot_key = code == KEY_HOME || (KEY_F1..=KEY_F4).contains(&code);
        if rctrl_down && spot_key {
            if value == 1 {
                let which = if code == KEY_HOME { "home".to_string() } else { (code - KEY_F1 + 1).to_string() };
                spot(if self.shift && code != KEY_HOME { "save" } else { "apply" }, &which);
            }
            return; // and swallow their release
        }
        // Right Ctrl + Space puts the cursor straight ahead.
        if rctrl_down && code == KEY_SPACE {
            if value == 1 {
                self.recenter();
            }
            return;
        }
        // Right Ctrl + D opens the SteamVR dashboard through the laser's system button.
        if rctrl_down && code == KEY_D {
            if value == 1 && laser::HEALTHY.load(Relaxed) {
                laser::pulse(laser::SYSTEM);
            }
            return;
        }
        // Right Ctrl + G toggles gaze lock.
        if rctrl_down && code == KEY_G {
            if value == 1 {
                self.gaze_lock = !self.gaze_lock;
                eprintln!("gaze lock {}", if self.gaze_lock { "on: the pointer stays on the panel you look at" } else { "off" });
            }
            return;
        }
        if (BTN_MOUSE..BTN_JOYSTICK).contains(&code) {
            let b = match code {
                BTN_LEFT => PTR_FLAGS_BUTTON1,
                BTN_RIGHT => PTR_FLAGS_BUTTON2,
                BTN_MIDDLE => PTR_FLAGS_BUTTON3,
                _ => return,
            };
            if value == 2 {
                return;
            }
            // Off our panels the click goes out through the SteamVR laser (if it works), and the
            // button stays held there until you let go, wherever the cursor is by then.
            let bit = match b {
                PTR_FLAGS_BUTTON1 => laser::TRIGGER,
                PTR_FLAGS_BUTTON2 => laser::B,
                _ => laser::X,
            };
            if value == 0 && b == PTR_FLAGS_BUTTON1 && self.card_held {
                self.card_held = false;
                return self.card_mouse.push((self.active, false));
            }
            if value == 0 && b == PTR_FLAGS_BUTTON1 && self.extra_held {
                self.extra_held = false;
                return self.extra_mouse.push(false);
            }
            if value == 0 && self.beam_buttons & b != 0 {
                self.beam_buttons &= !b;
                return laser::press(bit, false);
            }
            if value == 0 && self.shell_buttons & b != 0 {
                self.shell_buttons &= !b;
                return windows::shell_mouse(b, None); // wherever it is now: on Plasma's bar, or dragged off it
            }
            if value == 1 && self.on_plasma {
                self.type_to_shell(); // a click moves typing there (Kickoff's search)
                self.set_engaged(true, "clicked Plasma's bar");
                self.shell_buttons |= b;
                self.clicks += 1;
                // send it at the cursor's point, since something else may have moved the session's pointer
                return windows::shell_mouse(b | PTR_FLAGS_DOWN, Some(self.plasma_at));
            }
            if value == 1 && self.on_bar.is_some() {
                self.leave_field(); // a click on our bar leaves any window's text field
                windows::shell_outside(); // it counts as a click outside Plasma's open popup
                if b == PTR_FLAGS_BUTTON1 {
                    self.bar_clicks += 1; // the taskbar picks it up (nothing's held for the release)
                    self.clicks += 1;
                }
                return;
            }
            if value == 1 && self.free.is_some() {
                if !self.on_card && !self.on_extra {
                    // off everything of ours: empty space, or SteamVR's UI through the laser
                    self.set_engaged(false, "clicked off the panels");
                }
                let (f, a) = (self.free.unwrap(), self.anchor);
                let depth = norm(&[f[0] - a[0], f[1] - a[1], f[2] - a[2]]);
                if laser::HEALTHY.load(Relaxed) {
                    (self.beam_depth, self.beam_buttons) = (depth, self.beam_buttons | b);
                    laser::press(bit, true);
                    self.clicks += 1;
                } else if b == PTR_FLAGS_BUTTON1 && self.extra_at().is_some() {
                    self.extra_held = true; // it's the window's, like the laser's click would be (ui.rs next_event)
                    self.extra_mouse.push(true);
                    self.clicks += 1;
                } else if b == PTR_FLAGS_BUTTON1 && (self.on_card || self.on_extra) {
                    // no laser (a real controller in the off hand), so our card's press goes
                    // straight to grab.rs, carried on the ray (R-1: our panels never need the laser)
                    (self.beam_depth, self.card_held) = (depth, true);
                    self.card_mouse.push((if self.on_extra { usize::MAX } else { self.active }, true));
                    self.clicks += 1;
                }
                return; // otherwise it lands nowhere
            }
            if value == 0 && self.buttons & b == 0 {
                return; // releasing a click that landed nowhere
            }
            if value == 1 {
                self.type_to(self.active); // a click moves typing to this panel
                if !self.engaged {
                    self.set_engaged(true, &format!("clicked {}", panel(self.active).v.name));
                }
            }
            if value != 0 { self.buttons |= b } else { self.buttons &= !b }
            panel(self.active).mouse(b | if value != 0 { PTR_FLAGS_DOWN } else { 0 }, self.x, self.y);
            if self.buttons == 0 {
                self.reaim(); // a drag let go between monitors: put the ray back on the cursor
            }
            self.clicks += (value == 1) as u32;
            return;
        }
        if value != 1 && self.field_held.contains(&code) {
            if value == 0 {
                self.field_held.retain(|&k| k != code);
            }
            if self.field.is_none() {
                return; // held in a field that's gone, so it's not a remote's
            }
        } else if value == 1 {
            self.field_held.retain(|&k| k != code); // its release went to the Frame
        }
        if self.field.is_some() {
            if value == 1 {
                self.field_held.push(code);
            }
            return self.field_keys.push((code, value, self.shift)); // it's for a window's text field, not a remote
        }
        match value {
            1 => self.keys.push(code),
            0 => self.keys.retain(|&k| k != code),
            _ => {}
        }
        self.send_key(code, value);
        self.keystrokes += (value == 1) as u32;
        if value != 0 {
            self.typed = Some(Instant::now());
        }
    }

    fn wheel(&mut self, notches: i32, horizontal: bool) {
        if self.rctrl.is_some() {
            self.rctrl_clean = false;
        }
        if !self.awake {
            return self.set_awake(true); // a turn of the wheel wakes it
        }
        self.last_used = Some(Instant::now());
        if self.rctrl.is_some() && !horizontal {
            // Right Ctrl + wheel: distance, or size with Shift
            self.rctrl_clean = false;
            return self.scale(notches, self.shift);
        }
        if self.on_plasma {
            self.wheels += 1;
            return windows::wheel(horizontal, notches as f64);
        }
        if self.card_held {
            if !horizontal {
                self.card_wheel += notches as f64; // what the mouse carries, for grab.rs poll
            }
            return;
        }
        if self.free.is_some() {
            if laser::HEALTHY.load(Relaxed) && !horizontal {
                laser::scroll(notches as f64); // off our panels it's SteamVR's scrolling
            }
            return;
        }
        panel(self.active).wheel(horizontal, notches as f64);
        self.wheels += 1;
    }

    /// Open new keyboards and mice (a Bluetooth one that slept comes back as a new node), and
    /// grab them when engaged. The input thread probes without the lock (input_loop).
    pub fn rescan(&mut self) {
        let (found, _) = probe(INPUT, &self.paths(), &mut HashSet::new());
        self.take_in(found);
    }

    fn paths(&self) -> Vec<String> {
        self.devices.iter().map(|d| d.path.clone()).collect()
    }

    /// Take in devices that `probe` found, ours from now on (keyboards grabbed when engaged).
    fn take_in(&mut self, found: Vec<Device>) {
        for mut d in found {
            if self.keyboards() && !d.mouse_only {
                d.grab_when_free();
            }
            eprintln!("input: {} ({})", d.name, d.path);
            self.devices.push(d);
        }
    }

    /// Hand the devices back to whoever had them (safe to call twice).
    pub fn return_devices(&mut self) {
        for d in self.devices.drain(..) {
            unsafe { libc::ioctl(d.file.as_raw_fd(), EVIOCGRAB as _, 0) };
        }
    }
}

/// Add motion counts to the wake tally (restarting it after WAKE_WINDOW). Enough to wake?
fn count_toward_waking(pending: &mut (f64, Option<Instant>), counts: f64, now: Instant) -> bool {
    if pending.1.is_none_or(|t| now - t > WAKE_WINDOW) {
        *pending = (0.0, Some(now));
    }
    pending.0 += counts;
    pending.0 >= WAKE_COUNTS
}

/// Presets: cc-home does the saving and recalling through our control socket. It runs on
/// its own thread because the input thread holds the KVM lock, which the socket needs.
fn spot(verb: &str, spot: &str) {
    let mut cmd = Command::new(root().join("cc-home"));
    cmd.args([verb, spot]).stdout(std::io::stderr());
    std::thread::spawn(move || cmd.status());
    eprintln!("spot: {verb} {spot}");
}

fn bit(bits: &[u8], n: u16) -> bool {
    bits[n as usize / 8] >> (n % 8) & 1 != 0
}

/// Real keyboards (letter keys) and mice (X/Y motion and a left button) only: no virtual
/// devices (ours or another app's relay), and no buttons, sensors or game pads.
/// Returns its name and whether it's a mouse without keys. A keyboard with a touchpad counts
/// as a keyboard, because its keys have to reach the Frame while the pointer sleeps.
fn our_kind(f: &File) -> Option<(String, bool)> {
    let fd = f.as_raw_fd();
    let mut name = [0u8; 256];
    let mut id = [0u16; 4]; // struct input_id: bustype, vendor, product, version
    let (mut keys, mut rels) = ([0u8; 0x300 / 8], [0u8; 2]);
    unsafe {
        libc::ioctl(fd, eviocgname(name.len() as u64) as _, name.as_mut_ptr());
        libc::ioctl(fd, EVIOCGID as _, id.as_mut_ptr());
        libc::ioctl(fd, eviocgbit(EV_KEY as u64, keys.len() as u64) as _, keys.as_mut_ptr());
        libc::ioctl(fd, eviocgbit(EV_REL as u64, rels.len() as u64) as _, rels.as_mut_ptr());
    }
    let name = String::from_utf8_lossy(name.split(|&b| b == 0).next().unwrap()).into_owned();
    if id[0] == BUS_VIRTUAL {
        return None;
    }
    let keyboard = bit(&keys, KEY_A) && bit(&keys, KEY_Z) && bit(&keys, KEY_SPACE);
    let mouse = bit(&rels, REL_X) && bit(&rels, REL_Y) && bit(&keys, BTN_LEFT);
    (keyboard || mouse).then_some((name, !keyboard))
}

const INPUT: &str = "/dev/input";

/// Open and check the input nodes in `dir` that aren't `known` (already ours) or `rejected`
/// (not ours, never opened again). Mice get grabbed and held for as long as we run. Also
/// returns whether a node wouldn't open or a mouse was held by something else, since those
/// are worth trying again. This runs without the KVM lock: closing a node that isn't ours
/// waits on the kernel (evdev_release, synchronize_rcu; ~100 ms for the Frame's), which held
/// up the cursor every 2 s.
fn probe(dir: &str, known: &[String], rejected: &mut HashSet<String>) -> (Vec<Device>, bool) {
    let (mut found, mut busy) = (Vec::new(), false);
    let Ok(list) = std::fs::read_dir(dir) else { return (found, busy) };
    for e in list.flatten() {
        let path = e.path().to_string_lossy().into_owned();
        if !e.file_name().to_string_lossy().starts_with("event") || known.contains(&path) || rejected.contains(&path) {
            continue;
        }
        // a node that won't open isn't rejected: a new one belongs to root until udev's chmod
        // or ACL, and that leaves the directory's mtime as it was, so it's worth trying again
        let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path) else {
            busy = true;
            continue;
        };
        let Some((name, mouse_only)) = our_kind(&file) else {
            rejected.insert(path);
            continue;
        };
        let mut d = Device { path, name, file, mouse_only, grabbed: false, pending: false };
        if mouse_only && !d.grab(true) {
            eprintln!("input: {} is held by something else ({})", d.name, std::io::Error::last_os_error());
            busy = true;
            continue;
        }
        found.push(d);
    }
    (found, busy)
}

pub fn input_loop() {
    let mut last_scan = Instant::now() - Duration::from_secs(10);
    // /dev/input as last scanned (its mtime changes when a node's added or removed), the nodes
    // we've found aren't ours since then, and whether to look again anyway (a mouse held
    // elsewhere, or a device gone while its node might still be there)
    let (mut listed, mut rejected, mut again) = (None, HashSet::new(), true);
    let mut ev = [libc::input_event { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: 0, code: 0, value: 0 }; 64];
    while !QUIT.load(Relaxed) {
        if last_scan.elapsed() > Duration::from_secs(2) {
            last_scan = Instant::now();
            let now = std::fs::metadata(INPUT).and_then(|m| m.modified()).ok();
            if now != listed || now.is_none() {
                rejected.clear(); // a node's path might belong to a new device now
                listed = now;
                again = true;
            }
            if again {
                let known = KVM.lock().unwrap().paths();
                let (found, busy) = probe(INPUT, &known, &mut rejected);
                again = busy;
                if !found.is_empty() {
                    KVM.lock().unwrap().take_in(found);
                }
            }
        }
        let mut fds: Vec<libc::pollfd> = {
            let k = KVM.lock().unwrap();
            k.devices.iter().map(|d| libc::pollfd { fd: d.file.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect()
        };
        // 500 ms: a timeout only runs tick (the 30 s idle sleep) and the rescan's 2 s check. A
        // key coming up (a keyboard waiting to be grabbed) is an event anyway (efficiency-plan.md 5)
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 500) };
        let mut k = KVM.lock().unwrap();
        k.tick();
        if ready <= 0 {
            continue;
        }
        let mut poke = false;
        for f in &fds {
            if f.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                // unplugged, or a Bluetooth device gone to sleep
                if let Some(i) = k.devices.iter().position(|d| d.file.as_raw_fd() == f.fd) {
                    k.release_held(); // anything it held would stick on the remote (and block the idle sleep)
                    eprintln!("input: {} gone", k.devices.remove(i).name);
                    again = true;
                }
                continue;
            }
            if f.revents & libc::POLLIN == 0 {
                continue;
            }
            let n = unsafe { libc::read(f.fd, ev.as_mut_ptr() as *mut _, size_of_val(&ev)) };
            if n <= 0 {
                continue;
            }
            let ours = k.devices.iter().any(|d| d.file.as_raw_fd() == f.fd && d.grabbed);
            for e in &ev[..n as usize / size_of::<libc::input_event>()] {
                poke |= e.type_ == EV_KEY || (e.type_ == EV_REL && matches!(e.code, REL_WHEEL | REL_HWHEEL));
                match (e.type_, e.code) {
                    (EV_KEY, _) => k.key(e.code, e.value, ours),
                    (EV_REL, _) if !ours => {} // a touchpad while its keyboard is the Frame's
                    (EV_REL, REL_X) => k.dx += e.value as f64,
                    (EV_REL, REL_Y) => k.dy += e.value as f64,
                    (EV_REL, REL_WHEEL) => k.wheel(e.value, false),
                    (EV_REL, REL_HWHEEL) => k.wheel(e.value, true),
                    (EV_SYN, _) if k.dx != 0.0 || k.dy != 0.0 => {
                        if k.rctrl.is_some() {
                            k.rctrl_clean = false; // a gesture, not the toggle tap
                        }
                        if k.rctrl.is_some() && k.engaged && k.awake {
                            let (dx, dy) = (k.dx, k.dy);
                            k.swing(dx, dy);
                            (k.dx, k.dy) = (0.0, 0.0);
                        } else {
                            k.motion();
                        }
                    }
                    _ => {}
                }
            }
        }
        // (an asleep mouse's motion doesn't count, or a jittery sensor would keep the loop at full rate)
        if poke || k.awake {
            POKED.store(true, Relaxed);
            vr::wake();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ioctl_numbers_match_linux() {
        assert_eq!(super::EVIOCGRAB, 0x40044590);
        assert_eq!(super::EVIOCGID, 0x80084502);
        assert_eq!(super::eviocgname(256), 0x81004506);
        assert_eq!(super::ioc(2, 0x18, 0x300 / 8), 0x80604518); // EVIOCGKEY(96)
        assert_eq!(super::scancode(103), 0x148);
        assert_eq!(super::scancode(30), 30);
    }

    #[test]
    fn a_held_drag_goes_on_to_another_monitor_of_its_machine() {
        use super::Viewer;
        let v = |host: &str, user: &str, machine: &str| Viewer { name: String::new(), user: user.into(), host: host.into(), port: 0, screen: 0, w: 1920, h: 1080, auto: false, machine: machine.into(), label: String::new(), pop: None, vnc: None };
        let (a, b) = (v("desk", "me", ""), v("desk", "me", ""));
        assert_eq!(super::drag_to((0, (&a, false)), Some((1, (&b, false)))), 1, "same host and user: on to it");
        assert_eq!(super::drag_to((0, (&a, false)), None), 0, "between panels: stays, clamped");
        assert_eq!(super::drag_to((0, (&a, false)), Some((1, (&v("laptop", "me", ""), false)))), 0, "another machine: stays");
        assert_eq!(super::drag_to((0, (&v("desk", "cc-frame", "box"), false)), Some((1, (&v("desk", "a", ""), false)))), 1, "a paired slot and the box's own: on to it");
        let (p, q) = (v("10.0.0.2", "me", "desk"), v("desk.lan", "me", "desk"));
        assert_eq!(super::drag_to((0, (&p, false)), Some((1, (&q, false)))), 1, "same machine=: on to it");
        assert_eq!(super::drag_to((0, (&p, false)), Some((1, (&v("10.0.0.2", "me", "other"), false)))), 0, "another machine=: stays");
        assert_eq!(super::drag_to((0, (&v("", "", ""), false)), Some((1, (&v("", "", ""), false)))), 0, "empty slots: stays");
        let pop = Viewer { pop: Some(("desk-wide".into(), "{u}".into())), ..p.clone() };
        assert_eq!(super::drag_to((0, (&p, false)), Some((1, (&pop, false)))), 0, "its own popped-out window: stays");
        assert_eq!(super::drag_to((0, (&pop, false)), Some((1, (&q, false)))), 0, "from a popped-out window: stays");
    }

    #[test]
    fn a_held_drag_goes_on_to_another_window_panel() {
        use super::Viewer;
        let win = |n: u32| Viewer { name: format!("win-{n}"), user: String::new(), host: String::new(), port: 0, screen: 0, w: 1280, h: 800, auto: false, machine: String::new(), label: String::new(), pop: None, vnc: None };
        let desk = Viewer { host: "desk".into(), ..win(0) };
        let (a, b) = (win(1), win(2));
        assert_eq!(super::drag_to((0, (&a, true)), Some((1, (&b, true)))), 1, "another window: on to it (a tab into another browser)");
        assert_eq!(super::drag_to((0, (&a, true)), None), 0, "off every panel: still its drag");
        assert_eq!(super::drag_to((0, (&a, true)), Some((1, (&desk, false)))), 0, "a remote monitor: stays");
        assert_eq!(super::drag_to((0, (&desk, false)), Some((1, (&a, true)))), 0, "from a remote monitor: stays");
    }

    #[test]
    fn a_window_drag_tears_off_past_the_edge() {
        use crate::geometry::{Placement, Pose, panel_matrix};
        let pl = Placement::from_matrix(&panel_matrix(&Pose { centre: [0.0, 1.0, -1.0], width: 1.0, ..Default::default() }), 1.0, 0.5, 0.0); // 0.5 high
        assert_eq!(super::held_on_window(&pl, 0.1, -0.2), Some((0.1, -0.2)), "on it");
        assert_eq!(super::held_on_window(&pl, 0.55, 0.1), Some((0.5, 0.1)), "just past its edge: clamped");
        assert_eq!(super::held_on_window(&pl, 0.2, -0.3), Some((0.2, -0.25)), "just below");
        assert_eq!(super::held_on_window(&pl, 0.6, 0.1), None, "further: off every window");
        assert_eq!(super::held_on_window(&pl, 0.56, 0.31), None, "past a corner");
    }

    #[test]
    fn waking_needs_deliberate_motion() {
        use super::*;
        let t0 = Instant::now();
        let mut p = (0.0, None);
        assert!(!count_toward_waking(&mut p, 15.0, t0)); // a knock
        assert!(!count_toward_waking(&mut p, 15.0, t0 + Duration::from_millis(1500))); // window restarted
        assert!(!count_toward_waking(&mut p, 20.0, t0 + Duration::from_millis(1700)));
        assert!(count_toward_waking(&mut p, 10.0, t0 + Duration::from_millis(1900))); // 45 within 1 s
    }

    #[test]
    fn a_nearer_panel_covers_the_bar() {
        use crate::geometry::{Placement, Pose, panel_matrix};
        let mut k = super::KVM.lock().unwrap();
        let pose = Pose { centre: [0.0, 1.0, -1.0], width: 0.5, ..Default::default() };
        k.bar = Some(Placement::from_matrix(&panel_matrix(&pose), 0.5, 0.1, 0.0));
        let (o, d) = ([0.0, 1.0, 0.0], [0.0, 0.0, -1.0]);
        let t = super::Kvm::bar_hit(k.bar, 0.0, &o, &d, None).expect("on the bar").0;
        assert!((t - 1.0).abs() < 1e-6, "{t}");
        assert!(super::Kvm::bar_hit(k.bar, 0.0, &o, &d, Some((3, 2.0, 0.0, 0.0))).is_some(), "a panel behind it");
        assert!(super::Kvm::bar_hit(k.bar, 0.0, &o, &d, Some((3, 0.6, 0.0, 0.0))).is_none(), "a panel in front of it");
        assert!(super::Kvm::bar_hit(k.bar, 0.0, &o, &[0.0, 0.5f64.sin(), -0.5f64.cos()], None).is_none(), "above it");
        // the frame's clear reach isn't the cursor's
        let edge = [0.24, 0.0, -1.0].map(|x: f64| x / (1.0f64 + 0.24 * 0.24).sqrt());
        assert!(super::Kvm::bar_hit(k.bar, 0.0, &o, &edge, None).is_some(), "near its edge");
        assert!(super::Kvm::bar_hit(k.bar, 0.02, &o, &edge, None).is_none(), "in its reach");
        k.bar = None;
    }

    #[test]
    fn the_extra_window_and_its_card_take_the_ray() {
        use crate::geometry::{Placement, Pose, panel_matrix};
        let mut k = super::KVM.lock().unwrap();
        let pose = Pose { centre: [0.0, 1.0, -0.6], width: 0.5, ..Default::default() };
        let pl = Placement::from_matrix(&panel_matrix(&pose), 0.5, 0.6, 0.0); // 0.3 high
        k.extra = Some((pl, [0.02, 0.05, 0.03])); // its card: 2 cm each side, 5 below, 3 above
        let o = [0.0, 1.0, 0.0];
        let at = |u: f64, v: f64| {
            let d = [u, v, -0.6];
            let n = super::norm(&d);
            d.map(|x| x / n)
        };
        let t = k.extra_hit(&o, &at(0.0, 0.0), None).expect("on it");
        assert!((t - 0.6).abs() < 1e-6, "{t}");
        assert!(k.extra_hit(&o, &at(0.26, 0.0), None).is_some() && k.extra_hit(&o, &at(0.28, 0.0), None).is_none(), "its card's side");
        assert!(k.extra_hit(&o, &at(0.0, -0.19), None).is_some() && k.extra_hit(&o, &at(0.0, -0.21), None).is_none(), "the bar below");
        assert!(k.extra_hit(&o, &at(0.0, 0.17), None).is_some() && k.extra_hit(&o, &at(0.0, 0.19), None).is_none(), "the tab above");
        assert!(k.extra_hit(&o, &at(0.0, 0.0), Some((3, 0.4, 0.0, 0.0))).is_none(), "a panel in front of it");
        // land(): over a panel behind it (none here), the cursor floats on it, as ours
        let (anchor, yaw, pitch, free, active, depth) = (k.anchor, k.yaw, k.pitch, k.free, k.active, k.depth);
        (k.anchor, k.yaw, k.pitch) = (o, 0.0, 0.0);
        k.land();
        assert!(k.on_extra && k.on_bar.is_none(), "on it");
        let (p, ours) = k.cursor();
        assert!(ours && (p[2] + 0.6).abs() < 1e-6 && (k.depth - 0.6).abs() < 1e-6, "{p:?}");
        assert_eq!(k.active, active, "the panel behind keeps the keyboard");
        // no laser: a press on its window is the window's (ui.rs next_event), at the ray's point
        assert!(k.extra_at().is_some_and(|(u, v)| u.abs() < 1e-6 && v.abs() < 1e-6));
        k.awake = true;
        k.key(super::BTN_LEFT, 1, true);
        k.key(super::BTN_LEFT, 0, true);
        assert_eq!(std::mem::take(&mut k.extra_mouse), vec![true, false]);
        assert!(k.card_mouse.is_empty(), "not its card's");
        k.awake = false;
        // a press held through the laser keeps it ours (its card is the laser's origin) until let go
        (k.beam_buttons, k.beam_depth) = (1, 0.6);
        k.land();
        assert!(k.on_extra && k.cursor().1, "held on it");
        k.beam_buttons = 0;
        // a bar nearer along the ray takes it; one behind doesn't
        let bar = |z: f64| Some(Placement::from_matrix(&panel_matrix(&Pose { centre: [0.0, 1.0, z], width: 0.5, ..Default::default() }), 0.5, 0.2, 0.0));
        k.bar = bar(-0.35);
        assert!(k.extra_hit(&o, &at(0.0, 0.0), None).is_none(), "a wrist bar raised in front");
        k.bar = bar(-1.0);
        assert!(k.extra_hit(&o, &at(0.0, 0.0), None).is_some(), "a bar behind it");
        k.bar = None;
        k.extra = None;
        (k.anchor, k.yaw, k.pitch, k.free, k.depth, k.on_extra) = (anchor, yaw, pitch, free, depth, false);
    }

    #[test]
    fn a_panels_card_in_front_of_another_takes_the_ray() {
        use crate::geometry::{Placement, Pose, panel_matrix};
        let mut k = super::KVM.lock().unwrap();
        let place = |z: f64, w: f64, aspect: f64| Placement::from_matrix(&panel_matrix(&Pose { centre: [0.0, 1.0, z], width: w, ..Default::default() }), w, aspect, 0.0);
        // a pop-out (0.4 by 0.2) in front of its monitor (1 by 0.6), both with 2 cm card sides
        let saved = (std::mem::take(&mut k.place), std::mem::take(&mut k.cards), k.anchor, k.yaw, k.pitch, k.free, k.active, k.depth);
        (k.place, k.cards) = (vec![place(-0.6, 0.4, 0.5), place(-1.0, 1.0, 0.6)], vec![[0.02, 0.05, 0.03]; 2]);
        let o = [0.0, 1.0, 0.0];
        let at = |u: f64| {
            let d = [u, 0.0, -0.6];
            let n = super::norm(&d);
            d.map(|x| x / n)
        };
        let behind = |d: &[f64; 3]| k.nearest(&o, d);
        assert_eq!(behind(&at(0.21)).map(|h| h.0), Some(1), "the monitor behind is under its frame");
        assert_eq!(k.card_hit(&o, &at(0.21), behind(&at(0.21))).map(|h| h.0), Some(0), "its frame");
        assert!(k.card_hit(&o, &at(0.1), behind(&at(0.1))).is_none(), "its picture is the panel's");
        assert!(k.card_hit(&o, &at(0.25), behind(&at(0.25))).is_none(), "past its card: the monitor's");
        // almost coplanar (2 mm in front, the card's own depth): the picture behind wins
        k.place[0] = place(-0.998, 0.4, 0.5);
        let d = super::norm(&[0.21, 0.0, -0.998]);
        let d = [0.21 / d, 0.0, -0.998 / d];
        assert!(k.card_hit(&o, &d, k.nearest(&o, &d)).is_none(), "a near tie is the picture's");
        k.place[0] = place(-0.6, 0.4, 0.5);
        // land(): the cursor floats on its frame, and it's the active panel (its card shows)
        (k.anchor, k.active) = (o, 1);
        (k.yaw, k.pitch) = super::angles(&at(0.21));
        k.land();
        let t = super::norm(&[0.21, 0.0, -0.6]);
        assert!(k.free.is_some() && k.active == 0 && (k.depth - t).abs() < 1e-6, "{:?} {} {}", k.free, k.active, k.depth);
        assert!(!k.cursor().1, "SteamVR's laser gives the card its events");
        // no laser (both controllers on): a left press there is grab's, carried on the ray
        let r = k.ray();
        assert!((0..3).all(|i| (-r[i][2] as f64 - at(0.21)[i]).abs() < 1e-5 && (r[i][3] as f64 - o[i]).abs() < 1e-5), "the ray's pose: -z along it");
        k.awake = true;
        k.key(super::BTN_LEFT, 1, true);
        assert_eq!(std::mem::take(&mut k.card_mouse), vec![(0, true)]);
        k.wheel(-2, false);
        assert_eq!(std::mem::take(&mut k.card_wheel), -2.0, "the wheel pushes what's carried");
        k.yaw += 5.0;
        k.land();
        let f = k.free.unwrap();
        assert!((super::norm(&[f[0] - o[0], f[1] - o[1], f[2] - o[2]]) - t).abs() < 1e-6, "held: the cursor keeps its distance");
        k.key(super::BTN_LEFT, 0, true);
        assert_eq!(std::mem::take(&mut k.card_mouse), vec![(0, false)]);
        k.awake = false;
        (k.place, k.cards, k.anchor, k.yaw, k.pitch, k.free, k.active, k.depth) = saved;
    }

    #[test]
    fn nodes_that_arent_ours_are_rejected_once() {
        let dir = std::env::temp_dir().join(format!("cc-kvm-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["event0", "event1", "mouse0"] {
            std::fs::write(dir.join(n), b"").unwrap(); // not input devices: their ioctls fail
        }
        let (d, known) = (dir.to_string_lossy().into_owned(), vec![dir.join("event1").to_string_lossy().into_owned()]);
        let mut rejected = std::collections::HashSet::new();
        let (found, busy) = super::probe(&d, &known, &mut rejected);
        assert!(found.is_empty() && !busy);
        assert_eq!(rejected.iter().cloned().collect::<Vec<_>>(), [dir.join("event0").to_string_lossy().into_owned()], "event1 is ours, mouse0 isn't an event node");
        // one that won't open (udev hasn't let us in yet) gets tried again, never rejected
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("event2")).unwrap();
        let (found, busy) = super::probe(&d, &known, &mut rejected);
        assert!(found.is_empty() && busy && rejected.len() == 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_window_field_takes_the_keyboards_until_typing_goes_to_a_remote() {
        let mut k = super::KVM.lock().unwrap();
        let (engaged, kbd) = (k.engaged, k.kbd);
        k.engaged = false;
        k.focus_field("machines", true);
        assert_eq!(k.field_keys("prefs"), None, "another window's: none");
        k.key(super::KEY_LEFTSHIFT, 1, true);
        k.key(super::KEY_A, 1, true);
        k.key(super::KEY_A, 2, true);
        k.key(super::KEY_A, 1, false); // a keyboard not grabbed yet: the Frame's
        assert_eq!(k.field_keys("machines"), Some(vec![(super::KEY_LEFTSHIFT, 1, true), (super::KEY_A, 1, true), (super::KEY_A, 2, true)]));
        assert!(k.keys.is_empty() && k.keystrokes == 0, "none to a remote");
        assert_eq!(k.field_keys("machines"), Some(vec![]), "taken");
        k.focus_field("prefs", false);
        assert!(k.field_keys("machines").is_some(), "only its owner lets it go");
        k.focus_field("machines", false);
        assert_eq!(k.field_keys("machines"), None);
        k.focus_field("machines", true);
        k.type_to(kbd);
        assert_eq!(k.field_keys("machines"), None, "typing sent to a remote ends it");
        k.focus_field("machines", true);
        k.type_to_shell();
        assert_eq!(k.field_keys("machines"), None, "or to the session");
        k.key(super::KEY_A, 1, true);
        k.focus_field("machines", false); // still held after Enter submits
        k.key(super::KEY_A, 2, true);
        k.key(super::KEY_A, 0, true);
        assert!(k.keys.is_empty() && k.keystrokes == 0, "a key held from the field: its repeats and release none of a remote's");
        k.key(super::KEY_LEFTSHIFT, 0, true);
        (k.engaged, k.kbd, k.kbd_shell) = (engaged, kbd, false);
    }

    #[test]
    fn click_to_type() {
        use super::*;
        let mut k = KVM.lock().unwrap();
        let saved = (k.engaged, k.kbd, k.kbd_shell, k.awake, k.free, k.on_plasma, k.on_card, k.on_extra, k.gaze_lock);
        // kbd_shell sends keys to the session (none in tests), since a panel's need a live panel
        (k.engaged, k.kbd_shell, k.awake) = (false, true, true);
        (k.on_card, k.on_extra, k.on_bar) = (false, false, None); // (other tests' land() can leave them)
        let click = |k: &mut Kvm| {
            k.key(BTN_LEFT, 1, true);
            k.key(BTN_LEFT, 0, true);
            (std::mem::take(&mut k.card_mouse), std::mem::take(&mut k.extra_mouse))
        };
        // a key held on the Frame's keyboard while clicking: its release stays the Frame's
        k.key(KEY_A, 1, false);
        k.on_plasma = true;
        click(&mut k);
        assert!(k.engaged && k.kbd_shell, "a click on Plasma's bar engages, typing there");
        k.key(KEY_A, 0, false);
        assert!(k.keys.is_empty(), "nothing held on the remote side");
        // engaged, a key held, then a click off our panels: let go, then the Frame's
        k.key(KEY_A, 1, true);
        assert_eq!(k.keys, [KEY_A]);
        (k.on_plasma, k.free) = (false, Some([0.0, 1.0, -1.5]));
        click(&mut k);
        assert!(!k.engaged && k.keys.is_empty(), "a click on nothing gives it back, the held key let go");
        k.key(KEY_A, 0, false); // its release, now the Frame's
        assert!(k.keys.is_empty());
        // a click on a card or the Machines/Preferences window is ours, so nothing changes
        k.engaged = true;
        k.on_card = true;
        click(&mut k);
        (k.on_card, k.on_extra) = (false, true);
        click(&mut k);
        k.on_extra = false;
        assert!(k.engaged, "a card's or the Extra's click keeps it");
        // the dashboard (main.rs) gives it back the same way
        k.key(KEY_A, 1, true);
        k.set_engaged(false, "dashboard opened");
        assert!(!k.engaged && k.keys.is_empty());
        k.key(KEY_A, 0, false);
        // a Right Ctrl tap still toggles both ways: in from the Frame (its keyboard not grabbed)...
        k.key(KEY_RIGHTCTRL, 1, false);
        k.key(KEY_RIGHTCTRL, 0, false);
        assert!(k.engaged, "tap: to the remotes");
        // ...and out, with the remote's Right Ctrl let up
        k.key(KEY_RIGHTCTRL, 1, true);
        assert_eq!(k.keys, [KEY_RIGHTCTRL]);
        k.key(KEY_RIGHTCTRL, 0, true);
        assert!(!k.engaged && k.keys.is_empty(), "tap: to the Frame, nothing stuck");
        // a chord isn't a tap
        k.engaged = true;
        k.key(KEY_RIGHTCTRL, 1, true);
        k.key(KEY_G, 1, true);
        k.key(KEY_G, 0, true);
        k.key(KEY_RIGHTCTRL, 0, true);
        assert!(k.engaged && k.keys.is_empty(), "Right Ctrl + G: still engaged");
        (k.engaged, k.kbd, k.kbd_shell, k.awake, k.free, k.on_plasma, k.on_card, k.on_extra, k.gaze_lock) = saved;
    }

    #[test]
    fn input_event_is_24_bytes() {
        assert_eq!(size_of::<libc::input_event>(), 24);
    }
}

/// A panel as a drag sees it: its viewer, and whether it shows a Frame window.
type Seat<'a> = (&'a Viewer, bool);

/// The panel a held button stays on. If the ray meets another monitor of the same machine
/// (machine= when both have it, otherwise the host), a window dragged between them carries
/// on: their krdp sessions inject into one KWin seat, so the button stays down and its moves
/// go to the new one at its own pixels. Same from one Frame window panel to another (a
/// browser tab into another window). Otherwise (another machine, between panels, off both)
/// it's the one it's held on.
fn drag_to(held: (usize, Seat), hit: Option<(usize, Seat)>) -> usize {
    hit.filter(|&(_, s)| one_seat(held.1, s)).map_or(held.0, |h| h.0)
}

/// Whether two panels share one seat: the Frame's windows (all in the Desktop's KWin), or two
/// remote monitors of one machine, by machine= when both have it, otherwise by host. The user
/// doesn't count since it's only the RDP login, so a paired slot's cc-frame and the box's own
/// are one seat. A popped-out window isn't (popout.rs), because its pixels are the window's,
/// not a monitor's.
fn one_seat((a, a_win): Seat, (b, b_win): Seat) -> bool {
    if a_win || b_win {
        return a_win && b_win;
    }
    !a.host.is_empty() && a.pop.is_none() && b.pop.is_none() && if !a.machine.is_empty() && !b.machine.is_empty() { a.machine == b.machine } else { a.host == b.host }
}

/// Whether panel i shows a Frame window (windows.rs). Never in tests.
fn window(i: usize) -> bool {
    crate::panels().get(i).is_some_and(|p| matches!(p.src, crate::Source::Window(_)))
}

/// A drag held on a window panel, with the ray at (u, v) on its plane: where the pointer
/// stays on it, clamped to its edges, or None past TEAR_OFF (off every window).
fn held_on_window(pl: &Placement, u: f64, v: f64) -> Option<(f64, f64)> {
    let (cu, cv) = (u.clamp(-pl.width / 2.0, pl.width / 2.0), v.clamp(-pl.height / 2.0, pl.height / 2.0));
    ((u - cu).hypot(v - cv) <= TEAR_OFF).then_some((cu, cv))
}

/// Whether the mouse can land on panel i: it's shown (an empty window slot is nowhere) and
/// not away (minimized, or hidden for another panel's theater mode). A place with no panel
/// (tests) counts as reachable.
fn reachable(i: usize) -> bool {
    crate::panels().get(i).is_none_or(|p| p.live() && !p.away())
}
