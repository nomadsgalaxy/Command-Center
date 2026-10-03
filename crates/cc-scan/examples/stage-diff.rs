//! `stage-diff <frame.jpg> <dir>` compares each preprocessing stage with OpenCV's dump of the same
//! frame (dir/rgb.raw, gray.raw, blur.raw, clahe.raw, thr<win>.raw) and counts the pixels that differ.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::Path::new(&a[2]);
    let raw = |n: &str| std::fs::read(dir.join(n)).expect(n);
    let data = std::fs::read(&a[1]).unwrap();
    let mut dec = zune_jpeg::JpegDecoder::new(&data);
    let rgb = dec.decode().unwrap();
    let (w, h) = dec.dimensions().unwrap();
    let diff = |name: &str, mine: &[u8], theirs: &[u8]| {
        let n = mine.iter().zip(theirs).filter(|(x, y)| x != y).count();
        let max = mine.iter().zip(theirs).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0);
        println!("{name}: {n} of {} differ (max {max})", mine.len());
    };
    diff("jpeg rgb", &rgb, &raw("rgb.raw"));
    // From here on, each stage takes OpenCV's own previous stage as input, so it's judged on its own.
    let theirs_rgb = raw("rgb.raw");
    let g = cc_scan::image::Gray::from_rgb(w, h, &theirs_rgb);
    diff("gray", &g.px, &raw("gray.raw"));
    let gray = cc_scan::image::Gray { w, h, px: raw("gray.raw") };
    let b = cc_scan::image::gaussian3(&gray);
    diff("blur", &b.px, &raw("blur.raw"));
    let blur = cc_scan::image::Gray { w, h, px: raw("blur.raw") };
    let c = cc_scan::image::clahe(&blur, 3.0, 8);
    diff("clahe", &c.px, &raw("clahe.raw"));
    let cl = cc_scan::image::Gray { w, h, px: raw("clahe.raw") };
    for win in [3, 33, 63] {
        let t = cc_scan::image::threshold_inv(&cl, win, 7.0);
        diff(&format!("threshold {win}"), &t, &raw(&format!("thr{win}.raw")));
        let cs = cc_scan::contours::find(&raw(&format!("thr{win}.raw")), w, h);
        println!("  contours {} points {}", cs.len(), cs.iter().map(|c| c.len()).sum::<usize>());
    }
}
