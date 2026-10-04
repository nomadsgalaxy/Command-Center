//! `cc-home install` is the part of install.sh that comes after the bootstrap. The bootstrap
//! (the container, its packages and cc-home itself) stays in shell because there's no Rust
//! until it's done. Safe to re-run.
//!   cc-home install          2 · build: FreeRDP 3.31.1 against the container's FFmpeg
//!                            (panels/third_party/prefix), libvncclient for VNC panels
//!                            (panels/third_party/vnc-prefix), the workspace (cc-panels), the
//!                            mouse's SteamVR driver; 3 · commands: cc-panels, cc-home, cc-box
//!                            in ~/.local/bin, and KWin's grant for the window panels
//!   cc-home install desktop  the VR launcher's "Desktop" starts Command Center
//!                            (session/cc-launch), replacing the stock desktop's
//!                            entry (remove ~/.local/share/applications/
//!                            deckard-nested-desktop.desktop to give the stock one back)
//!   cc-home install remove   undoes both, and unregisters the pointer driver; the checkout,
//!                            the container and ~/.config/control-center stay
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

fn desktop() -> Result<(), String> {
    let (root, home) = (root(), home_dir());
    let apps = format!("{home}/.local/share/applications");
    fs::create_dir_all(&apps).map_err(|e| format!("{apps}: {e}"))?;
    // Overrides /usr/share/applications/deckard-nested-desktop.desktop (SteamOS's nested desktop).
    let launch = root.join("session/cc-launch");
    let entry = format!("[Desktop Entry]\nType=Application\nName=Desktop\nComment=Command Center: your Plasma desktop and remote machines as VR panels\nExec={}\nCategories=Utility;\n", launch.display());
    fs::write(format!("{apps}/deckard-nested-desktop.desktop"), entry).map_err(|e| format!("{apps}: {e}"))?;
    sycoca();
    println!("the VR launcher's Desktop now starts Command Center ({})", launch.display());
    Ok(())
}

fn remove() -> Result<(), String> {
    let (root, home) = (root(), home_dir());
    ok(&mut Command::new(root.join("driver/cc_pointer/install.sh")).arg("uninstall"), false)?;
    // Only our own links, and the Desktop entry only if it's ours (another app may have put its own there).
    for c in ["cc-panels", "cc-home", "cc-box"] {
        let l = format!("{home}/.local/bin/{c}");
        if fs::read_link(&l).is_ok_and(|t| t.starts_with(&root)) {
            fs::remove_file(&l).map_err(|e| format!("{l}: {e}"))?;
        }
    }
    let apps = format!("{home}/.local/share/applications");
    let _ = fs::remove_file(format!("{apps}/org.controlcenter.panels.desktop"));
    let entry = format!("{apps}/deckard-nested-desktop.desktop");
    if fs::read_to_string(&entry).is_ok_and(|t| t.contains("Comment=Command Center")) {
        fs::remove_file(&entry).map_err(|e| format!("{entry}: {e}"))?;
    }
    sycoca();
    println!("removed: the commands, the Desktop entry (the VR launcher's Desktop is SteamOS's again) and the pointer driver");
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
    "-DWITH_FFMPEG=ON", "-DWITH_VIDEO_FFMPEG=ON", "-DWITH_DSP_FFMPEG=OFF", "-DWITH_SWSCALE=ON",
    "-DWITH_OPENH264=OFF", "-DWITH_CLIENT=ON", "-DWITH_CLIENT_SDL=OFF", "-DWITH_X11=OFF", "-DWITH_WAYLAND=OFF", "-DWITH_SERVER=OFF",
    "-DWITH_SHADOW=OFF", "-DWITH_PROXY=OFF", "-DWITH_SAMPLE=OFF", "-DWITH_MANPAGES=OFF", "-DWITH_CUPS=OFF", "-DWITH_PCSC=OFF",
    "-DWITH_PULSE=ON", "-DWITH_ALSA=OFF", "-DWITH_OSS=OFF", "-DWITH_FUSE=OFF", "-DWITH_KRB5=OFF", "-DWITH_SMARTCARD_EMULATE=OFF",
    "-DWITH_FDK_AAC=OFF", "-DWITH_LAME=OFF", "-DWITH_SOXR=OFF", "-DWITH_OPUS=OFF", "-DWITH_GSM=OFF", "-DWITH_FAAD2=OFF", "-DWITH_FAAC=OFF",
    "-DWITH_AAD=OFF", "-DWITH_WEBVIEW=OFF", "-DWITH_SDL_IMAGE_DIALOGS=OFF", "-DCHANNEL_URBDRC=OFF", "-DWITH_JSONC_REQUIRED=OFF",
    "-DBUILD_TESTING=OFF", "-DWITH_SIMD=ON",
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
    println!("a Nomads Galaxy project");
    Ok(())
}
