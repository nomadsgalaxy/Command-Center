//! The boundary between Command Center and a VR runtime (docs/openxr-plan.md, section 3).
//!
//! This is the design, not the move: nothing uses it yet, and OpenVR is still called the way it
//! always was. Phase 0 moves today's OpenVR code behind these traits a step at a time, with the
//! golden tests (docs/openxr-plan.md, phase 0) replaying the same writes before and after each step.
//! OpenXR implements them later.
//!
//! What stays out of here on purpose: placement maths (`overlay::Place`, `At`, `apply`), geometry,
//! the KVM, sessions to computers and the layout. None of them know which runtime is underneath.
use crate::geometry::{Mat, V3};

#[allow(unused_imports)] // and gaze.rs
pub use crate::gaze::Input;
pub use crate::ids::{Ctl, Dev, Hand, SurfaceId};
#[allow(unused_imports)] // the design names them; the core takes them from keyboard.rs
pub use crate::keyboard::{Keyboard, KeyboardMode};
#[allow(unused_imports)] // and shell.rs
pub use crate::shell::Shell;
#[allow(unused_imports)] // and texture.rs
pub use crate::texture::{Dmabuf, Refused, TexHandle, Textures};

#[allow(unused_imports)] // the design names them; the core takes them from events.rs
pub use crate::events::{Button, KeyboardEvent, PointerEvent, PointerKind, SessionEvent, SessionState, SurfaceEvent};

/// One frame, as `begin_frame` starts it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// False when the runtime says not to draw (OpenXR's shouldRender). The frame still ends.
    pub should_render: bool,
    /// Seconds from now until this frame's picture reaches the eyes: when poses are read for
    /// (vr::to_photons on OpenVR, predictedDisplayTime on OpenXR).
    pub predicted: f32,
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
    /// A device's pose in standing space, at the current frame's predicted time (`Frame::predicted`;
    /// vr::to_photons on OpenVR today). None when it isn't tracked.
    fn device(&self, d: Dev) -> Option<Mat>;
    /// A device's laser, pointing along -z, at the same time: the controller's tip on OpenVR, the
    /// aim pose on OpenXR.
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
    /// A short buzz in the hand whose laser last worked surface s (a dock target arming). Its one
    /// caller today uses one strength and length, so they aren't parameters (stage.rs).
    fn pulse(&mut self, s: SurfaceId);
}

/// The session and its clock (main.rs's event loop, vr.rs's prediction and pacing).
pub trait Session {
    fn next_event(&mut self) -> Option<SessionEvent>;
    fn display_period(&self) -> Option<f32>;
    /// OpenVR's universe. None where the runtime has no such thing; the room rule then applies
    /// (docs/openxr-plan.md, phase 1).
    fn universe_id(&self) -> Option<u64>;
    /// Whether a game is the scene app. None when the backend can't tell, which isn't "no game".
    fn scene_app_running(&self) -> Option<bool>;
    /// OpenVR: no wait, should_render always, and predicted is vr::to_photons. On OpenXR, wait,
    /// begin and end are called every frame while the session runs, and `should_render` only skips
    /// the drawing.
    fn begin_frame(&mut self) -> Frame;
    fn end_frame(&mut self, layers: &[SurfaceId]);
}

/// One runtime, all of it.
pub trait Backend: Surfaces + Stage + Textures + Pointing + Session + Keyboard + Shell + Input {}
