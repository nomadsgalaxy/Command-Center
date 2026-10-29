//! The Frame controller's shoulder button (bumper) as right click on SteamVR's laser, so a
//! context menu is one press away (Planning/SteamVR Laser Binding.md, option 2). It's a user
//! binding for SteamVR's own laser app, `openvr.component.vrcompositor`: Valve's default for the
//! Frame controller with the two bumper clicks in `/actions/lasermouse` sending right click
//! instead of left. Everything else is Valve's, including LockMousePosition on the bumper, which
//! keeps the laser still while it's held.
//!
//! The binding is active while SteamVR runs. We write the user binding file any time
//! (`~/steamvr/input`). The selection goes through vrserver's own web API while SteamVR runs
//! (127.0.0.1:27062, as SteamVR's Controller Bindings page does it: `selectconfig`, which reloads
//! the laser's binding live, one binding load, no restart and no OpenVR connection), and into
//! steamvr.vrsettings only while it isn't, because vrserver rewrites that file on exit. It looks
//! first (`getactions`, which loads nothing), so a Desktop start that finds it selected costs
//! nothing at all.
//!
//! It never takes the place of a binding you picked yourself, and it fails safe: a missing or
//! broken file, a steamreset (which wipes ~/.config/openvr) or a new version of SteamVR's laser
//! manifest all put Valve's default back. settings.json `"bumper_right_click": false` keeps it
//! off, and takes ours out if it's there; removing undoes it.
use crate::home_dir;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP: &str = "openvr.component.vrcompositor";
/// Valve's Frame controller binding for the laser, in the read-only image: what ours starts from.
const DEFAULT: &str = "/opt/steamvr/drivers/frame_controller/resources/input/vrcompositor_bindings_frame_controller.json";
/// vrserver's local web API. It only answers requests that come from its own Controller Bindings page.
const API: &str = "http://127.0.0.1:27062";
const PAGE: &str = "http://127.0.0.1:27062/dashboard/controllerbinding.html";

/// Our binding file, where SteamVR keeps personal bindings.
fn ours() -> PathBuf {
    PathBuf::from(format!("{}/steamvr/input/{APP}_frame_controller.json", home_dir()))
}

/// SteamVR's settings. Spelled out from HOME: inside the Desktop, XDG_CONFIG_HOME points elsewhere.
fn vrsettings() -> PathBuf {
    PathBuf::from(format!("{}/.config/openvr/config/steamvr.vrsettings", home_dir()))
}

/// A binding's URL, the way vrserver's getactions reports them (`file:///…`, verified on the Frame).
fn url(p: &Path) -> String {
    format!("file://{}", p.display())
}

/// The selection's key in steamvr.vrsettings, for when SteamVR is off: controller type, manifest
/// version, binding kind. Read off vrserver's strings (`%s_%s%s`, `CurrentURL`, `_steamvrinput`);
/// nobody has seen one written on the Frame yet, so it's only used with SteamVR closed, and a
/// wrong spelling just leaves Valve's default in place.
fn key(version: u64) -> String {
    format!("frame_controller_{version}_CurrentURL_steamvrinput")
}

/// Valve's default with the bumper's left clicks on the laser made right clicks, and our name.
/// None if the default doesn't look the way it did (no lasermouse bumper click to change).
pub fn bumper_right_click(default: &Value) -> Option<Value> {
    let mut b = default.clone();
    let mut changed = 0;
    for s in b["bindings"]["/actions/lasermouse"]["sources"].as_array_mut()? {
        let bumper = s["path"].as_str().is_some_and(|p| p.ends_with("/input/bumper"));
        let out = &mut s["inputs"]["click"]["output"];
        if bumper && out == "/actions/lasermouse/in/leftclick" {
            *out = json!("/actions/lasermouse/in/rightclick");
            changed += 1;
        }
    }
    b["name"] = json!("Command Center: bumper is right click");
    b["description"] = json!(
        "Steam Frame dashboard defaults with the shoulder button (bumper) sending right click instead of left click. Installed by Command Center; remove it from Command Center or from the Controller Bindings UI."
    );
    (changed > 0).then_some(b)
}

