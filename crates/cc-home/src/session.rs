//! The Desktop's launch glue, ported from the shell scripts it replaces. Their paths are symlinks
//! to cc-home now and it dispatches on argv[0]. The behaviour, timeouts, log format, markers and
//! exit codes all match the scripts:
//!   cc-home desktop [name ...]  session/cc-launch: what the VR launcher's "Desktop" runs. The
//!                               desktop session (its own systemd user unit cc-desktop, so it
//!                               outlives cc-panels) if it isn't up, hibernate restore,
//!                               autoconnect, then cc-panels with no time limit; a deliberate
//!                               close (cc-panels' desktop-closed marker) rests too
//!   cc-home panels [--for MIN] [name ...] | stop
//!                               the cc-panels wrapper: target/release/cc-panels in cc-box, the
//!                               session's env file kept fresh, its log, the restore after a crash
//!   cc-home rest                session/cc-rest: hibernate save, the session stopped, and the
//!                               programs named in `rest_nice` at nice 10 and idle-ish IO
//!   cc-home session             session/cc-desktop: Plasma in a headless KWin (the cc-desktop unit)
//!   cc-home box <cmd> [args...] cc-box: a command in the control-center container, or right
//!                               here when the build is native (native())
//! With CC_DRY=1, desktop, panels, rest and box print the commands that would change something
//! (systemd-run, systemctl stop, distrobox enter, renice ...) instead of running them.
use crate::{cache, home_dir};
use std::ffi::OsString;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};
use std::time::Duration;
use std::{fs, thread};

/// The commands this module answers, by subcommand or by the name it was run as.
pub fn dispatch(name: &str, args: &[OsString]) -> Option<i32> {
    let a: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let (sub, rest) = match name {
        "cc-home" => (a.first()?.as_str(), &a[1.min(a.len())..]),
        n => (n, &a[..]),
    };
    DRY.store(std::env::var_os("CC_DRY").is_some_and(|v| v == "1"), Relaxed);
    Some(match sub {
        "desktop" | "cc-launch" => desktop(rest),
        "panels" | "cc-panels" => panels(rest),
        "rest" | "cc-rest" => rest_now(),
        "session" | "cc-desktop" => session(),
        "box" | "cc-box" => boxed(&args[usize::from(name == "cc-home")..]),
        "kwin_wayland_wrapper" => kwin_wrapper(args),
        "flatpak-bwrap" => flatpak_bwrap(args),
        "install" => crate::install::main(rest),
        _ => return None,
    })
}

static DRY: AtomicBool = AtomicBool::new(false);

/// Runs it, or with CC_DRY=1 just prints it and calls that a success.
fn run(c: &mut Command) -> std::io::Result<ExitStatus> {
    if DRY.load(Relaxed) {
        println!("dry: {c:?}");
        return Ok(ExitStatus::from_raw(0));
    }
    c.status()
}

/// What a shell's $? would say: the exit code, or 128 + the signal.
pub fn code(s: ExitStatus) -> i32 {
    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// The repository, worked out from target/<triple>/release/cc-home (same as cc-panels' root()).
/// Installed from the sysext image, cc-home sits right in that tree's root, /usr/lib/command-center,
/// with no target/ above it (packaging/sysext/build.sh).
pub fn root() -> PathBuf {
    root_of(&fs::read_link("/proc/self/exe").unwrap_or_default())
}

pub fn root_of(exe: &Path) -> PathBuf {
    let checkout = exe.ancestors().find(|a| a.ends_with("target")).and_then(|t| t.parent());
    checkout.or(exe.parent()).map(PathBuf::from).unwrap_or_default()
}

/// Built for SteamOS itself (packaging/sysext/build-native.sh), the sysext image or a checkout
/// built that way, so everything runs on the host and there's no container.
pub fn native(root: &Path) -> bool {
    root.join("panels/third_party/prefix/steamos-release").is_file()
}

fn exe() -> PathBuf {
    fs::read_link("/proc/self/exe").unwrap_or_else(|_| "cc-home".into())
}

/// Same as the shell's `${VAR:-default}`.
fn var_or(var: &str, default: &str) -> String {
    std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.into())
}

fn sleep_s(s: u64) {
    thread::sleep(Duration::from_secs(s))
}

// ---- the Steam client's environment

/// What the launcher's environment (the Steam client's) carries that the desktop mustn't have.
/// Its LD_LIBRARY_PATH puts Steam's libavcodec (no H.264) ahead of the system's, and then FreeRDP
/// can't decode the remotes' screens. The session drops two more (`session` is the cc-desktop list).
pub fn steam_var(name: &str, session: bool) -> bool {
    matches!(name, "LD_LIBRARY_PATH" | "LD_PRELOAD" | "STEAMVIDEOTOKEN")
        || ["STEAM_", "Steam", "SRT_", "PRESSURE_VESSEL_", "MANGOHUD_", "ENABLE_VK_LAYER_VALVE_steam_overlay_"].iter().any(|p| name.starts_with(p))
        || session && matches!(name, "ENABLE_GAMESCOPE_WSI" | "XDG_DESKTOP_PORTAL_DIR")
}

