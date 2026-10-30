//! The one place a placement becomes SteamVR calls. Everything that sets where an overlay is,
//! how wide, how bent and how its picture is stretched builds a `Place` and hands it to `apply`,
//! instead of making the same four or five calls itself (the panels, the Extra window, the cards,
//! the taskbar, Plasma's bar, the backs, the HUD, the cursor and the theater backdrop each used to).
//!
//! The calls go through `Overlays`, so a test can stand in for SteamVR: `Live` makes the real
//! calls, and the tests' `fake::Fake` writes them down. `Live` is the only thing here that touches
//! OpenVR. This file is also built by the cc-win-spike binary (with back.rs), so it uses only vr.rs
//! and geometry.rs.
//!
//! `apply` writes everything it's given every time. When a caller wants "only if it changed", it
//! still decides that itself, since each one compares different things (the taskbar and the cards
//! round to millimetres, Kvm::set_place doesn't compare at all). Moving that here would change when
//! SteamVR gets called.
use crate::geometry::{Mat, bend};
use crate::ids::Dev;
use crate::{call, vr};
use openvr_sys as sys;

pub type Handle = vr::Handle;

/// Where an overlay is placed: in the room (standing space), or relative to a tracked device (the
/// headset, a controller).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum At {
    Room(Mat),
    Device(Dev, Mat),
}

impl At {
    /// `dev` as the taskbar keeps it: None is the room, anything else a tracked device.
    pub fn on(dev: Option<Dev>, m: Mat) -> At {
        match dev {
            None => At::Room(m),
            Some(d) => At::Device(d, m),
        }
    }
}

/// An overlay's placement. Only what's set gets written, so a caller that never bent its overlay
/// doesn't start calling SetOverlayCurvature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub at: At,
    /// Across, in metres.
    pub width: f64,
    /// The curve radius in metres (0 is flat), written as SteamVR's fraction (geometry::bend).
    pub curve: Option<f64>,
    /// The picture's stretch: 1 is the pixels' own shape.
    pub texel: Option<f32>,
    /// The part of the texture shown: u min, v min, u max, v max.
    pub bounds: Option<[f32; 4]>,
    /// What mouse events count in (x, y), from the bottom left.
    pub mouse_scale: Option<[f32; 2]>,
}

impl Place {
    pub fn new(at: At, width: f64) -> Place {
        Place { at, width, curve: None, texel: None, bounds: None, mouse_scale: None }
    }

    pub fn curve(self, radius: f64) -> Place {
        Place { curve: Some(radius), ..self }
    }

    pub fn texel(self, aspect: f32) -> Place {
        Place { texel: Some(aspect), ..self }
    }

    pub fn bounds(self, b: [f32; 4]) -> Place {
        Place { bounds: Some(b), ..self }
    }

    pub fn mouse_scale(self, s: [f32; 2]) -> Place {
        Place { mouse_scale: Some(s), ..self }
    }
}

/// The writes behind a placement.
pub trait Overlays {
    fn width(&mut self, h: Handle, metres: f32);
    fn curvature(&mut self, h: Handle, fraction: f32);
    fn texel_aspect(&mut self, h: Handle, aspect: f32);
    fn bounds(&mut self, h: Handle, b: [f32; 4]);
    fn mouse_scale(&mut self, h: Handle, s: [f32; 2]);
    fn place_room(&mut self, h: Handle, m: &Mat);
    fn place_device(&mut self, h: Handle, dev: Dev, m: &Mat);
}

/// Sets overlay `h` to `p`: the texture bounds, width, picture stretch, curvature and mouse scale
/// if they're given, then the transform. One order, for every overlay.
pub fn apply(o: &mut impl Overlays, h: Handle, p: &Place) {
    if let Some(b) = p.bounds {
        o.bounds(h, b);
    }
    o.width(h, p.width as f32);
    if let Some(a) = p.texel {
        o.texel_aspect(h, a);
    }
    if let Some(r) = p.curve {
        o.curvature(h, bend(p.width, r));
    }
    if let Some(s) = p.mouse_scale {
        o.mouse_scale(h, s);
    }
    match &p.at {
        At::Room(m) => o.place_room(h, m),
        At::Device(dev, m) => o.place_device(h, *dev, m),
    }
}

