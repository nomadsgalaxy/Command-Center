//! The Machines window (docs/config-window.md, screen 1). Every remote machine in viewers.conf
//! gets a row: its connected dot, name and host, then [Connect]/[Disconnect], [Auto-connect]
//! (viewers.conf's autoconnect=, used on the next Desktop launch), [Align] (the camera scan) and
//! [x], which removes it on the second click. [Add machine] under the rows opens a form (address,
//! monitor, size, name) you type on SteamVR's keyboard. Add runs `cc-home machine add` and fills a
//! spare remote slot (main.rs SPARES), so the new machine shows up and connects without a
//! restart. A status line says how cc-home's runs went (the last line of its error).
//!
//! The taskbar's Machines chip and `machines show|hide` open and close it. It's two overlays,
//! `controlcenter.machines` and its card (grab.rs `Extra`), so it's carried, resized, bent and
//! closed like a panel. Both are made when it opens and destroyed when it closes, since SteamVR's
//! 128 overlays are shared by every app. It opens where it was last put down (spots.home.machines),
//! otherwise OUT ahead of the eyes, facing them. It sits nearer than the panels because the Frame
//! draws by depth, not sort order. It uses the cards' colours (theme.rs `win`) and the taskbar's
//! units (a Breeze logical pixel). Its labels are assets.rs's tags (LABELS), and typed text comes
//! from its ascii.rgba (monospace cells, drawn a character at a time).
//!
//! cc-home writes viewers.conf (`machine set|add|remove`). Those are plain file edits, so they run
//! here in the container. Align runs `cc-home machine align` on the host (distrobox-host-exec)
//! because the scan needs podman and the container doesn't have it. The title bar's Fast/Precise
//! switch is settings.json's `align`, which both cc-home and the scan read. When an align prints
//! `@reanchor <m> mm=N deg=N` (a monitor moved a lot), the window offers [Move everything with it]:
//! that runs `cc-home reanchor <m>`, then recalls every placement from the moved spots. A
//! relocalization (StandingZeroPoseReset, main.rs) shows up in the status.
//!
//! The form also searches for hosts announcing themselves (`cc-home machine discover`, on the host
//! since avahi-browse isn't in cc-box) and lists each with [Pair]. You type the key the host shows
//! (`cc-share pair`) and the pairing runs here in Rust (cc_proto: docs/pairing.md, the same as
//! `cc-home machine pair`). Its @pair lines go to the status, and @paired's monitors fill spare
//! slots the way Add's do. The key is never logged. If a machine was unpaired while its host was
//! away, its row says the host still trusts this Frame (pending-unpair/); running
//! `cc-home machine unpair` again tells it.
//!
//! A clicked field gets the physical keyboards too (kvm.rs focus_field): their first key closes
//! SteamVR's keyboard, Enter submits, Tab goes to the next field and Escape leaves it.
//!
//! Clicking a row's name renames it (D-050). That row turns into one field holding its label, plus
//! Machine / This monitor when its machine has 2 or more monitors or the monitor has its own label,
//! and [Save]. Save (or Enter) runs `cc-home machine rename` here (a file edit) and redraws its
//! tags with the new text. An empty label clears it, and Escape or its x cancels. Names (spots,
//! logs, overlays) never change.
//!
//! It's also a page of a workspace (the Workspace window's Machines, control/workspace.rs): the
//! title names that workspace, [Back] goes back there, and each row's [In workspace] switch puts
//! that row's machine (all its monitors) in or out of it. That's home.json's per-workspace
//! "machines" (cc_proto::conf::set_member), and no list means every machine. Machines stay paired
//! for every workspace, but only the active one's get panels and chips, so flipping the switch on
//! the active workspace applies right away (workspace::switched). Auto-connect, align, rename and
//! pairing work the same as before.
//!
//! ponytail: the mouse's cursor doesn't land on it (SteamVR's laser does), a keystroke redraws the
//! whole thing, and Align's @progress lines only go to the log.
use super::ui::{self, BTN_H, Edit, GAP, MPP, PAD, ROW, Rect, SCALE, SORT, TITLE, switch, text, text_w, typed, typed_w};
use crate::grab::{self, Look, Paint, TagImg, line, over, tint};
use crate::geometry::{Placement, panel_matrix};
use crate::kvm::KVM;
use crate::taskbar::{Taskbar, tex_scale};
use crate::windows::Windows;
use crate::{HIDDEN, Source, call, config, panel, panels, theme, vr};
use openvr_sys as sys;
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, mpsc};

/// Whether it's wanted open. The chip, `machines show|hide` and its card's close set it, and tick follows.
pub static OPEN: AtomicBool = AtomicBool::new(false);

/// Its labels. assets.rs draws them at start as tag-ui-<key>.rgba, so no commas in the text.
pub const LABELS: [(&str, &str); 25] = [
    ("machines", "Machines"),
    ("connect", "Connect"),
    ("disconnect", "Disconnect"),
    ("auto", "Auto-connect"),
    ("align", "Align"),
    ("scan", "use cc-home scan"),
    ("aligning", "Aligning..."),
    ("new", "Add machine"),
    ("address", "Address"),
    ("monitor", "Monitor"),
    ("size", "Size"),
    ("name", "Name"),
    ("cancel", "Cancel"),
    ("add", "Add"),
    ("fast", "Fast"),
    ("precise", "Precise"),
    ("reanchor", "Move everything with it"),
    ("pair", "Pair"),
    ("refresh", "Refresh"),
    ("replace", "Replace"),
    ("key", "Key"),
    ("look", "Pair by looking"),
    ("machine", "Machine"),
    ("thismonitor", "This monitor"),
    ("save", "Save"),
];

const W: f64 = 880.0; // in units: the taskbar's, a Breeze logical pixel
const NAME: f64 = PAD + 24.0; // the name's column, with the dot before it
const HOST: f64 = 172.0; // the host's column, up to the buttons; also a form field's box
const BUTTONS: [f64; 4] = [104.0, 130.0, 120.0, 28.0]; // Connect, Auto-connect, Align, x (remove)
const NEW: f64 = 150.0; // [Add machine]
const FOOT: [f64; 3] = [104.0, 104.0, 130.0]; // the form's Cancel, Add (or Pair, Replace), and pairing's Pair by looking
const PRECISE: f64 = 140.0; // the title bar's Fast/Precise switch
const OFFER: f64 = 180.0; // [Move everything with it], at the foot's right
const FIELDS: usize = 4; // the form's fields: Address, Monitor, Size, Name
const HINTS: [&str; FIELDS + 1] = ["Address: user@host", "Monitor: 0 is the first", "Size: WxH", "Name", "Key: the 6 digits on the host"]; // SteamVR's keyboard's hints (the last one is pairing's)
const PAIR: f64 = 104.0; // a found host's [Pair], and the search row's [Refresh]
const HOSTS: usize = 6; // at most this many found hosts listed
const RENAME: [f64; 3] = [240.0, 104.0, 28.0]; // a renamed row's Machine / This monitor, Save, and x (cancel)
const OSK_RENAME: usize = FIELDS + 1; // SteamVR's keyboard's user value for a rename's field
const MEMBER: f64 = 130.0; // a row's [In workspace] switch, left of the buttons
const BACK: f64 = 90.0; // the title bar's [Back], left of Fast/Precise

/// The workspace the next opening shows (None: the active one). The Workspace window's Machines sets it.
static SCOPE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Sets the workspace its next opening is for (None: the active one).
pub fn scope(ws: Option<&str>) {
    *SCOPE.lock().unwrap() = ws.map(str::to_owned);
}

/// The form's shape: closed, found hosts above the fields, or pairing (with Replace offered or not).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Shape {
    Closed,
    Hosts(usize),
    Pairing(bool),
}

fn shape(f: Option<&Form>) -> Shape {
    match f {
        None => Shape::Closed,
        Some(Form { pairing: Some((_, r)), .. }) => Shape::Pairing(*r),
        Some(f) => Shape::Hosts(f.hosts.len()),
    }
}

/// What a laser is on.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Connect(usize), // a row
    Auto(usize),
    Align(usize),
    Remove(usize),
    New,          // [Add machine]
    Field(usize), // a form field
    Cancel,
    Add,
    Precise,  // the title bar's Fast/Precise switch
    Reanchor, // [Move everything with it]
    Refresh,  // the search row's: search again
    Host(usize), // a found host's [Pair]
    Key,      // pairing's key field
    Pair,     // pairing's [Pair]
    Replace,  // pairing's [Replace] (the host changed)
    Look,     // pairing's [Pair by looking]: the camera reads the key off the host's screen
    Name(usize),         // a row's name: click to rename it
    Label(usize),        // a renamed row's field
    Scope(usize, usize), // its Machine (0) / This monitor (1)
    Save(usize),
    Unrename(usize), // its x: cancel
    Member(usize),   // a row's [In workspace]
    Back,            // the title bar's [Back], to the Workspace window
}

/// The row under the machines' rows and the form (if it's open). It holds [Add machine] or the
/// form's buttons, plus the status. The form is the search row, a row per found host and the
/// fields; or, while pairing, two lines and the key.
fn foot(n: usize, s: Shape) -> usize {
    n + match s {
        Shape::Closed => 0,
        Shape::Hosts(h) => 1 + h + FIELDS,
        Shape::Pairing(_) => 3,
    }
}

/// The foot's button widths: Cancel and the submit, then pairing's Look.
fn foot_w(s: Shape) -> &'static [f64] {
    if matches!(s, Shape::Pairing(_)) { &FOOT } else { &FOOT[..2] }
}

/// Its height in units with n rows and the form in shape s.
fn height(n: usize, s: Shape) -> f64 {
    TITLE + (foot(n, s) + 1) as f64 * ROW + PAD
}

/// How many rows the found hosts take.
fn found(s: Shape) -> usize {
    if let Shape::Hosts(h) = s { h } else { 0 }
}

/// Button k's span (x0, x1) for widths b, right-aligned and GAP apart.
fn right(b: &[f64], k: usize) -> (f64, f64) {
    ui::right(W - PAD, b, k)
}

fn button_x(k: usize) -> (f64, f64) {
    right(&BUTTONS, k)
}

/// A row's [In workspace] span.
fn member_x() -> (f64, f64) {
    (button_x(0).0 - GAP - MEMBER, button_x(0).0 - GAP)
}

/// The middle of row i.
fn row_y(i: usize) -> f64 {
    TITLE + (i as f64 + 0.5) * ROW
}

/// What's at (x, y), in units from the top left, with n rows, the form in shape s, and an offer or not.
fn hit(n: usize, s: Shape, offer: bool, x: f64, y: f64) -> Option<Hit> {
    let on = |(x0, x1): (f64, f64)| (x0..=x1).contains(&x);
    if y < TITLE {
        if (y - TITLE / 2.0).abs() > BTN_H / 2.0 {
            return None;
        }
        return if on(right(&[PRECISE], 0)) { Some(Hit::Precise) } else { on(right(&[BACK, PRECISE], 0)).then_some(Hit::Back) };
    }
    let i = ((y - TITLE) / ROW) as usize;
    if i > foot(n, s) || (y - row_y(i)).abs() > BTN_H / 2.0 {
        return None;
    }
    if i < n && on((PAD, HOST - GAP)) {
        return Some(Hit::Name(i));
    }
    if i < n && on(member_x()) {
        return Some(Hit::Member(i));
    }
    if i < n {
        return match (0..BUTTONS.len()).find(|&k| on(button_x(k)))? {
            0 => Some(Hit::Connect(i)),
            1 => Some(Hit::Auto(i)),
            2 => Some(Hit::Align(i)),
            _ => Some(Hit::Remove(i)),
        };
    }
    let j = i - n;
    match s {
        Shape::Hosts(_) if j == 0 => return on(right(&[PAIR], 0)).then_some(Hit::Refresh),
        Shape::Hosts(h) if j <= h => return on(right(&[PAIR], 0)).then_some(Hit::Host(j - 1)),
        Shape::Hosts(h) if j <= h + FIELDS => return on((HOST, W - PAD)).then_some(Hit::Field(j - 1 - h)),
        Shape::Pairing(_) if j < 2 => return None,
        Shape::Pairing(_) if j == 2 => return on((HOST, W - PAD)).then_some(Hit::Key),
        Shape::Closed if offer && on(right(&[OFFER], 0)) => return Some(Hit::Reanchor),
        Shape::Closed => return on((PAD, PAD + NEW)).then_some(Hit::New),
        _ => {}
    }
    let b = foot_w(s);
    [Hit::Cancel, submit(s), Hit::Look].into_iter().take(b.len()).enumerate().find(|&(k, _)| on(right(b, k))).map(|(_, h)| h)
}

/// The form's right foot button: Add, Pair, or Replace once the host changed.
fn submit(s: Shape) -> Hit {
    match s {
        Shape::Pairing(false) => Hit::Pair,
        Shape::Pairing(true) => Hit::Replace,
        _ => Hit::Add,
    }
}

/// The area hit a's look covers (x0, y0, x1, y1): its button plus the room around it that pixel() gives it.
fn rect(n: usize, s: Shape, a: Hit) -> Rect {
    let ((x0, x1), i) = match a {
        Hit::Precise | Hit::Back => {
            let (x0, x1) = if a == Hit::Back { right(&[BACK, PRECISE], 0) } else { right(&[PRECISE], 0) };
            return (x0 - 8.0, 0.0, x1 + 8.0, TITLE);
        }
        Hit::Member(i) => (member_x(), i),
        Hit::Reanchor => (right(&[OFFER], 0), foot(n, s)),
        Hit::Connect(i) => (button_x(0), i),
        Hit::Auto(i) => (button_x(1), i),
        Hit::Align(i) => (button_x(2), i),
        Hit::Remove(i) => (button_x(3), i),
        Hit::New => ((PAD, PAD + NEW), foot(n, s)),
        Hit::Refresh => (right(&[PAIR], 0), n),
        Hit::Host(k) => (right(&[PAIR], 0), n + 1 + k),
        Hit::Field(f) => ((HOST, W - PAD), n + 1 + found(s) + f),
        Hit::Key => ((HOST, W - PAD), n + 2),
        Hit::Cancel => (right(foot_w(s), 0), foot(n, s)),
        Hit::Add | Hit::Pair | Hit::Replace => (right(foot_w(s), 1), foot(n, s)),
        Hit::Look => (right(&FOOT, 2), foot(n, s)),
        Hit::Name(i) => ((PAD, HOST - GAP), i),
        Hit::Label(i) => (rename_field(), i),
        Hit::Scope(i, _) => (right(&RENAME, 0), i),
        Hit::Save(i) => (right(&RENAME, 1), i),
        Hit::Unrename(i) => (right(&RENAME, 2), i),
    };
    let cy = row_y(i);
    (x0 - 8.0, cy - ROW / 2.0, x1 + 8.0, cy + ROW / 2.0)
}

/// What a laser at (x, y) is on: a button, or else the one it was already on while it's still
/// nearby. The gaps between rows and buttons don't drop it, so there's no flicker and a press that
/// slips still lands.
fn held(was: Option<Hit>, n: usize, s: Shape, offer: bool, x: f64, y: f64) -> Option<Hit> {
    let still = |a: Hit| match a {
        Hit::Connect(i) | Hit::Auto(i) | Hit::Align(i) | Hit::Remove(i) | Hit::Name(i) | Hit::Member(i) => i < n, // rows gone: act() would index past them
        Hit::Host(k) => k < found(s),
        Hit::Field(_) | Hit::Refresh => matches!(s, Shape::Hosts(_)),
        Hit::Key | Hit::Look => matches!(s, Shape::Pairing(_)),
        Hit::Cancel => s != Shape::Closed,
        Hit::Add | Hit::Pair | Hit::Replace => s != Shape::Closed && a == submit(s),
        _ => true,
    };
    hit(n, s, offer, x, y).or(was.filter(|&a| still(a) && {
        let (x0, y0, x1, y1) = rect(n, s, a);
        (x0..=x1).contains(&x) && (y0..=y1).contains(&y)
    }))
}

