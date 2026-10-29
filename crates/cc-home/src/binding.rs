//! The Frame controller's shoulder button (bumper) as right click on SteamVR's laser, so a
//! context menu is one press away (Planning/SteamVR Laser Binding.md, option 2). It's a user
//! binding for SteamVR's own laser app, `openvr.component.vrcompositor`: Valve's default for the
//! Frame controller with the two bumper clicks in `/actions/lasermouse` sending right click
//! instead of left. Everything else is Valve's, including LockMousePosition on the bumper, which
//! keeps the laser still while it's held.
//!
//! The binding is active while SteamVR runs. We write the user binding file any time
//! (`~/steamvr/input`), but the selection in steamvr.vrsettings only while SteamVR isn't running,
//! because vrserver rewrites that file on exit. While it runs, the selection waits, and the next
//! `cc-home install` or Desktop start with SteamVR closed makes it. No OpenVR connection and no
//! vrserver web API: that costs none of SteamVR's binding loads beyond the one it does at every
//! start anyway.
//!
//! It fails safe: a missing or broken file, a steamreset (which wipes ~/.config/openvr) or a new
//! version of SteamVR's laser manifest all put Valve's default back. settings.json
//! `"bumper_right_click": false` keeps it off; removing undoes it.
use crate::home_dir;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const APP: &str = "openvr.component.vrcompositor";
/// Valve's Frame controller binding for the laser, in the read-only image: what ours starts from.
const DEFAULT: &str = "/opt/steamvr/drivers/frame_controller/resources/input/vrcompositor_bindings_frame_controller.json";

/// Our binding file, where SteamVR keeps personal bindings.
fn ours() -> PathBuf {
    PathBuf::from(format!("{}/steamvr/input/{APP}_frame_controller.json", home_dir()))
}

/// SteamVR's settings. Spelled out from HOME: inside the Desktop, XDG_CONFIG_HOME points elsewhere.
fn vrsettings() -> PathBuf {
    PathBuf::from(format!("{}/.config/openvr/config/steamvr.vrsettings", home_dir()))
}

fn url(p: &Path) -> String {
    format!("file://{}", p.display())
}

/// The selection's key in the app's section: controller type, manifest version, binding kind.
/// The spelling is read off vrserver's strings (`%s_%s%s`, `CurrentURL`, `_steamvrinput`); nobody
/// has seen one written on the Frame yet.
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

/// Points the selection in `settings` at `url` for manifest `version`. True if it changed.
fn select(settings: &mut Value, version: u64, url: &str) -> bool {
    if !settings.is_object() {
        *settings = json!({});
    }
    let section = settings.as_object_mut().unwrap().entry(APP).or_insert_with(|| json!({}));
    if !section.is_object() {
        *section = json!({});
    }
    let k = key(version);
    if section[&k] == url {
        return false;
    }
    section[&k] = json!(url);
    true
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

/// Edits steamvr.vrsettings with f, if SteamVR isn't running. Some(true) if it wrote, Some(false)
/// if there was nothing to change, None if SteamVR runs (it'd overwrite the edit on exit).
fn edit_vrsettings(f: impl FnOnce(&mut Value) -> bool) -> Result<Option<bool>, String> {
    if steamvr_running() {
        return Ok(None);
    }
    let path = vrsettings();
    let mut v: Value = match fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}; left alone", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !f(&mut v) {
        return Ok(Some(false));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    replace(&path, &serde_json::to_string_pretty(&v).unwrap_or_default()).map(|_| Some(true))
}

fn wanted() -> bool {
    let s: Value = fs::read(crate::conf_dir().join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    s["bumper_right_click"] != false
}

/// Writes our binding file and selects it. Returns what happened, in a line for the user.
pub fn apply() -> Result<String, String> {
    if !wanted() {
        return Ok("the bumper stays left click (settings.json \"bumper_right_click\": false)".into());
    }
    let default: Value = fs::read(DEFAULT).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or(format!("{DEFAULT}: not there or not JSON, so the bumper stays as it is"))?;
    let ours_json = bumper_right_click(&default).ok_or("SteamVR's Frame controller binding has no bumper left click to change, so it stays as it is")?;
    let (path, version) = (ours(), default["action_manifest_version"].as_u64().unwrap_or(0));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&ours_json).unwrap_or_default();
    if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        replace(&path, &text)?;
    }
    Ok(match edit_vrsettings(|v| select(v, version, &url(&path)))? {
        Some(true) => "the bumper is right click on SteamVR's laser from SteamVR's next start".into(),
        Some(false) => "the bumper is right click on SteamVR's laser".into(),
        None => "the bumper right click applies next time SteamVR is closed".into(),
    })
}

/// At the Desktop's start: puts the binding back if it went (a steamreset, a deleted file), quietly
/// when there's nothing to do.
pub fn check() {
    if !wanted() {
        return;
    }
    match apply() {
        Ok(say) if say.contains("from SteamVR's next start") || !ours().is_file() => eprintln!("{say}"),
        Ok(_) => {}
        Err(e) => eprintln!("bumper right click: {e}"),
    }
}

/// Undoes it: our selection out of steamvr.vrsettings (or, while SteamVR runs, left for SteamVR to
/// drop: with our file gone it falls back to Valve's default by itself), and our file deleted.
pub fn restore() -> Result<String, String> {
    let path = ours();
    let done = edit_vrsettings(|v| unselect(v, &url(&path)))?;
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
    }
    Ok(match done {
        Some(_) => "the bumper is left click again".into(),
        None => "the bumper is left click again from SteamVR's next start (its own default, since our binding's gone)".into(),
    })
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

    #[test]
    fn selecting_and_unselecting_touch_only_ours() {
        let mine = "file:///home/x/steamvr/input/openvr.component.vrcompositor_frame_controller.json";
        let mut s = json!({ "steamvr": { "enableHomeApp": false }, APP: { "knuckles_0_CurrentURL_steamvrinput": "file:///theirs.json" } });
        assert!(select(&mut s, 0, mine));
        assert_eq!(s[APP]["frame_controller_0_CurrentURL_steamvrinput"], mine);
        assert!(!select(&mut s, 0, mine), "already so");
        assert!(unselect(&mut s, mine));
        assert_eq!(s, json!({ "steamvr": { "enableHomeApp": false }, APP: { "knuckles_0_CurrentURL_steamvrinput": "file:///theirs.json" } }), "the rest as it was");
        let mut only = json!({});
        select(&mut only, 0, mine);
        unselect(&mut only, mine);
        assert_eq!(only, json!({}), "an empty section goes too");
        let mut user = json!({ APP: { "frame_controller_0_CurrentURL_steamvrinput": "file:///their_own.json" } });
        assert!(!unselect(&mut user, mine), "a binding the user picked isn't ours to take out");
    }
}
