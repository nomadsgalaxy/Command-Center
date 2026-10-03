//! Tests cc-share's commands (cc-host, src/share.rs) against the bash cc-share they replaced. Its runs
//! are recorded in tests/fixtures/share/, made from it once before it was removed. It's black-box on
//! throwaway HOMEs, and the system's tools are shims that log every call (systemctl, kscreen-doctor
//! with a two-monitor desk, loginctl, avahi-publish, notify-send, kbuildsycoca6, ufw, firewall-cmd,
//! sudo, ss, krdpserver), so nothing on this machine changes. firewall-cmd is always shimmed, so a
//! run doesn't depend on whether the machine has it (the recording expects it there). Each scenario's output, the tools it called and
//! the files it left have to match the recording. CC_RECORD=1 rewrites the recording from cc-host;
//! after an intended change, review the diff. This was tests/share-cross (bash).
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DESK: &str = r#"{"outputs": [
 {"name": "DP-3", "enabled": true, "priority": 2, "size": {"width": 1440, "height": 2560}, "currentModeId": "7",
  "modes": [{"id": "7", "size": {"width": 1440, "height": 2560}, "refreshRate": 60.0}]},
 {"name": "HDMI-A-1", "enabled": false, "priority": 3, "size": {"width": 1920, "height": 1080}},
 {"name": "DP-1", "enabled": true, "priority": 1, "size": {"width": 5120, "height": 1440}, "currentModeId": "1",
  "modes": [{"id": "1", "size": {"width": 5120, "height": 1440}, "refreshRate": 120.0},
            {"id": "3", "size": {"width": 3840, "height": 1080}, "refreshRate": 120.0}]}
]}
"#;

/// A scenario's step: either a cc-share command or a setup on the throwaway HOME.
enum Step {
    Cmd(&'static str),
    Shared(&'static str),
    Dir,
    Frame,
    Announce,
    Ufw(bool),
}

struct Run {
    dir: PathBuf,
    work: PathBuf,
}

impl Run {
    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    /// Writes the system's tools as shims (POSIX sh, test-only) that log each call to <run>/log.
    fn shims(&self) {
        let bin = self.dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let (log, work) = (self.dir.join("log"), &self.work);
        for t in ["systemctl", "kscreen-doctor", "loginctl", "avahi-publish", "notify-send", "kbuildsycoca6", "ufw", "firewall-cmd", "sudo", "ss", "krdpserver"] {
            let body = format!(r#"#!/bin/sh
echo "{t} $*" >> "{log}"
case "{t} $*" in
  "kscreen-doctor -j") cat "{work}/desk.json" ;;
  "loginctl "*) echo no ;;
  "systemctl is-active -q "*) [ -e "{work}/$3-on" ] ;;
  "systemctl --user is-active "*) false ;;
  "systemctl --user is-enabled "*) false ;;
  "systemctl --user list-units --no-legend --state=running control-center-frame@f1-*")
    echo "control-center-frame@f1-1.service loaded active running Command Center share for paired Frame f1-1" ;;
  *) true ;;
