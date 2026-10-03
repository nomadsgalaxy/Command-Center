//! cc-host runs Command Center on a machine you see and control from the Frame (docs/rust-host.md).
//! It replaced home/pair.py, home/agent.py, home/tagshow.py and cc-share's bash, which are all gone now.
//!
//!   cc-host serve [--fake] [--port N]   the agent on 3399 (--fake: the conformance suite's host)
//!   cc-host pair [--test]                       a pairing key on screen (pair.py host_main)
//!   cc-host tagscreen <output> [--ask <text>]   the align's tag screen (tagshow.py --params / --ask)
//!   cc-host cert                                krdp's certificate (cert.pem, key.pem), made once
//!   cc-host check --agent [--port N]            the agent answers TLS with this host's key
//!   cc-host <cc-share's commands>               install, up, down, check, ... (src/share.rs; cc-share execs this)
//!   cc-host version
//! The config directory is ~/.config/control-center, or $CC_CONF.

mod agent;
mod aruco;
mod draw;
mod hostcert;
mod pair;
mod platform;
mod screen;
mod share;
mod work;

use std::net::TcpListener;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // When it's called as cc-share (the link install makes), it takes the same commands but prints cc-share's usage when bare.
    let as_share = args.first().and_then(|a| std::path::Path::new(a).file_name()).is_some_and(|n| n == "cc-share");
    if as_share && matches!(args.get(1).map(String::as_str), None | Some("help" | "--help" | "-h")) {
        eprintln!("usage: cc-share list | install <monitor> ... [--firewall] [--announce|--no-announce] [--autostart|--no-autostart] [--dry-run]\n       \
                   cc-share uninstall | status | check | up | down | autostart on|off|status | announce on|off|status\n       \
                   cc-share pair [--firewall] | unpair <frame> | frames | stop | lock | unlock | windows on|off|status\n\
                   (cc-share is cc-host: docs/rust-host.md. The shared password the first time: CC_PASSWORD=... cc-share install 0)");
        std::process::exit(2);
    }
    let conf = std::env::var("CC_CONF").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").expect("HOME")).join(".config/control-center"));
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let port: u16 = args.iter().position(|a| a == "--port").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or(cc_proto::agent::PORT);
            let plat: Box<dyn platform::Platform> = if args.iter().any(|a| a == "--fake") { Box::new(platform::Fake::default()) } else { Box::new(platform::Linux { conf: conf.clone() }) };
            let agent = agent::Agent::new(conf, plat);
            let lis = TcpListener::bind(("0.0.0.0", port)).unwrap_or_else(|e| panic!("port {port}: {e}"));
            println!("agent: listening on {port}");
            agent.serve(lis);
        }
        Some("render") => {
            // Renders a screen as a PGM on stdout for checks: render tags <w> <h> (params on stdin) | key <w> <h> <host> <key> | prompt <w> <h> <text>
            let (w, h): (usize, usize) = (args[3].parse().expect("width"), args[4].parse().expect("height"));
            let c = match args[2].as_str() {
                "tags" => {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).expect("params");
                    draw::tag_screen(&serde_json::from_str(&s).expect("params JSON"), w, h)
                }
                "key" => draw::key_screen(&args[5], &args[6], 299, None, w, h),
                _ => draw::prompt(&args[5].replace("\\n", "\n"), w, h),
            };
            std::io::Write::write_all(&mut std::io::stdout(), &draw::to_pgm(&c)).expect("stdout");
        }
        Some("pair") => {
            let test = args.iter().any(|a| a == "--test");
            if !test && let Err(code) = share::pair_precheck(&share::env(), if args.iter().any(|a| a == "--firewall") { "--firewall" } else { "" }) {
                std::process::exit(code);
            }
            let plat: Box<dyn platform::Platform + Send + Sync> = if test { Box::new(platform::Fake::default()) } else { Box::new(platform::Linux { conf: conf.clone() }) };
            std::process::exit(pair::main(&conf, plat, test));
        }
        Some("tagscreen") if args.len() > 2 => {
            let ask = (args.get(3).map(String::as_str) == Some("--ask")).then(|| args[4..].join(" "));
            std::process::exit(screen::tag_screen_main(&args[2], ask));
        }
        Some("cert") => match hostcert::cert(&conf, &platform::Platform::hostname(&platform::Linux { conf: conf.clone() })) {
            Ok(what) => println!("krdp certificate {what}"),
            Err(e) => {
                eprintln!("cc-host cert: {e}");
                std::process::exit(1);
            }
        },
        Some("check") if args.iter().any(|a| a == "--agent") => {
            let port: u16 = args.iter().position(|a| a == "--port").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or(cc_proto::agent::PORT);
            if let Err(e) = hostcert::check(&conf, port) {
                eprintln!("cc-host check: {e}");
                std::process::exit(1);
            }
            println!("agent answering on {port}");
        }
        Some("version") | None => println!("cc-host {}", env!("CARGO_PKG_VERSION")),
        Some(other) => match share::main(other, &args[2..]) {
            Some(code) => std::process::exit(code),
            None => {
                eprintln!("cc-host: unknown command {other} (serve, pair, tagscreen, render, cert, check, version, and cc-share's: list install uninstall status up down autostart announce unpair frames stop lock unlock windows guard)");
                std::process::exit(2);
            }
        },
    }
}
