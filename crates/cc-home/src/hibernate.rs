//! Hibernate keeps the Desktop's open apps across a deliberate close and opens them again next
//! time (I asked for this on 2026-10-03). KDE's own session save records nothing on Wayland, and the apps' GPU
//! and Wayland connections can't be frozen, so this works at the app level: I write down what's
//! running and relaunch it.
//!   cc-home hibernate save     (session/cc-rest, before the session stops) every normal
//!                              window's app, in KWin's order, to desktop-hibernate.json
//!   cc-home hibernate restore  (session/cc-launch, once Plasma is up) each app launched once
//!                              in the session, the file then moved to .restored (a crash loop
//!                              never relaunches twice); cc-panels' windows.rs takes the panels' places
//!   cc-home hibernate dry      what save would write
//! It's a list of apps, not their state. Open documents, tabs and unsaved work are up to the apps.
//! Each app is saved as its .desktop id (KWin's desktopFileName, launched with kstart
//! --application), or else its /proc/<pid>/cmdline, which only gets relaunched for programs named
//! in hibernate-allow. What's left out: Plasma's own windows, cc-view's viewers, dialogs, anything
//! that isn't a normal window, and whatever the session's autostart opens anyway (a tray app, say). The
//! file is 0600, and the last one that had apps in it is kept as .prev.
use crate::{cache as config_cache, home_dir};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

/// The runtime dir session/cc-desktop uses.
fn desktop() -> String {
    format!("/run/user/{}/cc-desktop", unsafe { libc::getuid() })
}

fn unload(bus: &zbus::blocking::Connection, name: &str) {
    let _ = bus.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &(name,));
}

/// Loads and starts a script file under `name`, unloading any earlier one with that name first.
/// Same as cc-panels' kwin.rs.
fn load(bus: &zbus::blocking::Connection, path: &str, name: &str) -> Result<(), String> {
    unload(bus, name);
    let id: i32 = bus
        .call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "loadScript", &(path, name))
        .and_then(|m| m.body().deserialize())
        .map_err(|e| format!("loadScript {path}: {e}"))?;
    if id < 0 {
        return Err(format!("KWin refused {path}"));
    }
    bus.call_method(Some("org.kde.KWin"), format!("/Scripting/Script{id}").as_str(), Some("org.kde.kwin.Script"), "run", &())
        .map_err(|e| format!("run {path}: {e}"))?;
    Ok(())
}

const NAME: &str = "org.controlcenter.Hibernate";
const SHELL: [&str; 4] = ["plasmashell", "org.kde.plasmashell", "krunner", "org.kde.krunner"]; // same list as cc-windows.js

/// One-shot KWin script that sends every window, as KWin has it, back to us. A script can only
/// call out, so it has to come back this way.
const SCRIPT: &str = r#"
const out = workspace.windowList().map(w => ({
    uuid: w.internalId.toString(), app: w.desktopFileName || "", cls: w.resourceClass || "",
    pid: w.pid, caption: w.caption, minimized: w.minimized, normal: w.normalWindow,
    managed: w.managed && !w.deleted, transient: !!w.transient, skipTaskbar: w.skipTaskbar,
    special: w.desktopWindow || w.dock || w.notification || w.onScreenDisplay || w.popupWindow || w.splash}));
callDBus("org.controlcenter.Hibernate", "/", "org.controlcenter.Hibernate", "Windows", JSON.stringify(out));
"#;

fn file() -> String {
    config_cache("desktop-hibernate.json")
}

fn log(msg: &str) {
    eprintln!("hibernate: {msg}"); // also to the Desktop's log, since cc-launch's own output goes nowhere
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(config_cache("cc-panels.log")) {
        let _ = writeln!(f, "hibernate: {msg}");
    }
}

