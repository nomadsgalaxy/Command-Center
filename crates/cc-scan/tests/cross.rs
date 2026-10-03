//! Checks cc-scan against what home/pattern.py and home/solve.py gave (docs/rust-host.md R9). Their
//! output was recorded once in tests/fixtures/ before the Python went away. These used to be
//! tests/pattern-cross, solve-cross and solve-real.
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixtures(suite: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(suite)
}

fn json(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn dist(a: &Value, b: &Value) -> f64 {
    (0..3).map(|i| (a[i].as_f64().unwrap() - b[i].as_f64().unwrap()).powi(2)).sum::<f64>().sqrt()
}

/// The solve's output for a job: (stdout lines, the fits by name).
fn solve(job: &Value) -> (Vec<String>, std::collections::BTreeMap<String, Value>) {
    let mut lines = vec![];
    cc_scan::solve::solve(job, &mut |l| lines.push(l)).unwrap_or_else(|e| panic!("solve: {e}"));
    let fits = fits(&lines.join("\n"));
    (lines, fits)
}

fn fits(text: &str) -> std::collections::BTreeMap<String, Value> {
    text.lines().filter(|l| l.starts_with('{')).map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|r| (r["name"].as_str().unwrap().to_owned(), r)).collect()
}

/// Checks they're equal, with numbers to 1e-12, because serde_json's float parsing can be one ulp off Python's shortest repr.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() <= 1e-12 * x.as_f64().unwrap().abs().max(1.0),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

/// Matches pattern.py's layouts value for value, over monitor sizes and steps.
#[test]
fn pattern_cross() {
    let cases = json(&fixtures("pattern-cross").join("layouts.json"));
    let mut n = 0;
    for c in cases.as_array().unwrap() {
        let argv: Vec<&str> = c["argv"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
        if c.get("refused").is_some() {
            continue; // pattern.py's own self-check refused it. Only the Python had that check.
        }
        let got = cc_scan::pattern::run(&argv).unwrap_or_else(|| panic!("{argv:?}: no layout"));
        assert!(same(&got.json, &c["want"]), "{argv:?}:\n got {}\nwant {}", got.json, c["want"]);
        n += 1;
    }
    assert_eq!(n, 47);
}

/// Synthetic scans with a known answer: a curved ultrawide, a curved portrait and a flat laptop, with
/// 0.3 px noise, misreads, and one moving head with 40 ms lag. The fits have to match solve.py's and
/// the truth.
#[test]
fn solve_cross() {
    let dir = fixtures("solve-cross");
    let want = json(&dir.join("expected.json"));
    let mut n = 0;
    for (name, j) in want["jobs"].as_object().unwrap() {
        let mut job = json(&dir.join(j["job"].as_str().unwrap()));
        for k in ["visible", "hidden", "head"] {
            if let Some(f) = job[k].as_str() {
                job[k] = dir.join(f).to_string_lossy().into();
            }
        }
        let (lines, rs) = solve(&job);
        let py_text = j["python"].as_str().unwrap();
        let py = fits(py_text);
        assert_eq!(py.keys().collect::<Vec<_>>(), rs.keys().collect::<Vec<_>>(), "{name}");
        for (m, x) in &py {
            let y = &rs[m];
            assert_eq!(x["axis"], y["axis"], "{name} {m}");
            assert!(dist(&x["centre"], &y["centre"]) < 0.002, "{name} {m}: centres differ: {x} {y}");
            for ax in ["x", "y", "z"] {
                assert!(dist(&x[ax], &y[ax]) < 0.003, "{name} {m} {ax}: {x} {y}");
            }
            let (rx, ry) = (x["radius"].as_f64().unwrap(), y["radius"].as_f64().unwrap());
            assert!((rx - ry).abs() < (0.02 * rx).max(0.005), "{name} {m}: radius {rx} {ry}");
            assert!((x["rms"].as_f64().unwrap() - y["rms"].as_f64().unwrap()).abs() < 0.05, "{name} {m}: rms {x} {y}");
        }
        n += py.len();
        let sorted = |it: &mut dyn Iterator<Item = &str>| { let mut v: Vec<String> = it.filter(|l| l.contains("misread")).map(str::to_owned).collect(); v.sort(); v };
        assert_eq!(sorted(&mut py_text.lines()), sorted(&mut lines.iter().map(String::as_str)), "{name}: misreads dropped");
        let shape = name.rsplit('-').next().unwrap();
        if name.starts_with("moving") {
            assert!(lines.iter().any(|l| l == "#lag 0.040") && py_text.lines().any(|l| l == "#lag 0.040"), "{name}: the lag");
        }
        // The truth: name, mm, pixels, axis, radius, centre, rotation, base.
        for m in want["monitors"].as_array().unwrap() {
            let (mname, axis, radius) = (m[0].as_str().unwrap(), m[3].as_str().unwrap(), m[4].as_f64().unwrap());
            if shape == "flat" && axis != "flat" {
                continue;
            }
            let r = &rs[mname];
            let want_axis = if shape == "flat" { "flat" } else { axis };
            assert_eq!(r["axis"], want_axis, "{name} {mname}");
            assert!(dist(&r["centre"], &m[5]) < 0.01, "{name} {mname}: centre off: {r}");
            if want_axis != "flat" && shape != "pinned" {
                assert!((r["radius"].as_f64().unwrap() - radius).abs() < 0.15 * radius, "{name} {mname}: radius {r}");
            }
        }
    }
    assert_eq!(n, 15);
}

/// The final test's fit check (the review's gate). It's optional. It solves the real scans kept in
/// ~/.cache/control-center/scan-regression-<name>/ again (270+ MB each, so they're not committed) and
/// compares them with solve.py's fit recorded in tests/fixtures/solve-real/<name>-<job>.out. It needs
/// the same shape, centres within 6 mm and radius within 5%. tests/solve-real runs it
/// (cargo test -- --ignored).
#[test]
#[ignore]
fn solve_real() {
    let cache = PathBuf::from(std::env::var("HOME").unwrap()).join(".cache/control-center");
    let mut ran = 0;
    for f in std::fs::read_dir(fixtures("solve-real")).unwrap().map(|e| e.unwrap().path()) {
        let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
        let (scan, job) = stem.rsplit_once("-job").map(|(s, j)| (s.to_owned(), format!("job{j}.json"))).unwrap();
        let path = cache.join(format!("scan-regression-{scan}")).join(job);
        if !path.exists() {
            eprintln!("{stem}: skipped, no {}", path.display());
            continue;
        }
        let (_, rs) = solve(&json(&path));
        let py = fits(&std::fs::read_to_string(&f).unwrap());
        assert!(!py.is_empty() && py.keys().eq(rs.keys()), "{stem}: placed {:?} vs {:?}", py.keys(), rs.keys());
        for (m, x) in &py {
            let y = &rs[m];
            let d = dist(&x["centre"], &y["centre"]) * 1000.0;
            let (rx, ry) = (x["radius"].as_f64().unwrap(), y["radius"].as_f64().unwrap());
            let dr = if x["axis"] != "flat" { (rx - ry).abs() / rx.max(1e-9) } else { 0.0 };
            eprintln!("{stem} {m}: axis {}/{}  centre {d:.1} mm apart  radius {rx}/{ry}  rms {}/{} px", x["axis"], y["axis"], x["rms"], y["rms"]);
            assert!(x["axis"] == y["axis"] && d <= 6.0 && dr <= 0.05, "{stem} {m}: the fits differ");
        }
        ran += 1;
    }
    eprintln!("solve-real: {ran} real scan job(s) compared");
}
