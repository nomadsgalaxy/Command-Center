//! A Steam Deck's motion sensors, read through hidraw (docs/deck-tracking.md, docs/agent.md "IMU stream").
//!
//! The Deck's controller (Valve 28de:1205) shows up as three hidraw nodes, a keyboard, a mouse and
//! the controller. Only the controller's report descriptor has a feature report, so that's how it's
//! found (the Linux driver, drivers/hid/hid-steam.c, tells them apart the same way). It sends a
//! 64-byte "deck state" report about every 4 ms, with no report id. The layout is in Valve's
//! SDL driver (src/joystick/hidapi/SDL_hidapi_steamdeck.c, steam/controller_structs.h
//! SteamDeckStatePacket_t) and hid-steam.c's steam_deck_imu_mappings:
//!
//!   0-1 u16 version (1)   2 u8 type (9 = deck state)   3 u8 length (64)   4-7 u32 packet counter
//!   24 s16 accel x,y,z (+-2 g)   30 s16 gyro x,y,z (+-2000 deg/s)   36 s16 quaternion w,x,y,z
//!
//! The motion fields stay zero until a setting turns the IMU on. That's a feature report, 0x87
//! (SET_SETTINGS_VALUES) with setting 48 (IMU_MODE) and a bit mask (steam/controller_constants.h
//! SETTING_GYRO_MODE_*: 4 orientation, 8 raw accel, 16 raw gyro). 0x89 reads a setting back.
//! The Steam client reads the same node, and hidraw lets both read, so this touches as little as it can: it
//! remembers the setting, turns on only the bits it needs, and puts the old value back when it
//! stops, but only if nobody changed the setting in the meantime.
use cc_proto::imu::Sample;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SET_SETTINGS: u8 = 0x87;
const GET_SETTINGS: u8 = 0x89;
const SETTING_IMU_MODE: u8 = 48;
const ORIENTATION: u16 = 0x04;
const RAW_ACCEL: u16 = 0x08;
const RAW_GYRO: u16 = 0x10;
const WANT: u16 = ORIENTATION | RAW_ACCEL | RAW_GYRO;
const REPORT: usize = 64;
/// The nominal rate of the controller's reports.
pub const DEVICE_HZ: u32 = 250;

/// Whether this is the Deck's controller: a report descriptor with a feature report and no report ids.
pub fn is_controller(desc: &[u8]) -> bool {
    let (mut i, mut feature, mut ids) = (0, false, false);
    while i < desc.len() {
        let b = desc[i];
        let size = match b & 3 {
            3 => 4,
            n => n as usize,
        };
        match b & 0xfc {
            0xb0 => feature = true, // Feature
            0x84 => ids = true,     // Report ID
            _ => {}
        }
        i += 1 + size;
    }
    feature && !ids
}

/// Finds the Deck's controller under a sysfs class directory (/sys/class/hidraw) and returns its /dev node.
pub fn find_in(class: &Path) -> Option<PathBuf> {
    let mut nodes: Vec<_> = std::fs::read_dir(class).ok()?.flatten().collect();
    nodes.sort_by_key(|e| e.file_name());
    nodes.into_iter().find_map(|e| {
        let dev = e.path().join("device");
        let uevent = std::fs::read_to_string(dev.join("uevent")).ok()?;
        let id = uevent.lines().find_map(|l| l.strip_prefix("HID_ID="))?.to_ascii_lowercase();
        let mut p = id.split(':');
        let (bus, vid, pid) = (p.next()?, u32::from_str_radix(p.next()?, 16).ok()?, u32::from_str_radix(p.next()?, 16).ok()?);
        (bus == "0003" && vid == 0x28de && pid == 0x1205 && is_controller(&std::fs::read(dev.join("report_descriptor")).ok()?))
            .then(|| PathBuf::from("/dev").join(e.file_name()))
    })
}

/// The Deck's controller on this machine, if there is one.
pub fn find() -> Option<PathBuf> {
    find_in(Path::new("/sys/class/hidraw"))
}

/// Reads one deck-state report: the packet counter and the motion fields, or None for any other report.
pub fn decode(r: &[u8]) -> Option<Sample> {
    if r.len() != REPORT || r[2] != 9 || r[3] != REPORT as u8 {
        return None;
    }
    let s = |o: usize| i16::from_le_bytes([r[o], r[o + 1]]);
    Some(Sample {
        seq: u32::from_le_bytes([r[4], r[5], r[6], r[7]]),
        t_us: 0,
        accel: [s(24), s(26), s(28)],
        gyro: [s(30), s(32), s(34)],
        quat: [s(36), s(38), s(40), s(42)],
    })
}

