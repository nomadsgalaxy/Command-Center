//! What the installer found and what it's going to do: the machine (a Frame or a host), what's
//! installed, what's missing, and the steps for an install, update or removal. Only look() touches
//! the system to gather the facts. Everything after that is plain data, which is why it can be
//! tested here.
use std::path::{Path, PathBuf};

pub const ONE_LINE: &str = "curl -fsSL https://raw.githubusercontent.com/nomadsgalaxy/Command-Center/main/install | sh";
const REPO: &str = "https://github.com/nomadsgalaxy/Command-Center.git";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Frame,
    Host,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Install,
    Update,
    Remove,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Install => "Install",
            Action::Update => "Update",
            Action::Remove => "Remove",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub name: String,
    pub w: u64,
    pub h: u64,
}

/// The Frame's checkout. Missing gets cloned, on main with nothing changed gets pulled, and
/// anything else gets built as it is (with the reason why).
#[derive(Clone, Debug, PartialEq)]
pub enum Repo {
    Missing,
    Pull,
    Keep(String),
}

#[derive(Clone, Debug)]
pub struct Facts {
    pub kind: Kind,
    pub arch: String,
    pub host: String,
    pub home: PathBuf,
    /// what's installed, as its version (None when nothing is yet)
    pub installed: Option<String>,
    /// prerequisites that aren't there, as (what, how to fix it)
    pub missing: Vec<(String, String)>,
    // Frame only
    pub repo: PathBuf,
    pub repo_state: Repo,
    pub desktop_open: bool,
    // host only
    pub monitors: Vec<Monitor>,
    pub shared: Vec<usize>,
    pub announcing: bool,
}

/// The Frame is aarch64 SteamOS with SteamVR; anything else is a host.
pub fn kind(arch: &str, os_release: &str, steamvr: bool) -> Kind {
    let steamos = os_release.lines().any(|l| l.trim() == "ID=steamos" || l.trim() == "ID=\"steamos\"");
    if arch == "aarch64" && steamos && steamvr { Kind::Frame } else { Kind::Host }
}

/// kscreen-doctor -j's enabled outputs in krdp's --monitor order (by priority). Same reading as
/// outputs() in cc-host's share.rs, so the numbers match.
pub fn monitors(json: &str) -> Vec<Monitor> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let mut outs: Vec<&serde_json::Value> = v["outputs"].as_array().map(|a| a.iter().filter(|o| o["enabled"] == true).collect()).unwrap_or_default();
    outs.sort_by_key(|o| o["priority"].as_i64().unwrap_or(0));
    outs.iter().map(|o| Monitor { name: o["name"].as_str().unwrap_or("").into(), w: o["size"]["width"].as_u64().unwrap_or(0), h: o["size"]["height"].as_u64().unwrap_or(0) }).collect()
}

/// The actions on offer: Install when nothing's there, otherwise Update and Remove.
pub fn actions(f: &Facts) -> Vec<Action> {
    if f.installed.is_some() { vec![Action::Update, Action::Remove] } else { vec![Action::Install] }
}

/// The monitors ticked to start with: the ones shared now, or all of them if none are.
pub fn ticked(f: &Facts) -> Vec<bool> {
    let now: Vec<usize> = f.shared.iter().copied().filter(|&i| i < f.monitors.len()).collect();
    (0..f.monitors.len()).map(|i| now.is_empty() || now.contains(&i)).collect()
}

