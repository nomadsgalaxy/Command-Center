//! The OpenVR boundary's ratchet (docs/openxr-plan.md, phase 0 step 6). It reads the sources and
//! fails if any file outside the OpenVR side uses an OpenVR name that isn't on its list below:
//! a `call!` into one of OpenVR's tables, an `openvr_sys` item, or a `vr::` item (vr::wake and
//! vr::timed aren't OpenVR, so they don't count). It also fails if a listed name is gone, so the
//! list only ever shrinks: each move (6b to 6e) takes its group off as it lands, and once the list
//! is empty nothing outside the OpenVR side touches OpenVR.
//!
//! What it doesn't do: it's an interim ratchet, not proof the boundary is done.
//! - It counts names, not call sites, so another use of a name a file is already allowed passes.
//! - The groups for tests (`FAKES`), main.rs's old_loop and Plasma's raw loop are file-wide
//!   allowances, not spans it enforces, so a production use of a name allowed there for a test
//!   or for that loop passes too.
//! - It reads text, not Rust (see `names`).
#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};

/// The OpenVR side: its `Live`s and what they call. Not scanned.
const SIDE: &[&str] = &["vr.rs", "overlay.rs", "stage.rs", "texture.rs", "keyboard.rs", "shell.rs", "gaze.rs", "poll.rs", "trace.rs"];

/// Whole files that stay on OpenVR, and why. Not scanned.
const WHOLE: &[(&str, &str)] = &[
    ("tab.rs", "the dashboard tab's own program: SteamVR's app manifest and autostart, before any backend runs"),
    ("bin/cc-win-spike.rs", "the window spike, a program of its own with its own queue; it moves onto the backend's path later"),
    ("boundary.rs", "this file: its lists are nothing but the names"),
];

/// `vr::` items that aren't OpenVR: waking the main loop and timing a call.
const NEUTRAL: &[&str] = &["wake", "timed", "WOKEN", "MAIN", "Handle", "fake"];

const SESSION: &str = "Session: the display's rate, the universe, the scene app, whether it's worn at start, start and stop (6d)";
const FAKES: &str = "tests: SteamVR's events and roles, built for vr::fake";

/// Each file's OpenVR names today, in groups by the step that takes them away.
const ALLOWED: &[(&str, &str, &[&str])] = &[
    ("control.rs", "the control socket's SteamVR inspection (its devices, roles and laser tips), OpenVR on purpose", &["vr::devices", "vr::tip"]),
    ("grab.rs", "tests: the golden's \"by {index}\" line and the roles vr::fake answers, by OpenVR's numbers", &["vr::index"]),
    ("grab.rs", FAKES, &["sys::ETrackedControllerRole_TrackedControllerRole_LeftHand", "sys::ETrackedControllerRole_TrackedControllerRole_RightHand"]),
    (
        "laser.rs",
        "the cc_pointer lease and SteamVR's laser mode, moving to the OpenVR side whole (6e)",
        &[
            "GetPrimaryDashboardDevice",
            "GetStringTrackedDeviceProperty",
            "GetTrackedDeviceIndexForControllerRole",
            "HideOverlay",
            "SetOverlayFlag",
            "SetOverlayWidthInMeters",
            "ShowOverlay",
            "sys::ETrackedControllerRole_TrackedControllerRole_LeftHand",
            "sys::ETrackedControllerRole_TrackedControllerRole_RightHand",
            "sys::ETrackedDeviceProperty_Prop_ControllerType_String",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::k_unMaxTrackedDeviceCount",
            "sys::k_unTrackedDeviceIndexInvalid",
            "vr::head",
            "vr::head_raw",
            "vr::place",
        ],
    ),
    (
        "main.rs",
        SESSION,
        &[
            "vr::head_worn",
            "GetFloatTrackedDeviceProperty",
            "GetUint64TrackedDeviceProperty",
            "sys::ETrackedDeviceProperty_Prop_CurrentUniverseId_Uint64",
            "sys::ETrackedDeviceProperty_Prop_DisplayFrequency_Float",
            "vr::init",
            "vr::shutdown",
        ],
    ),
    (
        "main.rs",
        "old_loop, golden_steamvr_events' copy of the loop from before phase 0, and its events (test only)",
        &[
            "PollNextEvent",
            "sys::EVREventType_VREvent_ButtonPress",
            "sys::EVREventType_VREvent_DashboardActivated",
            "sys::EVREventType_VREvent_DashboardDeactivated",
            "sys::EVREventType_VREvent_Quit",
            "sys::EVREventType_VREvent_StandingZeroPoseReset",
            "sys::EVREventType_VREvent_TrackedDeviceActivated",
            "sys::EVREventType_VREvent_TrackedDeviceUserInteractionEnded",
            "sys::EVREventType_VREvent_TrackedDeviceUserInteractionStarted",
            "vr::VREvent_t",
            "vr::is_real_controller",
        ],
    ),
    ("osk.rs", "tests: SteamVR's keyboard bytes, decoded as vr.rs does", &["vr::typed"]),
    ("plasmabar.rs", "plasma-debug's line, by SteamVR's own event type: a diagnostic", &["vr::openvr_type"]),
    (
        "plasmabar.rs",
        "Plasma's raw loop: a non-pointer event still moves its last point (events.rs, 5a's two readers)",
        &["sys::EVREventType_VREvent_MouseMove", "trace::poll", "vr::VREvent_t", "vr::dev", "vr::laser_pose", "vr::surface_event"],
    ),
    (
        "plasmabar.rs",
        FAKES,
        &[
            "sys::EVREventType_VREvent_FocusEnter",
            "sys::EVREventType_VREvent_FocusLeave",
            "sys::EVREventType_VREvent_MouseButtonDown",
            "sys::EVREventType_VREvent_MouseButtonUp",
            "sys::EVRMouseButton_VRMouseButton_Left",
            "sys::VREvent_Mouse_t",
        ],
    ),
    ("taskbar.rs", SESSION, &["GetCurrentSceneProcessId"]),
    (
        "taskbar.rs",
        FAKES,
        &[
            "sys::EVREventType_VREvent_MouseButtonDown",
            "sys::EVREventType_VREvent_MouseButtonUp",
            "sys::EVREventType_VREvent_MouseMove",
            "sys::EVRMouseButton_VRMouseButton_Left",
            "sys::VREvent_Mouse_t",
            "vr::VREvent_t",
        ],
    ),
];

