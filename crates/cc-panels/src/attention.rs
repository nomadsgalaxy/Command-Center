//! How much of each panel's stream is actually worth having (D-042, docs/efficiency-plan.md 1).
//! The panel you're looking at runs at full rate, and the rest you can see get a few frames a
//! second. Panels that are out of view, hidden, minimized or hidden for theater mode get paused,
//! and so does everything during a VR game or with the headset off.
//! Where you're looking comes from the eye tracker's gaze (gaze.rs, ~30 a second), or from
//! where your head points when there's no fresh sample. main.rs works out the levels each
//! tick, and rdp.rs, gpu.rs and windows.rs act on them.
//! D-045: a glance doesn't make a panel full rate (DWELL), and a picture that hasn't changed
//! for a while (an email, a document) drops to a frame a second (QUIET_EVERY) until you work in it.
use crate::geometry::{Placement, V3, dot, norm};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Paused,
    Quiet,
    Peripheral,
    #[default]
    Full,
}

impl Level {
    pub fn from_u8(v: u8) -> Level {
        [Level::Paused, Level::Quiet, Level::Peripheral, Level::Full].get(v as usize).copied().unwrap_or_default()
    }
}

/// A peripheral remote's upload interval: 10 frames a second (3 looked choppy at a glance).
pub const PERIPHERAL: Duration = Duration::from_millis(100);
/// A peripheral window's: 5 a second (D-045: there are lots of windows and only a few remotes).
pub const PERIPHERAL_WINDOW: Duration = Duration::from_millis(200);
/// A quiet panel's (D-045): a frame a second, no matter what.
pub const QUIET_EVERY: Duration = Duration::from_secs(1);

// D-045's tunables, for tuning live (the level log says which one applies).
/// How long a panel has to be looked at before it goes full rate, since a glance across panels isn't a look.
const DWELL: Duration = Duration::from_millis(300);
/// A frame that changes this much of the picture goes up right away and lifts quiet for BURST_HOLD.
const BURST_AREA: f64 = 0.05;
const BURST_HOLD: Duration = Duration::from_secs(2);
/// Changes smaller than this (a caret, a clock) leave a picture quiet...
const QUIET_AREA: f64 = 0.01;
/// ...and so do bigger ones that don't keep up for SUSTAINED (unlike video or a scroll); it goes quiet after QUIET_AFTER of that.
const SUSTAINED: Duration = Duration::from_secs(1);
const QUIET_AFTER: Duration = Duration::from_secs(5);
/// Full rate for this long after the last direct input (the mouse, a laser, typing), quiet or not.
const INPUT_HOLD: Duration = Duration::from_secs(3);
/// A lower level has to be wanted this long before we take it, so levels don't flap; a higher one
/// is taken right away. It's 3 s because at 1 s, glancing between windows flipped them about once
/// a second (live: 269 changes in 6 minutes).
const SETTLE: Duration = Duration::from_secs(3);
/// Degrees around the gaze point that count as looked at, to cover the tracker's few degrees of error.
const FOVEA_EYES: f64 = 5.0;
/// With no fresh gaze: degrees around where the head points.
const FOVEA_HEAD: f64 = 30.0;
/// ponytail: the view frustum as a cone around the head's forward, a margin wider than the
/// Frame's ~110 degrees; use the eyes' own projections (GetProjectionRaw) if edge panels pause visibly.
const VIEW: f64 = 60.0;

/// What this tick knows about you.
pub struct Seen {
    pub hidden: bool,  // every panel hidden (HIDDEN)
    pub game: bool,    // a VR game is running (taskbar.rs)
    pub on_head: bool, // the headset is worn (VREvent 103/104)
    pub head: Option<(V3, V3)>, // where the head is and its unit forward, while tracked
    pub gaze: Option<(V3, V3)>, // a fresh gaze ray (origin, unit direction)
}

