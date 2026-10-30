//! The OpenVR boundary's ratchet (docs/openxr-plan.md, phase 0 step 6). It reads the sources and
//! fails if any file outside the OpenVR side uses an OpenVR name that isn't on its list below:
//! a `call!` into one of OpenVR's tables, an `openvr_sys` item, or a `vr::` item (vr::wake and
//! vr::timed aren't OpenVR, so they don't count). It also fails if a listed name is gone, so the
//! list only ever shrinks: each move (6b to 6e) takes its group off as it lands, and once the list
//! is empty nothing outside the OpenVR side touches OpenVR.
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

const SURFACES: &str = "Surfaces and overlay calls not yet behind a Live (6b)";
const STAGE: &str = "Stage: poses, roles and OpenVR's device numbering, kept for logs and R-2 (6c)";
const SESSION: &str = "Session: the display's rate, the universe, the scene app, start and stop (6d)";
const FAKES: &str = "tests: SteamVR's events and roles, built for vr::fake";

/// Each file's OpenVR names today, in groups by the step that takes them away.
const ALLOWED: &[(&str, &str, &[&str])] = &[
    (
        "back.rs",
        SURFACES,
        &[
            "GetOverlaySortOrder",
            "GetOverlayTexelAspect",
            "SetOverlayAlpha",
            "SetOverlayColor",
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlaySortOrder",
            "SetOverlayTextureBounds",
            "ShowOverlay",
            "sys::VROverlayFlags_IgnoreTextureAlpha",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::VROverlayInputMethod_Mouse",
            "sys::VRTextureBounds_t",
        ],
    ),
    ("control.rs", SURFACES, &["HideOverlay", "ShowOverlay"]),
    ("control.rs", STAGE, &["vr::devices", "vr::head", "vr::tip"]),
    (
        "control/dashtab.rs",
        SURFACES,
        &[
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlayWidthInMeters",
            "sys::VROverlayFlags_EnableControlBar",
            "sys::VROverlayFlags_EnableControlBarClose",
            "sys::VROverlayInputMethod_Mouse",
            "vr::set_from_file",
        ],
    ),
    (
        "control/hud.rs",
        SURFACES,
        &[
            "SetOverlayAlpha",
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlayMouseScale",
            "SetOverlaySortOrder",
            "ShowOverlay",
            "sys::HmdMatrix34_t",
            "sys::HmdVector2_t",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::VROverlayInputMethod_Mouse",
        ],
    ),
    (
        "control/machines.rs",
        SURFACES,
        &["HideOverlay", "SetOverlayFlag", "SetOverlayInputMethod", "SetOverlaySortOrder", "ShowOverlay", "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible", "sys::VROverlayInputMethod_Mouse"],
    ),
    ("control/machines.rs", STAGE, &["vr::index", "vr::is_real_controller"]),
    (
        "control/prefs.rs",
        SURFACES,
        &["HideOverlay", "SetOverlayFlag", "SetOverlayInputMethod", "SetOverlaySortOrder", "ShowOverlay", "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible", "sys::VROverlayInputMethod_Mouse"],
    ),
    ("control/prefs.rs", STAGE, &["vr::index", "vr::is_real_controller"]),
    ("control/ui.rs", SURFACES, &["GetOverlayMouseScale", "SetOverlayMouseScale", "sys::HmdVector2_t"]),
    ("control/ui.rs", STAGE, &["vr::head"]),
    (
        "control/workspace.rs",
        SURFACES,
        &["HideOverlay", "SetOverlayFlag", "SetOverlayInputMethod", "SetOverlaySortOrder", "ShowOverlay", "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible", "sys::VROverlayInputMethod_Mouse"],
    ),
    ("control/workspace.rs", STAGE, &["vr::head", "vr::index", "vr::is_real_controller"]),
    ("gather.rs", STAGE, &["vr::head"]),
    ("gpu.rs", "the handle type its buffers keep, Textures' TexHandle in all but name (6b)", &["sys::SharedTextureHandle_t"]),
    (
        "grab.rs",
        SURFACES,
        &[
            "HideOverlay",
            "SetOverlayAlpha",
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlaySortOrder",
            "ShowOverlay",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::VROverlayFlags_SendVRDiscreteScrollEvents",
            "sys::VROverlayInputMethod_Mouse",
        ],
    ),
    (
        "grab.rs",
        STAGE,
        &[
            "GetDeviceToAbsoluteTrackingPose",
            "sys::ETrackingUniverseOrigin_TrackingUniverseStanding",
            "sys::TrackedDevicePose_t",
            "sys::k_unMaxTrackedDeviceCount",
            "vr::head_position",
            "vr::index",
            "vr::is_real_controller",
            "vr::laser_pose",
            "vr::to_photons",
        ],
    ),
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
        SURFACES,
        &[
            "HideOverlay",
            "IsOverlayVisible",
            "SetOverlayColor",
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlayMouseScale",
            "SetOverlayName",
            "SetOverlaySortOrder",
            "ShowOverlay",
            "sys::HmdVector2_t",
            "sys::VROverlayFlags_IgnoreTextureAlpha",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::VROverlayFlags_SendVRDiscreteScrollEvents",
            "sys::VROverlayInputMethod_Mouse",
            "vr::set_from_file",
        ],
    ),
    ("main.rs", STAGE, &["vr::head", "vr::head_position", "vr::head_worn"]),
    (
        "main.rs",
        SESSION,
        &[
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
    (
        "osk.rs",
        SURFACES,
        &[
            "HideOverlay",
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlaySortOrder",
            "SetOverlayWidthInMeters",
            "ShowOverlay",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::VROverlayInputMethod_Mouse",
        ],
    ),
    ("osk.rs", STAGE, &["vr::place"]),
    ("osk.rs", "tests: SteamVR's keyboard bytes, decoded as vr.rs does", &["vr::typed"]),
    (
        "plasmabar.rs",
        SURFACES,
        &[
            "HideOverlay",
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlayMouseScale",
            "SetOverlaySortOrder",
            "ShowOverlay",
            "sys::HmdVector2_t",
            "sys::VROverlayFlags_IgnoreTextureAlpha",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::VROverlayFlags_SendVRDiscreteScrollEvents",
            "sys::VROverlayInputMethod_Mouse",
            "sys::VROverlayInputMethod_None",
        ],
    ),
    ("plasmabar.rs", STAGE, &["vr::index", "vr::is_real_controller", "vr::laser_pose", "vr::openvr_type"]),
    (
        "plasmabar.rs",
        "Plasma's raw loop: a non-pointer event still moves its last point (events.rs, 5a's two readers)",
        &["sys::EVREventType_VREvent_MouseMove", "trace::poll", "vr::VREvent_t", "vr::dev", "vr::surface_event"],
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
    ("popout.rs", STAGE, &["vr::head_position"]),
    ("surface.rs", "Surfaces::create and destroy, which move into the Live (6b)", &["DestroyOverlay", "vr::create_dashboard_overlay", "vr::create_overlay"]),
    (
        "taskbar.rs",
        SURFACES,
        &[
            "HideOverlay",
            "SetOverlayAlpha",
            "SetOverlayFlag",
            "SetOverlayInputMethod",
            "SetOverlayMouseScale",
            "SetOverlaySortOrder",
            "ShowOverlay",
            "sys::HmdVector2_t",
            "sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible",
            "sys::VROverlayFlags_SendVRDiscreteScrollEvents",
            "sys::VROverlayInputMethod_Mouse",
        ],
    ),
    (
        "taskbar.rs",
        STAGE,
        &[
            "GetTrackedDeviceIndexForControllerRole",
            "sys::ETrackedControllerRole_TrackedControllerRole_LeftHand",
            "sys::ETrackedControllerRole_TrackedControllerRole_RightHand",
            "sys::k_unTrackedDeviceIndexInvalid",
            "vr::dev",
            "vr::head",
            "vr::head_position",
            "vr::index",
            "vr::is_real_controller",
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
    ("windows.rs", SURFACES, &["HideOverlay", "ShowOverlay"]),
    ("windows.rs", STAGE, &["vr::head", "vr::head_position"]),
];

/// The OpenVR names in a file's code (comments left out): what it `call!`s, its `openvr_sys`
/// items (bare `sys::` only where `openvr_sys` is `sys`, since spa and pipewire have one too), its
/// `vr::` items but the neutral ones, and the trace's raw poll.
fn names(text: &str) -> BTreeSet<String> {
    let ident = |s: &str| s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>();
    // each place `pat` starts a path of its own (not the end of a longer one, like spa::sys::)
    let starts = |code: &str, pat: &str| -> Vec<usize> {
        code.match_indices(pat).map(|(i, _)| i).filter(|&i| !code[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == ':') || code[..i].ends_with("crate::")).collect()
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
        // a grouped import hides its names from the scan, so it's a name of its own that no list has
        for pat in ["vr::{", "openvr_sys::{"] {
            if !starts(code, pat).is_empty() {
                out.insert(pat.into());
            }
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
            if n.ends_with('{') {
                bad.push(format!("{f}: imports {n}..}}: import OpenVR names one at a time, so this test sees each"));
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
    let grouped = BTreeMap::from([("c.rs".to_string(), names("use crate::vr::{head, index};"))]);
    assert!(check(&grouped, &[("c.rs", "why", &["vr::head", "vr::index"])]).iter().any(|l| l.contains("one at a time")), "and fails even when its names are listed");
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