fn strip_steam(session: bool) {
    for (k, _) in std::env::vars_os() {
        if steam_var(&k.to_string_lossy(), session) {
            unsafe { std::env::remove_var(&k) }; // safe, there's still only one thread here
        }
    }
}

// ---- /proc (pgrep's part), under a root so the tests can fake it

fn pids(proc: &Path) -> Vec<u32> {
    let mut v: Vec<u32> = fs::read_dir(proc).into_iter().flatten().flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
    v.sort();
    v
}

/// Same as `pgrep -x name`: matches by process name (comm).
pub fn named(proc: &Path, name: &str) -> Vec<u32> {
    pids(proc).into_iter().filter(|p| fs::read_to_string(proc.join(format!("{p}/comm"))).is_ok_and(|c| c.trim_end_matches('\n') == name)).collect()
}

/// Same as `pgrep -P pid`.
pub fn children(proc: &Path, pid: u32) -> Vec<u32> {
    let ppid = |p: u32| -> Option<u32> {
        let stat = fs::read_to_string(proc.join(format!("{p}/stat"))).ok()?;
        stat[stat.rfind(')')? + 1..].split_whitespace().nth(1)?.parse().ok()
    };
    pids(proc).into_iter().filter(|&p| ppid(p) == Some(pid)).collect()
}

/// The pid and everything under it, parents first (cc-rest's tree()).
pub fn tree(proc: &Path, pid: u32) -> Vec<u32> {
    let mut out = vec![pid];
    for c in children(proc, pid) {
        out.extend(tree(proc, c));
    }
    out
}

fn environ(proc: &Path, pid: u32) -> Option<Vec<String>> {
    let b = fs::read(proc.join(format!("{pid}/environ"))).ok()?;
    Some(b.split(|&c| c == 0).filter(|l| !l.is_empty()).map(|l| String::from_utf8_lossy(l).into_owned()).collect())
}

/// The environment of the plasmashell whose session runtime dir is `runtime`.
fn plasma_env(proc: &Path, runtime: &str) -> Option<Vec<String>> {
    let want = format!("XDG_RUNTIME_DIR={runtime}");
    named(proc, "plasmashell").into_iter().find_map(|p| environ(proc, p).filter(|e| e.contains(&want)))
}

/// Same as `pgrep -f '^[^ ]*target/release/cc-panels'`: the binary itself, which runs in cc-box
/// and shows up here through PidMode=host.
pub fn is_panels(cmdline: &[u8]) -> bool {
    let first = cmdline.split(|&c| c == 0 || c == b' ').next().unwrap_or(b"");
    String::from_utf8_lossy(first).contains("target/release/cc-panels")
}

fn panels_running(proc: &Path) -> Vec<u32> {
    let me = std::process::id();
    pids(proc).into_iter().filter(|&p| p != me && fs::read(proc.join(format!("{p}/cmdline"))).is_ok_and(|c| is_panels(&c))).collect()
}

// ---- cc-launch

/// The transient unit the session runs as. On a stop its apps get 10 s to quit before they're
/// killed. Its output goes to /tmp/cc-desktop.log, fresh each start.
pub fn session_unit(exe: &Path) -> Vec<String> {
    let mut v: Vec<String> = ["systemd-run", "--user", "--collect", "--quiet", "--unit", "cc-desktop", "-p", "TimeoutStopSec=10", "-p", "StandardOutput=truncate:/tmp/cc-desktop.log", "-p", "StandardError=inherit"]
        .map(String::from)
        .into();
    v.extend([exe.display().to_string(), "session".into()]);
    v
}

fn cmd(argv: &[String]) -> Command {
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    c
}

fn systemctl(args: &[&str]) -> Command {
    let mut c = Command::new("systemctl");
    c.arg("--user").args(args);
    c
}

/// What session/cc-launch runs.
fn desktop(args: &[String]) -> i32 {
    strip_steam(false);
    let root = root();
    let runtime = format!("{}/cc-desktop", var_or("XDG_RUNTIME_DIR", &format!("/run/user/{}", uid())));
    // A Desktop closed a moment ago may still be stopping because an app is slow to quit. Let it
    // finish, or the new session can't start and cc-panels comes up without one (live,
    // 2026-10-03: a tray app hung its stop for over a minute).
    for _ in 0..100 {
        let out = systemctl(&["is-active", "cc-desktop"]).stderr(Stdio::inherit()).output();
        if out.map_or(true, |o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n') != "deactivating") {
            break;
        }
        sleep_s(1);
    }
    if !systemctl(&["is-active", "-q", "cc-desktop"]).status().is_ok_and(|s| s.success()) {
        let _ = run(systemctl(&["reset-failed", "cc-desktop"]).stderr(Stdio::null()));
        if let Err(e) = run(&mut cmd(&session_unit(&exe()))) {
            eprintln!("systemd-run: {e}");
        }
    }
    // Wait for Plasma, since cc-panels finds the session from its shell. A minute at most;
    // cc-panels waits for it too if it's slower.
    for _ in 0..60 {
        if plasma_env(Path::new("/proc"), &runtime).is_some() {
            break;
        }
        sleep_s(1);
    }
    // Reopen the apps that were open at the last deliberate close (cc-home hibernate).
    let _ = run(Command::new(exe()).args(["hibernate", "restore"]));
    // Work out which remotes connect now (autoconnect=yes, on a known network). This runs on the
    // host side because it needs nmcli.
    let _ = run(Command::new(exe()).args(["autoconnect", "--write"]).stdout(Stdio::null()).stderr(Stdio::null()));
    // Not exec, because there's work after. When it's closed on purpose (cc-panels leaves
    // desktop-closed), the session gets stopped so a closed Desktop costs next to nothing. A crash
    // or a restart (a signal) keeps the session.
    let closed = PathBuf::from(cache("desktop-closed"));
    if !DRY.load(Relaxed) {
        let _ = fs::remove_file(&closed);
    }
    let status = match run(Command::new(root.join("cc-panels")).args(["--for", "0"]).args(args)) {
        Ok(s) => code(s),
        Err(e) => {
            eprintln!("{}: {e}", root.join("cc-panels").display());
            127
        }
    };
    if closed.exists() {
        rest_now();
    }
    status
}

