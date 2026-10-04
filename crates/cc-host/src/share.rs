//! What cc-share used to do in bash, now in cc-host (docs/rust-host.md). It installs the host (units,
//! certificate, launcher), starts and stops it, and runs the guard, announcing, paired Frames' servers,
//! pairing's firewall check and the self-check. There's no bash, jq, ss or pkill: it reads
//! kscreen-doctor's JSON with serde, sockets from /proc/net/tcp and processes from /proc. systemctl,
//! kscreen-doctor, krdpserver, avahi-publish and the firewall tools stay what they are, the system's.
//! cc-share is a symlink to cc-host (main.rs checks argv[0]), so every documented command keeps working.
use serde_json::{Value, json};
use std::io::{BufRead, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

const NETS: [&str; 3] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]; // Private ranges only (docs/agent.md M-C).

pub struct Env {
    home: PathBuf,
    dir: PathBuf,
    units: PathBuf,
    share: PathBuf,
    max: u64,
}

pub fn env() -> Env {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    Env {
        dir: home.join(".config/control-center"),
        units: home.join(".config/systemd/user"),
        share: home.join(".local/share/control-center"),
        max: std::env::var("CC_MAX_PIXELS").ok().and_then(|v| v.parse().ok()).unwrap_or(4278016),
        home,
    }
}

/// cc-host came from a distro package (it runs from /usr), so the package owns the binaries, the
/// units, the launcher and krdp's grants (docs/packaging.md). Otherwise it's a user install.
pub fn packaged() -> bool {
    // Read from /proc, since current_exe() fails once an update has replaced the file.
    std::fs::read_link("/proc/self/exe").is_ok_and(|p| p.starts_with("/usr/"))
}

/// The package's private directory with our patched krdp (Arch: /usr/lib, Fedora and Debian: /usr/libexec).
pub fn libexec() -> Option<PathBuf> {
    ["/usr/lib/command-center", "/usr/libexec/command-center"].iter().map(PathBuf::from).find(|d| d.join("krdpserver").exists())
}

/// The krdpserver to run: the package's patched one when it's there, else an older install's.
fn krdp(e: &Env, window: bool) -> Option<PathBuf> {
    match (libexec(), window) {
        (Some(d), false) => Some(d.join("krdpserver")),
        (Some(d), true) => Some(d.join("krdpserver-window")).filter(|p| p.exists()),
        (None, false) => Some(PathBuf::from("/usr/bin/krdpserver")),
        (None, true) => e.window_server(),
    }
}

/// This program was replaced on disk by an update while it runs (/proc/self/exe ends in "(deleted)").
pub fn replaced() -> bool {
    std::fs::read_link("/proc/self/exe").is_ok_and(|p| p.to_string_lossy().ends_with(" (deleted)"))
}

/// The installed package's version and the command that removes it, from whichever package manager has it.
fn package() -> Option<(String, &'static str)> {
    [(["pacman", "-Q", "command-center-host"].as_slice(), "sudo pacman -R command-center-host"),
     (&["rpm", "-q", "--qf", "%{VERSION}-%{RELEASE}", "command-center-host"], "sudo dnf remove command-center-host"),
     (&["dpkg-query", "-W", "-f", "${Version}", "command-center-host"], "sudo apt remove command-center-host")]
        .into_iter().find_map(|(q, rm)| {
            let (ok, out) = run(q[0], &q[1..]);
            let v = out.trim().trim_start_matches("command-center-host ").to_owned();
            (ok && !v.is_empty()).then_some((v, rm))
        })
}

// ---------------------------------------------------------------- the system

fn run(cmd: &str, args: &[&str]) -> (bool, String) {
    match Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output() {
        Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()),
        Err(_) => (false, String::new()),
    }
}

/// Runs systemctl --user quietly and returns whether it succeeded.
fn sysq(args: &[&str]) -> bool {
    let mut a = vec!["--user"];
    a.extend_from_slice(args);
    run("systemctl", &a).0
}

/// Runs systemctl --user with its output shown, the way bash ran it without redirection.
fn sys(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).status().is_ok_and(|s| s.success())
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| {
        let f = d.join(cmd);
        std::fs::metadata(&f).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }))
}

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_owned()
}

fn user() -> String {
    std::env::var("USER").unwrap_or_default()
}

/// Reads TCP sockets from /proc/net/tcp{,6} as (local port, state, remote address). IPv4-mapped IPv6
/// addresses come back as IPv4, which is what ss shows.
pub fn sockets() -> Vec<(u16, u8, std::net::IpAddr)> {
    let ip = |hex: &str| -> Option<std::net::IpAddr> {
        let b: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect::<Option<_>>()?;
        // The kernel prints each 32-bit word in host (little-endian) order.
        let words: Vec<u8> = b.chunks(4).flat_map(|w| w.iter().rev().copied().collect::<Vec<_>>()).collect();
        Some(match words.len() {
            4 => std::net::IpAddr::from([words[0], words[1], words[2], words[3]]),
            16 => {
                let v6 = std::net::Ipv6Addr::from(<[u8; 16]>::try_from(words).ok()?);
                v6.to_ipv4_mapped().map_or(std::net::IpAddr::V6(v6), std::net::IpAddr::V4)
            }
            _ => return None,
        })
    };
    ["/proc/net/tcp", "/proc/net/tcp6"].iter().flat_map(|f| std::fs::read_to_string(f).unwrap_or_default().lines().skip(1).filter_map(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        let port = u16::from_str_radix(w.get(1)?.rsplit(':').next()?, 16).ok()?;
        let remote = ip(w.get(2)?.split(':').next()?)?;
        Some((port, u8::from_str_radix(w.get(3)?, 16).ok()?, remote))
    }).collect::<Vec<_>>()).collect()
}

/// Counts established connections on any share port: slot 0's (3400+m) and every paired Frame's (3400+10k+m).
fn viewers() -> usize {
    sockets().iter().filter(|(p, s, _)| *s == 1 && (3400..=3449).contains(p)).count()
}

pub fn listening(port: u16) -> bool {
    sockets().iter().any(|(p, s, _)| *s == 0x0a && *p == port)
}

/// kscreen-doctor -j's enabled outputs in krdp's --monitor order, which is by priority with the primary first.
fn outputs() -> Vec<Value> {
    let v: Value = serde_json::from_str(&run("kscreen-doctor", &["-j"]).1).unwrap_or(json!({}));
    let mut outs: Vec<Value> = v["outputs"].as_array().cloned().unwrap_or_default().into_iter().filter(|o| o["enabled"] == json!(true)).collect();
    outs.sort_by_key(|o| o["priority"].as_i64().unwrap_or(0));
    outs
}

fn output_name(m: usize) -> Option<String> {
    outputs().get(m).and_then(|o| o["name"].as_str()).map(str::to_owned)
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim_end_matches('\n').to_owned())
}

fn valid_frame(f: &str) -> bool {
    cc_proto::server::valid_name(f)
}

fn notify(text: &str) {
    if have("notify-send") {
        let _ = run("notify-send", &["-a", "Command Center", "Command Center", text]);
    }
}

fn is_tty() -> bool {
    std::io::stdin().is_terminal()
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    s.trim().to_owned()
}

/// Copies a file as a new one and renames it into place, mode 755, so a running process keeps its own copy.
fn put(src: &Path, dst: &Path) -> std::io::Result<()> {
    let new = dst.with_extension("new");
    std::fs::copy(src, &new)?;
    std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(new, dst)
}


