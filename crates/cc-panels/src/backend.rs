//! The boundary between Command Center and a VR runtime (docs/openxr-plan.md, section 3).
//!
//! This is the design, not the move: nothing uses it yet, and OpenVR is still called the way it
//! always was. Phase 0 moves today's OpenVR code behind these traits a step at a time, with the
//! golden tests (docs/openxr-plan.md, phase 0) replaying the same writes before and after each step.
//! OpenXR implements them later.
//!
//! What stays out of here on purpose: placement maths (`overlay::Place`, `At`, `apply`), geometry,
//! the KVM, sessions to computers and the layout. None of them know which runtime is underneath.
use std::os::fd::BorrowedFd;

use crate::geometry::{Mat, V3};

/// One surface: a panel, card, bar, back, cursor or window of ours. On OpenVR it's the overlay
/// handle, so phase 0 can swap `vr::Handle` for it without changing a single write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hand {
    Left,
    Right,
}

/// A controller, as the backend knows it. On OpenVR it's the tracked-device index; on OpenXR it's
/// one per hand path. It stays opaque because today's code tells controllers apart by index, not
/// by hand: a laser event carries an index, a drag keeps the index it started with, docking takes
/// "the other controllers" as a set, and a controller can have no hand role at all (Invalid or
/// OptOut) or share one. `Stage::hand` says which hand it is right now, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ctl(pub u32);

/// What can point, be tracked or hold a surface. The room isn't one: a surface in the room is
/// `At::Room` (overlay.rs), and the taskbar's u32::MAX for it becomes `Option<Dev>` = None.
/// Trackers, base stations and our own virtual pointer (cc-pointer) aren't `Dev`s either, because
/// nothing uses them as one: `vr::is_real_controller` filters them out today, and the virtual
/// pointer stays inside the OpenVR backend (laser.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dev {
    Head,
    Controller(Ctl),
    /// The mouse's ray (grab::MOUSE today): a pointer with a pose but no device behind it.
    Mouse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerKind {
    Enter,
    Move,
    Down(Button),
    Up(Button),
    /// `smooth` is SteamVR's ScrollSmooth against ScrollDiscrete. The panels take both, grab.rs's
    /// cards only discrete, so the difference is kept.
    Scroll {
        dx: f32,
        dy: f32,
        smooth: bool,
    },
    Leave,
}

/// A pointer on a surface. x and y are in the surface's pointer units (what `Overlays::mouse_scale`
/// set: pixels for a panel, metres for a card), from its bottom left, as SteamVR's mouse events are
/// now. So `geometry::surface_point` and `from_top` stay in the core.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub kind: PointerKind,
    pub x: f32,
    pub y: f32,
    pub from: Dev,
    /// How old the event is, in seconds (VREvent_t's eventAgeSeconds; 0 where a backend doesn't know).
    pub age: f32,
}

/// The keyboard's events. `user` is the value the keyboard was opened with (osk.rs uses it for
/// which panel is typing).
#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardEvent {
    /// What was typed since the last one, as UTF-8 (up to 8 bytes on SteamVR).
    Char {
        user: u64,
        text: String,
    },
    Done {
        user: u64,
    },
    Closed {
        user: u64,
    },
}

/// What one surface's queue gives (the ten `PollNextOverlayEvent` loops).
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceEvent {
    Pointer(PointerEvent),
    Keyboard(KeyboardEvent),
    /// SteamVR's close button on the dashboard tab's bar (control/dashtab.rs).
    Closed,
}

/// What the runtime says about our session. On OpenVR an overlay app is always running, visible,
/// focused and rendering, so this only changes on OpenXR. It isn't whether the headset is worn:
/// that's `SessionEvent::Worn`, and the two are never folded together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionState {
    pub running: bool,
    pub visible: bool,
    pub focused: bool,
    pub should_render: bool,
}

/// What the runtime's own queue gives (main.rs's `PollNextEvent` loop).
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    /// The runtime wants us gone (VREvent_Quit, or OpenXR's EXITING or loss pending).
    Quit,
    State(SessionState),
    /// The headset's proximity sensor, or `XR_EXT_user_presence`.
    Worn(bool),
    /// The tracking origin moved inside the same room (StandingZeroPoseReset, same universe).
    Relocalized,
    /// A different room: the new universe id. OpenVR compares universes itself and sends one of
    /// these two, as main.rs does now.
    RoomChanged(u64),
    DashboardShown(bool),
    /// A real controller's button, anywhere (VREvent_ButtonPress): it's the primary device now.
    ControllerUsed(Ctl),
}