/// A renamed row's field span.
fn rename_field() -> (f64, f64) {
    (NAME - 4.0, right(&RENAME, 0).0 - GAP)
}

/// What's at (x, y) on row r while it's being renamed (choice: Machine / This monitor shown). None
/// off that row. On it: its field, choice, Save or x, or nothing. Never the row's own buttons.
fn rename_hit(r: usize, choice: bool, x: f64, y: f64) -> Option<Option<Hit>> {
    if (y - row_y(r)).abs() > ROW / 2.0 {
        return None;
    }
    let on = |(x0, x1): (f64, f64)| (x0..=x1).contains(&x);
    Some(if on(rename_field()) {
        Some(Hit::Label(r))
    } else if choice && let Some(k) = ui::segment(2, right(&RENAME, 0), x) {
        Some(Hit::Scope(r, k))
    } else if on(right(&RENAME, 1)) {
        Some(Hit::Save(r))
    } else {
        on(right(&RENAME, 2)).then_some(Hit::Unrename(r))
    })
}

/// All of row i.
fn whole_row(i: usize) -> Rect {
    (0.0, row_y(i) - ROW / 2.0, W, row_y(i) + ROW / 2.0)
}

/// What to redraw going from a to b. None means all of it (a new shape or theme).
fn dirty(a: &Key, b: &Key) -> Option<Vec<Rect>> {
    let (n, s) = (b.rows.len(), shape(b.form.as_ref()));
    if a.theme != b.theme || a.rows.len() != n || shape(a.form.as_ref()) != s || a.scope != b.scope {
        return None;
    }
    let at = |a: Hit| rect(n, s, a);
    let mut r: Vec<Rect> = if a.hover != b.hover { [a.hover, b.hover].into_iter().flatten().map(at).collect() } else { Vec::new() };
    r.extend((0..n).filter(|&i| a.rows[i] != b.rows[i]).map(whole_row));
    if (a.aligning, a.no_align) != (b.aligning, b.no_align) {
        r.extend((0..n).map(|i| at(Hit::Align(i))));
    }
    if a.armed != b.armed {
        r.extend([a.armed, b.armed].into_iter().flatten().map(|i| at(Hit::Remove(i))));
    }
    if a.form != b.form {
        r.extend((n..foot(n, s)).map(whole_row)); // a field's text, and Name's placeholder, which comes from the others
    }
    if (&a.rename, a.tags) != (&b.rename, b.tags) {
        r.extend((0..n).map(whole_row)); // a rename's row and the one it was on; the names get redrawn
    }
    if (&a.status, &a.offer) != (&b.status, &b.offer) {
        r.push(whole_row(foot(n, s)));
    }
    if a.precise != b.precise {
        r.push(at(Hit::Precise));
    }
    Some(r)
}

/// The name Add gives when none is typed: the host's first label (or every part of an address),
/// plus the monitor after it when it isn't the first (desk, desk-1; 203-0-113-63).
fn default_name(addr: &str, mon: &str) -> String {
    let host = addr.rsplit_once('@').map_or(addr, |a| a.1).trim();
    let base = if host.split('.').all(|p| p.chars().all(|c| c.is_ascii_digit())) { host.replace('.', "-") } else { host.split('.').next().unwrap_or("").into() };
    match mon.trim() {
        "" | "0" => base,
        m => format!("{base}-{m}"),
    }
}

/// The form's fields (Address, Monitor, Size, Name) turned into cc-home machine add's arguments:
/// name, user, host, port (krdp's 3400 + the monitor) and WxH. Otherwise, what's wrong. It checks
/// the way cc-home does so most mistakes show up before it runs.
fn add_args(f: &[String; FIELDS]) -> Result<(String, String, String, u32, String), String> {
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || "_.-".contains(c));
    let (addr, mon, size) = (f[0].trim(), f[1].trim(), f[2].trim());
    let mon = if mon.is_empty() { "0" } else { mon }; // the placeholder. An empty size is fine: cc-home asks the machine (machine probe)
    let Some((user, host)) = addr.split_once('@').filter(|(u, h)| word(u) && word(h)) else {
        return Err("address: user@host".into());
    };
    let Some(n) = mon.parse::<u32>().ok().filter(|&n| n < 100) else {
        return Err("monitor: 0 is the first, 1 the second...".into());
    };
    let ok = |(w, h): (&str, &str)| w.parse::<u32>().is_ok_and(|w| w > 0) && h.parse::<u32>().is_ok_and(|h| h > 0);
    if !size.is_empty() && !size.split_once('x').is_some_and(ok) {
        return Err("size: WxH, as 1920x1080, or empty to ask the machine".into());
    }
    let name = if f[3].trim().is_empty() { default_name(addr, mon) } else { f[3].trim().to_string() };
    if !word(&name) {
        return Err("name: letters, digits, . - _".into());
    }
    Ok((name, user.into(), host.into(), 3400 + n, size.to_string()))
}

/// GetKeyboardText's buffer as text: up to its NUL, printable ASCII only, since that's what ascii.rgba draws.
fn keyboard_text(buf: &[u8]) -> String {
    buf.iter().take_while(|&&b| b != 0).filter(|b| (32..127).contains(*b)).map(|&b| b as char).collect()
}

/// GetKeyboardText's buffer as a label: up to its NUL, any UTF-8 except control characters. That
/// way a label's "Café" survives the keyboard; ascii.rgba only draws the ASCII part while typing.
fn keyboard_label(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).chars().filter(|c| !c.is_control()).collect()
}

/// An align's offer, `@reanchor <monitor> mm=N deg=N`: the monitor, how far (mm) and how much (°) it moved.
fn reanchor_offer(l: &str) -> Option<(String, f64, f64)> {
    let mut w = l.trim().strip_prefix("@reanchor ")?.split_whitespace();
    let m = w.next()?.to_string();
    let (mut mm, mut deg) = (None, None);
    for kv in w {
        match kv.split_once('=') {
            Some(("mm", v)) => mm = v.parse().ok(),
            Some(("deg", v)) => deg = v.parse().ok(),
            _ => {}
        }
    }
    Some((m, mm?, deg?))
}

/// A host found announcing itself (cc-home machine discover).
#[derive(Clone, PartialEq, Debug)]
struct Host {
    name: String,
    addr: String,
    monitors: u32,
    known: bool, // viewers.conf already has it
    ready: bool, // a key is up on it right now (pair=1)
}

/// `@host name=<h> addr=<ip> port=<p> monitors=<N> m0=... pair=0|1 known=<viewers>|-`.
fn host_line(l: &str) -> Option<Host> {
    let w: Vec<&str> = l.trim().strip_prefix("@host ")?.split_whitespace().collect();
    let get = |k: &str| w.iter().find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='));
    Some(Host {
        name: get("name")?.into(),
        addr: get("addr")?.into(),
        monitors: get("monitors").and_then(|m| m.parse().ok()).unwrap_or(0),
        known: get("known").is_some_and(|k| k != "-"),
        ready: get("pair") == Some("1"),
    })
}

fn host_text(h: &Host) -> String {
    let mut s = format!("{}  {}  {} monitor{}", h.name, h.addr, h.monitors, if h.monitors == 1 { "" } else { "s" });
    if h.known {
        s += "  added";
    }
    if h.ready {
        s += "  key ready";
    }
    s
}

/// `@pair <addr> state=<state>`: the state.
fn pair_line(l: &str) -> Option<&str> {
    l.trim().strip_prefix("@pair ")?.split_whitespace().find_map(|w| w.strip_prefix("state="))
}

/// `@pairscan state=<state>`: the camera's read of the key (looking, read, cancelled, timeout).
fn pairscan_line(l: &str) -> Option<&str> {
    l.trim().strip_prefix("@pairscan ")?.split_whitespace().find_map(|w| w.strip_prefix("state="))
}

/// A pairscan state, worded for the status.
fn scan_status(state: &str, host: &str) -> String {
    match state {
        "looking" => format!("look at {host}'s screen"),
        "read" => "key read".into(),
        "cancelled" => "cancelled".into(),
        "timeout" => "timed out: look closer, or type the key".into(),
        _ => format!("looking: {state}"),
    }
}

/// The machines unpaired while their host was away (cc_proto::conf's pending-unpair/<m>.json),
/// minus any paired again since. Pairing leaves that file behind (unpair_prepare's own test).
fn untold() -> Vec<String> {
    let conf = std::path::PathBuf::from(config::config(""));
    let paired: Vec<_> = cc_proto::conf::trusted_hosts(&conf).into_iter().filter(|(_, t)| t["host_pk"].is_string()).map(|(m, _)| m).collect();
    let files = std::fs::read_dir(conf.join("pending-unpair")).into_iter().flatten().flatten();
    files.filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string)).filter(|m| !paired.contains(m)).collect()
}

/// A paired machine at this address or host name (cc-home's machine_at).
fn paired_at(host: &str) -> bool {
    let conf = std::path::PathBuf::from(config::config(""));
    cc_proto::conf::trusted_hosts(&conf).iter().any(|(_, t)| t["host_pk"].is_string() && (t["addr"] == host || t["host"] == host))
}

/// `@paired <host> id=<id> monitors=N ...`: the machine (viewers.conf's machine=, which is its id,
/// else the host) and the host, as the status names it.
fn paired_line(l: &str) -> Option<(&str, &str)> {
    let mut w = l.trim().strip_prefix("@paired ")?.split_whitespace();
    let host = w.next()?;
    Some((w.find_map(|x| x.strip_prefix("id=")).unwrap_or(host), host))
}

/// The Frame's half of a pairing, in Rust. It's what `cc-home machine pair <addr> <key> [--replace]`
/// and pair.py's frame side do: check the per-host limit, run the exchange on the host's port 3399,
/// then write what it brought (cc_proto::conf). Its `@pair <addr> state=..`, `@pair .. kept=..` and
/// `@paired ..` lines go to `say`. Err is cc-home's last line (pair_status reads
/// `pairing: bad-key <n>`). The key never leaves.
fn pair(addr: &str, key: &str, replace: bool, say: &dyn Fn(String)) -> Result<(), String> {
    use cc_proto::{agent, conf as cc, pair::Fail};
    let state = |s: &str| say(format!("@pair {addr} state={s}"));
    if addr.is_empty() || addr.len() > 253 || !addr.chars().all(|c| c.is_alphanumeric() || "_.:-".contains(c)) {
        return Err(format!("pairing: bad address {addr}"));
    }
    let conf = std::path::PathBuf::from(config::config(""));
    if let Err(e) = cc::pair_try(&conf, addr) {
        state("too-many");
        return Err(e);
    }
    let frame = agent::frame_name();
    // the key paired before at this address: a changed one is refused unless we're replacing
    let known = cc::trusted_hosts(&conf).iter().filter(|(_, t)| t["addr"] == addr && t["host_pk"].is_string()).filter_map(|(m, _)| agent::trusted(&conf, m).ok()).map(|t| t.host_pk).last();
    let sk = agent::frame_key_or_new(&conf).map_err(|e| format!("pairing: this Frame's key: {e}"))?;
    state("waiting");
    let at = std::net::ToSocketAddrs::to_socket_addrs(&(addr, agent::PORT)).ok().and_then(|mut a| a.next());
    let Some(s) = at.and_then(|a| std::net::TcpStream::connect_timeout(&a, cc_proto::pair::STEP).ok()) else {
        state("unreachable");
        return Err(format!("pairing: unreachable (nothing answered on {addr}:{}: is its key up, and its firewall open? cc-share pair --firewall on it)", agent::PORT));
    };
    s.set_read_timeout(Some(cc_proto::pair::STEP)).ok();
    s.set_write_timeout(Some(cc_proto::pair::STEP)).ok();
    let (reply, host, host_pk) = cc_proto::pair::frame_pair(s, key, &frame, &sk, known, replace).map_err(|e| {
        state(match &e {
            Fail::Refused(s, _) => s.as_str(),
            Fail::Bad(_) => "failed",
        });
        format!("pairing: {e}").trim().to_string()
    })?;
    state("ok");
    let result = serde_json::json!({"host": host, "host_pk": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, host_pk), "addr": addr, "reply": reply});
    let said = cc::write_pairing(&conf, addr, &frame, &result, replace).map_err(|e| {
        if e != "host-changed" {
            return e;
        }
        state("host-changed");
        format!("pairing: {addr} is another host now (its id changed): Replace to trust it")
    })?;
    cc::pair_done(&conf, addr);
    said.into_iter().for_each(say);
    Ok(())
}

/// A pairing's state, worded for the status. err is cc-home's last stderr line once it failed
/// (pair.py's `pairing: bad-key <tries left>`, or why).
fn pair_status(state: &str, err: Option<&str>) -> String {
    match state {
        "waiting" => "waiting for the host...".into(),
        "ok" => "paired: adding its monitors...".into(),
        "bad-key" => match err.and_then(|e| e.trim().strip_prefix("pairing: bad-key ")?.parse::<u32>().ok()) {
            Some(n) => format!("wrong key: {n} tries left"),
            None => "wrong key".into(),
        },
        "locked" => "wrong key 3 times: rerun cc-share pair".into(),
        "expired" => "expired: run cc-share pair again".into(),
        "cancelled" => "cancelled on the host".into(),
        "host-changed" => "host changed: Replace, or Look again".into(),
        "bad-name" => "bad name: the host refused the name".into(),
        "name-taken" => "name taken: another Frame has it".into(),
        "full" => "the host has 4 Frames paired already".into(),
        "scan-cancelled" => scan_status("cancelled", ""), // the scan's end (cc-home exits without saying anything)
        "scan-timeout" => scan_status("timeout", ""),
        _ => err.map_or_else(|| format!("pairing: {state}"), str::to_string), // failed, too-many: cc-home says why
    }
}

/// A row as drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Row {
    live: bool,      // connected or connecting
    connected: bool, // its session is up
    auto: bool,      // autoconnect=yes
    untold: bool,    // its machine was unpaired while the host was away, so the host still trusts this Frame
    member: bool,    // its machine is in the workspace
}

/// The Add machine form.
#[derive(Clone, PartialEq, Debug)]
struct Form {
    text: [String; FIELDS],
    editing: Option<usize>, // the field being typed in (FIELDS is the key): the keyboards', and SteamVR's keyboard's if it's open
    hosts: Vec<Host>,       // the hosts found
    search: String,         // the search row's text: searching, found, or why not
    pairing: Option<(Host, bool)>, // the host the key is typed for, and whether Replace is offered (host-changed)
    key: String,
}

impl Form {
    fn new() -> Form {
        let (text, search) = ([String::new(), "0".into(), String::new(), String::new()], "searching for hosts...".into());
        Form { text, editing: None, hosts: Vec::new(), search, pairing: None, key: String::new() }
    }

    /// Field j's text; FIELDS is the key's.
    fn field(&mut self, j: usize) -> &mut String {
        if j < FIELDS { &mut self.text[j] } else { &mut self.key }
    }

    /// A physical key in the field being typed in: its text (64 at most, SteamVR's keyboard's limit),
    /// Tab to the next field (not from the key), Escape out of it. Enter gives the submit.
    fn typed(&mut self, e: Edit) -> Option<Hit> {
        let j = self.editing?;
        match e {
            Edit::Char(c) if self.field(j).len() < 64 => self.field(j).push(c),
            Edit::Back => {
                self.field(j).pop();
            }
            Edit::Next if j < FIELDS => self.editing = Some((j + 1) % FIELDS),
            Edit::Submit => return Some(submit(shape(Some(self)))),
            Edit::Leave => self.editing = None,
            Edit::Char(_) | Edit::Next => {}
        }
        None
    }
}