esac
"#, log = log.display(), work = work.display());
            let p = bin.join(t);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn setup(&self, s: &Step) {
        let conf = self.home().join(".config/control-center");
        match s {
            Step::Dir => std::fs::create_dir_all(&conf).unwrap(),
            Step::Shared(m) => {
                std::fs::create_dir_all(&conf).unwrap();
                std::fs::write(conf.join("shared"), m).unwrap();
            }
            Step::Announce => std::fs::write(conf.join("announce"), "on\n").unwrap(),
            Step::Frame => {
                std::fs::create_dir_all(conf.join("frames")).unwrap();
                std::fs::create_dir_all(conf.join("trusted-frames")).unwrap();
                std::fs::write(conf.join("frames/f1.json"), "{\"slot\": 1, \"user\": \"cc-f1\", \"password\": \"x\"}\n").unwrap();
                std::fs::write(conf.join("trusted-frames/f1.pub"), "k\n").unwrap();
                let last = File::create(conf.join("frames/f1.last")).unwrap();
                // 2026-10-01 16:34 UTC, as recorded, since frames prints it in UTC.
                last.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1790872440)).unwrap();
            }
            Step::Ufw(on) => {
                let f = self.work.join("ufw-on");
                if *on { std::fs::write(f, "").unwrap() } else { let _ = std::fs::remove_file(f); }
            }
            Step::Cmd(_) => unreachable!(),
        }
    }

    fn cmd(&self, line: &str) {
        let (out, log) = (self.dir.join("out"), self.dir.join("log"));
        let append = |p: &Path| OpenOptions::new().create(true).append(true).open(p).unwrap();
        writeln!(append(&out), "$ cc-share {line}").unwrap();
        writeln!(append(&log), "-- cc-share {line}").unwrap();
        let o = append(&out);
        let status = Command::new(self.home().join(".local/share/control-center/cc-host")).args(line.split_whitespace())
            .env_clear().env("HOME", self.home()).env("USER", "tester").env("TZ", "UTC").env("CC_PASSWORD", "sekrit")
            .env("PATH", format!("{}:/usr/bin:/bin", self.dir.join("bin").display()))
            .stdin(Stdio::null()).stdout(o.try_clone().unwrap()).stderr(o).status().unwrap();
        writeln!(append(&out), "[exit {}]", status.code().unwrap_or(-1)).unwrap();
    }

    /// The files left, as "path mode[: content]". Secrets and cc-host's own files only show that they're there.
    fn files(&self) -> String {
        let home = self.home();
        let mut lines = vec![];
        let mut stack = vec![home.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let md = std::fs::symlink_metadata(&p).unwrap();
                if md.is_dir() {
                    stack.push(p);
                    continue;
                }
                let rel = format!("./{}", p.strip_prefix(&home).unwrap().display());
                let mode = format!("{:o}", md.permissions().mode() & 0o777);
                let secret = matches!(rel.as_str(), "./.config/control-center/password" | "./.config/control-center/cert.pem" | "./.config/control-center/key.pem" | "./.local/bin/cc-share")
                    || rel.starts_with("./.local/share/control-center/");
                if md.file_type().is_symlink() {
                    lines.push(format!("{rel} {mode} -> {}", std::fs::read_link(&p).unwrap().display())); // It gets normalised to HOME/...
                } else if secret {
                    lines.push(format!("{rel} {mode}"));
                } else {
                    let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).replace('\n', "|");
                    lines.push(format!("{rel} {mode}: {text}"));
                }
            }
        }
        lines.sort();
        lines.join("\n") + "\n"
    }

    /// Rewrites this run's paths, the host's name and cc-host's version the way the recording has them.
    fn norm(&self, text: &str) -> String {
        let h = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap().trim().to_owned();
        let (home, bin) = (self.home().display().to_string(), self.dir.join("bin").display().to_string());
        text.lines().filter(|l| !l.starts_with("ss ") && !l.starts_with("  copy cc-host to ")).map(|l| {
            let mut l = l.replace(&home, "HOME").replace(&bin, "BIN");
            for (from, to) in [(format!(" on {h}:"), " on HOSTNAME:".to_owned()), (format!("announcing {h} as"), "announcing HOSTNAME as".into()), (format!("sharing {h}:"), "sharing HOSTNAME:".into()), (format!("Pick {h},"), "Pick HOSTNAME,".into())] {
                l = l.replace(&from, &to);
            }
            if let Some(s) = l.strip_suffix(&format!(" on {h}")) {
                l = format!("{s} on HOSTNAME");
            }
            // "cc-host 0.1.0)" -> "cc-host VERSION)", wherever it is on the line.
            let mut from = 0;
            while let Some(k) = l[from..].find("cc-host ") {
                let i = from + k + 8;
                let n = l[i..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').count();
                if n > 0 && l[i + n..].starts_with(')') {
                    l = format!("{}VERSION{}", &l[..i], &l[i + n..]);
                }
                from = i;
            }
            l + "\n"
        }).collect()
    }
}

