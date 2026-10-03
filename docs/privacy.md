# Host privacy cover (D-055): research

On 2026-10-02 I asked for this: "for privacy 6, we should blur/black out the hosts, but allow the frame to see everything. Sharing your view seems too complicated, we won't do 8." I ran a read-only research pass the same day. Nothing here is built yet.

Both hosts, .63 and .85, run KWin 6.7.5 (checked over diagnostic SSH on 2026-10-02), which is above the 6.6 this needs.

### Bottom line

Build an **excluded cover overlay**. On each streamed output, put up a full-screen layer-shell surface that's opaque black and shows a notice, and have a small KWin script set `excludeFromCapture = true` on it. KWin then shows that window only on the physical monitor. krdp's stream is rendered separately through `FilteredSceneView`, which leaves the overlay out, so the Frame sees the live desktop underneath. Apps under the cover keep getting frame callbacks, so they keep updating.

This needs **KWin/Plasma 6.6 or later**, and it works on both of krdp's capture paths: the portal (`stream_output`) and `--plasma` (`stream_output` or `stream_region`). On 6.5 and older the cast copies the finished screen image, so any cover would show up on the Frame too, and there's no way around that on those versions.

**Blur works on 6.6+, but it isn't real privacy.** Blur-behind is drawn as part of the excluded window, so the Frame won't see it. But its strength comes from the user's global Blur effect settings, and large text and layout stay readable through a weak blur. So **black-out is the default. Blur is only a style option, labelled "obscure", never "private".**

### Ranking

1. **Excluded cover overlay on KWin 6.6+ (pursue).** It's real privacy, the Frame still sees everything, and crash safety comes almost for free. The same design works on Windows (`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, Windows 10 build 19041+, already proven by RustDesk) and on macOS (ScreenCaptureKit's `SCContentFilter(display:excludingWindows:)` in our own capturer).
2. **On KWin < 6.6: refuse.** `cc-share privacy on` should say "needs Plasma 6.6+" and do nothing. Arch hosts should be on 6.6 or 6.7 already, so the real fix is upgrading. There's no honest fallback on those versions:
   - **DDC brightness and contrast at 0 (external monitors) or the backlight at 0 (laptop) only dims.** Text stays readable on the G7/G9 VA panels, amdgpu panels and OLED panels. KWin 6.3+ owns brightness and overwrites the values. The monitor also keeps the value in its own memory, so a crash leaves it dark until someone restores it, which means a persistent restore file plus a systemd `ExecStopPost`. If we ever ship it, it's called "dim", not "privacy".
3. **Rejected:**
   - **DPMS off:** the Frame's first mouse move wakes the monitors. KWin ≤ 6.4 freezes the stream, and Samsung DP monitors drop hotplug in standby, which ends it.
   - **Disabling the output:** the stream ends, windows move to other monitors, and the change sticks in `kwinoutputconfig.json`.
   - **Lock screen:** the Frame would see the greeter, and its input would go to the greeter.
   - **A virtual output / krdp's `ExclusiveMode`:** the Frame gets an empty screen at a different size, the G9/G7 layout and the align mapping break, and it needs Plasma 6.8. Worth keeping as a separate "remote-only session" mode later.
   - **GNOME and wlroots:** there's no capture-exclusion API, so the feature is unsupported there. Hyprland's `no_screen_share` paints black into the capture, which is the opposite of what we want.

### Build cost (rough: 2-3 days, plus a test session on the hosts)

- **A `cc-privacy` overlay client.** One process, with one surface per streamed output.
  - Layer `overlay`, all anchors, exclusive zone -1, `keyboard_interactivity=none`, and an **empty input region**, so the Frame's EIS clicks reach the desktop.
  - In Rust with smithay-client-toolkit's layer-shell, like the rest of Command Center, or Qt with LayerShellQt. About 200-300 lines.
  - It draws black and a centred notice: "Being viewed from Steam Frame — Meta+Shift+Esc to reveal". (The review notes, V2, later dropped the chord from the notice.)
- **A KWin script** (about 30 lines of JS). The agent generates it with the overlay's pid in it and loads it over D-Bus (`org.kde.KWin /Scripting loadScript(path,"cc-privacy")`, then `start`).
  - It flags matching windows in `workspace.windowList()` and on `windowAdded`.
  - It calls back to the agent with `callDBus` for each window it flagged.
  - It matches on pid rather than app id, because layer surfaces may not carry a resourceClass.
- **The agent's state machine and command** (below), plus a version gate: parse `kwin_wayland --version` and require ≥ 6.6.

### How it meets each requirement

- **Opt-in per host.**
  - `cc-share privacy on|off|status` writes a host-local setting. It's off by default, and `on` refuses below KWin 6.6.
  - The Frame can only *ask* for the cover on hosts that opted in.
- **A visible notice.**
  - The notice is on the overlay itself, so only people at the host see it.
  - The Frame gets a "host covered" flag in its UI.
- **Revealing it locally.**
  - The overlay takes no input, so reveal is a KGlobalAccel shortcut the overlay registers, plus `cc-share privacy off`.
  - Reveal removes the cover for the rest of the session and sends the Frame a toast.
  - The Frame's keyboard could trigger the shortcut too. That's fine, because it can only remove privacy, never add it.
