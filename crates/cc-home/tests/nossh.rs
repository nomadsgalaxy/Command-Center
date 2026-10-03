//! Checks that Command Center works with SSH gone (docs/ssh-free.md §4). It does three things:
//! - runs cc-home's selftest and its SSH-only paths with ssh and scp replaced by shims that fail
//!   loudly, on PATH and over /usr/bin/ssh and /usr/bin/scp in a private mount namespace, so an
//!   absolute path can't slip past
//! - shows the gate opens with CC_SSH=1, so the shims really were in the way
//! - checks the code for any ssh or scp command line made outside ssh_argv()
//!
//! It uses a throwaway HOME (a fresh Frame) and a private CC_PANELS_SOCKET, because a namespace
//! doesn't hide @controlcenter. Nothing here touches the user's files or Desktop. This used to be
//! tests/no-ssh (bash).
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Box_ {
    dir: PathBuf,
}

impl Drop for Box_ {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Box_ {
    fn new() -> Box_ {
        let dir = std::env::temp_dir().join(format!("cc-home-nossh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        for b in ["ssh", "scp"] {
            let p = dir.join(b);
            std::fs::write(&p, format!("#!/bin/sh\necho \"$0 $*\" >> {}/ssh.log\necho \"SSH CALLED: $0 $*\" >&2\nexit 97\n", dir.display())).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("ssh.log"), "").unwrap();
        Box_ { dir }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("ssh.log")).unwrap_or_default()
    }

    /// Runs cc-home with these arguments and SSH unreachable: in a user and mount namespace of its
    /// own where /usr/bin/ssh and /usr/bin/scp are the shims (bind mounts, made in the child before exec).
    fn cc_home(&self, args: &[&str], ssh_on: bool) -> Output {
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let shims: Vec<(std::ffi::CString, std::ffi::CString)> = ["ssh", "scp"].iter()
            .filter(|b| Path::new(&format!("/usr/bin/{b}")).exists())
            .map(|b| (std::ffi::CString::new(self.dir.join(b).to_str().unwrap()).unwrap(), std::ffi::CString::new(format!("/usr/bin/{b}")).unwrap()))
            .collect();
        let mut c = Command::new(env!("CARGO_BIN_EXE_cc-home"));
        c.args(args).env("HOME", self.dir.join("home")).env("CC_PANELS_SOCKET", format!("cc-test-nossh-{}", std::process::id()))
            .env("PATH", format!("{}:{}", self.dir.display(), std::env::var("PATH").unwrap_or_default()))
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        if ssh_on { c.env("CC_SSH", "1"); } else { c.env_remove("CC_SSH"); }
        unsafe {
            c.pre_exec(move || {
                let w = |path: &str, s: &str| -> std::io::Result<()> { std::fs::write(path, s) };
                if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                w("/proc/self/setgroups", "deny")?;
                w("/proc/self/uid_map", &format!("0 {uid} 1"))?;
                w("/proc/self/gid_map", &format!("0 {gid} 1"))?;
                for (from, to) in &shims {
                    if libc::mount(from.as_ptr(), to.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        c.output().expect("cc-home in a private namespace (unshare needs user namespaces)")
    }
}

#[test]
fn works_with_ssh_gone() {
    let b = Box_::new();
    // 1-3: the selftest, and the SSH-only paths refused with a message naming CC_SSH=1
    let st = b.cc_home(&["selftest"], false);
    assert!(st.status.success(), "selftest with SSH gone:\n{}{}", String::from_utf8_lossy(&st.stdout), String::from_utf8_lossy(&st.stderr));
    let probe = b.cc_home(&["machine", "probe", "u@10.1.2.3"], false);
    assert!(!probe.status.success() && String::from_utf8_lossy(&probe.stderr).contains("CC_SSH=1"), "probe: {}", String::from_utf8_lossy(&probe.stderr));
    let add = b.cc_home(&["machine", "add", "other", "u@10.1.2.3:3400"], false);
    assert!(!add.status.success() && String::from_utf8_lossy(&add.stderr).contains("give a size"), "add: {}", String::from_utf8_lossy(&add.stderr));
    assert_eq!(b.log(), "", "something ran ssh or scp");
    // 4: the gate opens with CC_SSH=1 (so the shims really were in the way)
    let _ = b.cc_home(&["machine", "probe", "u@127.0.0.1"], true);
    assert!(!b.log().is_empty(), "with CC_SSH=1 cc-home's probe didn't reach ssh: the shims aren't on its path");
}

/// Whether a line makes an ssh/scp command: a quoted command word, an absolute path, a shell call,
/// or Rust's Command::new.
fn makes_ssh(l: &str) -> bool {
    ["ssh", "scp"].iter().any(|b| {
        let quoted = l.contains(&format!("\"{b}\"")) || l.contains(&format!("'{b}'"));
        let abs = l.contains(&format!("/usr/bin/{b}"));
        let shell = l.split([';', '&', '|', '(']).any(|part| part.trim_start().starts_with(&format!("{b} -")))
            || l.trim_start().starts_with(&format!("{b} -"));
        quoted || abs || shell
    })
}

/// Lines that mention it without running it: ssh_argv itself, JSON/option keys and subscripts.
fn allowed(l: &str) -> bool {
    ["ssh_argv(", "why=\"ssh\""].iter().any(|s| l.contains(s))
        || ["ssh", "scp"].iter().any(|b| {
            let q = format!("\"{b}\"");
            l.contains(&format!("{q}:")) || l.contains(&format!("{q} :")) || l.contains(&format!("get({q})")) || l.contains(&format!("opt({q})"))
                || l.match_indices(&format!("[{q}]")).any(|(i, _)| i > 0 && l[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ')' || c == ']'))
        })
}

#[test]
fn no_ssh_command_outside_ssh_argv() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![root.join("install.sh")];
    let mut stack: Vec<PathBuf> = std::fs::read_dir(root.join("crates")).unwrap().flatten().map(|e| e.path().join("src")).collect();
    stack.push(root.join("session"));
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() { stack.push(p) } else { files.push(p) }
        }
    }
    let mut hits = vec![];
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for (i, l) in text.lines().enumerate() {
            let code = l.trim_start();
            if code.starts_with("//") || code.starts_with('#') {
                continue;
            }
            if makes_ssh(l) && !allowed(l) {
                hits.push(format!("{}:{}: {}", f.strip_prefix(&root).unwrap_or(&f).display(), i + 1, l.trim()));
            }
        }
    }
    assert!(hits.is_empty(), "ssh/scp outside ssh_argv():\n{}", hits.join("\n"));
}

#[test]
fn the_static_check_catches_what_it_should() {
    for bad in [r#"Command::new("ssh").arg(h)"#, "let x = \"/usr/bin/scp\";", "  ssh -o BatchMode=yes host true", "a; scp -q f h:", "run('ssh', x)"] {
        assert!(makes_ssh(bad) && !allowed(bad), "missed: {bad}");
    }
    for ok in [r#"ssh_argv(&["ssh", "-o", "BatchMode=yes"], "probe", "")"#, r#"v["ssh"].as_str()"#, r#"json!({"ssh": null})"#, r#"m.get("ssh")"#, "let sshd = 1;"] {
        assert!(!makes_ssh(ok) || allowed(ok), "flagged: {ok}");
    }
}
