//! What the manipulation code needs to know about the world, besides where to write: where a
//! device is, where its laser points, where the head is, and a panel's overlay, picture size and
//! whether it's curved top to bottom. `Live` reads them from SteamVR and the panel list, and a
//! test hands in a stand-in (stage's tests, kvm.rs and grab.rs), so carrying, pushing, resizing
//! and placing can be checked without a headset.
//!
//! `Stage` is `Overlays` (overlay.rs, the writes) plus these reads. Anything that moves a panel
//! takes one.
use crate::geometry::{Mat, V3};
use crate::ids::{Ctl, Dev, Hand};
pub use crate::overlay::Live;
use crate::overlay::{Handle, Overlays};
use crate::vr;
use openvr_sys as sys;

/// What a panel's slot says about its overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelFacts {
    pub overlay: Handle,
    /// The picture's size in pixels.
    pub size: (u32, u32),
    /// Curved top to bottom, so its overlay is turned a quarter (kvm.rs `set_place`).
    pub vert: bool,
}

pub trait Stage: Overlays {
    /// A device's pose in standing space. `Dev::Mouse` is the mouse's ray. None when it isn't tracked.
    fn device(&self, dev: Dev) -> Option<Mat>;
    /// A device's laser: its pose moved to the controller's tip (vr::laser_pose). The laser points along -z.
    fn laser(&self, dev: Dev) -> Option<Mat>;
    /// Where the head is (or was last tracked).
    fn head(&self) -> V3;
    /// The head's pose right now, if it's tracked.
    fn head_pose(&self) -> Option<Mat>;
    /// The head's pose as it is now, not predicted to the frame, if it's tracked: what new things
    /// are placed in front of (vr::head). Using the predicted pose there would be a change of its own.
    fn head_now(&self) -> Option<Mat>;
    /// Whether d is a real controller: not the mouse, and not our cc_pointer device, which SteamVR
    /// tracks as a controller. A press from one makes it the primary device (R-2).
    fn is_controller(&self, d: Dev) -> bool;
    /// The real controller in hand h, if there is one (the wrist taskbar's).
    fn in_hand(&self, h: Hand) -> Option<Ctl>;
    /// How a device is named in the logs: OpenVR's index, so "by device 3" reads as it always did
    /// (0 the headset, 64 the mouse). Another runtime gives its own names.
    fn label(&self, d: Dev) -> String;
    /// The real controllers SteamVR tracks (not the mouse's pointer device), for docking (grab.rs).
    fn controllers(&self) -> Vec<Ctl>;
    /// A short buzz in the hand whose laser last worked overlay h (a dock target arming, grab.rs).
    fn pulse(&mut self, h: Handle);
    /// Whether device dev is connected (switched on), tracked or not.
    fn connected(&self, dev: Dev) -> bool;
    /// Slot i's overlay facts.
    fn panel(&self, i: usize) -> PanelFacts;
    /// Sends panel i's machine (or window) a pointer move to pixel (x, y).
    fn move_pointer(&mut self, i: usize, x: f64, y: f64);
}

/// Every tracked device's pose in standing space, as it'll be when this frame's picture reaches
/// your eyes (vr::to_photons), so what's carried or docked keeps up with the hand.
fn poses() -> Vec<sys::TrackedDevicePose_t> {
    let n = sys::k_unMaxTrackedDeviceCount as usize;
    let mut all: Vec<sys::TrackedDevicePose_t> = vec![Default::default(); n];
    crate::call!(sys, GetDeviceToAbsoluteTrackingPose, sys::ETrackingUniverseOrigin_TrackingUniverseStanding, vr::to_photons(), all.as_mut_ptr(), n as u32);
    all
}

impl Stage for Live {
    fn head_now(&self) -> Option<Mat> {
        vr::head()
    }

    fn is_controller(&self, d: Dev) -> bool {
        vr::is_real_controller(vr::index(d))
    }

