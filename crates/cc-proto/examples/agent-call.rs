//! `agent-call <machine> <cmd> [json args]` sends one command to a paired host's agent as this Frame
//! (~/.config/control-center) and prints the answer as JSON, with events on stderr. I use it to check
//! a live host by hand.
use serde_json::{Map, Value};
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let conf = std::path::PathBuf::from(std::env::var("CC_CONF").unwrap_or_else(|_| format!("{}/.config/control-center", std::env::var("HOME").unwrap())));
    let args: Map<String, Value> = a.get(3).map(|s| serde_json::from_str(s).expect("args: a JSON object")).unwrap_or_default();
    let t = cc_proto::agent::trusted(&conf, &a[1]).unwrap_or_else(|e| panic!("{e}"));
    let key = cc_proto::agent::frame_key(&conf).unwrap_or_else(|e| panic!("{e}"));
    let mut c = match cc_proto::agent::Client::connect(&t, &key, cc_proto::agent::PORT, Duration::from_secs(5)) {
        Ok(c) => c,
        Err(e) => {
            println!("{{\"error\":\"{e}\"}}");
            std::process::exit(1);
        }
    };
    match c.call(&a[2], args, Duration::from_secs(15), |e| eprintln!("event {e}")) {
        Ok(v) => println!("{v}"),
        Err(e) => {
            println!("{{\"error\":\"{e}\"}}");
            std::process::exit(1);
        }
    }
}
