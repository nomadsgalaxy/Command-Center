//! `aruco-compare <ref.json>` runs the Rust detector on every recorded frame in ref.json and
//! compares it with what OpenCV (scan.py's detect, in the container) found in the same JPEG. It
//! reports the ids found by both or by only one, and how far apart the shared tags' corners are.
use serde_json::Value;

fn main() {
    let path = std::env::args().nth(1).expect("ref.json");
    let refs: Value = serde_json::from_slice(&std::fs::read(path).expect("ref.json")).expect("JSON");
    let (mut both, mut only_cv, mut only_rs, mut n) = (0, 0, 0, 0);
    let mut errs: Vec<f32> = vec![];
    let mut worst: Vec<(f32, String, usize)> = vec![];
    let t0 = std::time::Instant::now();
    for (file, r) in refs.as_object().unwrap() {
        let data = std::fs::read(file).expect("frame");
        let gray = if file.ends_with(".gray") {
            cc_scan::image::Gray { w: 1920, h: 1080, px: data } // OpenCV's own decode, so the detector is judged on its own.
        } else {
            let mut dec = zune_jpeg::JpegDecoder::new(&data);
            let rgb = dec.decode().expect("jpeg");
            let (w, h) = dec.dimensions().unwrap();
            cc_scan::image::Gray::from_rgb(w, h, &rgb)
        };
        n += 1;
        for (kind, dict) in [("aruco", cc_scan::dict::DICT_4X4_250), ("key", cc_scan::dict::DICT_4X4_1000)] {
            let mine = cc_scan::detect(&gray, &dict);
            let theirs = r[kind].as_array().unwrap();
            for t in theirs {
                let id = t["id"].as_u64().unwrap() as usize;
                let c = t["corners"].as_array().unwrap();
                let centre = |k: usize| (0..4).map(|j| c[j][k].as_f64().unwrap() as f32).sum::<f32>() / 4.0;
                let (cx, cy) = (centre(0), centre(1));
                let near = |m: &&cc_scan::Tag| {
                    let (mx, my) = (m.corners.iter().map(|p| p.0).sum::<f32>() / 4.0, m.corners.iter().map(|p| p.1).sum::<f32>() / 4.0);
                    (mx - cx).powi(2) + (my - cy).powi(2)
                };
                match mine.iter().filter(|m| m.id == id).min_by(|a, b| near(a).partial_cmp(&near(b)).unwrap()) {
                    Some(m) if near(&m) < 400.0 => {
                        both += 1;
                        let e = (0..4).map(|k| {
                            let (x, y) = (c[k][0].as_f64().unwrap() as f32, c[k][1].as_f64().unwrap() as f32);
                            ((m.corners[k].0 - x).powi(2) + (m.corners[k].1 - y).powi(2)).sqrt()
                        }).fold(0f32, f32::max);
                        errs.push(e);
                        worst.push((e, file.rsplit('/').next().unwrap().to_owned(), id));
                    }
                    _ => only_cv += 1,
                }
            }
            only_rs += mine.iter().filter(|m| !theirs.iter().any(|t| t["id"].as_u64() == Some(m.id as u64))).count();
        }
    }
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let pct = |q: f64| errs.get(((errs.len() as f64 - 1.0) * q) as usize).copied().unwrap_or(0.0);
    println!("{n} frames in {:.1} s: {both} tags found by both, {only_cv} only by OpenCV, {only_rs} only by Rust", t0.elapsed().as_secs_f64());
    println!("corner distance (worst corner per tag, px): median {:.3}, p90 {:.3}, p99 {:.3}, max {:.3}", pct(0.5), pct(0.9), pct(0.99), pct(1.0));
    for w in worst.iter().take(5) {
        println!("  {:.2} px: {} id {}", w.0, w.1, w.2);
    }
}