/// The desktop ids and program names that the session's autostart opens.
fn autostarted() -> (HashSet<String>, HashSet<String>) {
    let (mut ids, mut progs) = (HashSet::new(), HashSet::new());
    for d in [format!("{}/.config/control-center/desktop/autostart", home_dir()), "/etc/xdg/autostart".into()] {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let Some(id) = n.strip_suffix(".desktop") else { continue };
            ids.insert(id.to_lowercase());
            let text = std::fs::read(e.path()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            if let Some(prog) = text.lines().find_map(|l| l.strip_prefix("Exec=")).and_then(|x| x.split_whitespace().next()) {
                progs.insert(base(prog).to_lowercase());
            }
        }
    }
    (ids, progs)
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A program's name from its argv. Electron puts it all in one string with spaces.
fn prog(cmd: &[String]) -> String {
    cmd.first().and_then(|a| a.split(' ').next()).map(|a| base(a).to_lowercase()).unwrap_or_default()
}

/// Turns KWin's windows into the apps to relaunch, in order, one entry per app.
fn pick(wins: &[Value], cmdline: impl Fn(i64) -> Vec<String>, poses: &Value, auto: &(HashSet<String>, HashSet<String>)) -> Vec<Value> {
    let mut apps: Vec<Value> = Vec::new();
    for w in wins {
        let (cls, app) = (w["cls"].as_str().unwrap_or("").to_lowercase(), w["app"].as_str().unwrap_or(""));
        let al = app.to_lowercase();
        let flag = |k: &str| w[k].as_bool().unwrap_or(false);
        if !flag("managed") || !flag("normal") || flag("transient") || flag("special") || SHELL.contains(&cls.as_str()) || SHELL.contains(&al.as_str())
            || cls.starts_with("cc-view-") || cls.starts_with("xfreerdp") || al == "com.freerdp.freerdp" || al.starts_with("xfreerdp")
        {
            continue;
        }
        let pid = w["pid"].as_i64().unwrap_or(0);
        let cmd = if pid > 0 { cmdline(pid) } else { vec![] };
        let p = prog(&cmd);
        if auto.0.contains(&al) || auto.0.contains(&cls) || (!p.is_empty() && auto.1.contains(&p)) {
            continue;
        }
        let key = if app.is_empty() { w["cls"].as_str().unwrap_or("") } else { app }; // cc-windows.js's `app`
        if key.is_empty() && cmd.is_empty() {
            continue;
        }
        let pose = &poses[w["uuid"].as_str().unwrap_or("")];
        if let Some(seen) = apps.iter_mut().find(|a| a["app"] == key) {
            if !pose.is_null() {
                seen["poses"].as_array_mut().unwrap().push(pose.clone()); // kept in case the app reopens that window
            }
            continue;
        }
        let poses = if pose.is_null() { vec![] } else { vec![pose.clone()] };
        apps.push(json!({"app": key, "desktop": app, "cmdline": cmd, "minimized": flag("minimized"), "poses": poses}));
    }
    apps
}

/// The session plasmashell's whole environment, since that's what apps launched from Plasma get.
fn session_env() -> Option<Vec<(String, String)>> {
    let runtime = desktop();
    for d in std::fs::read_dir("/proc").ok()?.flatten() {
        let p = d.path();
        if !std::fs::read_to_string(p.join("comm")).is_ok_and(|c| c.trim() == "plasmashell") {
            continue;
        }
        let Ok(raw) = std::fs::read(p.join("environ")) else { continue };
        let vars: Vec<(String, String)> =
            raw.split(|&b| b == 0).filter_map(|kv| String::from_utf8_lossy(kv).split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned()))).collect();
        if vars.iter().any(|(k, v)| k == "XDG_RUNTIME_DIR" && *v == runtime) {
            return Some(vars);
        }
    }
    None
}

fn cmdline(pid: i64) -> Vec<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    raw.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
}

/// The argv of every process in the session. One that hides its environ, like some tray apps, won't
/// show up here, but it's autostarted anyway.
fn session_cmdlines() -> Vec<Vec<String>> {
    let want = format!("XDG_RUNTIME_DIR={}", desktop()).into_bytes();
    std::fs::read_dir("/proc").into_iter().flatten().flatten().filter_map(|d| {
        let pid: i64 = d.file_name().to_str()?.parse().ok()?;
        let env = std::fs::read(d.path().join("environ")).ok()?;
        env.split(|&b| b == 0).any(|kv| kv == want.as_slice()).then(|| cmdline(pid))
    }).collect()
}

