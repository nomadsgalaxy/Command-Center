//! Lists the extensions each OpenXR runtime on this machine advertises (docs/openxr-plan.md, section 1).
//!
//!   xr-probe [--loader PATH] [MANIFEST...]
//!
//! With no manifests it finds them: the active runtime, every manifest in the OpenXR config dirs,
//! and SteamVR's for this CPU. Each runtime is asked in its own process, with XR_RUNTIME_JSON
//! pointing at it, because the loader can remember the first runtime it found.
//!
//! It only calls xrEnumerateInstanceExtensionProperties. It never creates an instance or a session,
//! so it doesn't connect an app to SteamVR or spend its binding loads. Whether a runtime can create
//! an overlay session, composite beside a game or give it input needs an instance, so those are
//! printed as "not tested".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use openxr::sys;

/// The ones the plan turns on. Anything else the runtime advertises is listed after them.
const WATCH: &[&str] = &[
    "XR_EXTX_overlay",
    "XR_KHR_composition_layer_cylinder",
    "XR_KHR_composition_layer_color_scale_bias",
    "XR_KHR_vulkan_enable2",
    "XR_EXT_eye_gaze_interaction",
    "XR_EXT_user_presence",
    "XR_VALVE_frame_controller_interaction",
];

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let loader = match args.iter().position(|a| a == "--loader") {
        Some(i) if i + 1 < args.len() => {
            let l = args.remove(i + 1);
            args.remove(i);
            Some(PathBuf::from(l))
        }
        Some(_) => fail("--loader needs a path"),
        None => None,
    };
    if args.first().map(String::as_str) == Some("--one") {
        // The child: the parent has already pointed XR_RUNTIME_JSON at the runtime.
        one(loader.as_deref());
        return;
    }
    let manifests: Vec<PathBuf> = if args.is_empty() { found() } else { args.iter().map(PathBuf::from).collect() };
    if manifests.is_empty() {
        println!("no OpenXR runtime manifests found");
    }
    for m in &manifests {
        report(m, loader.as_deref());
    }
}

fn fail(why: &str) -> ! {
    eprintln!("xr-probe: {why}");
    std::process::exit(2)
}

/// Where the loader looks (the OpenXR loader spec, Linux), then SteamVR's own manifest for this CPU.
fn found() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    let mut dirs = vec![PathBuf::from(config)];
    let xdg_dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    dirs.extend(xdg_dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.push("/etc".into());
    let data = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.push(PathBuf::from(data));
    dirs.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));

    let mut out = Vec::new();
    for d in dirs {
        let Ok(list) = std::fs::read_dir(d.join("openxr/1")) else { continue };
        let mut here: Vec<PathBuf> = list.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        here.sort();
        out.extend(here);
    }
    let steamvr = match std::env::consts::ARCH {
        "aarch64" => "steamxr_linuxarm64.json",
        "x86_64" => "steamxr_linux64.json",
        _ => "",
    };
    if !steamvr.is_empty() {
        out.push(Path::new("/opt/steamvr").join(steamvr));
        out.push(Path::new(&home).join(".steam/steam/steamapps/common/SteamVR").join(steamvr));
    }
    // The active_runtime.json is usually a link to one of the others: ask each runtime once.
    let mut seen = BTreeSet::new();
    out.into_iter().filter(|p| p.is_file()).filter(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone()))).collect()
}

