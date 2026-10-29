//! Stepping aside for a game, and coming back, for VR games and 2D Steam games alike.
//!
//! A 2D Steam game on the Frame isn't a scene app. It runs in gamescope (`--backend openvr`) as a
//! SteamVR system overlay, so `GetCurrentSceneProcessId` stays 0 (docs/ARCHITECTURE.md, the
//! dashboard tab). `start` runs a thread that notices one from gamescope's X server on :0: the root
//! window's GAMESCOPE_FOCUSED_APP is a real app id (not 0, and not 769, which is Steam's own UI).
//! If the X server can't be read it falls back to a `reaper SteamLaunch AppId=` process. The thread
//! polls at taskbar.rs's GAME_EVERY and leaves the main loop alone, because an X call can block.
//!
//! `Handoff` is what the main loop asks every tick. While a game is playing, the panels hide (so
//! SteamVR's laser mode, which our overlays keep on, lets the controllers go), the keyboards and
//! mice go back to the Frame, and the Desktop waits. The tab in the dashboard (control/dashtab.rs)
//! takes it back without quitting either side:
//!   - picking the Command Center tab, or pressing its "Command Center" choice, shows the panels and
//!     takes the mice, until the game ends or you press "Steam / game" there.
//!   - pressing "Steam / game" hands it back to the game.
//! When a game ends only what it hid comes back; panels you hid yourself stay hidden.
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

/// Steam's own UI in GAMESCOPE_FOCUSED_APP (controller.txt): focused, but not a game.
const STEAM_UI: u32 = 769;
/// gamescope's X server for Steam: its root window carries the focus properties.
const GAMESCOPE_X: &str = "/tmp/.X11-unix/X0";

static PLAYING: AtomicBool = AtomicBool::new(false);

/// A 2D Steam game has focus, or is still running with Steam's UI in front of it.
pub fn flat_game() -> bool {
    PLAYING.load(Relaxed)
}

/// Starts the thread that watches for a 2D game.
pub fn start() {
    let _ = std::thread::Builder::new().name("cc-flatgame".into()).spawn(|| {
        let mut was = false;
        loop {
            let focused = focused_app_at(Path::new(GAMESCOPE_X));
            let now = decide(was, focused, reaper_running);
            if now != was {
                was = now;
                PLAYING.store(now, Relaxed);
                eprintln!("game: {}", if now { format!("a 2D Steam game has focus (app {focused:?})") } else { "the 2D Steam game ended".into() });
                crate::vr::wake();
            }
            std::thread::sleep(crate::taskbar::GAME_EVERY);
        }
    });
}

/// Whether a 2D game is playing, from what gamescope says has focus (Err: it couldn't be read) and,
/// when that doesn't settle it, whether Steam's launcher process for a game is still there.
fn decide(was: bool, focused: Result<Option<u32>, ()>, reaper: impl FnOnce() -> bool) -> bool {
    match focused {
        Ok(Some(id)) if id != 0 && id != STEAM_UI => true,
        Ok(_) => was && reaper(), // nothing focused, or Steam's UI: still playing while the game's process is
        Err(()) => reaper(),
    }
}

/// The app id in a `reaper SteamLaunch AppId=<id> -- ...` command line (arguments split by NULs).
fn reaper_app(cmdline: &[u8]) -> Option<u32> {
    let mut a = cmdline.split(|&b| b == 0);
    let (exe, verb, id) = (a.next()?, a.next()?, a.next()?);
    if !exe.ends_with(b"reaper") || verb != b"SteamLaunch" {
        return None;
    }
    std::str::from_utf8(id.strip_prefix(b"AppId=")?).ok()?.parse().ok()
}

/// Whether Steam's launcher for a game is running, by looking through /proc.
fn reaper_running() -> bool {
    std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|p| {
        let name = p.file_name();
        name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) && std::fs::read(p.path().join("cmdline")).is_ok_and(|c| reaper_app(&c).is_some_and(|id| id != STEAM_UI))
    })
}

