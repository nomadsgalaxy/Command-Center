//! Updates for the Frame's system image without a password: `cc-home update` asks polkit to run
//! /usr/lib/command-center/cc-sysext-update (org.controlcenter.update, allowed for the user at the
//! device, allow_active only), and that root helper does this, in this order, and nothing else:
//!
//! 1. Its own work directory, /var/lib/command-center/update: root's, 0700, a real directory (not
//!    a symlink), emptied of anything left from before. Nothing but root can put a file in it.
//! 2. SHA256SUMS and SHA256SUMS.sig from our release URL (fixed here, HTTPS only, redirects
//!    HTTPS only, with timeouts and size caps).
//! 3. `gpgv` checks the signature against our release key, pinned in the image
//!    (/usr/lib/command-center/release-key.gpg, from packaging/command-center.asc). No shared
//!    keyring is used or touched. Nothing in SHA256SUMS is trusted before this passes.
//! 4. The one `command-center_<version>.raw` SHA256SUMS lists (a strict name, no paths), fetched
//!    the same way, and its SHA-256 checked against the signed list.
//! 5. Only then `systemd-sysupdate --component=command-center --verify=no update`, which copies it
//!    from the work directory into /var/lib/extensions/command-center.raw.v/ (the transfer file in
//!    /usr/lib/sysupdate.command-center.d/). --verify=no because the checking is done above.
//! 6. `systemd-sysext refresh` only with the Desktop's lock (tab.rs) taken and held across the
//!    check and the refresh, so a Desktop starting meanwhile waits for it, and with nothing else
//!    running from the image. Otherwise the new image is merged at the next boot or the next update
//!    with those closed. What this can't cover: SteamVR starting the dashboard tab's process in the
//!    few milliseconds between the check and the refresh (the tab doesn't take that lock).
//!
//! It takes no arguments, reads no paths from anywhere, follows no symlink it makes or empties,
//! and runs every program by its full path with an empty environment. It's a copy of cc-home
//! (not a link, so polkit's exec.path matches it and its name is its own), and as root under
//! pkexec cc-home refuses to be anything else (`root_ok`). The downloads' size caps hold during
//! the transfer too: the Frame's curl is 8.7, and --max-filesize covers transfers of unknown size
//! from 8.4 on; each file's size is checked again after. sysupdate itself never installs an older
//! version than the newest installed one unless asked for a version, which this never does. Signing a release is
//! Anthony's, by hand, with the key's secret half; nothing here touches that.
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const RELEASES: &str = "https://github.com/nomadsgalaxy/Command-Center/releases/latest/download";
const WORK: &str = "/var/lib/command-center/update";
const KEYRING: &str = "/usr/lib/command-center/release-key.gpg";
const HELPER: &str = "/usr/lib/command-center/cc-sysext-update";
const EXTENSIONS: &str = "/var/lib/extensions";

/// Size caps and time limits for the downloads.
const SUMS_MAX: u64 = 64 * 1024;
const SIG_MAX: u64 = 16 * 1024;
const IMAGE_MAX: u64 = 2 * 1024 * 1024 * 1024;
const SMALL_SECS: u64 = 60;
const IMAGE_SECS: u64 = 1800;

/// The steps that reach outside: a fake one in tests records the order.
trait Ops {
    /// Downloads RELEASES/name into the work directory, at most `max` bytes, in `secs`.
    fn fetch(&mut self, name: &str, max: u64, secs: u64) -> Result<(), String>;
    /// gpgv on SHA256SUMS.sig and SHA256SUMS in the work directory.
    fn verify_signature(&mut self) -> Result<(), String>;
    fn sysupdate(&mut self) -> Result<(), String>;
    /// Takes the Desktop's lock and holds it until `release`, so a Desktop that starts meanwhile
    /// waits for it. False if a Desktop holds it, or who asked isn't known.
    fn claim(&mut self) -> bool;
    /// Something other than this helper runs a program from the image (the dashboard tab, say).
    fn in_use(&self) -> bool;
    fn refresh(&mut self) -> Result<(), String>;
    fn release(&mut self);
}

