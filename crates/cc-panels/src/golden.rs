//! The golden tests' fixtures (docs/openxr-plan.md, phase 0). A golden test writes down what the
//! code did, step by step: the OpenVR calls it made (vr::fake's log), what it sent to the computer
//! (the session's pointer and keys) and what it was holding (a press, a drag). The text is
//! compared, line for line, with a file in crates/cc-panels/golden/ that the code wrote before
//! phase 0 moved anything. A difference fails the test: the move changed behaviour.
//!
//! The files are never written by an ordinary run, a missing one included. To write them, on
//! purpose, for a change in behaviour that's meant and reviewed:
//!
//!   CC_GOLDEN_WRITE=1 cargo test -p cc-panels golden
//!
//! then commit the files with the change, so the diff shows what it changed.
use std::fmt::Write as _;
use std::path::PathBuf;

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden").join(format!("{name}.txt"))
}

/// Fails unless `got` is exactly the committed fixture `name`. With CC_GOLDEN_WRITE=1 it writes
/// the fixture instead.
pub fn check(name: &str, got: &str) {
    let p = path(name);
    if std::env::var_os("CC_GOLDEN_WRITE").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, got).unwrap();
        eprintln!("golden: wrote {}", p.display());
        return;
    }
    let want = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("golden: no fixture {} ({e}). It's written only on purpose: CC_GOLDEN_WRITE=1 (golden.rs)", p.display()));
    if want == got {
        return;
    }
    let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    let n = (0..w.len().max(g.len())).find(|&i| w.get(i) != g.get(i)).unwrap_or(0);
    panic!(
        "golden: {name} changed at line {}\n  want: {}\n  got:  {}\nIf the change is meant, CC_GOLDEN_WRITE=1 rewrites it (golden.rs); commit the diff with it.",
        n + 1,
        w.get(n).unwrap_or(&"(the end)"),
        g.get(n).unwrap_or(&"(the end)")
    );
}

pub use crate::vr::fake::{mat, num}; // the same numbers as vr::fake's call log

/// A transcript, one step at a time. Anything written from an unordered collection (a HashMap,
/// a set) is sorted first, so a run's order can't change it.
#[derive(Default)]
pub struct Log(pub String);

impl Log {
    pub fn step(&mut self, what: &str) {
        let _ = writeln!(self.0, "== {what}");
    }

    pub fn line(&mut self, l: impl std::fmt::Display) {
        let _ = writeln!(self.0, "  {l}");
    }

    /// What vr::fake was called with since the last time.
    pub fn vr_calls(&mut self) {
        for c in crate::vr::fake::take_calls() {
            self.line(format!("vr {c}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_the_same_whichever_way_they_round() {
        assert_eq!(num(-0.000001), "0.00000");
        assert_eq!(num(0.0), "0.00000");
        assert_eq!(num(-1.234567), "-1.23457");
        assert_eq!(num(2.5), "2.50000");
    }
}