// ------------------------------------------------------------------ reading the X property
// Three requests on gamescope's socket: connect, find the atom, read the root's property. It's
// the X11 wire protocol (little-endian) done by hand because it's all we need from X, and no X
// crate is in the tree. The requests and parsers are plain functions so tests can check them
// without a server.

/// The connection setup request: byte order, protocol 11.0, and no authorization (gamescope's
/// :0 accepts its own user's connections).
fn setup_request() -> [u8; 12] {
    [b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// The root window from the server's setup reply: its 8-byte head and the rest. None if refused.
fn parse_setup(head: &[u8; 8], body: &[u8]) -> Option<u32> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(body.get(at..at + 2)?.try_into().ok()?) as usize);
    if head[0] != 1 {
        return None;
    }
    // after the fixed part: the vendor string padded to 4, 8 bytes per pixmap format, then the first screen's root
    let at = 32 + u16_at(16)?.next_multiple_of(4) + *body.get(21)? as usize * 8;
    Some(u32::from_le_bytes(body.get(at..at + 4)?.try_into().ok()?))
}

/// InternAtom, only if it exists.
fn intern_request(name: &str) -> Vec<u8> {
    let padded = name.len().next_multiple_of(4);
    let mut r = vec![16, 1];
    r.extend(((2 + padded / 4) as u16).to_le_bytes());
    r.extend((name.len() as u16).to_le_bytes());
    r.extend([0, 0]);
    r.extend(name.as_bytes());
    r.resize(8 + padded, 0);
    r
}

/// The atom in an InternAtom reply. None for an error, or an atom that doesn't exist.
fn parse_atom(reply: &[u8; 32]) -> Option<u32> {
    let atom = u32::from_le_bytes(reply[8..12].try_into().unwrap());
    (reply[0] == 1 && atom != 0).then_some(atom)
}

/// GetProperty of any type, from the start, one 32-bit unit.
fn property_request(window: u32, atom: u32) -> [u8; 24] {
    let mut r = [0u8; 24];
    r[0] = 20;
    r[2..4].copy_from_slice(&6u16.to_le_bytes());
    r[4..8].copy_from_slice(&window.to_le_bytes());
    r[8..12].copy_from_slice(&atom.to_le_bytes());
    r[20..24].copy_from_slice(&1u32.to_le_bytes());
    r
}

/// The number in a GetProperty reply (its 32-byte head, then `length` units of data), if it holds
/// one. An empty property, which is what no focus looks like, has none.
fn parse_property(head: &[u8; 32], data: &[u8]) -> Option<u32> {
    let len = u32::from_le_bytes(head[16..20].try_into().unwrap());
    if head[0] == 1 && head[1] == 32 && len >= 1 { Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?)) } else { None }
}

fn read_n<const N: usize>(s: &mut UnixStream) -> Result<[u8; N], ()> {
    let mut b = [0u8; N];
    s.read_exact(&mut b).map_err(|_| ())?;
    Ok(b)
}

/// Reads n bytes, refusing more than `most`: n comes from the X server's reply, so a broken or
/// hostile one can't make us allocate gigabytes (RESEXHAUST-003).
fn read_vec(s: &mut UnixStream, n: usize, most: usize) -> Result<Vec<u8>, ()> {
    if n > most {
        return Err(());
    }
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).map_err(|_| ())?;
    Ok(b)
}

