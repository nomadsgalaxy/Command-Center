//! What the manipulation code needs to know about the world, besides where to write: where a
//! device is, where its laser points, where the head is, and a panel's overlay, picture size and
//! whether it's curved top to bottom. `Live` reads them from SteamVR and the panel list, and a
//! test hands in a stand-in (stage's tests, kvm.rs and grab.rs), so carrying, pushing, resizing
//! and placing can be checked without a headset.
//!
//! `Stage` is `Overlays` (overlay.rs, the writes) plus these reads. Anything that moves a panel
//! takes one.
use crate::geometry::{Mat, V3};
use crate::overlay::{Handle, Live, Overlays};
use crate::vr;

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
    /// A device's pose in standing space. `grab::MOUSE` is the mouse's ray. None when it isn't tracked.
    fn device(&self, dev: u32) -> Option<Mat>;
    /// A device's laser: its pose moved to the controller's tip (vr::laser_pose). The laser points along -z.
    fn laser(&self, dev: u32) -> Option<Mat>;
    /// Where the head is (or was last tracked).
    fn head(&self) -> V3;
    /// Slot i's overlay facts.
    fn panel(&self, i: usize) -> PanelFacts;
}

impl Stage for Live {
    fn device(&self, dev: u32) -> Option<Mat> {
        crate::grab::pose_of(dev)
    }

    fn laser(&self, dev: u32) -> Option<Mat> {
        crate::grab::laser_of(dev)
    }

    fn head(&self) -> V3 {
        vr::head_position()
    }

    fn panel(&self, i: usize) -> PanelFacts {
        let p = crate::panel(i);
        PanelFacts { overlay: p.overlay, size: p.size(), vert: p.vert() }
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
        pub devices: HashMap<u32, Mat>,
        pub lasers: HashMap<u32, Mat>,
        pub head: V3,
        pub panels: Vec<PanelFacts>,
    }

    impl Fake {
        /// A stage with `n` panels, each 1920x1080 with overlay 100 + its slot, and the head 1.6 m up.
        pub fn with_panels(n: usize) -> Fake {
            Fake { head: [0.0, 1.6, 0.0], panels: (0..n).map(|i| PanelFacts { overlay: 100 + i as u64, size: (1920, 1080), vert: false }).collect(), ..Default::default() }
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

        fn place_device(&mut self, h: Handle, dev: u32, m: &Mat) {
            self.writes.place_device(h, dev, m);
        }
    }

    impl Stage for Fake {
        fn device(&self, dev: u32) -> Option<Mat> {
            self.devices.get(&dev).copied()
        }

        fn laser(&self, dev: u32) -> Option<Mat> {
            self.lasers.get(&dev).copied().or_else(|| self.device(dev))
        }

        fn head(&self) -> V3 {
            self.head
        }

        fn panel(&self, i: usize) -> PanelFacts {
            self.panels[i]
        }
    }
}