/// A row being renamed: one field with the label typed in.
#[derive(Clone, PartialEq, Debug)]
struct Rename {
    machine: String,     // its machine (config::machine_of), which cc-home wants for a machine's label
    labels: [String; 2], // the machine's and the monitor's own labels now; the field starts with the chosen one
    scope: usize,        // 0 the machine's label, 1 this monitor's
    choice: bool,        // its machine has 2 or more monitors, so Machine / This monitor shows
    text: String,
    hint: String, // shown when it's empty: its name, which is what an empty label leaves
    editing: bool, // the keyboards' focus
}

impl Rename {
    /// A physical key: its text (64 at most), Enter saves (true), Escape cancels (false).
    fn typed(&mut self, e: Edit) -> Option<bool> {
        match e {
            Edit::Char(c) if self.text.chars().count() < 64 => self.text.push(c),
            Edit::Back => {
                self.text.pop();
            }
            Edit::Submit => return Some(true),
            Edit::Leave => return Some(false),
            Edit::Char(_) | Edit::Next => {}
        }
        None
    }
}

/// What it's drawn from. When this changes, it gets redrawn.
#[derive(Clone, PartialEq, Debug)]
struct Key {
    rows: Vec<Row>,
    hover: Option<Hit>,
    aligning: Option<usize>, // the row an Align runs for: its button says so, and every other Align is greyed out
    no_align: bool, // distrobox-host-exec failed: "use cc-home scan"
    armed: Option<usize>, // the row whose x was clicked once: the next click removes it
    form: Option<Form>,
    status: String,
    precise: bool,         // settings.json's align
    offer: Option<String>, // an align's @reanchor, as shown
    rename: Option<(usize, Rename)>, // the row being renamed
    tags: u32,                       // bumps when the names' tags get redrawn (a rename)
    scope: String,                   // the workspace it's for, as the title says
    theme: u32,
}

/// Its labels (in LABELS' order, minus the title) and ascii.rgba.
#[derive(Default)]
struct Ui {
    title: Option<TagImg>,
    connect: Option<TagImg>,
    disconnect: Option<TagImg>,
    auto: Option<TagImg>,
    align: Option<TagImg>,
    scan: Option<TagImg>,
    aligning: Option<TagImg>,
    new: Option<TagImg>,
    fields: [Option<TagImg>; FIELDS],
    cancel: Option<TagImg>,
    add: Option<TagImg>,
    fast: Option<TagImg>,
    precise: Option<TagImg>,
    reanchor: Option<TagImg>,
    pair: Option<TagImg>,
    refresh: Option<TagImg>,
    replace: Option<TagImg>,
    key: Option<TagImg>,
    look: Option<TagImg>,
    machine: Option<TagImg>,
    this_monitor: Option<TagImg>,
    save: Option<TagImg>,
    member: Option<TagImg>,
    back: Option<TagImg>,
    ascii: Option<TagImg>,
}

struct Scene<'a> {
    k: &'a Key,
    p: Paint,
    ui: &'a Ui,
    names: &'a [Option<&'a TagImg>],
    hosts: &'a [Option<Arc<TagImg>>],
    shown: [(String, bool); FIELDS], // each field's text, or its placeholder (true) when it's empty
    found: Vec<String>,              // each found host's row
}

impl<'a> Scene<'a> {
    fn new(k: &'a Key, p: Paint, ui: &'a Ui, names: &'a [Option<&'a TagImg>], hosts: &'a [Option<Arc<TagImg>>]) -> Scene<'a> {
        let shown = std::array::from_fn(|j| match &k.form {
            Some(f) if !f.text[j].is_empty() => (f.text[j].clone(), false),
            Some(f) if j == 3 => (default_name(&f.text[0], &f.text[1]), true),
            _ => (["user@host", "0", "auto", ""][j].to_string(), true),
        });
        let found = k.form.iter().flat_map(|f| f.hosts.iter().map(host_text)).collect();
        Scene { k, p, ui, names, hosts, shown, found }
    }

    /// Button k (0 Connect, 1 Auto-connect, 2 Align, 3 x) of row i at (x, y), its middle at cy.
    fn button(&self, i: usize, k: usize, x: f64, y: f64, cy: f64) -> ([f64; 3], f64) {
        let (t, acc, r) = (&self.p.t, &self.p.acc, self.k.rows[i]);
        let (x0, x1) = button_x(k);
        let (hit, label) = match k {
            0 => (Hit::Connect(i), if r.live { &self.ui.disconnect } else { &self.ui.connect }),
            1 => (Hit::Auto(i), &self.ui.auto),
            2 if self.k.aligning == Some(i) => (Hit::Align(i), &self.ui.aligning),
            2 => (Hit::Align(i), if self.k.no_align { &self.ui.scan } else { &self.ui.align }),
            _ => (Hit::Remove(i), &None),
        };
        let busy = k == 2 && self.k.aligning == Some(i); // lit from the click to the end, so it doesn't look frozen (I asked for that)
        let armed = k == 3 && self.k.armed == Some(i); // in the accent: the next click removes it
        let off = k == 2 && !busy && (self.k.aligning.is_some() || self.k.no_align);
        let look = if armed { Look::Carry } else if busy || (self.k.hover == Some(hit) && !off) { Look::Lit } else { Look::Rest };
        let mut out = ui::button(&self.p, look, (x0, x1), x, y, cy);
        let ink = if off { t.wdim } else if armed { acc.ink } else { t.wtext };
        let left = if k == 1 {
            out = switch(&self.p, out, x, y, x0 + 10.0, cy, r.auto);
            x0 + 40.0
        } else {
            (x0 + x1 - text_w(label.as_ref())) / 2.0
        };
        let tx = if k == 3 {
            let g = self.ui.ascii.as_ref();
            typed(g, "x", x, y, (x0 + x1 - typed_w(g, "x")) / 2.0, cy)
        } else {
            text(label.as_ref(), x, y, left, cy)
        };
        let tx = tint(tx, ink, ink, ink);
        let a = if x < x1 - 2.0 { tx.1 } else { 0.0 }; // clipped at its end
        let out = over(out, (tx.0, a));
        if off { (out.0, out.1 * 0.6) } else { out }
    }

    /// A plain button over (x0, x1) with its label centred, lit under a laser.
    fn labelled(&self, span: (f64, f64), cy: f64, label: Option<&TagImg>, hit: Hit, x: f64, y: f64) -> ([f64; 3], f64) {
        ui::labelled(&self.p, self.k.hover == Some(hit), span, cy, label, x, y)
    }

    /// The title bar's Fast [switch] Precise, with the chosen one bright, lit under a laser.
    fn precise(&self, x: f64, y: f64) -> ([f64; 3], f64) {
        let (x0, x1) = right(&[PRECISE], 0);
        if x <= x0 - 7.0 || x >= x1 + 7.0 {
            return ([0.0; 3], 0.0);
        }
        let (t, cy, on) = (&self.p.t, TITLE / 2.0, self.k.precise);
        let look = if self.k.hover == Some(Hit::Precise) { Look::Lit } else { Look::Rest };
        let sx = x0 + 52.0;
        let mut out = switch(&self.p, ui::button(&self.p, look, (x0, x1), x, y, cy), x, y, sx, cy, on);
        for (img, left, ink) in [(&self.ui.fast, sx - 6.0 - text_w(self.ui.fast.as_ref()), if on { t.wdim } else { t.wtext }), (&self.ui.precise, sx + 28.0, if on { t.wtext } else { t.wdim })] {
            out = over(out, tint(text(img.as_ref(), x, y, left, cy), ink, ink, ink));
        }
        out
    }

    /// A field: its label, and its box holding s (dim when it's a placeholder) k times as big.
    fn field(&self, label: Option<&TagImg>, lit: bool, (s, hint): (&str, bool), k: f64, x: f64, y: f64, cy: f64) -> ([f64; 3], f64) {
        let (t, acc, g) = (&self.p.t, &self.p.acc, self.ui.ascii.as_ref());
        let mut out = ([0.0; 3], 0.0);
        if x < HOST - GAP {
            out = over(out, tint(text(label, x, y, PAD + 4.0, cy), t.wtext, t.wdim, acc.line));
        }
        let (x0, x1) = (HOST, W - PAD);
        if x > x0 - 7.0 && x < x1 + 7.0 {
            out = over(out, ui::button(&self.p, if lit { Look::Lit } else { Look::Rest }, (x0, x1), x, y, cy));
            let ink = if hint { t.wdim } else { t.wtext };
            let left = (x0 + 8.0).min(x1 - 8.0 - typed_w(g, s) * k); // a long one shows its end
            if x > x0 + 4.0 && x < x1 - 4.0 {
                out = over(out, tint(big(g, s, x, y, left, cy, k), ink, ink, ink));
            }
        }
        out
    }

    /// Renamed row i: its field (the typed label, else its name dimmed), Machine / This monitor if its
    /// machine has 2 or more, then [Save] and x.
    fn renaming(&self, i: usize, r: &Rename, x: f64, y: f64, cy: f64) -> ([f64; 3], f64) {
        let (t, g) = (&self.p.t, self.ui.ascii.as_ref());
        let mut out = ([0.0; 3], 0.0);
        let (x0, x1) = rename_field();
        if x > x0 - 7.0 && x < x1 + 7.0 {
            let lit = r.editing || self.k.hover == Some(Hit::Label(i));
            out = over(out, ui::button(&self.p, if lit { Look::Lit } else { Look::Rest }, (x0, x1), x, y, cy));
            let (s, ink) = if r.text.is_empty() { (r.hint.as_str(), t.wdim) } else { (r.text.as_str(), t.wtext) };
            let left = (x0 + 8.0).min(x1 - 8.0 - typed_w(g, s)); // a long one shows its end
            if x > x0 + 4.0 && x < x1 - 4.0 {
                out = over(out, tint(typed(g, s, x, y, left, cy), ink, ink, ink));
            }
        }
        if r.choice {
            let hover = match self.k.hover {
                Some(Hit::Scope(_, k)) => Some(k),
                _ => None,
            };
            out = over(out, ui::segmented(&self.p, &[self.ui.machine.as_ref(), self.ui.this_monitor.as_ref()], r.scope, hover, right(&RENAME, 0), cy, x, y));
        }
        out = over(out, self.labelled(right(&RENAME, 1), cy, self.ui.save.as_ref(), Hit::Save(i), x, y));
        let (c0, c1) = right(&RENAME, 2);
        if x > c0 - 7.0 && x < c1 + 7.0 {
            let look = if self.k.hover == Some(Hit::Unrename(i)) { Look::Lit } else { Look::Rest };
            out = over(out, ui::button(&self.p, look, (c0, c1), x, y, cy));
            out = over(out, tint(typed(g, "x", x, y, (c0 + c1 - typed_w(g, "x")) / 2.0, cy), t.wtext, t.wtext, t.wtext));
        }
        out
    }

    /// Row j under the machines' rows: the form's (the search, found hosts, fields; or pairing's lines
    /// and key), or the foot.
    fn below(&self, j: usize, x: f64, y: f64, cy: f64) -> ([f64; 3], f64) {
        let (t, g) = (&self.p.t, self.ui.ascii.as_ref());
        let mut out = ([0.0; 3], 0.0);
        let lit = |f: &Form, j: usize, a: Hit| f.editing == Some(j) || self.k.hover == Some(a);
        let (status, s) = match (&self.k.form, shape(self.k.form.as_ref())) {
            (Some(f), Shape::Hosts(h)) if j <= h => {
                let (label, a) = if j == 0 { (&self.ui.refresh, Hit::Refresh) } else { (&self.ui.pair, Hit::Host(j - 1)) };
                out = over(out, self.labelled(right(&[PAIR], 0), cy, label.as_ref(), a, x, y));
                ((PAD + 4.0, right(&[PAIR], 0).0 - GAP), if j == 0 { &f.search } else { &self.found[j - 1] })
            }
            (Some(f), Shape::Hosts(h)) if j <= h + FIELDS => {
                let i = j - 1 - h;
                let (s, hint) = &self.shown[i];
                return self.field(self.ui.fields[i].as_ref(), lit(f, i, Hit::Field(i)), (s, *hint), 1.0, x, y, cy);
            }
            // the host's name large, so you can match it to the screen showing the key
            (Some(Form { pairing: Some((h, _)), .. }), _) if j < 2 => {
                let (s, k) = if j == 0 { (format!("On {}:", h.name), 2.0) } else { ("run cc-share pair, then look at its screen or type the key".into(), 1.3) };
                if x > PAD && x < W - PAD {
                    out = over(out, tint(big(g, &s, x, y, PAD + 4.0, cy, k), t.wtext, t.wtext, t.wtext));
                }
                return out;
            }
            (Some(f), Shape::Pairing(_)) if j == 2 => {
                let shown = if f.key.is_empty() { ("6 digits", true) } else { (f.key.as_str(), false) };
                return self.field(self.ui.key.as_ref(), lit(f, FIELDS, Hit::Key), shown, 1.5, x, y, cy);
            }
            (Some(_), sh) => {
                let label = match submit(sh) {
                    Hit::Pair => &self.ui.pair,
                    Hit::Replace => &self.ui.replace,
                    _ => &self.ui.add,
                };
                let b = foot_w(sh);
                out = over(out, self.labelled(right(b, 0), cy, self.ui.cancel.as_ref(), Hit::Cancel, x, y));
                out = over(out, self.labelled(right(b, 1), cy, label.as_ref(), submit(sh), x, y));
                if b.len() > 2 {
                    out = over(out, self.labelled(right(b, 2), cy, self.ui.look.as_ref(), Hit::Look, x, y));
                }
                ((PAD + 4.0, right(b, 0).0 - GAP), &self.k.status)
            }
            (None, _) => {
                out = over(out, self.labelled((PAD, PAD + NEW), cy, self.ui.new.as_ref(), Hit::New, x, y));
                match &self.k.offer {
                    Some(o) => {
                        out = over(out, self.labelled(right(&[OFFER], 0), cy, self.ui.reanchor.as_ref(), Hit::Reanchor, x, y));
                        // a status that arrived since (it was cleared then) shows over it
                        let s = if self.k.status.is_empty() { o } else { &self.k.status };
                        ((PAD + NEW + 2.0 * GAP, right(&[OFFER], 0).0 - GAP), s)
                    }
                    None => ((PAD + NEW + 2.0 * GAP, W - PAD), &self.k.status),
                }
            }
        };
        if x > status.0 && x < status.1 {
            let left = status.0.min(status.1 - typed_w(g, s)); // a long one shows its end
            out = over(out, tint(typed(g, s, x, y, left, cy), t.wtext, t.wtext, t.wtext));
        }
        out
    }