// ---- cc-rest

/// The Desktop was closed on purpose, so what's left has to cost next to nothing and a game gets
/// the headset. The desktop session is stopped (that's ~2 GB back, and Desktop starts it again).
/// Background work the user wants kept going (settings.json `rest_nice`, a list of process
/// names, empty by default) goes to nice 10 with best-effort IO at the lowest priority, along
/// with everything it started.
fn rest_now() -> i32 {
    let _ = run(Command::new(exe()).args(["hibernate", "save"])); // save its apps first, for the next open
    let _ = run(systemctl(&["stop", "cc-desktop"]).stderr(Stdio::null()));
    let proc = Path::new("/proc");
    let names = setting_list("rest_nice");
    for h in names.iter().flat_map(|n| named(proc, n)) {
        for p in tree(proc, h) {
            if DRY.load(Relaxed) {
                println!("dry: renice -n 10 -p {p}; ionice -c 2 -n 7 -p {p}");
                continue;
            }
            // renice -n 10 (absolute) and ionice -c 2 -n 7, per pid like those tools do
            unsafe {
                libc::setpriority(libc::PRIO_PROCESS, p, 10);
                libc::syscall(libc::SYS_ioprio_set, 1 as libc::c_long /* IOPRIO_WHO_PROCESS */, p as libc::c_long, ((2 << 13) | 7) as libc::c_long);
            }
        }
    }
    if names.is_empty() {
        println!("Desktop closed: its session stopped");
    } else {
        println!("Desktop closed: its session stopped, {} at nice 10", names.join(", "));
    }
    0
}

