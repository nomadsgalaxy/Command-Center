//! viewers.conf has more than one writer: cc-home machine (as its own process) and a VNC panel
//! pinning its first certificate (cc_proto::conf::pin_first_use, in cc-panels). Both go through
//! cc_proto::conf::edit_viewers, which locks the file, so running them at the same time loses
//! neither's change.
use std::path::PathBuf;
use std::process::Command;

#[test]
fn a_machine_set_and_first_pins_both_survive() {
    let home = std::env::temp_dir().join(format!("cc-home-viewers-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let conf: PathBuf = home.join(".config/control-center");
    std::fs::create_dir_all(&conf).unwrap();
    let lines: String = (0..9).map(|i| format!("m{i}  me@10.0.0.{i}:5900  {i}  1920x1080  proto=vnc\n")).collect();
    std::fs::write(conf.join("viewers.conf"), format!("# a desk of macs\n{lines}")).unwrap();

    // cc-home rewrites the whole file again and again, setting m0's label.
    let sets = std::thread::spawn({
        let home = home.clone();
        move || {
            for n in 0..12 {
                let ok = Command::new(env!("CARGO_BIN_EXE_cc-home")).args(["machine", "set", "m0", &format!("label=take{n}")]).env("HOME", &home).env_remove("XDG_CONFIG_HOME").status().unwrap();
                assert!(ok.success(), "machine set {n}");
            }
        }
    });
    // Meanwhile, eight first pins land on the other lines.
    for i in 1..9 {
        let hex = format!("{i:02x}").repeat(32);
        assert_eq!(cc_proto::conf::pin_first_use(&conf, &format!("m{i}"), &hex), Ok(cc_proto::conf::FirstPin::Saved), "m{i}");
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    sets.join().unwrap();

    let text = std::fs::read_to_string(conf.join("viewers.conf")).unwrap();
    let _ = std::fs::remove_dir_all(&home);
    assert!(text.starts_with("# a desk of macs\n"), "the comment stays: {text}");
    assert!(text.lines().any(|l| l.starts_with("m0 ") && l.contains("label=take11")), "the last set: {text}");
    for i in 1..9 {
        assert_eq!(cc_proto::conf::viewer_pin_in(&text, &format!("m{i}")), Some(format!("{i:02x}").repeat(32)), "m{i}'s pin survived: {text}");
    }
}