    /// One pixel at (x, y), in units from the top left. It's a Breeze window: the surface, a title bar
    /// in the frame colour, a 1 u border with rounded corners, holding the rows, the form and the foot.
    fn pixel(&self, x: f64, y: f64) -> ([f64; 3], f64) {
        let (t, acc, n) = (&self.p.t, &self.p.acc, self.k.rows.len());
        let h = height(n, shape(self.k.form.as_ref()));
        let Some((mut out, d)) = ui::chrome(&self.p, self.ui.title.as_ref(), W, h, x, y) else { return ([0.0; 3], 0.0) };
        if y < TITLE {
            out = over(out, self.precise(x, y));
            out = over(out, self.labelled(right(&[BACK, PRECISE], 0), TITLE / 2.0, self.ui.back.as_ref(), Hit::Back, x, y));
            // the workspace it's for, after the title
            let left = PAD + 4.0 + text_w(self.ui.title.as_ref()) + 10.0;
            if x > left && x < right(&[BACK, PRECISE], 0).0 - GAP {
                out = over(out, tint(typed(self.ui.ascii.as_ref(), &self.k.scope, x, y, left, TITLE / 2.0), t.dim, t.dim, t.dim));
            }
        }
        let i = ((y - TITLE) / ROW).floor();
        if i >= 0.0 && (i as usize) < n {
            let (i, cy) = (i as usize, row_y(i as usize));
            let r = self.k.rows[i];
            // the dot: solid when connected, a ring while connecting, dim when not
            let dd = (x - PAD - 10.0).hypot(y - cy) - 5.0;
            out = over(out, match (r.connected, r.live) {
                (true, _) => (acc.line, (0.5 - dd).clamp(0.0, 1.0)),
                (false, true) => (acc.line, line(dd + 0.75, 0.75)),
                _ => (t.wdim, 0.6 * (0.5 - dd).clamp(0.0, 1.0)),
            });
            if let Some((_, r)) = self.k.rename.as_ref().filter(|(r, _)| *r == i) {
                out = over(out, self.renaming(i, r, x, y, cy));
            } else if x < HOST - GAP {
                let lit = self.k.hover == Some(Hit::Name(i)); // a click renames it
                let (c, a) = tint(text(self.names[i], x, y, NAME, cy), if lit { acc.line } else { t.wtext }, t.wdim, acc.line);
                // a label too long for its column fades out over its last 24 units
                let fade = if NAME + text_w(self.names[i]) > HOST - GAP { ((HOST - GAP - x) / 24.0).min(1.0) } else { 1.0 };
                out = over(out, (c, a * fade));
            } else if x < member_x().0 - GAP && r.untold { // the host still trusts this Frame: 17 cells fit the column
                out = over(out, tint(typed(self.ui.ascii.as_ref(), "unpair pending", x, y, HOST, cy), acc.line, acc.line, acc.line));
            } else if x < member_x().0 - GAP {
                out = over(out, tint(text(self.hosts[i].as_deref(), x, y, HOST, cy), t.wdim, t.wdim, acc.line));
            }
            let (m0, m1) = member_x();
            if x > m0 - 7.0 && x < m1 + 7.0 && self.k.rename.as_ref().is_none_or(|(r, _)| *r != i) {
                let look = if self.k.hover == Some(Hit::Member(i)) { Look::Lit } else { Look::Rest };
                let b = switch(&self.p, ui::button(&self.p, look, (m0, m1), x, y, cy), x, y, m0 + 10.0, cy, r.member);
                let ink = if r.member { t.wtext } else { t.wdim };
                let tx = tint(text(self.ui.member.as_ref(), x, y, m0 + 40.0, cy), ink, ink, ink);
                out = over(out, over(b, (tx.0, if x < m1 - 2.0 { tx.1 } else { 0.0 })));
            }
            for k in (0..BUTTONS.len()).filter(|_| self.k.rename.as_ref().is_none_or(|(r, _)| *r != i)) {
                let (x0, x1) = button_x(k);
                if x > x0 - 7.0 && x < x1 + 7.0 {
                    out = over(out, self.button(i, k, x, y, cy));
                }
            }
        } else if i >= 0.0 && i as usize <= foot(n, shape(self.k.form.as_ref())) {
            out = over(out, self.below(i as usize - n, x, y, row_y(i as usize)));
        }
        ui::border(&self.p, out, d)
    }
}

/// typed() k times as big.
fn big(g: Option<&TagImg>, s: &str, x: f64, y: f64, left: f64, cy: f64, k: f64) -> ui::Px {
    typed(g, s, left + (x - left) / k, cy + (y - cy) / k, left, cy)
}

/// Work done in Rust on the worker (a pairing) before its runs: its @ lines go to `say`, then its end.
type Work = Box<dyn FnOnce(&dyn Fn(String)) -> Result<(), String> + Send>;

/// A cc-home run on the worker, and what to do when it ends.
enum Job {
    Set,           // autoconnect=: a failure shows
    Add(String),   // the new machine's name: a spare panel takes it
    Remove(usize), // its panel: gone
    Reanchor(String), // the monitor every spot moved with: every placement gets recalled
    Discover,         // its @host lines: the form's found hosts
    Pair(String),     // the host's name: @pair lines go to the status, @paired's monitors get added
    Paired(Vec<String>), // a pairing's new monitors (once their tags are drawn): spare panels take them
    Line(String),     // a run's @ line as it comes (not an end)
    Rename(usize),    // its panel: its machine's tags get redrawn
    Retag(Vec<usize>), // those panels' tags are drawn: show them
}

fn cc_home(args: &[&str]) -> Command {
    let mut c = Command::new(crate::root().join("cc-home"));
    c.args(args);
    c
}

/// cc-home on the host: Align's scan needs podman, discover needs avahi-browse, pair needs cc-box.
/// A native cc-panels (the sysext image) is on the host already.
fn on_host(args: &[&str]) -> Command {
    if !std::path::Path::new("/run/.containerenv").exists() {
        return cc_home(args);
    }
    let mut c = Command::new(format!("{}/.local/bin/distrobox-host-exec", config::home_dir()));
    c.arg(crate::root().join("cc-home")).args(args);
    c
}

pub struct Machines {
    ov: Option<vr::Handle>,
    shown: bool,
    ui: Arc<Ui>,
    assets: String,
    names: Vec<Option<Arc<TagImg>>>, // each remote's name (its chip's tag), by panel index, spares included
    auto: Vec<bool>,            // each remote's autoconnect=, as viewers.conf says (or as just set)
    rows: Vec<usize>,           // the panels shown as rows: the remotes in use (Panel::used)
    shape: (usize, Shape),      // the rows and form shape it's sized for (grab.rs reshape_extra)
    painter: ui::Painter<Key>,
    hover: Option<Hit>,
    jobs: mpsc::Sender<(Job, Vec<Command>, Option<Work>)>, // cc-home runs (after a pairing's work), one at a time since cc-home rewrites all of viewers.conf
    ended: mpsc::Receiver<(Job, Result<(), String>)>,
    pending: usize, // sent and not ended yet: viewers.conf is read again once none are left
    busy: bool,     // an add or remove is running
    align: Option<(usize, String, Child)>, // the panel, its name, cc-home
    no_align: bool,
    armed: Option<usize>, // the panel whose x was clicked once
    form: Option<Form>,
    status: String,
    precise: bool,                            // settings.json's align: "precise", else fast
    offer: Option<(String, f64, f64)>,        // an align's @reanchor: the monitor, mm, degrees
    offers: mpsc::Sender<(String, f64, f64)>, // the align's stdout reader sends on this
    offered: mpsc::Receiver<(String, f64, f64)>,
    relocs: usize, // relocalizations seen this run
    pair_state: String,     // the running pairing's last @pair state
    paired: Option<(String, String)>, // its @paired machine and host
    untold: Vec<String>,    // machines unpaired without telling the host (pending-unpair/<m>.json): read when it opens and after each run
    osk: bool,              // SteamVR's keyboard is open for the field being typed in
    rename: Option<(usize, Rename)>, // the panel being renamed
    tags: u32,                       // bumps when names' tags get redrawn (Key's)
    ws: String,                      // the workspace it's for (scope, when it opened)
    members: Option<Vec<String>>,    // that workspace's machines (None: all of them)
    reload: bool,                    // the active workspace's machines changed: applied on the tick
}

impl Machines {
    pub fn new(assets: &str) -> Machines {
        let load = |k: &str| crate::load_tag(&format!("{assets}/tag-{k}.rgba"));
        let remotes: Vec<_> = panels().iter().take_while(|p| matches!(p.src, Source::Rdp)).collect();
        let (jobs, todo) = mpsc::channel::<(Job, Vec<Command>, Option<Work>)>();
        let (done, ended) = mpsc::channel();
        let (offers, offered) = mpsc::channel();
        std::thread::spawn(move || {
            for (job, cmds, work) in todo {
                let say = |l: String| {
                    let _ = done.send((Job::Line(l), Ok(())));
                };
                let mut r = work.map_or(Ok(()), |w| w(&say));
                if let Err(e) = &r {
                    eprintln!("machines: pairing failed: {e}"); // never the key
                }
                for (k, mut cmd) in cmds.into_iter().enumerate() {
                    // its @ lines as they come (discover's hosts, a scan pairing's progress), then its end.
                    // ponytail: stderr is read after stdout ends, so a run that fills its stderr pipe (64 KiB) first would hang
                    let run = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().and_then(|mut c| {
                        for l in c.stdout.take().map(|o| BufReader::new(o).lines().map_while(Result::ok)).into_iter().flatten() {
                            if l.starts_with('@') {
                                say(l);
                            }
                        }
                        let mut err = String::new();
                        if let Some(mut e) = c.stderr.take() {
                            let _ = e.read_to_string(&mut err);
                        }
                        Ok((c.wait()?, err))
                    });
                    let out = match run {
                        Ok((s, _)) if s.success() => Ok(()),
                        Ok((s, err)) => Err(err.lines().rev().find(|l| !l.trim().is_empty()).map_or(s.to_string(), str::to_string)),
                        Err(e) => Err(e.to_string()),
                    };
                    if let Err(e) = out {
                        eprintln!("machines: {cmd:?} failed: {e}");
                        // past the first step (cc-home) a failure is only logged: an add's tags (it shows without them)
                        if k == 0 {
                            r = Err(e);
                            break;
                        }
                    }
                }
                let _ = done.send((job, r));
            }
        });
        Machines {
            ov: None,
            shown: false,
            ui: Arc::new(Ui {
                title: load("ui-machines"),
                connect: load("ui-connect"),
                disconnect: load("ui-disconnect"),
                auto: load("ui-auto"),
                align: load("ui-align"),
                scan: load("ui-scan"),
                aligning: load("ui-aligning"),
                new: load("ui-new"),
                fields: ["address", "monitor", "size", "name"].map(|k| load(&format!("ui-{k}"))),
                cancel: load("ui-cancel"),
                add: load("ui-add"),
                fast: load("ui-fast"),
                precise: load("ui-precise"),
                reanchor: load("ui-reanchor"),
                pair: load("ui-pair"),
                refresh: load("ui-refresh"),
                replace: load("ui-replace"),
                key: load("ui-key"),
                look: load("ui-look"),
                machine: load("ui-machine"),
                this_monitor: load("ui-thismonitor"),
                save: load("ui-save"),
                member: load("ui-inworkspace"),
                back: load("ui-back"),
                ascii: crate::load_tag(&format!("{assets}/ascii.rgba")),
            }),
            assets: assets.into(),
            names: remotes.iter().map(|p| load(&format!("chip-{}", p.v.name)).map(Arc::new)).collect(),
            auto: remotes.iter().map(|p| p.v.auto).collect(),
            rows: Vec::new(),
            shape: (0, Shape::Closed),
            painter: ui::Painter::default(),
            hover: None,
            jobs,
            ended,
            pending: 0,
            busy: false,
            align: None,
            no_align: false,
            armed: None,
            form: None,
            status: String::new(),
            precise: precise(),
            offer: None,
            offers,
            offered,
            relocs: 0,
            pair_state: String::new(),
            paired: None,
            untold: Vec::new(),
            osk: false,
            rename: None,
            tags: 0,
            ws: String::new(),
            members: None,
            reload: false,
        }
    }

    /// The Frame relocalized (main.rs): its standing origin turned and shifted, so every spot did too.
    pub fn relocalized(&mut self) {
        self.relocs += 1;
        eprintln!("tracking relocalized ({}): saved places may have shifted; align one monitor and move everything with it", self.relocs);
        self.status = "tracking relocalized: align a monitor, move all with it".into(); // shown when it opens
    }

    /// Under a laser, so the loop keeps the display's rate.
    pub fn moving(&self) -> bool {
        self.hover.is_some()
    }

    /// Every frame: its children opened or closed as OPEN says, then its events, drawing and showing.
    pub fn tick(&mut self, grab: &mut grab::Grab, windows: &mut Windows, bar: &mut Taskbar) {
        if std::mem::take(&mut self.reload) {
            super::workspace::switched(grab, windows, bar);
        }
        if let Some(s) = crate::popout::news() {
            self.status = s; // a pop-out's error, or its window closing
        }
        for (job, r) in self.ended.try_iter().collect::<Vec<_>>() {
            if let Job::Line(l) = &job {
                self.heard(l);
                continue;
            }
            self.pending -= 1;
            self.busy &= matches!(job, Job::Set | Job::Discover);
            match (job, r) {
                (Job::Add(name), Ok(())) => {
                    self.added(&name, bar);
                }
                (Job::Discover, r) => {
                    if let Some(f) = self.form.as_mut() {
                        f.search = match r {
                            Ok(()) if f.hosts.is_empty() => "none found: cc-share announce on, on the host".into(),
                            Ok(()) => format!("found {}", f.hosts.len()),
                            Err(e) => format!("can't search: {e}"),
                        };
                    }
                }
                (Job::Pair(_), Ok(())) => self.pair_done(),
                (Job::Pair(name), Err(e)) => {
                    self.status = pair_status(&self.pair_state, Some(&e));
                    eprintln!("machines: pairing with {name}: {}", self.status);
                }
                // its tags got drawn or not (it shows without them)
                (Job::Paired(names), _) => {
                    let added: Vec<_> = names.iter().filter(|n| self.added(n, bar)).cloned().collect();
                    if added.len() > 1 {
                        self.status = format!("added {}", added.join(", "));
                    }
                }
                (Job::Rename(i), Ok(())) => self.retag(i),
                (Job::Retag(vs), _) => {
                    for i in vs {
                        self.load_tags(i, bar);
                    }
                    self.status = "renamed".into();
                }
                (Job::Remove(i), Ok(())) => {
                    if self.rename.as_ref().is_some_and(|r| r.0 == i) {
                        self.hide_keyboard();
                        self.rename = None;
                    }
                    panel(i).remove();
                    self.status = format!("removed {}", panel(i).v.name);
                    eprintln!("machines: removed {}", panel(i).v.name);
                }
                (Job::Reanchor(m), Ok(())) => {
                    recall(grab, windows, bar);
                    self.status = format!("moved everything with {m}");
                    eprintln!("machines: {}", self.status);
                }
                (_, Err(e)) => self.status = e,
                (Job::Set | Job::Line(_), Ok(())) => {}
            }
            if self.pending == 0 {
                self.untold = untold();
                for v in config::viewers(&[]) {
                    if let Some(i) = panels().iter().take(self.auto.len()).position(|p| p.used() && p.v.name == v.name) {
                        self.auto[i] = v.auto;
                    }
                }
            }
        }
        if let Some((_, name, c)) = self.align.as_mut()
            && let Ok(Some(s)) = c.try_wait()
        {
            eprintln!("machines: align {name} ended ({s})");
            self.align = None;
            // distrobox-host-exec's own failures: 127 (host-spawn: no host bus, or no such command), 126 (not in a container)
            if matches!(s.code(), Some(126 | 127)) {
                eprintln!("machines: can't run cc-home on the host; use cc-home scan");
                self.no_align = true;
            }
        }
        // only once the align ends (it saves before-align last), and only while open: a closed window drops it
        if self.align.is_none()
            && let Some(o) = self.offered.try_iter().last()
            && self.ov.is_some()
        {
            (self.offer, self.status) = (Some(o), String::new()); // a later status shows over it
        }
        self.rows = (0..self.auto.len()).filter(|&i| panel(i).used() && panel(i).v.pop.is_none()).collect(); // a pop-out has no monitor
        let open = OPEN.load(Relaxed);
        if open && self.ov.is_none() {
            self.open(grab);
        } else if !open && let Some(h) = self.ov.take() {
            self.hide_keyboard();
            grab.close_extra("machines");
            crate::gpu::forget(h);
            call!(ov, DestroyOverlay, h);
            (self.shown, self.hover, self.painter, self.offer) = (false, None, ui::Painter::default(), None);
            eprintln!("machines: closed");
        }
        let Some(h) = self.ov else { return };
        self.events(h, grab, windows);
        self.keyboard(h);
        // along with its texture: the card around it matches the size it shows
        if let Some(shape) = self.drawn_shape()
            && shape != self.shape
        {
            self.shape = shape;
            grab.reshape_extra(height(shape.0, shape.1) / W);
        }
        // hidden while the scan hides the panels, and while a VR game runs
        let show = !HIDDEN.load(Relaxed) && !bar.game();
        if show != self.shown {
            self.shown = show;
            if !show {
                self.hide_keyboard(); // give the keyboards back: they're a game's now, not a hidden field's
            }
            grab.set_extra_shown(show);
            if show { call!(ov, ShowOverlay, h) } else { call!(ov, HideOverlay, h) };
        }
        if show {
            self.paint(h);
        }
    }

