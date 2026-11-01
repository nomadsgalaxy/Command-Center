//! The runtime's two event queues: each surface's (`Pointing`) and its own (`SessionEvents`), the
//! boundary for them (docs/openxr-plan.md, phase 0 step 5) and `Live` and `Queue`, their OpenVR
//! side, which polls SteamVR and translates each event (vr::surface_event, vr::session_event). Built into
//! cc-win-spike too, so it uses only vr.rs, ids.rs and events.rs.
use crate::events::{SessionEvent, SurfaceEvent};
use crate::ids::SurfaceId;
use crate::{call, vr};

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

/// The runtime's own queue. None is an empty queue; an event nothing acts on is
/// `SessionEvent::Other`, so a caller can tell the two apart.
pub trait SessionEvents {
    fn next_event(&mut self) -> Option<SessionEvent>;
}

/// What the runtime says about the session when asked (not its events). Each is None when the
/// runtime can't tell; the callers' fallbacks are theirs (main.rs, taskbar.rs).
pub trait Session {
    /// The display's refresh rate, in Hz (not a period: Pace takes Hz, and a round trip through
    /// 1/x isn't always the same float). None when the runtime can't tell.
    fn display_hz(&self) -> Option<f32>;
    /// OpenVR's universe: the room. None where the runtime can't tell, or has no such thing (the
    /// room rule then applies, docs/openxr-plan.md phase 1).
    fn universe_id(&self) -> Option<u64>;
    /// Whether a game is the scene app. None when the runtime can't tell, which isn't "no game".
    fn scene_app_running(&self) -> Option<bool>;
    /// Whether the headset is worn now, from its proximity sensor: the start before any
    /// `SessionEvent::Worn`. None when it can't tell.
    fn worn(&self) -> Option<bool>;
}

/// SteamVR's overlay queues and session, for real.
pub struct Live;

/// SteamVR's own queue, for real. `universe` is the one the session's in, which a relocalization
/// is compared with (vr::session_event); it follows each RoomChanged it hands out.
pub struct Queue {
    pub universe: u64,
}

impl Queue {
    pub fn new(universe: u64) -> Queue {
        Queue { universe }
    }
}

impl Pointing for Live {
    fn next(&mut self, s: SurfaceId) -> Option<SurfaceEvent> {
        let mut e: vr::VREvent_t = Default::default();
        call!(ov, PollNextOverlayEvent, s.0, &mut e, size_of::<vr::VREvent_t>() as u32).then(|| vr::surface_event(&e))
    }
}

/// The rate and the universe are None when OpenVR sets the property's error, whatever value came
/// with it (`vr::prop`): that value is only documented for a bad index or type, 0 (openvr.h,
/// IVRSystem_022). The callers' fallbacks then apply, 0.0 (Pace's 90 Hz) and 0 ("no room yet").
/// The properties are spelled `openvr_sys::` in full: this file has no `sys` alias outside its
/// tests (in `call!`, `sys` is the table's name), and the two uses don't earn one.
impl Session for Live {
    fn display_hz(&self) -> Option<f32> {
        let mut err = 0;
        vr::prop(call!(sys, GetFloatTrackedDeviceProperty, 0, openvr_sys::ETrackedDeviceProperty_Prop_DisplayFrequency_Float, &mut err), err)
    }

    /// It logs what SteamVR said, error and all, before any fallback: main.rs asks once, at
    /// start, and the Frame makes a new universe each time it tracks again without finding its
    /// saved map, so the log is the only record of which one a run had.
    fn universe_id(&self) -> Option<u64> {
        let mut err = 0;
        let u = call!(sys, GetUint64TrackedDeviceProperty, 0, openvr_sys::ETrackedDeviceProperty_Prop_CurrentUniverseId_Uint64, &mut err);
        eprintln!("room: SteamVR's universe {u} (property error {err})");
        vr::prop(u, err)
    }

    /// GetCurrentSceneProcessId has no error to read, so OpenVR always tells.
    fn scene_app_running(&self) -> Option<bool> {
        Some(call!(apps, GetCurrentSceneProcessId) != 0)
    }

    fn worn(&self) -> Option<bool> {
        vr::head_worn()
    }
}