/// A list of strings from settings.json, or none when it isn't set.
fn setting_list(key: &str) -> Vec<String> {
    let s: serde_json::Value = fs::read(crate::conf_dir().join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    s[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect()
}

// ---- the cc-panels wrapper

/// The desktop session's variables for cc-panels (session.rs). Inside cc-box it can't read
/// another process's /proc/*/environ, so they go to desktop-session.env, which is removed when
/// there's no session.
pub fn session_env(proc: &Path, cache: &Path, xdg: &str) {
    let file = cache.join("desktop-session.env");
    let Some(env) = plasma_env(proc, &format!("{xdg}/cc-desktop")) else {
        let _ = fs::remove_file(&file);
        return;
    };
    let keep = ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "DISPLAY", "XAUTHORITY"];
    let text: String = env.iter().filter(|l| l.split_once('=').is_some_and(|(k, _)| keep.contains(&k))).map(|l| format!("{l}\n")).collect();
    let new = cache.join("desktop-session.env.new");
    if fs::write(&new, text).is_ok() {
        let _ = fs::rename(&new, &file);
    }
}

/// Opens the log for appending and writes a run marker first, so a crashed or frozen run's log
/// survives the next start. Past 5 MB the old one moves to cc-panels.log.1.
pub fn start_log(cache: &Path, stamp: &str) -> std::io::Result<fs::File> {
    let log = cache.join("cc-panels.log");
    if fs::metadata(&log).map_or(0, |m| m.len()) > 5_000_000 {
        fs::rename(&log, cache.join("cc-panels.log.1"))?;
    }
    let mut f = fs::OpenOptions::new().append(true).create(true).open(&log)?;
    std::io::Write::write_all(&mut f, format!("=== cc-panels {stamp} ===\n").as_bytes())?;
    Ok(f)
}

/// Same as `date '+%F %T'`, in local time.
fn stamp() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let mut buf = [0u8; 32];
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%F %T".as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

extern "C" fn noted(sig: libc::c_int) {
    SIGNALLED.store(sig, Relaxed);
}

static SIGNALLED: AtomicI32 = AtomicI32::new(0);

/// Handled, not ignored, so a child still gets the signal's default (a handler doesn't survive
/// exec, but ignoring would).
fn catch(sigs: &[libc::c_int]) {
    for &s in sigs {
        unsafe { libc::signal(s, noted as *const () as libc::sighandler_t) };
    }
}

/// Puts back windows cc-panels changed and couldn't restore itself because it crashed or was killed.
fn finish(proc: &Path, cache: &Path, root: &Path, xdg: &str) {
    if !DRY.load(Relaxed) {
        session_env(proc, cache, xdg);
    }
    if cache.join("kwin-restore.json").is_file() {
        let _ = run(Command::new(root.join("cc-box")).arg(root.join("target/release/cc-panels")).arg("--restore"));
    }
}

/// The `cc-panels` in the repo root: cc-panels [--for MIN] [name ...] (2 minutes by default,
/// --for 0 runs until stopped), or cc-panels stop. It logs to ~/.cache/control-center/cc-panels.log.
fn panels(args: &[String]) -> i32 {
    let xdg = format!("/run/user/{}", uid());
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &xdg) };
    let (root, proc) = (root(), Path::new("/proc"));
    let cache = PathBuf::from(cache(""));
    let _ = fs::create_dir_all(&cache);
    if args.first().map(String::as_str) == Some("stop") {
        let found = panels_running(proc);
        let sent = !DRY.load(Relaxed) && found.iter().filter(|&&p| unsafe { libc::kill(p as i32, libc::SIGINT) } == 0).count() > 0;
        println!("{}", if sent { "stopped" } else if DRY.load(Relaxed) && !found.is_empty() { "dry: would stop" } else { "not running" });
        for _ in 0..50 {
            if DRY.load(Relaxed) || panels_running(proc).is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        finish(proc, &cache, &root, &xdg);
        return 0;
    }
    if !panels_running(proc).is_empty() {
        eprintln!("cc-panels is already running (cc-panels stop)");
        return 1;
    }
    // Not niced like cc-box's default, because waiting for a CPU at nice 10 made window opens
    // stall 0.4-0.6 s.
    let mut c = Command::new(root.join("cc-box"));
    c.env("CC_NICE", "0").arg(root.join("target/release/cc-panels")).args(args);
    if DRY.load(Relaxed) {
        println!("dry: {c:?} 2>&1 | tee -a {}", cache.join("cc-panels.log").display());
        return 0;
    }
    catch(&[libc::SIGINT, libc::SIGTERM]); // so cc-panels itself still gets Ctrl+C, and finish still runs
    session_env(proc, &cache, &xdg);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let envloop = {
        let (stop, cache, xdg) = (stop.clone(), cache.clone(), xdg.clone());
        thread::spawn(move || loop {
            sleep_s(5);
            if stop.load(Relaxed) {
                break;
            }
            session_env(Path::new("/proc"), &cache, &xdg);
        })
    };
    let mut log = start_log(&cache, &stamp()).ok();
    // Like 2>&1 | tee -a, so cc-box's own lines (its waiting) go to the log too.
    let spawned = std::io::pipe().and_then(|(mut r, w)| {
        let child = c.stdout(w.try_clone()?).stderr(w).spawn();
        drop(c); // drops our copies of the pipe's write end, so we get EOF once cc-panels is done
        let mut child = child?;
        let mut buf = [0u8; 8192];
        let mut out = std::io::stdout();
        loop {
            match std::io::Read::read(&mut r, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    use std::io::Write;
                    let _ = out.write_all(&buf[..n]).and_then(|_| out.flush());
                    if let Some(f) = log.as_mut() {
                        let _ = f.write_all(&buf[..n]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        child.wait()
    });
    if let Err(e) = spawned {
        eprintln!("{}: {e}", root.join("cc-box").display());
    }
    stop.store(true, Relaxed);
    drop(envloop); // detached: it sees stop within 5 s, or just ends with us
    finish(proc, &cache, &root, &xdg);
    0 // tee's status, same as the script's pipeline. A crash shows in the log, not the exit code.
}

// ---- cc-box

/// Builds `nice -n N distrobox enter <box> -- args`, or just `nice -n N args` with no box.
pub fn box_argv(nice: &str, home: &str, name: Option<&str>, args: &[OsString]) -> Vec<OsString> {
    let mut v: Vec<OsString> = ["nice", "-n", nice].map(OsString::from).into();
    if let Some(name) = name {
        v.extend([format!("{home}/.local/bin/distrobox"), "enter".into(), name.into(), "--".into()].map(OsString::from));
    }
    v.extend(args.iter().cloned());
    v
}

fn setup_done(name: &str) -> bool {
    Command::new("podman").args(["logs", name]).output().is_ok_and(|o| {
        let has = |b: &[u8]| b.windows(20).any(|w| w == b"container_setup_done");
        has(&o.stdout) || has(&o.stderr)
    })
}

/// cc-box runs a command in Command Center's container (Fedora, through distrobox), which is where
/// the build tools, FreeRDP, OpenCV and the OpenVR runtime's libraries live. The container starts in
/// a systemd scope of its own, so ending whatever started it doesn't stop it. A native build
/// needs none of that, so there it's only the nice.
fn boxed(args: &[OsString]) -> i32 {
    let name = var_or("CC_CONTAINER", "control-center");
    let native = native(&root());
    if !native {
        if let Err(c) = container_up(&name) {
            return c;
        }
    }
    // Gentle by default for builds and tools, since the headset is rendering VR too. cc-panels
    // passes CC_NICE=0.
    let argv = box_argv(&var_or("CC_NICE", "10"), &home_dir(), (!native).then_some(name.as_str()), args);
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    if DRY.load(Relaxed) {
        println!("dry: {c:?}");
        return 0;
    }
    let e = c.exec();
    eprintln!("nice: {e}");
    if e.kind() == std::io::ErrorKind::NotFound { 127 } else { 126 }
}

/// Starts the container if it isn't running, and waits for its first-boot setup.
fn container_up(name: &str) -> Result<(), i32> {
    let xdg = format!("/run/user/{}", uid());
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &xdg);
        // podman needs the real user bus (systemd, for the container's cgroup), and a desktop
        // session may be running on a private one.
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={xdg}/bus"));
    }
    let running = Command::new("podman").args(["container", "inspect", "-f", "{{.State.Running}}", name]).stderr(Stdio::null()).output();
    if !running.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n') == "true") {
        if !Command::new("podman").args(["container", "exists", name]).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
            eprintln!("no {name} container: run install.sh");
            return Err(1);
        }
        let desc = format!("--description={name} container (Command Center)");
        let _ = run(Command::new("systemd-run").args(["--user", "--scope", "--quiet", "--collect", &desc, "podman", "start", name]).stdout(Stdio::null()));
    }
    // A new container sets itself up on first boot. distrobox enter only waits for that when it
    // starts the container itself, so wait here.
    if !setup_done(name) {
        eprintln!("waiting for the {name} container's first-boot setup...");
        for _ in 0..600 {
            if setup_done(name) {
                break;
            }
            sleep_s(1);
        }
    }
    Ok(())
}