    /// Made where it was last put down, else OUT ahead of the eyes and DROP under them, facing them.
    /// Its card comes with it (grab.rs places it).
    fn open(&mut self, grab: &mut grab::Grab) {
        if grab.extra_taken() {
            return; // Preferences still has the slot: it closes on its tick, and this opens on the next one
        }
        let h = match vr::create_overlay("controlcenter.machines", "Command Center machines") {
            Ok(h) => h,
            Err(e) => {
                eprintln!("machines: no overlay: {e}"); // SteamVR's 128 are shared, so we're out of them
                OPEN.store(false, Relaxed);
                return;
            }
        };
        call!(ov, SetOverlayInputMethod, h, sys::VROverlayInputMethod_Mouse);
        call!(ov, SetOverlayFlag, h, sys::VROverlayFlags_MakeOverlaysInteractiveIfVisible, true);
        call!(ov, SetOverlaySortOrder, h, SORT);
        let pose = ui::spot("machines", W * MPP);
        self.precise = precise(); // whatever settings.json says now
        self.untold = untold();
        let data = config::workspaces().unwrap_or_else(|_| cc_proto::conf::Json::obj());
        self.ws = SCOPE.lock().unwrap().take().unwrap_or_else(|| cc_proto::conf::active_workspace(&data));
        self.members = cc_proto::conf::workspace_machines(&data, &self.ws);
        self.shape = (self.rows.len(), shape(self.form.as_ref()));
        let pl = Placement::from_matrix(&panel_matrix(&pose), pose.width, height(self.shape.0, self.shape.1) / W, pose.curve);
        grab.open_extra("machines", h, SORT - 1, theme::CYAN, pl, &OPEN);
        self.ov = Some(h);
        eprintln!("machines: opened");
    }

    /// Laser events (hovering, a press on a button or close) and SteamVR's keyboard's text.
    fn events(&mut self, h: vr::Handle, grab: &mut grab::Grab, windows: &mut Windows) {
        let (n, form, offer) = (self.rows.len(), shape(self.form.as_ref()), self.offer.is_some());
        let renamed = self.rename.as_ref().and_then(|(i, r)| Some((self.rows.iter().position(|x| x == i)?, r.choice)));
        let mut e: vr::VREvent_t = unsafe { std::mem::zeroed() };
        while ui::next_event(h, &mut e) {
            let (m, dev) = (unsafe { e.data.mouse }, e.trackedDeviceIndex);
            // its units, from the bottom left; none while the texture (and its mouse scale) is still another shape's
            let (x, y) = (m.x as f64, height(n, form) - m.y as f64);
            let at = renamed.and_then(|(r, c)| rename_hit(r, c, x, y)).unwrap_or_else(|| held(self.hover, n, form, offer, x, y)).filter(|_| self.drawn_shape() == Some((n, form)));
            // its card fades in, and a release ends a carry (including a press elsewhere let go over it)
            grab.panel_event(grab.slot(), &e, &mut KVM.lock().unwrap());
            match e.eventType {
                sys::EVREventType_VREvent_MouseMove => self.hover = at,
                sys::EVREventType_VREvent_FocusLeave => self.hover = None,
                sys::EVREventType_VREvent_MouseButtonDown if m.button == sys::EVRMouseButton_VRMouseButton_Left => {
                    if vr::is_real_controller(dev) {
                        let mut k = KVM.lock().unwrap();
                        if k.awake {
                            k.set_awake(false); // the last device pressed is primary (R-2)
                        }
                    }
                    match at {
                        Some(a) => self.act(a, h),
                        None => self.hide_keyboard(), // a click on nothing leaves the field too
                    }
                }
                // a press on Plasma's bar let go over it (taskbar.rs)
                sys::EVREventType_VREvent_MouseButtonUp => windows.plasma_release(),
                // the whole text each time, since SteamVR's keyboard keeps it (backspace too)
                sys::EVREventType_VREvent_KeyboardCharInput | sys::EVREventType_VREvent_KeyboardDone => {
                    let mut buf = [0u8; 256];
                    call!(ov, GetKeyboardText, buf.as_mut_ptr() as *mut _, buf.len() as u32);
                    let j = unsafe { e.data.keyboard }.uUserValue as usize;
                    let done = e.eventType == sys::EVREventType_VREvent_KeyboardDone;
                    if let Some((_, r)) = self.rename.as_mut().filter(|r| self.osk && r.1.editing && j == OSK_RENAME) {
                        r.text = keyboard_label(&buf);
                        if done {
                            self.osk = false;
                            self.act(Hit::Save(0), h); // its Enter saves, same as the physical one's
                        }
                    } else if let Some(f) = self.form.as_mut().filter(|f| self.osk && f.editing == Some(j)) {
                        *f.field(j) = keyboard_text(&buf);
                        if done {
                            self.osk = false;
                            self.hide_keyboard();
                        }
                    }
                }
                // only its field's: a field left for another closes the first after the second opened, and one
                // we closed ourselves (a physical key) leaves the field typed in
                sys::EVREventType_VREvent_KeyboardClosed => {
                    let j = unsafe { e.data.keyboard }.uUserValue as usize;
                    if self.osk && (self.form.as_ref().is_some_and(|f| f.editing == Some(j)) || (j == OSK_RENAME && self.rename.as_ref().is_some_and(|r| r.1.editing))) {
                        self.osk = false;
                        self.hide_keyboard();
                    }
                }
                _ => {}
            }
        }
    }

    /// The rows and form its uploaded texture was drawn for.
    fn drawn_shape(&self) -> Option<(usize, Shape)> {
        self.painter.drawn().map(|k| (k.rows.len(), shape(k.form.as_ref())))
    }

    /// Align panel i (the Machines window's Align, or a card's align button after a relocalization).
    /// cc-home on the host scans it, one at a time.
    pub fn align_panel(&mut self, i: usize) {
        if self.align.is_some() || self.no_align {
            return eprintln!("machines: an align is running");
        }
        if self.busy {
            self.status = "busy: try again when it ends".into(); // home.json is written by both
            return;
        }
        let name = panel(i).v.name.clone();
        // on the host: the scan runs its camera tools through podman (cc-box)
        let run = on_host(&["machine", "align", &name, "--progress"]).stdout(Stdio::piped()).spawn();
        match run {
            Ok(mut c) => {
                // its stdout to the log, a line at a time; @reanchor offers a move
                let (out, offers) = (c.stdout.take(), self.offers.clone());
                std::thread::spawn(move || {
                    for l in out.map(|o| std::io::BufRead::lines(std::io::BufReader::new(o)).map_while(Result::ok)).into_iter().flatten() {
                        eprintln!("{l}");
                        if let Some(o) = reanchor_offer(&l) {
                            let _ = offers.send(o);
                        }
                    }
                });
                eprintln!("machines: align {name}");
                self.offer = None; // drop the last one's
                self.align = Some((i, name, c));
            }
            Err(e) => {
                eprintln!("machines: align {name}: can't run cc-home on the host ({e}); use cc-home scan");
                self.no_align = true;
            }
        }
    }

    fn act(&mut self, a: Hit, h: vr::Handle) {
        if !matches!(a, Hit::Field(_) | Hit::Key | Hit::Label(_) | Hit::Scope(..)) {
            self.hide_keyboard(); // a click elsewhere leaves the field, keeping its text
        }
        let armed = self.armed.take(); // any other click disarms an x
        if armed.is_some() {
            self.status.clear(); // its "x again removes it"
        }
        match a {
            Hit::Connect(r) => {
                let (i, p) = (self.rows[r], panel(self.rows[r]));
                eprintln!("machines: {} {}", if p.live() { "disconnect" } else { "connect" }, p.v.name);
                if p.live() { crate::disconnect(p) } else { super::connect_later(i) }
            }
            Hit::Auto(r) => {
                let i = self.rows[r];
                let (name, on) = (panel(i).v.name.clone(), !self.auto[i]);
                self.auto[i] = on; // shown now; viewers.conf gets read back once it's written
                let yes = if on { "yes" } else { "no" };
                eprintln!("machines: {name} autoconnect {yes}");
                self.run(Job::Set, vec![cc_home(&["machine", "set", &name, &format!("autoconnect={yes}")])]);
            }
            Hit::Align(r) => self.align_panel(self.rows[r]),
            Hit::Back => {
                super::show(&super::workspace::OPEN, true); // this one closes and that one opens on its tick
            }
            Hit::Member(r) => {
                let p = panel(self.rows[r]);
                let m = config::machine_of(&p.v).to_string();
                let on = !self.members.as_ref().is_none_or(|l| l.iter().any(|x| *x == m));
                let mut all: Vec<String> = self.rows.iter().map(|&i| config::machine_of(&panel(i).v).to_string()).collect();
                all.dedup();
                let ws = self.ws.clone();
                match config::edit_workspaces(|d| {
                    cc_proto::conf::set_member(d, &ws, &m, on, &all)?;
                    Ok((cc_proto::conf::workspace_machines(d, &ws), cc_proto::conf::active_workspace(d) == ws))
                }) {
                    Ok((members, active)) => {
                        eprintln!("machines: {m} {} workspace {ws}", if on { "into" } else { "out of" });
                        self.members = members;
                        self.reload = active; // only the active one's get panels, so apply it now
                    }
                    Err(e) => self.status = e,
                }
            }
            // Host too: a running pairing's @pair lines and end go to the open one
            Hit::Remove(_) | Hit::Add | Hit::Reanchor | Hit::Host(_) | Hit::Pair | Hit::Replace | Hit::Look | Hit::Save(_) if self.busy => {
                eprintln!("machines: busy (an add or remove running)");
                self.status = "busy: try again when it ends".into();
            }
            Hit::Remove(r) => {
                let i = self.rows[r];
                let name = panel(i).v.name.clone();
                if armed != Some(i) {
                    self.armed = Some(i);
                    self.status = format!("remove {name}? x again removes it");
                    return;
                }
                eprintln!("machines: remove {name}");
                self.status = format!("removing {name}...");
                self.run(Job::Remove(i), vec![cc_home(&["machine", "remove", &name])]);
            }
            Hit::Precise => {
                self.precise = !self.precise;
                let v = if self.precise { "precise" } else { "fast" };
                eprintln!("machines: align {v}");
                if let Err(e) = config::set_setting("align", serde_json::json!(v)) {
                    self.status = e;
                }
            }
            Hit::Reanchor => {
                let Some((m, ..)) = self.offer.take() else { return };
                eprintln!("machines: reanchor {m}");
                self.status = format!("moving everything with {m}...");
                self.run(Job::Reanchor(m.clone()), vec![cc_home(&["reanchor", &m])]);
            }
            Hit::New => {
                (self.form, self.status) = (Some(Form::new()), String::new());
                self.discover();
            }
            Hit::Refresh => {
                if let Some(f) = self.form.as_mut() {
                    (f.hosts, f.search) = (Vec::new(), "searching for hosts...".into());
                }
                self.discover();
            }
            Hit::Host(k) => {
                let Some(host) = self.form.as_ref().and_then(|f| f.hosts.get(k)).cloned() else { return };
                if let Some(f) = self.form.as_mut() {
                    (f.pairing, f.key) = (Some((host, false)), String::new());
                }
                self.status.clear();
                self.type_in(h, FIELDS);
            }
            Hit::Name(r) => {
                let i = self.rows[r];
                let all = config::viewers(&[]);
                let Some(v) = all.iter().find(|v| v.name == panel(i).v.name) else { return };
                let machine = config::machine_of(v).to_string();
                // a one-monitor machine's own monitor label (set elsewhere) is what shows, so offer it, and first
                let choice = all.iter().filter(|x| config::machine_of(x) == machine).count() > 1 || !v.label.is_empty();
                let labels = [config::machine_label(v), v.label.clone()];
                let scope = usize::from(!v.label.is_empty());
                let text = labels[scope].clone();
                self.rename = Some((i, Rename { machine, labels, scope, choice, text, hint: v.name.clone(), editing: false }));
                self.type_in(h, OSK_RENAME);
            }
            Hit::Label(_) => self.type_in(h, OSK_RENAME),
            Hit::Scope(_, k) => {
                let osk = self.osk;
                self.close_osk(); // its text is the other label's now, so SteamVR's keyboard opens again with it
                if let Some((_, r)) = self.rename.as_mut().filter(|(_, r)| r.scope != k) {
                    (r.scope, r.text) = (k, r.labels[k].clone());
                }
                if osk {
                    self.type_in(h, OSK_RENAME);
                }
            }
            Hit::Unrename(_) => self.rename = None,
            Hit::Save(_) => {
                if self.rename.as_ref().is_some_and(|r| r.1.text.trim() == "--monitor") {
                    self.status = "--monitor: cc-home's flag, not a name".into(); // cc-home would read it as its flag
                    return;
                }
                let Some((i, r)) = self.rename.take() else { return };
                let (name, label) = (panel(i).v.name.clone(), r.text.trim().to_string());
                eprintln!("machines: rename {} {label:?}", if r.scope == 1 { format!("{name} (its monitor)") } else { format!("{} ({name}'s machine)", r.machine) });
                self.status = "renaming...".into();
                let cmd = if r.scope == 1 { cc_home(&["machine", "rename", &name, &label, "--monitor"]) } else { cc_home(&["machine", "rename", &r.machine, &label]) };
                self.run(Job::Rename(i), vec![cmd]);
            }
            Hit::Field(j) => self.type_in(h, j),
            Hit::Key => self.type_in(h, FIELDS),
            Hit::Cancel => {
                self.status.clear();
                match self.form.as_mut() {
                    Some(f) if f.pairing.is_some() => f.pairing = None, // back to the list
                    _ => self.form = None,
                }
            }
            Hit::Pair | Hit::Replace => {
                let Some(f) = self.form.as_mut() else { return };
                let Some((host, replace)) = f.pairing.as_mut() else { return };
                let key: String = f.key.chars().filter(|c| !c.is_whitespace()).collect();
                if key.len() != 6 || !key.chars().all(|c| c.is_ascii_digit()) {
                    self.status = "type the 6 digits on the host's screen".into();
                    return;
                }
                *replace = false;
                let (name, addr, r) = (host.name.clone(), host.addr.clone(), a == Hit::Replace);
                eprintln!("machines: pair {name} ({addr}){}", if r { ", replacing" } else { "" }); // never the key
                (self.pair_state, self.paired, self.status) = (String::new(), None, format!("pairing with {name}..."));
                self.run_with(Job::Pair(name), Vec::new(), Some(Box::new(move |say: &dyn Fn(String)| pair(&addr, &key, r, say))));
            }
            // the key read off the host's screen (cc-panels' HUD, the camera): nothing typed, no stdin
            Hit::Look => {
                // its text is kept: typing stays the fallback
                let Some((host, replace)) = self.form.as_mut().and_then(|f| f.pairing.as_mut()) else { return };
                let (name, addr, r) = (host.name.clone(), host.addr.clone(), *replace); // kept till the host is reached, which a cancelled or timed-out scan never does
                eprintln!("machines: pair {name} ({addr}) by looking{}", if r { ", replacing" } else { "" });
                let mut args = vec!["machine", "pair", "--scan", &addr];
                if r {
                    args.push("--replace");
                }
                (self.pair_state, self.paired, self.status) = (String::new(), None, format!("look at {name}'s screen"));
                self.run(Job::Pair(name), vec![on_host(&args)]);
            }
            Hit::Add => {
                let Some(f) = &self.form else { return };
                match add_args(&f.text) {
                    Err(e) => self.status = e,
                    // only a paired machine's agent gets asked its size (cc-home would refuse; we say so before it runs)
                    Ok((_, _, host, _, size)) if size.is_empty() && !paired_at(&host) => self.status = "give a size (WxH), or Pair it (Pair reads the size)".into(),
                    Ok((name, user, host, port, size)) => {
                        let addr = format!("{user}@{host}:{port}");
                        eprintln!("machines: add {name} {addr} {size}");
                        self.status = format!("adding {name}...");
                        // its border tag and chip label, as main.rs draws the others' at start
                        let tags = self.tags(&[config::Viewer { name: name.clone(), host: host.clone(), port, ..Default::default() }]);
                        self.run(Job::Add(name.clone()), vec![cc_home(&["machine", "add", &name, &addr, &size].into_iter().filter(|a| !a.is_empty()).collect::<Vec<_>>()), tags]);
                    }
                }
            }
        }
    }