/// SteamVR, for real.
pub struct Live;

impl Overlays for Live {
    fn width(&mut self, h: Handle, metres: f32) {
        call!(ov, SetOverlayWidthInMeters, h.0, metres);
    }

    fn curvature(&mut self, h: Handle, fraction: f32) {
        call!(ov, SetOverlayCurvature, h.0, fraction);
    }

    fn texel_aspect(&mut self, h: Handle, aspect: f32) {
        call!(ov, SetOverlayTexelAspect, h.0, aspect);
    }

    fn bounds(&mut self, h: Handle, b: [f32; 4]) {
        let mut t = sys::VRTextureBounds_t { uMin: b[0], vMin: b[1], uMax: b[2], vMax: b[3] };
        call!(ov, SetOverlayTextureBounds, h.0, &mut t);
    }

    fn mouse_scale(&mut self, h: Handle, s: [f32; 2]) {
        let mut scale = sys::HmdVector2_t { v: s };
        call!(ov, SetOverlayMouseScale, h.0, &mut scale);
    }

    fn place_room(&mut self, h: Handle, m: &Mat) {
        vr::place(h, m);
    }

    fn place_device(&mut self, h: Handle, dev: Dev, m: &Mat) {
        let mut t = sys::HmdMatrix34_t { m: *m };
        call!(ov, SetOverlayTransformTrackedDeviceRelative, h.0, vr::index(dev), &mut t);
    }
}

/// A stand-in for SteamVR's writes, which writes each call down.
#[cfg(test)]
pub mod fake {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Call {
        Width(Handle, f32),
        Curvature(Handle, f32),
        Texel(Handle, f32),
        Bounds(Handle, [f32; 4]),
        MouseScale(Handle, [f32; 2]),
        Room(Handle, Mat),
        Device(Handle, u32, Mat),
    }

    #[derive(Default)]
    pub struct Writes(pub Vec<Call>);

    impl Writes {
        /// What was written to overlay h, in order.
        pub fn of(&self, h: Handle) -> Vec<Call> {
            self.0.iter().filter(|c| call_handle(c) == h).cloned().collect()
        }

        /// The last width written to h, in metres.
        pub fn width_of(&self, h: Handle) -> Option<f32> {
            self.0.iter().rev().find_map(|c| if let Call::Width(x, w) = c { (*x == h).then_some(*w) } else { None })
        }

        pub fn curvature_of(&self, h: Handle) -> Option<f32> {
            self.0.iter().rev().find_map(|c| if let Call::Curvature(x, v) = c { (*x == h).then_some(*v) } else { None })
        }

        pub fn texel_of(&self, h: Handle) -> Option<f32> {
            self.0.iter().rev().find_map(|c| if let Call::Texel(x, v) = c { (*x == h).then_some(*v) } else { None })
        }

        /// The last room transform written to h.
        pub fn room_of(&self, h: Handle) -> Option<Mat> {
            self.0.iter().rev().find_map(|c| if let Call::Room(x, m) = c { (*x == h).then_some(*m) } else { None })
        }
    }

    fn call_handle(c: &Call) -> Handle {
        match c {
            Call::Width(h, _) | Call::Curvature(h, _) | Call::Texel(h, _) | Call::Bounds(h, _) | Call::MouseScale(h, _) | Call::Room(h, _) | Call::Device(h, ..) => *h,
        }
    }

    impl Overlays for Writes {
        fn width(&mut self, h: Handle, metres: f32) {
            self.0.push(Call::Width(h, metres));
        }

        fn curvature(&mut self, h: Handle, fraction: f32) {
            self.0.push(Call::Curvature(h, fraction));
        }

        fn texel_aspect(&mut self, h: Handle, aspect: f32) {
            self.0.push(Call::Texel(h, aspect));
        }

        fn bounds(&mut self, h: Handle, b: [f32; 4]) {
            self.0.push(Call::Bounds(h, b));
        }

        fn mouse_scale(&mut self, h: Handle, s: [f32; 2]) {
            self.0.push(Call::MouseScale(h, s));
        }

