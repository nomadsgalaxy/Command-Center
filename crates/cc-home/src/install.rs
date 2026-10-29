//! `cc-home install` is the part of install.sh that comes after the bootstrap. The bootstrap
//! (the container, its packages and cc-home itself) stays in shell because there's no Rust
//! until it's done. Safe to re-run.
//!   cc-home install          2 · build: FreeRDP 3.31.1 against the container's FFmpeg
//!                            (panels/third_party/prefix), libvncclient for VNC panels
//!                            (panels/third_party/vnc-prefix), the workspace (cc-panels), the
//!                            mouse's SteamVR driver; 3 · commands: cc-panels, cc-home, cc-box
//!                            in ~/.local/bin, and KWin's grant for the window panels
//!   cc-home install desktop  a Command Center tile in the VR launcher, with its own Steam
//!                            library art (~/.local/share/applications/command-center.desktop,
//!                            from packaging/command-center.desktop.in), beside SteamOS's Desktop.
//!                            An older install overrode SteamOS's own Desktop entry; that
//!                            override is removed, so the stock Desktop is back
//!                            It also writes command-center.vrmanifest in the checkout and
//!                            registers it with SteamVR (one connection), so Command Center's
//!                            dashboard tab is there while the Desktop is closed (cc-panels' tab.rs).
//!                            With SteamVR off, the Desktop's next start registers it instead
//!                            And it makes the controller's shoulder button right click on
//!                            SteamVR's laser (binding.rs; settings.json "bumper_right_click": false
//!                            keeps it off)
//!   cc-home install remove   undoes both, takes the manifest out of SteamVR, puts the shoulder
//!                            button back to Valve's left click, and unregisters the
//!                            pointer driver; the checkout, the container and
//!                            ~/.config/control-center stay
use crate::home_dir;
use crate::session::{code, force_link, root};
use std::path::Path;
use std::process::{Command, Stdio};
use std::{fs, io};

fn step(s: &str) {
    println!("\n\x1b[1;36m// {s}\x1b[0m"); // Warp Cyan kicker
}

/// Runs it and fails unless it succeeds. `quiet` drops its stdout.
fn ok(c: &mut Command, quiet: bool) -> Result<(), String> {
    if quiet {
        c.stdout(Stdio::null());
    }
    match c.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{c:?}: exit {}", code(s))),
        Err(e) => Err(format!("{c:?}: {e}")),
    }
}

fn in_box(root: &Path, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(root.join("cc-box"));
    c.current_dir(dir).args(args); // distrobox enter keeps the working directory, so this carries into the box
    c
}

fn sycoca() {
    let _ = Command::new("kbuildsycoca6").stdout(Stdio::null()).stderr(Stdio::null()).status();
}

pub fn main(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("desktop") => desktop(),
        Some("remove") => remove(),
        None => build().and_then(|_| commands()),
        _ => Err("usage: cc-home install [desktop|remove]".into()),
    };
    r.map_or_else(
        |e| {
            eprintln!("{e}");
            1
        },
        |_| 0,
    )
}

/// The launcher tile's text: packaging/command-center.desktop.in with where the program, its icon and
/// its art are. The sysext image builds the same entry (packaging/sysext/build.sh) with its own paths.
pub fn launcher_entry(exec: &str, icon: &str, art: &str) -> String {
    include_str!("../../../packaging/command-center.desktop.in").replace("@EXEC@", exec).replace("@ICON@", icon).replace("@ART@", art)
}

/// command-center.vrmanifest's text: packaging/command-center.vrmanifest.in with what SteamVR runs
/// (session/cc-tab) and the tab's icon. The sysext image builds the same one (packaging/sysext/build.sh).
pub fn tab_manifest(tab: &str, icon: &str) -> String {
    let json = |s: &str| serde_json::Value::from(s).to_string();
    include_str!("../../../packaging/command-center.vrmanifest.in").replace("\"@TAB@\"", &json(tab)).replace("\"@ICON@\"", &json(icon))
}

/// SteamVR's list of app manifests (appconfig.json) without ours. None if it doesn't list it.
fn without_manifest(appconfig: &str, ours: &str) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(appconfig).ok()?;
    let list = v.get_mut("manifest_paths")?.as_array_mut()?;
    let n = list.len();
    list.retain(|p| p.as_str() != Some(ours));
    (list.len() != n).then(|| serde_json::to_string_pretty(&v).unwrap_or_default())
}

