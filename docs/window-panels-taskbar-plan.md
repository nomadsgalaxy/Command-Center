# Plan: floating taskbar, with each Frame program window as its own panel

> **Heads up:** this plan's main recommendation didn't ship. Frame windows became KWin window
> streams instead, which is design 2 here ([window-panels-design2.md](window-panels-design2.md);
> `stream_window` in crates/cc-panels/src/windows.rs). I'm keeping this doc for the reasoning and
> the taskbar parts.

## Recommendation: let gamescope give each window its own panel; we build only the taskbar and launcher

I merged designs 1 and 3. Design 2 (KWin `stream_window`) is the fallback.

**Why this one:**
- **It's the least work.** We'd write no compositor, capture, PipeWire, KWin or
  Wayland-permission code. The Steam gamescope that's already running
  (`--backend openvr --virtual-connector-strategy PerAppId`) already gives every X11 window with
  no appID its own SteamVR overlay, `gamescope.gamescope-0.window.<seq>`
  (steamcompmgr_shared.hpp:278-290, OpenVRBackend.cpp:2038-2094).
- **It already works.** Steam's logs show `window.142`, `window.177` and `window.2737` being
  created and destroyed today. gamescope handles the DMA-BUF import, laser input, the keyboard,
  the Close button, the window title as the overlay name, and removing the overlay when the
  window closes.
- **It survives the switch to the stock desktop,** since gamescope-0 is the host in both setups.
- **Design 2 costs about 2–3 weeks**: a `.desktop` permission grant, PipeWire DMA-BUF
  negotiation, fake_input with raise-on-hover, and a KWin plugin that has crashed before
  (design.md:24). It only pays off if dashboard-owned placement turns out to be unacceptable.

**What we give up:** SteamVR's dashboard owns these panels.
- New windows open docked in the dashboard. We can float them, but we can't set or read their
  pose, and we can't set their size.
- Their controls are the dashboard's control bar, not our violet card. The only place violet is
  guaranteed is the window's taskbar chip.

## How it works

**Launcher.** It starts each app on the host, in its own systemd scope, on gamescope's X display,
with Wayland and the Steam app IDs cleared:
```
systemd-run --user --scope env -u WAYLAND_DISPLAY -u SteamAppId -u SteamGameId \
  DISPLAY=:0 QT_QPA_PLATFORM=xcb GDK_BACKEND=x11 SDL_VIDEODRIVER=x11 <Exec>
```
- Chromium, Electron and flatpak apps also get `--ozone-platform=x11`. This gamescope has no
  `--expose-wayland`, so apps have to use X11.
- The scope keeps the app out of Steam's process tree, so it has appID 0 and each window gets its
  own panel. It also keeps apps running when cc-panels exits.
- cc-panels runs in the container, so the launch has to reach the host's user systemd: either
  `systemd-run` over the shared `/run/user/1000` bus, or `distrobox-host-exec`.

**Window list.**
- Once a second, cc-panels calls `FindOverlay("gamescope.gamescope-0.window.N")` for numbers just
  above the highest one seen so far.
- It rechecks the windows it already knows. A lookup that fails means that window closed.
- The chip label comes from `GetOverlayName`, which is the window title.
- At startup it does one scan up to about 4096, because the logs show numbers around 2737.
- `// ponytail: seq probe; switch to GAMESCOPE_FOCUSABLE_WINDOWS on :0 via x11rb if it misses windows.`

**Taskbar.** It's one overlay, `controlcenter.taskbar`, in a new `taskbar.rs` that reuses
grab.rs's pieces:
- Drawing with `draw`/`over`/`mix`, uploaded with `set_raw`.
- The Mouse input method and flags from grab.rs:246-249, with buttons hit-tested from
  `e.data.mouse.x/y`.
- Its Placement added to `kvm.nearest`, so the room dot snaps onto it.
- Layout, left to right:
  - **Apps** opens a second overlay, a grid of `.desktop` entries from `$XDG_DATA_DIRS`. It skips
    NoDisplay, Hidden, `steam://` and X-Steam-Special entries, and strips `%uUfFick` and
    `@@…@@` from Exec lines.
  - **Remote machines** as cyan chips. A click uses the existing `show_panels`/`spot()`.
  - **Frame windows** as violet chips. A click calls `ShowDashboard(key)`. Float runs the
    `vrcmd --dock-overlay` sequence, rate-limited because each vrcmd connection costs a binding
    load.
  - **Clock** from `libc::localtime_r`.
- It repaints on hover, when the window list changes, and once a minute.
- **Text** needs one new crate, `fontdue`: PROXON.ttf for headings and the clock, plus Space
  Grotesk converted once from woff2 to ttf for titles.
- **Placement** is fixed in the world about 0.25 m below the lowest panel. It hides while a VR
  game runs (`GetCurrentSceneProcessId() != 0`).

**Input.** The SteamVR laser and our cc_pointer laser already work on these overlays; gamescope
turns their events into pointer or touch input for that window. Typing goes through the
control-bar keyboard into gamescope's IME. Copy and paste works between Frame windows because
they share the `:0` clipboard.