/// How far, in degrees, a ray (origin o, unit direction d) passes from a panel; 0 if it goes through it.
pub fn miss(pl: &Placement, o: &V3, d: &V3) -> f64 {
    let (w, h) = (pl.width / 2.0, pl.height / 2.0);
    let angle = |p: V3| {
        let v = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
        (dot(&v, d) / norm(&v).max(1e-9)).clamp(-1.0, 1.0).acos().to_degrees()
    };
    match pl.hit(o, d) {
        Some((_, u, v)) if u.abs() <= w && v.abs() <= h => 0.0,
        Some((_, u, v)) => {
            let m = pl.on_surface(u.clamp(-w, w), v.clamp(-h, h), 0.0); // its nearest edge point
            angle([m[0][3] as f64, m[1][3] as f64, m[2][3] as f64])
        }
        None => {
            // pointed away from its surface, so treat it as its bounding circle
            let c = pl.c;
            let dist = norm(&[c[0] - o[0], c[1] - o[1], c[2] - o[2]]).max(1e-9);
            (angle(c) - (w.hypot(h) / dist).atan().to_degrees()).max(0.0)
        }
    }
}

/// What level a panel would get now, and why (for its log line).
pub fn want(s: &Seen, minimized: bool, theater_hidden: bool, pl: &Placement) -> (Level, &'static str) {
    let paused = [(s.hidden, "hidden"), (minimized, "minimized"), (theater_hidden, "theater"), (s.game, "VR game"), (!s.on_head, "headset off")];
    if let Some((_, why)) = paused.iter().find(|(on, _)| *on) {
        return (Level::Paused, why);
    }
    let Some((eye, fwd)) = s.head else { return (Level::Full, "head not tracked") };
    if miss(pl, &eye, &fwd) > VIEW {
        return (Level::Paused, "out of view");
    }
    let looked = match s.gaze {
        Some((o, d)) => miss(pl, &o, &d) <= FOVEA_EYES,
        None => miss(pl, &eye, &fwd) <= FOVEA_HEAD,
    };
    match (looked, s.gaze.is_some()) {
        (true, true) => (Level::Full, "looked at"),
        (true, false) => (Level::Full, "head toward it"),
        _ => (Level::Peripheral, "peripheral"),
    }
}

/// A panel's level with hysteresis.
#[derive(Default)]
pub struct Attention {
    pub level: Level,
    lower: Option<Instant>,  // when a lower level started being wanted
    looked: Option<Instant>, // when it started being looked at (DWELL)
    pub input: Option<Instant>, // its last direct input (main.rs)
    changing: Option<(Instant, Instant)>, // its last QUIET_AREA change, and since when they've kept coming
    busy: Option<Instant>,   // the last time it had been changing for SUSTAINED
    burst: Option<Instant>,  // its last BURST_AREA change
}

impl Attention {
    /// A frame changed `area` of its picture (0 to 1: the most since the last tick, 0 for nothing).
    pub fn sense(&mut self, area: f64, now: Instant) {
        if self.level == Level::Paused {
            // no frames come while paused, so what it was before carries over (video stays video)
            if self.busy.is_some_and(|b| now - b < QUIET_AFTER) {
                self.busy = Some(now);
            }
            return;
        }
        self.busy.get_or_insert(now); // new, so not quiet yet
        if area >= QUIET_AREA {
            let since = self.changing.filter(|c| now - c.0 < SUSTAINED).map_or(now, |c| c.1);
            self.changing = Some((now, since));
            if now - since >= SUSTAINED {
                self.busy = Some(now);
            }
        }
        if area >= BURST_AREA {
            if self.burst.is_some_and(|b| now - b < QUIET_AFTER) {
                self.busy = Some(now); // bursts that keep coming (a dashboard, a chat) count as busy, so no quiet and no flapping
            }
            self.burst = Some(now);
        }
    }