/// GAMESCOPE_FOCUSED_APP on the X server at `socket`. Ok(None) means it's empty or isn't set (no
/// game open), and Err that the server couldn't be read at all.
fn focused_app_at(socket: &Path) -> Result<Option<u32>, ()> {
    let mut s = UnixStream::connect(socket).map_err(|_| ())?;
    let t = Some(Duration::from_millis(200)); // a server that stops answering mustn't hang the thread
    s.set_read_timeout(t).and_then(|_| s.set_write_timeout(t)).map_err(|_| ())?;
    s.write_all(&setup_request()).map_err(|_| ())?;
    let head: [u8; 8] = read_n(&mut s)?;
    let body = read_vec(&mut s, u16::from_le_bytes([head[6], head[7]]) as usize * 4, 1 << 18)?; // a u16 of units is 256 KiB at most anyway
    let root = parse_setup(&head, &body).ok_or(())?;
    s.write_all(&intern_request("GAMESCOPE_FOCUSED_APP")).map_err(|_| ())?;
    let reply: [u8; 32] = read_n(&mut s)?;
    if reply[0] != 1 {
        return Err(());
    }
    let Some(atom) = parse_atom(&reply) else { return Ok(None) }; // gamescope never set it
    s.write_all(&property_request(root, atom)).map_err(|_| ())?;
    let head: [u8; 32] = read_n(&mut s)?;
    if head[0] != 1 {
        return Err(());
    }
    // We asked for one unit (property_request), so more than a few is a server not doing as asked.
    let data = read_vec(&mut s, (u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize).saturating_mul(4), 16)?;
    Ok(parse_property(&head, &data))
}

// ------------------------------------------------------------------ the hand-off

/// Who has the mouse, the keyboard and the room: Command Center, or a game. The main loop calls
/// `step` every tick and then asks what to do.
#[derive(Default)]
pub struct Handoff {
    pref_steam: bool, // "Steam / game" chosen on the tab with no game running
    vr: bool,         // a VR game is running
    flat: bool,       // a 2D game is playing (and no VR one)
    back: bool,       // you took Command Center back from the 2D game
    selected: bool,   // our tab was the dashboard's active one last tick
    hid: bool,        // the panels are hidden because of a game
}

impl Handoff {
    /// This tick's facts: a VR game is running, a 2D one is playing, our tab is selected.
    pub fn step(&mut self, vr: bool, flat: bool, selected: bool) {
        let flat = flat && !vr; // a VR game outranks it
        if !flat || !self.flat {
            self.back = false; // no game, or a new one: it gets the input
        }
        // Picking the tab takes it back. Only the moment it's picked counts, so pressing Steam /
        // game with the tab still open sticks.
        if flat && selected && !self.selected {
            self.back = true;
        }
        (self.vr, self.flat, self.selected) = (vr, flat, selected);
    }

    /// Command Center steps aside: the panels hide, and the keyboards and mice go to the game.
    pub fn playing(&self) -> bool {
        self.vr || (self.flat && !self.back)
    }

    /// Whether the keyboards and mice are the Frame's (kvm.rs `set_yielded`), which is also what
    /// the tab shows as chosen.
    pub fn yielded(&self) -> bool {
        self.playing() || self.pref_steam
    }

    /// A choice pressed on the tab: Steam / game (true) or Command Center (false). In a 2D game
    /// that gives it back or takes it. Otherwise it's a preference that lasts until changed. It
    /// doesn't outlast the game, so the next one isn't handed the input by an old press.
    pub fn press(&mut self, steam: bool) {
        if self.flat {
            self.back = !steam;
        } else {
            self.pref_steam = steam;
        }
    }

