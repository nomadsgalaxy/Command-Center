//! Gather: every surface we place, brought in front of you, for when some get lost behind you or
//! out of reach. The remote and window panels that are showing, and the Machines, Preferences or
//! Workspace window if one's open, go on an arc limits::NEW_AHEAD from your eyes (where a new
//! window opens), centred on where you're looking, each turned to face you. They keep their size
//! and curve. A row that would be wider than SPAN wraps, and the rows stack above and below the
//! first, at eye height.
//!
//! It does what `summon` does first (nothing minimized, theater off, nothing hidden), and a docked
//! panel is put back in the world. Nothing is saved: where gather puts a panel isn't its home or
//! any spot, so Right Ctrl + Home (`cc-home apply home`) still puts everything back. A panel you
//! then carry and let go of is saved as usual.
//!
//! The taskbar's chip, the dashboard tab's button, Right Ctrl + B, the control socket's `gather`
//! and `cc-home gather` all end here.
use crate::geometry::{Mat, Pose, V3, angles, level_ahead, panel_matrix};
use crate::{Source, grab, panels, vr, windows::Windows};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// How wide a row can be, in degrees round you, before it wraps.
const SPAN: f64 = 100.0;
/// The space between two panels, along a row and between rows, in metres.
const GAP: f64 = 0.04;

/// Asked for from somewhere without the main loop's state (the dashboard tab, Right Ctrl + B):
/// the main loop runs it on its next tick.
pub static ASKED: AtomicBool = AtomicBool::new(false);

/// Asks the main loop for a gather, and wakes it.
pub fn ask() {
    ASKED.store(true, Relaxed);
    vr::wake();
}

/// Where each of `sizes` (width, height in metres) goes for a head at `head`, in order: the first
/// row left to right from the middle of the gaze, then the next rows, one above, one below, and so
/// on. A panel wider than SPAN gets a row to itself.
pub fn layout(head: &Mat, sizes: &[(f64, f64)]) -> Vec<Pose> {
    let r = crate::limits::NEW_AHEAD;
    let eye: V3 = [0, 1, 2].map(|i| head[i][3] as f64);
    let (yaw0, _) = angles(&level_ahead(head));
    let across = |w: f64| (2.0 * (w / (2.0 * r)).atan()).to_degrees(); // a panel's width, in degrees from the eye
    let gap = across(GAP);
    // the rows, as runs of indices
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, &(w, _)) in sizes.iter().enumerate() {
        match rows.last_mut() {
            Some(row) if used + gap + across(w) <= SPAN => {
                row.push(i);
                used += gap + across(w);
            }
            _ => {
                rows.push(vec![i]);
                used = across(w);
            }
        }
    }
    // each row's height off the eyes: the first level with them, then one above, one below, ...
    let tall = |row: &Vec<usize>| row.iter().map(|&i| sizes[i].1).fold(0.0, f64::max);
    let (mut up, mut down) = (tall(&rows.first().cloned().unwrap_or_default()) / 2.0, tall(&rows.first().cloned().unwrap_or_default()) / 2.0);
    let mut out = vec![Pose::default(); sizes.len()];
    for (n, row) in rows.iter().enumerate() {
        let h = tall(row);
        let dy = match n {
            0 => 0.0,
            n if n % 2 == 1 => {
                let y = up + GAP + h / 2.0;
                up = y + h / 2.0;
                y
            }
            _ => {
                let y = down + GAP + h / 2.0;
                down = y + h / 2.0;
                -y
            }
        };
        let width: f64 = row.iter().map(|&i| across(sizes[i].0)).sum::<f64>() + gap * (row.len() - 1) as f64;
        let mut left = yaw0 + width / 2.0; // a bigger yaw is further left
        for &i in row {
            let a = across(sizes[i].0);
            let yaw = (left - a / 2.0).to_radians();
            left -= a + gap;
            let centre = [eye[0] - r * yaw.sin(), eye[1] + dy, eye[2] - r * yaw.cos()];
            let (y, p) = angles(&[centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]]);
            out[i] = Pose { centre, yaw: y, pitch: p, width: sizes[i].0, ..Default::default() };
        }
    }
    out
}

