//! cc-install is Command Center's installer, the terminal app the `install` script downloads and
//! runs. It figures out whether it's on the Steam Frame or a host, offers Install (or Update and
//! Remove when something's already there), and then runs the steps the repo already has instead of
//! a second copy of them. On the Frame that's install.sh's stages and `cc-home install`
//! (crates/cc-home/src/install.rs), built from source in the container. On a host it's the
//! release's cc-host, checked against SHA256SUMS, then `cc-host install`. It never restarts
//! anything unless you type yes here.
//! ponytail: the Frame builds from source, which is about 10 GB and a long first run. A prebuilt
//! Frame release would need cc-panels linked against what the container provides now (FFmpeg with
//! H.264 from RPM Fusion, GBM, libdrm, PipeWire, json-c and the rest of FreeRDP's dependencies)
//! plus the FreeRDP 3.31.1 prefix (panels/third_party/prefix), either shipped as a container image
//! or bundled with the binary and an rpath, built on an aarch64 runner.
//!   cc-install                 the terminal app
//!   cc-install --yes [N ...]   no questions: install, or update what's there; on a host, share
//!                              monitors N (default: the ones shared now, else all)
//!   cc-install --remove --yes  remove, no questions
//!   cc-install --dry-run       say what it found and what it would do, change nothing
//! Environment:
//!   CC_DL      the release to download from (a URL; file:// works too)
//!   CC_SUMS    that release's SHA256SUMS, already downloaded and checked (the install script
//!              passes both of these)
//!   CC_DIR     the Frame's checkout
//!   CC_AS      frame|host, to act as one on the other (testing)
//!   CC_REPO    clone a different repo (testing)
mod plan;
mod ui;

use plan::{Action, Do, Facts, Kind, Step};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const DL: &str = "https://framecc.nomadsgalaxy.com/dl"; // forwards to the latest GitHub release (site/worker.js)

/// Everything the steps print. The screens show it, it's saved to a file, and the plain mode
/// echoes it.
pub struct Log {
    lines: Mutex<Vec<String>>,
    file: Mutex<Option<std::fs::File>>,
    pub path: PathBuf,
    echo: bool,
}

impl Log {
    fn new(path: PathBuf, echo: bool) -> Log {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")));
        Log { lines: Mutex::new(vec![]), file: Mutex::new(std::fs::File::create(&path).ok()), path, echo }
    }

    #[cfg(test)]
    pub fn memory() -> Log {
        Log { lines: Mutex::new(vec![]), file: Mutex::new(None), path: "/tmp/install.log".into(), echo: false }
    }

    pub fn push(&self, l: String) {
        if self.echo {
            println!("    {l}");
        }
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{l}");
        }
        self.lines.lock().unwrap().push(l);
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

/// What the worker thread tells the screens.
pub enum Ev {
    Started(usize),
    Finished(usize, Result<(), String>),
    Skipped(usize, String),
    Restart(bool),
    AllDone,
}

/// The running step's process, kept so quitting can stop it.
static CHILD: AtomicI32 = AtomicI32::new(0);

/// Runs a command to the end and puts its output (both streams, line by line) in the log.
fn run(cmd: &[String], frame: bool, log: &Arc<Log>) -> Result<(), String> {
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if frame {
        // podman needs the real user bus, and a desktop session may be on a private one (cc-box is)
        let rt = format!("/run/user/{}", unsafe { libc::getuid() });
        c.env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={rt}/bus")).env("XDG_RUNTIME_DIR", rt);
    }
    c.env("CC_INSTALLER", "1"); // so cc-host install leaves the next step to us
    log.push(format!("$ {}", cmd.join(" ")));
    let mut child = c.spawn().map_err(|e| format!("{}: {e}", cmd[0]))?;
    CHILD.store(child.id() as i32, Relaxed);
    let readers: Vec<_> = [child.stdout.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|e| Box::new(e) as Box<dyn std::io::Read + Send>)]
        .into_iter()
        .flatten()
        .map(|r| {
            let log = log.clone();
            std::thread::spawn(move || {
                for l in BufReader::new(r).split(b'\n').map_while(Result::ok) {
                    // cargo and curl draw progress with \r, so keep only the last part of each line
                    let l = String::from_utf8_lossy(&l);
                    log.push(l.rsplit('\r').next().unwrap_or("").trim_end().to_owned());
                }
            })
        })
        .collect();
    let status = child.wait().map_err(|e| e.to_string());
    CHILD.store(0, Relaxed);
    for r in readers {
        let _ = r.join();
    }
    match status? {
        s if s.success() => Ok(()),
        s => Err(format!("{} exited with {}", cmd[0], s.code().map_or("a signal".into(), |c| c.to_string()))),
    }
}