- **Crash-safe restore.**
  - The cover is just a Wayland surface. If the process dies, the surface goes with it and the monitor shows again. There's no kscreen, DPMS, DDC or VT state to undo.
  - The overlay runs as a child of the agent with `PR_SET_PDEATHSIG`. It also needs a heartbeat from the agent and exits if it misses about 3 s of them, which covers a *hung* agent, not just a dead one.
  - The cover comes down on disconnect, idle stop, `cc-share down` and krdp exiting.
  - The KWin script is unloaded on stop. A leftover one is harmless anyway, since it only flags a pid that no longer exists.
- **First-frame safety.**
  - The overlay maps fully transparent and only turns opaque once the script confirms the flag is set.
  - If no confirmation arrives within about 1 s, it **doesn't cover** and reports "privacy unavailable" to both sides. Otherwise the Frame would see black while the user thought privacy was on.
- **Never during align's tag screens or the pairing-key screen.**
  - Both already go through the agent. Before drawing tags or the key, the agent destroys the cover and waits for a compositor roundtrip.
  - Afterwards it re-covers only if a session is still active and nobody revealed.
  - "Cover active" and "tag/key screen active" are mutually exclusive inside the agent's state, not a convention callers have to remember.
- **A scoped, logged agent command.**
  - One verb: `privacy.set {on|off, outputs, style: black|blur}`.
  - It's accepted only from the authenticated, paired Frame that owns the current session, and only on hosts that opted in.
  - All it can do is start or kill `cc-privacy` and load or unload that one script.
  - Each call writes one log line: the peer, outputs, style, result, and the reason for any refusal.

### Risks

- **Other surfaces can sit above the cover and leak content:**
  - other overlay-layer surfaces (OSDs, notification popups);
  - the Overview and other full-screen effects;
  - the lock screen, which is fine.
  - Mitigation: turn on Do Not Disturb while covered.
- **The hardware cursor** probably draws above the cover, so someone at the host sees the pointer moving.
- **Blur** depends on the user's Blur effect settings, so it's weak by design (above).
- **Hotplug or mode changes** recreate outputs, so the overlay has to recreate its surfaces and wait for the script's callback again.
- **D-043 portability:** the feature only exists on KWin 6.6+, Windows and macOS. GNOME and wlroots hosts report it as unsupported.

### Still unknown until I test on the hosts

1. ~~`kwin_wayland --version` on the desktop and on the laptop.~~ Both are 6.7.5 (top of this page).
2. Whether layer-shell windows show up in `workspace.windowList()` / `windowAdded` and honour `excludeFromCapture`. If they don't, use a full-screen keep-above xdg_toplevel instead; on KWin 6.7+ the "excludefromcapture" window rule can then replace the script, though stacking against panels is weaker that way.
3. End to end: krdp in portal mode streaming the G9 (5120×1440) and the G7 with the cover on. Does the Frame see the live desktop, does video under the cover keep playing, and what do the extra cast render's GPU cost and latency look like?
4. Whether the Frame's EIS clicks and scrolls pass through the empty input region.
5. What leaks above the cover: the cursor, notifications, OSDs, the Overview.
6. Whether the first-frame path (transparent, confirmed, then opaque) ever flashes black on the Frame.
7. Whether blur at the user's Blur settings hides 12-14 pt text from 1 m. This decides whether blur ships at all.
8. Any differences with HDR on the G9 or with the laptop's eDP panel.

The downloaded sources and scratch notes lived in a scratch directory and weren't kept; the links under each part below are the sources. Nothing was installed or changed on any machine, and the repo wasn't edited.

---

## Research: KWin capture exclusion

## Part C: a full-screen overlay that KWin leaves out of screen capture

**Verdict:** it works on KWin, but only from **Plasma 6.6.0** (released February 2026), not on 6.5 or older. On 6.6+ the overlay can be a real blur or a black-out on the physical monitor while krdp's stream keeps showing the real desktop, on both of krdp's capture paths (portal and `--plasma`). I didn't change anything on any machine or in the repo.

### 1. The API (KWin source, branch Plasma/6.6)

- **The window property:** `Window` gets `Q_PROPERTY(bool excludeFromCapture READ excludeFromCapture WRITE setExcludeFromCapture NOTIFY excludeFromCaptureChanged)` in src/window.h, from MR !8442 ("Allow windows to be excluded from screen capture"). Transient children such as popups and menus inherit it recursively (window.cpp, `setTransientFor` / `setExcludeFromCapture`).
- **Ways to set it:**
  - **A KWin script.** The property is writable from JS (`win.excludeFromCapture = true`). There's also a `slotWindowExcludeFromCapture` shortcut slot in scripting/workspace_wrapper.cpp, and `workspace.windowList()` returns `workspace()->windows()`.
  - **The title-bar menu:** More Actions → "Hide from Screencast".
  - **The Wayland protocol:** `org_kde_plasma_window_management` server version 20 has the state flag `ORG_KDE_PLASMA_WINDOW_MANAGEMENT_STATE_EXCLUDE_FROM_CAPTURE` (`excludeFromCaptureRequested`). It's a privileged Plasma protocol and only covers managed toplevels, so it doesn't suit us.
  - **A window rule:** "excludefromcapture" (MR !8828). It isn't in 6.6; it's in the Plasma/6.7 and 6.8 branches, and it only applies to normal windows, not layer-shell.