struct Sink(mpsc::Sender<String>);

#[zbus::interface(name = "org.controlcenter.Hibernate")]
impl Sink {
    fn windows(&self, text: &str) {
        let _ = self.0.send(text.to_owned());
    }
}

/// Every window, from a one-shot KWin script over the session's bus. Gives up after 3 s.
fn query() -> Result<Vec<Value>, String> {
    let env = session_env().unwrap_or_default();
    let dbus = env.iter().find(|(k, _)| k == "DBUS_SESSION_BUS_ADDRESS").map(|(_, v)| v.clone()).filter(|v| !v.is_empty()).ok_or("no session bus")?;
    let (tx, rx) = mpsc::channel();
    let bus = zbus::blocking::connection::Builder::address(dbus.as_str())
        .and_then(|b| b.name(NAME)?.serve_at("/", Sink(tx))?.build())
        .map_err(|e| format!("session bus: {e}"))?;
    let path = config_cache("hibernate.js");
    std::fs::write(&path, SCRIPT).map_err(|e| e.to_string())?;
    load(&bus, &path, "cc-hibernate")?;
    let got = rx.recv_timeout(Duration::from_secs(3));
    unload(&bus, "cc-hibernate");
    let text = got.map_err(|_| "KWin's script didn't answer")?;
    serde_json::from_str::<Vec<Value>>(&text).map_err(|e| e.to_string())
}

fn save(dry: bool) {
    if session_env().is_none() {
        return log("no desktop session: nothing saved");
    }
    let wins = match query() {
        Ok(w) => w,
        Err(e) => return log(&format!("can't list the windows ({e}): nothing saved")), // the file from before stays put
    };
    let poses: Value = std::fs::read_to_string(config_cache("win-poses.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let apps = pick(&wins, cmdline, &poses, &autostarted());
    if dry {
        return println!("{}", serde_json::to_string_pretty(&apps).unwrap_or_default());
    }
    let (f, tmp) = (file(), format!("{}.new", file()));
    let text = serde_json::to_string_pretty(&json!({"saved": now(), "apps": apps})).unwrap_or_default();
    use std::os::unix::fs::OpenOptionsExt;
    let written = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut o| o.write_all(text.as_bytes())); // it holds commands, so only you can read it
    if let Err(e) = written {
        return log(&format!("can't write {tmp} ({e}): nothing saved"));
    }
    // Keep the last list that had apps, so a few empty closes in a row never wipe it.
    if std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).is_some_and(|v| v["apps"].as_array().is_some_and(|a| !a.is_empty())) {
        let _ = std::fs::rename(&f, format!("{f}.prev"));
    }
    let _ = std::fs::rename(&tmp, &f);
    let names: Vec<&str> = apps.iter().filter_map(|a| a["app"].as_str()).collect();
    log(&format!("saved {} app(s): {}", apps.len(), if names.is_empty() { "-".into() } else { names.join(" ") }));
}

fn now() -> String {
    let out = std::process::Command::new("date").arg("+%F %T").output().ok();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default()
}

/// The session's .desktop entry for KWin's id, <id>.desktop, looked up the way Plasma does it:
/// data home first, then the data dirs.
fn find_desktop(id: &str, env: &[(String, String)]) -> Option<String> {
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let home = get("XDG_DATA_HOME").unwrap_or_else(|| format!("{}/.local/share", home_dir()));
    let dirs = get("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let apps: Vec<String> = std::iter::once(home.as_str()).chain(dirs.split(':')).filter(|d| !d.is_empty()).map(|d| format!("{d}/applications")).collect();
    if apps.iter().any(|a| std::path::Path::new(&format!("{a}/{id}.desktop")).is_file()) {
        return Some(id.to_owned());
    }
    // When the app sets it that way, KWin's id is the window's class (Flatpaks, Electron). Vivaldi
    // says "Vivaldi-flatpak" but its entry is com.vivaldi.Vivaldi.desktop, so fall back to the
    // entry whose StartupWMClass matches.
    apps.iter().flat_map(|a| std::fs::read_dir(a).into_iter().flatten().flatten()).find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let stem = name.strip_suffix(".desktop")?.to_owned();
        let text = std::fs::read_to_string(e.path()).ok()?;
        text.lines().any(|l| l.strip_prefix("StartupWMClass=").is_some_and(|c| c.trim().eq_ignore_ascii_case(id))).then_some(stem)
    })
}