fn scenario(work: &Path, name: &str, steps: &[Step], fixtures: &Path, record: bool) -> Vec<String> {
    let run = Run { dir: work.join(name), work: work.to_owned() };
    let share = run.home().join(".local/share/control-center");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_cc-host"), share.join("cc-host")).unwrap();
    run.shims();
    std::fs::write(run.dir.join("out"), "").unwrap();
    for s in steps {
        match s {
            Step::Cmd(c) => run.cmd(c),
            other => run.setup(other),
        }
    }
    let got = [("out", std::fs::read_to_string(run.dir.join("out")).unwrap()), ("log", std::fs::read_to_string(run.dir.join("log")).unwrap_or_default()), ("files", run.files())];
    let mut fails = vec![];
    for (k, text) in got {
        let mine = run.norm(&text);
        let path = fixtures.join(format!("{name}.{k}"));
        if record {
            std::fs::write(&path, &mine).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        let (mut a, mut b): (Vec<&str>, Vec<&str>) = (want.lines().collect(), mine.lines().collect());
        if k == "files" {
            a.sort(); // The recording was sorted by sort(1), in its locale.
            b.sort();
        }
        if a != b {
            let first = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
            fails.push(format!("{name}.{k}: first difference at line {}:\n  recorded: {:?}\n  cc-host:  {:?}", first + 1, a.get(first), b.get(first)));
        }
    }
    fails
}

#[test]
fn share_commands_match_the_recorded_bash() {
    use Step::*;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/share");
    let record = std::env::var("CC_RECORD").is_ok();
    let work = std::env::temp_dir().join(format!("cc-host-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("desk.json"), DESK).unwrap();
    let scenarios: Vec<(&str, Vec<Step>)> = vec![
        ("list", vec![Cmd("list")]),
        ("install", vec![Cmd("install 0 1 --no-announce --no-autostart"), Cmd("check"), Cmd("autostart status")]),
        ("dry-run", vec![Cmd("install 0 1 --dry-run")]),
        ("autostart", vec![Shared("0\n"), Cmd("autostart status"), Cmd("autostart on"), Cmd("autostart status"), Cmd("autostart off"), Cmd("autostart status"), Cmd("autostart bogus")]),
        ("up-down", vec![Shared("0\n1\n"), Announce, Cmd("up"), Cmd("down")]),
        ("lock", vec![Dir, Cmd("lock"), Cmd("unlock")]),
        ("announce", vec![Dir, Cmd("announce status"), Cmd("announce on"), Cmd("announce off"), Cmd("announce bogus")]),
        ("windows", vec![Cmd("windows status"), Cmd("windows on"), Cmd("windows off"), Cmd("windows bogus")]),
        ("frames", vec![Frame, Cmd("frames"), Cmd("unpair ../x"), Cmd("unpair nobody"), Cmd("unpair f1"), Cmd("frames")]),
        ("firewall", vec![Frame, Ufw(true), Cmd("unpair f1"), Cmd("install 0 --no-announce --no-autostart"), Ufw(false)]),
        ("firewall-run", vec![Frame, Ufw(true), Cmd("unpair f1 --firewall"), Ufw(false)]),
        ("pair-first", vec![Cmd("pair")]),
        ("instances", vec![Cmd("frame-run bad"), Cmd("frame-run ../x-0"), Cmd("window-run f1-0-nope"), Cmd("window-run f1-9-0123abcd-0000-4000-8000-000000000000"), Cmd("windows on")]),
        ("uninstall", vec![Cmd("install 0 --no-announce --autostart"), Frame, Cmd("uninstall")]),
    ];
    let mut fails = vec![];
    for (name, steps) in &scenarios {
        fails.extend(scenario(&work, name, steps, &fixtures, record));
    }
    let _ = std::fs::remove_dir_all(&work);
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}
