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
use crate::geometry::{Mat, Placement, Pose, V3, angles, panel_matrix, sagitta};
use crate::stage::Stage;
use crate::{Source, grab, panels, vr, windows::Windows};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// How wide a row can be, in degrees round you, before it wraps.
const SPAN: f64 = 100.0;
/// Past this many rows the arc widens instead (WIDER), so a big desktop doesn't stack up out of
/// sight above and below you.
const ROWS: usize = 3;
const WIDER: [f64; 2] = [160.0, 220.0];
/// The space between two panels, along a row and between rows, in metres.
const GAP: f64 = 0.04;

/// Asked for from somewhere without the main loop's state (the dashboard tab, Right Ctrl + B):
/// the main loop runs it on its next tick. `UNDO_ASKED` is the ungather key's
/// (kvm.rs, Left Ctrl + Shift + B unless settings.json says otherwise) or Right Ctrl + Shift + B's.
pub static ASKED: AtomicBool = AtomicBool::new(false);
pub static UNDO_ASKED: AtomicBool = AtomicBool::new(false);

/// Asks the main loop for a gather (or, with `undo`, an ungather), and wakes it.
pub fn ask(undo: bool) {
    if undo { &UNDO_ASKED } else { &ASKED }.store(true, Relaxed);
    vr::wake();
}

/// What gets gathered.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Item {
    Panel(usize),
    Extra(&'static str),
}

/// The panels gather put where they are now, by index. A window panel's place isn't kept for it
/// while it's in here (windows.rs `keep_pose`), so a gather is never saved. Carrying one and letting
/// go (grab.rs `save`), a spot or `place`, or an ungather takes it out.
static GATHERED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Where everything was before the last gather, for `ungather`.
static BEFORE: Mutex<Vec<(Item, Placement)>> = Mutex::new(Vec::new());

/// Whether panel i is where a gather put it, and not saved.
pub fn gathered(i: usize) -> bool {
    crate::ffi::lock(&GATHERED).contains(&i)
}

/// Panel i was put somewhere on purpose (a carry let go of, a spot, `place`), so it's the user's
/// place again, saved as usual, and an ungather leaves it be.
pub fn placed(i: usize) {
    crate::ffi::lock(&GATHERED).retain(|&j| j != i);
    crate::ffi::lock(&BEFORE).retain(|(it, _)| *it != Item::Panel(i));
}

/// The level direction ahead of the head. Looking straight up or down, its -z has no level part,
/// so the top of the head stands in: it points behind you looking up, and ahead looking down.
fn forward(head: &Mat) -> V3 {
    let level = |v: [f64; 3]| {
        let n = v[0].hypot(v[2]);
        (n > 0.1).then(|| [v[0] / n, 0.0, v[2] / n])
    };
    let ahead = [-head[0][2] as f64, -head[1][2] as f64, -head[2][2] as f64];
    let up = [head[0][1] as f64, head[1][1] as f64, head[2][1] as f64];
    let s = if ahead[1] > 0.0 { -1.0 } else { 1.0 };
    level(ahead).or_else(|| level(up.map(|v| v * s))).unwrap_or([0.0, 0.0, -1.0])
}

/// Where each of `sizes` (width and height in metres, and the curve radius, 0 flat) goes for a head
/// at `head`, in order: the first row left to right from the middle of the gaze, then the next
/// rows, one above, one below, and so on. A panel wider than the arc gets a row to itself. If it
/// would take more than ROWS rows, the arc widens.
pub fn layout(head: &Mat, sizes: &[(f64, f64, f64)]) -> Vec<Pose> {
    let span = [SPAN].into_iter().chain(WIDER).find(|&s| rows_for(sizes, s).len() <= ROWS).unwrap_or(WIDER[WIDER.len() - 1]);
    place_rows(head, sizes, &rows_for(sizes, span))
}

/// A panel's width in degrees from the eye, NEW_AHEAD away. A curved one's edges come toward you
/// (its sagitta), and it's the chord between them that you see.
fn across(w: f64, curve: f64) -> f64 {
    let r = crate::limits::NEW_AHEAD;
    let half = if curve > 0.0 { curve * (w / (2.0 * curve)).min(std::f64::consts::FRAC_PI_2).sin() } else { w / 2.0 };
    (2.0 * (half / (r - sagitta(w, curve)).max(0.1)).atan()).to_degrees()
}