    /// Their border tags and chip labels, drawn the way main.rs draws the others' at start (remote_tags).
    fn tags(&self, vs: &[config::Viewer]) -> Command {
        let all = config::viewers(&[]);
        // ourselves with `--assets` (assets.rs), as a job step like the cc-home runs
        let mut tags = Command::new(std::env::current_exe().unwrap_or_else(|_| crate::root().join("target/release/cc-panels")));
        tags.arg("--assets").arg(&self.assets).arg(theme::font());
        tags.args(vs.iter().flat_map(|v| crate::remote_tags(v, &all)));
        tags
    }

    /// A rename ended: every shown remote of panel i's machine gets its tags redrawn (a machine's
    /// label shows on each), then loaded (Job::Retag). Nothing moves or reconnects.
    fn retag(&mut self, i: usize) {
        let all = config::viewers(&[]);
        let m = all.iter().find(|v| v.name == panel(i).v.name).map(|v| config::machine_of(v).to_string());
        let vs: Vec<_> = all.iter().filter(|v| Some(config::machine_of(v)) == m.as_deref()).cloned().collect();
        let shown = (0..self.auto.len()).filter(|&k| panel(k).used() && vs.iter().any(|v| v.name == panel(k).v.name)).collect();
        let tags = self.tags(&vs);
        self.run(Job::Retag(shown), vec![tags]);
    }

    /// Loads panel i's border tag and chip label (its row's name here) as assets.rs drew them.
    fn load_tags(&mut self, i: usize, bar: &mut Taskbar) {
        let p = panel(i);
        let load = |k: &str| crate::load_tag(&format!("{}/tag-{k}.rgba", self.assets));
        p.set_tag(load(&p.v.name).as_ref());
        self.names[i] = load(&format!("chip-{}", p.v.name)).map(Arc::new);
        bar.set_name(i, self.names[i].clone());
        self.tags += 1;
    }

    /// Hosts announcing themselves, into the form as they come (@host lines, run on the host since avahi-browse isn't in cc-box).
    fn discover(&mut self) {
        self.run(Job::Discover, vec![on_host(&["machine", "discover", "--wait", "3"])]);
    }

    /// A run's @ line: a found host, a pairing's state, or the host it paired.
    fn heard(&mut self, l: &str) {
        if let Some(h) = host_line(l) {
            let Some(f) = self.form.as_mut() else { return };
            match f.hosts.iter().position(|x| x.name == h.name) {
                Some(i) => f.hosts[i] = h,
                None if f.hosts.len() < HOSTS => f.hosts.push(h),
                None => {}
            }
        } else if let Some(s) = pair_line(l) {
            eprintln!("machines: {}", l.trim());
            (self.pair_state, self.status) = (s.into(), pair_status(s, None));
            if s == "host-changed"
                && let Some((_, r)) = self.form.as_mut().and_then(|f| f.pairing.as_mut())
            {
                *r = true; // [Replace]
            }
        } else if let Some(s) = pairscan_line(l) {
            eprintln!("machines: {}", l.trim());
            let host = self.form.as_ref().and_then(|f| f.pairing.as_ref()).map_or("the host", |(h, _)| h.name.as_str());
            (self.pair_state, self.status) = (format!("scan-{s}"), scan_status(s, host));
        } else if let Some((m, h)) = paired_line(l) {
            eprintln!("machines: {}", l.trim());
            self.paired = Some((m.into(), h.into()));
        }
    }

    /// A pairing ended well: its new monitors (viewers.conf's machine=<host> lines no panel has) go
    /// into spare panels once their tags are drawn.
    fn pair_done(&mut self) {
        let Some((machine, host)) = self.paired.take() else {
            self.status = "paired; cc-home didn't say which host".into();
            return;
        };
        let vs: Vec<_> = config::viewers(&[]).into_iter().filter(|v| v.machine == machine).collect();
        // a live panel's viewer is fixed at start, so a line cc-home re-aimed (another slot) needs a restart
        if vs.iter().any(|v| panels().iter().any(|p| p.used() && p.v.name == v.name && (&p.v.user, &p.v.host, p.v.port) != (&v.user, &v.host, v.port))) {
            (self.form, self.status) = (None, format!("paired {host} again; restart cc-panels to reach it"));
            eprintln!("machines: paired {host}: its viewers moved, shown after a restart");
            return;
        }
        let new: Vec<_> = vs.into_iter().filter(|v| !panels().iter().any(|p| p.used() && p.v.name == v.name)).collect();
        eprintln!("machines: paired {host}: {} new monitor(s)", new.len());
        if new.is_empty() {
            (self.form, self.status) = (None, format!("paired {host} again: its monitors log in anew when they reconnect"));
            return;
        }
        self.status = format!("paired {host}: adding its monitors...");
        let tags = self.tags(&new);
        self.run(Job::Paired(new.into_iter().map(|v| v.name).collect()), vec![tags]);
    }

    fn run(&mut self, job: Job, cmds: Vec<Command>) {
        self.run_with(job, cmds, None);
    }

    fn run_with(&mut self, job: Job, cmds: Vec<Command>, work: Option<Work>) {
        self.busy |= !matches!(job, Job::Set | Job::Discover);
        if self.jobs.send((job, cmds, work)).is_ok() {
            self.pending += 1;
        }
    }

    /// cc-home added it: a spare panel takes it, placed (at its saved spot, if any) and connecting.
    /// False when there's no spare (or it wasn't taken).
    fn added(&mut self, name: &str, bar: &mut Taskbar) -> bool {
        self.hide_keyboard(); // a field clicked while it ran
        self.form = None;
        let v = config::viewers(&[name.to_string()]).pop();
        let spare = panels().iter().take(self.auto.len()).find(|p| p.v.name.is_empty());
        let (Some(v), Some(p)) = (v, spare) else {
            self.status = format!("added {name}; restart cc-panels to show it");
            eprintln!("machines: added {name}, no spare panel: shown after a restart");
            return false;
        };
        if !p.fill(v) {
            return false;
        }
        self.load_tags(p.index, bar);
        self.auto[p.index] = p.v.auto;
        crate::place_remote(p);
        super::connect_later(p.index);
        self.status = format!("added {name}");
        eprintln!("machines: added {name} ({}:{}), connecting", p.v.host, p.v.port);
        true
    }

    /// Field j typed in, with the physical keyboards (kvm.rs) and SteamVR's keyboard holding its text.
    /// There's one SteamVR keyboard for every app, so it may be in use.
    /// j: a form field, FIELDS for its key, OSK_RENAME for a rename's field.
    fn type_in(&mut self, h: vr::Handle, j: usize) {
        let up = |e: bool| e && self.osk; // already up for it
        if if j == OSK_RENAME { self.rename.as_ref().is_none_or(|r| up(r.1.editing)) } else { self.form.as_ref().is_none_or(|f| up(f.editing == Some(j))) } {
            return;
        }
        self.close_osk(); // another field's
        let text = if j == OSK_RENAME {
            let Some((_, r)) = self.rename.as_mut() else { return };
            r.editing = true;
            if let Some(f) = self.form.as_mut() {
                f.editing = None; // one field at a time
            }
            r.text.clone()
        } else {
            let Some(f) = self.form.as_mut() else { return };
            f.editing = Some(j);
            if let Some((_, r)) = self.rename.as_mut() {
                r.editing = false;
            }
            f.field(j).clone()
        };
        KVM.lock().unwrap().focus_field("machines", true);
        let hint = if j == OSK_RENAME { "Name shown: empty for none" } else { HINTS[j] };
        let (hint, text) = (CString::new(hint).unwrap_or_default(), CString::new(text).unwrap_or_default());
        let mode = sys::EGamepadTextInputMode_k_EGamepadTextInputModeNormal;
        let lines = sys::EGamepadTextInputLineMode_k_EGamepadTextInputLineModeSingleLine;
        let e = call!(ov, ShowKeyboardForOverlay, h, mode, lines, 0, hint.as_ptr() as *mut _, 64, text.as_ptr() as *mut _, j as u64);
        if e == 0 {
            self.osk = true;
        } else {
            self.status = format!("no keyboard (SteamVR's error {e})");
            eprintln!("machines: {}", self.status);
        }
    }

    /// Leaves the field: SteamVR's keyboard closed and the physical ones given back.
    fn hide_keyboard(&mut self) {
        self.close_osk();
        if let Some(f) = self.form.as_mut() {
            f.editing = None;
        }
        if let Some((_, r)) = self.rename.as_mut() {
            r.editing = false;
        }
        KVM.lock().unwrap().focus_field("machines", false);
    }

    /// Closes SteamVR's keyboard and keeps its text, since without Done it only hands it over when asked.
    fn close_osk(&mut self) {
        if !std::mem::take(&mut self.osk) {
            return;
        }
        let mut buf = [0u8; 256];
        call!(ov, GetKeyboardText, buf.as_mut_ptr() as *mut _, buf.len() as u32);
        if let Some(f) = self.form.as_mut()
            && let Some(j) = f.editing
        {
            *f.field(j) = keyboard_text(&buf);
        } else if let Some((_, r)) = self.rename.as_mut().filter(|r| r.1.editing) {
            r.text = keyboard_label(&buf);
        }
        call!(ov, HideKeyboard);
    }

    /// The physical keyboards' keys in the field being typed in (kvm.rs queues them while it has
    /// them). The first one closes SteamVR's keyboard; typing sent to a remote leaves the field.
    fn keyboard(&mut self, h: vr::Handle) {
        let editing = |m: &Machines| m.form.as_ref().is_some_and(|f| f.editing.is_some()) || m.rename.as_ref().is_some_and(|r| r.1.editing);
        if !editing(self) {
            return;
        }
        let keys = KVM.lock().unwrap().field_keys("machines");
        let Some(keys) = keys else { return self.hide_keyboard() };
        for (code, value, shift) in keys {
            let Some(e) = ui::edit(code, value, shift) else { continue };
            self.close_osk(); // it's the physical keyboard's now
            if let Some((_, r)) = self.rename.as_mut().filter(|r| r.1.editing) {
                match r.typed(e) {
                    Some(true) => self.act(Hit::Save(0), h), // Enter
                    Some(false) => (self.rename, self.status) = (None, String::new()), // Escape: cancelled
                    None => {}
                }
                continue;
            }
            let Some(a) = self.form.as_mut().and_then(|f| f.typed(e)) else { continue };
            self.act(a, h); // Enter: Add or Pair (leaving the field)
        }
        if !editing(self) {
            self.hide_keyboard(); // Escape: the keyboards given back
        }
    }

    /// Redrawn (off the main thread) when anything on it changed, and only what did.
    fn paint(&mut self, h: vr::Handle) {
        let rows = self
            .rows
            .iter()
            .map(|&i| {
                let p = panel(i);
                let m = config::machine_of(&p.v);
                Row { live: p.live(), connected: p.connected.load(Relaxed), auto: self.auto[i], untold: self.untold.iter().any(|x| x == m), member: self.members.as_ref().is_none_or(|l| l.iter().any(|x| x == m)) }
            })
            .collect();
        let row = |i: Option<usize>| i.and_then(|i| self.rows.iter().position(|&r| r == i));
        let key = Key {
            rows,
            hover: self.hover,
            aligning: row(self.align.as_ref().map(|a| a.0)),
            no_align: self.no_align,
            armed: row(self.armed),
            form: self.form.clone(),
            status: self.status.clone(),
            precise: self.precise,
            offer: self.offer.as_ref().map(|(m, mm, deg)| format!("{m} moved {mm:.0} mm, {deg:.1} deg")),
            rename: self.rename.as_ref().and_then(|(i, r)| Some((row(Some(*i))?, r.clone()))),
            tags: self.tags,
            scope: cc_proto::conf::workspace_title(&self.ws).to_string(),
            theme: theme::generation(),
        };
        self.painter.paint(h, key, || {
            let hosts: Vec<_> = self.rows.iter().map(|&i| panel(i).tag().1).collect();
            let names: Vec<_> = self.rows.iter().map(|&i| self.names[i].clone()).collect();
            let (ui, p) = (self.ui.clone(), Paint::new(&theme::get(), theme::CYAN));
            move |was: Option<Key>, px, k: &Key| {
                let names: Vec<_> = names.iter().map(Option::as_deref).collect();
                let sc = Scene::new(k, p, &ui, &names, &hosts);
                let ht = height(k.rows.len(), shape(k.form.as_ref()));
                let s = tex_scale(W, ht, SCALE);
                ui::redraw(px, ((W * s) as usize, (ht * s) as usize), s, (W, ht), was.and_then(|w| dirty(&w, k)), |x, y| sc.pixel(x, y))
            }
        });
    }

    pub fn destroy(&mut self) {
        if let Some(h) = self.ov.take() {
            self.hide_keyboard();
            crate::gpu::forget(h);
            call!(ov, DestroyOverlay, h);
        }
    }
}

/// settings.json's align: precise, else fast (the default).
pub(super) fn precise() -> bool {
    config::settings()["align"] == "precise"
}