/// HIDIOCSFEATURE and HIDIOCGFEATURE for a 65-byte buffer (report id 0, then 64 bytes): _IOC(read|write, 'H', 6 or 7, 65).
fn ioc(nr: u64) -> libc::Ioctl {
    ((3u64 << 30) | (65 << 16) | (0x48 << 8) | nr) as libc::Ioctl
}

fn feature(f: &File, nr: u64, buf: &mut [u8; 65]) -> std::io::Result<()> {
    if unsafe { libc::ioctl(f.as_raw_fd(), ioc(nr), buf.as_mut_ptr()) } < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

fn set_setting(f: &File, num: u8, val: u16) -> std::io::Result<()> {
    let mut b = [0u8; 65];
    b[1..6].copy_from_slice(&[SET_SETTINGS, 3, num, val as u8, (val >> 8) as u8]);
    feature(f, 6, &mut b)
}

fn get_setting(f: &File, num: u8) -> std::io::Result<u16> {
    let mut b = [0u8; 65];
    b[1..4].copy_from_slice(&[GET_SETTINGS, 1, num]);
    feature(f, 6, &mut b)?;
    let mut r = [0u8; 65];
    feature(f, 7, &mut r)?;
    if r[1] != GET_SETTINGS || r[3] != num {
        return Err(std::io::Error::other("the controller didn't answer the setting read"));
    }
    Ok(u16::from_le_bytes([r[4], r[5]]))
}

/// An open controller with its IMU on. Dropping it puts the IMU setting back the way it was.
pub struct Imu {
    f: File,
    before: u16,
    ours: u16,
}

impl Imu {
    pub fn open(path: &Path) -> std::io::Result<Imu> {
        let f = OpenOptions::new().read(true).write(true).open(path)?;
        let before = get_setting(&f, SETTING_IMU_MODE)?;
        let ours = before | WANT;
        let mut imu = Imu { f, before, ours: before };
        if ours != before {
            set_setting(&imu.f, SETTING_IMU_MODE, ours)?;
            imu.ours = ours;
        }
        Ok(imu)
    }

    /// Turns the IMU on again if something (Steam, between games) turned it off.
    fn reassert(&self) {
        let _ = set_setting(&self.f, SETTING_IMU_MODE, self.ours | WANT);
    }
}

impl Drop for Imu {
    fn drop(&mut self) {
        if self.ours != self.before && get_setting(&self.f, SETTING_IMU_MODE).is_ok_and(|now| now & WANT == WANT) {
            let _ = set_setting(&self.f, SETTING_IMU_MODE, self.before);
        }
    }
}

fn poll(f: &File, ms: i32) -> bool {
    let mut p = libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}

/// Reads reports until `emit` says stop (false) or `stop` is set, and hands over batches about `hz`
/// times a second. Samples carry the host's time since this began. ponytail: it reads on this
/// thread's clock only; nothing here estimates the gap between the read and the sensor's own sample.
pub fn run(imu: &mut Imu, hz: u32, stop: &std::sync::atomic::AtomicBool, mut emit: impl FnMut(Vec<Sample>) -> bool) {
    use std::sync::atomic::Ordering::Relaxed;
    let (t0, period) = (Instant::now(), Duration::from_micros(1_000_000 / hz.max(1) as u64));
    let (mut batch, mut due, mut dead_since, mut last_kick) = (Vec::new(), Instant::now() + period, None::<Instant>, Instant::now());
    let mut buf = [0u8; 128];
    while !stop.load(Relaxed) {
        if poll(&imu.f, 20) {
            let Ok(n) = imu.f.read(&mut buf) else { break };
            if let Some(mut s) = decode(&buf[..n]) {
                s.t_us = t0.elapsed().as_micros() as u64;
                // An IMU that's off sends zeros, and gravity means a real one never reads all zeros.
                if s.accel == [0; 3] {
                    let since = *dead_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > Duration::from_millis(500) && last_kick.elapsed() > Duration::from_secs(1) {
                        last_kick = Instant::now();
                        imu.reassert();
                    }
                } else {
                    dead_since = None;
                }
                batch.push(s);
            }
        }
        if Instant::now() >= due {
            due += period;
            if due < Instant::now() {
                due = Instant::now() + period;
            }
            if !batch.is_empty() && !emit(std::mem::take(&mut batch)) {
                break;
            }
        }
    }
}

/// `cc-host imu-probe [seconds]`: turns the IMU on, prints what it reads twice a second, and puts the setting back.
pub fn probe(secs: f64) -> i32 {
    let Some(path) = find() else {
        eprintln!("cc-host imu-probe: no Steam Deck controller here");
        return 1;
    };
    let mut imu = match Imu::open(&path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cc-host imu-probe: {}: {e}", path.display());
            return 1;
        }
    };
    println!("{}: IMU_MODE was {:#x}, now {:#x}", path.display(), imu.before, imu.ours);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let (start, mut total, mut first) = (Instant::now(), 0usize, None::<Sample>);
    let mut last = Sample::default();
    run(&mut imu, 2, &stop, |b| {
        total += b.len();
        first.get_or_insert(b[0]);
        last = *b.last().unwrap();
        let (g, w) = (last.accel_g(), last.gyro_dps());
        let e = last.euler_deg().map_or("no orientation".into(), |(y, p, r)| format!("yaw {y:+7.1} pitch {p:+6.1} roll {r:+6.1}"));
        println!("seq {} t {:.2}s  accel {:+.3} {:+.3} {:+.3} g  gyro {:+.2} {:+.2} {:+.2} deg/s  {e}  raw quat {:?}", last.seq, last.t_us as f64 / 1e6, g[0], g[1], g[2], w[0], w[1], w[2], last.quat);
        start.elapsed().as_secs_f64() < secs
    });
    let first = first.unwrap_or_default();
    println!("{total} reports in {:.1}s, {:.0}/s, counter advanced {} (a gap means lost reports)", start.elapsed().as_secs_f64(), total as f64 / start.elapsed().as_secs_f64(), last.seq.wrapping_sub(first.seq) + 1);
    let before = imu.before;
    drop(imu);
    if let Ok(f) = File::open(&path).and_then(|_| OpenOptions::new().read(true).write(true).open(&path)) {
        println!("IMU_MODE restored to {:#x} (read back: {:#x})", before, get_setting(&f, SETTING_IMU_MODE).unwrap_or(0xffff));
    }
    let _ = std::io::stdout().flush();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // A Deck's three hidraw report descriptors, as read from sysfs.
    const KEYBOARD: &str = "05010906a101050719e029e715002501750195088102810119002965150025657508950681 00c0";
    const MOUSE: &str = "05010902a1010901a10005091901290215002501750195028102750695018101050109300931158125 7f75089502810695010938810605 0c0a38029501810 6c0c0";
    const CONTROLLER: &str = "06ffff0901a10115002 6ff00750895400901810209 01b102c0";

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn only_the_controller_has_a_feature_report() {
        assert!(is_controller(&hex(CONTROLLER)));
        assert!(!is_controller(&hex(KEYBOARD)));
        assert!(!is_controller(&hex(MOUSE)));
    }

    #[test]
    fn finds_the_node_by_id_not_by_number() {
        let d = std::env::temp_dir().join(format!("cc-imu-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (n, id, desc) in [("hidraw0", "0003:000028DE:00001205", MOUSE), ("hidraw1", "0003:0000046D:0000C52B", CONTROLLER),
                              ("hidraw2", "0003:000028DE:00001205", KEYBOARD), ("hidraw7", "0003:000028DE:00001205", CONTROLLER)] {
            let dev = d.join(n).join("device");
            std::fs::create_dir_all(&dev).unwrap();
            std::fs::write(dev.join("uevent"), format!("DRIVER=hid-generic\nHID_ID={id}\nHID_NAME=x\n")).unwrap();
            std::fs::write(dev.join("report_descriptor"), hex(desc)).unwrap();
        }
        assert_eq!(find_in(&d), Some(PathBuf::from("/dev/hidraw7")));
        assert_eq!(find_in(&d.join("nothing")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A report captured on a Deck lying flat with the IMU on (counter 1496899).
    fn captured() -> Vec<u8> {
        let mut r = hex("0100 0940 43d0 1600");
        r.resize(24, 0);
        r.extend(hex("62fe 7d01 f93f 0000 0000 0000 7ef9 a401 ecfe 2e80 0000 0000 8b05 1b01"));
        r.resize(64, 0);
        r
    }

    #[test]
    fn decodes_a_captured_report() {
        let s = decode(&captured()).unwrap();
        assert_eq!((s.seq, s.accel, s.gyro, s.quat), (0x16d043, [-414, 381, 16377], [0, 0, 0], [-1666, 420, -276, -32722]));
        assert!((s.accel_g()[2] - 1.0).abs() < 0.01);
    }

    #[test]
    fn ignores_other_reports() {
        let mut r = captured();
        r[2] = 1;
        assert_eq!(decode(&r), None);
        assert_eq!(decode(&captured()[..63]), None);
        let off = hex("0100 0940 31cf 1600");
        let mut z = off.clone();
        z.resize(64, 0);
        assert_eq!(decode(&z).unwrap().accel, [0, 0, 0], "an IMU that's off decodes as zeros");
    }
}