    /// D-045 on top of want's level: direct input is full rate right away and for INPUT_HOLD
    /// after, a quiet picture stays QUIET_EVERY even when looked at, and a look only goes full
    /// rate after DWELL.
    pub fn refine(&mut self, (want, why): (Level, &'static str), now: Instant) -> (Level, &'static str) {
        let ago = |t: Option<Instant>| t.map_or(Duration::MAX, |t| now - t);
        let look = want == Level::Full && matches!(why, "looked at" | "head toward it");
        self.looked = look.then(|| self.looked.unwrap_or(now));
        let still = self.busy.is_some() && ago(self.busy) >= QUIET_AFTER;
        if want == Level::Paused {
            (want, why)
        } else if ago(self.input) < INPUT_HOLD {
            (Level::Full, "input")
        } else if still && ago(self.burst) >= BURST_HOLD {
            (Level::Quiet, "quiet")
        } else if look && ago(self.looked) < DWELL && self.level < Level::Full {
            (Level::Peripheral, "glance")
        } else if still {
            (want, "burst") // lifted for BURST_HOLD, at the edge's rate when it's at the edge
        } else {
            (want, why)
        }
    }

    /// This tick's want. Returns the new level when it changes. Up goes right away, down only once
    /// it's been wanted for SETTLE (except down to quiet, which has its own waits).
    pub fn step(&mut self, want: Level, now: Instant) -> Option<Level> {
        if want == self.level {
            self.lower = None;
            return None;
        }
        if want < self.level && want != Level::Quiet && now - *self.lower.get_or_insert(now) < SETTLE {
            return None;
        }
        (self.level, self.lower) = (want, None);
        Some(want)
    }
}

/// Starting up (my words: "force all panels to be 5fps until the lag spike calms down"). Every
/// remote's first full screens, every window's first frame and every card all land at once, so
/// for a while every panel only takes a frame every WARM_EVERY (main.rs ends it).
pub static WARM: AtomicBool = AtomicBool::new(true);
pub const WARM_EVERY: Duration = Duration::from_millis(200);

/// Whether a frame can go up now: always at full rate, every `peripheral` (PERIPHERAL,
/// PERIPHERAL_WINDOW) at the edge, every QUIET_EVERY when quiet, never when paused. While
/// warming up, every WARM_EVERY at most.
pub fn due(level: Level, peripheral: Duration, last: Option<Instant>, now: Instant) -> bool {
    until_due(level, peripheral, last, now) == Some(Duration::ZERO)
}

/// How long until a frame can go up (zero means now); None while paused.
pub fn until_due(level: Level, peripheral: Duration, last: Option<Instant>, now: Instant) -> Option<Duration> {
    let every = |d: Duration| last.map_or(Duration::ZERO, |t| d.saturating_sub(now - t));
    let warm = WARM.load(Relaxed);
    match level {
        Level::Full if warm => Some(every(WARM_EVERY)),
        Level::Full => Some(Duration::ZERO),
        Level::Peripheral => Some(every(if warm { WARM_EVERY.max(peripheral) } else { peripheral })),
        Level::Quiet => Some(every(QUIET_EVERY)),
        Level::Paused => None,
    }
}

/// Whether startup's warm-up is over: everything has come up (`settled`: every remote connected
/// and every window showing, since the heavy decode is on their threads where slow ticks don't
/// see it), we're at least WARM_MIN in, and there's been no slow tick for WARM_CALM. Or we're
/// WARM_MAX in, no matter what.
pub fn warm_over(since_start: Duration, since_slow: Duration, settled: bool) -> bool {
    since_start >= WARM_MAX || (settled && since_start >= WARM_MIN && since_slow >= WARM_CALM)
}
const WARM_MIN: Duration = Duration::from_secs(6); // live, 2 s ended it before the remotes' first screens came in
const WARM_CALM: Duration = Duration::from_secs(2);
const WARM_MAX: Duration = Duration::from_secs(15);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Pose, panel_matrix};

    /// A 1 m x 0.5 m panel 2 m ahead (-z) of an eye at 1.6 m, turned `yaw` degrees around you.
    fn ahead(yaw: f64) -> Placement {
        let a = yaw.to_radians();
        let centre = [-2.0 * a.sin(), 1.6, -2.0 * a.cos()];
        Placement::from_matrix(&panel_matrix(&Pose { centre, yaw, width: 1.0, ..Default::default() }), 1.0, 0.5, 0.0)
    }

    #[test]
    fn where_you_look_sets_the_level() {
        let eye = [0.0, 1.6, 0.0];
        let fwd = [0.0, 0.0, -1.0];
        let mut s = Seen { hidden: false, game: false, on_head: true, head: Some((eye, fwd)), gaze: None };
        assert_eq!(miss(&ahead(0.0), &eye, &fwd), 0.0);
        // its nearest edge: 0.5 m off centre at 2 m is 14 degrees, so 40 degrees around is ~26 off
        let m = miss(&ahead(40.0), &eye, &fwd);
        assert!((m - (40.0 - 14.04)).abs() < 1.0, "{m}");
        assert!(miss(&ahead(180.0), &eye, &fwd) > 160.0, "behind you");
        // no gaze: about 30 degrees around the head's forward counts as looked at
        assert_eq!(want(&s, false, false, &ahead(30.0)), (Level::Full, "head toward it"));
        assert_eq!(want(&s, false, false, &ahead(60.0)), (Level::Peripheral, "peripheral"));
        assert_eq!(want(&s, false, false, &ahead(100.0)), (Level::Paused, "out of view"));
        // fresh eyes decide: looking 30 degrees left leaves the one ahead peripheral
        let left = [-(30f64.to_radians().sin()), 0.0, -(30f64.to_radians().cos())];
        s.gaze = Some((eye, left));
        assert_eq!(want(&s, false, false, &ahead(0.0)).0, Level::Peripheral);
        assert_eq!(want(&s, false, false, &ahead(30.0)), (Level::Full, "looked at"));
        // these pause no matter what the eyes do
        assert_eq!(want(&s, true, false, &ahead(30.0)), (Level::Paused, "minimized"));
        assert_eq!(want(&s, false, true, &ahead(30.0)), (Level::Paused, "theater"));
        s.on_head = false;
        assert_eq!(want(&s, false, false, &ahead(30.0)), (Level::Paused, "headset off"));
        s.game = true;
        assert_eq!(want(&s, false, false, &ahead(30.0)), (Level::Paused, "VR game"));
        s.hidden = true;
        assert_eq!(want(&s, false, false, &ahead(30.0)), (Level::Paused, "hidden"));
    }

    #[test]
    fn levels_drop_after_a_second_and_rise_at_once() {
        let t = Instant::now();
        let at = |f: f64| t + SETTLE.mul_f64(f); // in SETTLEs
        let mut a = Attention::default();
        assert_eq!(a.step(Level::Full, t), None);
        assert_eq!(a.step(Level::Paused, at(0.0)), None);
        assert_eq!(a.step(Level::Peripheral, at(0.5)), None, "still lower: the wait runs on");
        assert_eq!(a.step(Level::Paused, at(1.0)), Some(Level::Paused));
        assert_eq!(a.step(Level::Full, at(1.01)), Some(Level::Full), "looked at: at once");
        assert_eq!(a.step(Level::Paused, at(1.02)), None);
        assert_eq!(a.step(Level::Full, at(1.5)), None, "a glance away resets it");
        assert_eq!(a.step(Level::Paused, at(2.0)), None);
        assert_eq!(a.level, Level::Full);
        assert_eq!(Level::from_u8(Level::Peripheral as u8), Level::Peripheral);
    }

    #[test]
    fn peripheral_frames_go_a_few_a_second() {
        WARM.store(false, Relaxed); // (the only test that reads WARM; its own warm-up part is below)
        let t = Instant::now();
        assert!(due(Level::Full, PERIPHERAL, Some(t), t) && !due(Level::Paused, PERIPHERAL, None, t));
        assert!(due(Level::Peripheral, PERIPHERAL, None, t));
        assert!(!due(Level::Peripheral, PERIPHERAL, Some(t), t + PERIPHERAL / 2));
        assert!(due(Level::Peripheral, PERIPHERAL, Some(t), t + PERIPHERAL));
        assert_eq!(until_due(Level::Peripheral, PERIPHERAL, Some(t), t + PERIPHERAL / 4), Some(PERIPHERAL * 3 / 4), "the RDP thread's wait (gpu.rs)");
        assert_eq!(until_due(Level::Paused, PERIPHERAL, Some(t), t), None);
        assert!(!due(Level::Peripheral, PERIPHERAL_WINDOW, Some(t), t + PERIPHERAL) && due(Level::Peripheral, PERIPHERAL_WINDOW, Some(t), t + PERIPHERAL_WINDOW), "windows: 5 a second");
        assert!(!due(Level::Quiet, PERIPHERAL, Some(t), t + QUIET_EVERY / 2) && due(Level::Quiet, PERIPHERAL, Some(t), t + QUIET_EVERY), "quiet: 1 a second");
        // startup's warm-up: every panel every WARM_EVERY, then as usual
        WARM.store(true, Relaxed);
        assert!(!due(Level::Full, PERIPHERAL, Some(t), t + WARM_EVERY / 2) && due(Level::Full, PERIPHERAL, Some(t), t + WARM_EVERY));
        WARM.store(false, Relaxed);
        assert!(due(Level::Full, PERIPHERAL, Some(t), t));
        let s = Duration::from_secs;
        assert!(!warm_over(s(1), s(9), true), "too early");
        assert!(!warm_over(s(8), s(1), true), "still slow");
        assert!(!warm_over(s(8), s(3), false), "a remote or window still coming up");
        assert!(warm_over(s(8), s(3), true), "calm");
        assert!(warm_over(s(20), Duration::ZERO, false), "long enough whatever");
    }
    /// A panel looked at the whole time: its refined want at each SENSE after `t`, sensing `area` each time.
    fn run(a: &mut Attention, t: Instant, from: f64, to: f64, area: f64) -> (Level, &'static str) {
        let mut w = (Level::Full, "");
        let mut s = from;
        while s <= to {
            let now = t + Duration::from_secs_f64(s);
            a.sense(area, now);
            w = a.refine((Level::Full, "looked at"), now);
            a.step(w.0, now);
            s += 0.05;
        }
        w
    }

    #[test]
    fn a_glance_isnt_a_look() {
        let t = Instant::now();
        let ms = |m: u64| t + Duration::from_millis(m);
        let mut a = Attention { level: Level::Peripheral, ..Default::default() };
        a.sense(0.0, t);
        assert_eq!(a.refine((Level::Full, "looked at"), t), (Level::Peripheral, "glance"));
        assert_eq!(a.refine((Level::Full, "looked at"), ms(200)), (Level::Peripheral, "glance"));
        assert_eq!(a.refine((Level::Peripheral, "peripheral"), ms(250)), (Level::Peripheral, "peripheral"), "looked away: the dwell starts over");
        assert_eq!(a.refine((Level::Full, "looked at"), ms(300)), (Level::Peripheral, "glance"));
        assert_eq!(a.refine((Level::Full, "looked at"), ms(600)), (Level::Full, "looked at"));
        assert_eq!(a.refine((Level::Full, "held"), ms(650)).0, Level::Full, "not a look: at once");
        a.level = Level::Full;
        assert_eq!(a.refine((Level::Full, "looked at"), ms(700)), (Level::Full, "looked at"), "already full: a look back keeps it");
        a.level = Level::Paused;
        assert_eq!(a.refine((Level::Full, "looked at"), ms(710)), (Level::Peripheral, "glance"), "from paused: the edge's rate meanwhile");
    }

    #[test]
    fn a_still_picture_goes_quiet_until_worked() {
        let t = Instant::now();
        let mut a = Attention::default();
        assert_eq!(run(&mut a, t, 0.0, 4.0, 0.002).0, Level::Full, "a caret blinking: not yet quiet");
        assert_eq!(run(&mut a, t, 4.05, 6.0, 0.002), (Level::Quiet, "quiet"));
        assert_eq!(a.level, Level::Quiet, "taken at once (no SETTLE)");
        // a mouse on it: full right away, and for INPUT_HOLD after the last input
        let at = |s: f64| t + Duration::from_secs_f64(s);
        a.input = Some(at(6.1));
        assert_eq!(a.refine((Level::Full, "looked at"), at(6.1)), (Level::Full, "input"));
        assert_eq!(a.refine((Level::Peripheral, "peripheral"), at(8.0)), (Level::Full, "input"), "looking at the keyboard");
        assert_eq!(a.refine((Level::Paused, "minimized"), at(8.0)), (Level::Paused, "minimized"));
        assert_eq!(a.refine((Level::Full, "looked at"), at(9.2)), (Level::Quiet, "quiet"), "still nothing changed");
        // video: big changes that keep coming, full rate when looked at; peripheral at the edge's rate
        let mut v = Attention::default();
        assert_eq!(run(&mut v, t, 0.0, 30.0, 0.5), (Level::Full, "looked at"));
        assert_eq!(v.refine((Level::Peripheral, "peripheral"), at(30.0)), (Level::Peripheral, "peripheral"));
    }

    #[test]
    fn a_big_change_lifts_quiet_a_while() {
        let t = Instant::now();
        let at = |s: f64| t + Duration::from_secs_f64(s);
        let mut a = Attention::default();
        assert_eq!(run(&mut a, t, 0.0, 6.0, 0.0).0, Level::Quiet);
        a.sense(0.02, at(6.1));
        assert_eq!(a.refine((Level::Full, "looked at"), at(6.1)).0, Level::Quiet, "a small change alone: still quiet");
        a.sense(0.3, at(6.2)); // an email opened
        assert_eq!(a.refine((Level::Full, "looked at"), at(6.2)), (Level::Full, "burst"), "up at once");
        assert_eq!(a.step(Level::Full, at(6.2)), Some(Level::Full));
        assert_eq!(a.refine((Level::Peripheral, "peripheral"), at(7.0)).0, Level::Peripheral);
        assert_eq!(run(&mut a, t, 7.0, 8.1, 0.0).0, Level::Full, "held for BURST_HOLD");
        assert_eq!(run(&mut a, t, 8.25, 8.5, 0.0).0, Level::Quiet, "then quiet again: one frame isn't video");
        // bursts every few seconds (a dashboard): busy, so no flapping between quiet and full
        a.sense(0.3, at(9.0));
        a.sense(0.3, at(12.0));
        assert_eq!(a.refine((Level::Full, "looked at"), at(15.0)), (Level::Full, "looked at"));
    }

    #[test]
    fn a_pause_keeps_what_it_showed() {
        let t = Instant::now();
        let at = |s: f64| t + Duration::from_secs_f64(s);
        let mut v = Attention::default();
        run(&mut v, t, 0.0, 3.0, 0.5); // video
        v.level = Level::Paused;
        for i in 0..200 {
            let now = at(3.0 + i as f64 * 0.05);
            v.sense(0.0, now); // paused, so no frames
            v.refine((Level::Paused, "minimized"), now);
        }
        v.level = Level::Peripheral;
        assert_eq!(v.refine((Level::Full, "looked at"), at(13.0)).0, Level::Peripheral, "a glance, not quiet");
        let mut q = Attention::default();
        run(&mut q, t, 0.0, 6.0, 0.0);
        q.level = Level::Paused;
        q.sense(0.0, at(20.0));
        q.level = Level::Peripheral;
        assert_eq!(q.refine((Level::Full, "looked at"), at(20.0)).0, Level::Quiet, "still quiet");
    }
}