/// The one image SHA256SUMS lists, and its hash: `<64 hex>  command-center_<version>.raw` (a `*`
/// before the name, binary mode, is fine). A version is letters, digits and `._+~-`, so a name can
/// never be a path. None unless there's exactly one.
fn image_entry(sums: &str) -> Option<(String, String)> {
    let ok_version = |v: &str| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || "._+~-".contains(c)) && !v.contains("..");
    let mut found = sums.lines().filter_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        let v = name.strip_prefix("command-center_")?.strip_suffix(".raw")?;
        (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) && ok_version(v)).then(|| (name.to_string(), hash.to_ascii_lowercase()))
    });
    let one = found.next()?;
    found.next().is_none().then_some(one)
}

/// Whether file `path` hashes to `want` (lowercase hex SHA-256).
fn hash_ok(path: &Path, want: &str) -> Result<bool, String> {
    use sha2::Digest;
    let mut f = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>() == want)
}

/// The work directory, made ready: created 0700 if it isn't there, refused if it's a symlink, not
/// a directory, not ours (`uid`), or open to anyone else; then emptied of regular files, refusing
/// anything else in it.
fn prepare(dir: &Path, uid: u32) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Err(e) => return Err(format!("{}: {e}", dir.display())),
        Ok(m) if !m.file_type().is_dir() => return Err(format!("{} isn't a plain directory (a symlink?), so nothing's done", dir.display())),
        Ok(m) if m.uid() != uid || m.permissions().mode() & 0o077 != 0 => return Err(format!("{} isn't private to its owner, so nothing's done", dir.display())),
        Ok(_) => {}
    }
    for e in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
        let t = e.file_type().map_err(|e| format!("{}: {e}", e))?;
        if !t.is_file() {
            return Err(format!("{} has something other than a file in it ({}), so nothing's done", dir.display(), e.file_name().to_string_lossy()));
        }
        fs::remove_file(e.path()).map_err(|err| format!("{}: {err}", e.path().display()))?;
    }
    Ok(())
}

/// Steps 2 to 6 (step 1 is `prepare`), in the only order that's safe: nothing's believed before
/// the signature, nothing's installed before the hash.
fn run(o: &mut impl Ops, dir: &Path) -> Result<String, String> {
    o.fetch("SHA256SUMS", SUMS_MAX, SMALL_SECS)?;
    o.fetch("SHA256SUMS.sig", SIG_MAX, SMALL_SECS)?;
    o.verify_signature().map_err(|e| format!("the release's signature doesn't check out ({e}), so nothing's installed"))?;
    let sums = fs::read_to_string(dir.join("SHA256SUMS")).map_err(|e| format!("SHA256SUMS: {e}"))?;
    let (name, hash) = image_entry(&sums).ok_or("the signed SHA256SUMS doesn't list exactly one command-center_<version>.raw, so nothing's installed")?;
    o.fetch(&name, IMAGE_MAX, IMAGE_SECS)?;
    if !hash_ok(&dir.join(&name), &hash)? {
        return Err(format!("{name} doesn't match its signed hash, so it isn't installed"));
    }
    o.sysupdate()?;
    let later = format!("{name} is installed; it's loaded at the next boot, or run cc-home update again with the Desktop closed");
    if !o.claim() {
        return Ok(later);
    }
    // the Desktop's lock is held from here to the refresh's end, so no Desktop starts under it
    let r = if o.in_use() { Ok(later) } else { o.refresh().map(|_| format!("{name} is installed and loaded")) };
    o.release();
    r
}

/// What a process running as root under pkexec may be: only the helper, by its own name. If
/// polkit or a link ever handed root some other name (cc-home itself), its normal commands must
/// not run as root.
pub fn root_ok(euid: u32, under_pkexec: bool, name: &str) -> bool {
    !(euid == 0 && under_pkexec) || name == "cc-sysext-update"
}

/// Programs that run from the image: refreshing would swap their files under them.
fn from_image(exe: &Path) -> bool {
    exe.starts_with("/usr/lib/command-center") || exe.parent() == Some(Path::new("/usr/bin")) && exe.file_name().is_some_and(|n| n.to_string_lossy().starts_with("cc-"))
}

/// Whether anything but `ours` (this helper and who started it) runs a program from the image,
/// going by `proc`'s `<pid>/exe` links: the Desktop's cc-panels, the dashboard tab's, cc-home.
fn image_in_use(proc: &Path, ours: &[u32]) -> bool {
    let Ok(dir) = fs::read_dir(proc) else { return true }; // can't tell: don't refresh
    dir.flatten().filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()).filter(|p| !ours.contains(p)).any(|p| fs::read_link(proc.join(format!("{p}/exe"))).is_ok_and(|x| from_image(&x)))
}