/// Downloads cc-host for this arch from the release to `dest` and checks it against SHA256SUMS.
fn download(dest: &Path, arch: &str, log: &Arc<Log>) -> Result<(), String> {
    use sha2::Digest;
    let dl = std::env::var("CC_DL").unwrap_or_else(|_| DL.into());
    let dir = dest.parent().unwrap_or(Path::new("/tmp"));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let sums = match std::env::var("CC_SUMS") {
        Ok(p) => std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?,
        Err(_) => {
            let p = dir.join("SHA256SUMS");
            run(&["curl".into(), "-fsSL".into(), "-o".into(), p.display().to_string(), format!("{dl}/SHA256SUMS")], false, log)?;
            std::fs::read_to_string(&p).map_err(|e| e.to_string())?
        }
    };
    let name = format!("cc-host-{arch}");
    let want = plan::sum_for(&sums, &name).ok_or(format!("The release has no checksum for {name}, so I won't use it."))?;
    run(&["curl".into(), "-fsSL".into(), "-o".into(), dest.display().to_string(), format!("{dl}/{name}")], false, log)?;
    let got: String = sha2::Sha256::digest(std::fs::read(dest).map_err(|e| e.to_string())?).iter().map(|b| format!("{b:02x}")).collect();
    if got != want {
        let _ = std::fs::remove_file(dest);
        return Err(format!("{name} doesn't match its checksum (got {got}, want {want})"));
    }
    log.push(format!("{name}: checksum {got} matches"));
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

/// Runs the steps in order and stops at the first one that fails. On the Frame it then checks
/// whether SteamVR needs a restart to pick up the pointer driver.
fn work(f: &Facts, action: Action, steps: &[Step], log: &Arc<Log>, emit: &dyn Fn(Ev)) {
    for (i, s) in steps.iter().enumerate() {
        emit(Ev::Started(i));
        let r = match &s.what {
            Do::Skip(why) => {
                log.push(format!("{}: skipped, {why}", s.title));
                emit(Ev::Skipped(i, why.clone()));
                continue;
            }
            Do::Run(cmd) => run(cmd, f.kind == Kind::Frame, log),
            Do::Download(dest) => download(dest, &f.arch, log),
        };
        if let Err(e) = &r {
            log.push(e.clone());
        }
        let failed = r.is_err();
        emit(Ev::Finished(i, r));
        if failed {
            emit(Ev::AllDone);
            return;
        }
    }
    if f.kind == Kind::Frame && action != Action::Remove {
        // exit 1 plus "running without" means SteamVR is running an older pointer than the one
        // registered (or none at all)
        let check = f.repo.join("driver/cc_pointer/install.sh");
        let o = Command::new(&check).arg("check").stdin(Stdio::null()).output();
        let restart = o.is_ok_and(|o| !o.status.success() && String::from_utf8_lossy(&o.stdout).contains("running without"));
        emit(Ev::Restart(restart));
    }
    emit(Ev::AllDone);
}

fn usage() -> ! {
    eprintln!("usage: cc-install [--yes [monitor ...]] [--remove --yes] [--dry-run]");
    std::process::exit(2)
}

fn main() {
    let (mut yes, mut remove, mut dry, mut mons) = (false, false, false, vec![]);
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            "--remove" => remove = true,
            "--dry-run" => dry = true,
            "--help" | "-h" => usage(),
            m if m.parse::<usize>().is_ok() => mons.push(m.to_owned()),
            _ => usage(),
        }
    }
    let f = plan::look();
    let work_dir = std::env::temp_dir().join(format!("cc-install-{}", std::process::id()));
    let log_path = f.home.join(".cache/control-center/install.log");
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdin()) && std::io::IsTerminal::is_terminal(&std::io::stdout());
    let code = if yes || dry || remove || !tty {
        if !(yes || dry) {
            eprintln!("There's no terminal to ask questions on here. Run it with --yes to {} without questions, or --dry-run to see what it would do.", if remove { "remove Command Center" } else { "install" });
            2
        } else {
            plain(&f, remove, dry, &mons, Arc::new(Log::new(log_path, true)), &work_dir)
        }
    } else {
        tui(f, Arc::new(Log::new(log_path, false)), work_dir.clone())
    };
    let _ = std::fs::remove_dir_all(&work_dir);
    std::process::exit(code)
}