fn steamvr_running() -> bool {
    !crate::session::named(Path::new("/proc"), "vrserver").is_empty()
}

/// The tab's manifest in or out of SteamVR (cc-panels' `--register-tab`). With SteamVR off, a
/// registration waits for the Desktop's next start, and a removal edits appconfig.json, which
/// vrserver only rewrites while it runs.
fn tab_in_steamvr(on: bool) -> Result<(), String> {
    let (root, manifest) = (root(), root().join("command-center.vrmanifest"));
    if steamvr_running() {
        let flag = if on { "--register-tab" } else { "--unregister-tab" };
        return ok(Command::new(root.join("cc-box")).arg(root.join("target/release/cc-panels")).arg(flag), false);
    }
    if on {
        println!("SteamVR isn't running: the dashboard tab registers itself the next time you open the Desktop");
        return Ok(());
    }
    let conf = format!("{}/.config/openvr/config/appconfig.json", home_dir());
    if let Some(t) = fs::read_to_string(&conf).ok().and_then(|t| without_manifest(&t, &manifest.to_string_lossy())) {
        fs::write(&conf, t).map_err(|e| format!("{conf}: {e}"))?;
    }
    Ok(())
}

/// Whether a .desktop file's text is the entry an older `install desktop` wrote over SteamOS's own Desktop.
fn is_old_override(text: &str) -> bool {
    text.contains("Comment=Command Center") && text.contains("Name=Desktop")
}

fn desktop() -> Result<(), String> {
    let (root, home) = (root(), home_dir());
    let apps = format!("{home}/.local/share/applications");
    fs::create_dir_all(&apps).map_err(|e| format!("{apps}: {e}"))?;
    // Our own tile, with art, beside SteamOS's Desktop. The art stays in the checkout.
    let launch = root.join("session/cc-launch");
    let entry = launcher_entry(&launch.to_string_lossy(), &root.join("packaging/command-center.png").to_string_lossy(), &root.join("packaging/art").to_string_lossy());
    fs::write(format!("{apps}/command-center.desktop"), entry).map_err(|e| format!("{apps}: {e}"))?;
    // An older install replaced SteamOS's Desktop entry. Give it back.
    let old = format!("{apps}/deckard-nested-desktop.desktop");
    if fs::read_to_string(&old).is_ok_and(|t| is_old_override(&t)) {
        fs::remove_file(&old).map_err(|e| format!("{old}: {e}"))?;
        println!("removed the old override of SteamOS's Desktop entry, so the stock Desktop is back");
    }
    sycoca();
    println!("a Command Center tile for the VR launcher ({}), beside SteamOS's Desktop", launch.display());
    Ok(())
}

fn remove() -> Result<(), String> {
    let (root, home) = (root(), home_dir());
    // A checkout install registered its own copy; the sysext image has no such script. And if the
    // image's driver was registered because SteamVR runs from somewhere else (steamos.rs), that goes too.
    let script = root.join("driver/cc_pointer/install.sh");
    if script.is_file() {
        ok(Command::new(script).arg("uninstall"), false)?;
    }
    if crate::steamos::forget_driver(crate::session::unregister_driver) {
        println!("unregistered the image's pointer driver from vrpathreg");
    }
    // Only our own links, and the Desktop entry only if it's ours (another app may have put its own there).
    for c in ["cc-panels", "cc-home", "cc-box"] {
        let l = format!("{home}/.local/bin/{c}");
        if fs::read_link(&l).is_ok_and(|t| t.starts_with(&root)) {
            fs::remove_file(&l).map_err(|e| format!("{l}: {e}"))?;
        }
    }
    let apps = format!("{home}/.local/share/applications");
    let _ = fs::remove_file(format!("{apps}/org.controlcenter.panels.desktop"));
    let _ = fs::remove_file(format!("{apps}/command-center.desktop"));
    let entry = format!("{apps}/deckard-nested-desktop.desktop");
    if fs::read_to_string(&entry).is_ok_and(|t| is_old_override(&t)) {
        fs::remove_file(&entry).map_err(|e| format!("{entry}: {e}"))?;
    }
    sycoca();
    if let Err(e) = tab_in_steamvr(false) {
        eprintln!("the dashboard tab: {e}"); // the rest is removed either way
    }
    match crate::binding::restore() {
        Ok(say) => println!("{say}"),
        Err(e) => eprintln!("bumper right click: {e}"),
    }
    if !crate::session::native(&root) {
        let _ = fs::remove_file(root.join("command-center.vrmanifest")); // the image's is part of the image
    }
    println!("removed: the commands, the Command Center tile (SteamOS's Desktop is untouched), the dashboard tab and the pointer driver");
    println!("kept: {} (the code and its builds), the control-center container, and your settings in ~/.config/control-center", root.display());
    Ok(())
}