    /// What the panels should do now: Some(false) to hide them for a game, Some(true) to show what
    /// the game hid. `hidden` is whether they're hidden at all. Panels you hid yourself stay hidden.
    pub fn panels(&mut self, hidden: bool) -> Option<bool> {
        if self.playing() && !self.hid && !hidden {
            self.hid = true;
            Some(false)
        } else if !self.playing() && self.hid {
            self.hid = false;
            Some(true)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply length bigger than we'd take is refused before anything is allocated.
    #[test]
    fn huge_x_replies_are_refused() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        std::io::Write::write_all(&mut b, &[1u8; 16]).unwrap();
        assert!(read_vec(&mut a, 16 << 30, 16).is_err(), "16 GiB asked for");
        assert_eq!(read_vec(&mut a, 16, 16), Ok(vec![1u8; 16]));
    }
    use std::os::unix::net::UnixListener;

    #[test]
    fn gamescope_says_what_is_playing() {
        let no = || -> bool { panic!("the launcher isn't asked when gamescope says") };
        assert!(decide(false, Ok(Some(2825598531)), no), "a real app id");
        assert!(!decide(false, Ok(Some(0)), || false), "0 is nothing");
        assert!(!decide(false, Ok(Some(STEAM_UI)), || true), "Steam's own UI isn't a game, and none was playing");
        assert!(!decide(false, Ok(None), || true), "nothing focused and no game before: the launcher doesn't start one");
        assert!(decide(true, Ok(None), || true), "focus cleared but the game's process is still there");
        assert!(decide(true, Ok(Some(STEAM_UI)), || true), "Steam's UI over a running game");
        assert!(!decide(true, Ok(None), || false), "and gone: it ended");
        assert!(decide(false, Err(()), || true), "gamescope unreadable: the launcher says");
        assert!(!decide(true, Err(()), || false));
    }

    #[test]
    fn it_finds_the_app_in_steams_launcher_command_line() {
        assert_eq!(reaper_app(b"/home/u/.steam/ubuntu12_32/reaper\0SteamLaunch\0AppId=2825598531\0--\0/bin/game\0"), Some(2825598531));
        assert_eq!(reaper_app(b"reaper\0SteamLaunch\0AppId=769"), Some(769));
        assert_eq!(reaper_app(b"reaper\0SteamLaunch\0--\0AppId=5"), None, "AppId comes right after");
        assert_eq!(reaper_app(b"steam\0SteamLaunch\0AppId=5\0"), None, "not the reaper");
        assert_eq!(reaper_app(b"reaper\0other\0AppId=5\0"), None);
        assert_eq!(reaper_app(b"reaper\0SteamLaunch\0AppId=x\0"), None);
        assert_eq!(reaper_app(b""), None);
    }

    #[test]
    fn the_x_requests_are_what_the_protocol_says() {
        assert_eq!(setup_request(), [b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // InternAtom: opcode 16, only-if-exists, 2 + padded name / 4 units, the name length, the name padded to 4
        let r = intern_request("GAMESCOPE_FOCUSED_APP"); // 21 characters, padded to 24
        assert_eq!(r.len(), 32);
        assert_eq!(&r[..8], &[16, 1, 8, 0, 21, 0, 0, 0]);
        assert_eq!(&r[8..29], b"GAMESCOPE_FOCUSED_APP");
        assert_eq!(&r[29..], &[0, 0, 0]);
        assert_eq!(intern_request("ABCD").len(), 12, "a name that already fits");
        // GetProperty: opcode 20, 6 units, window, property, any type, from 0, one unit
        let r = property_request(0x0102_0304, 0x0a0b_0c0d);
        assert_eq!(&r[..4], &[20, 0, 6, 0]);
        assert_eq!(&r[4..8], &[4, 3, 2, 1]);
        assert_eq!(&r[8..12], &[0x0d, 0x0c, 0x0b, 0x0a]);
        assert_eq!(&r[12..20], &[0; 8]);
        assert_eq!(&r[20..], &[1, 0, 0, 0]);
    }

    /// A setup reply body: a vendor of `vendor` bytes, `formats` pixmap formats and one screen at `root`.
    fn setup_body(vendor: usize, formats: usize, root: u32) -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[16..18].copy_from_slice(&(vendor as u16).to_le_bytes());
        b[20] = 1;
        b[21] = formats as u8;
        b.resize(32 + vendor.next_multiple_of(4) + formats * 8, 1);
        b.extend(root.to_le_bytes());
        b
    }

    #[test]
    fn it_finds_the_root_window_past_the_vendor_and_formats() {
        let ok = [1, 0, 11, 0, 0, 0, 0, 0];
        assert_eq!(parse_setup(&ok, &setup_body(11, 7, 0x0000_0777)), Some(0x777), "an 11-letter vendor pads to 12");
        assert_eq!(parse_setup(&ok, &setup_body(8, 0, 9)), Some(9), "already a multiple of 4, no formats");
        assert_eq!(parse_setup(&[0, 5, 11, 0, 0, 0, 0, 0], &setup_body(8, 0, 9)), None, "refused");
        assert_eq!(parse_setup(&ok, &setup_body(8, 0, 9)[..20]), None, "cut short");
    }

    fn property(format: u8, len: u32, value: u32) -> ([u8; 32], Vec<u8>) {
        let mut h = [0u8; 32];
        (h[0], h[1]) = (1, format);
        h[4..8].copy_from_slice(&(len.min(1)).to_le_bytes());
        h[16..20].copy_from_slice(&len.to_le_bytes());
        (h, if len > 0 { value.to_le_bytes().to_vec() } else { vec![] })
    }

    #[test]
    fn it_reads_the_number_or_nothing() {
        let (h, d) = property(32, 1, 2825598531);
        assert_eq!(parse_property(&h, &d), Some(2825598531));
        let (h, d) = property(32, 0, 0);
        assert_eq!(parse_property(&h, &d), None, "empty: no game");
        let (h, d) = property(8, 1, 5);
        assert_eq!(parse_property(&h, &d), None, "not 32-bit");
        let (mut h, d) = property(32, 1, 5);
        h[0] = 0;
        assert_eq!(parse_property(&h, &d), None, "an error packet");
        let (h, _) = property(32, 1, 5);
        assert_eq!(parse_property(&h, &[1, 2]), None, "data cut short");
        let mut a = [0u8; 32];
        (a[0], a[8]) = (1, 77);
        assert_eq!(parse_atom(&a), Some(77));
        a[8] = 0;
        assert_eq!(parse_atom(&a), None, "no such atom");
    }

    /// A server that answers the three requests with `value` (None: an empty property), on a
    /// socket in a throwaway folder. Returns the socket and what the client sent.
    fn serve(value: Option<u32>, tag: &str) -> (std::path::PathBuf, std::thread::JoinHandle<Vec<Vec<u8>>>) {
        let dir = std::env::temp_dir().join(format!("cc-handoff-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("X0");
        let _ = std::fs::remove_file(&path);
        let lis = UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = lis.accept().unwrap();
            let mut got = vec![];
            fn take(c: &mut UnixStream, n: usize) -> Vec<u8> {
                let mut b = vec![0u8; n];
                c.read_exact(&mut b).unwrap();
                b
            }
            got.push(take(&mut c, 12));
            let body = setup_body(11, 2, 0x777);
            let mut head = vec![1, 0, 11, 0, 0, 0];
            head.extend(((body.len() / 4) as u16).to_le_bytes());
            c.write_all(&[head, body].concat()).unwrap();
            got.push(take(&mut c, 32)); // InternAtom for a 21-letter name
            let mut atom = vec![0u8; 32];
            (atom[0], atom[8]) = (1, 99);
            c.write_all(&atom).unwrap();
            got.push(take(&mut c, 24));
            let (h, d) = property(32, value.is_some() as u32, value.unwrap_or(0));
            c.write_all(&[h.to_vec(), d].concat()).unwrap();
            got
        });
        (path, t)
    }

    #[test]
    fn it_talks_to_a_server_end_to_end() {
        let (path, t) = serve(Some(2825598531), "full");
        assert_eq!(focused_app_at(&path), Ok(Some(2825598531)));
        let got = t.join().unwrap();
        assert_eq!(got[0], setup_request());
        assert_eq!(got[1], intern_request("GAMESCOPE_FOCUSED_APP"));
        assert_eq!(got[2], property_request(0x777, 99), "the root window from setup, the atom from InternAtom");
        let (path, t) = serve(None, "empty");
        assert_eq!(focused_app_at(&path), Ok(None));
        t.join().unwrap();
        assert_eq!(focused_app_at(Path::new("/nonexistent/X9")), Err(()), "no server");
    }

    /// Against the real thing: `cargo test -p cc-panels live_gamescope -- --ignored --nocapture`.
    /// It only reads the property.
    #[test]
    #[ignore]
    fn live_gamescope() {
        let r = focused_app_at(Path::new(GAMESCOPE_X));
        println!("GAMESCOPE_FOCUSED_APP on {GAMESCOPE_X}: {r:?}");
        assert!(r.is_ok(), "the server couldn't be read");
    }

    #[test]
    fn nothing_happens_without_a_game() {
        let mut h = Handoff::default();
        h.step(false, false, false);
        assert!(!h.playing() && !h.yielded());
        assert_eq!(h.panels(false), None);
        h.press(true);
        assert!(h.yielded() && !h.playing(), "Steam / game with no game: the input goes, the panels stay");
        assert_eq!(h.panels(false), None);
        h.press(false);
        assert!(!h.yielded());
    }

    #[test]
    fn a_2d_game_takes_the_input_and_gives_it_back_when_it_ends() {
        let mut h = Handoff::default();
        h.step(false, true, false);
        assert!(h.playing() && h.yielded());
        assert_eq!(h.panels(false), Some(false), "the panels hide");
        assert_eq!(h.panels(true), None, "once");
        h.step(false, false, false);
        assert!(!h.playing() && !h.yielded());
        assert_eq!(h.panels(true), Some(true), "and come back");
        assert_eq!(h.panels(false), None);
    }

    #[test]
    fn picking_the_tab_takes_it_back_and_steam_hands_it_to_the_game_again() {
        let mut h = Handoff::default();
        h.step(false, true, false);
        h.panels(false);
        h.step(false, true, true); // the tab gets picked
        assert!(!h.playing() && !h.yielded(), "Command Center has it");
        assert_eq!(h.panels(true), Some(true), "panels show");
        h.step(false, true, true); // still picked
        assert!(!h.playing(), "stays");
        h.press(true); // Steam / game, with the tab still open
        h.step(false, true, true);
        assert!(h.playing() && h.yielded(), "the game has it, and the tab being open doesn't take it again");
        assert_eq!(h.panels(false), Some(false));
        h.step(false, true, false); // the dashboard closes
        h.step(false, true, true); // and opens on our tab again
        assert!(!h.playing(), "picked again: back");
    }

    #[test]
    fn the_tabs_choice_works_both_ways_during_a_game() {
        let mut h = Handoff::default();
        h.step(false, true, false);
        h.press(false);
        assert!(!h.playing() && !h.yielded(), "Command Center pressed: back, without picking the tab");
        h.press(true);
        assert!(h.playing() && h.yielded(), "Steam / game: the game's");
        h.press(true);
        assert!(h.playing(), "pressing it twice changes nothing");
    }

    #[test]
    fn a_press_in_a_game_does_not_outlast_it_and_a_new_game_starts_with_the_input() {
        let mut h = Handoff::default();
        h.step(false, true, false);
        h.press(true);
        h.step(false, false, false);
        assert!(!h.yielded(), "the game ended: the press doesn't stay as a preference");
        h.step(false, true, false);
        h.press(false);
        assert!(!h.playing());
        h.step(false, false, false);
        h.step(false, true, false);
        assert!(h.playing(), "the next game: it has the input even though the last was taken back");
        h.step(false, false, true); // the dashboard is on our tab, with no game
        h.step(false, true, true); // and a game starts
        assert!(h.playing(), "no edge, so it isn't taken");
    }

    #[test]
    fn panels_you_hid_yourself_stay_hidden() {
        let mut h = Handoff::default();
        h.step(false, true, false);
        assert_eq!(h.panels(true), None, "already hidden by you: the game didn't hide them");
        h.step(false, false, false);
        assert_eq!(h.panels(true), None, "so nothing comes back");
    }

    #[test]
    fn a_vr_game_comes_first() {
        let mut h = Handoff::default();
        h.step(true, true, true); // a VR game running, and the tab picked
        assert!(h.playing() && h.yielded(), "the tab can't take it from a VR game");
        assert_eq!(h.panels(false), Some(false));
        h.step(false, false, false);
        assert_eq!(h.panels(true), Some(true));
        h.step(false, true, false);
        h.step(true, true, false);
        h.press(false);
        assert!(h.playing(), "a press in a VR game isn't a 2D game's back");
    }
}