/// Every placement back to its home spot (cc-home reanchor moved them all), the way cc-home's spot
/// recall does it: a `place` for each shown panel, the taskbar and this window.
pub(super) fn recall(grab: &mut grab::Grab, windows: &mut Windows, bar: &mut Taskbar) {
    let keys = panels().iter().filter(|p| p.live()).map(|p| p.spot_key()).chain(["taskbar".into(), "machines".into(), "workspace".into()]);
    for k in keys.collect::<Vec<_>>() {
        let Some(p) = config::home_pose(&k, None) else { continue };
        let (curve, vcurve) = if p.vert { (0.0, p.curve) } else { (p.curve, 0.0) };
        let [cx, cy, cz] = p.centre;
        let r = super::command(&format!("place {k} {cx} {cy} {cz} {} {} {} {} {curve} 0 {vcurve}", p.yaw, p.pitch, p.roll, p.width), grab, windows, bar);
        if r != "ok" {
            eprintln!("machines: recall {k}: {r}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(rows: &[Row], hover: Option<Hit>, form: Option<Form>) -> Key {
        Key { rows: rows.to_vec(), hover, aligning: None, no_align: false, armed: None, form, status: "status".into(), precise: false, offer: None, rename: None, tags: 0, scope: "home".into(), theme: 0 }
    }

    fn found() -> Form {
        let h = |n: &str, known, ready| Host { name: n.into(), addr: "10.0.0.9".into(), monitors: 2, known, ready };
        Form { hosts: vec![h("desk", true, false), h("laptop", false, true)], ..Form::new() }
    }

    fn pairing(replace: bool) -> Form {
        let f = found();
        Form { pairing: Some((f.hosts[1].clone(), replace)), ..f }
    }

    const C: Shape = Shape::Closed;

    const TWO: [Row; 2] = [Row { live: true, connected: true, auto: true, untold: false, member: true }, Row { live: false, connected: false, auto: false, untold: false, member: false }];

    #[test]
    fn buttons_sit_right_aligned_in_order() {
        let b: Vec<_> = (0..4).map(button_x).collect();
        assert_eq!(b[3].1, W - PAD);
        for k in 0..4 {
            assert_eq!(b[k].1 - b[k].0, BUTTONS[k]);
        }
        assert!(b[0].1 + GAP == b[1].0 && b[1].1 + GAP == b[2].0 && b[2].1 + GAP == b[3].0);
        assert!(HOST > NAME && b[0].0 - GAP > HOST + 100.0, "room for the host");
        assert_eq!(height(3, Shape::Closed), TITLE + 4.0 * ROW + PAD, "the rows and the foot");
        assert_eq!(height(3, Shape::Hosts(0)), TITLE + 9.0 * ROW + PAD, "and the form's search and fields");
        assert_eq!(height(3, Shape::Hosts(2)), TITLE + 11.0 * ROW + PAD, "and a row a found host");
        assert_eq!(height(3, Shape::Pairing(false)), TITLE + 7.0 * ROW + PAD, "pairing: two lines and the key");
        let (f, p) = (foot_w(Shape::Hosts(0)), foot_w(Shape::Pairing(false)));
        assert!(right(f, 1).1 == W - PAD && right(f, 0).1 + GAP == right(f, 1).0);
        assert!(right(p, 2).1 == W - PAD && right(p, 1).1 + GAP == right(p, 2).0, "pairing: Cancel, Pair, Look");
        assert!(PAD + NEW < right(f, 0).0, "the status between");
        assert!(PAD + 4.0 + 39.0 * 9.6 <= right(p, 0).0 - GAP, "room for pairing's status (39 chars, its longest)");
    }

    #[test]
    fn hits() {
        let mid = |k: usize| (button_x(k).0 + button_x(k).1) / 2.0;
        assert_eq!(hit(2, C, false, mid(0), row_y(0)), Some(Hit::Connect(0)));
        assert_eq!(hit(2, C, false, mid(1), row_y(1) + BTN_H / 2.0 - 1.0), Some(Hit::Auto(1)));
        assert_eq!(hit(2, C, false, mid(2), row_y(1)), Some(Hit::Align(1)));
        assert_eq!(hit(2, C, false, mid(3), row_y(0)), Some(Hit::Remove(0)));
        assert_eq!(hit(2, C, false, W / 2.0, TITLE / 2.0), None, "the title bar");
        assert_eq!(hit(2, C, false, button_x(0).1 + GAP / 2.0, row_y(0)), None, "between buttons");
        assert_eq!(hit(2, C, false, mid(0), row_y(0) + BTN_H / 2.0 + 1.0), None, "between rows");
        assert_eq!(hit(2, C, false, NAME + 10.0, row_y(0)), Some(Hit::Name(0)), "the name: rename it");
        assert_eq!(hit(2, C, false, HOST + 10.0, row_y(0)), None, "the host");
        assert_eq!(hit(2, C, false, PAD + 10.0, row_y(2)), Some(Hit::New), "the foot");
        assert_eq!(hit(2, C, false, mid(0), row_y(2)), None, "the status");
        assert_eq!(hit(2, C, false, PAD + 10.0, row_y(3)), None, "past the foot");
        // the form: the search row, its fields' boxes, then Cancel and Add
        let f = Shape::Hosts(0);
        assert_eq!(hit(2, f, false, HOST + 1.0, row_y(3)), Some(Hit::Field(0)));
        assert_eq!(hit(2, f, false, W - PAD - 1.0, row_y(6)), Some(Hit::Field(3)));
        assert_eq!(hit(2, f, false, PAD + 10.0, row_y(4)), None, "a field's label");
        assert_eq!(hit(2, f, false, PAD + 10.0, row_y(7)), None, "no Add machine under the form");
        let foot = |k: usize| (right(foot_w(f), k).0 + right(foot_w(f), k).1) / 2.0;
        assert_eq!(hit(2, f, false, foot(0), row_y(7)), Some(Hit::Cancel));
        assert_eq!(hit(2, f, false, foot(1), row_y(7)), Some(Hit::Add));
        assert_eq!(hit(2, f, false, foot(1), row_y(8)), None);
        assert_eq!(hit(0, f, false, HOST + 1.0, row_y(1)), Some(Hit::Field(0)), "no machines yet");
        // the title bar's switch, and an offer's button at the foot's right
        let (p0, p1) = right(&[PRECISE], 0);
        assert_eq!(p1, W - PAD);
        assert_eq!(hit(2, C, false, p0 + 1.0, TITLE / 2.0), Some(Hit::Precise));
        assert_eq!(hit(2, f, false, p1 - 1.0, TITLE / 2.0 + BTN_H / 2.0 - 1.0), Some(Hit::Precise), "with the form too");
        assert_eq!(hit(2, C, false, p0 - 1.0, TITLE / 2.0), None, "the title");
        assert_eq!(hit(2, C, false, p0 + 1.0, 1.0), None, "above it");
        let (o0, o1) = right(&[OFFER], 0);
        assert!(PAD + NEW + 2.0 * GAP + 200.0 < o0, "room for the offer's text");
        assert_eq!(hit(2, C, true, (o0 + o1) / 2.0, row_y(2)), Some(Hit::Reanchor));
        assert_eq!(hit(2, C, false, (o0 + o1) / 2.0, row_y(2)), None, "no offer: the status");
        assert_eq!(hit(2, C, true, PAD + 10.0, row_y(2)), Some(Hit::New), "Add machine still");
        assert_eq!(hit(2, f, true, foot(1), row_y(7)), Some(Hit::Add), "the form's foot over the offer");
    }

    #[test]
    fn the_workspace_switch_and_back_hit_and_draw() {
        let mid = |(x0, x1): (f64, f64)| (x0 + x1) / 2.0;
        assert_eq!(hit(2, C, false, mid(member_x()), row_y(1)), Some(Hit::Member(1)));
        assert_eq!(hit(2, C, false, mid(right(&[BACK, PRECISE], 0)), TITLE / 2.0), Some(Hit::Back));
        assert_eq!(hit(2, C, false, mid(right(&[PRECISE], 0)), TITLE / 2.0), Some(Hit::Precise));
        assert!(member_x().1 < button_x(0).0 && member_x().0 > HOST + 100.0, "room for the host");
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::CYAN));
        let k = key(&TWO, None, None);
        let sc = Scene::new(&k, p, &ui, &[None, None], &[None, None]);
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        let (m0, _) = member_x();
        assert!(near(sc.pixel(m0 + 10.0 + 11.0, row_y(0) + 6.0).0, p.acc.line), "in it: the switch's track in the accent");
        assert!(!near(sc.pixel(m0 + 10.0 + 11.0, row_y(1) + 6.0).0, p.acc.line), "out of it: not");
    }

    #[test]
    fn found_hosts_and_pairing_hits() {
        let mid = |(x0, x1): (f64, f64)| (x0 + x1) / 2.0;
        let (b, foot) = (mid(right(&[PAIR], 0)), |s: Shape, k: usize| mid(right(foot_w(s), k)));
        let s = Shape::Hosts(2);
        assert_eq!(hit(1, s, false, b, row_y(1)), Some(Hit::Refresh), "the search row");
        assert_eq!(hit(1, s, false, PAD + 10.0, row_y(1)), None, "its text");
        assert_eq!(hit(1, s, false, b, row_y(2)), Some(Hit::Host(0)));
        assert_eq!(hit(1, s, false, b, row_y(3)), Some(Hit::Host(1)));
        assert_eq!(hit(1, s, false, PAD + 10.0, row_y(3)), None, "a host's text");
        assert_eq!(hit(1, s, false, HOST + 1.0, row_y(4)), Some(Hit::Field(0)), "the fields under them");
        assert_eq!(hit(1, s, false, HOST + 1.0, row_y(7)), Some(Hit::Field(3)));
        assert_eq!(hit(1, s, false, foot(s, 1), row_y(8)), Some(Hit::Add));
        assert_eq!(hit(1, s, false, mid(right(&FOOT, 2)), row_y(8)), Some(Hit::Add), "no Look: Add rightmost");
        assert_eq!(rect(1, s, Hit::Field(0)).1, row_y(4) - ROW / 2.0, "rect agrees");
        let p = Shape::Pairing(false);
        assert_eq!(hit(1, p, false, W / 2.0, row_y(1)), None, "the instruction");
        assert_eq!(hit(1, p, false, W / 2.0, row_y(2)), None);
        assert_eq!(hit(1, p, false, HOST + 1.0, row_y(3)), Some(Hit::Key));
        assert_eq!(hit(1, p, false, foot(p, 0), row_y(4)), Some(Hit::Cancel));
        assert_eq!(hit(1, p, false, foot(p, 1), row_y(4)), Some(Hit::Pair));
        assert_eq!(hit(1, p, false, foot(p, 2), row_y(4)), Some(Hit::Look), "rightmost");
        assert_eq!(hit(1, Shape::Pairing(true), false, foot(p, 1), row_y(4)), Some(Hit::Replace), "host changed");
        assert_eq!(hit(1, Shape::Pairing(true), false, foot(p, 2), row_y(4)), Some(Hit::Look), "Look still");
        assert_eq!(hit(1, p, false, foot(p, 1), row_y(5)), None);
        assert_eq!(rect(1, p, Hit::Look).2, W - PAD + 8.0, "rect agrees");
        let gap = row_y(2) + BTN_H / 2.0 + 2.0;
        assert_eq!(held(Some(Hit::Host(0)), 1, s, false, b, gap), Some(Hit::Host(0)));
        assert_eq!(held(Some(Hit::Host(1)), 1, Shape::Hosts(1), false, b, row_y(3) + BTN_H / 2.0 + 2.0), None, "that host gone");
        assert_eq!(held(Some(Hit::Pair), 1, Shape::Pairing(true), false, foot(p, 1), row_y(4) + BTN_H / 2.0 + 2.0), None, "Replace now");
        assert_eq!(held(Some(Hit::Look), 1, p, false, foot(p, 2), row_y(4) + BTN_H / 2.0 + 2.0), Some(Hit::Look));
        assert_eq!(held(Some(Hit::Look), 1, s, false, foot(p, 2), row_y(8) + BTN_H / 2.0 + 2.0), None, "back to the list");
    }

    #[test]
    fn it_reads_discover_and_pair_lines() {
        let h = host_line("@host name=desk addr=203.0.113.63 port=3399 monitors=2 m0=DP-1,3840x1080 m1=DP-2,1440x2560 pair=1 known=desk-wide,desk-portrait").unwrap();
        assert_eq!(h, Host { name: "desk".into(), addr: "203.0.113.63".into(), monitors: 2, known: true, ready: true });
        let l = host_line("@host name=laptop addr=10.0.0.2 port=3399 monitors=1 m0=eDP-1,1920x1080 pair=0 known=-\n").unwrap();
        assert!(!l.known && !l.ready && l.monitors == 1);
        assert_eq!(host_text(&l), "laptop  10.0.0.2  1 monitor");
        assert_eq!(host_text(&h), "desk  203.0.113.63  2 monitors  added  key ready");
        assert_eq!(host_line("@host addr=10.0.0.2"), None, "no name");
        assert_eq!(host_line("@pair 10.0.0.2 state=ok"), None);
        assert_eq!(pair_line("@pair 10.0.0.2 state=bad-key"), Some("bad-key"));
        assert_eq!(pair_line("@paired desk monitors=2 cred=per-frame"), None);
        assert_eq!(paired_line("@paired desk monitors=2 cred=per-frame\n"), Some(("desk", "desk")), "a pre-id host");
        assert_eq!(paired_line("@paired desk id=0123abcd-0000-4000-8000-00000000beef monitors=2"), Some(("0123abcd-0000-4000-8000-00000000beef", "desk")), "machine= is its id, shown as its host");
        assert_eq!(paired_line("@pair desk state=ok"), None);
        assert_eq!(pair_status("bad-key", None), "wrong key");
        assert_eq!(pair_status("bad-key", Some("pairing: bad-key 2")), "wrong key: 2 tries left");
        assert_eq!(pair_status("host-changed", None), "host changed: Replace, or Look again");
        assert_eq!(pair_status("cancelled", Some("pairing: cancelled")), "cancelled on the host");
        assert_eq!(pair_status("too-many", Some("3 tries with x in 10 minutes: wait 9 s")), "3 tries with x in 10 minutes: wait 9 s", "cc-home's why");
        assert_eq!(pair_status("", Some("no such command")), "no such command", "no state line: the error");
        assert_eq!(pairscan_line("@pairscan state=looking\n"), Some("looking"));
        assert_eq!(pairscan_line("@pair 10.0.0.2 host=desk state=scanning"), None);
        assert_eq!(pair_line("@pairscan state=read"), None);
        assert_eq!(scan_status("looking", "desk"), "look at desk's screen");
        assert_eq!(pair_status("scan-timeout", Some("exit status: 1")), "timed out: look closer, or type the key", "the scan's end");
        assert_eq!(pair_status("scan-cancelled", Some("exit status: 1")), "cancelled");
        for st in ["waiting", "ok", "bad-key", "locked", "expired", "cancelled", "host-changed", "bad-name", "name-taken", "full", "scan-cancelled", "scan-timeout"] {
            assert!(pair_status(st, Some("pairing: bad-key 2")).len() <= 39, "{st}: fits pairing's status");
        }
    }

    #[test]
    fn an_align_offers_a_reanchor() {
        assert_eq!(reanchor_offer("@reanchor desk-wide mm=42 deg=3.8"), Some(("desk-wide".into(), 42.0, 3.8)));
        assert_eq!(reanchor_offer("@reanchor desk deg=-1.5 mm=7.25\n"), Some(("desk".into(), 7.25, -1.5)), "either order");
        assert_eq!(reanchor_offer("@reanchor desk mm=42"), None, "no deg");
        assert_eq!(reanchor_offer("@reanchor desk mm=x deg=1"), None);
        assert_eq!(reanchor_offer("@reanchor"), None);
        assert_eq!(reanchor_offer("@progress 3/10"), None);
        assert_eq!(reanchor_offer("desk moved: @reanchor desk mm=1 deg=1"), None, "a line of its own");
    }

    #[test]
    fn it_draws_rows_and_buttons() {
        let k = Key { armed: Some(1), ..key(&TWO, Some(Hit::Connect(1)), None) };
        let ui = Ui::default();
        let sc = Scene::new(&k, Paint::new(&theme::Theme::default(), theme::CYAN), &ui, &[None, None], &[None, None]);
        let t = sc.p.t;
        let near = |c: [f64; 3], want: [f64; 3]| c.iter().zip(&want).all(|(a, b)| (a - b).abs() < 2.0);
        assert_eq!(sc.pixel(0.2, 0.2).1, 0.0, "round corners: clear");
        assert!(near(sc.pixel(W / 2.0, TITLE / 2.0).0, t.frame), "the title bar");
        assert!(near(sc.pixel(HOST + 2.0, row_y(1)).0, t.surface), "a row's surface");
        assert!(near(sc.pixel(PAD + 10.0, row_y(0)).0, sc.p.acc.line), "connected: a solid dot");
        assert!(!near(sc.pixel(PAD + 10.0, row_y(1)).0, sc.p.acc.line), "not connected: dim");
        let rest = sc.pixel(button_x(0).0 + 4.0, row_y(0)).0;
        let lit = sc.pixel(button_x(0).0 + 4.0, row_y(1)).0;
        assert!(near(rest, t.raised) && !near(lit, t.raised), "hovered: tinted {rest:?} {lit:?}");
        let on = sc.pixel(button_x(1).0 + 12.0, row_y(0)).0;
        assert!(near(on, sc.p.acc.line), "auto on: the track in the accent {on:?}");
        assert!(near(sc.pixel(button_x(3).0 + 3.0, row_y(1)).0, sc.p.acc.line), "armed: x in the accent");
        assert!(near(sc.pixel(PAD + 4.0, row_y(2)).0, t.raised), "Add machine");
        let sx = right(&[PRECISE], 0).0 + 52.0;
        assert!(!near(sc.pixel(sx + 12.0, TITLE / 2.0).0, sc.p.acc.line), "fast: the switch's track off");
        let k = Key { precise: true, ..k.clone() };
        let sc = Scene::new(&k, sc.p, &ui, &[None, None], &[None, None]);
        assert!(near(sc.pixel(sx + 12.0, TITLE / 2.0).0, sc.p.acc.line), "precise: on");
        let px = grab::draw(W as usize, height(2, Shape::Closed) as usize, |x, y| sc.pixel(x, y));
        assert_eq!(px.len(), W as usize * height(2, Shape::Closed) as usize * 4);
    }

    #[test]
    fn a_patch_draws_as_all_of_it() {
        let (ui, p) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::CYAN));
        let s = 1.5;
        let cases = [
            (None, Some(Hit::Connect(1)), Some(Hit::Auto(1))),
            (None, Some(Hit::Connect(0)), Some(Hit::Align(1))),
            (None, Some(Hit::Align(0)), None),
            (None, Some(Hit::Remove(1)), Some(Hit::New)),
            (Some(Form::new()), Some(Hit::Field(0)), Some(Hit::Add)),
            (Some(Form { editing: Some(2), ..Form::new() }), Some(Hit::Field(2)), Some(Hit::Cancel)),
            (Some(Form::new()), Some(Hit::Field(3)), None),
            (None, Some(Hit::Precise), Some(Hit::Reanchor)),
            (None, Some(Hit::Reanchor), Some(Hit::Connect(0))),
            (Some(Form::new()), Some(Hit::Precise), Some(Hit::Cancel)),
            (Some(found()), Some(Hit::Refresh), Some(Hit::Host(1))),
            (Some(found()), Some(Hit::Host(0)), Some(Hit::Field(1))),
            (Some(pairing(false)), Some(Hit::Key), Some(Hit::Pair)),
            (Some(Form { editing: Some(FIELDS), ..pairing(false) }), Some(Hit::Pair), Some(Hit::Cancel)),
            (Some(pairing(true)), Some(Hit::Replace), None),
            (Some(pairing(false)), Some(Hit::Pair), Some(Hit::Look)),
            (Some(pairing(true)), Some(Hit::Look), Some(Hit::Cancel)),
            (Some(Form { editing: Some(FIELDS), ..pairing(false) }), Some(Hit::Key), Some(Hit::Look)),
        ];
        for (form, from, to) in cases {
            let h = height(2, shape(form.as_ref()));
            let (tw, th) = ((W * s) as usize, (h * s) as usize);
            let offered = |hover| Key { offer: Some("desk moved 42 mm, 3.8 deg".into()), precise: true, ..key(&TWO, hover, form.clone()) };
            let draw = |hover| {
                let k = offered(hover);
                let sc = Scene::new(&k, p, &ui, &[None, None], &[None, None]);
                grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
            };
            let (a, k) = (offered(from), offered(to));
            let sc = Scene::new(&k, p, &ui, &[None, None], &[None, None]);
            let t = ui::redraw(draw(from), (tw, th), s, (W, h), dirty(&a, &k), |x, y| sc.pixel(x, y));
            assert!(!t.full && t.px == draw(to), "{form:?} {from:?} -> {to:?}");
        }
        // a value: only what it changes
        let h = height(2, C);
        let (tw, th) = ((W * s) as usize, (h * s) as usize);
        let draw = |k: &Key| {
            let sc = Scene::new(k, p, &ui, &[None, None], &[None, None]);
            grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
        };
        let a = key(&TWO, Some(Hit::Connect(0)), None);
        let off = Row { live: false, ..TWO[0] };
        for b in [
            key(&[off, TWO[1]], Some(Hit::Connect(0)), None),
            Key { aligning: Some(1), ..a.clone() },
            Key { no_align: true, ..a.clone() },
            Key { armed: Some(0), ..a.clone() },
            Key { status: "a much longer status than it was before".into(), ..a.clone() },
            Key { offer: Some("desk moved 42 mm, 3.8 deg".into()), ..a.clone() },
            Key { precise: true, hover: None, ..a.clone() },
        ] {
            let sc = Scene::new(&b, p, &ui, &[None, None], &[None, None]);
            let t = ui::redraw(draw(&a), (tw, th), s, (W, h), dirty(&a, &b), |x, y| sc.pixel(x, y));
            assert!(!t.full && t.px == draw(&b), "{a:?} -> {b:?}");
        }
        let f = |text: &str| Key { form: Some(Form { text: [text.into(), "0".into(), String::new(), String::new()], editing: Some(0), ..Form::new() }), ..a.clone() };
        let (a, b) = (f("me@desk"), f("me@laptop"));
        let (h, sc) = (height(2, Shape::Hosts(0)), Scene::new(&b, p, &ui, &[None, None], &[None, None]));
        let (tw, th) = ((W * s) as usize, (h * s) as usize);
        let full = |k: &Key| {
            let sc = Scene::new(k, p, &ui, &[None, None], &[None, None]);
            grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
        };
        let t = ui::redraw(full(&a), (tw, th), s, (W, h), dirty(&a, &b), |x, y| sc.pixel(x, y));
        assert!(!t.full && t.px == full(&b), "typed: the address, and Name's placeholder");
        // pairing: a key typed, a state; the search's text
        let g = |key: &str, status: &str| Key { form: Some(Form { key: key.into(), ..pairing(false) }), status: status.into(), ..a.clone() };
        let srch = |s: &str| Key { form: Some(Form { search: s.into(), ..found() }), ..a.clone() };
        for (a, b) in [(g("", ""), g("482 9", "")), (g("482917", "waiting for the host..."), g("482917", "wrong key: 2 tries left")), (g("", "look at laptop's screen"), g("", "timed out: look closer, or type the key")), (srch("searching for hosts..."), srch("found 2"))] {
            let h = height(2, shape(b.form.as_ref()));
            let (tw, th) = ((W * s) as usize, (h * s) as usize);
            let full = |k: &Key| {
                let sc = Scene::new(k, p, &ui, &[None, None], &[None, None]);
                grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
            };
            let sc = Scene::new(&b, p, &ui, &[None, None], &[None, None]);
            let t = ui::redraw(full(&a), (tw, th), s, (W, h), dirty(&a, &b), |x, y| sc.pixel(x, y));
            assert!(!t.full && t.px == full(&b), "{a:?} -> {b:?}");
        }
        assert!(dirty(&key(&TWO, None, Some(found())), &key(&TWO, None, Some(pairing(false)))).is_none(), "pairing: a new shape");
        assert!(dirty(&key(&TWO, None, Some(pairing(false))), &key(&TWO, None, Some(pairing(true)))).is_none(), "Replace offered");
        assert!(dirty(&key(&TWO, None, None), &key(&TWO, None, Some(Form::new()))).is_none(), "a new shape: all of it");
        assert!(dirty(&key(&TWO, None, None), &key(&TWO[..1], None, None)).is_none());
    }

    #[test]
    fn a_renamed_row_hits_types_and_draws() {
        let mid = |(x0, x1): (f64, f64)| (x0 + x1) / 2.0;
        assert_eq!(rename_hit(1, true, NAME + 10.0, row_y(0)), None, "another row: its own hits");
        assert_eq!(rename_hit(1, true, NAME + 10.0, row_y(1)), Some(Some(Hit::Label(1))));
        let (s0, s1) = right(&RENAME, 0);
        assert_eq!(rename_hit(1, true, s0 + 5.0, row_y(1)), Some(Some(Hit::Scope(1, 0))), "Machine");
        assert_eq!(rename_hit(1, true, s1 - 5.0, row_y(1)), Some(Some(Hit::Scope(1, 1))), "This monitor");
        assert_eq!(rename_hit(1, false, s0 + 5.0, row_y(1)), Some(None), "one monitor: no choice, and not Connect under it");
        assert_eq!(rename_hit(1, true, mid(right(&RENAME, 1)), row_y(1)), Some(Some(Hit::Save(1))));
        assert_eq!(rename_hit(1, true, mid(right(&RENAME, 2)), row_y(1)), Some(Some(Hit::Unrename(1))));
        assert_eq!(right(&RENAME, 2).1, W - PAD);
        assert!(rename_field().1 - rename_field().0 > 300.0, "room for a label");
        let mut r = Rename { machine: "id1".into(), labels: ["Desk".into(), String::new()], scope: 0, choice: true, text: "Desk".into(), hint: "desk-wide".into(), editing: true };
        for e in [Edit::Back, Edit::Char('!'), Edit::Next] {
            assert_eq!(r.typed(e), None);
        }
        assert_eq!(r.text, "Des!");
        for _ in 0..70 {
            r.typed(Edit::Char('z'));
        }
        assert_eq!(r.text.chars().count(), 64, "a label's most");
        assert_eq!(r.typed(Edit::Submit), Some(true), "Enter: Save");
        assert_eq!(r.typed(Edit::Leave), Some(false), "Escape: cancel");
        // drawn: only its rows again, as all of it
        let (ui, p, s) = (Ui::default(), Paint::new(&theme::Theme::default(), theme::CYAN), 1.5);
        let r = Rename { text: "Desk".into(), ..r };
        let a = key(&TWO, None, None);
        let (h, tw) = (height(2, C), (W * s) as usize);
        let th = (h * s) as usize;
        let draw = |k: &Key| {
            let sc = Scene::new(k, p, &ui, &[None, None], &[None, None]);
            grab::draw(tw, th, |x, y| sc.pixel(x / s, y / s))
        };
        let on = |row: usize, r: &Rename, hover| Key { rename: Some((row, r.clone())), hover, ..a.clone() };
        let empty = Rename { text: String::new(), ..r.clone() };
        let monitor = Rename { scope: 1, choice: true, ..r.clone() };
        for (a, b) in [
            (a.clone(), on(1, &r, None)),
            (on(1, &r, None), on(1, &empty, Some(Hit::Save(1)))),
            (on(1, &r, Some(Hit::Scope(1, 1))), on(1, &monitor, Some(Hit::Unrename(1)))),
            (on(0, &r, Some(Hit::Label(0))), on(1, &r, None)),
            (on(1, &r, None), a.clone()),
            (a.clone(), Key { tags: 1, hover: Some(Hit::Name(0)), ..a.clone() }),
        ] {
            let sc = Scene::new(&b, p, &ui, &[None, None], &[None, None]);
            let t = ui::redraw(draw(&a), (tw, th), s, (W, h), dirty(&a, &b), |x, y| sc.pixel(x, y));
            assert!(!t.full && t.px == draw(&b), "{a:?} -> {b:?}");
        }
    }

    #[test]
    fn a_laser_between_buttons_keeps_its_hover() {
        let gap = row_y(0) + BTN_H / 2.0 + 2.0;
        let x = button_x(0).0 + 2.0;
        assert_eq!(hit(2, C, false, x, gap), None);
        assert_eq!(held(Some(Hit::Connect(0)), 2, C, false, x, gap), Some(Hit::Connect(0)));
        assert_eq!(held(Some(Hit::Connect(0)), 2, C, false, x, row_y(1)), Some(Hit::Connect(1)), "on another: that one");
        assert_eq!(held(Some(Hit::Connect(0)), 2, C, false, HOST + 2.0, row_y(0)), None, "off it");
        assert_eq!(held(Some(Hit::Remove(1)), 1, C, false, button_x(3).0 + 2.0, row_y(1)), None, "its row gone");
    }

    #[test]
    fn the_form_makes_cc_homes_arguments() {
        let f = |a: &str, m: &str, s: &str, n: &str| add_args(&[a.into(), m.into(), s.into(), n.into()]);
        assert_eq!(f("user@203.0.113.63", "1", "2560x1440", "desk-2"), Ok(("desk-2".into(), "user".into(), "203.0.113.63".into(), 3401, "2560x1440".into())));
        assert_eq!(f(" user@desk.lan ", "0", "1920x1080", "").unwrap().0, "desk", "the name from the host");
        assert_eq!(f("user@desk", "", "", ""), Ok(("desk".into(), "user".into(), "desk".into(), 3400, String::new())), "empty: monitor 0, the size asked of the machine");
        assert_eq!(f("a@203.0.113.63", "2", "1920x1080", "").unwrap().0, "203-0-113-63-2");
        assert_eq!(f("user@desk", "0", "1920x1080", "").unwrap().3, 3400, "monitor 0: krdp's 3400");
        assert!(f("desk", "0", "1920x1080", "").unwrap_err().starts_with("address"));
        assert!(f("a@b:3400", "0", "1920x1080", "").unwrap_err().starts_with("address"), "the port is the monitor's");
        assert!(f("a@b", "x", "1920x1080", "").unwrap_err().starts_with("monitor"));
        assert!(f("a@b", "0", "1920", "").unwrap_err().starts_with("size"));
        assert!(f("a@b", "0", "0x1080", "").unwrap_err().starts_with("size"));
        assert!(f("a@b", "0", "1920x1080", "my desk").unwrap_err().starts_with("name"));
        assert_eq!(default_name("a@desk.lan", "1"), "desk-1");
    }

    #[test]
    fn the_keyboards_edit_the_field_typed_in() {
        let mut f = Form::new();
        assert_eq!(f.typed(Edit::Char('a')), None);
        assert_eq!(f.text[0], "", "no field typed in: nothing");
        f.editing = Some(0);
        for e in [Edit::Char('m'), Edit::Char('e'), Edit::Char('x'), Edit::Back, Edit::Char('@'), Edit::Char('h')] {
            f.typed(e);
        }
        assert_eq!(f.text[0], "me@h");
        f.typed(Edit::Next);
        assert_eq!(f.editing, Some(1), "Tab: the next field");
        f.typed(Edit::Back);
        f.typed(Edit::Char('1'));
        assert_eq!(f.text[1], "1", "Monitor's 0 gone");
        f.editing = Some(FIELDS - 1);
        f.typed(Edit::Next);
        assert_eq!(f.editing, Some(0), "from the last: the first");
        for _ in 0..70 {
            f.typed(Edit::Char('z'));
        }
        assert_eq!(f.text[0].len(), 64, "SteamVR's keyboard's most");
        assert_eq!(f.typed(Edit::Submit), Some(Hit::Add), "Enter: Add");
        assert_eq!(f.typed(Edit::Leave), None);
        assert_eq!(f.editing, None, "Escape: out of it");
        let mut p = Form { editing: Some(FIELDS), ..pairing(false) };
        for e in [Edit::Char('1'), Edit::Char('2'), Edit::Next] {
            p.typed(e);
        }
        assert_eq!((p.key.as_str(), p.editing), ("12", Some(FIELDS)), "the key: no next field");
        assert_eq!(p.typed(Edit::Submit), Some(Hit::Pair));
        assert_eq!(Form { editing: Some(FIELDS), ..pairing(true) }.typed(Edit::Submit), Some(Hit::Replace));
    }

    #[test]
    fn keyboard_text_reads_to_the_nul_printable_only() {
        assert_eq!(keyboard_text(b"a@b\0junk"), "a@b");
        assert_eq!(keyboard_text("dé sk\0".as_bytes()), "d sk", "not ASCII: dropped (ascii.rgba can't draw it)");
        assert_eq!(keyboard_text(&[b'x'; 4]), "xxxx", "no NUL: all of it");
        assert_eq!(keyboard_label("Café\tB\0junk".as_bytes()), "CaféB", "a label: UTF-8 kept, control characters dropped");
        assert_eq!(keyboard_label(b"a\xffb"), "a\u{fffd}b", "bad UTF-8: replaced, not lost");
    }
}