    fn in_hand(&self, h: Hand) -> Option<Ctl> {
        let role = if h == Hand::Left { sys::ETrackedControllerRole_TrackedControllerRole_LeftHand } else { sys::ETrackedControllerRole_TrackedControllerRole_RightHand };
        let dev = crate::call!(sys, GetTrackedDeviceIndexForControllerRole, role);
        // (not our cc_pointer, which takes that hand's role while the mouse borrows it)
        (dev != sys::k_unTrackedDeviceIndexInvalid as u32 && vr::is_real_controller(dev)).then(|| Ctl::new(dev))
    }

    fn label(&self, d: Dev) -> String {
        vr::index(d).to_string()
    }

    fn device(&self, dev: Dev) -> Option<Mat> {
        if dev == Dev::Mouse {
            return crate::grab::mouse_ray();
        }
        let p = *poses().get(vr::index(dev) as usize)?;
        p.bPoseIsValid.then_some(p.mDeviceToAbsoluteTracking.m)
    }

    /// For the mouse, the laser is just its pose.
    fn laser(&self, dev: Dev) -> Option<Mat> {
        if dev == Dev::Mouse { self.device(dev) } else { vr::laser_pose(vr::index(dev)) }
    }

    fn head(&self) -> V3 {
        vr::head_position()
    }

    fn head_pose(&self) -> Option<Mat> {
        vr::head_predicted()
    }

    fn pulse(&mut self, h: Handle) {
        crate::call!(ov, TriggerLaserMouseHapticVibration, h.0, 0.05, 160.0, 0.6);
    }

    /// It can be connected and still untracked for a moment.
    fn connected(&self, dev: Dev) -> bool {
        dev == Dev::Mouse || poses().get(vr::index(dev) as usize).is_some_and(|p| p.bDeviceIsConnected)
    }

    fn controllers(&self) -> Vec<Ctl> {
        // connected, tracked or not: a hand that's out of the cameras' sight right now can still be a
        // target later in the carry (arm() reads its pose each frame)
        let poses = poses(); // once, not per device
        (1..sys::k_unMaxTrackedDeviceCount as u32).filter(|&d| poses.get(d as usize).is_some_and(|p| p.bDeviceIsConnected) && vr::is_real_controller(d)).map(Ctl::new).collect()
    }

    fn panel(&self, i: usize) -> PanelFacts {
        let p = crate::panel(i);
        PanelFacts { overlay: p.overlay, size: p.size(), vert: p.vert() }
    }

    fn move_pointer(&mut self, i: usize, x: f64, y: f64) {
        crate::panel(i).mouse(freerdp_sys::PTR_FLAGS_MOVE, x, y);
    }
}

#[cfg(test)]
pub mod fake {
    //! A stand-in for SteamVR: the devices and panels a test sets, and the writes it records.
    use super::*;
    use crate::overlay::fake::Writes;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct Fake {
        pub writes: Writes,
        pub devices: HashMap<Dev, Mat>,
        pub lasers: HashMap<Dev, Mat>,
        pub head: V3,
        /// The head's pose, when it's tracked; None is an untracked headset.
        pub head_pose: Option<Mat>,
        pub panels: Vec<PanelFacts>,
        /// The haptic pulses, by overlay.
        pub pulses: Vec<Handle>,
        /// Devices switched off (not just untracked).
        pub off: Vec<Dev>,
        /// Controllers that are on but untracked right now (no pose in `devices`).
        pub hidden: Vec<Dev>,
        /// The pointer moves sent, as (panel, x, y).
        pub moves: Vec<(usize, f64, f64)>,
    }

    impl Fake {
        /// A stage with `n` panels, each 1920x1080 with overlay 100 + its slot, and the head 1.6 m up.
        pub fn with_panels(n: usize) -> Fake {
            Fake { head: [0.0, 1.6, 0.0], panels: (0..n).map(|i| PanelFacts { overlay: crate::vr::Handle(100 + i as u64), size: (1920, 1080), vert: false }).collect(), ..Default::default() }
        }

        /// Overlay handle of panel i, for reading what was written to it.
        pub fn overlay(&self, i: usize) -> Handle {
            self.panels[i].overlay
        }
    }