/// What a surface asks of the runtime. Each is a SteamVR overlay flag today. OpenXR does without
/// most of them (we draw our own cursor and get every scroll).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    /// MakeOverlaysInteractiveIfVisible: the laser works it whenever it's visible.
    Interactive,
    /// SendVRDiscreteScrollEvents.
    DiscreteScroll,
    /// IgnoreTextureAlpha: alpha comes from `Surfaces::alpha`, not the picture.
    IgnoreAlpha,
    /// EnableControlBar and EnableControlBarClose: SteamVR's bar under the dashboard tab.
    ControlBar,
    ControlBarClose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    None,
    /// The runtime's laser sends this surface pointer events.
    Pointer,
}

/// Creating, showing and ordering surfaces (surface.rs's create and destroy, and the show, hide,
/// sort, alpha, colour, flag and input-method calls everywhere).
pub trait Surfaces {
    fn create(&mut self, key: &str, name: &str) -> Result<SurfaceId, String>;
    /// The dashboard tab and its thumbnail. `None` where there's no dashboard (OpenXR).
    fn create_dashboard(&mut self, key: &str, name: &str) -> Option<Result<(SurfaceId, SurfaceId), String>>;
    fn destroy(&mut self, s: SurfaceId);
    fn show(&mut self, s: SurfaceId);
    fn hide(&mut self, s: SurfaceId);
    fn visible(&self, s: SurfaceId) -> bool;
    fn order(&mut self, s: SurfaceId, order: u32);
    fn order_of(&self, s: SurfaceId) -> u32;
    fn alpha(&mut self, s: SurfaceId, a: f32);
    fn color(&mut self, s: SurfaceId, rgb: [f32; 3]);
    fn flag(&mut self, s: SurfaceId, f: Flag, on: bool);
    fn input(&mut self, s: SurfaceId, m: InputMode);
    fn rename(&mut self, s: SurfaceId, name: &str);
    /// A picture from a file (icons).
    fn picture_file(&mut self, s: SurfaceId, path: &str);
}

/// The writes behind a placement: today's `overlay::Overlays`, with `SurfaceId` and `Dev`. It keeps
/// `mouse_scale` for now, since SteamVR's mouse coordinates depend on it and phase 0 doesn't change
/// them. OpenXR's ray casting scales its hits to the same units.
pub trait Overlays {
    fn width(&mut self, s: SurfaceId, metres: f32);
    fn curvature(&mut self, s: SurfaceId, fraction: f32);
    fn texel_aspect(&mut self, s: SurfaceId, aspect: f32);
    fn bounds(&mut self, s: SurfaceId, b: [f32; 4]);
    fn mouse_scale(&mut self, s: SurfaceId, scale: [f32; 2]);
    fn place_room(&mut self, s: SurfaceId, m: &Mat);
    fn place_device(&mut self, s: SurfaceId, dev: Dev, m: &Mat);
}

/// A dmabuf to show: one plane, as gpu.rs and capture.rs import them now.
pub struct Dmabuf<'a> {
    pub fd: BorrowedFd<'a>,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
}

/// An imported picture: the IPC resource handle on OpenVR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TexHandle(pub u64);

/// Pictures (gpu.rs, capture.rs). The GBM buffers and their stale tracking stay in the core.
///
/// Who owns what, as OpenVR works today (docs/openxr-plan.md, 2.3, has the details):
///
/// - **`import` doesn't take the fd.** The runtime takes its own reference, so the caller can close
///   its fd straight after (gpu.rs does; capture.rs drops its dup with the frame). The picture
///   stays alive until `release`.
/// - **The core may write a buffer again once a tick has passed since `present` replaced it** on
///   screen. Nothing from the runtime says it's done reading: the core assumes it has finished by
///   its next frame. gpu.rs double-buffers and holds the replaced one for a tick (`held`); capture.rs
///   holds the shown buffer and the one before it back from PipeWire.
/// - **The producer says when it's ready, before `present`.** For a PipeWire dmabuf the core polls
///   the fd for its implicit fence and doesn't present until it's readable. For our own GBM
///   buffers the copy is done on the CPU, mapped and unmapped, so it's ready when the copy returns.
/// - **`release` returns at once, with no signal when the runtime has let go.** The core frees the
///   GBM buffer right after it. Only a UI overlay's resize keeps the old pair a tick longer
///   (gpu.rs `raw_tick`); a panel's resize frees both at once, the shown one included.
/// - **One handle can be on two surfaces:** a panel's back shows the panel's handle (back.rs).
pub trait Textures {
    fn import(&mut self, buf: &Dmabuf) -> Result<TexHandle, String>;
    fn present(&mut self, s: SurfaceId, t: TexHandle);
    fn release(&mut self, t: TexHandle);
    /// CPU pixels, RGBA (SetOverlayRaw today). False if the runtime wouldn't take them.
    fn present_pixels(&mut self, s: SurfaceId, px: &[u8], w: usize, h: usize) -> bool;
    fn clear(&mut self, s: SurfaceId);
}