fn report(manifest: &Path, loader: Option<&Path>) {
    println!("== {}", manifest.display());
    let real = manifest.canonicalize().unwrap_or_else(|_| manifest.to_path_buf());
    if real != manifest {
        println!("   links to {}", real.display());
    }
    match std::fs::read_to_string(manifest).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
        Some(j) => {
            let rt = &j["runtime"];
            println!("   name     {}", rt["name"].as_str().unwrap_or("(none given)"));
            println!("   library  {}", rt["library_path"].as_str().unwrap_or("(none given)"));
            println!("   format   {}", j["file_format_version"].as_str().unwrap_or("?"));
        }
        None => println!("   (not a manifest the probe could read)"),
    }
    println!("   version  not tested (needs an instance)");

    let me = std::env::current_exe().unwrap_or_else(|e| fail(&format!("can't find myself: {e}")));
    let mut child = Command::new(me);
    child.env("XR_RUNTIME_JSON", manifest).arg("--one");
    if let Some(l) = loader {
        child.arg("--loader").arg(l);
    }
    let out = match child.output() {
        Ok(o) => o,
        Err(e) => return println!("   couldn't run the probe: {e}"),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut exts: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        if let Some(l) = line.strip_prefix("loader ") {
            println!("   loader   {l}");
        } else if let Some(e) = line.strip_prefix("ext ") {
            exts.push(e.split_once(' ').unwrap_or((e, "?")));
        }
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return println!("   failed: {}", err.trim());
    }
    println!("   advertised {} extensions", exts.len());
    for w in WATCH {
        let got = exts.iter().find(|(n, _)| n == w);
        println!("   {:44} {}", w, got.map_or("no".to_string(), |(_, v)| format!("yes (v{v})")));
    }
    println!("   overlay session, beside a game, usable input: not tested (needs an instance)");
    let rest: Vec<String> = exts.iter().filter(|(n, _)| !WATCH.contains(n)).map(|(n, v)| format!("{n} v{v}")).collect();
    println!("   others: {}", if rest.is_empty() { "none".into() } else { rest.join(", ") });
}

/// In the child: load the loader and print one "ext NAME VERSION" line for each extension.
fn one(loader: Option<&Path>) {
    let mut tried: Vec<PathBuf> = loader.map(Path::to_path_buf).into_iter().collect();
    if loader.is_none() {
        tried.extend(["libopenxr_loader.so.1", "libopenxr_loader.so"].map(PathBuf::from));
        let arch = if std::env::consts::ARCH == "aarch64" { "linuxarm64" } else { "linux64" };
        tried.push(Path::new("/opt/steamvr/bin").join(arch).join("libopenxr_loader.so"));
    }
    let mut last = String::new();
    for path in &tried {
        // SAFETY: loading a library runs its constructors. These are OpenXR loaders, which is
        // what the openxr crate expects to load here.
        match unsafe { openxr::Entry::load_from(path, &()) } {
            Ok(entry) => {
                println!("loader {}", path.display());
                return list(&entry);
            }
            Err(e) => last = format!("{}: {e}", path.display()),
        }
    }
    fail(&format!("no OpenXR loader would load (last: {last})"))
}

fn list(entry: &openxr::Entry) {
    let f = entry.fp().enumerate_instance_extension_properties;
    let mut n = 0u32;
    // SAFETY: the two-call idiom. A null list with capacity 0 asks only for the count.
    let r = unsafe { f(std::ptr::null(), 0, &mut n, std::ptr::null_mut()) };
    if r.into_raw() < 0 {
        fail(&format!("xrEnumerateInstanceExtensionProperties: {r:?}"));
    }
    let blank = sys::ExtensionProperties { ty: sys::ExtensionProperties::TYPE, next: std::ptr::null_mut(), extension_name: [0; sys::MAX_EXTENSION_NAME_SIZE], extension_version: 0 };
    let mut props = vec![blank; n as usize];
    // SAFETY: props holds n entries, each with its type set, as the call requires.
    let r = unsafe { f(std::ptr::null(), n, &mut n, props.as_mut_ptr()) };
    if r.into_raw() < 0 {
        fail(&format!("xrEnumerateInstanceExtensionProperties: {r:?}"));
    }
    for p in &props[..n as usize] {
        // SAFETY: the runtime writes a NUL-terminated name inside the fixed array.
        let name = unsafe { std::ffi::CStr::from_ptr(p.extension_name.as_ptr()) };
        println!("ext {} {}", name.to_string_lossy(), p.extension_version);
    }
}
