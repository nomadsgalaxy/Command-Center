# Plasma look: design

Command Center's own UI (the cards around each panel, the window controls, the grab bar, the knobs and the taskbar) had its own NomadsGalaxy style, glass and Starlight. Next to a streamed Plasma window it looked like a different app, so I made it look as much like KDE Plasma as I could.

> **My decisions (2026-10-02):** CC's own UI looks as much like KDE Plasma as possible, and
> it follows **whichever Plasma style is active** (Breeze Dark, Breeze Light, Breeze Classic,
> Breeze Twilight, Vapor, VGUI, or a user scheme), light or dark. If nothing can be read, it
> uses Breeze Dark. NomadsGalaxy stays as the accents only: violet #A48BFF for Frame windows,
> cyan #7DF9FF for remote machines, Magenta #FF5CF3 for close-hover, plus a subtle accent
> glow. Surfaces, borders, text, shapes, radii and the font (Noto Sans, or the scheme's)
> follow Breeze. The layout stays the same: the folder tabs, and the bar and knobs under each
> panel. The taskbar becomes **one merged frame**: Plasma's bar on top and our chips as a
> second row. It is carried by an edge, resized by a corner, and bent by the curve knob, which
> moves into the frame. The size knob goes. CC follows a theme switch live.

The doc has four parts: (a) how cc-panels reads the active theme, (b) the restyle element by element, (c) the merged taskbar, and (d) the build plan, including what Stage B looked like once it was built.

## What was measured (this Frame, 2026-10-02)

Before designing anything, I looked at how Plasma actually stores its look on the Frame, because a few of the files don't behave the way you'd expect.

- **The colour scheme cascades.** The session's `kdeglobals`
  (`~/.config/control-center/desktop/kdeglobals`) already holds the full `[Colors:*]` sections,
  because the colour KCM copies the scheme's colours into it when you apply a scheme. Its
  `[General]` section only has `ColorSchemeHash`. The scheme's name lives in
  `kdedefaults/kdeglobals` (`ColorScheme=Vapor`).
- **Scheme files can be missing sections.** `Vapor.colors` and `BreezeClassic.colors` have no
  `[Colors:Header]`. `VGUI.colors` has `[Colors:Header][Inactive]`. A scheme file's own
  `[General] ColorScheme=` can be wrong, too: Vapor's says "Breeze Dark". So I never trust it.
- **Plasma's panel can use different colours from the apps.** A desktop theme that ships a
  `colors` file forces those colours on the panel and its popups:
  - `breeze-dark`, `breeze-light` and `Vapor` ship one.
  - `default` (Breeze) doesn't, so it follows the scheme.
  - `kdedefaults/plasmarc` currently says `[Theme] name=Vapor`.
  - The look-and-feel package `org.kde.breezetwilight` is ColorScheme=BreezeLight with
    `plasmarc name=breeze-dark`: light windows, dark panel. So our cards have to follow the
    scheme and our taskbar frame has to follow the panel, or Twilight looks wrong.
- **Shapes depend on the widget style.** `com.valve.vgui` sets `[KDE] widgetStyle=Windows`
  (square corners), and the others set `Breeze` (rounded).
- **Font.** There's no `[General] font=`, which means Plasma's default, Noto Sans 10 pt.
  `fc-match 'Noto Sans'` gives `/usr/share/fonts/noto/NotoSans-Regular.ttf`.
- **Plasma's panel** (plasmashellrc) is `floating=1`, `panelLengthMode=1` (fit content) and
  thickness 50. Its applets are Kickoff, the pager, icontasks, the system tray, the **digital
  clock** and show-desktop.
- **The cursor checks our bar first.** `kvm.rs land()` tests our bar (`bar`) before Plasma's
  (`plasma`). They never overlapped before, but once Plasma's bar sits inside our frame, they
  will.

## (a) The theme module: `theme.rs`

### Lookup

Every token is a lookup of `(section, key)` through a list of ini files. The first file that
has the key wins, which is how KConfig cascades too:

1. `~/.config/control-center/desktop/kdeglobals`. cc-panels runs in the container, so this path
   is fixed and doesn't depend on its own `XDG_CONFIG_HOME`.
2. `~/.config/control-center/desktop/kdedefaults/kdeglobals`.
3. The named scheme file: `ColorScheme` from (1), else from (2). It's looked up in
   `~/.local/share/color-schemes/<name>.colors`, then `/usr/share/color-schemes/<name>.colors`.
4. The built-in Breeze Dark: a `const` copy of the keys below from `BreezeDark.colors`, so
   there's always an answer.

Missing sections and keys:
- A missing `[Colors:Header]` key falls back to the same key in `[Colors:Window]`, as KDE
  does.
- Sections with a state suffix (`[Colors:Header][Inactive]`) are ignored.
- A value that doesn't parse as `r,g,b` counts as missing.

The parser is about 25 lines: a `[section]` line, `key=value` lines, and `#` comments
skipped. It doesn't need a new dependency.

The taskbar frame and chips use a second chain, the **shell chain**, so they match Plasma's
panel rather than the apps:
- Find the desktop theme's name: `[Theme] name` from `desktop/plasmarc`, then
  `desktop/kdedefaults/plasmarc`, else `default`.
- If `~/.local/share/plasma/desktoptheme/<name>/colors` or
  `/usr/share/plasma/desktoptheme/<name>/colors` exists, it goes in front of the chain above.
- Otherwise the shell tokens are just the window tokens.

### Tokens

The `Theme` struct is `Copy`: two `Tokens` sets (`win` and `shell`) plus a few flags.

| token | key (section `[Colors:X]`) | used for |
|---|---|---|
| `frame` | Header BackgroundNormal | card frame and both folder tabs (Breeze titlebar) |
| `surface` | Window BackgroundNormal | taskbar frame (shell), theater backdrop base |
| `raised` | Button BackgroundNormal | grab bar, knobs, chip hover base |
| `text` | Header ForegroundNormal (on `frame`); Window ForegroundNormal (on `surface`) | tag name, glyphs v ^ x, knob glyphs, chip labels |
| `dim` | Window ForegroundInactive | tag's "Monitor 2", grip dots, minimized chips |
| `border` | computed: `mix(frame, text, 0.25)` | 1 px outlines at rest (Breeze's frame outline is a bg/fg mix; check it against a dump) |
| `dark` | luminance(`surface`) < 0.18 | glow strength, accent fitting direction |
| `radius` | `[KDE] widgetStyle`: Breeze → 5 bp, anything else → 2 bp | corner radii (bp: see (b)) |
| `font` | `[General] font` up to the first comma, default `Noto Sans` | passed to assets.rs |

**Keys I deliberately don't read**, because the brand accents replace them (decision 2):
DecorationFocus, DecorationHover, `[Colors:Selection]`, ForegroundActive, ForegroundNegative,
and Plasma 6's `[General] AccentColor`.

### Accents on any scheme

The brand colours are tuned for a dark background. On Breeze Light, cyan on near-white is
barely visible, so each accent gets fitted to the surface it sits on.
`theme.accent(rgb, on: [f64; 3]) -> Accent` derives these:

- `line`: the brand colour, mixed toward black on a light surface (white on a dark one) just
  enough to reach **3:1** contrast with `on` (WCAG 1.4.11, non-text). It finds the mix by
  bisection on the mix factor, so the hue stays the same.
- `text`: the same, to **4.5:1**. It's only used if a stage needs accent text, and none does
  now.
- `fill`: `(rgb, 0.22)` on dark, `(line, 0.16)` on light. Used for hover and selected tints.
- `glow`: `(line, 0.35)` peak on dark, `0.2` on light. This is the brand touch.
- `ink`: whichever of Breeze's (35,38,41) or (252,252,252) contrasts more with `line`. It's
  the glyph colour on a solid accent fill.

The fits I computed for `line` (contrast against the scheme's Window background):

| scheme | cyan | violet | magenta |
|---|---|---|---|
| Breeze Dark (42,46,50) | unchanged, 11:1 | unchanged, 5.0:1 | unchanged, 5.3:1 |
| Vapor (36,39,44) | unchanged, 12:1 | unchanged, 5.5:1 | unchanged, 5.8:1 |
| VGUI (77,88,69) | unchanged, 6.0:1 | (171,148,255), lightened | (255,100,244) |
| Breeze Light / Classic (239,240,241) | (75,150,154) | (145,123,226) | (221,80,210) |

On magenta, `ink` is the dark one (5.9:1), since white would only give 2.5:1. So close-hover
shows a dark x on a magenta circle, where Breeze shows a white x on red.

### Live reload

Switching the theme in System Settings should restyle CC right away, without a restart:

- `theme::poll()` runs from the main loop every 30 ticks (about 3 times a second).
- It `stat`s the four config files (the two kdeglobals and the two plasmarc).
- When their mtimes change and then stay the same for one more poll, it reloads. It waits that
  extra poll because the KCM can write a file twice.
- It stores the new `Theme` in `static THEME: Mutex<Theme>`, bumps `static GEN: AtomicU32`,
  and logs `theme: Breeze Light (light), panel breeze-dark`.

Readers call `theme::get()` (a copy) and compare `theme::gen()` with the generation they drew
with:
- A grab.rs `Drawn` gets a `gen` field. A different gen sets `again`, so the card is redrawn
  once.
- The taskbar's drawn state carries the gen, so the chips are repainted once.
- `Grab` re-uploads the theater backdrop's 4×4 texture once.

Tags are theme-free masks (see (b)), so a switch doesn't need an assets.rs run.

I don't use the D-Bus signal `org.kde.KGlobalSettings.notifyChange`. It lives on the desktop
session's bus (`/run/user/$UID/cc-desktop`), and polling 4 `stat`s is simpler.

> ponytail: a font *family* change takes effect at the next start. main.rs passes the font to
> assets.rs and deletes `assets/tag-app-*` when the font stamp (`assets/font`) differs. Edits
> to a user `.colors` file that isn't re-applied aren't watched. Re-applying it in the KCM
> rewrites kdeglobals, so that case is covered.

## (b) Restyle, element by element

### The unit: one Breeze pixel (bp)

To copy Breeze's measurements, I needed a unit that maps its pixels onto our cards. The tag
text is 0.4 g high and stands in for Breeze's 10 pt title font (13.3 px), so
**1 bp = 0.03 g** (g is the card's frame width from `chrome()`).

The layout's geometry (`legend`, `controls`, `bar_box`, `knob_at`, LINE, TAB) **stays as it
is**. Only the paint changes, so their tests and hit zones don't move.

### Card (`card()`, `CardSpec::pixel`)

- **Shape.** An opaque `frame` band from the picture's edge to LINE (0.3 g = 10 bp). Before,
  it was glass at 0.55 alpha. The outer edge becomes a rounded rectangle of radius `radius`:
  5 bp = 0.15 g, or 2 bp for widgetStyle Windows.
  - It's the box SDF of `w/2 + LINE·g − r` by `h/2 + LINE·g − r`, minus r.
  - It replaces the old distance-from-the-picture, which rounded the corners at 0.3 g.
  - Beyond the edge, the card stays transparent laser reach out to g, as before.
- **Border.** 1 bp (`half = max(0.015 g · px, 0.5)`).
  - Rest: `border`.
  - Lit: `accent.line`.
  - Carry: 2 bp `accent.line`.
- **Glow.** Lit and Carry only: outside the border, 0.3 g of falloff, peak `accent.glow`.
- **Corner grip** (hover or resize): it used to be a thick Starlight arc. It becomes 3 bp of
  `accent.line` along the middle 60° of the corner's arc.
- **Folder tabs** (the tag's and the controls'): the same `frame` fill and border, joined to
  the band, so they read as a Breeze titlebar.
  - Their outer corners use the same `radius` box SDF.
  - The concave shoulder stays `smin`, with k = 0.15 g (it was 0.25 g).
  - There's no seam: the tab and the band are one distance field, as before.

### Tag (crates/cc-panels/src/assets.rs `tag()`, `glyphs()`)

The text is set in Noto Sans Regular (weight 400) from `font=<family>`, found through
`fc-match -f '%{file}' '<family>'`, and written as typed:
- No uppercase, no tracking, no PROXON, no text glow.
- Size 44 px in the 64-px-high image, as before, so `legend()` keeps its proportions.
- Layout: `desktop · Monitor 2`.

The image becomes a **mask**:
- R = the name's coverage.
- G = the secondary text's ("Monitor 2").
- B = the separator dot.
- A = max of the three.

grab.rs tints it at draw time: `text`·R + `dim`·G + `accent.line`·B. `average()` stays as it
is and a `tint()` runs after it. Glyphs (the "+N" digits) are R only. That way tags and glyphs
are the same for every theme, and a theme switch never redraws them. The header format is
unchanged. Old cached coloured tags get replaced at startup, because main.rs reruns assets.rs,
and windows.rs's get replaced through the font stamp above.

### Window controls v ^ x (`ctl_pixel`): Breeze decoration buttons

- **Button.** A circle of 18 bp diameter (0.54 g), centred where it was. The hit box stays
  ±0.3 g.
- **Glyph.** 10 bp wide (`s = 0.15`, was 0.11), with a 1.5 bp stroke (`half 0.0225 g`).
  - Minimize is a v chevron, theater a ^ chevron, close an x. These are Breeze's own glyphs.
- **Looks:**

| look | v and ^ | close (x) |
|---|---|---|
| Rest | `text` glyph on the tab, no circle | same |
| Lit | `accent.fill` circle, `text` glyph | solid magenta `line` circle, `ink` glyph (Breeze's red circle, in brand magenta) |
| Carry (pressed) | solid `accent.line` circle, `accent.ink` glyph | same, in magenta |

There are no gradients. The "Warp mix" goes everywhere.

### Grab bar (`bar_pixel`): a Breeze scrollbar handle

- It stays a pill, since Breeze's scrollbar handles are full pills.
- Fill `raised`, 1 bp `border`.
- Three grip dots in `dim`, radius 1.5 bp.
- Lit: `accent.line` border, `accent.fill` over `raised`, and the glow.
- Carry: solid `accent.line` fill, `accent.ink` dots, and the glow.

### Knobs, curve and zoom (`knob_pixel`)

- The same as the bar, made round (Kirigami RoundButton). The radius doesn't change.
- Glyph stroke 1.5 bp in `text` (`accent.ink` on Carry).

### Theater backdrop

`mix(surface, black, 0.85)` at the same 0.88 alpha. A light scheme gets a tinted near-black,
because theater is still a dark room.

### The cursor

It doesn't change. A dot that has to read on any content isn't part of the theme.

## (c) The merged taskbar

Before this, Plasma's bar and our chip bar were two separate overlays, stacked one under the
other, each with its own controls. The decision was to make them one frame, so the taskbar
reads as a single Plasma panel with our row added.

### The frame

The taskbar is one overlay, `controlcenter.taskbar`. It's drawn in **bar units**: one unit is
one logical pixel of Plasma's panel, at `mpp` metres per unit, as plasmabar.rs already uses.
That way Breeze measurements apply 1:1, and Plasma's bar and our frame scale together.

```
 ┌──────────────────────────────────────────────┐  ← 1 u border (shell border), radius 8 u
 │ PAD                                          │    REACH (transparent laser reach) all round
 │   ┌──────── Plasma's bar (pw × ph) ───────┐  │    ← Plasma's overlay, 2 mm in front
 │   └───────────────────────────────────────┘  │
 │   ROW_GAP                                    │
 │   [desk-wide] [work-laptop] [Konsole] [+2]   │  ← chip row, ROW high, centred
 │                   ( ◡ )                      │  ← BOTTOM margin: the curve knob, centred
 └──────────────────────────────────────────────┘
```

| constant | value | meaning |
|---|---|---|
| `REACH` | 28 u | transparent ring that catches lasers: 17 mm at follow's 0.625 mm/u. A calibration knob. |
| `PAD` | 6 u | inside the border, around the content (between Kirigami's 4 and 8) |
| `ROW_GAP` | 4 u | between Plasma's bar and the chip row |
| `ROW` | 40 u | chip row; tiles 36 u high |
| `BOTTOM` | 30 u | under the chips, holding the knob (r = 10 u) |
| `RADIUS` | 8 u | the frame's corners. Calibrate against Plasma's panel corners in the live stream. |

Sizes:
- Content width `cw = max(pw, chips)`, where `pw` is Plasma's reported width (fit content, so
  it changes as apps open).
- Frame `W = cw + 2·PAD + 2·REACH`, `Hf = PAD + ph + ROW_GAP + ROW + BOTTOM + 2·REACH`.
- Plasma's bar and the chip row are each centred.
- With no Plasma panel (the session is down), the frame holds only the chip row.

**Texture.** It's `W·s × Hf·s` pixels, with `s = min(1, 1920/W, √(1.5e6/(W·Hf)))`, because
Plasma's bar can be up to 2560 wide. The overlay's width in metres is `W·mpp` whatever `s` is.
`SetOverlayMouseScale` is (W, Hf), so events arrive in units and `s` never reaches the hit
test. The frame is redrawn when (pw, ph), the chips or the theme generation change.

**Paint.**
- Fill `shell.surface`, opaque, inside the border. Transparent in the REACH ring.
- Plasma's rectangle is a **well**: `shell.surface` with a 1 u `shell.border` outline. Plasma's
  overlay covers it exactly. If its translucent background doesn't quite match our surface,
  the outline makes the seam look deliberate. Under IgnoreTextureAlpha, a translucent panel
  shows its straight RGB.
- Lit (a laser near, or held): border `accent.line` with VIOLET (the Frame's own) and the
  glow. Carry: 2 u border.

### Plasma's overlay sits in front of ours

- plasmabar.rs places Plasma's overlay at the **frame's** pose, offset by `(0, v_plasma, +0.002)`
  in the frame's own axes, where `v_plasma` is the well's centre minus the frame's centre in
  metres. The +0.002 is 2 mm toward you.
  - The Frame draws by depth, so it ends up in front.
  - Its sort order goes to `SORT + 1`, in case depth isn't used.
  - Lasers hit the nearer overlay, so anything inside the well goes to Plasma.
- Its popups still grow the crop upward from the bar (`shift()` doesn't change). They float
  over our frame's top edge and out past it, in front, and catch lasers there.
- `taskbar::stacked()` and `STACK` go. Instead, `place_now()` returns the **frame** anchor, and
  taskbar.rs passes `v_plasma` and the shared curve radius to `tick_plasma`.
  - Both overlays bend about their own centres on the same radius
    (`curvature(width, r)`), as before.
  - ponytail: while a popup shifts Plasma's overlay sideways, its arc isn't concentric with
    ours. That was already true, and popups are brief.
- **Fading.** At the wrist, our overlay fades. Plasma's would show as a tinted sheet below
  alpha 1 (IgnoreTextureAlpha), so it's shown only at alpha ≥ 0.99 and hidden below that.
  Before, it faded with ours.
- **Cursor.** `kvm.rs land()` checks `plasma` **before** `bar`, because Plasma's is now in front
  and inside the bar. `KVM.bar` is the whole frame's placement.

### Hit testing (`part_at(x, y)` in units from the top left, in this order)

1. Inside Plasma's well → `None`. Plasma's overlay normally takes these events, so this only
   happens before its first frame.
2. The knob, within 1.3 r → `Knob`.
3. A chip tile (`chip_at`, over the row's band) → `Chip(c)`.
4. Within `REACH + 12` of two outer edges → `Corner(k)`, using grab.rs's `CORNER_SIGN` order.
5. Anything else on the frame (the reach ring, the padding, the gaps between chips) →
   `Edge`.

A pure function, `hold_for(part, mode) -> Option<HoldKind>`, decides what a press does:

| part | fixed | follow / wrist |
|---|---|---|
| Edge | Carry | nothing (decision 4) |
| Corner | Resize | Resize |
| Knob | Curve | Curve |
| Chip | a click (toggle its panel) | same |

### Manipulation (taskbar.rs)

`drag` and `adjust` merge into one `hold: Option<Hold { dev, kind }>`:

- **Carry** (fixed only): `rel = inv(device)·pose`, and the pose rides on the device, as
  `grab::Mode::Move` does. Letting go saves `spots.home.taskbar`, which is the old
  `put_down()`.
- **Resize**: when you take a corner, it stores `(sx, sy)`, the grab offset `(gx, gy)` from
  the corner, `s0 = scale` and `W0`, the frame's width in metres.
  - While held, the laser's point on the frame's plane goes through the existing
    `grab::width_for(sx, sy, u−gx, v−gy, aspect)` (made `pub`).
  - `scale = (s0 · W'/W0).clamp(0.5, 3.0)`.
  - It grows about the centre, so both rows grow together (they share `mpp`). In fixed,
    `set_fixed` recomputes `metres`.
  - Letting go saves `taskbar_scale`, the existing key.
- **Curve**: the old `Adjust` for `Chip::Curve`, now taken from the knob: the point taken rides
  on the device, the joystick bends it 3 cm a notch, and letting go saves `taskbar_curve`.
- **Ending a hold**: a button-up on us, a release over another overlay (`grab.released`, as
  before), or lost tracking (500 ms, as `grab::LOST`).
- ponytail: lasers only. The mouse cursor still only clicks chips (`bar_clicks`), and an edge
  drag with the mouse would need kvm.rs to forward its drags. There's no joystick push/pull
  while carrying either. Panels have it, so I'll add it if anyone reaches for it.

### Chips: the Plasma task manager look

With Plasma's bar in the same frame, several of our chips repeat something it already does,
so they go:
- Grip, because edges carry.
- Size, because corners resize.
- Curve, which becomes the knob in the frame.
- Apps, because Kickoff is in Plasma's row.
- Clock, because Plasma's digital clock is right there.

What's left is `Chip::Panel(i)` and `Chip::More(n)`. `glyphs.rgba` stays, for "+N".

Each chip is a tile like Plasma 6's `tasks.svg` states:

| state | tile | label | indicator (3 u line on the tile's bottom edge, its panel's `accent.line`) |
|---|---|---|---|
| rest | none (the frame shows through) | `shell.text` | 40% of the tile's width, centred |
| hover | `accent.fill`, 1 u `accent.line` outline, radius 4 u | `shell.text` | 40% |
| lit (the keyboard's) | `accent.fill` at 1.5×, radius 4 u | `shell.text` | full width |
| dim (minimized or away) | none | `shell.dim` | none |

Measurements:
- Labels are the tag masks, tinted, at `LABEL = 13.3/44 ≈ 0.30`. That's Plasma's own 10 pt,
  the size of its clock text beside them.
- 10 u of padding each side, 4 u between tiles, at most 260 u wide, as before.
- Too many chips for 1920 still narrows the panel chips (`layout()` doesn't change).
- The cyan and violet accents live in the indicator and the hover, so you can tell remote from
  Frame at a glance.

### Hiding

This doesn't change:
- Hidden while a VR game runs (`GetCurrentSceneProcessId`).
- Hidden while every panel is hidden (HIDDEN).
- In theater mode: hidden in fixed, shown in follow and wrist.
- Hidden at alpha 0 at the wrist. An overlay at alpha 0 still catches lasers, so it's hidden,
  not just faded.

When ours hides, Plasma's hides with it (`tick_plasma(None)`). A hide during a hold ends the
hold where the bar is (as `grab.end`).

## (d) Build plan

I split it into two stages: the theme and the panel restyle first, since everything else
depends on the tokens, then the merged taskbar.

### Stage A: theme module and panel restyle

**Files:**
- `theme.rs` (new): the ini parser, the chains, `Theme`/`Tokens`/`Accent`, `poll`, `get`,
  `gen`, and the built-in Breeze Dark.
- `main.rs`: `mod theme`; load it before assets.rs; pass `font=`; `theme::poll()` in the loop;
  write the font stamp.
- `grab.rs`: `STAR`/`GLASS` go; `CardSpec.theme`; the paint changes in (b); the mask `tint()`;
  `Drawn.gen`; the backdrop colour.
- `crates/cc-panels/src/assets.rs`: the `font=` arg, Noto Sans, mask output, no glow or tracking.
- `windows.rs`: `font=` on its assets.rs call.
- `taskbar.rs`: mechanical changes only. Its `STAR`/`GLASS` become `shell` tokens and labels
  go through `tint()`, so it keeps rendering until stage B.

**Tests** (`cargo test --release -p cc-panels`, zero warnings). The fixtures are inline
snippets of the real files, so the tests don't depend on what's installed:
- `theme::cascade`: a user key beats kdedefaults; a missing key comes from the scheme named in
  kdedefaults; Vapor's missing Header gives Window; `[Colors:Header][Inactive]` is ignored;
  no files at all gives the Breeze Dark constants.
- `theme::twilight`: scheme BreezeLight plus plasmarc `breeze-dark` (with a `colors` file) →
  `win` light and `shell` dark.
- `theme::accents_stay_readable`: for the Dark, Light, Vapor and VGUI fixtures, `line` reaches
  at least 3:1 against `frame` and `surface`, and `ink` at least 4.5:1 against `line`. Breeze
  Dark keeps the exact brand hex.
- `theme::poll_reloads_once`: in a temp dir, a rewrite raises `gen` once, and only after the
  stable poll.
- `grab::card_textures_fit_and_stay_crisp`: runs under Breeze Dark **and** Breeze Light (the
  patch equals the full redraw in both), plus `tag_mask_tints` (R → text, G → dim,
  B → accent line).
- `taskbar::the_bar_draws_its_chips`, with mask labels.

**Previews.** `dump` reads `CC_SCHEME=<.colors path>` through a `theme::from_scheme(path)` test
hook, and writes `card_<look>_<scheme>_WxH.rgba`.

```
D=<scratch dir>
~/control-center/cc-box bash -c "cd ~/control-center && CC_ASSETS_OUT=$D cargo test --release -p cc-panels -- --ignored assets_draw"
for s in BreezeDark BreezeLight Vapor VGUI; do
  ~/control-center/cc-box bash -c "cd ~/control-center && CC_DUMP=$D CC_SCHEME=/usr/share/color-schemes/$s.colors \
    cargo test --release -p cc-panels -- --ignored dump"
done
# then PIL inside cc-box: Image.frombytes('RGBA',(w,h),...) per file, composited on mid-grey, and Read the PNGs
```

**What to look at:**
- Light and dark frames read as Breeze titlebars.
- The cyan and violet borders show on Breeze Light, and the tag reads.
- The close-hover magenta circle has a dark x.
- VGUI has square-ish corners.

### Stage B: merged taskbar, frame manipulation, chip restyle

**Files:**
- `taskbar.rs`: the frame layout (`frame()`), `part_at`, `hold_for`, `Hold`, the chip enum
  cut down, the task-manager chip paint, texture scale `s`, frame-anchor placement, and the
  `v_plasma` handoff. `stacked`, `STACK`, `GRIP`, `APPS`, `TEXT_PAD` and the Size/Apps/Clock
  code go.
- `plasmabar.rs`: `tick` takes the frame anchor plus the `(0, v_plasma, +0.002)` offset;
  `height()` becomes `size()` returning (w, h); sort order `SORT + 1`; hidden below alpha 1.
- `windows.rs`: `plasma_size()` and the `tick_plasma` signature.
- `kvm.rs`: `land()` checks `plasma` before `bar`.
- `grab.rs`: `width_for` and `CORNER_SIGN` become `pub`.

**Tests:**
- `frame_lays_out_around_plasma`: Plasma 891×50 with narrower chips → `W = 891 + 2·PAD +
  2·REACH`, centred. Wider chips → W comes from the chips. Plasma 2560 wide → the texture stays
  within 1920 and 1.5 MP.
- `frame_parts`: the well gives None. The knob, a chip, all four corners, an edge, and the gap
  between two chips (→ Edge) each give their part.
- `hold_for_modes`: Edge carries only in fixed. Corner and Knob work in all three modes.
- `corner_resize_scales_both_rows`: a corner dragged out 10% along the diagonal gives
  scale × 1.1, clamped to 0.5–3.
- `plasma_sits_in_its_well`: replaces `this_bar_stacks_under_plasmas`. It checks the offset in
  the frame's axes under follow's 25° tilt, and the 2 mm toward the eye.
- The existing follow and fixed tests stay.

**Previews.** `taskbar_dump` takes `CC_SCHEME` and `CC_PLASMA_THEME` (a desktoptheme `colors`
path, or nothing). The live stream can't be dumped offline, so it draws the frame with a
placeholder in the well: `shell.surface` with "Plasma" blocks. It shows:
- Breeze Dark.
- Breeze Light.
- Twilight: BreezeLight plus `/usr/share/plasma/desktoptheme/breeze-dark/colors`.
- Vapor plus the Vapor theme.

Each run includes rest, hover, lit and dim chips, the knob at rest and held, and the frame lit.

**What to look at:**
- Chips read at Plasma's size.
- Indicators are visible on light.
- The corner and edge zones are wide enough to aim at: overlay the hit map as a debug tint in
  the dump.

**Live checks for me** (we don't run cc-panels):
- Does the frame's surface match the streamed panel?
- Do the corner radii match? (`RADIUS`)
- Does the 50 px panel include the floating gap? If it does, set `floating=0` in the session's
  plasmashellrc, since our frame floats it anyway.
- Is `REACH` comfortable?

### Stage B as built (2026-10-02)

I built it as designed above, with these differences:

- **Corners are bigger.** `CORNER` is 28 u, not 12. A corner reaches `REACH + 28` in from each
  outer edge, a 56 u square. At 12 it was a 40 u square that mostly lay in the reach ring, and
  resizing has to be easy to find.
- **The grip.** Hovering a corner, or resizing from it, thickens that corner's stretch of the
  border to 3 u of violet, as on the cards.
- **Plasma's overlay.**
  - **Placement.** `taskbar::in_well(m, up)` computes Plasma's anchor, the frame's matrix moved
    by `(0, up, FRONT)`, and passes it to `tick_plasma`. plasmabar.rs still adds its popup
    shift. Its `tick` no longer takes an alpha.
  - **Fading.** While the wrist fades (alpha < 0.99), Plasma's overlay gets `None`, so it hides
    instead of showing as a tinted sheet. Its streams linger for 3 s, as for any hide.
  - **Sort order.** It sorts at `SORT + 1`.
  - **Releases.** A button-up on Plasma's overlay now ends a hold on the frame
    (`Plasma.released`, drained by `Windows::plasma_released`). Shrinking the frame from a
    corner can let go over Plasma's bar, and that release would otherwise be lost.
- **Joystick.** The taskbar overlay sets `SendVRDiscreteScrollEvents`. The old bar listened
  for scroll events but never got any. Now the joystick bends the frame by 3 cm a notch while
  the knob is held.
- **Wrist size.** `WRIST_MPP = 0.00044` m a unit replaced `WRIST_H`, keeping the old chip text
  size, 4 mm caps. With Plasma's bar in the frame, the wrist frame was about 0.42 m wide at
  scale 1, where before it was Plasma's 0.28 m plus our bar. That has changed since: the wrist
  frame now aims for `WRIST_W` = 0.22 m wide and never goes under `WRIST_MPP` = 0.00025 m a
  unit (taskbar.rs).
- **The well outline** is 1 u of `shell.border` just outside Plasma's rectangle, so it frames
  the stream instead of hiding under it.
- **"+N"** has no indicator. Only panel chips get one. A dim chip gets no indicator and a
  `shell.dim` label, unless it's hovered.
- **`frame()` and the mouse.** `frame()` returns the layout in units: the well, the row, the
  chip spans and the knob. `Scene` draws it at `tex_scale` pixels a unit.
  `SetOverlayMouseScale` is (W, Hf), so laser events and the mouse's `on_bar` (metres ÷ `mpp`)
  both arrive in units.
- **Removed:** `stacked`, `STACK`, `GRIP`, `APPS`, `TEXT_PAD`, `pill`, `clock()`,
  `Chip::{Grip, Curve, Size, Apps, Clock}`, `drag` and `adjust`. `glyphs.rgba` keeps `:` in
  its cell order.
- **The spot.** `spots.home.taskbar` is now the frame's centre, not Plasma's bar. An old saved
  spot puts the frame there, so Plasma's bar sits about 35 u higher than before.

**Tests:**
- `chips_lay_out_and_narrow_when_crowded`
- `frame_lays_out_around_plasma` (includes the 2560-wide texture limits)
- `frame_parts` (Plasma's area, the knob, a chip, between chips, the reach, the padding, all
  four corners at the tip and just inside the border, past a corner)
- `hold_for_modes`
- `corner_resize_scales_both_rows`
- `plasma_sits_in_its_well`
- `the_frame_draws_its_chips` (opaque frame, clear reach, label colour, the 40% and
  full-width indicators, none for a dim chip, no tile at rest)

The follow and fixed tests didn't change.

**Previews** (`taskbar_dump` → `taskbar_{rest,held,hits}_959x186.rgba`). They show Breeze
Dark, Breeze Light, Twilight (BreezeLight with breeze-dark's `colors`: a dark frame) and
Vapor with Vapor:
- Chips read at Plasma's size in Noto Sans.
- The cyan and violet indicators show on Light, darkened to teal and a deeper violet.
- On hover, the chip gets a tint and an accent outline. The keyboard's chip gets a stronger
  tint and a full-width line.
- The held knob is solid violet with a dark glyph.
- The hit map shows corners as 56 u squares, with edges everywhere else outside the chips and
  the knob.
- On Vapor, `ForegroundInactive` is close to its text colour, so a dim chip there mostly
  stands out by its missing indicator.

**Live checks** (we don't run cc-panels):
- Does the frame's surface match the streamed panel's?
- `RADIUS` against Plasma's own corners.
- Does the reported 50 px include the floating gap? If so, set `floating=0` in the session's
  plasmashellrc.
- Are `REACH` and `CORNER` comfortable?
- Is the wrist frame's 0.42 m too big?
- Does Plasma's overlay, 2 mm in front, show over our well at every curve?
- Does a release over Plasma's bar end a resize?
- Does the joystick bend the frame while the knob is held?

### Left out

- Inactive titlebar colours for panels without the keyboard.
- Using the scheme's font size: it would scale tags by size/10.
- Accent text, because nothing needs it yet.
- Watching edits to `.colors` files.
- Push and pull while carrying the taskbar.
- Mouse drags on the taskbar frame.

Each one is a few lines when somebody asks for it.

One thing I noted but didn't reopen: the violet chips repeat Plasma's own icontasks entries for
the same windows. Decision 4 keeps both.