/// This process and its parents, up to init: pkexec, and the `cc-home update` that ran it.
fn ancestors(proc: &Path, me: u32) -> Vec<u32> {
    let mut out = vec![me];
    let mut p = me;
    for _ in 0..16 {
        let Ok(stat) = fs::read_to_string(proc.join(format!("{p}/stat"))) else { break };
        // after the last ')', so a program name with spaces or brackets can't move the fields
        let Some(ppid) = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok()) else { break };
        if ppid <= 1 {
            break;
        }
        out.push(ppid);
        p = ppid;
    }
    out
}

/// The real steps.
struct Live {
    dir: PathBuf,
    user: Option<u32>,           // who asked (PKEXEC_UID), for their Desktop's lock
    held: Option<std::fs::File>, // that lock, while it's ours (claim to release)
}

/// A program by its full path with nothing from the caller's environment.
fn tool(path: &str) -> Command {
    let mut c = Command::new(path);
    c.env_clear().env("PATH", "/usr/bin:/usr/sbin").current_dir("/");
    c
}

fn ran(c: &mut Command, what: &str) -> Result<(), String> {
    let out = c.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

impl Ops for Live {
    fn fetch(&mut self, name: &str, max: u64, secs: u64) -> Result<(), String> {
        let to = self.dir.join(name);
        let mut c = tool("/usr/bin/curl");
        c.args(["--proto", "=https", "--proto-redir", "=https", "--tlsv1.2", "-fsSL", "--max-time", &secs.to_string(), "--max-filesize", &max.to_string(), "-o"])
            .arg(&to)
            .arg(format!("{RELEASES}/{name}"));
        ran(&mut c, &format!("downloading {name}"))?;
        let len = fs::symlink_metadata(&to).map_err(|e| format!("{name}: {e}"))?;
        if !len.file_type().is_file() || len.len() > max {
            return Err(format!("{name} came back bigger than {max} bytes, or not a file"));
        }
        Ok(())
    }

    fn verify_signature(&mut self) -> Result<(), String> {
        let mut c = tool("/usr/bin/gpgv");
        c.env("GNUPGHOME", &self.dir).args(["--keyring", KEYRING]).arg(self.dir.join("SHA256SUMS.sig")).arg(self.dir.join("SHA256SUMS"));
        ran(&mut c, "gpgv")
    }

    fn sysupdate(&mut self) -> Result<(), String> {
        ran(tool("/usr/lib/systemd/systemd-sysupdate").args(["--component=command-center", "--verify=no", "update"]), "systemd-sysupdate")?;
        // the first install put a plain command-center.raw in place; the .raw.v directory supersedes it
        let plain = Path::new(EXTENSIONS).join("command-center.raw");
        if fs::symlink_metadata(&plain).is_ok_and(|m| m.file_type().is_file()) && Path::new(EXTENSIONS).join("command-center.raw.v").is_dir() {
            fs::remove_file(&plain).map_err(|e| format!("{}: {e}", plain.display()))?;
        }
        Ok(())
    }

    fn claim(&mut self) -> bool {
        use std::os::unix::fs::OpenOptionsExt;
        // No PKEXEC_UID: whose Desktop it would be isn't known, so never refresh.
        let Some(uid) = self.user else { return false };
        // It's root in the user's runtime directory: the last part is never followed if it's a
        // symlink, and has to be a plain file. One that isn't there yet is made, theirs, 0600, so
        // their Desktop can still take it.
        let path = format!("/run/user/{uid}/control-center-desktop.lock");
        let opened = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path).or_else(|e| {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e);
            }
            let f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path)?;
            if unsafe { libc::fchown(std::os::fd::AsRawFd::as_raw_fd(&f), uid, u32::MAX) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(f)
        });
        let Ok(f) = opened else { return false };
        if !f.metadata().is_ok_and(|m| m.file_type().is_file()) {
            return false;
        }
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&f), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return false; // a Desktop has it
        }
        self.held = Some(f);
        true
    }

    fn in_use(&self) -> bool {
        // anything running from the image, the dashboard tab's process too, besides this helper
        // and the cc-home update that started it
        let proc = Path::new("/proc");
        image_in_use(proc, &ancestors(proc, std::process::id()))
    }

    fn release(&mut self) {
        self.held = None; // closing it lets go of the lock
    }

    fn refresh(&mut self) -> Result<(), String> {
        ran(tool("/usr/bin/systemd-sysext").arg("refresh"), "systemd-sysext refresh")
    }
}

