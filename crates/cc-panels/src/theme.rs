//! The Plasma session's look (docs/plasma-look-design.md (a)). We read its active colour scheme
//! the way KConfig cascades it and boil it down to the few tokens our cards, tabs, bar, knobs
//! and taskbar paint with. That way Command Center matches whichever Plasma style is on (Breeze
//! Dark, Breeze Light, Breeze Classic, Twilight, Vapor, VGUI, a user scheme), light or dark.
//! The brand only lives in the accents: cyan, violet and magenta, fitted so they stay readable
//! on the scheme. If nothing's readable, we fall back to Breeze Dark.
//!   cards        the colour scheme (`win`)
//!   taskbar      Plasma's panel (`shell`). A desktop theme with its own `colors` file
//!                (breeze-dark, breeze-light, Vapor) paints the panel with those, whatever the
//!                scheme is (Twilight: light windows, dark panel).
//! The main loop polls the config files' mtimes a few times a second (`Watch`). A switch bumps
//! `generation()`, and anything drawn with an older one gets drawn again.
//! ponytail: the font family is read at startup only (assets.rs draws the tags with it), and
//! edits to a .colors file that isn't re-applied aren't watched (applying rewrites kdeglobals).
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

pub type Rgb = [f64; 3];

/// The brand's accents (nomadsgalaxy.com).
pub const CYAN: Rgb = [125.0, 249.0, 255.0]; // Warp Cyan #7DF9FF: remote machines
pub const VIOLET: Rgb = [164.0, 139.0, 255.0]; // #A48BFF: Frame windows
pub const MAGENTA: Rgb = [255.0, 92.0, 243.0]; // #FF5CF3: close

/// One set of colours, either a scheme's (for cards) or Plasma's panel's (for the taskbar).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tokens {
    pub frame: Rgb,   // Header BackgroundNormal: the card's frame and its tabs (a titlebar)
    pub surface: Rgb, // Window BackgroundNormal
    pub raised: Rgb,  // Button BackgroundNormal: the grab bar, the knobs
    pub text: Rgb,    // Header ForegroundNormal: text on `frame`
    pub dim: Rgb,     // Header ForegroundInactive: secondary text on `frame`, grip dots
    pub wtext: Rgb,   // Window ForegroundNormal: text on `surface` (the taskbar's chips)
    pub wdim: Rgb,    // Window ForegroundInactive: secondary text on `surface`
    pub border: Rgb,  // 1 px outlines at rest: frame and text mixed, the way Breeze does its frames
    pub dark: bool,
}

/// A brand colour fitted to a token set.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Accent {
    pub line: Rgb,         // lines and solid fills: the brand colour, darkened (light schemes) or lightened just enough to reach 3:1
    pub fill: (Rgb, f64),  // hover and selected tints
    pub glow: f64,         // the brand's soft glow: its peak alpha, in `line`
    pub ink: Rgb,          // glyphs on a solid `line` fill: Breeze's dark or light text (or black or white), whichever reads
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub win: Tokens,
    pub shell: Tokens,
    pub radius: f64, // corner radii in Breeze pixels: 5 for the Breeze widget style, 2 for square ones (VGUI's Windows)
}