impl Env {
    /// Writes a file in ~/.config/control-center, making the folder (0700) if it isn't there.
    fn set(&self, name: &str, content: &str) {
        if !self.dir.exists() {
            let _ = std::fs::create_dir_all(&self.dir);
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        if let Err(x) = std::fs::write(self.dir.join(name), content) {
            eprintln!("{}: {x}", self.dir.join(name).display());
        }
    }

    fn frames(&self) -> Vec<String> {
        let mut f: Vec<String> = std::fs::read_dir(self.dir.join("frames")).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_owned)).collect();
        f.sort();
        f
    }

    /// The shared monitors as install recorded them. For an older install, it's the share units enabled at login.
    fn shared(&self) -> Vec<String> {
        if let Some(s) = read(&self.dir.join("shared")).filter(|s| !s.is_empty()) {
            return s.lines().map(str::to_owned).collect();
        }
        let mut v: Vec<String> = std::fs::read_dir(self.units.join("graphical-session.target.wants")).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_prefix("control-center-share@")?.strip_suffix(".service").filter(|m| m.bytes().all(|b| b.is_ascii_digit())).map(str::to_owned)).collect();
        v.sort();
        v
    }

    /// Every unit a running host has: the shares, the guard, the agent, and announcing if it's on.
    fn host_units(&self) -> Vec<String> {
        let mut u: Vec<String> = self.shared().iter().map(|m| format!("control-center-share@{m}.service")).collect();
        u.push("control-center-guard.service".into());
        u.push("control-center-agent.service".into());
        let a = read(&self.dir.join("announce")).unwrap_or_else(|| if sysq(&["is-enabled", "-q", "control-center-announce.service"]) { "on".into() } else { String::new() });
        if a == "on" {
            u.push("control-center-announce.service".into());
        }
        u
    }

    fn autostart(&self) -> bool {
        read(&self.dir.join("autostart")).unwrap_or_else(|| "on".into()) == "on"
    }

    fn units_cmd(&self, verb: &[&str], quiet: bool) -> bool {
        let units = self.host_units();
        let mut a: Vec<&str> = verb.to_vec();
        a.extend(units.iter().map(String::as_str));
        if quiet { sysq(&a) } else { sys(&a) }
    }

    /// Writes the app menu entry, which you can pin to the taskbar. It starts the host and has Stop in its menu.
    fn launcher(&self) {
        if packaged() {
            return; // the package installs its own
        }
        let d = self.home.join(".local/share/applications");
        let _ = std::fs::create_dir_all(&d);
        let h = self.home.display();
        let _ = std::fs::write(d.join("command-center-host.desktop"), format!("[Desktop Entry]\nType=Application\nName=Command Center Host\nComment=Share this machine's monitors with the Steam Frame\nExec={h}/.local/bin/cc-share up\nIcon=video-display\nCategories=Network;RemoteAccess;\nActions=stop;\n\n[Desktop Action stop]\nName=Stop Command Center\nExec={h}/.local/bin/cc-share down\n"));
    }

    /// The window server's own executable, not /usr/bin/krdpserver, because its grant is its own (W1).
    fn window_server(&self) -> Option<PathBuf> {
        [self.share.join("krdp-window/bin/krdpserver"), self.home.join("src/krdp-window/build/bin/krdpserver")].into_iter()
            .find(|b| std::fs::metadata(b).is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
    }

    fn cc_host(&self) -> PathBuf {
        self.share.join("cc-host")
    }
}

/// The install line for a missing command on this distro. It's printed, never run.
fn need(pkg: &str) -> String {
    for (pm, line) in [("pacman", "sudo pacman -S --needed"), ("dnf", "sudo dnf install"), ("apt", "sudo apt install"), ("zypper", "sudo zypper install")] {
        if have(pm) {
            return format!("{line} {pkg}");
        }
    }
    format!("install {pkg}")
}

// ---------------------------------------------------------------- the firewall

enum Fw {
    Ok,
    /// The ports are closed and a pairing can't work. It says so and exits 1.
    Closed,
}

/// firewalld zones Command Center never opens its ports in, even for the private ranges: a
/// network in one of these isn't one the user trusts.
const UNTRUSTED: [&str; 5] = ["public", "external", "dmz", "block", "drop"];

/// The commands that open (add) or close (remove) the pairing and paired-Frame ports (3399-3449) for
/// the private ranges, for whichever firewall is active. There are none if no firewall is active.
/// The note is set when the ports can't be opened here: firewalld has the network in an untrusted zone.
fn fw_cmds(action: &str) -> (Vec<Vec<String>>, Option<Vec<String>>) {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if have("firewall-cmd") && run("systemctl", &["is-active", "-q", "firewalld"]).0 {
        let out = Command::new("firewall-cmd").arg("--get-active-zones").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let ours = |z: &str| {
            Command::new("firewall-cmd").args(["--permanent", &format!("--zone={z}"), "--list-rich-rules"]).output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("3399-3449"))
        };
        return firewalld_cmds(action, &active_zones(&out), ours);
    }
    let mut cmds = vec![];
    if have("ufw") && run("systemctl", &["is-active", "-q", "ufw"]).0 {
        for n in NETS {
            if action == "add" {
                cmds.push(s(&["sudo", "ufw", "allow", "proto", "tcp", "from", n, "to", "any", "port", "3399:3449", "comment", "Command Center"]));
            } else {
                cmds.push(s(&["sudo", "ufw", "delete", "allow", "proto", "tcp", "from", n, "to", "any", "port", "3399:3449"]));
            }
        }
    }
    (cmds, None)
}

/// firewalld's half of fw_cmds, given the active zones and their interfaces, and whether a zone
/// already has our rules. Adding goes into each active zone that isn't UNTRUSTED (on a Steam Deck
/// the Wi-Fi is in "home" while "public" is the default, so rules without --zone never applied),
/// and takes ours out of the untrusted ones, where an older cc-host put them. Removing takes them
/// out of every zone that has them.
fn firewalld_cmds(action: &str, zones: &[(String, Vec<String>)], ours: impl Fn(&str) -> bool) -> (Vec<Vec<String>>, Option<Vec<String>>) {
    let rules = |z: &str, act: &str| -> Vec<Vec<String>> {
        NETS.iter().map(|n| vec!["sudo".into(), "firewall-cmd".into(), "--permanent".into(), format!("--zone={z}"),
                                  format!("--{act}-rich-rule=rule family=ipv4 source address={n} port port=3399-3449 protocol=tcp accept")]).collect()
    };
    let mut cmds = vec![];
    let mut closed = vec![];
    for (z, ifaces) in zones {
        let bad = UNTRUSTED.contains(&z.as_str());
        if action == "add" && !bad {
            cmds.extend(rules(z, "add"));
        } else if ours(z) {
            cmds.extend(rules(z, "remove"));
        }
        if bad {
            closed.extend(ifaces.iter().cloned());
        }
    }
    let note = (action == "add" && zones.iter().all(|(z, _)| UNTRUSTED.contains(&z.as_str()))).then(|| {
        let ifaces = if closed.is_empty() { "<interface>".to_owned() } else { closed.join(" ") };
        let mut l = vec![format!("firewalld has this network ({ifaces}) in an untrusted zone ({}), and Command Center doesn't open ports there.",
                                 zones.iter().map(|z| z.0.as_str()).collect::<Vec<_>>().join(", ")),
                         "If it's your home network, move it to the home zone, then run this again:".to_owned()];
        l.extend(closed.iter().map(|i| format!("  sudo firewall-cmd --permanent --zone=home --change-interface={i}")));
        l.push("  sudo firewall-cmd --reload".into());
        l
    });
    if !cmds.is_empty() {
        cmds.push(vec!["sudo".into(), "firewall-cmd".into(), "--reload".into()]);
    }
    (cmds, note)
}

/// The zones in `firewall-cmd --get-active-zones` output and their interfaces: a zone is an
/// unindented line (without its " (default)" note), its "  interfaces:" line follows. With none
/// listed, the default zone.
fn active_zones(out: &str) -> Vec<(String, Vec<String>)> {
    let mut z: Vec<(String, Vec<String>)> = vec![];
    for l in out.lines().filter(|l| !l.trim().is_empty()) {
        if !l.starts_with(char::is_whitespace) {
            z.push((l.split_whitespace().next().unwrap_or_default().to_owned(), vec![]));
        } else if let (Some(last), Some(i)) = (z.last_mut(), l.trim().strip_prefix("interfaces:")) {
            last.1.extend(i.split_whitespace().map(str::to_owned));
        }
    }
    if z.is_empty() { vec![("public".into(), vec![])] } else { z }
}

/// Shows a command as the shell line it is, quoting any word that has spaces.
fn shown(c: &[String]) -> String {
    c.iter().map(|w| if w.contains(' ') { if let Some((a, b)) = w.split_once('=') && a.starts_with("--") { format!("{a}='{b}'") } else { format!("'{w}'") } } else { w.clone() }).collect::<Vec<_>>().join(" ")
}

