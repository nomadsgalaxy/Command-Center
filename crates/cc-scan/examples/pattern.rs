//! `pattern <pattern.py's arguments>` prints the layout's JSON, the same way pattern.py printed it.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = a.iter().map(String::as_str).collect();
    match cc_scan::pattern::run(&argv) {
        Some(l) => println!("{}", l.json),
        None => std::process::exit(2),
    }
}