/// Programs (one name per line) that get relaunched from their saved command line when they have
/// no .desktop entry. Anything else without one isn't relaunched.
fn allowed() -> HashSet<String> {
    let text = std::fs::read_to_string(format!("{}/.config/control-center/hibernate-allow", home_dir())).unwrap_or_default();
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}

/// Each app's launch command: its .desktop entry, or a command line whose program is in allow.
/// Apps that are already running (autostarted, or a session that never stopped) are left out.
fn plan(apps: &[Value], running: &[Vec<String>], desktop: impl Fn(&str) -> Option<String>, allow: &HashSet<String>) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for a in apps {
        let cmd: Vec<String> = serde_json::from_value(a["cmdline"].clone()).unwrap_or_default();
        let (app, id) = (a["app"].as_str().unwrap_or("").to_owned(), a["desktop"].as_str().unwrap_or(""));
        if !cmd.is_empty() && running.contains(&cmd) {
            continue;
        }
        if let Some(entry) = Some(id).filter(|i| !i.is_empty()).and_then(&desktop) {
            out.push((app, vec!["kstart".into(), "--application".into(), entry]));
        } else if !cmd.is_empty() && allow.contains(&prog(&cmd)) {
            out.push((app, cmd));
        }
    }
    out
}

fn restore() {
    let f = file();
    if !std::path::Path::new(&f).exists() {
        return;
    }
    let Some(env) = session_env() else { return log(&format!("no desktop session: {f} kept for next time")) };
    // Claim the file by renaming it before reading. If two launches happen at once (live,
    // 2026-10-03: a double click reopened Konsole twice), only the one whose rename wins
    // relaunches anything, and a crash from here on never relaunches twice.
    let taken = format!("{f}.restored");
    if std::fs::rename(&f, &taken).is_err() {
        return;
    }
    let apps = match std::fs::read_to_string(&taken).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string())) {
        Ok(v) => v["apps"].as_array().cloned().unwrap_or_default(),
        Err(e) => {
            log(&format!("unreadable {taken} ({e})"));
            vec![]
        }
    };
    let mut launched = Vec::new();
    for (app, cmd) in plan(&apps, &session_cmdlines(), |d| find_desktop(d, &env), &allowed()) {
        let mut c = std::process::Command::new(&cmd[0]);
        c.args(&cmd[1..]).env_clear().envs(env.iter().cloned()).current_dir(home_dir()).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        use std::os::unix::process::CommandExt;
        unsafe { c.pre_exec(|| { libc::setsid(); Ok(()) }) }; // its own session, so it outlives us
        match c.spawn() {
            Ok(_) => launched.push(app),
            Err(e) => log(&format!("can't launch {app} ({e})")),
        }
    }
    log(&format!("restored {} of {} app(s): {}", launched.len(), apps.len(), if launched.is_empty() { "-".into() } else { launched.join(" ") }));
}