/// What firewall() says without --firewall, as (lines, whether the ports are closed for a pairing).
fn firewall_lines(e: &Env, action: &str, _flag: &str) -> Vec<String> {
    firewall_said(e, action).0
}

fn firewall_said(e: &Env, action: &str) -> (Vec<String>, bool) {
    let (cmds, note) = fw_cmds(action);
    if let Some(mut l) = note {
        if !cmds.is_empty() {
            l.push("and take Command Center's rules out of the untrusted zone:".into());
            l.extend(cmds.iter().map(|c| format!("  {}", shown(c))));
        }
        return (l, true);
    }
    if cmds.is_empty() {
        return (vec![], false);
    }
    let open = e.dir.join("firewall-open");
    if action == "add" && !open.exists() {
        // Don't put up a key that no Frame can reach. Live, the Frame timed out while the host kept waiting.
        let mut l = vec!["a firewall is active here, and the Frame can't reach this machine until these ports are open:".to_owned()];
        l.extend(cmds.iter().map(|c| format!("  {}", shown(c))));
        l.push(format!("rerun as cc-share pair --firewall (sudo asks), or run them yourself and touch {}", open.display()));
        (l, true)
    } else {
        let mut l = vec![format!("firewall: to {action} the pairing and paired-Frame ports, run (or pass --firewall):")];
        l.extend(cmds.iter().map(|c| format!("  {}", shown(c))));
        (l, false)
    }
}

/// Prints the ports' commands, or runs them with --firewall (sudo asks).
fn firewall(e: &Env, action: &str, flag: &str) -> Fw {
    if flag == "--firewall" {
        let (cmds, note) = fw_cmds(action);
        for c in &cmds {
            println!("+ {}", shown(c));
            let _ = Command::new(&c[0]).args(&c[1..]).status();
        }
        if let Some(l) = note {
            l.iter().for_each(|l| eprintln!("{l}"));
            return Fw::Closed;
        }
        if cmds.is_empty() {
            return Fw::Ok;
        }
        let open = e.dir.join("firewall-open");
        if action == "add" {
            let _ = std::fs::write(&open, b"");
        } else {
            let _ = std::fs::remove_file(&open);
        }
        return Fw::Ok;
    }
    let (lines, closed) = firewall_said(e, action);
    for l in &lines {
        if closed { eprintln!("{l}") } else { println!("{l}") }
    }
    if closed { Fw::Closed } else { Fw::Ok }
}

/// install --dry-run's look at the firewall. It prints what firewall add would say, each line as "  firewall: ...".
fn firewall_preview(e: &Env, flag: &str) {
    for line in firewall_lines(e, "add", flag) {
        println!("  firewall: {line}");
    }
}

// ---------------------------------------------------------------- check

const AVAHI_ON: &str = "sudo systemctl enable --now avahi-daemon";

/// Whether avahi-daemon.conf's [publish] section has disable-publishing or
/// disable-user-service-publishing set to yes, either of which stops avahi-publish ("Not permitted").
fn avahi_publishing_off(conf: &str) -> bool {
    let mut publish = false;
    conf.lines().map(str::trim).filter(|l| !l.starts_with('#') && !l.starts_with(';')).any(|l| {
        if l.starts_with('[') {
            publish = l == "[publish]";
            return false;
        }
        publish && matches!(l.split_once('=').map(|(k, v)| (k.trim(), v.trim())), Some(("disable-publishing" | "disable-user-service-publishing", "yes")))
    })
}

fn avahi_running() -> bool {
    run("systemctl", &["is-active", "-q", "avahi-daemon"]).0
}

fn check(e: &Env) -> bool {
    let mut bad = false;
    let mut tools = vec![("kscreen-doctor", "libkscreen"), ("avahi-publish", "avahi")];
    if packaged() {
        match package() {
            Some((v, _)) => println!("  ok   package command-center-host {v}"),
            None => println!("  note cc-host runs from /usr but no package manager owns command-center-host"),
        }
        match libexec() {
            Some(d) => println!("  ok   krdp (the package's, patched): {}", d.join("krdpserver").display()),
            None => {
                println!("  need the package's krdp in /usr/lib/command-center: reinstall command-center-host");
                bad = true;
            }
        }
    } else {
        tools.insert(0, ("krdpserver", "krdp"));
    }
    for (c, pkg) in tools {
        if have(c) {
            println!("  ok   {c}");
        } else {
            println!("  need {c}: {}", need(pkg));
            bad = true;
        }
    }
    // avahi-publish needs the daemon: without it, it dies at once and the Frame never lists this
    // machine (seen on a Steam Deck, where avahi-daemon is off).
    let on = std::fs::read_to_string(e.dir.join("announce")).is_ok_and(|a| a.trim() == "on");
    if have("avahi-publish") && !avahi_running() {
        println!("  {} avahi-daemon isn't running, so the Frame can't find this machine: {AVAHI_ON}", if on { "need" } else { "note" });
        bad |= on;
    } else if avahi_publishing_off(&std::fs::read_to_string("/etc/avahi/avahi-daemon.conf").unwrap_or_default()) {
        // SteamOS ships it this way. Changing the system's avahi config is the user's call, so it's a note.
        println!("  note avahi's config turns publishing off (/etc/avahi/avahi-daemon.conf), so the Frame won't list this machine: type its address in Add machine");
    }
    println!("  ok   cc-host (cc-host {}): the agent, pairing and tag screens", env!("CARGO_PKG_VERSION"));
    for u in ["control-center-agent", "control-center-guard"] {
        if sysq(&["is-active", "-q", u]) {
            println!("  ok   {u} running");
        } else {
            println!("  need {u}: systemctl --user status {u}");
            bad = true;
        }
    }
    if crate::hostcert::check(&e.dir, cc_proto::agent::PORT).is_ok() {
        println!("  ok   agent answering on 3399 (TLS)");
    } else {
        println!("  need the agent on 3399: journalctl --user -u control-center-agent");
        bad = true;
    }
    if std::fs::metadata(e.dir.join("cert.pem")).is_ok_and(|m| m.len() > 0) {
        println!("  ok   krdp certificate");
    } else {
        println!("  need krdp's certificate: cc-share install");
        bad = true;
    }
    if std::fs::metadata(e.dir.join("host-id")).is_ok_and(|m| m.len() > 0) {
        println!("  ok   host id");
    } else {
        println!("  note host id: made at the first pairing");
    }
    if sysq(&["is-active", "-q", "control-center-announce.service"]) {
        println!("  ok   announcing (the Frame finds this machine)");
    } else {
        println!("  note not announcing: the Frame won't list this machine (cc-share announce on), its address can still be typed");
    }
    if e.autostart() {
        println!("  ok   starts when you log in (cc-share autostart off: from the app menu / taskbar instead)");
    } else {
        println!("  ok   starts from the app menu / taskbar: Command Center Host (cc-share autostart on: at login)");
    }
    if run("loginctl", &["show-user", &user(), "-p", "Linger", "--value"]).1.trim() == "yes" {
        println!("  ok   linger");
    } else {
        println!("  note linger off: the units run while you're logged in (loginctl enable-linger to run them without)");
    }
    let shared = e.shared();
    if !shared.is_empty() {
        println!("  note shared login (slot 0) still active on {}: anyone with its password can connect", shared.iter().map(|m| format!("monitor {m}")).collect::<Vec<_>>().join(","));
    }
    !bad
}

// ---------------------------------------------------------------- install

