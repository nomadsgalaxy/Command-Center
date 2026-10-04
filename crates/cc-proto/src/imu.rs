//! One motion sample from a Deck's controller, the way the agent streams it (docs/agent.md, "IMU
//! stream"), and what a Frame does with it. The host sends raw integers so nothing is lost and the
//! lines stay small. The scales come from the controller's own report: Valve's SDL driver
//! (src/joystick/hidapi/SDL_hidapi_steamdeck.c) reads the accelerometer as +-2 g and the gyro as
//! +-2000 deg/s over 16 bits, and the Linux driver (drivers/hid/hid-steam.c) says the same with
//! 16384 counts per g and 16 counts per deg/s.
//!
//! Axes are the Deck's own. x points to the right edge, y to the top edge (away from you when you hold
//! it) and z out of the screen, which is right-handed. Lying flat and face up, accel reads (0, 0, +1 g).

/// Counts per g of acceleration: the +-2 g range over 16 bits.
pub const ACCEL_PER_G: f64 = 16384.0;
/// Counts per degree per second of rotation: the +-2000 deg/s range over 16 bits.
pub const GYRO_PER_DPS: f64 = 32768.0 / 2000.0;
/// The quaternion's components are fixed point with this as 1.0.
pub const QUAT_ONE: f64 = 32768.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sample {
    /// The controller's own packet counter. It ticks once per report (about every 4 ms), so a gap is a lost sample.
    pub seq: u32,
    /// When the host read it, in microseconds since the stream started (the host's monotonic clock).
    pub t_us: u64,
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
    /// The firmware's fused orientation, w x y z. It maps the Deck's axes to a z-up world with an arbitrary heading.
    pub quat: [i16; 4],
}

impl Sample {
    /// `[seq, t_us, ax, ay, az, gx, gy, gz, qw, qx, qy, qz]`, the row an `imu` event carries.
    pub fn to_row(&self) -> serde_json::Value {
        let [ax, ay, az] = self.accel;
        let [gx, gy, gz] = self.gyro;
        let [qw, qx, qy, qz] = self.quat;
        serde_json::json!([self.seq, self.t_us, ax, ay, az, gx, gy, gz, qw, qx, qy, qz])
    }

    pub fn from_row(v: &serde_json::Value) -> Option<Sample> {
        let a = v.as_array().filter(|a| a.len() == 12)?;
        let i = |k: usize| a[k].as_i64();
        let s = |k: usize| i(k).and_then(|n| i16::try_from(n).ok());
        Some(Sample {
            seq: u32::try_from(i(0)?).ok()?,
            t_us: u64::try_from(i(1)?).ok()?,
            accel: [s(2)?, s(3)?, s(4)?],
            gyro: [s(5)?, s(6)?, s(7)?],
            quat: [s(8)?, s(9)?, s(10)?, s(11)?],
        })
    }

    pub fn accel_g(&self) -> [f64; 3] {
        self.accel.map(|v| v as f64 / ACCEL_PER_G)
    }

    pub fn gyro_dps(&self) -> [f64; 3] {
        self.gyro.map(|v| v as f64 / GYRO_PER_DPS)
    }

    /// The orientation as a unit quaternion [w, x, y, z], or None while the firmware sends zeros (the IMU isn't on yet).
    pub fn quat_unit(&self) -> Option<[f64; 4]> {
        let q = self.quat.map(|v| v as f64 / QUAT_ONE);
        let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
        (n > 0.5).then(|| q.map(|v| v / n))
    }

    /// Heading, pitch and roll in degrees, or None without an orientation:
    /// - yaw: which way the top edge points on the floor plane, counter-clockwise from above, from an arbitrary zero;
    /// - pitch: how far the top edge is raised above the horizon (0 flat, +90 standing on its bottom edge);
    /// - roll: how far the right edge is raised when it's tilted about the top edge (0 flat).
    pub fn euler_deg(&self) -> Option<(f64, f64, f64)> {
        let [w, x, y, z] = self.quat_unit()?;
        let (r01, r11, r21) = (2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x));
        let (r20, r22) = (2.0 * (x * z - w * y), 1.0 - 2.0 * (x * x + y * y));
        Some(((-r01).atan2(r11).to_degrees(), r21.clamp(-1.0, 1.0).asin().to_degrees(), r20.atan2(r22).to_degrees()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured on a Deck lying flat and face up: the quaternion (w x y z) is a half turn about z plus a tilt of under 2 degrees.
    const FLAT: Sample = Sample { seq: 1496899, t_us: 0, accel: [-414, 381, 16377], gyro: [0, 0, 0], quat: [-1666, 420, -276, -32722] };

    #[test]
    fn row_round_trips() {
        let s = Sample { t_us: 123456, ..FLAT };
        assert_eq!(Sample::from_row(&s.to_row()), Some(s));
        assert_eq!(Sample::from_row(&serde_json::json!([1, 2, 3])), None);
        assert_eq!(Sample::from_row(&serde_json::json!([1, 2, 99999, 0, 0, 0, 0, 0, 0, 0, 0, 0])), None);
    }

    #[test]
    fn scales_and_flat_pose() {
        let g = FLAT.accel_g();
        assert!((g[2] - 1.0).abs() < 0.01 && g[0].abs() < 0.05);
        let (_, pitch, roll) = FLAT.euler_deg().unwrap();
        assert!(pitch.abs() < 2.5 && roll.abs() < 2.5, "flat is flat: {pitch} {roll}");
        assert_eq!(Sample::default().euler_deg(), None, "all zeros is no orientation");
        assert!((Sample { gyro: [2000 * 16, 0, 0], ..Default::default() }.gyro_dps()[0] - 2000.0 * 16.0 / GYRO_PER_DPS).abs() < 1e-9);
    }

    #[test]
    fn known_turns() {
        let q = |deg: f64, axis: [f64; 3]| {
            let h = deg.to_radians() / 2.0;
            let c = |v: f64| (v * QUAT_ONE).round() as i16;
            Sample { quat: [c(h.cos().min(0.99997)), c(axis[0] * h.sin()), c(axis[1] * h.sin()), c(axis[2] * h.sin())], ..Default::default() }
        };
        let (yaw, pitch, roll) = q(90.0, [0.0, 0.0, 1.0]).euler_deg().unwrap();
        assert!((yaw - 90.0).abs() < 0.1 && pitch.abs() < 0.1 && roll.abs() < 0.1, "{yaw} {pitch} {roll}");
        let (_, pitch, _) = q(30.0, [1.0, 0.0, 0.0]).euler_deg().unwrap();
        assert!((pitch - 30.0).abs() < 0.1, "top edge up by 30: {pitch}");
        let (_, pitch, roll) = q(-20.0, [0.0, 1.0, 0.0]).euler_deg().unwrap();
        assert!(pitch.abs() < 0.1 && (roll - 20.0).abs() < 0.1, "right edge up by 20: {pitch} {roll}");
    }
}