**Not duplicated:** volume, battery and notifications (SteamVR's Quick Access), and launching
Steam games (the Steam library).

## First live proof (about 10 minutes, on the host, SteamVR running, no code)

1. `for a in konsole dolphin; do systemd-run --user --scope env -u WAYLAND_DISPLAY -u SteamAppId -u SteamGameId DISPLAY=:0 QT_QPA_PLATFORM=xcb $a & done`
2. `xprop -display :0 -root GAMESCOPE_FOCUSABLE_WINDOWS` should list two windows.
3. Run `vrcmd --overlays | grep gamescope-0.window` once. Expect two `window.N` overlays with the
   window titles.
4. In Konsole, press Ctrl+Shift+N. Expect a third, separate overlay. That proves one panel per
   window.
5. In the headset: the laser clicks, the control-bar keyboard types, and Close shuts only that
   window.
6. `vrcmd --dock-overlay world <key>` should float it.
7. Open Konsole's Settings dialog and note whether it becomes a layer of the same panel or a new
   panel.

**Pass:** separate panels with working input and Close. **Fail:** everything merges into one
`desktopgame.<appid>` panel (an appID leaked in), or nothing appears. Either way, this decides
whether the plan goes ahead.

## Stages

| Stage | Work | Check |
|---|---|---|
| S0 | The proof above | Steps 1–7 pass |
| S1 | `launch <desktop-id>` on `@controlcenter`: scrubbed env, systemd-run reached from the container | konsole, dolphin and a Flatpak X11 browser each give one overlay per window, and keep running after cc-panels is killed |
| S2 | Window list (seq probe, titles) plus a `windows` command on `@controlcenter` | An assert self-check for the probe and close detection; opening and closing 3 windows matches `windows` within 1 s |
| S3 | `taskbar.rs` (remote chips, window chips, clock), fontdue, `kvm.nearest` | In the headset: chips appear and disappear with windows, clicks reach the right target, the room dot lands on the bar |
| S4 | Apps grid overlay with hicolor icons and a text-tile fallback | Every visible `.desktop` entry launches its app into its own panel |
| S5 | Window actions: float (rate-limited vrcmd), focus (`ShowDashboard`), close (`WM_DELETE_WINDOW` via x11rb) | Each action works; at most one vrcmd call per user action |
| S6 (optional) | Violet card: `controlcenter.card.fw.<seq>` attached with `SetOverlayTransformOverlayRelative` to the gamescope handle | A 30-line test first. If the card doesn't follow a floated panel, drop S6 and note the gap in the docs |
| Later | Clipboard bridge to `:0`, lazy follow or wrist pin, physical keyboard into Frame windows. Design 2 only if dashboard placement is rejected | n/a |

**Effort:** about 4–5 days in total for S1–S5. New crates: `fontdue`, and later `x11rb`.

## R-3

- **Remote panels only ever show their remote machine.** That holds by construction:
  - krdp panels are our own `controlcenter.panel.*` overlays, and only their own RDP decode in
    gpu.rs feeds them.
  - Frame windows exist only as gamescope's overlays, in another process.
  - No code path connects the two.
- **Accents:**
  - Remote panels keep Warp Cyan on their card and taskbar chips, as they do now.
  - Frame windows always get Violet #A48BFF on their taskbar chips. The panel itself only gets
    violet if S6 works; otherwise it shows the dashboard's own bar, which we can't recolour.
    That's a known, documented gap.
  - Pop-outs: add `MAGENTA` (#FF5CF3) next to `VIOLET` in grab.rs, for our own remote pop-outs.
    gamescope keeps a Frame window's popups inside its parent panel's `.layerN` subviews, so Frame
    windows have no separate pop-outs.

## Then vs after the stock-desktop switch

- **At the time:** apps launched from the taskbar go to gamescope's `:0`, one panel per window.
- **After the stock switch:** nothing in this plan changes.
  - The stock nested Plasma becomes one `valve.steam.desktopgame.<appid>` panel. It can
    optionally get a single "Desktop" chip.
  - Apps launched from the taskbar still go to `:0`, so they still get one panel per window.
  - No `.desktop` permission grant, no KWin script, no changes to the read-only root.

## Risks

1. **The dashboard owns placement.** We can't read the pose, the default width is 2.67 m per
   window, floating needs the slow vrcmd dock sequence, and saved layouts aren't possible. If
   that proves too clunky, it's the trigger for design 2.
2. **An appID can leak in** (reaper ancestry, cgroup, the `STEAM_GAME` property). Then all of an
   app's windows merge into one panel. The scope is meant to prevent that, and S0 checks it.
3. **Apps that prefer Wayland or are sandboxed** need per-app X11 flags. A single-instance app
   that's already running somewhere else may open its window there instead.
4. **Window size may be capped.** The OpenVR backend's fixed 1920×1080 upload buffer and
   letterboxing, or the `--nested-width 1280x720` setting, may limit per-window size. Unverified.
5. **The seq probe depends on gamescope internals** (the key format, the numbering), which a
   Steam update could change. The fallback is `GAMESCOPE_FOCUSABLE_WINDOWS`.
6. **The keyboard is the SteamVR IME only.** There are no real shortcuts until there's a
   physical-keyboard path into `:0`.
7. **Frame windows die if gamescope or SteamVR restarts.**
8. **Launching on the host from the container is unverified** (S1 checks it).

## Questions for you

1. **For v1, are you okay with Frame-window panels using SteamVR's own control bar, and floating
   or moving them through the dashboard?** Violet would only be guaranteed on their taskbar
   chips, with our violet card added if the S6 test works. If you need our card, grab bar and
   exact placement from day one, I'll build design 2 instead (KWin per-window capture, about
   2–3 weeks).
2. **Where should the taskbar live by default?** Fixed in the world below your panels (the
   plan), following you lazily when you turn away, or pinned to your wrist?
