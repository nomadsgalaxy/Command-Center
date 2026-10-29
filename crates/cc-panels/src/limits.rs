//! The numbers every surface shares: how big a panel can get, how far a wheel notch pushes it, how
//! much it bends. They used to be typed again wherever they were used, and they aren't all the
//! same: the laser and Right Ctrl push and pull by different steps over different ranges, and
//! pop-outs size to a third range. They stay different here, named, so nothing behaves differently
//! yet. The one-manipulator spec (Planning/SteamVR Window Manipulation.md, rules 4 and 5) merges
//! each pair into one number.
//!
//! The formulas that go with them (`bend`, `chord`, `sagitta`, `radius_for`, `curve_for_width`)
//! are in geometry.rs.

// ------------------------------------------------------------------ size

/// The least and most a panel is across, in metres, by the laser (a corner) or by Right Ctrl +
/// Shift + wheel (kvm.rs `scale`).
pub const MIN_WIDTH: f64 = 0.15;
pub const MAX_WIDTH: f64 = 6.0;

/// A pop-out's width from its window's pixels (popout.rs `tick`): least and most, in metres.
pub const POPOUT_WIDTH: (f64, f64) = (0.1, 4.0);

/// A window panel's text zoom, least and most (1 is the window's own size). Held and pulled with
/// the zoom button, or a notch at a time (`ZOOM_NOTCH`).
pub const ZOOM: (f64, f64) = (0.5, 3.0);
/// Each wheel notch on the zoom button multiplies the zoom by this.
pub const ZOOM_NOTCH: f64 = 1.1;

/// The taskbar's scale, least and most, as a multiple of its natural size. It's a scale, not
/// metres, so it has no `MIN_WIDTH` or `MAX_WIDTH` (the spec makes it metres).
pub const BAR_SCALE: (f64, f64) = (0.5, 3.0);

// ------------------------------------------------------------------ placement

/// A new surface opens this far ahead of the eyes, level and facing you (the spec's rule 9,
/// geometry.rs `new_pose`), and it stays where you then put it.
pub const NEW_AHEAD: f64 = 1.4;

// ------------------------------------------------------------------ theater mode

/// A window panel in theater mode (windows.rs `theater`): this far ahead of the eyes, this wide,
/// curved round this radius. SteamVR's own theater is `defaultTheaterScale` 2.8 at
/// `defaultTheaterDistance` 3.0 m, flat (`theaterCurvature` 0, default.vrsettings). Its scale
/// multiplies a page's own size, so it gives no metres. If SteamVR's scale-1 page is 1.2 m wide
/// (unverified), its theater spans 2·atan(1.68/3.0) = 58.5°, and ours, 2·atan(1.0/1.8) = 58.1°, the
/// same. Ours is nearer and curved where SteamVR's is far and flat; that's how it was built, and
/// matching SteamVR's distance or flatness would change it. New windows are a separate rule.
pub const THEATER: (f64, f64, f64) = (1.8, 2.0, 1.8); // ahead, width, curve radius (metres)

// ------------------------------------------------------------------ push and pull

/// Carrying a panel with the laser: the wheel or joystick moves it along the line from the head
/// by this fraction of its distance a notch, between these distances (metres).
pub const LASER_PUSH_STEP: f64 = 0.08;
pub const LASER_PUSH_RANGE: (f64, f64) = (0.25, 5.0);

/// Right Ctrl + wheel (kvm.rs `scale`): each notch multiplies the distance (or, with Shift, the
/// width) by this, and the distance stays between these (metres).
pub const KEY_NOTCH: f64 = 1.06;
pub const KEY_PUSH_RANGE: (f64, f64) = (0.3, 10.0);

// ------------------------------------------------------------------ curve

/// Bending with the curve button: each wheel notch adds this much depth at the edges (the arc's
/// sagitta, in metres).
pub const CURVE_NOTCH: f64 = 0.03;
/// An arc whose edges come this little toward you (metres) is flat.
pub const FLAT_SAG: f64 = 0.01;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theater_mode_is_as_wide_to_the_eye_as_steamvrs_own() {
        let angle = |half_width: f64, dist: f64| 2.0 * (half_width / dist).atan().to_degrees();
        let (ahead, width, _) = THEATER;
        let steamvr = angle(2.8 * 1.2 / 2.0, 3.0); // defaultTheaterScale 2.8 of a 1.2 m page, at 3 m
        assert!((angle(width / 2.0, ahead) - steamvr).abs() < 1.0, "{} vs {steamvr}", angle(width / 2.0, ahead));
    }
}
