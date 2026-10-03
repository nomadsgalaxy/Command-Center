//! Tests cc-host's tag screen against a private, headless KWin with its own D-Bus and Wayland socket,
//! so nothing shows on screen and the live session isn't touched. It sends three pictures in a row,
//! checks the screen is still up 5 s later, then makes sure EOF ends it cleanly. It's only a smoke
//! test, because the bug it was written for only shows on real outputs: a buffer destroyed before
//! KWin's release ended the screen at the second picture.
//! It needs kwin_wayland and dbus-run-session: `cargo test -p cc-host --test tagscreen -- --ignored`.
//! This was tests/tagscreen-kwin (bash).
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Group(i32);

impl Drop for Group {
    fn drop(&mut self) {
        unsafe { libc::kill(-self.0, libc::SIGKILL) }; // Kill KWin and its session bus, all of it.
    }
}

#[test]
#[ignore = "needs kwin_wayland and dbus-run-session"]
fn tag_screen_survives_three_pictures() {
    let sock = format!("cc-test-tagscreen-{}", std::process::id());
    let run = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR"));
    let _ = std::fs::remove_file(run.join(&sock));
    let _ = std::fs::remove_file(run.join(format!("{sock}.lock")));
    let kwin = Command::new("dbus-run-session")
        .args(["--", "kwin_wayland", "--virtual", "--socket", &sock, "--width", "1920", "--height", "1080", "--no-lockscreen", "--no-global-shortcuts"])
        .stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().expect("dbus-run-session kwin_wayland");
    let _group = Group(kwin.id() as i32);
    let up = Instant::now() + Duration::from_secs(20);
    while !run.join(&sock).exists() {
        assert!(Instant::now() < up, "KWin didn't come up");
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut screen = Command::new(env!("CARGO_BIN_EXE_cc-host")).args(["tagscreen", "Virtual-0"]).env("WAYLAND_DISPLAY", &sock)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = screen.stdin.take().unwrap();
    for p in [r#"{"bg":"white","size":[1920,1080],"tags":[[3,300,300,240]]}"#, r#"{"bg":"wait","size":[1920,1080],"tags":[]}"#,
              r#"{"bg":"white","size":[1920,1080],"tags":[[5,900,400,300],[6,1300,400,300]]}"#] {
        writeln!(input, "{p}").unwrap();
        std::thread::sleep(Duration::from_millis(700));
    }
    std::thread::sleep(Duration::from_secs(5));
    assert!(screen.try_wait().unwrap().is_none(), "the screen ended after the pictures");
    drop(input); // Send EOF.
    let status = screen.wait().unwrap();
    let oks = BufReader::new(screen.stdout.take().unwrap()).lines().map_while(Result::ok).filter(|l| l == "ok").count();
    assert!(status.success() && oks == 3, "exit {status}, {oks} ok");
    let _ = std::fs::remove_file(run.join(&sock));
    let _ = std::fs::remove_file(run.join(format!("{sock}.lock")));
}