    /// A device at `from` with its laser pointing at `at`: -z runs toward `at`, upright.
    pub fn aimed(from: V3, at: V3) -> Mat {
        use crate::geometry::{Pose, angles, panel_matrix};
        let (yaw, pitch) = angles(&[at[0] - from[0], at[1] - from[1], at[2] - from[2]]);
        panel_matrix(&Pose { centre: from, yaw, pitch, ..Default::default() })
    }

    /// The largest difference between two matrices' entries.
    pub fn apart(a: &Mat, b: &Mat) -> f32 {
        (0..3).flat_map(|r| (0..4).map(move |c| (r, c))).map(|(r, c)| (a[r][c] - b[r][c]).abs()).fold(0.0, f32::max)
    }

    impl Overlays for Fake {
        fn width(&mut self, h: Handle, metres: f32) {
            self.writes.width(h, metres);
        }

        fn curvature(&mut self, h: Handle, fraction: f32) {
            self.writes.curvature(h, fraction);
        }

        fn texel_aspect(&mut self, h: Handle, aspect: f32) {
            self.writes.texel_aspect(h, aspect);
        }

        fn bounds(&mut self, h: Handle, b: [f32; 4]) {
            self.writes.bounds(h, b);
        }

        fn mouse_scale(&mut self, h: Handle, s: [f32; 2]) {
            self.writes.mouse_scale(h, s);
        }

        fn place_room(&mut self, h: Handle, m: &Mat) {
            self.writes.place_room(h, m);
        }

        fn place_device(&mut self, h: Handle, dev: Dev, m: &Mat) {
            self.writes.place_device(h, dev, m);
        }

        fn texel_aspect_of(&self, h: Handle) -> f32 {
            self.writes.texel_aspect_of(h)
        }

        fn mouse_scale_of(&self, h: Handle) -> [f32; 2] {
            self.writes.mouse_scale_of(h)
        }
    }

    impl Stage for Fake {
        fn head_now(&self) -> Option<Mat> {
            self.head_pose
        }

        /// Every controller is real here; the mouse isn't.
        fn is_controller(&self, d: Dev) -> bool {
            matches!(d, Dev::Controller(_))
        }

        /// No hands here.
        fn in_hand(&self, _: Hand) -> Option<Ctl> {
            None
        }

        fn label(&self, d: Dev) -> String {
            Live.label(d)
        }

        fn device(&self, dev: Dev) -> Option<Mat> {
            self.devices.get(&dev).copied()
        }

        fn laser(&self, dev: Dev) -> Option<Mat> {
            self.lasers.get(&dev).copied().or_else(|| self.device(dev))
        }

        fn head(&self) -> V3 {
            self.head
        }

        fn head_pose(&self) -> Option<Mat> {
            self.head_pose
        }

        fn pulse(&mut self, h: Handle) {
            self.pulses.push(h);
        }

        fn connected(&self, dev: Dev) -> bool {
            !self.off.contains(&dev)
        }

        fn controllers(&self) -> Vec<Ctl> {
            let mut c: Vec<Ctl> = self.devices.keys().chain(&self.hidden).copied().filter(|d| !self.off.contains(d)).filter_map(|d| if let Dev::Controller(c) = d { Some(c) } else { None }).collect();
            c.sort();
            c.dedup();
            c
        }

        fn panel(&self, i: usize) -> PanelFacts {
            self.panels[i]
        }

        fn move_pointer(&mut self, i: usize, x: f64, y: f64) {
            self.moves.push((i, x, y));
        }
    }

    /// Today's SteamVR stage, `overlay::Live` and `Live`, over the fake SteamVR (vr::fake), with
    /// the panels a test sets. It's the golden tests' stage (golden.rs): the writes and reads that
    /// phase 0 moves are the ones the fixtures pin. Only `panel` and `move_pointer` are answered
    /// here, since they're the core's (the panel list), not SteamVR's. Devices are set in the
    /// fake's script (`put`).
    pub struct OverFake {
        pub panels: Vec<PanelFacts>,
        pub moves: Vec<(usize, f64, f64)>,
    }