/// The user units, for cc-host at `host` and the monitor servers' krdp at `krdp`. A user install
/// writes them to ~/.config/systemd/user; a package build writes them with `cc-host units`.
pub fn unit_files(host: &str, krdp: &str) -> Vec<(&'static str, String)> {
    let agent = format!("{host} serve");
    vec![
        // --plasma talks to KWin directly, so nothing asks for permission on every connect.
        ("control-center-share@.service", "[Unit]\nDescription=Command Center share: monitor %i over RDP (port 3400+%i)\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart=/bin/sh -c 'exec KRDP --plasma --monitor %i --port $((3400 + %i)) -u \"$USER\" -p \"$(cat %h/.config/control-center/password)\" --certificate %h/.config/control-center/cert.pem --certificate-key %h/.config/control-center/key.pem'\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=graphical-session.target\n".replace("KRDP", krdp)),
        // A paired Frame's own server for one monitor, instance <frame>-<monitor>.
        ("control-center-frame@.service", format!("[Unit]\nDescription=Command Center share for paired Frame %i\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart={host} frame-run %i\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=graphical-session.target\n")),
        // A paired Frame's popped-out window, instance <frame>-<k>-<uuid>.
        ("control-center-window@.service", format!("[Unit]\nDescription=Command Center window stream for paired Frame %i\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart={host} window-run %i\nRestart=no\n")),
        // The agent (docs/agent.md). It runs as this user, never root, and it's sandboxed as far as krdp control still works.
        ("control-center-agent.service", format!("[Unit]\nDescription=Command Center agent: paired Frames on port 3399 (docs/agent.md)\nAfter=graphical-session.target network-online.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart={agent}\nExecReload=/bin/kill -HUP $MAINPID\nRestart=on-failure\nRestartSec=5\nNoNewPrivileges=yes\nPrivateTmp=yes\nProtectSystem=strict\nReadWritePaths=%h/.config/control-center %h/.cache %h/.config/systemd/user %t\n\n[Install]\nWantedBy=graphical-session.target\n")),
        ("control-center-guard.service", format!("[Unit]\nDescription=Command Center: keep shared monitors encodable while a viewer is connected\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart={host} guard\nTimeoutStopSec=20\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=graphical-session.target\n")),
    ]
}

/// Copies cc-host itself to ~/.local/share/control-center and makes ~/.local/bin/cc-share a link to it,
/// since the units of older installs call cc-share.
fn copy_self(e: &Env) -> Result<(), String> {
    for d in [&e.units, &e.home.join(".local/bin"), &e.share] {
        std::fs::create_dir_all(d).map_err(|x| x.to_string())?;
    }
    let me = std::env::current_exe().map_err(|x| x.to_string())?;
    if std::fs::canonicalize(&me).ok() != std::fs::canonicalize(e.cc_host()).ok() {
        put(&me, &e.cc_host()).map_err(|x| format!("{}: {x}", e.cc_host().display()))?;
    }
    let link = e.home.join(".local/bin/cc-share");
    let new = link.with_extension("new");
    let _ = std::fs::remove_file(&new);
    std::os::unix::fs::symlink(e.cc_host(), &new).and_then(|_| std::fs::rename(&new, &link)).map_err(|x| format!("{}: {x}", link.display()))?;
    // Remove an older install's Python agent, pairing and tag screens. They're gone because cc-host is all of them.
    for f in ["pair.py", "agent.py", "tagshow.py"] {
        let _ = std::fs::remove_file(e.share.join(f));
    }
    let _ = std::fs::remove_dir_all(e.share.join("third_party"));
    Ok(())
}

/// Before a packaged install takes over: removes what an older user install put in the home
/// directory that would shadow or duplicate the package's files. Pairings, keys and settings in
/// ~/.config/control-center stay, so no Frame has to pair again (R10).
fn migrate_user_install(e: &Env) {
    let old = e.cc_host();
    let mut said = false;
    let mut gone = |p: &Path| {
        if std::fs::remove_file(p).is_ok() && !said {
            println!("moving from the old install in your home folder to the package (pairings and settings are kept)");
            said = true;
        }
    };
    gone(&old);
    let link = e.home.join(".local/bin/cc-share");
    if std::fs::read_link(&link).is_ok_and(|t| t == old) {
        gone(&link);
    }
    // unit files in ~/.config/systemd/user would shadow the package's in /usr/lib/systemd/user
    for f in std::fs::read_dir(&e.units).into_iter().flatten().flatten() {
        let n = f.file_name().to_string_lossy().into_owned();
        if n.starts_with("control-center-") && n.ends_with(".service") && f.file_type().is_ok_and(|t| t.is_file()) {
            gone(&f.path());
        }
    }
    for d in ["command-center-host.desktop", "command-center-krdp-window.desktop"] {
        gone(&e.home.join(".local/share/applications").join(d));
    }
    let _ = std::fs::remove_dir_all(e.share.join("krdp-window"));
}