/// The rows, as runs of indices, packed in order into `span` degrees.
fn rows_for(sizes: &[(f64, f64, f64)], span: f64) -> Vec<Vec<usize>> {
    let gap = across(GAP, 0.0);
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, &(w, _, c)) in sizes.iter().enumerate() {
        match rows.last_mut() {
            Some(row) if used + gap + across(w, c) <= span => {
                row.push(i);
                used += gap + across(w, c);
            }
            _ => {
                rows.push(vec![i]);
                used = across(w, c);
            }
        }
    }
    rows
}

fn place_rows(head: &Mat, sizes: &[(f64, f64, f64)], rows: &[Vec<usize>]) -> Vec<Pose> {
    let r = crate::limits::NEW_AHEAD;
    let eye: V3 = [0, 1, 2].map(|i| head[i][3] as f64);
    let (yaw0, _) = angles(&forward(head));
    let gap = across(GAP, 0.0);
    // each row's height off the eyes: the first level with them, then one above, one below, ...
    let tall = |row: &[usize]| row.iter().map(|&i| sizes[i].1).fold(0.0, f64::max);
    let first = rows.first().map_or(0.0, |row| tall(row)) / 2.0;
    let (mut up, mut down) = (first, first);
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
        let width: f64 = row.iter().map(|&i| across(sizes[i].0, sizes[i].2)).sum::<f64>() + gap * (row.len() - 1) as f64;
        let mut left = yaw0 + width / 2.0; // a bigger yaw is further left
        for &i in row {
            let a = across(sizes[i].0, sizes[i].2);
            let yaw = (left - a / 2.0).to_radians();
            left -= a + gap;
            let centre = [eye[0] - r * yaw.sin(), eye[1] + dy, eye[2] - r * yaw.cos()];
            let (y, p) = angles(&[centre[0] - eye[0], centre[1] - eye[1], centre[2] - eye[2]]);
            out[i] = Pose { centre, yaw: y, pitch: p, width: sizes[i].0, curve: sizes[i].2, ..Default::default() };
        }
    }
    out
}

/// Puts panel or Extra `item` at `pl` (its size and curve kept from `pl`).
fn put(grab: &mut grab::Grab, item: Item, m: &Mat, pl: &Placement) {
    match item {
        Item::Panel(i) => {
            grab.cancel(i); // carried or docked: it's in the world now
            let mut k = crate::kvm::lock();
            match panels()[i].src {
                // a window keeps its height (its window follows, windows.rs); a remote screen's comes from its monitor's shape
                Source::Window(_) => k.set_place(i, m, pl.width, pl.height, pl.curve),
                Source::Rdp => k.set_pose(i, m, pl.width, pl.curve),
            }
        }
        Item::Extra(name) => {
            grab.cancel(grab.slot());
            grab.place_extra(name, m, pl.width, pl.curve);
        }
    }
}

/// Brings everything in front of the head now, and returns how many surfaces it moved. With the
/// headset untracked there's no "in front of you", so it changes nothing, summon included, and
/// says so. Popups aren't moved: they follow their window (windows.rs). The fixed taskbar is the
/// caller's to put back under them (`Taskbar::refix`), as after a spot is applied.
pub fn gather(grab: &mut grab::Grab, windows: &mut Windows) -> Result<usize, String> {
    let head = crate::stage::Live.head_now().ok_or("the headset isn't tracked, so nothing moved")?;
    crate::control::summon(grab, windows);
    let mut items: Vec<(Item, Placement)> = Vec::new();
    {
        let k = crate::kvm::lock();
        for p in panels().iter().filter(|p| p.live() && !crate::windows::is_popup(p.index)) {
            items.push((Item::Panel(p.index), k.place[p.index]));
        }
    }
    if let Some((name, pl)) = grab.extra_shown() {
        items.push((Item::Extra(name), pl));
    }
    let sizes: Vec<(f64, f64, f64)> = items.iter().map(|(_, pl)| (pl.width, pl.height, pl.curve)).collect();
    let poses = layout(&head, &sizes);
    for ((item, pl), pose) in items.iter().zip(&poses) {
        put(grab, *item, &panel_matrix(pose), pl);
    }
    *crate::ffi::lock(&GATHERED) = items.iter().filter_map(|(it, _)| if let Item::Panel(i) = it { Some(*i) } else { None }).collect();
    *crate::ffi::lock(&BEFORE) = items.clone();
    eprintln!("gather: {} brought in front of you, not saved (the ungather key, Left Ctrl + Shift + B unless set, or Right Ctrl + Home puts them back)", items.len());
    Ok(items.len())
}