impl SessionEvents for Queue {
    fn next_event(&mut self) -> Option<SessionEvent> {
        let mut e: vr::VREvent_t = Default::default();
        if !call!(sys, PollNextEvent, &mut e, size_of::<vr::VREvent_t>() as u32) {
            return None;
        }
        let ev = vr::session_event(&e, self.universe);
        if let SessionEvent::RoomChanged(now) = ev {
            self.universe = now;
        }
        Some(ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openvr_sys as sys;

    /// The session's reads through Live against vr::fake: what SteamVR returns, None for the rate
    /// and the universe when it sets their error, and the scene app and worn state. The error
    /// fixtures are synthetic: what SteamVR returns with its error set is only documented for a
    /// bad index or type (0, openvr.h).
    #[test]
    fn the_session_reads_are_what_steamvr_returns_or_none_on_its_error() {
        let ok =
            vr::fake::Script { vsync: Some((0.0, 90.0, 0.011)), universe: 7, scene_pid: 4242, activity: Some(sys::EDeviceActivityLevel_k_EDeviceActivityLevel_UserInteraction), ..Default::default() };
        let _vr = vr::fake::install(ok);
        assert_eq!((Live.display_hz(), Live.universe_id()), (Some(90.0), Some(7)), "what SteamVR says");
        assert_eq!((Live.scene_app_running(), Live.worn()), (Some(true), Some(true)), "a game, and on the head");
        drop(_vr);

        let unknown = Some(sys::ETrackedPropertyError_TrackedProp_UnknownProperty);
        // synthetic: a value with the error set (undocumented), not trusted
        let failed =
            vr::fake::Script { vsync: Some((0.0, 90.0, 0.011)), universe: 7, prop_error: unknown, activity: Some(sys::EDeviceActivityLevel_k_EDeviceActivityLevel_Idle), ..Default::default() };
        let _vr = vr::fake::install(failed);
        assert_eq!((Live.display_hz(), Live.universe_id()), (None, None), "the error set: can't tell, whatever the value");
        assert_eq!((Live.scene_app_running(), Live.worn()), (Some(false), Some(false)), "no game, and off the head");
        drop(_vr);

        // the documented case: 0 with the error set (a bad index or type)
        let _vr = vr::fake::install(vr::fake::Script { vsync: Some((0.0, 0.0, 0.0)), universe: 0, prop_error: Some(sys::ETrackedPropertyError_TrackedProp_WrongDataType), ..Default::default() });
        assert_eq!((Live.display_hz(), Live.universe_id()), (None, None), "can't tell, not 0");
        drop(_vr);

        let _vr = vr::fake::install(vr::fake::Script::default()); // the activity level Unknown
        assert_eq!(Live.worn(), None, "the sensor can't tell");
    }

    /// At the polling boundary: an event nothing acts on is an event (Other), an empty queue is
    /// None, and the universe follows a room change, so the next relocalization in that room is
    /// only that. Each has the core's own name for the logs (main.rs's `last_event`), and only
    /// Other keeps OpenVR's number.
    #[test]
    fn an_ignored_event_is_one_and_an_empty_queue_isnt() {
        let ev = |t: u32, dev: u32| {
            let mut e: vr::VREvent_t = Default::default();
            (e.eventType, e.trackedDeviceIndex) = (t, dev);
            e
        };
        let (reset, ended) = (sys::EVREventType_VREvent_StandingZeroPoseReset, sys::EVREventType_VREvent_TrackedDeviceUserInteractionEnded);
        // a controller's interaction ending isn't the headset coming off: only device 0 is worn
        let changed = sys::EVREventType_VREvent_ChaperoneUniverseHasChanged; // logged, and only that for now
        let events = [ev(sys::EVREventType_VREvent_TrackedDeviceActivated, 3), ev(ended, 3), ev(ended, 0), ev(changed, 0), ev(reset, 0), ev(reset, 0)];
        let _vr = vr::fake::install(vr::fake::Script { universe: 8, sys_events: events.into(), ..Default::default() });
        let mut q = Queue::new(7);
        let mut got = vec![];
        while let Some(e) = q.next_event() {
            got.push((e.name(), e));
        }
        let other = |t: u32| (format!("other (the backend's {t})"), SessionEvent::Other(t));
        assert_eq!(
            got,
            [
                other(sys::EVREventType_VREvent_TrackedDeviceActivated),
                other(ended),
                ("headset off".into(), SessionEvent::Worn(false)),
                other(changed),
                ("room changed".into(), SessionEvent::RoomChanged(8)),
                ("relocalized".into(), SessionEvent::Relocalized),
            ]
        );
        assert_eq!((q.next_event(), q.universe), (None, 8), "an empty poll changes nothing");
    }

    /// A relocalization's re-read of the universe follows `Session::universe_id`'s rule: with its
    /// error set, whatever its value, it can't tell, and the room it's in stays. That's a
    /// continuity policy, not proof it's the same physical room: it only means a read that failed
    /// doesn't move anything. A later read that works still moves the room, from a known one or
    /// from none.
    #[test]
    fn a_relocalization_with_the_universe_unreadable_keeps_the_room() {
        let reset = vr::VREvent_t { eventType: sys::EVREventType_VREvent_StandingZeroPoseReset, ..Default::default() };
        let unknown = Some(sys::ETrackedPropertyError_TrackedProp_UnknownProperty);
        let _vr = vr::fake::install(vr::fake::Script { universe: 8, ..Default::default() });
        // the next reset, with the read failing or not; the fake's universe is 8 throughout
        let next = |q: &mut Queue, failing: bool| {
            // the script's lock let go of before the poll, which takes it again
            if let Some(s) = vr::fake::SCRIPT.lock().unwrap().as_mut() {
                (s.prop_error, s.sys_events) = (if failing { unknown } else { None }, [reset].into());
            }
            (q.next_event(), q.universe)
        };
        let (kept, moved) = (|| Some(SessionEvent::Relocalized), || Some(SessionEvent::RoomChanged(8)));
        // (a) in room 7: unreadable keeps it, then a good read moves it
        let mut q = Queue::new(7);
        assert_eq!(next(&mut q, true), (kept(), 7), "not room 8, nor no room: still 7");
        assert_eq!(next(&mut q, false), (moved(), 8), "a later good read still moves the room");
        // (b) unreadable again and again: still 7 each time
        let mut q = Queue::new(7);
        for i in 0..3 {
            assert_eq!(next(&mut q, true), (kept(), 7), "unreadable read {i}");
        }
        // (c) with no room yet: unreadable stays none, then a good read gives the room
        let mut q = Queue::new(0);
        assert_eq!(next(&mut q, true), (kept(), 0), "with no room yet, still none");
        assert_eq!(next(&mut q, false), (moved(), 8), "and a good read gives it");
    }
}