/// cc-sysext-update, as root through pkexec. Returns the exit code.
pub fn helper(args: &[std::ffi::OsString]) -> i32 {
    let fail = |e: String| {
        eprintln!("cc-sysext-update: {e}");
        1
    };
    if !args.is_empty() {
        return fail("it takes no arguments".into());
    }
    if unsafe { libc::geteuid() } != 0 {
        return fail("it runs as root, through `cc-home update`".into());
    }
    let user = std::env::var("PKEXEC_UID").ok().and_then(|u| u.parse().ok());
    let dir = PathBuf::from(WORK);
    if let Err(e) = prepare(&dir, 0) {
        return fail(e);
    }
    match run(&mut Live { dir, user, held: None }, Path::new(WORK)) {
        Ok(say) => {
            println!("{say}");
            0
        }
        Err(e) => fail(e),
    }
}

/// `cc-home update`: the helper through pkexec, on an image install. A checkout updates with git.
pub fn main() -> i32 {
    if !Path::new(HELPER).exists() {
        eprintln!("this is a checkout, not the system image: update it with git pull and cc-home install");
        return 1;
    }
    match Command::new("/usr/bin/pkexec").arg(HELPER).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("pkexec: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the steps, and fails the ones it's told to. `image` is what the image download writes.
    struct Fake<'a> {
        dir: &'a Path,
        steps: Vec<String>,
        bad_sig: bool,
        image: Vec<u8>,
        desktop: bool,
        tab: bool, // something else runs from the image
        sums: String,
    }

    impl Ops for Fake<'_> {
        fn fetch(&mut self, name: &str, _max: u64, _secs: u64) -> Result<(), String> {
            self.steps.push(format!("fetch {name}"));
            let body = match name {
                "SHA256SUMS" => self.sums.as_bytes().to_vec(),
                "SHA256SUMS.sig" => b"sig".to_vec(),
                _ => self.image.clone(),
            };
            fs::write(self.dir.join(name), body).map_err(|e| e.to_string())
        }
        fn verify_signature(&mut self) -> Result<(), String> {
            self.steps.push("verify".into());
            if self.bad_sig { Err("BAD signature".into()) } else { Ok(()) }
        }
        fn sysupdate(&mut self) -> Result<(), String> {
            self.steps.push("sysupdate".into());
            Ok(())
        }
        fn claim(&mut self) -> bool {
            self.steps.push("claim".into());
            !self.desktop
        }
        fn in_use(&self) -> bool {
            self.tab
        }
        fn refresh(&mut self) -> Result<(), String> {
            self.steps.push("refresh".into());
            Ok(())
        }
        fn release(&mut self) {
            self.steps.push("release".into());
        }
    }

    fn sha(b: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(b).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-update-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn fake(dir: &Path) -> Fake<'_> {
        let image = b"an image".to_vec();
        let sums = format!("{}  command-center_0.9.1.raw\n{}  cc-host.tar.gz\n", sha(&image), sha(b"other"));
        Fake { dir, steps: vec![], bad_sig: false, image, desktop: false, tab: false, sums }
    }

    #[test]
    fn it_verifies_the_signature_then_the_hash_and_only_then_installs() {
        let d = scratch("order");
        prepare(&d, unsafe { libc::geteuid() }).unwrap();
        let mut f = fake(&d);
        assert!(run(&mut f, &d).is_ok());
        assert_eq!(
            f.steps,
            ["fetch SHA256SUMS", "fetch SHA256SUMS.sig", "verify", "fetch command-center_0.9.1.raw", "sysupdate", "claim", "refresh", "release"],
            "the Desktop's lock held across the refresh"
        );
        let mut f = fake(&d);
        f.desktop = true;
        assert!(run(&mut f, &d).unwrap().contains("next boot"));
        assert!(!f.steps.contains(&"refresh".into()) && !f.steps.contains(&"release".into()), "never under a running Desktop, and its lock was never ours");
        let mut f = fake(&d);
        f.tab = true;
        assert!(run(&mut f, &d).unwrap().contains("next boot"));
        assert_eq!(f.steps[f.steps.len() - 2..], ["claim", "release"], "the tab runs from the image: no refresh, and the lock goes back");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bad_signature_or_hash_installs_nothing() {
        let d = scratch("bad");
        prepare(&d, unsafe { libc::geteuid() }).unwrap();
        let mut f = fake(&d);
        f.bad_sig = true;
        assert!(run(&mut f, &d).is_err());
        assert_eq!(f.steps, ["fetch SHA256SUMS", "fetch SHA256SUMS.sig", "verify"], "not even the image is fetched");
        let mut f = fake(&d);
        f.image = b"a different image".to_vec();
        assert!(run(&mut f, &d).unwrap_err().contains("doesn't match"));
        assert!(!f.steps.contains(&"sysupdate".into()));
        let mut f = fake(&d);
        f.sums = format!("{}  command-center_1.raw\n{}  command-center_2.raw\n", sha(b"a"), sha(b"b"));
        assert!(run(&mut f, &d).is_err() && !f.steps.contains(&"sysupdate".into()), "two images: which one? neither");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn only_a_plain_image_name_is_taken_from_the_list() {
        let h = "a".repeat(64);
        assert_eq!(image_entry(&format!("{h}  command-center_0.9.1-3.raw\n")), Some(("command-center_0.9.1-3.raw".into(), h.clone())));
        assert_eq!(image_entry(&format!("{h} *command-center_2.raw\n")).map(|e| e.0), Some("command-center_2.raw".into()), "binary mode");
        for bad in ["command-center_../x.raw", "command-center_a/b.raw", "command-center_.raw", "command-center_1.raw.gz", "../command-center_1.raw"] {
            assert_eq!(image_entry(&format!("{h}  {bad}\n")), None, "{bad}");
        }
        assert_eq!(image_entry("nothouse  command-center_1.raw\n"), None, "not a hash");
    }

    #[test]
    fn the_work_directory_has_to_be_private_and_plain() {
        let me = unsafe { libc::geteuid() };
        let d = scratch("dir");
        prepare(&d, me).unwrap();
        assert_eq!(fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        fs::write(d.join("old"), "x").unwrap();
        prepare(&d, me).unwrap();
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0, "emptied");
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare(&d, me).is_err(), "open to others");
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(prepare(&d, me + 1).is_err(), "someone else's");
        std::os::unix::fs::symlink("/etc", d.join("link")).unwrap();
        assert!(prepare(&d, me).is_err(), "a symlink inside");
        let l = scratch("link");
        std::os::unix::fs::symlink(&d, &l).unwrap();
        assert!(prepare(&l, me).is_err(), "the directory itself a symlink");
        let _ = fs::remove_file(&l);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn root_under_pkexec_is_only_ever_the_helper() {
        assert!(root_ok(0, true, "cc-sysext-update"));
        assert!(!root_ok(0, true, "cc-home"), "a link that resolved back to cc-home: refused");
        assert!(root_ok(1000, true, "cc-home") && root_ok(0, false, "cc-home"), "not pkexec's root: cc-home as usual");
    }

    #[test]
    fn nothing_refreshes_while_anything_runs_from_the_image() {
        let proc = scratch("proc");
        let pid = |n: u32, exe: &str, ppid: u32| {
            let d = proc.join(n.to_string());
            fs::create_dir_all(&d).unwrap();
            std::os::unix::fs::symlink(exe, d.join("exe")).unwrap();
            fs::write(d.join("stat"), format!("{n} (a (b) c) S {ppid} 0 0")).unwrap();
        };
        pid(10, "/usr/lib/command-center/cc-sysext-update", 9); // us
        pid(9, "/usr/bin/pkexec", 8);
        pid(8, "/usr/lib/command-center/cc-home", 1); // cc-home update
        pid(20, "/usr/bin/bash", 1);
        let ours = ancestors(&proc, 10);
        assert_eq!(ours, [10, 9, 8]);
        assert!(!image_in_use(&proc, &ours), "only us and who started us");
        pid(30, "/usr/lib/command-center/target/release/cc-panels", 1); // the dashboard tab
        assert!(image_in_use(&proc, &ours), "the tab runs from the image");
        assert!(from_image(Path::new("/usr/bin/cc-home")) && !from_image(Path::new("/usr/bin/curl")));
        let _ = fs::remove_dir_all(&proc);
    }

    #[test]
    fn it_takes_no_arguments() {
        assert_eq!(helper(&["--anything".into()]), 1);
    }
}