/// Pointer events for a surface. There are two contracts, and only the first is in force now:
///
/// **Phase 0, the OpenVR adapter: today's events and the core's recovery, as they are.** SteamVR
/// does the hit testing, and the adapter only translates each VREvent_t into a `SurfaceEvent`, one
/// for one, in order, with the same coordinates. It never makes up an event: no Up that SteamVR
/// didn't send, no Leave, no clamping. Where SteamVR sends no Up (a controller switched off), the
/// core's own recovery stays as it is (grab.rs ends the drag after `LOST`), and the golden tests
/// pin it. Any change in behaviour is its own branch, not part of the move.
///
/// **Phase 1, the target both backends meet:**
///
/// - **Capture:** a Down on a surface captures that pointer (`from`) for that button. Its moves keep
///   going to that surface, clamped to its edge, until the Up, even once the ray has left it. That's
///   how a drag goes past a panel's edge.
/// - **Hover:** a surface gets Enter, then moves while it's hit and nothing has captured that
///   pointer. When the hit goes to another surface, the old one gets Leave first.
/// - **Scroll** goes to the hovered surface, or the captured one during a capture.
/// - **Endings:** focus lost with a button held, the device lost mid-drag, and a surface destroyed
///   while it holds a capture all end the capture. The holder, if it still exists, gets Up and then
///   Leave, so nothing stays held.
///
/// OpenXR's ray casting has to meet these itself. Whether OpenVR's adapter grows them too, and how,
/// is decided after the golden tests show where SteamVR's events differ from them.
pub trait Pointing {
    /// The next event for surface s, if there is one (one `PollNextOverlayEvent` today).
    fn next(&mut self, s: SurfaceId) -> Option<SurfaceEvent>;
}

/// Where things are, besides where to write (today's `stage::Stage` reads).
pub trait Stage: Overlays {
    /// A device's pose in standing space. None when it isn't tracked.
    fn device(&self, d: Dev) -> Option<Mat>;
    /// A device's laser, pointing along -z: the controller's tip on OpenVR, the aim pose on OpenXR.
    fn laser(&self, d: Dev) -> Option<Mat>;
    fn head(&self) -> V3;
    fn head_pose(&self) -> Option<Mat>;
    /// The real controllers that are switched on, tracked or not.
    fn controllers(&self) -> Vec<Ctl>;
    /// Which hand a controller is right now, if it has one.
    fn hand(&self, c: Ctl) -> Option<Hand>;
    /// The controller in a hand right now, if there is one (the wrist taskbar asks each frame).
    fn in_hand(&self, h: Hand) -> Option<Ctl>;
    fn connected(&self, d: Dev) -> bool;
    /// A short buzz in the hand whose laser last worked surface s (a dock target arming).
    fn pulse(&mut self, s: SurfaceId);
}

/// The session and its clock (main.rs's event loop, vr.rs's prediction and pacing).
pub trait Session {
    fn next_event(&mut self) -> Option<SessionEvent>;
    /// Seconds from now until the picture reaches the eyes (vr::to_photons today).
    fn to_photons(&self) -> f32;
    fn display_period(&self) -> Option<f32>;
    /// OpenVR's universe. None where the runtime has no such thing; the room rule then applies
    /// (docs/openxr-plan.md, phase 1).
    fn universe_id(&self) -> Option<u64>;
    /// Whether a game is the scene app. None when the backend can't tell, which isn't "no game".
    fn scene_app_running(&self) -> Option<bool>;
    /// No-ops on OpenVR. On OpenXR, wait, begin and end are called every frame while the session
    /// runs, and `should_render` only skips the drawing.
    fn begin_frame(&mut self) -> SessionState;
    fn end_frame(&mut self, layers: &[SurfaceId]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardMode {
    Normal,
    Password,
    /// The key strip's minimal mode (osk.rs).
    Minimal,
}

/// The runtime's keyboard (SteamVR's, osk.rs, workspace.rs, machines.rs). Its events come in
/// through `Pointing::next` and `Session` as SteamVR sends them now.
pub trait Keyboard {
    /// False when there's no runtime keyboard (OpenXR), and the core opens its own.
    fn open(&mut self, s: SurfaceId, mode: KeyboardMode, multiline: bool, hint: &str, initial: &str, user: u64) -> bool;
    fn text(&self) -> String;
    fn close(&mut self);
}

/// What only some runtimes have.
pub trait Shell {
    fn has_dashboard(&self) -> bool;
    fn dashboard_visible(&self) -> bool;
    /// Whether our tab is the one showing.
    fn our_tab_active(&self, main: SurfaceId) -> bool;
    /// SteamVR's toast, the fallback when D-Bus notifications aren't there. False if it wasn't shown.
    fn toast(&mut self, text: &str) -> bool;
}

/// Controller input that isn't pointing.
pub trait Input {
    /// Where the eyes look, as an origin and a direction, if the runtime has eye tracking.
    fn gaze(&self) -> Option<(V3, V3)>;
}

/// One runtime, all of it.
pub trait Backend: Surfaces + Stage + Textures + Pointing + Session + Keyboard + Shell + Input {}