/// Monitors given as arguments (the non-interactive install). Each one has to be a number below n.
pub fn parse_monitors(args: &[String], n: usize) -> Result<Vec<usize>, String> {
    let mut v = vec![];
    for a in args {
        match a.parse::<usize>() {
            Ok(i) if i < n => v.push(i),
            _ => return Err(format!("{a} isn't a monitor here (there are {n}, numbered from 0)")),
        }
    }
    Ok(v)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    Run(Vec<String>),
    /// cc-host for this arch from the release, checked against SHA256SUMS, to this path
    Download(PathBuf),
    /// nothing to do, plus why
    Skip(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub title: String,
    pub what: Do,
    /// what to do when it fails
    pub fix: String,
}

fn step(title: &str, what: Do, fix: &str) -> Step {
    Step { title: title.into(), what, fix: fix.into() }
}

fn run(p: &Path, args: &[&str]) -> Do {
    let mut v = vec![p.display().to_string()];
    v.extend(args.iter().map(|a| a.to_string()));
    Do::Run(v)
}

/// The steps for an action. On a host it also takes the monitors to share and whether to announce.
pub fn plan(f: &Facts, action: Action, share: &[usize], announce: bool, work: &Path) -> Vec<Step> {
    let again = "Run the installer again: it picks up where it stopped.";
    match (f.kind, action) {
        (Kind::Frame, Action::Remove) => vec![step("Remove Command Center", run(&f.repo.join("cc-home"), &["install", "remove"]), again)],
        (Kind::Frame, _) => {
            let repo = f.repo.display().to_string();
            let code = match &f.repo_state {
                Repo::Missing => Do::Run(vec!["git".into(), "clone".into(), "-q".into(), std::env::var("CC_REPO").unwrap_or(REPO.into()), repo]),
                Repo::Pull => Do::Run(vec!["git".into(), "-C".into(), repo, "pull".into(), "--ff-only".into(), "-q".into()]),
                Repo::Keep(why) => Do::Skip(why.clone()),
            };
            let sh = f.repo.join("install.sh");
            vec![
                step("Get the code", code, "Check the Frame's Wi-Fi, then run the installer again."),
                step("Set up the build container (the first time takes a while)", run(&sh, &["container"]), "The log says what failed. Run the installer again: a download that broke halfway usually works the second time."),
                step("Build cc-home", run(&sh, &["cc-home"]), again),
                step("Build Command Center: FreeRDP, the Desktop and the pointer", run(&f.repo.join("cc-home"), &["install"]), again),
                step("Make Desktop in the SteamVR launcher open Command Center", run(&f.repo.join("cc-home"), &["install", "desktop"]), again),
            ]
        }
        (Kind::Host, Action::Remove) => vec![step("Stop and remove cc-host", run(&f.home.join(".local/share/control-center/cc-host"), &["uninstall"]), again)],
        (Kind::Host, _) => {
            let host = work.join("cc-host");
            let mut args: Vec<String> = vec!["install".into()];
            args.extend(share.iter().map(usize::to_string));
            args.push(if announce { "--announce" } else { "--no-announce" }.into());
            let mut cmd = vec![host.display().to_string()];
            cmd.extend(args);
            vec![
                step("Download cc-host", Do::Download(host), "Check that this machine is online, then run the installer again. If the checksum doesn't match twice, the release itself is broken: please open an issue on GitHub."),
                step("Install and start cc-host", Do::Run(cmd), "The log says what failed. Fix that and run the installer again."),
            ]
        }
    }
}

/// The lines of cc-host install's output that need you to do something: its checklist's "need"
/// items and the firewall commands.
pub fn needs_you(log: &[String]) -> Vec<String> {
    let mut out = vec![];
    let mut fw = false;
    for l in log {
        if l.starts_with("firewall:") || l.starts_with("a firewall is active") {
            fw = true;
            out.push(l.clone());
        } else if fw && (l.starts_with("  ") || l.starts_with("rerun as")) && !l.starts_with("  ok") && !l.starts_with("  need") && !l.starts_with("  note") {
            out.push(l.clone());
        } else {
            fw = false;
            if let Some(n) = l.strip_prefix("  need ") {
                out.push(format!("  {n}"));
            }
        }
    }
    out
}

/// "monitors 0 and 1", "monitor 2".
pub fn monitor_list(share: &[usize]) -> String {
    let n: Vec<String> = share.iter().map(usize::to_string).collect();
    match n.len() {
        0 => "no monitors".into(),
        1 => format!("monitor {}", n[0]),
        _ => format!("monitors {} and {}", n[..n.len() - 1].join(", "), n[n.len() - 1]),
    }
}

/// What the Done screen says. `restart`: the running SteamVR doesn't have this pointer driver.
pub fn done(f: &Facts, action: Action, share: &[usize], announce: bool, log: &[String], restart: bool) -> Vec<String> {
    let mut v = vec![];
    match (f.kind, action) {
        (Kind::Frame, Action::Remove) => {
            v.push("Command Center is removed. Desktop in the SteamVR launcher opens SteamOS's own desktop again.".into());
            v.push(String::new());
            v.push(format!("I kept the code ({}), the build container and your settings, so a reinstall is quick. To remove them too:", f.repo.display()));
            v.push(format!("  distrobox rm control-center; rm -rf {} ~/.config/control-center", f.repo.display()));
        }
        (Kind::Frame, a) => {
            v.push(format!("Command Center is {}.", if a == Action::Install { "installed" } else { "updated" }));
            v.push(String::new());
            v.push("Next:".into());
            if f.desktop_open {
                v.push("  1. Your Desktop is still open with the old version. Close it, then open Desktop from the SteamVR launcher again.".into());
            } else {
                v.push("  1. Open Desktop from the SteamVR launcher.".into());
            }
            v.push("  2. To see a computer from here, run the same command on it:".into());
            v.push(format!("     {ONE_LINE}"));
            if restart {
                v.push(String::new());
                v.push("The pointer driver changed. SteamVR loads drivers when it starts, so the new one".into());
                v.push("works after SteamVR restarts. Restarting the Frame does that.".into());
            }
        }
        (Kind::Host, Action::Remove) => {
            v.push(format!("Command Center is removed from {}. Your pairings and keys stay in ~/.config/control-center.", f.host));
        }
        (Kind::Host, a) => {
            v.push(format!("Command Center is {} on {}, sharing {}.", if a == Action::Install { "installed" } else { "updated" }, f.host, monitor_list(share)));
            let need = needs_you(log);
            if !need.is_empty() {
                v.push(String::new());
                v.push("These need you:".into());
                v.extend(need);
            }
            v.push(String::new());
            v.push("Next, pair it with the Frame:".into());
            v.push("  1. Here, run: cc-share pair".into());
            v.push("     A 6-digit key fills the screen for 5 minutes.".into());
            v.push(format!("  2. On the Frame, open Workspace, then Machines, then Add machine. Pick {}, press Pair,", f.host));
            v.push("     and type the key, or press Pair by looking and look at this screen.".into());
            if !announce {
                v.push("  The Frame lists this machine only while it announces itself (cc-share announce on).".into());
                v.push("  Without that, pair from a terminal on the Frame: cc-home machine pair <this machine's address> -".into());
            }
        }
    }
    v
}

/// The SHA-256 that SHA256SUMS lists for a file.
pub fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, n) = l.split_once(char::is_whitespace)?;
        (n.trim_start().trim_start_matches('*') == name).then(|| h.to_lowercase())
    })
}