fn install(e: &Env, args: &[String]) -> i32 {
    let (mut flag, mut announce, mut dry, mut autostart) = ("", None, false, None);
    let mut mons: Vec<String> = vec![];
    for a in args {
        match a.as_str() {
            "--firewall" => flag = "--firewall",
            "--announce" => announce = Some("on"),
            "--no-announce" => announce = Some("off"),
            "--dry-run" => dry = true,
            "--autostart" => autostart = Some("on".to_owned()),
            "--no-autostart" => autostart = Some("off".to_owned()),
            "--rust" => {} // It's the only agent now, but it's still accepted for the older docs.
            "--python" => {
                eprintln!("--python: the Python agent is gone (cc-host is the agent, pairing and tag screens)");
                return 2;
            }
            m => mons.push(m.to_owned()),
        }
    }
    if dry {
        println!("cc-share install {} would:", mons.join(" "));
        if mons.is_empty() {
            println!("  ask which monitors to share (Enter: all of them)");
        }
        if packaged() {
            println!("  use the package's cc-host, units and krdp (and remove an older install's copies from your home folder)");
        } else {
            println!("  copy cc-host to {}, and link ~/.local/bin/cc-share to it", e.share.display());
        }
        if !std::fs::metadata(e.dir.join("password")).is_ok_and(|m| m.len() > 0) {
            println!("  make the shared login's password in {}/password", e.dir.display());
        }
        if !std::fs::metadata(e.dir.join("cert.pem")).is_ok_and(|m| m.len() > 0) {
            println!("  make krdp's certificate in {}/cert.pem", e.dir.display());
        }
        let enable = mons.iter().map(|m| format!("share@{m} ")).collect::<String>();
        if packaged() {
            println!("  enable the package's units: {enable}agent guard");
        } else {
            println!("  write the units share@, frame@, agent, guard in {}, and enable: {enable}agent guard", e.units.display());
        }
        println!("  restart the agent; restart the guard only if no viewer is connected (now: {} connected)", viewers());
        if wants_frames(e) {
            println!("  stop starting paired Frames' servers at login (the agent starts them on connect)");
        }
        firewall_preview(e, flag);
        println!("  announce: {}", announce.unwrap_or("asked (or left as it is without a terminal)"));
        let at = read(&e.dir.join("autostart")).unwrap_or_else(|| "asked (on without a terminal)".into());
        println!("  start at login: {}; the app menu gets Command Center Host either way", autostart.as_deref().unwrap_or(&at));
        println!("and then check:");
        check(e);
        return 0;
    }
    if packaged() && libexec().is_none() {
        eprintln!("the package's krdp is missing (/usr/lib/command-center/krdpserver): reinstall command-center-host");
        return 1;
    }
    for (c, pkg) in [("krdpserver", "krdp"), ("kscreen-doctor", "libkscreen")] {
        if !have(c) && !(c == "krdpserver" && packaged()) {
            eprintln!("{c} isn't installed. Install it with: {}", need(pkg));
            return 1;
        }
    }
    if mons.is_empty() {
        match ask_monitors() {
            Some(m) => mons = m,
            None => return 2,
        }
    }
    let _ = std::fs::create_dir_all(&e.dir);
    let _ = std::fs::set_permissions(&e.dir, std::fs::Permissions::from_mode(0o700));
    let pw = e.dir.join("password");
    if let Ok(p) = std::env::var("CC_PASSWORD").map(|p| p.trim_end_matches('\n').to_owned()).and_then(|p| if p.is_empty() { Err(std::env::VarError::NotPresent) } else { Ok(p) }) {
        let _ = crate::agent::write_private(&pw, format!("{p}\n").as_bytes());
    } else if !std::fs::metadata(&pw).is_ok_and(|m| m.len() > 0) {
        use base64::Engine;
        let p: String = base64::engine::general_purpose::STANDARD.encode(crate::agent::urandom::<24>()).chars().filter(|c| !"/+=".contains(*c)).take(20).collect();
        let _ = crate::agent::write_private(&pw, format!("{p}\n").as_bytes());
        println!("generated a password in {}: store it in your password manager, and on the Frame", pw.display());
    }
    // A fixed certificate, so the Frame can pin it. It's kept once made.
    if let Err(x) = crate::hostcert::cert(&e.dir, &hostname()) {
        eprintln!("cc-host cert: {x}");
        return 1;
    }
    if packaged() {
        migrate_user_install(e);
        sysq(&["daemon-reload"]);
        e.units_cmd(&["disable"], true); // links to the old unit files go; enable below makes the package's
    } else {
        if let Err(x) = copy_self(e) {
            eprintln!("{x}");
            return 1;
        }
        for (name, body) in unit_files("%h/.local/share/control-center/cc-host", "/usr/bin/krdpserver") {
            let _ = std::fs::write(e.units.join(name), body);
        }
        sysq(&["daemon-reload"]);
    }
    // Paired Frames' servers start when they connect (the agent does it), not at login. Older installs enabled them.
    for u in frame_wants(e) {
        sysq(&["disable", &u]); // A running one keeps running.
    }
    let _ = std::fs::write(e.dir.join("shared"), mons.iter().map(|m| format!("{m}\n")).collect::<String>());
    // I wanted the choice between starting at login or from the app menu / taskbar. It's asked, or set with --autostart / --no-autostart.
    if autostart.is_none() && is_tty() && !e.dir.join("autostart").exists() {
        let a = ask("start Command Center when you log in, or from the app menu / taskbar? [L/t] ");
        autostart = Some(if a.starts_with(['t', 'T']) { "off" } else { "on" }.into());
    }
    if let Some(a) = &autostart {
        let _ = std::fs::write(e.dir.join("autostart"), format!("{a}\n"));
    }
    e.launcher();
    e.units_cmd(&["start"], false); // It's running now either way.
    if e.autostart() {
        e.units_cmd(&["enable"], true);
    } else {
        e.units_cmd(&["disable"], true);
    }
    sys(&["restart", "control-center-agent.service"]); // Start the new agent.
    for _ in 0..10 {
        if listening(3399) {
            break; // It's listening before the checklist asks it.
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    // The guard runs the new code too. With viewers connected, it only restarts when it hasn't lowered anything.
    let lowered = std::fs::read_dir(&e.dir).into_iter().flatten().flatten().any(|f| f.file_name().to_string_lossy().starts_with("guard-"));
    if viewers() == 0 || !lowered {
        sys(&["restart", "control-center-guard.service"]);
    } else {
        println!("the guard keeps running as it was: it has lowered a monitor for a connected viewer (restart it later: systemctl --user restart control-center-guard)");
    }
    println!("sharing as user {} on {}: ports {}", user(), hostname(), mons.iter().filter_map(|m| m.parse::<u32>().ok()).map(|m| format!("{} ", 3400 + m)).collect::<String>());
    let _ = firewall(e, "add", flag); // Printed, or run with --firewall. Install keeps going either way.
    // I wanted announcing to be opt-in per machine. It's asked here, or set with --announce / --no-announce.
    let mut announce = announce.map(str::to_owned);
    if announce.is_none() && is_tty() && !sysq(&["is-active", "-q", "control-center-announce.service"]) {
        let a = ask("announce this machine on the network so the Frame can find it? [Y/n] ");
        announce = Some(if a.starts_with(['n', 'N']) { "off" } else { "on" }.into());
    }
    if let Some(a) = announce {
        announce_cmd(e, &a);
    }
    println!("checklist:");
    if !check(e) {
        println!("some items need you (above)");
    }
    if std::env::var_os("CC_INSTALLER").is_some() {
        return 0; // cc-install says what's next in its own words.
    }
    println!("\nNext, pair this machine with the Frame:");
    println!("  1. Here, run: cc-share pair");
    println!("     A 6-digit key fills the screen for 5 minutes.");
    println!("  2. On the Frame, open Workspace, then Machines, then Add machine. Pick {}, press Pair,", hostname());
    println!("     and type the key, or press Pair by looking and look at this screen.");
    if !sysq(&["is-active", "-q", "control-center-announce.service"]) {
        println!("  The Frame only lists this machine while it announces itself: cc-share announce on");
    }
    0
}

/// Asks on the terminal which monitors to share, listing them, with Enter for all. Returns None, after
/// saying why, if there's no terminal or no monitor.
fn ask_monitors() -> Option<Vec<String>> {
    let outs = outputs();
    if outs.is_empty() {
        eprintln!("kscreen-doctor lists no monitors: cc-host install needs a KDE Plasma session on Wayland");
        return None;
    }
    let lines: Vec<String> = outs.iter().enumerate().map(|(i, o)| format!("  {i}  {}  {}x{}", o["name"].as_str().unwrap_or(""), o["size"]["width"], o["size"]["height"])).collect();
    if !is_tty() {
        eprintln!("usage: cc-share install <monitor> ...  (the monitors here:)\n{}", lines.join("\n"));
        return None;
    }
    println!("Monitors on {}:\n{}", hostname(), lines.join("\n"));
    loop {
        match pick(&ask("Which should the Frame see? Numbers with spaces between, or Enter for all: "), outs.len()) {
            Ok(m) => return Some(m),
            Err(x) => println!("{x}"),
        }
    }
}

/// Parses an answer to ask_monitors: numbers split by spaces or commas, each below n. Empty or "all" means all of them.
fn pick(answer: &str, n: usize) -> Result<Vec<String>, String> {
    let a = answer.trim();
    if a.is_empty() || a.eq_ignore_ascii_case("all") {
        return Ok((0..n).map(|i| i.to_string()).collect());
    }
    let mut v: Vec<usize> = vec![];
    for w in a.split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()) {
        match w.parse::<usize>() {
            Ok(i) if i < n => {
                if !v.contains(&i) {
                    v.push(i);
                }
            }
            _ => return Err(format!("{w} isn't one of them: type numbers from 0 to {}", n - 1)),
        }
    }
    Ok(v.iter().map(usize::to_string).collect())
}

fn frame_wants(e: &Env) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(e.units.join("graphical-session.target.wants")).into_iter().flatten().flatten()
        .map(|x| x.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("control-center-frame@")).collect();
    v.sort();
    v
}

fn wants_frames(e: &Env) -> bool {
    !frame_wants(e).is_empty()
}

// ---------------------------------------------------------------- the guard

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_: libc::c_int) {
    STOP.store(true, Relaxed);
}

fn stop_on_signals() {
    for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(s, on_stop as *const () as libc::sighandler_t) };
    }
}

/// The mode to show an output in while a viewer is connected: its biggest same-shape mode under max
/// pixels, because krdp streams black above ~4.28 MP. If the current one is already under, it stays.
pub fn mode_for(o: &Value, max: u64) -> Option<(String, String)> {
    let cur = o["currentModeId"].as_str()?.to_owned();
    let modes = o["modes"].as_array()?;
    let size = |m: &Value| (m["size"]["width"].as_f64().unwrap_or(0.0), m["size"]["height"].as_f64().unwrap_or(0.0));
    let (w, h) = size(modes.iter().find(|m| m["id"].as_str() == Some(&cur))?);
    if w * h <= max as f64 {
        return Some((cur.clone(), cur));
    }
    let ar = w / h;
    let key = |m: &Value| {
        let (x, y) = size(m);
        ((x / y - ar).abs(), -(x * y), -m["refreshRate"].as_f64().unwrap_or(0.0))
    };
    let best = modes.iter().filter(|m| { let (a, b) = size(m); a * b <= max as f64 }).min_by(|a, b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal));
    Some((cur.clone(), best.and_then(|m| m["id"].as_str()).map_or(cur, str::to_owned)))
}