fn build() -> Result<(), String> {
    let root = root();
    step("build");
    let fr = root.join("panels/third_party");
    let src = fr.join("FreeRDP");
    if !src.is_dir() {
        ok(Command::new("git").args(["clone", "-q", "--depth", "1", "--branch", "3.31.1", "https://github.com/FreeRDP/FreeRDP.git"]).arg(&src), false)?;
    }
    // Makes H.264's colour conversion serial (docs/efficiency-plan.md 2a). 3.31.1 creates its YUV
    // context with flags 0, so it runs on WinPR's 8-thread pool no matter what
    // FreeRDP_ThreadingFlags says.
    // ponytail: every H.264 context, not per connection (h264_context_new would need the flags).
    let h264 = src.join("libfreerdp/codec/h264.c");
    let text = fs::read_to_string(&h264).map_err(|e| format!("{}: {e}", h264.display()))?;
    if text.contains("yuv_context_new(Compressor, 0)") {
        fs::write(&h264, text.replace("yuv_context_new(Compressor, 0)", "yuv_context_new(Compressor, THREADING_FLAGS_DISABLE_THREADS)")).map_err(|e| format!("{}: {e}", h264.display()))?;
    }
    // rdpsnd drops a chunk once more than its latency plus two chunks is queued, and with a latency
    // set, PulseAudio's buffer of that same size counts as queued, so it sat at the limit and any
    // 10 ms hiccup dropped 20 ms of sound (docs/audio.md). Twice the latency is room for both.
    let rdpsnd = src.join("channels/rdpsnd/client/rdpsnd_main.c");
    let text = fs::read_to_string(&rdpsnd).map_err(|e| format!("{}: {e}", rdpsnd.display()))?;
    if text.contains("maxDuration = duration * 2 + rdpsnd->latency;") {
        fs::write(&rdpsnd, text.replace("maxDuration = duration * 2 + rdpsnd->latency;", "maxDuration = duration * 2 + rdpsnd->latency * 2;")).map_err(|e| format!("{}: {e}", rdpsnd.display()))?;
    }
    // Rebuild when the source is newer than the library. The old library gets unlinked first
    // instead of overwritten, so a running cc-panels isn't using a file that changes under it.
    let lib = fr.join("prefix/lib64/libfreerdp3.so");
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    // with-pulse says the build has the PulseAudio backends (sound and microphone, docs/audio.md), so
    // a prefix made before that gets rebuilt once.
    let stamp = fr.join("prefix/with-pulse");
    let built = fs::canonicalize(&lib).ok().and_then(|l| mtime(&l));
    if !lib.is_file() || !stamp.is_file() || mtime(&h264) > built || mtime(&rdpsnd) > built {
        let prefix = format!("-DCMAKE_INSTALL_PREFIX={}", fr.join("prefix").display());
        let mut cmake = vec!["cmake", "-S", ".", "-B", "build", "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release", &prefix];
        cmake.extend(FREERDP_FLAGS);
        ok(&mut in_box(&root, &src, &cmake), true)?;
        ok(&mut in_box(&root, &src, &["ninja", "-C", "build"]), true)?;
        remove_lib(&lib).map_err(|e| format!("{}: {e}", lib.display()))?;
        ok(&mut in_box(&root, &src, &["ninja", "-C", "build", "install"]), true)?;
        fs::write(&stamp, "").map_err(|e| format!("{}: {e}", stamp.display()))?;
    }
    vnc(&root, &fr)?;
    ok(&mut in_box(&root, &root, &["cargo", "build", "--release", "-q"]), false)?;
    // The mouse's SteamVR laser. SteamVR picks it up on its next start.
    ok(&mut Command::new(root.join("driver/cc_pointer/build.sh")), false)?;
    ok(&mut Command::new(root.join("driver/cc_pointer/install.sh")), false)
}