// ---------------------------------------------------------------- looking at the system

fn have(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| std::fs::metadata(d.join(cmd)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)))
}

fn out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

fn running(name: &str) -> bool {
    std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|p| std::fs::read_to_string(p.path().join("comm")).is_ok_and(|c| c.trim_end() == name))
}

fn free_gb(p: &Path) -> u64 {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else { return u64::MAX };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return u64::MAX; // can't tell, so don't block on it
    }
    s.f_bavail as u64 * s.f_frsize as u64 >> 30
}

/// The package command for this distro, worded the way cc-host's need() says it.
fn pkg(name: &str) -> String {
    for (pm, line) in [("pacman", "sudo pacman -S --needed"), ("dnf", "sudo dnf install"), ("apt", "sudo apt install"), ("zypper", "sudo zypper install")] {
        if have(pm) {
            return format!("Install it with: {line} {name}");
        }
    }
    format!("Install {name} from your distro's packages.")
}

pub fn look() -> Facts {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let arch = std::env::consts::ARCH.to_owned();
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let kind = match std::env::var("CC_AS").as_deref() {
        Ok("frame") => Kind::Frame, // for testing one on the other
        Ok("host") => Kind::Host,
        _ => kind(&arch, &os, Path::new("/opt/steamvr/bin/linuxarm64/vrpathreg").exists()),
    };
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_owned();
    let repo = std::env::var_os("CC_DIR").map(PathBuf::from).unwrap_or_else(|| home.join("control-center"));
    let mut f = Facts { kind, arch, host, home: home.clone(), installed: None, missing: vec![], repo: repo.clone(), repo_state: Repo::Missing, desktop_open: false, monitors: vec![], shared: vec![], announcing: false };
    let mut miss = |what: &str, fix: String| f.missing.push((what.into(), fix));
    if unsafe { libc::geteuid() } == 0 {
        miss("You're running it as root", "Run it as your own user, without sudo.".into());
    }
    match kind {
        Kind::Frame => {
            for c in ["git", "curl", "podman"] {
                if !have(c) {
                    miss(c, format!("{c} comes with SteamOS. Update SteamOS in Settings, then run this again."));
                }
            }
            let bus = format!("/run/user/{}/bus", unsafe { libc::getuid() });
            if !Path::new(&bus).exists() {
                miss("Your session's bus", "Run this in Konsole on the Frame's desktop, or over SSH as your own user.".into());
            }
            if free_gb(&home) < 10 {
                miss("10 GB of free space", "The build container and the build need about 10 GB. Free some space and run this again.".into());
            }
            if repo.exists() && !repo.join(".git").exists() {
                miss(&format!("{} is there but isn't a git checkout", repo.display()), format!("Move it out of the way (mv {0} {0}.old), then run this again.", repo.display()));
            }
            let git = |a: &[&str]| out("git", &[&["-C", &repo.display().to_string()][..], a].concat());
            f.repo_state = if !repo.join(".git").exists() {
                Repo::Missing
            } else {
                match (git(&["symbolic-ref", "--short", "HEAD"]).as_deref(), git(&["status", "--porcelain"]).as_deref()) {
                    (Some("main"), Some("")) => Repo::Pull,
                    (Some("main"), _) => Repo::Keep("it has local changes, so I'll build it as it is".into()),
                    (Some(b), _) => Repo::Keep(format!("it's on the branch {b}, so I'll build it as it is")),
                    (None, _) => Repo::Keep("it isn't on a branch, so I'll build it as it is".into()),
                }
            };
            if home.join(".local/bin/cc-home").exists() && repo.join(".git").exists() {
                f.installed = Some(git(&["describe", "--tags", "--match", "v*", "--always", "--dirty"]).unwrap_or_else(|| "a build from source".into()));
            }
            f.desktop_open = running("cc-panels");
        }
        Kind::Host => {
            for (c, p) in [("krdpserver", "krdp"), ("kscreen-doctor", "libkscreen"), ("systemctl", "systemd")] {
                if !have(c) {
                    miss(c, pkg(p));
                }
            }
            if !have("curl") {
                miss("curl", pkg("curl"));
            }
            if have("kscreen-doctor") {
                f.monitors = monitors(&out("kscreen-doctor", &["-j"]).unwrap_or_default());
                if f.monitors.is_empty() {
                    miss("Monitors", "kscreen-doctor lists none. Run this in Konsole inside your KDE Plasma session (Wayland).".into());
                }
            }
            let conf = home.join(".config/control-center");
            f.shared = std::fs::read_to_string(conf.join("shared")).unwrap_or_default().lines().filter_map(|l| l.trim().parse().ok()).collect();
            let cc_host = home.join(".local/share/control-center/cc-host");
            if cc_host.exists() {
                f.installed = Some(out(&cc_host.display().to_string(), &["version"]).unwrap_or_else(|| "cc-host".into()));
            }
            f.announcing = out("systemctl", &["--user", "is-active", "control-center-announce.service"]).is_some();
        }
    }
    f
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn frame() -> Facts {
        Facts { kind: Kind::Frame, arch: "aarch64".into(), host: "frame".into(), home: "/home/u".into(), installed: None, missing: vec![], repo: "/home/u/control-center".into(), repo_state: Repo::Missing, desktop_open: false, monitors: vec![], shared: vec![], announcing: false }
    }

    pub fn host() -> Facts {
        let m = |n: &str, w, h| Monitor { name: n.into(), w, h };
        Facts { kind: Kind::Host, arch: "x86_64".into(), host: "desk".into(), monitors: vec![m("DP-1", 5120, 1440), m("DP-3", 1440, 2560)], ..frame() }
    }

    #[test]
    fn tells_the_frame_from_a_host() {
        let steamos = "NAME=\"SteamOS\"\nID=steamos\nID_LIKE=arch\n";
        assert_eq!(kind("aarch64", steamos, true), Kind::Frame);
        assert_eq!(kind("aarch64", steamos, false), Kind::Host, "no SteamVR");
        assert_eq!(kind("x86_64", steamos, true), Kind::Host, "a Steam Deck or a desktop running SteamOS");
        assert_eq!(kind("aarch64", "ID=fedora\n", true), Kind::Host);
    }

    #[test]
    fn reads_monitors_in_krdp_order() {
        let j = r#"{"outputs":[{"name":"DP-3","enabled":true,"priority":2,"size":{"width":1440,"height":2560}},
            {"name":"HDMI-A-1","enabled":false,"priority":3,"size":{"width":1920,"height":1080}},
            {"name":"DP-1","enabled":true,"priority":1,"size":{"width":5120,"height":1440}}]}"#;
        assert_eq!(monitors(j), host().monitors);
        assert!(monitors("not json").is_empty());
    }

    #[test]
    fn offers_install_or_update_and_remove() {
        let mut f = frame();
        assert_eq!(actions(&f), vec![Action::Install]);
        f.installed = Some("v0.1.0".into());
        assert_eq!(actions(&f), vec![Action::Update, Action::Remove]);
    }

    #[test]
    fn ticks_every_monitor_or_the_shared_ones() {
        let mut f = host();
        assert_eq!(ticked(&f), vec![true, true]);
        f.shared = vec![1];
        assert_eq!(ticked(&f), vec![false, true]);
        f.shared = vec![7]; // a monitor that's gone should act like none were shared
        assert_eq!(ticked(&f), vec![true, true]);
        assert_eq!(parse_monitors(&["1".into(), "0".into()], 2), Ok(vec![1, 0]));
        assert!(parse_monitors(&["2".into()], 2).is_err());
        assert!(parse_monitors(&["x".into()], 2).is_err());
    }

    #[test]
    fn plans_the_frame() {
        let f = frame();
        let p = plan(&f, Action::Install, &[], false, Path::new("/tmp/w"));
        assert_eq!(p.len(), 5);
        assert_eq!(p[0].what, Do::Run(vec!["git".into(), "clone".into(), "-q".into(), REPO.into(), "/home/u/control-center".into()]));
        assert_eq!(p[1].what, Do::Run(vec!["/home/u/control-center/install.sh".into(), "container".into()]));
        assert_eq!(p[3].what, Do::Run(vec!["/home/u/control-center/cc-home".into(), "install".into()]));
        let mut f = frame();
        f.repo_state = Repo::Pull;
        assert_eq!(plan(&f, Action::Update, &[], false, Path::new("/w"))[0].what, Do::Run(vec!["git".into(), "-C".into(), "/home/u/control-center".into(), "pull".into(), "--ff-only".into(), "-q".into()]));
        f.repo_state = Repo::Keep("on a branch".into());
        assert_eq!(plan(&f, Action::Update, &[], false, Path::new("/w"))[0].what, Do::Skip("on a branch".into()));
        assert_eq!(plan(&f, Action::Remove, &[], false, Path::new("/w")).len(), 1);
    }

    #[test]
    fn plans_a_host() {
        let f = host();
        let p = plan(&f, Action::Install, &[0, 1], true, Path::new("/tmp/w"));
        assert_eq!(p[0].what, Do::Download("/tmp/w/cc-host".into()));
        assert_eq!(p[1].what, Do::Run(["/tmp/w/cc-host", "install", "0", "1", "--announce"].map(String::from).to_vec()));
        let p = plan(&f, Action::Remove, &[], false, Path::new("/tmp/w"));
        assert_eq!(p[0].what, Do::Run(["/home/u/.local/share/control-center/cc-host", "uninstall"].map(String::from).to_vec()));
    }

    #[test]
    fn picks_out_what_needs_the_user() {
        let log: Vec<String> = ["sharing as user u on desk: ports 3400", "firewall: to add the pairing and paired-Frame ports, run (or pass --firewall):", "  sudo ufw allow 3399:3449/tcp", "checklist:", "  ok   krdpserver", "  need avahi-publish: sudo pacman -S --needed avahi", "  note linger off"].map(String::from).to_vec();
        assert_eq!(needs_you(&log), vec!["firewall: to add the pairing and paired-Frame ports, run (or pass --firewall):", "  sudo ufw allow 3399:3449/tcp", "  avahi-publish: sudo pacman -S --needed avahi"]);
    }

    #[test]
    fn says_what_is_next() {
        assert_eq!(monitor_list(&[0, 1, 2]), "monitors 0, 1 and 2");
        assert_eq!(monitor_list(&[1]), "monitor 1");
        let d = done(&host(), Action::Install, &[0, 1], false, &[], false).join("\n");
        assert!(d.contains("installed on desk, sharing monitors 0 and 1") && d.contains("cc-share pair") && d.contains("Pick desk") && d.contains("cc-share announce on"));
        let mut f = frame();
        f.desktop_open = true;
        let d = done(&f, Action::Update, &[], false, &[], true).join("\n");
        assert!(d.contains("Close it, then open Desktop") && d.contains("pointer driver changed"));
        assert!(!done(&frame(), Action::Install, &[], false, &[], false).join("\n").contains("pointer"));
    }

    #[test]
    fn finds_a_checksum() {
        let s = "abc123  cc-install-x86_64\nDEF456 *cc-host-aarch64\n";
        assert_eq!(sum_for(s, "cc-host-aarch64").as_deref(), Some("def456"));
        assert_eq!(sum_for(s, "cc-install-x86_64").as_deref(), Some("abc123"));
        assert_eq!(sum_for(s, "cc-host"), None);
    }
}