- **Where the filter is applied:** `src/plugins/screencast/filteredsceneview.cpp`. It calls `SceneView::addWindowFilter(...)`, which hides any window where `excludeFromCapture()` is true or whose pid equals `pidToHide`, and forces a full repaint when the flag changes.
  - `OutputScreenCastSource` (zkde_screencast `stream_output`) and `RegionScreenCastSource` (`stream_region`) both use it.
  - Window streams don't need it.
  - Normal screenshots (Spectacle / ScreenShot2) **don't** honour it, on purpose. That doesn't matter for RDP.

### 2. Does the cast come from the same frame as the display, or a separate render?

It depends on the version, and it's why the minimum is 6.6:

- **Up to 6.4:** `OutputScreenCastSource::render` copies the finished screen image (`Compositor::self()->textureForOutput(m_output)`). Anything drawn on the output, an overlay included, ends up in the stream. **Not possible.**
- **6.5:** it re-renders the scene through its own `SceneView`, but with no filter yet. **Still not possible.**
- **6.6+:** it re-renders through `FilteredSceneView`, a second render of the scene for the cast with excluded windows removed. Windows hidden under the overlay are still drawn into the stream, because occlusion culling runs per view.

**Do the hidden apps keep updating?** Yes. `Item::framePainted` / `collectItems` sends frame callbacks to every visible item on the output, covered or not (`SurfaceItemWayland::handleFramePainted`). Apps under an opaque overlay keep updating, so the Frame doesn't get a frozen picture.

**Real blur:** effects run inside each view's paint (`effects->paintScreen` / `paintWindow` in workspacescene.cpp). Blur-behind on our overlay (`KWindowEffects::enableBlurBehind`, or the `org_kde_kwin_blur` protocol) is drawn as part of that window's paint, so it shows on the physical screen and gets filtered out of the cast along with the window. Two caveats:

- The strength comes from the user's global KWin Blur effect settings, and a soft blur can still show layout and large text.
- So black-out is the default, with blur as a style option.

**A pure KWin effect, with no window, isn't a good option.** Effects get the same `LogicalOutput` and viewport for the real screen and for the screencast view, and there's no public API to tell them apart. FilteredSceneView is private to the screencast plugin, so an effect that drew black would draw it into the stream too.

### 3. krdp's capture path specifically

- **Portal mode (the default):** krdp → xdg-desktop-portal-kde ScreenCast (MONITOR) → zkde_screencast `stream_output` → OutputScreenCastSource / FilteredSceneView. **Works.** `getPid()` deliberately returns no pid for xdg-desktop-portal-kde (a "HACK" in screencastmanager.cpp), so only the excludeFromCapture flag hides anything.
- **`krdpserver --plasma`:** it calls `createOutputStream` when `--monitor` is set, otherwise `createWorkspaceStream`, which becomes RegionScreenCastSource. **Both work.** `pidToHide` is krdpserver's own pid here, so any window krdp creates itself is left out of its stream automatically.
- **`--virtual-monitor`:** KWin makes a new virtual output for the stream, so the overlay doesn't matter on this path.

### 4. Suggested design for the agent

1. **An overlay client.** A small Qt or Wayland client per paired output: either a fullscreen keep-above xdg_toplevel, or a layer-shell surface on the `overlay` layer through LayerShellQt with exclusive zone -1. It shows the notice and a local "reveal" button or shortcut, and carries a unique app id (e.g. `cc-privacy`).
2. **Mark it excluded** with a KWin script loaded over D-Bus: `org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript(path, "cc-privacy")`, then `.start()`. The script sets `excludeFromCapture = true` on matching windows, both from `workspace.windowList()` and in `workspace.windowAdded`.
   - With a plain toplevel, KWin 6.7+ can use the persistent window rule instead.
   - With layer-shell, window rules don't apply, so it needs the script. Layer surfaces are KWin `Window`s, but I didn't verify end to end that they show up in `windowList()`.
3. **Don't leak the first frame.** `windowAdded` fires once the window is mapped, so:
   - map the overlay fully transparent first;
   - have the script confirm back to the agent (`callDBus`) once the flag is set;
   - only then go opaque or blurred.
4. **Crash-safe restore.** The overlay is a normal client, so if the agent or the overlay dies, the window and the black-out go with it. There's no compositor setting to undo and no stuck state.
   - Kill the overlay on disconnect, idle stop, `cc-share down`, align's tag screens and the pairing-key screen. The Frame's camera has to see the real screen for those last two.
5. **Version gate.** `cc-share privacy on` refuses unless KWin is ≥ 6.6 (`kwin_wayland --version`). At this point I'd only checked the Frame (kwin 6.2.5); the hosts were checked later and run 6.7.5.

### 5. Other compositors (D-043)