/// What gets gathered.
enum Item {
    Panel(usize),
    Extra(&'static str),
}

/// Brings everything in front of the head now. Returns how many surfaces it moved (0 with the
/// headset untracked: there's no "in front of you" then). The fixed taskbar is the caller's to
/// put back under them (`Taskbar::refix`), as after a spot is applied.
pub fn gather(grab: &mut grab::Grab, windows: &mut Windows) -> usize {
    crate::control::summon(grab, windows);
    let Some(head) = vr::head() else {
        eprintln!("gather: the headset isn't tracked, so nothing moved");
        return 0;
    };
    let (mut items, mut sizes) = (Vec::new(), Vec::new());
    {
        let k = crate::kvm::lock();
        for p in panels().iter().filter(|p| p.live()) {
            items.push(Item::Panel(p.index));
            sizes.push((k.place[p.index].width, k.place[p.index].height));
        }
    }
    if let Some((name, pl)) = grab.extra_shown() {
        items.push(Item::Extra(name));
        sizes.push((pl.width, pl.height));
    }
    let poses = layout(&head, &sizes);
    for (item, pose) in items.iter().zip(&poses) {
        match *item {
            Item::Panel(i) => {
                grab.cancel(i); // carried or docked: it's in the world now
                let p = &panels()[i];
                let mut k = crate::kvm::lock();
                let pl = k.place[i];
                let m = panel_matrix(&Pose { curve: pl.curve, ..*pose });
                match p.src {
                    // a window keeps its height (its window follows, windows.rs); a remote screen's comes from its monitor's shape
                    Source::Window(_) => k.set_place(i, &m, pl.width, pl.height, pl.curve),
                    Source::Rdp => k.set_pose(i, &m, pl.width, pl.curve),
                }
            }
            Item::Extra(name) => {
                grab.cancel(grab.slot());
                let curve = grab.extra_shown().map_or(0.0, |(_, pl)| pl.curve);
                grab.place_extra(name, &panel_matrix(pose), pose.width, curve);
            }
        }
    }
    eprintln!("gather: {} brought in front of you, not saved (Right Ctrl + Home puts them back)", items.len());
    items.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Pose as P, norm};

    /// A head at (0, 1.6, 0) looking along yaw degrees, level.
    fn head(yaw: f64) -> Mat {
        panel_matrix(&P { centre: [0.0, 1.6, 0.0], yaw, ..Default::default() })
    }

    /// Its eyes' height, as the f32 matrix has it.
    fn eye_y() -> f64 {
        head(0.0)[1][3] as f64
    }

    /// Each pose's horizontal span in degrees round the eye, and its vertical span in metres.
    fn spans(poses: &[Pose], sizes: &[(f64, f64)]) -> Vec<((f64, f64), (f64, f64))> {
        let r = crate::limits::NEW_AHEAD;
        poses
            .iter()
            .zip(sizes)
            .map(|(p, &(w, h))| {
                let half = (w / (2.0 * r)).atan().to_degrees();
                ((p.yaw - half, p.yaw + half), (p.centre[1] - h / 2.0, p.centre[1] + h / 2.0))
            })
            .collect()
    }

    fn apart(a: (f64, f64), b: (f64, f64)) -> bool {
        a.1 <= b.0 + 1e-9 || b.1 <= a.0 + 1e-9
    }