fn guard(e: &Env) -> i32 {
    // try-restart only touches running ones. A plain restart on the pattern would start stopped sessions.
    let restart_shares = || {
        sysq(&["try-restart", "control-center-share@*", "control-center-frame@*"]);
    };
    let restore = || -> bool {
        let mut changed = false;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&e.dir).into_iter().flatten().flatten().map(|f| f.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("guard-"))).collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().trim_start_matches("guard-").to_owned();
            let mode = read(&f).unwrap_or_default();
            let _ = run("kscreen-doctor", &[&format!("output.{name}.mode.{mode}")]);
            println!("restored {name}");
            let _ = std::fs::remove_file(&f);
            changed = true;
        }
        changed
    };
    let lower = || -> bool {
        let mut changed = false;
        for m in e.shared() {
            let Some(name) = m.parse().ok().and_then(output_name) else { continue };
            if e.dir.join(format!("guard-{name}")).exists() {
                continue;
            }
            let v: Value = serde_json::from_str(&run("kscreen-doctor", &["-j"]).1).unwrap_or(json!({}));
            let Some(o) = v["outputs"].as_array().and_then(|a| a.iter().find(|o| o["name"].as_str() == Some(&name))) else { continue };
            let Some((cur, mode)) = mode_for(o, e.max) else { continue };
            if cur == mode {
                continue;
            }
            let _ = std::fs::write(e.dir.join(format!("guard-{name}")), format!("{cur}\n"));
            if run("kscreen-doctor", &[&format!("output.{name}.mode.{mode}")]).0 {
                println!("viewer connected: {name} mode {cur} -> {mode}");
            }
            changed = true;
        }
        changed
    };
    if restore() {
        restart_shares(); // Left over from a guard that died mid-session.
    }
    stop_on_signals();
    let (mut active, mut idle) = (false, 0);
    loop {
        for _ in 0..20 {
            if STOP.load(Relaxed) {
                if restore() {
                    restart_shares();
                }
                return 0;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if viewers() > 0 {
            idle = 0;
            if !active {
                active = true;
                if lower() {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    restart_shares();
                }
            }
        } else if !active && replaced() {
            println!("updated: restarting into the new build");
            return 75; // Restart=on-failure brings up the new one.
        } else if active {
            idle += 2;
            if idle >= 15 {
                active = false;
                if restore() {
                    restart_shares();
                }
                println!("no viewers: restored");
            }
        }
        let _ = std::io::stdout().flush();
    }
}

// ---------------------------------------------------------------- announcing

/// The announce unit, for cc-host at `host`. `cc-share announce on` writes it in a user install.
pub fn announce_unit(host: &str) -> String {
    format!("[Unit]\nDescription=Command Center: announce this machine on the network (mDNS _controlcenter._tcp)\nAfter=graphical-session.target network-online.target\nPartOf=graphical-session.target\n\n[Service]\nExecStart={host} announce-run\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy=graphical-session.target\n")
}

fn announce_cmd(e: &Env, what: &str) -> i32 {
    let unit = e.units.join("control-center-announce.service");
    match what {
        "on" => {
            if !have("avahi-publish") {
                eprintln!("avahi-publish not found (install avahi)");
                return 1;
            }
            if !avahi_running() {
                eprintln!("avahi-daemon isn't running, so the Frame can't find this machine. Start it, then run this again: {AVAHI_ON}");
                return 1;
            }
            if !packaged() {
                if let Err(x) = copy_self(e) {
                    eprintln!("{x}");
                    return 1;
                }
                let _ = std::fs::create_dir_all(&e.units);
                let _ = std::fs::write(&unit, announce_unit("%h/.local/share/control-center/cc-host"));
            }
            sysq(&["daemon-reload"]);
            e.set("announce", "on\n");
            sys(&["start", "control-center-announce.service"]);
            if e.autostart() {
                sysq(&["enable", "control-center-announce.service"]);
            }
            println!("announcing {} as _controlcenter._tcp (cc-share announce off stops it)", hostname());
        }
        "off" => {
            e.set("announce", "off\n");
            sysq(&["disable", "--now", "control-center-announce.service"]);
            let _ = std::fs::remove_file(&unit);
            sysq(&["daemon-reload"]);
            println!("not announcing");
        }
        "status" => println!("{}", if sysq(&["is-active", "-q", "control-center-announce.service"]) { "on" } else { "off" }),
        _ => {
            eprintln!("usage: cc-share announce on|off|status");
            return 2;
        }
    }
    0
}

/// What the Frame's discover lists: the host, and each shared monitor's output and native size. It adds
/// pair=1 while a pairing screen is up. There's no login name because SSH is dev-only.
pub fn txt(e: &Env) -> Vec<String> {
    let outs = outputs();
    let mut list = vec![];
    for m in e.shared() {
        let Some(o) = m.parse::<usize>().ok().and_then(|i| outs.get(i)) else { continue };
        let name = o["name"].as_str().unwrap_or("");
        list.push(format!("m{m}={name},{}x{}", o["size"]["width"], o["size"]["height"]));
    }
    let mut out = vec![format!("host={}", hostname()), format!("monitors={}", list.len())];
    out.extend(list);
    out.push("version=1".into());
    out.push(format!("pair={}", if e.dir.join("pairing").exists() { 1 } else { 0 }));
    out
}

fn announce_run(e: &Env) -> i32 {
    stop_on_signals();
    let mut child: Option<std::process::Child> = None;
    let mut last: Vec<String> = vec![];
    loop {
        let now = txt(e);
        if now != last {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            child = Command::new("avahi-publish").arg("-s").arg(hostname()).arg("_controlcenter._tcp").arg("3399").args(&now).spawn().ok();
            last = now;
        }
        for _ in 0..20 {
            if STOP.load(Relaxed) {
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                return 0;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

// ---------------------------------------------------------------- paired Frames

fn frame_json(e: &Env, f: &str) -> Result<Value, String> {
    let p = e.dir.join("frames").join(format!("{f}.json"));
    serde_json::from_slice(&std::fs::read(&p).map_err(|x| format!("{}: {x}", p.display()))?).map_err(|x| format!("{}: {x}", p.display()))
}

/// Runs one paired Frame's server for one monitor. The instance is <frame>-<monitor>, and the slot and login come from frames/<frame>.json.
fn frame_run(e: &Env, inst: &str) -> i32 {
    let Some((f, m)) = inst.rsplit_once('-') else {
        eprintln!("bad instance: {inst}");
        return 2;
    };
    if !valid_frame(f) || m.len() != 1 || !m.bytes().all(|b| b.is_ascii_digit()) {
        eprintln!("bad instance: {inst}");
        return 2;
    }
    let j = match frame_json(e, f) {
        Ok(j) => j,
        Err(x) => {
            eprintln!("{x}");
            return 1;
        }
    };
    let (slot, mon) = (j["slot"].as_i64().unwrap_or(0), m.parse::<i64>().unwrap_or(0));
    let err = Command::new(krdp(e, false).unwrap_or_else(|| PathBuf::from("/usr/bin/krdpserver"))).args(["--plasma", "--monitor", m, "--port", &(3400 + 10 * slot + mon).to_string(), "-u", j["user"].as_str().unwrap_or(""), "-p", j["password"].as_str().unwrap_or("")])
        .arg("--certificate").arg(e.dir.join("cert.pem")).arg("--certificate-key").arg(e.dir.join("key.pem")).exec();
    eprintln!("krdpserver: {err}");
    1
}

/// Runs one paired Frame's popped-out window (docs/remote-windows.md). The instance is <frame>-<k>-<uuid>.
fn window_run(e: &Env, inst: &str) -> i32 {
    let bad = || {
        eprintln!("bad instance: {inst}");
        2
    };
    if inst.len() < 40 {
        return bad();
    }
    let (rest, uuid) = inst.split_at(inst.len() - 36);
    let Some(rest) = rest.strip_suffix('-') else { return bad() };
    let Some((f, k)) = rest.rsplit_once('-') else { return bad() };
    if !valid_frame(f) || !matches!(k, "0" | "1" | "2" | "3" | "4") || !crate::work::valid_uuid(&format!("{{{uuid}}}")) || uuid.chars().any(|c| c.is_ascii_uppercase()) {
        return bad();
    }
    if !e.dir.join("windows-on").exists() {
        eprintln!("window streams are off on this host (cc-share windows on)");
        return 1;
    }
    let Some(bin) = krdp(e, true) else {
        eprintln!("no window server built (docs/remote-windows.md step 0)");
        return 1;
    };
    let j = match frame_json(e, f) {
        Ok(j) => j,
        Err(x) => {
            eprintln!("{x}");
            return 1;
        }
    };
    let slot = j["slot"].as_i64().unwrap_or(0);
    let err = Command::new(bin).args(["--plasma", "--window", &format!("{{{uuid}}}"), "--port", &(3405 + 10 * slot + k.parse::<i64>().unwrap_or(0)).to_string(), "-u", j["user"].as_str().unwrap_or(""), "-p", j["password"].as_str().unwrap_or("")])
        .arg("--certificate").arg(e.dir.join("cert.pem")).arg("--certificate-key").arg(e.dir.join("key.pem")).exec();
    eprintln!("window server: {err}");
    1
}

fn windows_cmd(e: &Env, what: &str) -> i32 {
    match what {
        "on" => {
            let Some(bin) = krdp(e, true) else {
                eprintln!("no window server built yet (docs/remote-windows.md step 0)");
                return 1;
            };
            // The package ships the window build's grant (.desktop) itself; a user install writes its own.
            if !packaged() {
                let d = e.home.join(".local/share/applications");
                let _ = std::fs::create_dir_all(&d);
                let _ = std::fs::write(d.join("command-center-krdp-window.desktop"), format!("[Desktop Entry]\nType=Application\nName=Command Center window server\nExec={}\nNoDisplay=true\nX-KDE-Wayland-Interfaces=org_kde_kwin_fake_input,zkde_screencast_unstable_v1,org_kde_plasma_window_management\n", bin.display()));
                let _ = run("kbuildsycoca6", &[]);
            }
            e.set("windows-on", "");
            println!("window streams on: a paired Frame may list this machine's windows on shared monitors (app and size;");
            println!("captions only with: touch {}/windows-captions) and pop them out", e.dir.display());
        }
        "off" => {
            let _ = std::fs::remove_file(e.dir.join("windows-on"));
            sysq(&["stop", "control-center-window@*"]);
            println!("window streams off");
        }
        "status" => println!("{}", if e.dir.join("windows-on").exists() { "on" } else { "off" }),
        _ => {
            eprintln!("usage: cc-share windows on|off|status");
            return 2;
        }
    }
    0
}

fn unpair(e: &Env, f: &str, flag: &str) -> i32 {
    if !valid_frame(f) {
        eprintln!("usage: cc-share unpair <frame>  (cc-share frames lists them)");
        return 2;
    }
    if !e.dir.join("frames").join(format!("{f}.json")).exists() {
        eprintln!("no paired Frame named {f}");
        return 1;
    }
    // Its servers, whether enabled or only started, for both monitors and windows.
    for u in frame_wants(e).into_iter().filter(|u| u.starts_with(&format!("control-center-frame@{f}-"))) {
        sysq(&["disable", &u]); // Older installs enabled them.
    }
    sysq(&["stop", &format!("control-center-frame@{f}-*"), &format!("control-center-window@{f}-*")]);
    for p in [format!("frames/{f}.json"), format!("trusted-frames/{f}.pub"), format!("frames/{f}.last")] {
        let _ = std::fs::remove_file(e.dir.join(p));
    }
    sysq(&["kill", "-s", "HUP", "control-center-agent"]); // Its live connections end now.
    println!("unpaired {f}: its login no longer works");
    if e.frames().is_empty() {
        let _ = firewall(e, "remove", flag);
    }
    0
}

/// A file's modification time as local "YYYY-mm-dd HH:MM", like date -r.
fn local_time(p: &Path) -> Option<String> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64; // time_t is 64-bit on the 64-bit hosts this runs on.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t as *const i64 as *const _, &mut tm) };
    Some(format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min))
}

fn frames_cmd(e: &Env) -> i32 {
    for f in e.frames() {
        let last = local_time(&e.dir.join("frames").join(format!("{f}.last"))).unwrap_or_else(|| "never".into());
        let out = run("systemctl", &["--user", "list-units", "--no-legend", "--state=running", &format!("control-center-frame@{f}-*")]).1;
        let running: Vec<String> = out.lines().filter_map(|l| {
            let unit = l.split_whitespace().find(|w| w.starts_with("control-center-frame@"))?;
            let m = unit.strip_prefix(&format!("control-center-frame@{f}-"))?.chars().next()?;
            m.is_ascii_digit().then(|| format!("monitor {m}"))
        }).collect();
        let slot = frame_json(e, &f).ok().map_or("null".into(), |j| j["slot"].to_string());
        println!("{f} slot={slot} last-used={last} sessions={}", if running.is_empty() { "none".into() } else { running.join(",") });
    }
    if !e.shared().is_empty() {
        println!("shared login (slot 0) still active: anyone with its password can connect");
    }
    0
}

/// Finds processes whose command line (argv) matches and returns their pids, never this one's.
fn procs(matches: impl Fn(&[String]) -> bool) -> Vec<i32> {
    let me = std::process::id() as i32;
    std::fs::read_dir("/proc").into_iter().flatten().flatten().filter_map(|d| {
        let pid: i32 = d.file_name().to_str()?.parse().ok()?;
        let argv: Vec<String> = std::fs::read(d.path().join("cmdline")).ok()?.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        (pid != me && matches(&argv)).then_some(pid)
    }).collect()
}

/// The last resort: it closes any tag screen or pairing key still on screen. The Frame counts a closed
/// tag screen as escaped, and a pairing key gets destroyed.
fn stop_cmd() -> i32 {
    let is = |a: &[String], prog: &str, word: &str| a.iter().take(2).any(|x| x.ends_with(prog)) && a.iter().any(|x| x == word);
    let tags = procs(|a| is(a, "cc-host", "tagscreen") || a.iter().any(|x| x.ends_with("tagshow.py")));
    for p in &tags {
        unsafe { libc::kill(*p, libc::SIGTERM) };
    }
    println!("{}", if tags.is_empty() { "no scan viewer running" } else { "closed the scan viewer" });
    let keys = procs(|a| is(a, "cc-host", "pair") || (a.iter().any(|x| x.ends_with("pair.py")) && a.iter().any(|x| x == "host")));
    for p in &keys {
        unsafe { libc::kill(*p, libc::SIGTERM) };
    }
    if !keys.is_empty() {
        println!("cancelled the pairing key");
    }
    0
}

fn uninstall(e: &Env) -> i32 {
    sysq(&["disable", "--now", "control-center-announce.service"]);
    let _ = std::fs::remove_file(e.units.join("control-center-announce.service"));
    for f in e.frames() {
        let _ = unpair_quiet(e, &f);
    }
    sysq(&["disable", "--now", "control-center-agent.service"]);
    sys(&["stop", "control-center-share@*", "control-center-guard.service"]);
    let wants = e.units.join("graphical-session.target.wants");
    for f in std::fs::read_dir(&wants).into_iter().flatten().flatten() {
        let n = f.file_name().to_string_lossy().into_owned();
        if n.starts_with("control-center-share@") || n == "control-center-guard.service" {
            let _ = std::fs::remove_file(f.path());
        }
    }
    for u in ["control-center-share@.service", "control-center-frame@.service", "control-center-guard.service", "control-center-agent.service", "control-center-window@.service"] {
        let _ = std::fs::remove_file(e.units.join(u));
    }
    for p in [e.home.join(".local/bin/cc-share"), e.home.join(".local/share/applications/command-center-krdp-window.desktop"), e.home.join(".local/share/applications/command-center-host.desktop"), e.dir.join("shared"), e.dir.join("autostart")] {
        let _ = std::fs::remove_file(p);
    }
    for f in ["pair.py", "agent.py", "tagshow.py", "cc-host"] {
        let _ = std::fs::remove_file(e.share.join(f));
    }
    let _ = std::fs::remove_dir_all(e.share.join("third_party"));
    sysq(&["daemon-reload"]);
    if packaged() {
        let rm = package().map_or("your package manager", |(_, rm)| rm);
        println!("stopped and unpaired; the program itself is the package's: {rm} removes it");
    }
    0
}

fn unpair_quiet(e: &Env, f: &str) -> i32 {
    // For uninstall: unpair without printing anything.
    let gag = unsafe { libc::dup(1) };
    if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
        unsafe { libc::dup2(std::os::fd::AsRawFd::as_raw_fd(&null), 1) };
    }
    let r = unpair(e, f, "");
    let _ = std::io::stdout().flush();
    unsafe {
        libc::dup2(gag, 1);
        libc::close(gag);
    }
    r
}

/// Checks before the key screen that the host is installed and the ports are reachable (cc-share pair [--firewall]).
pub fn pair_precheck(e: &Env, flag: &str) -> Result<(), i32> {
    if !std::fs::metadata(e.dir.join("cert.pem")).is_ok_and(|m| m.len() > 0) || !(packaged() || e.units.join("control-center-frame@.service").exists()) {
        eprintln!("run cc-share install first");
        return Err(1);
    }
    if let Fw::Closed = firewall(e, "add", flag) {
        return Err(1);
    }
    Ok(())
}

/// Runs cc-share's commands. Returns None if it isn't one of them.
pub fn main(cmd: &str, args: &[String]) -> Option<i32> {
    let e = env();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    Some(match cmd {
        "list" => {
            for (i, o) in outputs().iter().enumerate() {
                println!("monitor {i}: {}", o["name"].as_str().unwrap_or(""));
            }
            0
        }
        "install" => install(&e, args),
        "check" => i32::from(!check(&e)),
        "up" => {
            e.units_cmd(&["start"], false);
            notify(&format!("sharing {}: the Frame can connect", hostname()));
            println!("Command Center is running on {}", hostname());
            0
        }
        "down" => {
            let units = e.host_units();
            let mut a = vec!["stop", "control-center-frame@*", "control-center-window@*"];
            a.extend(units.iter().map(String::as_str));
            sysq(&a);
            notify("stopped: the Frame can't connect until you start Command Center Host");
            println!("Command Center stopped on {}", hostname());
            0
        }
        "autostart" => match if arg(0).is_empty() { "status" } else { arg(0) } {
            "on" => {
                e.set("autostart", "on\n");
                e.units_cmd(&["enable"], true);
                println!("starts when you log in");
                0
            }
            "off" => {
                e.set("autostart", "off\n");
                e.units_cmd(&["disable"], true);
                e.launcher();
                println!("starts from the app menu / taskbar (Command Center Host); running now: until cc-share down or logout");
                0
            }
            "status" => {
                println!("{}", if e.autostart() { "on" } else { "off" });
                0
            }
            _ => {
                eprintln!("usage: cc-share autostart on|off|status");
                2
            }
        },
        "lock" => {
            e.set("agent-locked", "");
            sysq(&["kill", "-s", "HUP", "control-center-agent"]);
            println!("agent locked: every Frame's commands are refused and its tag screens closed (cc-share unlock)");
            0
        }
        "unlock" => {
            let _ = std::fs::remove_file(e.dir.join("agent-locked"));
            sysq(&["kill", "-s", "HUP", "control-center-agent"]);
            println!("agent unlocked (and any Frame blocked from the tag screen is allowed again)");
            0
        }
        "guard" => guard(&e),
        "announce" => announce_cmd(&e, if arg(0).is_empty() { "status" } else { arg(0) }),
        "announce-run" => announce_run(&e),
        "frame-run" => frame_run(&e, arg(0)),
        "window-run" => window_run(&e, arg(0)),
        "windows" => windows_cmd(&e, if arg(0).is_empty() { "status" } else { arg(0) }),
        "unpair" => unpair(&e, arg(0), arg(1)),
        "frames" => frames_cmd(&e),
        "stop" => stop_cmd(),
        "uninstall" => uninstall(&e),
        "status" => i32::from(!sys(&["list-units", "--all", "--no-pager", "control-center-*"])),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avahi_publishing_off_as_on_steamos() {
        assert!(avahi_publishing_off("[server]\nuse-ipv4=yes\n[publish]\ndisable-publishing=yes\n"));
        assert!(avahi_publishing_off("[publish]\n disable-user-service-publishing = yes\n"));
        assert!(!avahi_publishing_off("[publish]\n#disable-publishing=yes\npublish-addresses=no\n"));
        assert!(!avahi_publishing_off("[server]\ndisable-publishing=yes\n"), "only [publish]'s");
    }

    #[test]
    fn firewalld_opens_trusted_zones_only() {
        // a Steam Deck's, word for word
        let deck = active_zones("home\n  interfaces: wlan0\npublic (default)\n");
        assert_eq!(deck, [("home".to_owned(), vec!["wlan0".to_owned()]), ("public".to_owned(), vec![])]);
        let zone = |c: &[String]| c.iter().find_map(|w| w.strip_prefix("--zone=")).map(str::to_owned);
        // add: into home only, and an older cc-host's rules come out of public
        let (c, note) = firewalld_cmds("add", &deck, |z| z == "public");
        assert!(note.is_none());
        let adds: Vec<_> = c.iter().filter(|c| c.iter().any(|w| w.starts_with("--add"))).filter_map(|c| zone(c)).collect();
        let removes: Vec<_> = c.iter().filter(|c| c.iter().any(|w| w.starts_with("--remove"))).filter_map(|c| zone(c)).collect();
        assert!(adds.len() == NETS.len() && adds.iter().all(|z| z == "home"), "{adds:?}");
        assert!(removes.len() == NETS.len() && removes.iter().all(|z| z == "public"), "{removes:?}");
        assert_eq!(c.last().unwrap(), &["sudo", "firewall-cmd", "--reload"]);
        // the network only in public: nothing opened, and a note saying how to move it
        let (c, note) = firewalld_cmds("add", &active_zones("public (default)\n  interfaces: eth0\n"), |_| false);
        assert!(c.is_empty());
        assert!(note.unwrap().iter().any(|l| l.contains("--zone=home --change-interface=eth0")));
        // remove: wherever ours are
        let (c, _) = firewalld_cmds("remove", &deck, |z| z == "home");
        assert!(c.iter().filter_map(|c| zone(c)).all(|z| z == "home") && c.len() == NETS.len() + 1);
        assert_eq!(active_zones(""), [("public".to_owned(), vec![])], "none listed: treated as the default, public");
    }

    #[test]
    fn sockets_from_proc() {
        let lis = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = lis.local_addr().unwrap().port();
        assert!(listening(port));
        let _c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_s, peer) = lis.accept().unwrap();
        assert!(sockets().iter().any(|(p, st, a)| *p == port && *st == 1 && *a == peer.ip()), "{peer}");
        let six = std::net::TcpListener::bind("[::1]:0");
        if let Ok(l6) = six {
            assert!(listening(l6.local_addr().unwrap().port()));
        }
    }

    #[test]
    fn monitor_answers() {
        assert_eq!(pick("", 3), Ok(vec!["0".into(), "1".into(), "2".into()]));
        assert_eq!(pick(" All ", 2), Ok(vec!["0".into(), "1".into()]));
        assert_eq!(pick("1", 3), Ok(vec!["1".into()]));
        assert_eq!(pick("2, 0 2", 3), Ok(vec!["2".into(), "0".into()]));
        assert!(pick("3", 3).is_err());
        assert!(pick("-1", 3).is_err());
        assert!(pick("one", 3).is_err());
    }

    #[test]
    fn picks_the_biggest_same_shape_mode() {
        let o = json!({"currentModeId": "1", "modes": [
            {"id": "1", "size": {"width": 5120, "height": 1440}, "refreshRate": 120.0},
            {"id": "2", "size": {"width": 3840, "height": 1080}, "refreshRate": 60.0},
            {"id": "3", "size": {"width": 3840, "height": 1080}, "refreshRate": 120.0},
            {"id": "4", "size": {"width": 2560, "height": 1440}, "refreshRate": 144.0},
            {"id": "5", "size": {"width": 1920, "height": 1080}, "refreshRate": 60.0}]});
        assert_eq!(mode_for(&o, 4278016), Some(("1".into(), "3".into()))); // 32:9, the most pixels, the fastest.
        assert_eq!(mode_for(&o, 8000000), Some(("1".into(), "1".into()))); // Already under the limit.
    }
}