/// What the laser's selection for the Frame controller is, against ours.
#[derive(Debug, PartialEq)]
enum Pick {
    Ours,
    Default, // Valve's, or nothing picked: ours may go in
    Theirs,  // a binding the user picked: never replaced
}

fn pick(current: Option<&str>, ours: &str) -> Pick {
    match current {
        Some(u) if u == ours => Pick::Ours,
        None | Some("") => Pick::Default,
        Some(u) if u.trim_start_matches("file://") == DEFAULT => Pick::Default,
        Some(_) => Pick::Theirs,
    }
}

/// The Frame controller's current binding for the laser, from getactions' answer.
fn current(actions: &Value) -> Option<&str> {
    actions["current_binding_url"]["frame_controller"].as_str()
}

/// Points the selection in `settings` at `url` for manifest `version`, unless the user picked
/// their own there.
fn select(settings: &mut Value, version: u64, url: &str) -> Pick {
    if !settings.is_object() {
        *settings = json!({});
    }
    let section = settings.as_object_mut().unwrap().entry(APP).or_insert_with(|| json!({}));
    if !section.is_object() {
        *section = json!({});
    }
    let k = key(version);
    match pick(section[&k].as_str(), url) {
        Pick::Theirs => Pick::Theirs,
        Pick::Ours => Pick::Ours,
        Pick::Default => {
            section[&k] = json!(url);
            Pick::Default // was the default: now ours
        }
    }
}

/// Takes out any selection of ours (pointing at `url`), and the section if that leaves it empty.
/// A selection the user made of something else is theirs, and stays. True if it changed.
fn unselect(settings: &mut Value, url: &str) -> bool {
    let Some(section) = settings.get_mut(APP).and_then(Value::as_object_mut) else { return false };
    let n = section.len();
    section.retain(|k, v| !(k.starts_with("frame_controller_") && k.contains("_CurrentURL_") && v == url));
    let changed = section.len() != n;
    if section.is_empty() {
        settings.as_object_mut().unwrap().remove(APP);
    }
    changed
}

fn steamvr_running() -> bool {
    !crate::session::named(Path::new("/proc"), "vrserver").is_empty()
}

/// A call to vrserver's web API: a GET of `path`, or a POST of `body` to it. It answers only with
/// its Controller Bindings page's Origin and Referer, so those go along.
fn web(path: &str, body: Option<&Value>) -> Result<String, String> {
    let mut c = Command::new("curl");
    c.args(["-sS", "--fail", "-m", "5", "-H", &format!("Origin: {API}"), "-H", &format!("Referer: {PAGE}")]);
    if let Some(b) = body {
        c.args(["-H", "Content-Type: application/json", "--data-binary", &b.to_string()]);
    }
    let out = c.arg(format!("{API}{path}")).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("SteamVR's web API: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn actions() -> Result<Value, String> {
    let t = web(&format!("/input/getactions.json?app_key={APP}"), None)?;
    serde_json::from_str(&t).map_err(|e| format!("SteamVR's getactions: {e}"))
}

/// Makes `url` the laser's binding for the Frame controller through the web API (one binding load),
/// then reads it back until SteamVR shows it, since the POST's answer is only an acknowledgement.
fn select_live(url: &str) -> Result<(), String> {
    web("/input/selectconfig.action", Some(&json!({ "app_key": APP, "controller_type": "frame_controller", "url": url })))?;
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if actions().ok().as_ref().and_then(current) == Some(url) {
            return Ok(());
        }
    }
    Err(format!("SteamVR didn't switch the laser's binding to {url}"))
}