// ---- cc-desktop

/// Like `set -a; . file`, for mesavars.sh's plain NAME=value lines.
/// ponytail: no shell expansion or continuation; a sourced file that needs them needs `sh -c`.
pub fn assignments(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.is_empty() && k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
        .map(|(k, v)| (k.into(), v.trim_matches(|c| c == '"' || c == '\'').into()))
        .collect()
}

/// KWin's arguments for the headless screens. plasma-session runs kwin_wayland_wrapper from PATH,
/// which is how these get in.
pub fn kwin_args(size: &str, rest: &[OsString]) -> Vec<OsString> {
    let w = size.rsplit_once('x').map_or(size, |(w, _)| w); // ${SIZE%x*}
    let h = size.split_once('x').map_or(size, |(_, h)| h); // ${SIZE#*x}
    let mut v: Vec<OsString> = ["--virtual", "--width", w, "--height", h, "--output-count", "3", "--no-lockscreen"].map(OsString::from).into();
    v.extend(rest.iter().cloned());
    v
}

/// What runs as $runtime/bin/kwin_wayland_wrapper, which is a link to us.
fn kwin_wrapper(args: &[OsString]) -> i32 {
    // What I asked for on 2026-10-02: a window can be at most this big.
    let e = Command::new("/usr/bin/kwin_wayland_wrapper").args(kwin_args(&var_or("CC_DESKTOP_SIZE", "2560x1440"), args)).exec();
    eprintln!("/usr/bin/kwin_wayland_wrapper: {e}");
    127
}

/// What runs as $runtime/bin/flatpak-bwrap, the session's FLATPAK_BWRAP: bwrap with the
/// document portal also at the session's path (doc_link).
fn flatpak_bwrap(args: &[OsString]) -> i32 {
    // Flatpak hands bwrap its options in a memfd (`--args FD`). Opening it again reads it from
    // the start without moving bwrap's offset. Anything else, like a pipe, I leave alone.
    let spec = args.iter().position(|a| a == "--args").and_then(|i| args.get(i + 1)).map(|fd| PathBuf::from(format!("/proc/self/fd/{}", fd.to_string_lossy())));
    let spec = spec.filter(|f| fs::metadata(f).is_ok_and(|m| m.is_file())).and_then(|f| fs::read(f).ok()).unwrap_or_default();
    let e = Command::new("/usr/bin/bwrap").args(doc_link(args, &spec, uid())).exec();
    eprintln!("/usr/bin/bwrap: {e}");
    127
}

/// bwrap's arguments with the document portal also linked at its mount's own path inside an app's
/// sandbox.
///
/// The file chooser hands a Flatpak app the chosen file as a path in the document portal's mount,
/// $XDG_RUNTIME_DIR/doc/<id>/<name>, and here that's /run/user/$UID/cc-desktop/doc. The host
/// already has its own document portal on /run/user/$UID/doc, so the session's can't mount there.
/// Flatpak binds the portal's by-app view at /run/flatpak/doc and links only /run/user/$UID/doc to
/// it, so the path the app gets doesn't exist in its sandbox, and a browser's upload silently gets
/// nothing. The link fixes the path and shows the app no more than the by-app view it already has.
///
/// Only the app's own sandbox gets it, the one whose options bind `<mount>/by-app/<app>` on
/// /run/flatpak/doc, and that bind says where the mount is (flatpak runs bwrap with no
/// environment). The D-Bus proxy's sandbox sees the host's /run, where that path is the portal's
/// mount, and bwrap would fail on it.
pub fn doc_link(args: &[OsString], spec: &[u8], uid: u32) -> Vec<OsString> {
    let mut v = args.to_vec();
    let s: Vec<&[u8]> = spec.split(|&b| b == 0).collect();
    let mount = s.windows(3).find(|w| w[0] == b"--bind" && w[2] == b"/run/flatpak/doc").and_then(|w| {
        let src = String::from_utf8_lossy(w[1]);
        Some(src[..src.find("/by-app/")?].to_owned())
    });
    let at = args.iter().position(|a| a == "--args").map(|i| i + 2).filter(|&i| i <= args.len());
    if let (Some(i), Some(m)) = (at, mount.filter(|m| *m != format!("/run/user/{uid}/doc"))) {
        v.splice(i..i, ["--symlink".into(), "/run/flatpak/doc".into(), m.into()]);
    }
    v
}

