//! What cc-home checks about SteamOS each time the Desktop starts (`cc-home desktop`, session/cc-launch).
//! Both only apply to a native build (session.rs `native()`): the sysext image, or a checkout built
//! that way. The container build has its own libraries and doesn't care what SteamOS is.
//!
//! The ABI check. The sysext image is stamped `ID=_any`, so SteamOS updating (0.4.5 to 0.4.6)
//! doesn't make systemd-sysext drop it. What breaks a native build is a library it was built
//! against changing, so the version the build was made on (`panels/third_party/prefix/steamos-release`,
//! written by packaging/sysext/build-native.sh) is compared with the running one. If they're the same,
//! nothing more is done. If they differ, the libraries are checked with `ldd`, and it starts if they
//! still load, with a plain note, or stops with a plain message about what's gone.
use std::fs;
use std::path::Path;
use std::process::Command;

// ---- the SteamOS version

/// VERSION_ID from an os-release file's text (`VERSION_ID=0.4.5`, quoted or not).
pub fn os_version(os_release: &str) -> Option<String> {
    let v = os_release.lines().find_map(|l| l.trim().strip_prefix("VERSION_ID="))?.trim().trim_matches(|c| c == '"' || c == '\'');
    (!v.is_empty()).then(|| v.to_owned())
}

/// The SteamOS version a build was made on, from the file build-native.sh leaves (the version, and a newline).
pub fn stamp_version(text: &str) -> Option<String> {
    let v = text.trim();
    (!v.is_empty() && !v.contains(char::is_whitespace)).then(|| v.to_owned())
}

#[derive(Debug, PartialEq)]
pub enum Built {
    /// Built on this SteamOS.
    Same,
    Differs { built: String, running: String },
    /// Not known (no stamp, or no os-release), so nothing can be said.
    Unknown,
}

pub fn compare(built: Option<&str>, running: Option<&str>) -> Built {
    match (built, running) {
        (Some(b), Some(r)) if b == r => Built::Same,
        (Some(b), Some(r)) => Built::Differs { built: b.into(), running: r.into() },
        _ => Built::Unknown,
    }
}

// ---- ldd's answer