/// Puts back what the last gather moved, where it was before (one level of undo). A panel moved
/// since, on purpose, stays where it's been put. Returns how many went back.
pub fn ungather(grab: &mut grab::Grab) -> Result<usize, String> {
    let before = std::mem::take(&mut *crate::ffi::lock(&BEFORE));
    if before.is_empty() {
        return Err("nothing gathered to put back".into());
    }
    let mut n = 0;
    for (item, pl) in &before {
        if let Item::Panel(i) = item
            && !panels().get(*i).is_some_and(|p| p.live())
        {
            continue; // gone since
        }
        put(grab, *item, &pl.matrix(), pl);
        n += 1;
    }
    crate::ffi::lock(&GATHERED).clear();
    eprintln!("ungather: {n} put back where they were");
    Ok(n)
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
    fn spans(poses: &[Pose], sizes: &[(f64, f64, f64)]) -> Vec<((f64, f64), (f64, f64))> {
        poses
            .iter()
            .zip(sizes)
            .map(|(p, &(w, h, c))| {
                let half = across(w, c) / 2.0;
                ((p.yaw - half, p.yaw + half), (p.centre[1] - h / 2.0, p.centre[1] + h / 2.0))
            })
            .collect()
    }

    fn apart(a: (f64, f64), b: (f64, f64)) -> bool {
        a.1 <= b.0 + 1e-9 || b.1 <= a.0 + 1e-9
    }

    #[test]
    fn three_panels_sit_side_by_side_in_front_facing_you() {
        let sizes = [(0.6, 0.34, 0.0), (0.9, 0.5, 0.0), (0.5, 0.4, 0.0)]; // 24° + 36° + 20° and two gaps: one row
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
        let sizes = vec![(1.0, 0.6, 0.0); 5]; // two to a 100° row: three rows
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
        // a metre is 39° at 1.4 m, so two fit in 100° and five make three rows
        assert_eq!(rows.len(), 3, "5 one-metre panels make three rows: {ys:?}");
        assert!(ys[0] == eye_y() && ys[1] == eye_y(), "the first row is at eye height");
        assert!(ys[2] > eye_y() && ys[2] == ys[3], "the next row goes above");
        assert!(ys[4] < eye_y(), "and the one after below");
        for p in &poses {
            let d = [p.centre[0], p.centre[1] - eye_y(), p.centre[2]];
            let (y, pt) = angles(&d);
            assert!((y - p.yaw).abs() < 1e-9 && (pt - p.pitch).abs() < 1e-9, "every row faces the eye");
        }
    }

    #[test]
    fn a_panel_wider_than_the_arc_gets_a_row_to_itself() {
        let sizes = [(0.5, 0.3, 0.0), (4.0, 1.0, 0.0), (0.5, 0.3, 0.0)];
        let poses = layout(&head(0.0), &sizes);
        let ys: Vec<f64> = poses.iter().map(|p| p.centre[1]).collect();
        assert!(ys[0] != ys[1] && ys[1] != ys[2], "three rows: {ys:?}");
        assert!(poses[1].yaw.abs() < 1e-9, "centred on the gaze");
    }

    #[test]
    fn nothing_to_gather_is_nothing() {
        assert!(layout(&head(0.0), &[]).is_empty());
    }

    #[test]
    fn looking_straight_up_or_down_still_has_an_ahead() {
        // half a degree off straight up or down: what's ahead has almost no level part
        let up = panel_matrix(&P { centre: [0.0, 1.6, 0.0], yaw: 30.0, pitch: 89.5, ..Default::default() });
        let down = panel_matrix(&P { centre: [0.0, 1.6, 0.0], yaw: 30.0, pitch: -89.5, ..Default::default() });
        for h in [up, down] {
            let f = forward(&h);
            assert!((f[0].hypot(f[2]) - 1.0).abs() < 1e-6 && f[1] == 0.0, "a level unit direction: {f:?}");
            assert!((angles(&f).0 - 30.0).abs() < 1.0, "the way the face was turned: {f:?}");
            let p = layout(&h, &[(1.0, 0.6, 0.0)]);
            assert!(p[0].centre.iter().all(|v| v.is_finite()), "{p:?}");
        }
    }

    #[test]
    fn more_rows_than_three_widen_the_arc_instead() {
        let sizes = vec![(1.0, 0.6, 0.0); 9]; // five rows of two at 100°: it widens
        let poses = layout(&head(0.0), &sizes);
        let mut ys: Vec<f64> = poses.iter().map(|p| p.centre[1]).collect();
        ys.sort_by(f64::total_cmp);
        ys.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        assert!(ys.len() <= ROWS, "nine take at most {ROWS} rows on a wider arc: {ys:?}");
        let s = spans(&poses, &sizes);
        assert!(s.iter().any(|(a, _)| a.0 < -SPAN / 2.0 || a.1 > SPAN / 2.0), "and the arc's wider than 100°: {s:?}");
        for i in 0..9 {
            for j in i + 1..9 {
                assert!(apart(s[i].0, s[j].0) || apart(s[i].1, s[j].1), "{i} and {j} overlap");
            }
        }
    }

    #[test]
    fn a_curved_panel_takes_the_angle_of_its_chord() {
        // curved, its chord is shorter, but its edges come toward you, so it takes more of the view
        let (half, sag) = (1.4 * 0.5f64.sin(), 1.4 * (1.0 - 0.5f64.cos()));
        assert!((across(1.4, 1.4) - 2.0 * (half / (crate::limits::NEW_AHEAD - sag)).atan().to_degrees()).abs() < 1e-9);
        assert!(across(1.4, 1.4) > across(1.4, 0.0));
        assert!((across(1.0, 0.0) - 2.0 * (0.5f64 / crate::limits::NEW_AHEAD).atan().to_degrees()).abs() < 1e-12);
        let poses = layout(&head(0.0), &[(1.4, 0.6, 1.4), (1.4, 0.6, 1.4)]);
        assert_eq!(poses[0].curve, 1.4, "it keeps its curve");
    }

    /// With the headset untracked a gather changes nothing, summon included, and says why; the
    /// socket's `gather` answers that as an error, and with nothing gathered `ungather` does too.
    /// (A tracked gather walks the panel list, which a test has no way to make yet.)
    #[test]
    fn gather_needs_the_head_and_ungather_needs_a_gather() {
        let _vr = crate::trace::fake_vr();
        let mut windows = Windows::for_test(0);
        let mut grab = grab::Grab::with_backdrop(0, crate::vr::Handle::NONE);
        let dir = std::env::temp_dir().join(format!("cc-gather-test-{}", std::process::id()));
        let mut bar = crate::taskbar::Taskbar::new(dir.to_str().unwrap());
        crate::HIDDEN.store(true, Relaxed);
        assert!(gather(&mut grab, &mut windows).unwrap_err().contains("isn't tracked"));
        assert!(crate::HIDDEN.load(Relaxed), "nothing changed: summon would have shown the panels");
        assert!(crate::control::command("gather", &mut grab, &mut windows, &mut bar).starts_with("error the headset isn't tracked"));
        crate::ffi::lock(&BEFORE).clear();
        assert_eq!(crate::control::command("ungather", &mut grab, &mut windows, &mut bar), "error nothing gathered to put back");
        crate::HIDDEN.store(false, Relaxed);
    }

    fn pl0() -> Placement {
        Placement::from_matrix(&head(0.0), 1.0, 0.5625, 0.0)
    }

    #[test]
    fn a_gathered_panel_is_its_own_place_again_once_put_somewhere() {
        *crate::ffi::lock(&GATHERED) = vec![2, 5];
        *crate::ffi::lock(&BEFORE) = vec![(Item::Panel(2), pl0()), (Item::Panel(5), pl0())];
        assert!(gathered(2) && gathered(5) && !gathered(3));
        placed(5); // carried and let go of, or a spot
        assert!(gathered(2) && !gathered(5));
        assert_eq!(crate::ffi::lock(&BEFORE).len(), 1, "and an ungather leaves it where it was put");
        placed(2);
        assert!(crate::ffi::lock(&BEFORE).is_empty() && crate::ffi::lock(&GATHERED).is_empty());
    }
}