    #[test]
    fn three_panels_sit_side_by_side_in_front_facing_you() {
        let sizes = [(0.6, 0.34), (0.9, 0.5), (0.5, 0.4)]; // 24° + 36° + 20° and two gaps: one row
        for yaw in [0.0, 70.0, -150.0] {
            let poses = layout(&head(yaw), &sizes);
            let s = spans(&poses, &sizes);
            for i in 0..3 {
                for j in i + 1..3 {
                    assert!(apart(s[i].0, s[j].0) || apart(s[i].1, s[j].1), "{i} and {j} overlap: {s:?}");
                }
                let d = [poses[i].centre[0], poses[i].centre[1] - eye_y(), poses[i].centre[2]];
                assert!((norm(&[d[0], 0.0, d[2]]) - crate::limits::NEW_AHEAD).abs() < 1e-9, "on the arc");
                let (y, p) = angles(&d);
                assert!((y - poses[i].yaw).abs() < 1e-9 && (p - poses[i].pitch).abs() < 1e-9, "facing the eye");
                assert_eq!(poses[i].width, sizes[i].0, "the same size");
                assert!(poses[i].centre[1] == eye_y() && poses[i].pitch.abs() < 1e-9, "one row: level with the eyes");
            }
            // left to right, and centred on the gaze
            let off = |y: f64| (y - yaw + 540.0).rem_euclid(360.0) - 180.0;
            assert!(off(poses[0].yaw) > off(poses[1].yaw) && off(poses[1].yaw) > off(poses[2].yaw), "in order, from the left");
            let (l, r) = (off(s[0].0.1), off(s[2].0.0));
            assert!((l + r).abs() < 1e-4 && l < SPAN / 2.0, "centred (to the f32 head matrix): {l} and {r}");
        }
    }

    #[test]
    fn too_many_for_the_arc_wrap_into_rows_above_and_below() {
        let sizes = vec![(1.0, 0.6); 7];
        let poses = layout(&head(0.0), &sizes);
        let s = spans(&poses, &sizes);
        for (i, (a, b)) in s.iter().enumerate() {
            assert!(a.0 >= -SPAN / 2.0 - 1e-6 && a.1 <= SPAN / 2.0 + 1e-6, "{i} is within the arc: {a:?}");
            for (j, (c, d)) in s.iter().enumerate().skip(i + 1) {
                assert!(apart(*a, *c) || apart(*b, *d), "{i} and {j} overlap");
            }
            let _ = b;
        }
        let ys: Vec<f64> = poses.iter().map(|p| p.centre[1]).collect();
        let mut rows: Vec<f64> = ys.clone();
        rows.sort_by(f64::total_cmp);
        rows.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        // a metre is 39° at 1.4 m, so two fit in 100° and seven make four rows
        assert_eq!(rows.len(), 4, "7 one-metre panels make four rows: {ys:?}");
        assert!(ys[0] == eye_y() && ys[1] == eye_y(), "the first row is at eye height");
        assert!(ys[2] > eye_y() && ys[2] == ys[3], "the next row goes above");
        assert!(ys[4] < eye_y() && ys[4] == ys[5], "the one after below");
        assert!(ys[6] > ys[2], "and the fourth above the second");
        for p in &poses {
            let d = [p.centre[0], p.centre[1] - eye_y(), p.centre[2]];
            let (y, pt) = angles(&d);
            assert!((y - p.yaw).abs() < 1e-9 && (pt - p.pitch).abs() < 1e-9, "every row faces the eye");
        }
    }

    #[test]
    fn a_panel_wider_than_the_arc_gets_a_row_to_itself() {
        let sizes = [(0.5, 0.3), (4.0, 1.0), (0.5, 0.3)];
        let poses = layout(&head(0.0), &sizes);
        let ys: Vec<f64> = poses.iter().map(|p| p.centre[1]).collect();
        assert!(ys[0] != ys[1] && ys[1] != ys[2], "three rows: {ys:?}");
        assert!(poses[1].yaw.abs() < 1e-9, "centred on the gaze");
    }

    #[test]
    fn nothing_to_gather_is_nothing() {
        assert!(layout(&head(0.0), &[]).is_empty());
    }
}