/// What ldd says can't be loaded: `libfoo.so.1 => not found`, and glibc's
/// `... version `GLIBC_2.43' not found (required by ...)`. One readable name each, once.
pub fn unresolved(ldd_output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in ldd_output.lines().filter(|l| l.contains("not found")) {
        let l = l.trim();
        let name = if let Some((lib, _)) = l.split_once("=>") {
            lib.trim().to_owned()
        } else if let Some(v) = l.split('`').nth(1).and_then(|r| r.split('\'').next()) {
            format!("{v} (a newer glibc than SteamOS has)")
        } else {
            l.to_owned()
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// What to tell the user. `missing` is empty when everything still loads.
pub fn abi_message(built: &str, running: &str, missing: &[String]) -> String {
    if missing.is_empty() {
        format!("SteamOS is {running} now, and this Command Center was built on {built}. Everything it needs still loads, so it starts. Update Command Center when you can, since a later SteamOS may change a library it uses.")
    } else {
        format!(
            "SteamOS is {running} now, and this Command Center was built on {built}. It can't start because SteamOS no longer has what it was built against: {}. Update Command Center for this SteamOS (run its installer again), and it will start.",
            missing.join(", ")
        )
    }
}

// ---- the check

/// Writes what it says to stderr and to ~/.cache/control-center/launch.log, which cc-launch's own
/// output may not reach (the VR launcher runs it).
pub fn note(msg: &str) {
    eprintln!("{msg}");
    let path = crate::cache("launch.log");
    let _ = fs::create_dir_all(Path::new(&path).parent().unwrap_or(Path::new(".")));
    if let Ok(mut f) = fs::OpenOptions::new().append(true).create(true).open(&path) {
        let _ = std::io::Write::write_all(&mut f, format!("{}\n", msg).as_bytes());
    }
}

/// Whether the native build in `root` can start on this SteamOS. Err is what to say, and the
/// Desktop doesn't start.
pub fn check_abi(root: &Path) -> Result<(), String> {
    let built = fs::read_to_string(root.join("panels/third_party/prefix/steamos-release")).ok().and_then(|t| stamp_version(&t));
    let running = fs::read_to_string("/etc/os-release").ok().and_then(|t| os_version(&t));
    let Built::Differs { built, running } = compare(built.as_deref(), running.as_deref()) else {
        return Ok(()); // the same SteamOS, or nothing to compare
    };
    // ldd loads the libraries without running the program. Its answer for a missing one is on stdout,
    // and glibc's version complaints are on stderr.
    let panels = root.join("target/release/cc-panels");
    let out = Command::new("ldd").arg(&panels).output();
    let Ok(out) = out else {
        note(&format!("SteamOS is {running} now, and this Command Center was built on {built}. I couldn't check its libraries (no ldd), so I'm starting it anyway."));
        return Ok(());
    };
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let missing = unresolved(&text);
    let msg = abi_message(&built, &running, &missing);
    if missing.is_empty() {
        note(&msg);
        Ok(())
    } else {
        note(&msg);
        Err(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_the_version_from_os_release() {
        let os = "NAME=\"SteamOS\"\nID=steamos\nVERSION_ID=0.4.5\nVARIANT_ID=vr\nBUILD_ID=20261007.6125817\n";
        assert_eq!(os_version(os), Some("0.4.5".into()));
        assert_eq!(os_version("VERSION_ID=\"3.7.1\"\n"), Some("3.7.1".into()), "quoted");
        assert_eq!(os_version("  VERSION_ID='1'\n"), Some("1".into()), "indented and single-quoted");
        assert_eq!(os_version("ID=steamos\n"), None);
        assert_eq!(os_version("VERSION_ID=\n"), None, "empty");
        assert_eq!(os_version("BUILD_VERSION_ID=9\n"), None, "only the exact key");
    }

    #[test]
    fn it_reads_the_build_stamp() {
        assert_eq!(stamp_version("0.4.5\n"), Some("0.4.5".into()));
        assert_eq!(stamp_version(" 0.3.0 "), Some("0.3.0".into()));
        assert_eq!(stamp_version(""), None);
        assert_eq!(stamp_version("0.4 5"), None, "one version, not a sentence");
    }

    #[test]
    fn it_tells_the_same_steamos_from_a_newer_one_and_from_not_knowing() {
        assert_eq!(compare(Some("0.4.5"), Some("0.4.5")), Built::Same);
        assert_eq!(compare(Some("0.4.5"), Some("0.4.6")), Built::Differs { built: "0.4.5".into(), running: "0.4.6".into() });
        assert_eq!(compare(Some("0.4.5"), Some("0.3.0")), Built::Differs { built: "0.4.5".into(), running: "0.3.0".into() }, "older counts too");
        assert_eq!(compare(None, Some("0.4.5")), Built::Unknown, "no stamp: not a native build, or one that predates it");
        assert_eq!(compare(Some("0.4.5"), None), Built::Unknown);
        assert_eq!(compare(None, None), Built::Unknown);
    }

    #[test]
    fn it_picks_out_what_ldd_cannot_load() {
        let ok = "\tlinux-vdso.so.1 (0x0000ffff)\n\tlibfreerdp3.so.3 => /usr/lib/command-center/panels/third_party/prefix/lib64/libfreerdp3.so.3 (0x0000ffff)\n\tlibc.so.6 => /usr/lib/libc.so.6 (0x0000ffff)\n";
        assert!(unresolved(ok).is_empty());
        let bad = "\tlibavcodec.so.62 => not found\n\tlibc.so.6 => /usr/lib/libc.so.6 (0x1)\n\tlibavutil.so.60 => not found\n\tlibavcodec.so.62 => not found\n";
        assert_eq!(unresolved(bad), ["libavcodec.so.62", "libavutil.so.60"], "each once, in order");
        let glibc = "./cc-panels: /usr/lib/libc.so.6: version `GLIBC_2.43' not found (required by ./cc-panels)\n";
        assert_eq!(unresolved(glibc), ["GLIBC_2.43 (a newer glibc than SteamOS has)"]);
        assert!(unresolved("").is_empty());
        assert!(unresolved("\tnot a dynamic executable\n").is_empty(), "ldd on a static binary says so, which isn't a missing library");
    }

    #[test]
    fn it_says_plainly_what_changed_and_whether_it_starts() {
        let fine = abi_message("0.4.5", "0.4.6", &[]);
        assert!(fine.contains("0.4.6") && fine.contains("0.4.5") && fine.contains("still loads, so it starts"), "{fine}");
        let broken = abi_message("0.4.5", "0.5.0", &["libavcodec.so.62".into(), "libavutil.so.60".into()]);
        assert!(broken.contains("can't start") && broken.contains("libavcodec.so.62, libavutil.so.60") && broken.contains("installer"), "{broken}");
        assert!(!broken.contains("so it starts"));
    }
}