/// Same as `ln -sfn target link`.
pub fn force_link(target: impl AsRef<Path>, link: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(link).is_ok() {
        fs::remove_file(link)?;
    }
    std::os::unix::fs::symlink(target, link)
}

fn check(what: &str, r: std::io::Result<ExitStatus>) -> Result<(), i32> {
    match r {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(code(s)),
        Err(e) => {
            eprintln!("{what}: {e}");
            Err(127)
        }
    }
}

fn io<T>(what: impl std::fmt::Display, r: std::io::Result<T>) -> Result<T, i32> {
    r.map_err(|e| {
        eprintln!("{what}: {e}");
        1
    })
}

/// session/cc-desktop is Command Center's desktop session: Plasma in a headless KWin. There's no
/// host compositor, since cc-panels streams its windows and taskbar into VR through KWin's
/// screencast.
///
/// Screens:
/// - Virtual-0: Plasma's panel and its popups
/// - Virtual-1: every program window (cc-windows.js)
/// - Virtual-2: where the pointer parks (windows.rs)
///
/// It runs as the systemd user unit cc-desktop, with its runtime dir at
/// /run/user/$UID/cc-desktop and its Plasma config in ~/.config/control-center/desktop.
fn session() -> i32 {
    // Apps in it should see the system as a normal login does, not the Steam client's environment.
    strip_steam(true);
    if let Ok(t) = fs::read_to_string("/usr/share/deckard/mesavars.sh") {
        for (k, v) in assignments(&t) {
            unsafe { std::env::set_var(k, v) };
        }
    }
    let home = home_dir();
    // Flatpak apps publish their launchers under the Flatpak exports. Without those, Plasma opens
    // Discover instead.
    let data = format!("{}:{home}/.local/share/flatpak/exports/share:/var/lib/flatpak/exports/share", var_or("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
    unsafe { std::env::set_var("XDG_DATA_DIRS", data) };
    let host = var_or("XDG_RUNTIME_DIR", &format!("/run/user/{}", uid()));
    let runtime = PathBuf::from(format!("{host}/cc-desktop"));
    let cleanup = || {
        let _ = Command::new("fusermount3").args(["-u", "-z"]).arg(runtime.join("doc")).stderr(Stdio::null()).status();
        let _ = fs::remove_dir_all(&runtime);
    };
    cleanup();
    catch(&[libc::SIGTERM, libc::SIGINT, libc::SIGHUP]); // so on systemctl stop the runtime dir still gets cleaned up
    let status = session_body(&home, &host, &runtime).unwrap_or_else(|c| c);
    cleanup();
    let sig = SIGNALLED.load(Relaxed);
    if sig != 0 {
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
    status
}

fn session_body(home: &str, host: &str, runtime: &Path) -> Result<i32, i32> {
    use std::os::unix::fs::{DirBuilderExt, symlink};
    let config = PathBuf::from(format!("{home}/.config/control-center/desktop"));
    let state = PathBuf::from(format!("{home}/.local/state/control-center/desktop"));
    for d in [runtime.to_path_buf(), runtime.join("pulse"), runtime.join("bin")] {
        io(d.display(), fs::DirBuilder::new().mode(0o700).create(&d))?;
    }
    io("pulse", symlink(format!("{host}/pulse/native"), runtime.join("pulse/native")))?;
    let mut pw: Vec<String> = fs::read_dir(host).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with("pipewire")).collect();
    pw.sort();
    if pw.is_empty() {
        pw.push("pipewire*".into()); // the shell's unmatched glob
    }
    for n in pw {
        io(&n, symlink(format!("{host}/{n}"), runtime.join(&n)))?;
    }
    // These are one per user, inside this desktop or not: systemd (systemctl --user), plus any
    // other sockets the user's tools share (settings.json `session_links`, names in the runtime
    // dir, empty by default).
    for f in ["systemd".to_owned()].into_iter().chain(setting_list("session_links")) {
        let f = f.as_str();
        if Path::new(&format!("{host}/{f}")).exists() {
            io(f, symlink(format!("{host}/{f}"), runtime.join(f)))?;
        }
    }
    // Its own Plasma config, so the panel, wallpaper and shortcuts here don't touch the Frame's own.
    io(config.display(), fs::create_dir_all(&config))?;
    io(state.display(), fs::create_dir_all(&state))?;
    // KWin set up for our pointer: focus on click, because it crosses other windows on its way,
    // and no edge barrier, because that holds back absolute motion too. No file indexing either
    // (docs/efficiency-plan.md 6): unlike the Frame's own config, it doesn't skip network
    // drive mounts, where each file read can download the file.
    for (file, group, key, value) in [
        ("kwinrc", "Windows", "FocusPolicy", "ClickToFocus"),
        ("kwinrc", "EdgeBarrier", "EdgeBarrier", "0"),
        ("kwinrc", "EdgeBarrier", "CornerBarrier", "false"),
        ("baloofilerc", "Basic Settings", "Indexing-Enabled", "false"),
    ] {
        let f = config.join(file);
        check("kwriteconfig6", Command::new("kwriteconfig6").arg("--file").arg(&f).args(["--group", group, "--key", key, value]).status())?;
    }
    // plasma-session starts KWin through kwin_wayland_wrapper, so I shadow it to get the headless
    // screens.
    io("kwin_wayland_wrapper", symlink(exe(), runtime.join("bin/kwin_wayland_wrapper")))?;
    // Flatpak runs its sandboxes through this, for the document portal's path (doc_link).
    io("flatpak-bwrap", symlink(exe(), runtime.join("bin/flatpak-bwrap")))?;
    let path = format!("{}:{}", runtime.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    unsafe {
        std::env::set_var("PATH", path);
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("DISPLAY");
        std::env::set_var("XDG_RUNTIME_DIR", runtime);
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_STATE_HOME", &state);
        std::env::set_var("FLATPAK_BWRAP", runtime.join("bin/flatpak-bwrap"));
    }
    // not exec, because the runtime dir gets cleaned up after
    match Command::new("dbus-run-session").arg("startplasma-wayland").status() {
        Ok(s) => Ok(code(s)),
        Err(e) => {
            eprintln!("dbus-run-session: {e}");
            Err(127)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-home-session-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake /proc: (pid, ppid, comm, environ, cmdline).
    fn fake_proc(tag: &str, procs: &[(u32, u32, &str, &[&str], &str)]) -> PathBuf {
        let d = temp(tag);
        for (pid, ppid, comm, env, cmd) in procs {
            let p = d.join(pid.to_string());
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            fs::write(p.join("stat"), format!("{pid} ({comm} x) S {ppid} 1 1 0")).unwrap();
            fs::write(p.join("environ"), env.iter().map(|e| format!("{e}\0")).collect::<String>()).unwrap();
            fs::write(p.join("cmdline"), cmd.replace(' ', "\0")).unwrap();
        }
        fs::create_dir_all(d.join("self")).unwrap(); // not a pid
        d
    }

    #[test]
    fn steam_env() {
        for v in ["LD_LIBRARY_PATH", "LD_PRELOAD", "STEAM_RUNTIME", "STEAM_", "SteamAppId", "SRT_X", "PRESSURE_VESSEL_A", "MANGOHUD_CONFIG", "ENABLE_VK_LAYER_VALVE_steam_overlay_1", "STEAMVIDEOTOKEN"] {
            assert!(steam_var(v, false) && steam_var(v, true), "{v}");
        }
        for v in ["ENABLE_GAMESCOPE_WSI", "XDG_DESKTOP_PORTAL_DIR"] {
            assert!(!steam_var(v, false) && steam_var(v, true), "{v}");
        }
        for v in ["PATH", "HOME", "STEAMVIDEOTOKENX", "LD_LIBRARY", "XDG_RUNTIME_DIR", "steam_x", "MESA_GL_VERSION_OVERRIDE"] {
            assert!(!steam_var(v, true), "{v}");
        }
    }

    #[test]
    fn process_tree() {
        let p = fake_proc("tree", &[(1, 0, "systemd", &[], ""), (50, 1, "worker", &[], ""), (51, 50, "bash", &[], ""), (60, 51, "job", &[], ""),
            (52, 50, "bash", &[], ""), (70, 1, "bash", &[], ""), (71, 70, "job", &[], ""), (80, 1, "workerx", &[], ""), (90, 1, "worker", &[], "")]);
        assert_eq!(named(&p, "worker"), [50, 90]);
        assert_eq!(tree(&p, 50), [50, 51, 60, 52]);
        assert_eq!(tree(&p, 90), [90]);
        assert_eq!(children(&p, 1), [50, 70, 80, 90]);
        let _ = fs::remove_dir_all(p);
    }

    #[test]
    fn panels_match_and_session_env() {
        assert!(is_panels(b"/home/u/control-center/target/release/cc-panels\0--for\00\0"));
        assert!(!is_panels(b"/home/u/control-center/cc-panels\0--for\00\0")); // the wrapper itself
        assert!(!is_panels(b"nice\0-n\00\0/x/distrobox\0enter\0control-center\0--\0/x/target/release/cc-panels\0"));
        assert!(!is_panels(b"/bin/sh\0/x/target/release/cc-panels\0"));
        let p = fake_proc("env", &[
            (10, 1, "plasmashell", &["XDG_RUNTIME_DIR=/run/user/1000/other", "WAYLAND_DISPLAY=wrong"], ""),
            (20, 1, "plasmashell", &["HOME=/h", "WAYLAND_DISPLAY=wayland-0", "XDG_RUNTIME_DIR=/run/user/1000/cc-desktop", "DISPLAY=:1", "DBUS_SESSION_BUS_ADDRESS=unix:path=/x", "XAUTHORITY=/a", "XDG_RUNTIME_DIRX=no"], ""),
        ]);
        let cache = temp("cache");
        session_env(&p, &cache, "/run/user/1000");
        assert_eq!(fs::read_to_string(cache.join("desktop-session.env")).unwrap(),
            "WAYLAND_DISPLAY=wayland-0\nXDG_RUNTIME_DIR=/run/user/1000/cc-desktop\nDISPLAY=:1\nDBUS_SESSION_BUS_ADDRESS=unix:path=/x\nXAUTHORITY=/a\n");
        session_env(&p, &cache, "/run/user/1001"); // no such session: the file goes
        assert!(!cache.join("desktop-session.env").exists());
        let _ = (fs::remove_dir_all(p), fs::remove_dir_all(cache));
    }

    #[test]
    fn log_rotation() {
        let cache = temp("log");
        drop(start_log(&cache, "2026-10-03 12:00:00").unwrap());
        assert_eq!(fs::read_to_string(cache.join("cc-panels.log")).unwrap(), "=== cc-panels 2026-10-03 12:00:00 ===\n");
        assert!(!cache.join("cc-panels.log.1").exists());
        fs::write(cache.join("cc-panels.log"), vec![b'x'; 5_000_000]).unwrap(); // at 5 MB: kept
        drop(start_log(&cache, "b").unwrap());
        assert!(!cache.join("cc-panels.log.1").exists());
        drop(start_log(&cache, "c").unwrap()); // past it: moved
        assert_eq!(fs::metadata(cache.join("cc-panels.log.1")).unwrap().len(), 5_000_000 + 20);
        assert_eq!(fs::read_to_string(cache.join("cc-panels.log")).unwrap(), "=== cc-panels c ===\n");
        assert_eq!(stamp().len(), 19);
        let _ = fs::remove_dir_all(cache);
    }

    #[test]
    fn doc_link_in_app_sandbox_only() {
        let a = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let args = a(&["--args", "44", "--", "vivaldi"]);
        let app = b"--tmpfs\0/run/user/1000\0--bind\0/run/user/1000/cc-desktop/doc/by-app/x\0/run/flatpak/doc\0";
        assert_eq!(doc_link(&args, app, 1000),
            a(&["--args", "44", "--symlink", "/run/flatpak/doc", "/run/user/1000/cc-desktop/doc", "--", "vivaldi"]));
        // the D-Bus proxy's sandbox, the host's own portal, no --args: unchanged
        assert_eq!(doc_link(&args, b"--bind\0/run\0/run\0", 1000), args);
        assert_eq!(doc_link(&args, b"--bind\0/run/user/1000/doc/by-app/x\0/run/flatpak/doc\0", 1000), args);
        assert_eq!(doc_link(&a(&["--ro-bind", "/", "/", "true"]), app, 1000), a(&["--ro-bind", "/", "/", "true"]));
    }

    #[test]
    fn commands() {
        // session/cc-launch's systemd-run, its bash -c "exec ... > /tmp/cc-desktop.log 2>&1" as unit properties
        assert_eq!(session_unit(Path::new("/r/target/aarch64-unknown-linux-musl/release/cc-home")).join(" "),
            "systemd-run --user --collect --quiet --unit cc-desktop -p TimeoutStopSec=10 -p StandardOutput=truncate:/tmp/cc-desktop.log -p StandardError=inherit /r/target/aarch64-unknown-linux-musl/release/cc-home session");
        let a: Vec<OsString> = ["/r/target/release/cc-panels", "--for", "0"].map(OsString::from).into();
        assert_eq!(box_argv("0", "/h", Some("control-center"), &a), ["nice", "-n", "0", "/h/.local/bin/distrobox", "enter", "control-center", "--", "/r/target/release/cc-panels", "--for", "0"]);
        assert_eq!(box_argv("10", "/h", None, &a), ["nice", "-n", "10", "/r/target/release/cc-panels", "--for", "0"]);
        assert_eq!(root_of(Path::new("/r/target/aarch64-unknown-linux-musl/release/cc-home")), Path::new("/r"));
        assert_eq!(root_of(Path::new("/usr/lib/command-center/cc-home")), Path::new("/usr/lib/command-center"));
        assert_eq!(kwin_args("2560x1440", &["--xwayland".into()]), ["--virtual", "--width", "2560", "--height", "1440", "--output-count", "3", "--no-lockscreen", "--xwayland"]);
        let m = "# c\nMESA_GL_VERSION_OVERRIDE=4.3\n\n#VK_ICD_FILENAMES=\"a\"\nVRCOMPOSITOR_TU_DEBUG=sysmem,preempt\nQ=\"x y\"\n";
        assert_eq!(assignments(m), [("MESA_GL_VERSION_OVERRIDE".into(), "4.3".into()), ("VRCOMPOSITOR_TU_DEBUG".into(), "sysmem,preempt".into()), ("Q".into(), "x y".into())]);
    }
}