/// `cc-home hibernate save|restore|dry`.
pub fn main(how: Option<&str>) -> Result<(), String> {
    match how {
        Some("save") => save(false),
        Some("dry") => save(true),
        Some("restore") => restore(),
        _ => return Err("cc-home hibernate save|restore|dry".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(uuid: &str, app: &str, cls: &str, pid: i64, extra: Value) -> Value {
        let mut v = json!({"uuid": uuid, "app": app, "cls": cls, "pid": pid, "caption": "", "minimized": false,
                           "normal": true, "managed": true, "transient": false, "skipTaskbar": false, "special": false});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn picks_and_plans() {
        let wins = vec![
            w("{1}", "org.kde.konsole", "konsole", 10, json!({})),
            w("{2}", "org.kde.plasmashell", "plasmashell", 0, json!({})),
            w("{3}", "1password", "1Password", 11, json!({})),
            w("{4}", "org.kde.dolphin", "", 12, json!({"transient": true})), // its dialog
            w("{5}", "org.kde.konsole", "konsole", 10, json!({"minimized": true})), // 2nd window
            w("{6}", "", "xterm", 13, json!({})), // no desktop id
            w("{7}", "", "cc-view-laptop", 14, json!({})),
            w("{8}", "org.kde.kate", "", 15, json!({"normal": false})),
            w("{9}", "", "foo", 16, json!({"special": true})),
        ];
        let cmds = |pid: i64| -> Vec<String> {
            match pid {
                10 => vec!["/usr/bin/konsole".into()],
                11 => vec!["/home/u/.local/share/1password/1password --silent".into()],
                13 => vec!["xterm".into(), "-e".into(), "top".into()],
                _ => vec![],
            }
        };
        let poses = json!({"{1}": {"yaw": 1}, "{5}": {"yaw": 5}});
        let one = |s: &str| [s.to_owned()].into_iter().collect::<HashSet<_>>();
        let apps = pick(&wins, cmds, &poses, &(one("1password"), one("1password")));
        assert_eq!(apps.iter().map(|a| a["app"].as_str().unwrap()).collect::<Vec<_>>(), ["org.kde.konsole", "xterm"]);
        assert_eq!(apps[0]["poses"], json!([{"yaw": 1}, {"yaw": 5}]));
        assert_eq!(apps[0]["minimized"], false);
        assert_eq!(apps[1], json!({"app": "xterm", "desktop": "", "cmdline": ["xterm", "-e", "top"], "minimized": false, "poses": []}));
        // autostart is caught by program name alone
        assert!(pick(&wins[2..3], cmds, &json!({}), &(HashSet::new(), one("1password"))).is_empty());
        // the launch plan: desktop id first, else an allowed cmdline, and running ones skipped
        let gone = json!({"app": "gone", "desktop": "gone", "cmdline": [], "minimized": false, "poses": []});
        let all = [apps.clone(), vec![gone]].concat();
        let steps = plan(&all, &[vec!["xterm".into(), "-e".into(), "top".into()]], |d| (d == "org.kde.konsole").then(|| d.to_owned()), &one("xterm"));
        assert_eq!(steps, [("org.kde.konsole".to_owned(), vec!["kstart".to_owned(), "--application".into(), "org.kde.konsole".into()])]);
        assert_eq!(plan(&apps, &[], |_| None, &one("konsole")), [("org.kde.konsole".to_owned(), vec!["/usr/bin/konsole".to_owned()])]);
        assert!(plan(&apps, &[], |_| None, &HashSet::new()).is_empty(), "no .desktop, not allowed: not relaunched");
        // a Flatpak's class finds its entry by StartupWMClass
        let dir = std::env::temp_dir().join(format!("cc-hib-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("applications")).unwrap();
        std::fs::write(dir.join("applications/com.vivaldi.Vivaldi.desktop"), "[Desktop Entry]\nName=Vivaldi\nStartupWMClass=Vivaldi-flatpak\n").unwrap();
        let env = vec![("XDG_DATA_HOME".to_owned(), dir.to_string_lossy().into_owned()), ("XDG_DATA_DIRS".to_owned(), String::new())];
        assert_eq!(find_desktop("Vivaldi-flatpak", &env).as_deref(), Some("com.vivaldi.Vivaldi"));
        assert_eq!(find_desktop("com.vivaldi.Vivaldi", &env).as_deref(), Some("com.vivaldi.Vivaldi"));
        assert_eq!(find_desktop("nothing", &env), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