/// WCAG relative luminance of an sRGB colour (0..255).
pub fn luminance(c: Rgb) -> f64 {
    let lin = |v: f64| {
        let v = v / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

const DARK_TEXT: Rgb = [35.0, 38.0, 41.0]; // Breeze Light's text
const LIGHT_TEXT: Rgb = [252.0, 252.0, 252.0]; // Breeze Dark's text

impl Tokens {
    /// Mixes `rgb` toward white (dark schemes) or black (light ones) just enough to hit `ratio`
    /// against both the frame and the surface. It bisects on the mix, so the hue stays.
    fn fit(&self, rgb: Rgb, ratio: f64) -> Rgb {
        let to = if self.dark { [255.0; 3] } else { [0.0; 3] };
        let ok = |t: f64| {
            let c = mix(rgb, to, t);
            contrast(c, self.frame).min(contrast(c, self.surface)) >= ratio
        };
        if ok(0.0) {
            return rgb;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..30 {
            let mid = (lo + hi) / 2.0;
            if ok(mid) { hi = mid } else { lo = mid }
        }
        mix(rgb, to, hi).map(if self.dark { f64::ceil } else { f64::floor }) // round to whole values without dropping back under the ratio
    }

    pub fn accent(&self, rgb: Rgb) -> Accent {
        let line = self.fit(rgb, 3.0); // WCAG 1.4.11, non-text
        let best = |a: Rgb, b: Rgb| if contrast(a, line) >= contrast(b, line) { a } else { b };
        // Breeze's text colours, or black or white where neither reaches 4.5:1. That happens
        // with an accent fitted to just 3:1 on a light scheme.
        let ink = Some(best(DARK_TEXT, LIGHT_TEXT)).filter(|&c| contrast(c, line) >= 4.5).unwrap_or_else(|| best([0.0; 3], [255.0; 3]));
        if self.dark {
            Accent { line, fill: (rgb, 0.22), glow: 0.35, ink }
        } else {
            Accent { line, fill: (line, 0.16), glow: 0.2, ink }
        }
    }
}

// ------------------------------------------------------------------ reading the scheme

/// An ini file as (section, key, value), in order. A section with a state suffix
/// ("[Colors:Header][Inactive]") keeps the suffix in its name, so it never matches the plain one.
struct Ini(Vec<(String, String, String)>);

fn parse(text: &str) -> Ini {
    let mut sec = String::new();
    let mut all = Vec::new();
    for l in text.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(s) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            sec = s.to_string();
        } else if let Some((k, v)) = l.split_once('=') {
            all.push((sec.clone(), k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ini(all)
}

fn read(p: &Path) -> Ini {
    parse(&std::fs::read_to_string(p).unwrap_or_default())
}

/// The first file in the chain that has the key wins (KConfig's cascade).
fn lookup<'a>(chain: &'a [Ini], sec: &str, key: &str) -> Option<&'a str> {
    chain.iter().find_map(|f| f.0.iter().find(|(s, k, _)| s == sec && k == key).map(|e| e.2.as_str()))
}

/// A colour the way KConfig writes it: "r,g,b", "r,g,b,a" (alpha ignored) or "#rrggbb". If it
/// doesn't parse, it counts as missing and the next file's is used.
/// ponytail: no other QColor names ("red", "#rgb"); none seen in a scheme.
fn rgb(chain: &[Ini], sec: &str, key: &str) -> Option<Rgb> {
    chain.iter().find_map(|f| {
        let s = lookup(std::slice::from_ref(f), sec, key)?;
        if let Some(h) = s.strip_prefix('#').filter(|h| h.len() == 6 && h.is_ascii()) {
            let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(f64::from);
            return Some([c(0)?, c(2)?, c(4)?]);
        }
        let v: Vec<f64> = s.split(',').map(|n| n.trim().parse().ok()).collect::<Option<_>>()?;
        ((3..=4).contains(&v.len()) && v.iter().all(|c| (0.0..=255.0).contains(c))).then(|| [v[0], v[1], v[2]])
    })
}

/// Breeze Dark's own colours (BreezeDark.colors), at the bottom of every chain.
const BREEZE_DARK: &str = "
[Colors:Window]
BackgroundNormal=42,46,50
ForegroundNormal=252,252,252
ForegroundInactive=161,169,177
[Colors:Header]
BackgroundNormal=49,54,59
ForegroundNormal=252,252,252
[Colors:Button]
BackgroundNormal=49,54,59
";

fn tokens(chain: &[Ini]) -> Tokens {
    let builtin = [parse(BREEZE_DARK)];
    // A scheme without [Colors:Header] (Vapor, Breeze Classic) uses [Colors:Window]'s, same as KDE.
    let col = |sec: &str, key: &str| {
        let header = |c: &[Ini]| rgb(c, sec, key).or_else(|| (sec == "Colors:Header").then(|| rgb(c, "Colors:Window", key)).flatten());
        header(chain).or_else(|| header(&builtin)).unwrap_or([128.0; 3])
    };
    let (frame, text, surface) = (col("Colors:Header", "BackgroundNormal"), col("Colors:Header", "ForegroundNormal"), col("Colors:Window", "BackgroundNormal"));
    Tokens {
        frame,
        surface,
        raised: col("Colors:Button", "BackgroundNormal"),
        text,
        dim: col("Colors:Header", "ForegroundInactive"),
        wtext: col("Colors:Window", "ForegroundNormal"),
        wdim: col("Colors:Window", "ForegroundInactive"),
        border: mix(frame, text, 0.25),
        dark: luminance(surface) < 0.18,
    }
}

/// The first of `rel` under the data dirs that exists.
fn find(data: &[PathBuf], rel: &str) -> Option<PathBuf> {
    data.iter().map(|d| d.join(rel)).find(|p| p.is_file())
}

/// The theme for a session config dir (its XDG_CONFIG_HOME) and data dirs (where schemes and
/// desktop themes are installed), plus a line saying what it is and its font family.
fn load(dir: &Path, data: &[PathBuf]) -> (Theme, String, String) {
    let mut win = vec![read(&dir.join("kdeglobals")), read(&dir.join("kdedefaults/kdeglobals"))];
    // A scheme file's own [General] ColorScheme can be wrong (Vapor's says Breeze Dark), so only ours count.
    let scheme = lookup(&win, "General", "ColorScheme").map(str::to_string);
    if let Some(f) = scheme.as_ref().and_then(|s| find(data, &format!("color-schemes/{s}.colors"))) {
        win.push(read(&f));
    }
    let rc = [read(&dir.join("plasmarc")), read(&dir.join("kdedefaults/plasmarc"))];
    let panel = lookup(&rc, "Theme", "name").unwrap_or("default").to_string();
    let mut shell = Vec::new();
    if let Some(f) = find(data, &format!("plasma/desktoptheme/{panel}/colors")) {
        shell.push(read(&f));
    }
    let style = lookup(&win, "KDE", "widgetStyle").unwrap_or("Breeze");
    let radius = if style.eq_ignore_ascii_case("breeze") { 5.0 } else { 2.0 };
    let w = tokens(&win);
    shell.extend(win);
    let t = Theme { win: w, shell: tokens(&shell), radius };
    let font = lookup(&shell, "General", "font").and_then(|f| f.split(',').next()).filter(|f| !f.is_empty()).unwrap_or("Noto Sans");
    let say = format!(
        "theme: {} ({}), panel {panel}{}",
        scheme.as_deref().unwrap_or("Breeze Dark, built in"),
        if t.win.dark { "dark" } else { "light" },
        if t.shell != t.win { " (its own colours)" } else { "" }
    );
    (t, say, font.to_string())
}

impl Default for Theme {
    /// Breeze Dark.
    fn default() -> Theme {
        let t = tokens(&[]);
        Theme { win: t, shell: t, radius: 5.0 }
    }
}

/// The session's config dir and the data dirs. cc-panels runs in the container, so the host's
/// /usr/share shows up as /run/host/usr/share.
fn session() -> (PathBuf, Vec<PathBuf>) {
    let home = PathBuf::from(crate::config::home_dir());
    let data = vec![home.join(".local/share"), "/usr/share".into(), "/run/host/usr/share".into()];
    (home.join(".config/control-center/desktop"), data)
}

static THEME: Mutex<Option<Theme>> = Mutex::new(None);
static GEN: AtomicU32 = AtomicU32::new(0);
static FONT: OnceLock<String> = OnceLock::new();

/// The theme now (Breeze Dark until `init`).
pub fn get() -> Theme {
    THEME.lock().unwrap().unwrap_or_default()
}

/// Bumped by every theme switch, so anything drawn with an older one gets drawn again.
pub fn generation() -> u32 {
    GEN.load(Relaxed)
}

/// The font family for assets.rs: [General] font=, or Noto Sans.
pub fn font() -> String {
    FONT.get().cloned().unwrap_or_else(|| "Noto Sans".into())
}

fn set(t: Theme, say: &str) {
    eprintln!("{say}");
    *THEME.lock().unwrap() = Some(t);
    GEN.fetch_add(1, Relaxed);
}

/// Reads the session's theme. This runs at startup, before assets.rs, since that needs the font.
pub fn init() -> Watch {
    let (dir, data) = session();
    let (t, say, font) = load(&dir, &data);
    let _ = FONT.set(font); // changing the family needs a restart, since the tags are drawn with it
    set(t, &say);
    Watch::new(dir, data)
}

/// A test hook: one scheme file as the whole chain (the dumps' CC_SCHEME).
#[cfg(test)]
pub fn from_scheme(path: &str) -> Theme {
    let t = tokens(&[read(Path::new(path))]);
    Theme { win: t, shell: t, radius: if path.contains("VGUI") { 2.0 } else { 5.0 } }
}

/// A test hook: a desktop theme's `colors` in front of a scheme, the way Plasma's panel paints
/// with it (the taskbar dump's CC_PLASMA_THEME).
#[cfg(test)]
pub fn with_panel(scheme: &str, panel: &str) -> Tokens {
    tokens(&[read(Path::new(panel)), read(Path::new(scheme))])
}

/// Watches the files a theme switch writes (from the colour KCM, or the global theme's).
pub struct Watch {
    dir: PathBuf,
    data: Vec<PathBuf>,
    seen: Vec<Option<SystemTime>>,
    pending: bool,
}

impl Watch {
    fn new(dir: PathBuf, data: Vec<PathBuf>) -> Watch {
        let mut w = Watch { dir, data, seen: Vec::new(), pending: false };
        w.seen = w.stamps();
        w
    }

    fn stamps(&self) -> Vec<Option<SystemTime>> {
        ["kdeglobals", "kdedefaults/kdeglobals", "plasmarc", "kdedefaults/plasmarc"]
            .map(|f| std::fs::metadata(self.dir.join(f)).and_then(|m| m.modified()).ok())
            .to_vec()
    }

    /// Called a few times a second. A change only loads once it has held still for one more
    /// poll, because the KCM can write a file twice. Returns Some(theme, what it is) once per switch.
    fn check(&mut self) -> Option<(Theme, String, String)> {
        let now = self.stamps();
        if now != self.seen {
            (self.seen, self.pending) = (now, true);
            return None;
        }
        if !std::mem::take(&mut self.pending) {
            return None;
        }
        Some(load(&self.dir, &self.data))
    }

    pub fn poll(&mut self) {
        if let Some((t, say, _)) = self.check() {
            if t != get() {
                set(t, &say);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: &str = "[Colors:Window]\nBackgroundNormal=42,46,50\nForegroundNormal=252,252,252\nForegroundInactive=161,169,177\n[Colors:Header]\nBackgroundNormal=49,54,59\nForegroundNormal=252,252,252\n[Colors:Button]\nBackgroundNormal=49,54,59\n";
    const LIGHT: &str = "[Colors:Button]\nBackgroundNormal=252,252,252\n[Colors:Header]\nBackgroundNormal=222,224,226\nForegroundNormal=35,38,41\n[Colors:Window]\nBackgroundNormal=239,240,241\nForegroundInactive=112,125,138\nForegroundNormal=35,38,41\n[General]\nColorScheme=BreezeLight\n";
    // Vapor.colors: no [Colors:Header], and its [General] names the wrong scheme
    const VAPOR: &str = "[Colors:Button]\nBackgroundNormal=36,39,44\n[Colors:Window]\nBackgroundNormal=36,39,44\nForegroundInactive=189,195,199\nForegroundNormal=241,241,242\n[General]\nColorScheme=Breeze Dark\nName=Vapor\n";
    // VGUI.colors, with its state-suffixed header
    const VGUI: &str = "[Colors:Button]\nBackgroundNormal=77,88,69\n[Colors:Header]\nBackgroundNormal=77,88,69\nForegroundNormal=255,255,255\n[Colors:Header][Inactive]\nBackgroundNormal=1,1,1\n[Colors:Window]\nBackgroundNormal=77,88,69\nForegroundInactive=161,169,177\nForegroundNormal=252,252,252\n";

    /// A scratch session: files (path, contents) under a fresh dir. Returns (config dir, data dirs).
    fn scratch(name: &str, files: &[(&str, &str)]) -> (PathBuf, Vec<PathBuf>) {
        let root = std::env::temp_dir().join(format!("cc-theme-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (p, text) in files {
            let p = root.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        (root.join("config"), vec![root.join("data")])
    }

    #[test]
    fn cascade() {
        // Nothing at all: Breeze Dark.
        let (dir, data) = scratch("none", &[]);
        let (t, ..) = load(&dir, &data);
        assert_eq!(t, Theme::default());
        assert_eq!((t.win.frame, t.win.surface, t.win.text, t.win.dark, t.radius), ([49.0, 54.0, 59.0], [42.0, 46.0, 50.0], [252.0; 3], true, 5.0));
        // This Frame's layout: the name lives in kdedefaults, and the user's kdeglobals has a key of its own.
        let (dir, data) = scratch(
            "vapor",
            &[
                ("config/kdeglobals", "[General]\nColorSchemeHash=465905\n[Colors:Window]\nForegroundInactive=1,2,3\nBackgroundNormal=oops\n"),
                ("config/kdedefaults/kdeglobals", "[General]\nColorScheme=Vapor\n[KDE]\nwidgetStyle=Breeze\n"),
                ("data/color-schemes/Vapor.colors", VAPOR),
            ],
        );
        let (t, say, _) = load(&dir, &data);
        assert_eq!(t.win.dim, [1.0, 2.0, 3.0], "the user's key beats the scheme's");
        assert_eq!(t.win.surface, [36.0, 39.0, 44.0], "a value that isn't r,g,b counts as missing");
        assert_eq!(t.win.frame, [36.0, 39.0, 44.0], "no Header: Window's");
        assert_eq!(t.win.text, [241.0, 241.0, 242.0]);
        assert!(say.contains("Vapor (dark)"), "{say}");
        // VGUI: square, and [Colors:Header][Inactive] is ignored.
        // KConfig's other forms: with an alpha, and #hex.
        let (dir, data) = scratch("forms", &[("config/kdeglobals", "[Colors:Window]\nBackgroundNormal=#2a2e32\n[Colors:Header]\nBackgroundNormal=42,46,50,255\n")]);
        let (t, ..) = load(&dir, &data);
        assert_eq!((t.win.surface, t.win.frame), ([42.0, 46.0, 50.0], [42.0, 46.0, 50.0]));
        // Each foreground goes with its background: a dark header on a light window.
        let (dir, data) = scratch(
            "split",
            &[("config/kdeglobals", "[Colors:Header]\nBackgroundNormal=30,30,30\nForegroundNormal=250,250,250\nForegroundInactive=200,200,200\n[Colors:Window]\nBackgroundNormal=240,240,240\nForegroundNormal=20,20,20\nForegroundInactive=90,90,90\n")],
        );
        let (t, ..) = load(&dir, &data);
        let w = t.win;
        assert!(contrast(w.text, w.frame) > 4.5 && contrast(w.dim, w.frame) > 4.5, "{w:?}");
        assert!(contrast(w.wtext, w.surface) > 4.5 && contrast(w.wdim, w.surface) > 4.5, "{w:?}");
        let (dir, data) = scratch("vgui", &[("config/kdeglobals", "[General]\nColorScheme=VGUI\n[KDE]\nwidgetStyle=Windows\n"), ("data/color-schemes/VGUI.colors", VGUI)]);
        let (t, ..) = load(&dir, &data);
        assert_eq!((t.win.frame, t.radius), ([77.0, 88.0, 69.0], 2.0));
    }

    #[test]
    fn twilight() {
        // Breeze Twilight: the Breeze Light scheme with the breeze-dark desktop theme (which has colours).
        let (dir, data) = scratch(
            "twilight",
            &[
                ("config/kdeglobals", "[General]\nColorScheme=BreezeLight\n"),
                ("config/kdedefaults/plasmarc", "[Theme]\nname=breeze-dark\n"),
                ("data/color-schemes/BreezeLight.colors", LIGHT),
                ("data/plasma/desktoptheme/breeze-dark/colors", DARK),
            ],
        );
        let (t, say, _) = load(&dir, &data);
        assert!(!t.win.dark && t.shell.dark, "{t:?}");
        assert_eq!((t.win.frame, t.shell.frame), ([222.0, 224.0, 226.0], [49.0, 54.0, 59.0]));
        assert!(say.contains("panel breeze-dark (its own colours)"), "{say}");
        // Breeze's default desktop theme has no colours, so the panel follows the scheme.
        let (dir, data) = scratch("light", &[("config/kdeglobals", "[General]\nColorScheme=BreezeLight\n"), ("data/color-schemes/BreezeLight.colors", LIGHT)]);
        let (t, ..) = load(&dir, &data);
        assert_eq!(t.shell, t.win);
    }

    #[test]
    fn accents_stay_readable() {
        for (name, scheme) in [("dark", DARK), ("light", LIGHT), ("vapor", VAPOR), ("vgui", VGUI)] {
            let t = tokens(&[parse(scheme)]);
            for brand in [CYAN, VIOLET, MAGENTA] {
                let a = t.accent(brand);
                for on in [t.frame, t.surface] {
                    assert!(contrast(a.line, on) >= 3.0, "{name}: {brand:?} -> {:?} on {on:?}: {:.2}", a.line, contrast(a.line, on));
                }
                assert!(contrast(a.ink, a.line) >= 4.5, "{name}: ink on {:?}", a.line);
            }
        }
        let dark = tokens(&[parse(DARK)]);
        assert_eq!([CYAN, VIOLET, MAGENTA].map(|c| dark.accent(c).line), [CYAN, VIOLET, MAGENTA], "Breeze Dark keeps the brand's own");
        let light = tokens(&[parse(LIGHT)]);
        let cyan = light.accent(CYAN).line;
        assert!(cyan[0] < cyan[1] && cyan[1] < cyan[2] + 1.0 && cyan[1] < 200.0, "darkened, still cyan: {cyan:?}");
        assert_eq!(dark.accent(MAGENTA).ink, DARK_TEXT, "close-hover: a dark x on magenta");
    }

    #[test]
    fn poll_reloads_once() {
        let (dir, data) = scratch("poll", &[("config/kdeglobals", DARK)]);
        let mut w = Watch::new(dir.clone(), data);
        assert!(w.check().is_none(), "nothing changed");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("kdeglobals"), LIGHT).unwrap();
        assert!(w.check().is_none(), "changed: wait one more poll");
        let (t, ..) = w.check().expect("held still: loaded");
        assert!(!t.win.dark);
        assert!(w.check().is_none(), "once");
    }
}