/// libvncclient for VNC panels (vnc.rs), static, into panels/third_party/vnc-prefix.
/// tools/build-libvncclient.sh holds the pinned commit, our patch and the cmake options, so a
/// SteamOS-native build runs the same script.
fn vnc(root: &Path, fr: &Path) -> Result<(), String> {
    let prefix = fr.join("vnc-prefix");
    let script = root.join("tools/build-libvncclient.sh");
    ok(&mut in_box(root, root, &[&script.to_string_lossy(), &prefix.to_string_lossy()]), false)
}

/// Same as `rm -f libfreerdp3.so*`.
fn remove_lib(lib: &Path) -> io::Result<()> {
    let (dir, name) = (lib.parent().unwrap_or(Path::new(".")), lib.file_name().unwrap_or_default().to_string_lossy());
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&*name) {
            fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

const FREERDP_FLAGS: &[&str] = &[
    "-DWITH_FFMPEG=ON",
    "-DWITH_VIDEO_FFMPEG=ON",
    "-DWITH_DSP_FFMPEG=OFF",
    "-DWITH_SWSCALE=ON",
    "-DWITH_OPENH264=OFF",
    "-DWITH_CLIENT=ON",
    "-DWITH_CLIENT_SDL=OFF",
    "-DWITH_X11=OFF",
    "-DWITH_WAYLAND=OFF",
    "-DWITH_SERVER=OFF",
    "-DWITH_SHADOW=OFF",
    "-DWITH_PROXY=OFF",
    "-DWITH_SAMPLE=OFF",
    "-DWITH_MANPAGES=OFF",
    "-DWITH_CUPS=OFF",
    "-DWITH_PCSC=OFF",
    "-DWITH_PULSE=ON",
    "-DWITH_ALSA=OFF",
    "-DWITH_OSS=OFF",
    "-DWITH_FUSE=OFF",
    "-DWITH_KRB5=OFF",
    "-DWITH_SMARTCARD_EMULATE=OFF",
    "-DWITH_FDK_AAC=OFF",
    "-DWITH_LAME=OFF",
    "-DWITH_SOXR=OFF",
    "-DWITH_OPUS=OFF",
    "-DWITH_GSM=OFF",
    "-DWITH_FAAD2=OFF",
    "-DWITH_FAAC=OFF",
    "-DWITH_AAD=OFF",
    "-DWITH_WEBVIEW=OFF",
    "-DWITH_SDL_IMAGE_DIALOGS=OFF",
    "-DCHANNEL_URBDRC=OFF",
    "-DWITH_JSONC_REQUIRED=OFF",
    "-DBUILD_TESTING=OFF",
    "-DWITH_SIMD=ON",
];

fn commands() -> Result<(), String> {
    let (root, home) = (root(), home_dir());
    step("commands");
    let bin = format!("{home}/.local/bin");
    fs::create_dir_all(&bin).map_err(|e| format!("{bin}: {e}"))?;
    for c in ["cc-panels", "cc-home", "cc-box"] {
        force_link(root.join(c), Path::new(&format!("{bin}/{c}"))).map_err(|e| format!("{bin}/{c}: {e}"))?;
    }
    println!("installed: cc-panels, cc-home, cc-box in ~/.local/bin");
    // KWin only advertises its screencast and fake input to a client that a .desktop file grants
    // them to, matched by this desktop id through a security context, or else by Exec=. The Frame
    // windows' panels need both.
    let apps = format!("{home}/.local/share/applications");
    fs::create_dir_all(&apps).map_err(|e| format!("{apps}: {e}"))?;
    let grant = format!(
        "[Desktop Entry]\nType=Application\nName=Command Center panels\nExec={}\nNoDisplay=true\nX-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1,org_kde_kwin_fake_input\n",
        root.join("target/release/cc-panels").display()
    );
    fs::write(format!("{apps}/org.controlcenter.panels.desktop"), grant).map_err(|e| format!("{apps}: {e}"))?;
    sycoca();
    // The dashboard tab while the Desktop is closed (cc-panels' tab.rs).
    let manifest = root.join("command-center.vrmanifest");
    let text = tab_manifest(&root.join("session/cc-tab").to_string_lossy(), &root.join("packaging/command-center.png").to_string_lossy());
    fs::write(&manifest, text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    if let Err(e) = tab_in_steamvr(true) {
        eprintln!("the dashboard tab: {e}; the Desktop's next start tries again");
    }
    // The bumper as right click on SteamVR's laser (binding.rs).
    match crate::binding::apply() {
        Ok(say) => println!("{say}"),
        Err(e) => eprintln!("bumper right click: {e}"),
    }
    println!("a Nomads Galaxy project");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_manifest_is_a_dashboard_overlay_that_runs_cc_tab() {
        let m: serde_json::Value = serde_json::from_str(&tab_manifest("/x/session/cc-tab", "/x/a \"b\".png")).unwrap();
        let app = &m["applications"][0];
        assert_eq!(app["app_key"], "controlcenter.tab", "cc-panels' tab.rs APP_KEY");
        assert_eq!(app["is_dashboard_overlay"], true);
        assert_eq!(app["binary_path_linux"], "/x/session/cc-tab");
        assert_eq!(app["image_path"], "/x/a \"b\".png", "a path is a JSON string, quotes and all");
    }

    #[test]
    fn removing_the_tab_takes_only_its_manifest_out_of_appconfig() {
        let conf = r#"{"manifest_paths": ["/steam/steamapps.vrmanifest", "/cc/command-center.vrmanifest"]}"#;
        let out: serde_json::Value = serde_json::from_str(&without_manifest(conf, "/cc/command-center.vrmanifest").unwrap()).unwrap();
        assert_eq!(out["manifest_paths"], serde_json::json!(["/steam/steamapps.vrmanifest"]));
        assert_eq!(without_manifest(conf, "/other.vrmanifest"), None, "not ours: left alone");
        assert_eq!(without_manifest("not json", "/cc/command-center.vrmanifest"), None);
    }

    #[test]
    fn the_tile_has_a_name_what_to_run_and_valves_art_keys() {
        let t = launcher_entry("/x/session/cc-launch", "/x/packaging/command-center.png", "/x/packaging/art");
        let key = |k: &str| t.lines().find_map(|l| l.strip_prefix(&format!("{k}=")));
        assert_eq!(key("Name"), Some("Command Center"));
        assert_eq!(key("Exec"), Some("/x/session/cc-launch"));
        assert_eq!(key("Icon"), Some("/x/packaging/command-center.png"));
        assert_eq!(key("X-Steam-Library-Capsule"), Some("/x/packaging/art/capsule.png"));
        assert_eq!(key("X-Steam-Library-Hero"), Some("/x/packaging/art/hero.png"));
        assert_eq!(key("X-Steam-Library-Logo"), Some("/x/packaging/art/logo.png"));
        assert_eq!(key("X-Steam-Library-StoreCapsule"), Some("/x/packaging/art/store-capsule.png"));
        assert_eq!(key("X-Steam-Controller-Template"), Some("Desktop"));
        assert!(!t.contains('@'), "every placeholder filled: {t}");
        assert_eq!(key("X-Steam-Special"), None, "SteamOS's Desktop has it, and the launcher picks out one");
        assert!(t.starts_with("[Desktop Entry]\nType=Application\n"));
    }

    #[test]
    fn the_art_the_tile_names_is_in_the_repo_at_valves_sizes() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/art");
        for (f, w, h) in [("capsule.png", 600, 900), ("hero.png", 3840, 1240), ("logo.png", 1280, 720), ("store-capsule.png", 1280, 720)] {
            let b = fs::read(dir.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
            assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n", "{f} is a PNG");
            let dim = |at: usize| u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
            assert_eq!((dim(16), dim(20)), (w, h), "{f}");
        }
    }

    #[test]
    fn only_the_old_override_of_steamos_desktop_counts_as_ours_to_remove() {
        let old = "[Desktop Entry]\nType=Application\nName=Desktop\nComment=Command Center: your Plasma desktop and remote machines as VR panels\nExec=/x/session/cc-launch\nCategories=Utility;\n";
        assert!(is_old_override(old));
        assert!(!is_old_override("[Desktop Entry]\nName=Desktop\nExec=steamos-nested-desktop\n"), "Valve's own");
        assert!(!is_old_override("[Desktop Entry]\nName=Desktop\nComment=Someone else's desktop\n"), "another app's override");
        assert!(!is_old_override(&launcher_entry("a", "b", "c")), "our new tile isn't it");
    }
}