- **wlroots (sway etc.):** wlr-screencopy and ext-image-copy-capture copy the finished output image, and there's no exclusion API. **Not possible.**
- **Hyprland 0.50+:** the `no_screen_share` window/layer rule paints a **black box** into the capture instead of really excluding the window, so the Frame would see black. **Not usable.**
- **GNOME/Mutter:** I found no public per-window capture exclusion. Treat it as unsupported (not verified in source).
- **Windows 10 2004+:** `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` really does remove the window from Desktop Duplication and Windows.Graphics.Capture. **Works** (from memory, not re-checked).
- **macOS:** our own server does the capturing, so ScreenCaptureKit's `SCContentFilter(display:excludingWindows:)` can leave out the overlay window. Don't rely on `NSWindow.sharingType = .none`; recent macOS versions reportedly don't honour it reliably (unverified).

Sources: [KWin !8442](https://invent.kde.org/plasma/kwin/-/merge_requests/8442), [KWin !8828](https://invent.kde.org/plasma/kwin/-/merge_requests/8828), KWin source on Plasma/6.4, 6.5 and 6.6 (`src/plugins/screencast/outputscreencastsource.cpp`, `filteredsceneview.cpp`, `screencastmanager.cpp`, `src/window.{h,cpp}`, `src/scene/item.cpp`, `surfaceitem_wayland.cpp`, `workspacescene.cpp`, `src/wayland/plasmawindowmanagement.cpp`), krdp master (`src/PlasmaScreencastV1Session.cpp`, `src/PortalSession.cpp`, `server/main.cpp`), [kwin-hide-from-screencast backport](https://github.com/henriquejsza/kwin-hide-from-screencast), [OSTechNix on Plasma 6.6](https://ostechnix.com/exclude-certain-windows-from-screen-recording-in-plasma-6-6/), [Hyprland no_screen_share](https://github.com/hyprwm/Hyprland/discussions/12417).

## Research: DPMS and brightness

## Privacy blanking: DPMS/output-off (A) and brightness-to-zero (B) on the KWin hosts

I couldn't test this on a host. The session ran on the Frame (SteamOS, kwin 6.2.5, ddcutil 2.1.4), so these findings come from KWin, libkscreen and PowerDevil source on invent.kde.org, checked against tags v6.3.0 to v6.6.0 and master. I didn't change anything on any machine.

**Verdict:**

- **A fails.** Turning the output off kills the stream. DPMS freezes the stream on KWin 6.4 and older, and on every version the Frame's own input wakes the screen.
- **B only dims.** At zero brightness the content is still readable on the G7, the G9 and most laptops. It's only useful alongside a black overlay that the stream doesn't capture.

### A1. Turning the output off: rejected

- **Command:** `kscreen-doctor output.DP-1.disable` (to restore: `output.DP-1.enable`, then re-apply the mode and position).
- **What happens:** the output leaves the workspace. `OutputScreenCastSource` is connected to `Workspace::outputRemoved` and sends `closed()`, so krdp's stream for that monitor ends.
- **Side effects:**
  - Windows move to the other monitors.
  - The change is saved in kwinoutputconfig.json, so if the agent crashes, the monitor stays disabled after a reboot.
  - It would break align's tag screens too.

### A2. DPMS standby: rejected

- **Commands:**
  - `kscreen-doctor --dpms off` and `--dpms on`. These use the `org_kde_kwin_dpms` Wayland protocol and act on every screen; per-output control would need our own Wayland client speaking that protocol.
  - PowerDevil's shortcut: `qdbus6 org.kde.kglobalaccel /component/org_kde_powerdevil invokeShortcut "Turn Off Screen"`. Also all screens.
- **Rendering:** `DrmOutput::applyQueuedChanges` calls `m_renderLoop->inhibit()` when DPMS goes off.
  - **KWin 6.4 and older:** screencast frames come from `Output::outputChange`, which only fires after the output is painted. So DPMS off means no new frames, and the Frame sees a frozen image.
  - **KWin 6.5 and newer:** the screencast renders into its own `ScreencastLayer` with `setRenderLoop(nullptr)`, separate from the output's render loop. Frames will probably keep coming, but that needs a test. Apps that are only on that output may stop drawing because they get no frame callbacks, so video or browsers could stall.
- **Blocker 1, input wakes it.** `dpmsinputeventfilter.cpp` wakes every output on any pointer motion (warps excepted), button, scroll, key or touch, without checking for synthetic devices. krdp injects the Frame's input through fake input, so the first mouse move from the Frame turns the monitor back on. Local input does the same, which gives you "reveal" for free but means it can't be scoped.
- **Blocker 2, DisplayPort hotplug.** Many DP monitors drop the hotplug signal in standby (Samsung Odysseys are a common example). KWin then sees an unplug, removes the output, and the stream sends `closed()`, as in A1.

### B1. External monitors over DDC/CI (G9 and G7)

- **Commands:**
  - Find the buses: `ddcutil detect --brief`.
  - Save the current values: `ddcutil --bus N getvcp 10 12 --terse`.
  - Blank: `ddcutil --bus N --noverify setvcp 10 0` (optionally `setvcp 12 0` too, which takes contrast to 0 as well).
  - Restore: `ddcutil --bus N setvcp 10 <saved>` and `setvcp 12 <saved>`.
- **Latency:**
  - Always pass `--bus` to skip detection, which takes about 1-3 s cold.
  - One `setvcp` takes roughly 50-300 ms with ddcutil 2.x's dynamic sleep.
  - The two monitors are on separate buses, so they can be written in parallel.
  - Expect the odd "DDC communication failed". Retry 2-3 times and verify with `getvcp`.
- **Permissions:**
  - The `i2c-dev` module has to load at boot (`/etc/modules-load.d/i2c-dev.conf`).
  - ddcutil's udev rule (`60-ddcutil-i2c.rules`) tags `/dev/i2c-*` with `uaccess`, but only for GPU class 0x030000. A 0x038000 display controller (some hybrid laptops) gets no access, so the user would need to be in the `i2c` group (`/dev/i2c-*` is `root:i2c 0660`).
  - `uaccess` only covers the active local seat session, so the agent has to run as a user unit in the logged-in session.
  - On NVIDIA's proprietary driver, DDC is often flaky without `NVreg_RegistryDwords=RMUseSwI2c=0x01;RMI2cSpeed=100`.
- **Privacy:**
  - VCP 0x10 = 0 is the monitor's minimum backlight, not off. On a VA/LCD panel like the G7 (and the G9, unless it's an OLED model) that's tens of nits, and text stays readable.
  - Adding contrast 0x12 = 0 makes it darker, but still not reliably black.
  - Samsung Odysseys often ignore 0x10, or lock it with HDR, Eye Saver, dynamic or adaptive brightness, or certain picture modes. DDC/CI may be switched off in the on-screen menu too. There's also a recent PowerDevil report of an Odyssey G40B where DDC brightness writes kept failing.
  - Check each monitor: `ddcutil --bus N capabilities | grep -i 'Feature: 10'`, then set 10 to 0 and look at the screen with real content on it.
