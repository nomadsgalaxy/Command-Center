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

/// SteamVR's overlay queues, for real.
pub struct Live;

/// SteamVR's own queue, for real. `universe` is the one the session's in, which a relocalization
/// is compared with (vr::session_event); it follows each RoomChanged it hands out.
pub struct Queue {
    pub universe: u64,
    last: u32,
}

impl Queue {
    pub fn new(universe: u64) -> Queue {
        Queue { universe, last: 0 }
    }

    /// OpenVR's type number for the last event next_event handed out, whatever it became:
    /// main.rs's `last_event`, a diagnostic.
    pub fn last_type(&self) -> u32 {
        self.last
    }
}

impl Pointing for Live {
    fn next(&mut self, s: SurfaceId) -> Option<SurfaceEvent> {
        let mut e: vr::VREvent_t = Default::default();
        call!(ov, PollNextOverlayEvent, s.0, &mut e, size_of::<vr::VREvent_t>() as u32).then(|| vr::surface_event(&e))
    }
}

impl SessionEvents for Queue {
    fn next_event(&mut self) -> Option<SessionEvent> {
        let mut e: vr::VREvent_t = Default::default();
        if !call!(sys, PollNextEvent, &mut e, size_of::<vr::VREvent_t>() as u32) {
            return None;
        }
        self.last = e.eventType;
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

    /// At the polling boundary: an event nothing acts on is an event (Other, and the last type
    /// moves), an empty queue is None (and the last type stays), and the universe follows a room
    /// change, so the next relocalization in that room is only that.
    #[test]
    fn an_ignored_event_is_one_and_an_empty_queue_isnt() {
        let ev = |t: u32, dev: u32| {
            let mut e: vr::VREvent_t = Default::default();
            (e.eventType, e.trackedDeviceIndex) = (t, dev);
            e
        };
        let reset = sys::EVREventType_VREvent_StandingZeroPoseReset;
        let events = [ev(sys::EVREventType_VREvent_TrackedDeviceActivated, 3), ev(sys::EVREventType_VREvent_TrackedDeviceUserInteractionEnded, 0), ev(reset, 0), ev(reset, 0)];
        let _vr = vr::fake::install(vr::fake::Script { universe: 8, sys_events: events.into(), ..Default::default() });
        let mut q = Queue::new(7);
        let mut got = vec![];
        while let Some(e) = q.next_event() {
            got.push((e, q.last_type()));
        }
        assert_eq!(
            got,
            [
                (SessionEvent::Other(sys::EVREventType_VREvent_TrackedDeviceActivated), sys::EVREventType_VREvent_TrackedDeviceActivated),
                (SessionEvent::Worn(false), sys::EVREventType_VREvent_TrackedDeviceUserInteractionEnded),
                (SessionEvent::RoomChanged(8), reset),
                (SessionEvent::Relocalized, reset),
            ]
        );
        assert_eq!((q.next_event(), q.last_type(), q.universe), (None, reset, 8), "an empty poll changes nothing");
    }
}