/// Writes text to path through a fresh file beside it and a rename, keeping the old file's mode.
fn replace(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension(format!("cc-{}", std::process::id()));
    fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Ok(m) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, m.permissions());
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// Edits steamvr.vrsettings with f (SteamVR must be off: it'd overwrite the edit on exit). f says
/// whether it changed anything.
fn edit_vrsettings<T>(f: impl FnOnce(&mut Value) -> (T, bool)) -> Result<T, String> {
    let path = vrsettings();
    let mut v: Value = match fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}; left alone", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let (out, changed) = f(&mut v);
    if changed {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        replace(&path, &serde_json::to_string_pretty(&v).unwrap_or_default())?;
    }
    Ok(out)
}

fn wanted() -> bool {
    let s: Value = fs::read(crate::conf_dir().join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    s["bumper_right_click"] != false
}

const THEIRS: &str = "you've picked your own binding for SteamVR's laser, so the bumper stays as that binding has it";

/// Whether a call changed the laser's binding (so a Desktop start says so) or found it as it should be.
#[derive(Debug, PartialEq)]
pub enum Done {
    Changed,
    Unchanged,
}

/// A live select that SteamVR took but never switched to is written down here, with what it was
/// for (`stamp`). Until that changes, or `cc-home install` runs by hand, it isn't tried live again:
/// each try is a binding load, and SteamVR's budget for those running out stops the lasers.
fn marker() -> PathBuf {
    PathBuf::from(crate::cache("bumper-select-failed"))
}

/// What a live select is for: the binding it selects, Valve's manifest version, and our file (FNV-1a).
fn stamp(target: &str, version: u64, ours: &str) -> String {
    let h = ours.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{target} {version} {h:016x}")
}

/// Whether to try a live select for `now` (a `stamp`), given the marker's text, if any.
fn try_live(marker: Option<&str>, now: &str) -> bool {
    marker.map(str::trim) != Some(now)
}

/// select_live, at most once for the same stamp unless `by_hand`. A failure leaves the marker.
fn select_once(target: &str, now: &str, by_hand: bool) -> Result<(), String> {
    let m = marker();
    if !by_hand && !try_live(fs::read_to_string(&m).ok().as_deref(), now) {
        return Err("SteamVR didn't switch the laser's binding last time, so it isn't tried again by itself (each try costs a binding load); run cc-home install to try again".into());
    }
    match select_live(target) {
        Ok(()) => {
            let _ = fs::remove_file(&m);
            Ok(())
        }
        Err(e) => {
            if let Some(dir) = m.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(&m, now);
            Err(e)
        }
    }
}

/// Writes our binding file and selects it, or with it turned off takes it out. Returns whether
/// that changed anything and a line for the user. `by_hand` (cc-home install) also tries a live
/// select that failed before.
pub fn apply(by_hand: bool) -> Result<(Done, String), String> {
    if !wanted() {
        return restore(by_hand).map(|(d, _)| (d, "the bumper stays left click (settings.json \"bumper_right_click\": false)".into()));
    }
    let default: Value = fs::read(DEFAULT).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or(format!("{DEFAULT}: not there or not JSON, so the bumper stays as it is"))?;
    let version = default["action_manifest_version"].as_u64().ok_or("SteamVR's Frame controller binding has no action_manifest_version, so the bumper stays as it is")?;
    let ours_json = bumper_right_click(&default).ok_or("SteamVR's Frame controller binding has no bumper left click to change, so it stays as it is")?;
    let path = ours();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&ours_json).unwrap_or_default();
    if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        replace(&path, &text)?;
    }
    let u = url(&path);
    let same = |say: &str| Ok((Done::Unchanged, say.to_string()));
    if steamvr_running() {
        let a = actions()?;
        return match pick(current(&a), &u) {
            Pick::Ours => same("the bumper is right click on SteamVR's laser"),
            Pick::Theirs => same(THEIRS),
            Pick::Default => select_once(&u, &stamp(&u, version, &text), by_hand).map(|_| (Done::Changed, "the bumper is right click on SteamVR's laser, now (one binding load)".into())),
        };
    }
    match edit_vrsettings(|v| {
        let p = select(v, version, &u);
        let changed = p == Pick::Default;
        (p, changed)
    })? {
        Pick::Theirs => same(THEIRS),
        Pick::Ours => same("the bumper is right click on SteamVR's laser"),
        Pick::Default => Ok((Done::Changed, "the bumper is right click on SteamVR's laser from SteamVR's next start".into())),
    }
}