        fn place_room(&mut self, h: Handle, m: &Mat) {
            self.0.push(Call::Room(h, *m));
        }

        fn place_device(&mut self, h: Handle, dev: Dev, m: &Mat) {
            self.0.push(Call::Device(h, vr::index(dev), *m)); // OpenVR's index, as SteamVR gets it
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Call, Writes};
    use super::*;

    const M: Mat = [[1.0, 0.0, 0.0, 0.5], [0.0, 1.0, 0.0, 1.5], [0.0, 0.0, 1.0, -2.0]];

    #[test]
    fn a_bare_placement_writes_only_width_and_transform() {
        let mut w = Writes::default();
        apply(&mut w, crate::vr::Handle(7), &Place::new(At::Room(M), 1.25));
        assert_eq!(w.0, vec![Call::Width(crate::vr::Handle(7), 1.25), Call::Room(crate::vr::Handle(7), M)], "no curvature, stretch, bounds or mouse scale unless asked");
    }

    #[test]
    fn everything_given_is_written_in_the_same_order_every_time() {
        let mut w = Writes::default();
        let p = Place::new(At::Device(Dev::Head, M), 0.8).curve(1.0).texel(1.5).bounds([0.1, 0.2, 0.9, 0.8]).mouse_scale([800.0, 600.0]);
        apply(&mut w, crate::vr::Handle(3), &p);
        assert_eq!(
            w.0,
            vec![
                Call::Bounds(crate::vr::Handle(3), [0.1, 0.2, 0.9, 0.8]),
                Call::Width(crate::vr::Handle(3), 0.8),
                Call::Texel(crate::vr::Handle(3), 1.5),
                Call::Curvature(crate::vr::Handle(3), bend(0.8, 1.0)),
                Call::MouseScale(crate::vr::Handle(3), [800.0, 600.0]),
                Call::Device(crate::vr::Handle(3), 0, M),
            ]
        );
    }

    #[test]
    fn the_curve_is_written_as_steamvrs_fraction_and_a_flat_one_as_zero() {
        let mut w = Writes::default();
        apply(&mut w, crate::vr::Handle(1), &Place::new(At::Room(M), 2.0).curve(2.0));
        assert!((w.curvature_of(crate::vr::Handle(1)).unwrap() - 2.0 / (2.0 * std::f32::consts::PI * 2.0)).abs() < 1e-6, "width / (2 pi r)");
        apply(&mut w, crate::vr::Handle(2), &Place::new(At::Room(M), 2.0).curve(0.0));
        assert_eq!(w.curvature_of(crate::vr::Handle(2)), Some(0.0), "a flat panel is told it's flat, so a bent one straightens");
        apply(&mut w, crate::vr::Handle(3), &Place::new(At::Room(M), 20.0).curve(0.5));
        assert_eq!(w.curvature_of(crate::vr::Handle(3)), Some(1.0), "at most a closed cylinder");
    }

    #[test]
    fn the_taskbars_device_number_picks_the_room_or_a_device() {
        let five = Dev::Controller(crate::ids::Ctl(5));
        assert_eq!(At::on(None, M), At::Room(M));
        assert_eq!(At::on(Some(five), M), At::Device(five, M));
        let mut w = Writes::default();
        apply(&mut w, crate::vr::Handle(9), &Place::new(At::on(Some(five), M), 0.4));
        assert_eq!(w.of(crate::vr::Handle(9)).last(), Some(&Call::Device(crate::vr::Handle(9), 5, M)));
        assert_eq!(w.room_of(crate::vr::Handle(9)), None);
    }

    #[test]
    fn each_overlays_writes_stay_its_own() {
        let mut w = Writes::default();
        apply(&mut w, crate::vr::Handle(1), &Place::new(At::Room(M), 1.0));
        apply(&mut w, crate::vr::Handle(2), &Place::new(At::Room(M), 2.0));
        assert_eq!(w.width_of(crate::vr::Handle(1)), Some(1.0));
        assert_eq!(w.width_of(crate::vr::Handle(2)), Some(2.0));
        assert_eq!(w.of(crate::vr::Handle(2)).len(), 2);
    }
}
