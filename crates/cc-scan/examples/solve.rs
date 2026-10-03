//! `solve <job.json>` prints the fit's output for the job, the same as solve.py's.
fn main() {
    let path = std::env::args().nth(1).expect("job.json");
    let job: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).expect("job")).expect("job JSON");
    if let Err(e) = cc_scan::solve::solve(&job, &mut |line| println!("{line}")) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