- **Conflict with Plasma:**
  - From Plasma 6.3, KWin owns brightness: `DrmOutput` has a `brightnessDevice` with `usesDdcCi()`, and PowerDevil drives it through KScreen.
  - KWin re-applies its own brightness on mode changes and wake events, which would overwrite our raw ddcutil 0. Both also share the i2c bus, which can cause transient errors.
  - The KWin route is `kscreen-doctor output.DP-1.brightness.0`, restored with `.brightness.<saved>`. But that value is saved to kwinoutputconfig.json, so a crash survives a reboot.
- **Monitor memory:** the monitor stores DDC brightness itself, across power cycles, so the restore record has to be persistent. Each toggle also writes the monitor's EEPROM: fine once per session, never something to animate.
- **The power-mode trick: rejected.** VCP D6 = 4 or 5 (panel off over DDC) has the same hotplug-drop risk as DPMS. Many monitors also stop answering DDC while asleep, so `setvcp D6 1` can't wake them and someone has to press the power button.

### B2. Laptop panel (eDP)

- **Commands:**
  - `brightnessctl -s set 0`, restored with `brightnessctl -r`. `-s` saves the old value under `/tmp/brightnessctl`, which doesn't survive a reboot, so keep our own copy.
  - Or read `/sys/class/backlight/*/max_brightness` and `brightness` and write 0, which needs the `video` group or a udev rule.
  - Or, without root: `busctl call org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session SetBrightness ssu backlight intel_backlight 0`.
- **Is 0 black?**
  - On an Intel LCD, PWM 0 is often truly off: black, apart from a ghost image in bright light.
  - amdgpu maps 0 to the panel's minimum input signal, so it's dim but visible.
  - OLED laptops have no backlight. Brightness goes through a different panel channel, and 0 means minimum light, which is still readable.
- **KWin and PowerDevil:**
  - KWin owns the backlight from 6.3. PowerDevil's D-Bus calls go through KWin; `KWinDisplayBrightness::knownSafeMinBrightness()` returns 0 and the maximum is 10000.
  - Commands: `qdbus6 org.kde.ScreenBrightness /org/kde/ScreenBrightness DisplaysDBusNames`, then `qdbus6 org.kde.ScreenBrightness /org/kde/ScreenBrightness/<display> SetBrightness 0 0`.
  - Writing sysfs behind KWin's back gets overwritten the next time KWin applies brightness.
- **KWin's software dimming has a floor.** `createColorDescription` uses `5 + (maxRef - 5) * brightnessFactor`, so 0 still leaves a 5-nit white and is never black.

### Crash-safe restore (for B, or any blanking)

1. Before blanking, write `~/.local/state/cc-share/privacy-restore.json` with the bus or output and the original 0x10, 0x12 and backlight values, through a temp file and a rename. It has to be persistent because the monitor keeps its DDC value.
2. Run the agent as a systemd user unit:
   ```
   [Service]
   Type=notify
   WatchdogSec=5
   Restart=on-failure
   ExecStopPost=%h/.local/bin/cc-share privacy-restore
   ```
   - The agent sends `sd_notify("WATCHDOG=1")` every 2 s, so if it hangs, systemd kills it.
   - `ExecStopPost` runs after a normal stop, a crash, SIGKILL or an OOM kill. That one unit is the dead-man timer, so no separate watchdog unit is needed.
3. `privacy-restore` has to be idempotent: it does nothing when the restore file is missing, retries DDC 3 times, and only deletes the file once `getvcp` confirms the values.
4. Also run it at login with `ExecStartPre=` on the same unit. That covers power loss, a kernel panic and logout, where `ExecStopPost` never ran.
5. **The gap that's left:** if the monitor stops answering DDC while blanked, nothing can restore it. The local reveal then has to show instructions on screen: the monitor's own brightness button, or `cc-share privacy off`.