/// At the Desktop's start: puts the binding back if it went (a steamreset, a deleted file), or
/// takes ours out if it's been turned off. It only speaks when that changed something, or failed.
pub fn check() {
    match apply(false) {
        Ok((Done::Changed, say)) => eprintln!("{say}"),
        Ok((Done::Unchanged, _)) => {}
        Err(e) => eprintln!("bumper right click: {e}"),
    }
}

/// Undoes it: if the laser's selection is ours, back to Valve's default (through the web API while
/// SteamVR runs, one binding load; in steamvr.vrsettings while it doesn't), then our file deleted.
pub fn restore(by_hand: bool) -> Result<(Done, String), String> {
    let path = ours();
    let u = url(&path);
    let mut done = (Done::Unchanged, "the bumper is left click");
    if steamvr_running() {
        let a = actions()?;
        if pick(current(&a), &u) == Pick::Ours {
            let target = format!("file://{DEFAULT}");
            select_once(&target, &stamp(&target, 0, &u), by_hand)?;
            done = (Done::Changed, "the bumper is left click again (one binding load)");
        }
    } else if edit_vrsettings(|v| {
        let c = unselect(v, &u);
        (c, c)
    })? {
        done = (Done::Changed, "the bumper is left click again from SteamVR's next start");
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    Ok((done.0, done.1.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default() -> Value {
        json!({
            "action_manifest_version": 0,
            "app_key": APP,
            "controller_type": "frame_controller",
            "name": "Default VR Dashboard bindings for Steam Frame Controller",
            "bindings": {
                "/actions/lasermouse": { "sources": [
                    { "path": "/user/hand/left/input/trigger", "inputs": { "click": { "output": "/actions/lasermouse/in/leftclick" } } },
                    { "path": "/user/hand/left/input/bumper", "inputs": { "click": { "output": "/actions/lasermouse/in/leftclick" } } },
                    { "path": "/user/hand/left/input/bumper", "inputs": { "click": { "output": "/actions/lasermouse/in/LockMousePosition" } } },
                    { "path": "/user/hand/right/input/bumper", "inputs": { "click": { "output": "/actions/lasermouse/in/leftclick" } } },
                ]},
                "/actions/lasermouse_secondary": { "sources": [
                    { "path": "/user/hand/left/input/bumper", "inputs": { "click": { "output": "/actions/lasermouse_secondary/in/switchlaserhand" } } },
                ]},
            }
        })
    }

    #[test]
    fn only_the_two_bumper_clicks_on_the_laser_change() {
        let d = default();
        let b = bumper_right_click(&d).unwrap();
        let outs = |v: &Value, set: &str| v["bindings"][set]["sources"].as_array().unwrap().iter().map(|s| s["inputs"]["click"]["output"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(
            outs(&b, "/actions/lasermouse"),
            ["/actions/lasermouse/in/leftclick", "/actions/lasermouse/in/rightclick", "/actions/lasermouse/in/LockMousePosition", "/actions/lasermouse/in/rightclick"],
            "the trigger and LockMousePosition stay"
        );
        assert_eq!(outs(&b, "/actions/lasermouse_secondary"), outs(&d, "/actions/lasermouse_secondary"), "the off hand's bumper stays");
        assert_eq!((&b["action_manifest_version"], &b["app_key"], &b["controller_type"]), (&json!(0), &json!(APP), &json!("frame_controller")));
        assert!(b["name"].as_str().unwrap().starts_with("Command Center"));
        let mut gone = d.clone();
        gone["bindings"]["/actions/lasermouse"]["sources"] = json!([]);
        assert_eq!(bumper_right_click(&gone), None, "a default that changed shape: leave it alone");
    }

    /// getactions' answer on the Frame (2026-10-29, trimmed to the fields we read): full file:// URLs per controller type.
    const ACTIONS: &str = r#"{"version": 0, "category": "steamvr_input",
        "current_binding_url": {"frame_controller": "file:///opt/steamvr/drivers/frame_controller/resources/input/vrcompositor_bindings_frame_controller.json", "cc_pointer": "file:///home/steamos/.local/share/control-center/cc_pointer-dc635f17ceed/resources/input/cc_pointer_vrcompositor.json"},
        "previous_binding_url": {"frame_controller": ""}}"#;

    #[test]
    fn the_live_selection_is_read_as_valves_ours_or_the_users() {
        let mine = "file:///home/x/steamvr/input/openvr.component.vrcompositor_frame_controller.json";
        let a: Value = serde_json::from_str(ACTIONS).unwrap();
        assert_eq!(pick(current(&a), mine), Pick::Default, "Valve's own: ours may go in");
        assert_eq!(pick(Some(mine), mine), Pick::Ours, "already ours: nothing to do, no binding load");
        assert_eq!(pick(Some("file:///home/x/steamvr/input/my_own.json"), mine), Pick::Theirs, "the user's: never replaced");
        assert_eq!(pick(None, mine), Pick::Default);
    }

    #[test]
    fn a_live_select_that_never_took_isnt_tried_again_by_itself() {
        let now = stamp("file:///home/x/steamvr/input/ours.json", 0, "{ ours }");
        assert!(try_live(None, &now), "never tried: go");
        assert!(!try_live(Some(&now), &now), "it failed for exactly this: no more binding loads");
        assert!(!try_live(Some(&format!("{now}\n")), &now), "a trailing newline is the same");
        assert!(try_live(Some(&stamp("file:///home/x/steamvr/input/ours.json", 1, "{ ours }")), &now), "Valve's manifest moved on: try again");
        assert!(try_live(Some(&stamp("file:///home/x/steamvr/input/ours.json", 0, "{ ours, changed }")), &now), "our binding changed: try again");
        assert!(try_live(Some(&stamp("file:///opt/steamvr/default.json", 0, "{ ours }")), &now), "a different target");
    }

    #[test]
    fn selecting_and_unselecting_touch_only_ours() {
        let mine = "file:///home/x/steamvr/input/openvr.component.vrcompositor_frame_controller.json";
        let mut s = json!({ "steamvr": { "enableHomeApp": false }, APP: { "knuckles_0_CurrentURL_steamvrinput": "file:///theirs.json" } });
        assert_eq!(select(&mut s, 0, mine), Pick::Default, "was free: now ours");
        assert_eq!(s[APP]["frame_controller_0_CurrentURL_steamvrinput"], mine);
        assert_eq!(select(&mut s, 0, mine), Pick::Ours, "already so");
        assert!(unselect(&mut s, mine));
        assert_eq!(s, json!({ "steamvr": { "enableHomeApp": false }, APP: { "knuckles_0_CurrentURL_steamvrinput": "file:///theirs.json" } }), "the rest as it was");
        let mut only = json!({});
        select(&mut only, 0, mine);
        unselect(&mut only, mine);
        assert_eq!(only, json!({}), "an empty section goes too");
        let mut user = json!({ APP: { "frame_controller_0_CurrentURL_steamvrinput": "file:///their_own.json" } });
        assert_eq!(select(&mut user, 0, mine), Pick::Theirs, "the user's own binding stays");
        assert_eq!(user[APP]["frame_controller_0_CurrentURL_steamvrinput"], "file:///their_own.json");
        assert!(!unselect(&mut user, mine), "and isn't ours to take out");
    }
}