/// The OpenVR names in a file's code (comments left out): what it `call!`s, its `openvr_sys`
/// items (bare `sys::` only where `openvr_sys` is `sys`, since spa and pipewire have one too), its
/// `vr::` items but the neutral ones, and the trace's raw poll.
fn names(text: &str) -> BTreeSet<String> {
    let ident = |s: &str| s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>();
    // each place `pat` starts a path of its own, or follows crate::, super:: or self:: (chains too),
    // not the end of a longer one like spa::sys::
    let starts = |code: &str, pat: &str| -> Vec<usize> {
        let ours = |before: &str| ["crate::", "super::", "self::"].iter().any(|p| before.ends_with(p) && !before[..before.len() - p.len()].ends_with(|c: char| c.is_alphanumeric() || c == '_'));
        code.match_indices(pat).map(|(i, _)| i).filter(|&i| !code[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == ':') || ours(&code[..i])).collect()
    };
    let after = |code: &str, pat: &str| -> Vec<String> { starts(code, pat).into_iter().map(|i| ident(&code[i + pat.len()..])).filter(|n| !n.is_empty()).collect() };
    let openvr_is_sys = text.contains("openvr_sys as sys");
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("");
        for (i, _) in code.match_indices("call!(") {
            // call!(table, Name, ...)
            if let Some((_, rest)) = code[i + 6..].split_once(',') {
                let n = ident(rest.trim_start());
                if !n.is_empty() {
                    out.insert(n);
                }
            }
        }
        out.extend(after(code, "openvr_sys::").into_iter().map(|n| format!("sys::{n}")));
        if openvr_is_sys {
            out.extend(after(code, "sys::").into_iter().map(|n| format!("sys::{n}")));
        }
        out.extend(after(code, "vr::").into_iter().filter(|n| !NEUTRAL.contains(&n.as_str())).map(|n| format!("vr::{n}")));
        if code.contains("trace::poll(") {
            out.insert("trace::poll".into());
        }
        // a grouped import, a glob or an alias hides its names from the scan, so each is a name of
        // its own that no list has
        for pat in ["vr::{", "openvr_sys::{", "vr::*", "openvr_sys::*", "vr as ", "call as "] {
            if !starts(code, pat).is_empty() {
                out.insert(pat.trim_end().into());
            }
        }
        if after(code, "openvr_sys as ").iter().any(|a| a != "sys") {
            out.insert("openvr_sys as".into());
        }
    }
    out
}

/// What's wrong with `found` (each file's names) against `allowed`: a name a file isn't allowed,
/// and a listed name it no longer uses.
fn check(found: &BTreeMap<String, BTreeSet<String>>, allowed: &[(&str, &str, &[&str])]) -> Vec<String> {
    let mut want: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (f, _, ns) in allowed {
        want.entry(f).or_default().extend(ns.iter());
    }
    let mut bad = Vec::new();
    for (f, ns) in found {
        let ok = want.get(f.as_str());
        for n in ns.iter().filter(|n| !ok.is_some_and(|ok| ok.contains(n.as_str()))) {
            if n.ends_with('{') || n.ends_with('*') || n.ends_with(" as") {
                bad.push(format!(
                    "{f}: imports `{n}`: import OpenVR names one at a time and by their own names, so this test sees each (a local named vr or call cast with `as` trips it too, on purpose: rename the local)"
                ));
                continue;
            }
            bad.push(format!("{f}: uses {n}, which isn't on its list: go through the OpenVR side's Live, or list it with why"));
        }
    }
    for (f, ns) in &want {
        let has = found.get(*f);
        for n in ns.iter().filter(|n| !has.is_some_and(|has| has.contains(**n))) {
            bad.push(format!("{f}: no longer uses {n}: take it off its list"));
        }
    }
    bad
}