### A lead outside my part, for the overlay teammate

- In KWin 6.6 and later, `ScreencastManager::getPid()` hides from an output stream every window owned by the process that opened the stream. The portal is exempt, but direct `zkde_screencast` clients like krdp aren't (`FilteredSceneView` filters on `window->pid()`).
- So a fullscreen black layer-shell surface made by the process that owns the stream would be black on the monitor and missing from the RDP stream. The catch is that this process is krdpserver, not our agent.
- KWin 6.5 also renders screencasts separately from the output. (The hosts were checked later: both run 6.7.5.)

### Tests to run on a host when allowed

1. ~~`kwin_wayland --version`.~~ Done: 6.7.5 on both.
2. Start krdp, connect the Frame, run `kscreen-doctor --dpms off`, and check whether the stream keeps updating and whether the Frame's first mouse move wakes the monitors.
3. On each monitor, `ddcutil --bus N setvcp 10 0`, then judge whether the content is readable from 1 m and whether KWin resets it within a minute.
4. On the laptop, backlight 0 in a dark room and in a lit one.

### Sources

- KWin: `src/plugins/screencast/outputscreencastsource.cpp`, `screencastlayer.cpp`, `screencastmanager.cpp`, `src/backends/drm/drm_output.cpp`, `src/dpmsinputeventfilter.cpp` (https://invent.kde.org/plasma/kwin)
- libkscreen: `src/doctor/main.cpp`, `doctor.cpp`
- PowerDevil: `daemon/dbus/org.kde.ScreenBrightness*.xml`, `daemon/controllers/kwinbrightness.cpp`
- PowerDevil in Plasma 6.0 and beyond: https://blogs.kde.org/2024/04/23/powerdevil-in-plasma-6.0-and-beyond/
- KDE Discuss, brightness slider missing without a DDC or backlight backend: https://discuss.kde.org/t/brightness-slider-missing-in-plasma-when-powerdevil-has-no-ddc-or-backlight-backend/48004
- NVIDIA forum, pageflip timeouts when a powered-off DP monitor drops hotplug: https://forums.developer.nvidia.com/t/kwin-wayland-drm-pageflip-timed-out-this-is-a-bug-in-the-nvidia-drm-kernel-driver/331077

## Research: other approaches and prior art

## Research: D-055 privacy mode, part D/E/F (other approaches, prior art, other OSes)

Research only, nothing installed or changed. I read krdp's source from a scratch clone (2026-10-02) that wasn't kept.

### Short answer

Upstream KWin already has what we need to "blur or black out the host while the Frame sees everything". Since **Plasma/KWin 6.6** every KWin `Window` has an `excludeFromCapture` property, and KWin's output and region screencast sources skip windows that set it. I confirmed it in KWin master: `src/plugins/screencast/outputscreencastsource.cpp` renders through `FilteredSceneView`, and `filteredsceneview.cpp` has:

```cpp
addWindowFilter([pidToHide](Window *window) { ... return window->excludeFromCapture(); });
```

krdp's per-monitor stream goes through `zkde_screencast stream_output` and then `OutputScreenCastSource`, so it gets this filter too. The plan: put a fullscreen black (or blurred) overlay on each physical output and mark it `excludeFromCapture`. The monitor shows the overlay and the krdp stream shows the real desktop. It's the same technique RustDesk uses as its preferred privacy mode on Windows (`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`). Options D (a virtual output) and E (the lock screen) both fail my requirement; details below.

### (D) A virtual output: that's a different feature

- **What exists.** krdp has `--virtual-monitor WxH@scale`, which calls `zkde_screencast.stream_virtual_output` (src/screencasting.cpp:159), and `--mode RemoteAccess|SharedAccess|AdditionalDisplay`. The KCM's "exclusive mode" (`ExclusiveMode`) is labelled "Disable the host monitors and create virtual screens for exclusive session use by a remote user". It arrived in commit 8937570 ("Introduce OperatingMode") on 2026-09-01 and first ships in **v6.7.90 (the Plasma 6.8 beta)**, so current hosts don't have it.
- **What RemoteAccess actually does** (server/SessionController.cpp:222-245):
  - It streams a new virtual monitor, 1024x768@1 by default.
  - It calls `org.freedesktop.DisplayManager.Seat.SwitchToGreeter`, so the physical VT shows the SDDM greeter, then calls `login1 Session.Unlock`.
  - When the last client disconnects it calls `login1 Session.Lock`.
  - A developer comment says this logic should move into KWin and that a VT switch can break it.
  - So it's Windows-RDP-style "lock the console, serve a separate screen": prior art inside krdp itself.
- **Against D-055.** I want the Frame to see the host's real monitors. With a virtual output, the Frame sees a new, empty screen at a different resolution and scale. The user's windows would have to move there and back afterwards, which breaks the G9/G7 layouts and the per-monitor mapping align depends on.
- **Crash safety is poor.** If the agent dies mid-move, windows end up on surprising outputs, and VT/greeter state can be left behind if krdp dies. kscreen and krfb-virtualmonitor have the same problems: they add an output and don't hide the physical one.
- **Turning the physical monitors off doesn't work either.** KWin doesn't render DPMS-off outputs, so their screencasts produce no frames. The westers/krdp fork works around this by waking the display through PowerDevil.
- **Verdict:** reject for D-055. It's worth keeping as a separate "remote-only session" mode later, built on krdp's own RemoteAccess once hosts run Plasma 6.8 or later.

### (E) The lock screen: no

- krdp shares the user's session, so a stream of the physical output shows whatever KWin composites, lock screen included.
- KWin bug 515119 (kwin!9246) now hides the lock screen from ordinary screencasts, but a follow-up commit deliberately keeps it **visible while the session is remotely controlled** (authenticated fake input or a live EIS context), which is exactly krdp's case. The release it lands in wasn't stated.
- Even if the stream could hide the lock screen, a locked session sends all input to the greeter, so the Frame couldn't use the desktop.
- **Verdict:** a lock-based design always shows the Frame a lock screen. Reject it. Locking only makes sense as a separate disconnect action, as in krdp's RemoteAccess.

### (F) Prior art, and what transfers to KWin

| Product | Mechanism | Transfers? |
|---|---|---|
| Windows RDP | A separate session; the console goes to the lock screen. | No. That's the virtual-session model from D. |
| Chrome Remote Desktop, Windows curtain | An RDP loopback session to itself, so the console is locked. | No. Same model. |
| Chrome Remote Desktop, Linux | No curtain needed: the host runs its own separate virtual X/Wayland session and never shares the physical console. | No. A different desktop. |
| Chrome Remote Desktop, macOS | Curtain via `RemoteAccessHostRequireCurtain`; **unsupported on Big Sur and later**. | No. |
| Apple Remote Desktop "Curtain" / Interact > Lock Screen | The ARD agent shows a lock image on the client while the admin keeps viewing and controlling. | Yes, in spirit (a local cover, the remote sees everything). The implementation is private. |
| macOS Screen Sharing | Logging in as another user gives a separate session and leaves the console locked. | No. |
| UltraVNC "blank monitor on viewer request" | Before Vista, a layered top window captured separately from the desktop below. **Broken on Windows 8/10**, where the layers are merged before capture. | The idea transfers; it needs compositor support. |
| Parsec Privacy Mode, Windows | A Virtual Display Driver plus disabling every physical display; locks the host when the last guest leaves. | No. The virtual-output model, with the same layout problems. |
| Parsec Privacy Mode, macOS | Physical displays stay on and are blanked, with no virtual display. | Yes, it's the cover model. |
| RustDesk privacy mode | Windows, in priority order: `WDA_EXCLUDEFROMCAPTURE` on a cover window (Windows 10 build 19041+), then the Magnifier API, then an Amyuni virtual display with the physical displays off. macOS: a native `MacSetPrivacyMode`. **Linux: unsupported**, because there's "no reliable Linux API". | **Yes, directly.** KWin 6.6's `excludeFromCapture` is the Linux equivalent RustDesk didn't have. |

### The design that falls out of this (for the KWin hosts)

1. **One overlay per physical output.** A layer-shell surface on the overlay layer, fullscreen and opaque black, with the visible notice and the local reveal control. The blur variant is a translucent surface using KDE's blur protocol, also excluded. Black is the safer default, because a weak blur can leave large text readable.
2. **Mark it excluded.** For layer surfaces the only handles today are a KWin script, a window rule or the title-bar menu. The agent loads a small KWin script over D-Bus (`org.kde.KWin /Scripting`) that sets `window.excludeFromCapture = true` on windows matching the overlay's pid. It has to stay loaded and listen on `workspace.windowAdded`, because layer surfaces only appear once mapped and get recreated when outputs change. The DLSS5-NeuralScreen-Linux project does exactly this. The "Exclude from screencast" window rule (kwin!8828) was still pending when I looked.
3. **Let input through.** Otherwise the Frame's EIS clicks land on the overlay. Set an empty `wl_surface.set_input_region` and `keyboard_interactivity=none`. The local reveal then needs its own path, such as a global shortcut or the agent's CLI.
4. **Crash safety comes free.** If the agent dies, its Wayland surfaces are destroyed and the monitor shows again. A leftover KWin script is harmless because it only flags that pid; unload it when the session ends. There's no kscreen, DPMS or VT state to restore.
5. **Known leaks and limits to test:**
   - The hardware cursor plane may draw above the overlay, so someone at the host sees the pointer moving.
   - Other overlay-layer surfaces (OSDs, notifications), the lock screen and full-screen effects like the Overview may stack above it.
   - The hosts need **KWin 6.6 or later**. They run 6.7.5 (checked after this part was written).
   - Don't show the overlay during align's tag screens or the pairing-key screen; the agent hides it first.

### Other OSes (D-043)

- **GNOME (Mutter):** no native capture exclusion in the portal ScreenCast API or in `org.gnome.Mutter.ScreenCast`. Monitor streams copy scanout directly.
  - Third-party options: a GNOME Shell extension (DisplayXR PR #1620) uses a Clutter.Effect to drop an actor from area streams, and muscst patches Mutter.
  - The native alternatives are the virtual-session models: `RecordVirtual` virtual monitors, and gnome-remote-desktop's headless "remote login" session, which is Chrome Remote Desktop's Linux style.
  - Verdict: no clean equivalent today. Use an extension with an area stream, or accept a virtual-session mode.
- **wlroots:** no exclusion protocol. `wlr-screencopy` and the portal capture the composited output. Only a headless or virtual output (for example sway's `create_output`) is available, which is model D.
- **Windows:** the same design as on KWin. A topmost black window with `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` on Windows 10 build 19041 or later is honoured by DXGI Desktop Duplication and Windows.Graphics.Capture, as RustDesk shows. The fallback is a virtual display plus disabling the physical ones, as Parsec and RustDesk do.
- **macOS:** if our server captures with ScreenCaptureKit, pass the cover windows to `SCContentFilter(... excludingWindows:)` and they're left out of the stream. Parsec's macOS privacy mode likewise keeps the physical displays on and blanks them. Don't rely on `NSWindow.sharingType = .none` alone; I believe recent ScreenCaptureKit ignores it, but I didn't verify that.

### Recommendation

Build D-055 as an "excluded cover overlay": a layer-shell overlay on each output, marked `excludeFromCapture` by a KWin script matching its pid, with an empty input region. It needs KWin 6.6 or later on the hosts. It keeps the Frame on the real monitors, restores itself on a crash because the surface dies with the agent, and maps one-to-one onto Windows (`WDA_EXCLUDEFROMCAPTURE`) and macOS (ScreenCaptureKit's window exclusion). Reject D and E for this feature, and keep krdp's RemoteAccess (a virtual monitor, the greeter, and a lock on disconnect, Plasma 6.8 or later) in mind only as a separate "remote-only" mode later. GNOME and wlroots have no clean equivalent yet.

### Sources

- KWin MRs: [kwin!8442 (excludeFromCapture)](https://invent.kde.org/plasma/kwin/-/merge_requests/8442), [kwin!8828 (window rule)](https://invent.kde.org/plasma/kwin/-/merge_requests/8828), KWin master `src/plugins/screencast/filteredsceneview.cpp` and `outputscreencastsource.cpp`, `src/window.h` (`Q_PROPERTY excludeFromCapture`).
- [The 6.3 backport README](https://github.com/henriquejsza/kwin-hide-from-screencast); [DLSS5-NeuralScreen-Linux PR #10 (KWin script by pid for layer surfaces)](https://github.com/malik05051/DLSS5-NeuralScreen-Linux/pull/10).
- [KWin bug 515119 (lock screen and screencast)](http://www.mail-archive.com/kde-bugs-dist@kde.org/msg1205499.html); [westers/krdp (DPMS-off gives no frames)](https://github.com/westers/krdp).
- krdp source: server/SessionController.cpp, server/main.cpp, src/kcm/krdpserversettings.kcfg; [krdp repo](https://invent.kde.org/plasma/krdp).
- [RustDesk privacy mode overview](https://instagit.com/rustdesk/rustdesk/how-does-rustdesks-privacy-mode-work-across-different-platforms/); [RustDesk privacy mode implementations](https://zread.ai/rustdesk/rustdesk/24-privacy-mode-implementations).
- [Parsec Privacy Mode](https://support.parsec.app/hc/en-us/articles/32361381211284-Privacy-Mode); [Chrome Remote Desktop curtain mode](https://www.anyviewer.com/how-to/chrome-remote-desktop-curtain-mode-2578.html); [Apple Remote Desktop lock screen](https://support.apple.com/en-mt/guide/remote-desktop/apd37d6089c/mac).
- [UltraVNC forum: remote blank monitor](https://forum.ultravnc.net/viewtopic.php?f=72&t=35197).
- [DisplayXR GNOME capture exclusion PR](https://github.com/DisplayXR/displayxr-runtime/pull/1620); [muscst](https://github.com/sofyanox12/muscst).

---

## Review notes (D-055), to fold into the build

- **V1 Threat model:** the cover protects against onlookers and cameras at the host, not against anyone with the host's keyboard or a local session.
- **V2 Notice text:** the cover only says "Being viewed from Command Center". The reveal chord isn't printed, because that would tell onlookers how to undo it; it lives in settings. Consider a hold-for-2-s chord.
- **V3 Tie the cover to the Frame being there:** a Frame heartbeat of about 30 s, where a missed heartbeat uncovers, and the Frame uncovers when the headset comes off. The 10-minute session idle stop must never turn into a 10-minute black screen.
- **V4 Re-check the exclusion with each heartbeat:** if the flag is lost (a KWin restart, a re-created window), uncover and report "privacy lost".
- **V5 On the Frame:** a "host covered" indicator and a reveal button, plus a hint when the stream has been fully black for a while, so the Frame user is never left blind.
- **V6 Emergency:** document `cc-share privacy off` from a TTY.
- **V7 The KWin script:** built from a fixed template with only an integer pid substituted, written to a 0700 directory.
- **V8 Scope:** only shared (streamed) outputs are covered.
- **V9 Agent tests:** cover vs. tag/key-screen mutual exclusion, a hung agent, the first-frame timeout, and `privacy.set` from a Frame that doesn't own the session.