    impl OverFake {
        /// `n` panels, each 1920x1080 with overlay 100 + its slot, as `Fake::with_panels` has them.
        pub fn with_panels(n: usize) -> OverFake {
            OverFake { panels: (0..n).map(|i| PanelFacts { overlay: crate::vr::Handle(100 + i as u64), size: (1920, 1080), vert: false }).collect(), moves: Vec::new() }
        }
    }

    /// Device `dev` in the fake's script: tracked at `m`, on but untracked (`None`), or switched
    /// off (`off`).
    pub fn put(dev: Dev, m: Option<Mat>, off: bool) {
        let dev = crate::vr::index(dev); // the fake is SteamVR: it knows devices by index
        let mut s = crate::vr::fake::SCRIPT.lock().unwrap();
        let s = s.get_or_insert_with(Default::default);
        s.poses.retain(|p| p.0 != dev);
        s.connected.retain(|&d| d != dev);
        match m {
            Some(m) if !off => s.poses.push((dev, m)),
            None if !off => s.connected.push(dev),
            _ => {}
        }
    }

    impl Overlays for OverFake {
        fn width(&mut self, h: Handle, metres: f32) {
            Live.width(h, metres)
        }

        fn curvature(&mut self, h: Handle, fraction: f32) {
            Live.curvature(h, fraction)
        }

        fn texel_aspect(&mut self, h: Handle, aspect: f32) {
            Live.texel_aspect(h, aspect)
        }

        fn bounds(&mut self, h: Handle, b: [f32; 4]) {
            Live.bounds(h, b)
        }

        fn mouse_scale(&mut self, h: Handle, s: [f32; 2]) {
            Live.mouse_scale(h, s)
        }

        fn place_room(&mut self, h: Handle, m: &Mat) {
            Live.place_room(h, m)
        }

        fn place_device(&mut self, h: Handle, dev: Dev, m: &Mat) {
            Live.place_device(h, dev, m)
        }

        fn texel_aspect_of(&self, h: Handle) -> f32 {
            Live.texel_aspect_of(h)
        }

        fn mouse_scale_of(&self, h: Handle) -> [f32; 2] {
            Live.mouse_scale_of(h)
        }
    }

    impl Stage for OverFake {
        fn head_now(&self) -> Option<Mat> {
            Live.head_now()
        }

        fn is_controller(&self, d: Dev) -> bool {
            Live.is_controller(d)
        }

        fn in_hand(&self, h: Hand) -> Option<Ctl> {
            Live.in_hand(h)
        }

        fn label(&self, d: Dev) -> String {
            Live.label(d)
        }

        fn device(&self, dev: Dev) -> Option<Mat> {
            Live.device(dev)
        }

        fn laser(&self, dev: Dev) -> Option<Mat> {
            Live.laser(dev)
        }

        fn head(&self) -> V3 {
            Live.head()
        }

        fn head_pose(&self) -> Option<Mat> {
            Live.head_pose()
        }

        fn pulse(&mut self, h: Handle) {
            Live.pulse(h)
        }

        fn connected(&self, dev: Dev) -> bool {
            Live.connected(dev)
        }

        fn controllers(&self) -> Vec<Ctl> {
            Live.controllers()
        }

        fn panel(&self, i: usize) -> PanelFacts {
            self.panels[i]
        }

        fn move_pointer(&mut self, i: usize, x: f64, y: f64) {
            self.moves.push((i, x, y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The logs name devices by OpenVR's index, as they did before Stage::label: the headset 0, the
    /// mouse 64, a controller its own number.
    #[test]
    fn a_device_is_labelled_by_its_openvr_index() {
        assert_eq!(Live.label(Dev::Head), "0");
        assert_eq!(Live.label(Dev::Mouse), "64");
        assert_eq!(Live.label(Dev::Controller(Ctl::new(3))), "3");
        assert_eq!(fake::Fake::default().label(Dev::Mouse), "64", "the stand-in names them the same");
    }
}