/// Every .rs file under src, by its path from there, that isn't on the OpenVR side or a whole exception.
fn scanned() -> BTreeMap<String, BTreeSet<String>> {
    fn visit(root: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<String, BTreeSet<String>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                visit(root, &p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                if SIDE.contains(&rel.as_str()) || WHOLE.iter().any(|(w, _)| *w == rel) {
                    continue;
                }
                let n = names(&std::fs::read_to_string(&p).unwrap());
                if !n.is_empty() {
                    out.insert(rel, n);
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = BTreeMap::new();
    visit(&root, &root, &mut out);
    out
}

#[test]
fn nothing_outside_the_openvr_side_takes_up_an_openvr_name() {
    let bad = check(&scanned(), ALLOWED);
    assert!(bad.is_empty(), "the OpenVR boundary (boundary.rs):\n{}", bad.join("\n"));
}

#[test]
fn every_exception_is_a_file_that_exists() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for f in SIDE.iter().chain(WHOLE.iter().map(|(f, _)| f)).chain(ALLOWED.iter().map(|(f, _, _)| f)) {
        assert!(root.join(f).is_file(), "{f} is listed in boundary.rs but isn't in src");
    }
}

#[test]
fn it_finds_each_kind_of_name_and_leaves_the_rest() {
    let text = "use openvr_sys as sys;\n\
                call!(ov, ShowOverlay, h.0); crate::call!(sys, PollNextEvent, &mut e, n);\n\
                let f = sys::VROverlayFlags_IgnoreTextureAlpha | openvr_sys::k_unMaxTrackedDeviceCount;\n\
                let x = spa::sys::SPA_DATA_DmaBuf; let y = pw::sys::pw_buffer; let z = freerdp_sys::PTR_FLAGS_MOVE;\n\
                vr::head(); crate::vr::index(d); vr::wake(); vr::timed(|| s, f); let h: vr::Handle;\n\
                while crate::trace::poll(\"plasma\", h, &mut e) {}\n\
                // call!(ov, HideOverlay, h.0) in a comment\n";
    let got: Vec<String> = names(text).into_iter().collect();
    assert_eq!(got, ["PollNextEvent", "ShowOverlay", "sys::VROverlayFlags_IgnoreTextureAlpha", "sys::k_unMaxTrackedDeviceCount", "trace::poll", "vr::head", "vr::index"]);
    assert!(names("let x = sys::Thing;").is_empty(), "a bare sys:: only counts where openvr_sys is sys");
    assert!(names("use crate::vr::{head, index};").contains("vr::{"), "a grouped import is caught");
    assert!(names("use openvr_sys::{self as sys};").contains("openvr_sys::{"));
    assert!(!names("use crate::openvr::{x};").contains("vr::{"), "only a vr path of its own");
    assert!(
        names("super::vr::head(); super::super::vr::index(d); self::vr::tip(h);").is_superset(&["vr::head", "vr::index", "vr::tip"].map(String::from).into()),
        "super:: and self:: paths, chains too"
    );
    assert!(!names("my_super::vr::head();").contains("vr::head"), "but not a longer name ending in super::");
    assert!(names("use crate::vr::*;").contains("vr::*") && names("use openvr_sys::*;").contains("openvr_sys::*"), "globs");
    assert!(names("use openvr_sys as ovr;").contains("openvr_sys as") && !names("use openvr_sys as sys;").contains("openvr_sys as"), "an alias but sys");
    assert!(names("use crate::vr as v;").contains("vr as"), "an alias of vr");
    assert!(names("use crate::call as c;").contains("call as") && !names("let n = recall as u32;").contains("call as"), "an alias of call!, not a longer name");
    let grouped = BTreeMap::from([("c.rs".to_string(), names("use crate::vr::{head, index};"))]);
    assert!(check(&grouped, &[("c.rs", "why", &["vr::head", "vr::index"])]).iter().any(|l| l.contains("one at a time")), "and fails even when its names are listed");
    for hidden in ["use crate::vr::*;", "use openvr_sys as ovr;", "use crate::vr as v;", "use crate::call as c;"] {
        let found = BTreeMap::from([("c.rs".to_string(), names(hidden))]);
        assert!(check(&found, &[]).iter().any(|l| l.contains("one at a time")), "{hidden}");
    }
}

#[test]
fn a_new_name_fails_and_a_dropped_one_has_to_come_off() {
    let found = |ns: &[&str]| BTreeMap::from([("a.rs".to_string(), ns.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>())]);
    let allowed: &[(&str, &str, &[&str])] = &[("a.rs", "why", &["ShowOverlay"])];
    assert!(check(&found(&["ShowOverlay"]), allowed).is_empty());
    assert_eq!(check(&found(&["ShowOverlay", "HideOverlay"]), allowed).len(), 1, "a name the file added");
    assert_eq!(check(&found(&[]), allowed).len(), 1, "a name it dropped stays listed");
    let new_file = BTreeMap::from([("b.rs".to_string(), BTreeSet::from(["vr::head".to_string()]))]);
    assert!(check(&new_file, &[])[0].starts_with("b.rs: uses vr::head"), "a file that isn't listed at all");
}