/// The non-interactive install (--yes, --remove --yes, --dry-run): numbered steps with each one's
/// log under it.
fn plain(f: &Facts, remove: bool, dry: bool, mons: &[String], log: Arc<Log>, work_dir: &Path) -> i32 {
    println!("Command Center installer {}", env!("CARGO_PKG_VERSION"));
    println!("{}", match f.kind {
        Kind::Frame => "This is a Steam Frame.".to_owned(),
        Kind::Host => format!("This is {}, a computer the Frame can show ({}).", f.host, f.arch),
    });
    println!("{}", f.installed.as_ref().map_or("Command Center isn't installed here yet.".into(), |v| format!("Command Center is installed here: {v}.")));
    if f.kind == Kind::Frame && f.desktop_open {
        println!("Your Desktop is open. It keeps running while this works, and an update loads the next time you open it.");
    }
    if !f.missing.is_empty() {
        println!("Before I can install, these need fixing:");
        for (what, fix) in &f.missing {
            println!("  ✗ {what}\n    {fix}");
        }
        if !dry {
            return 1;
        }
    }
    let action = if remove {
        if f.installed.is_none() {
            println!("There's nothing to remove.");
            return 0;
        }
        Action::Remove
    } else if f.installed.is_some() { Action::Update } else { Action::Install };
    let share = if mons.is_empty() {
        plan::ticked(f).iter().enumerate().filter(|(_, t)| **t).map(|(i, _)| i).collect()
    } else {
        match plan::parse_monitors(mons, f.monitors.len()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        }
    };
    if f.kind == Kind::Host && action != Action::Remove {
        for (i, m) in f.monitors.iter().enumerate() {
            println!("  monitor {i}: {} {}x{}{}", m.name, m.w, m.h, if share.contains(&i) { ", shared" } else { "" });
        }
    }
    // --yes doesn't turn announcing on by itself, it just leaves on whatever's on now
    let steps = plan::plan(f, action, &share, f.announcing, work_dir);
    if dry {
        println!("{} would run:", action.label());
        for (i, s) in steps.iter().enumerate() {
            let what = match &s.what {
                Do::Run(c) => c.join(" "),
                Do::Download(p) => format!("download cc-host-{} from {} to {}, checked against SHA256SUMS", f.arch, std::env::var("CC_DL").unwrap_or(DL.into()), p.display()),
                Do::Skip(why) => format!("nothing: {why}"),
            };
            println!("  [{}/{}] {}\n        {what}", i + 1, steps.len(), s.title);
        }
        return 0;
    }
    // printed here so it lands in order with the log's lines
    let (failed, restart) = (std::cell::Cell::new(None), std::cell::Cell::new(false));
    work(f, action, &steps, &log, &|e| match e {
        Ev::Started(i) => println!("[{}/{}] {}", i + 1, steps.len(), steps[i].title),
        Ev::Finished(i, Ok(())) => println!("  ✓ {}", steps[i].title),
        Ev::Finished(i, Err(_)) => failed.set(Some(i)),
        Ev::Skipped(i, why) => println!("  ✓ {} (skipped: {why})", steps[i].title),
        Ev::Restart(r) => restart.set(r),
        Ev::AllDone => {}
    });
    let (failed, restart) = (failed.get(), restart.get());
    if let Some(i) = failed {
        println!("\n✗ \"{}\" didn't work.\n{}\nThe whole log is in {}", steps[i].title, steps[i].fix, log.path.display());
        return 1;
    }
    println!();
    for l in plan::done(f, action, &share, f.announcing, &log.lines(), restart) {
        println!("{l}");
    }
    0
}

fn tui(f: Facts, log: Arc<Log>, work_dir: PathBuf) -> i32 {
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    let mut app = ui::App::new(f, log.clone(), work_dir);
    let mut term = ratatui::init();
    let (tx, rx) = mpsc::channel::<Ev>();
    let mut tx = Some(tx);
    let after = loop {
        let _ = term.draw(|fr| ui::draw(fr, &app));
        if event::poll(Duration::from_millis(120)).unwrap_or(false) {
            if let Ok(Event::Key(k)) = event::read() {
                if k.kind == KeyEventKind::Press {
                    match app.key(k.code) {
                        Some(ui::Out::Start) => {
                            let (f, action, steps, log, tx) = (app.f.clone(), app.action, app.steps.clone(), log.clone(), tx.take().expect("started once"));
                            std::thread::spawn(move || work(&f, action, &steps, &log, &|e| {
                                let _ = tx.send(e);
                            }));
                        }
                        Some(o) => break o,
                        None => {}
                    }
                }
            }
        }
        while let Ok(e) = rx.try_recv() {
            app.event(e);
        }
        app.tick += 1;
    };
    ratatui::restore();
    match after {
        ui::Out::Quit => {
            let pid = CHILD.load(Relaxed);
            if pid > 0 {
                unsafe { libc::kill(pid, libc::SIGTERM) };
                println!("Stopped. Run the installer again to pick up where it stopped.");
                return 1;
            }
            if app.screen == ui::Screen::Done {
                for l in plan::done(&app.f, app.action, &app.share(), app.announce, &log.lines(), app.restart) {
                    println!("{l}"); // stays on the terminal after the screen closes
                }
            }
            i32::from(app.screen == ui::Screen::Failed)
        }
        ui::Out::Reboot => {
            println!("Restarting the Frame...");
            i32::from(!Command::new("systemctl").arg("reboot").status().is_ok_and(|s| s.success()))
        }
        ui::Out::Pair => {
            use std::os::unix::process::CommandExt;
            let e = Command::new(app.f.home.join(".local/bin/cc-share")).arg("pair").exec();
            eprintln!("cc-share pair: {e}");
            1
        }
        ui::Out::Start => 0,
    }
}
