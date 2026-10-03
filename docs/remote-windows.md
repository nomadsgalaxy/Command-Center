# Remote windows: pulling a window out of an RDP panel (spike design)

> **Parked (2026-10-02).** I wrote: "let's scrap the idea for popping out rdp windows for now, since
> only linux machines will be really able to do it, as the VNC hosts may have a problem". So window
> streams are off on both hosts (`cc-share windows off`). The krdp `--window` patch and its build
> directories stay, but I'm not building or installing them any further. The agent's window commands
> stay too, behind the host opt-in, which is now off by default (`windows-off`), and the Frame's pop
> panel is dormant. Everything below is the record of the spike.

Status: the spike got through step 0 and spike 1 (the CLI pop-out, host and Frame side) before it was
parked; spike 2, the gesture, was never built. See "Built so far" at the end. Hosts: KDE Plasma /
KWin 6.7.5 (.63, .85). The Frame runs KWin 6.2.5, and that difference matters for geometry (see
"Coordinates").

Since this was written, the agent, cc-share and cc-home have moved from Python and bash to Rust: the
agent's window commands are in `crates/cc-host/src/work.rs`, `window-run` and `cc-share windows` in
`crates/cc-host/src/share.rs`, and `cc-home machine window` in `crates/cc-home/src/machine.rs`. The
file names and line numbers below (home/agent.py, pair.py, the cc-share and cc-home scripts) point at
the Python originals, which aren't part of this repo.

## Goal

You grab a window's title bar inside a monitor's RDP panel and drag it past the panel's edge. When you
let go, the window becomes its own floating panel with working input, the same way the Frame's local
window panels work (windows.rs). No SSH (docs/ssh-free.md): all the host work goes through the paired
agent.

## Chosen approach: krdp window streams (`--window <uuid>`)

A per-window krdpserver streams `zkde_screencast stream_window(uuid)`, and the Frame connects to it
like any other monitor session: one FreeRDP session in one panel. I picked this because:

- the stream follows the window as it moves and when something covers it;
- it keeps working when the window is on another virtual desktop;
- KWin 6.7.5 already allows it: `zkde_screencast_unstable_v1` is in krdpserver's
  `X-KDE-Wayland-Interfaces`, and `ScreencastManager::streamWindow` has no per-window check.

Input goes in through fake_input in global logical coordinates, and krdp maps stream pixels onto the
window's live frameGeometry, which it gets from `org_kde_plasma_window` events.

## Fallback: Frame-only crop (no host change)

If the krdp patch stalls, there's a cheaper way that still proves the gesture, the placement, the slot
lifecycle and the input path. The pop panel gets no RDP session of its own. Its overlay shows the
monitor panel's shared texture cropped to the window's rectangle (back.rs `shown` plus
`SetOverlayTextureBounds = rect / monitor size`), and its input is forwarded to the source panel at
`(x + rect.x, y + rect.y)`.

It costs one extra overlay pair, with no decode and no host session. The limits:

- the window has to stay fully on the monitor and uncovered (the agent sets keepAbove);
- it's at the monitor's resolution;
- the window shows twice.

I ruled out `stream_region` (krdp on a fixed rectangle) and `stream_virtual_output` with the window
moved onto it for the spike. Both have the crop's problems with covered and pinned windows, or side
effects on KWin's layout, and neither is free like the crop.

## Coordinates (host 6.7.5 differs from the Frame's 6.2.5)

- On 6.7.5 a window stream covers `boundingRect`: the window's frameGeometry united with the
  frameGeometry of its popup transients, at targetScale. On 6.2.5 it was clientGeometry.
  window-panels-design2.md and windows.rs `cg` assume clientGeometry, so those assumptions don't hold
  on the hosts.
- UUIDs are KWin's `internalId().toString()`, **braced** `{...}`. `get_window_by_uuid` matches the
  exact string, so always pass the braced form.
- Host geometry is fractional (for example x=1677.6 at scale 1.25). Round it for display, but keep it
  as qreal in the mapping.

## 1. krdp patch: `krdp/window-stream.patch`

This is a third patch next to pointer-offset.patch and clipboard.patch. Bump pkgrel to 1.12 and add it
to `source=`/`prepare()`. All the changes are against krdp-6.7.5:

1. `src/CMakeLists.txt`: add `${PLASMA_WAYLAND_PROTOCOLS_DIR}/plasma-window-management.xml` to
   `qt6_generate_wayland_protocol_client_sources(KRdp ...)`.
2. `server/org.kde.krdpserver.desktop.cmake`:
   `X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input,zkde_screencast_unstable_v1,org_kde_plasma_window_management`.
3. `server/main.cpp`: add the option `window` ("Stream one window by its KWin uuid ({...})", value
   `uuid`). When it's set, call `controller.setWindow(...)` instead of `setMonitorIndex`. `--window`
   can't be combined with `--monitor` or `--virtual-monitor`.
4. `server/SessionController.h/.cpp`: add `QString m_window` and `setWindow()`. Next to the
   `setActiveStream` call (around line 137), add
   `if (!m_window.isEmpty()) wrapper->session->setWindow(m_window);`.
5. `src/AbstractSession.h/.cpp`: add `setWindow(const QString&)` and `window()` to Private, like
   `activeStream`.
6. `src/screencasting_p.h/.cpp`: add `Screencasting::createWindowStream(const QString &uuid, CursorMode)`,
   which calls `stream->d->init(d->stream_window(uuid, mode))` and leaves the size unset.
7. `src/PlasmaScreencastV1Session.cpp`:
   - Add `WindowManagement : QWaylandClientExtensionTemplate<WindowManagement>(16), QtWayland::org_kde_plasma_window_management`,
     set up like `FakeInput`.
   - Add `PlasmaWindow : QtWayland::org_kde_plasma_window`:
     - `geometry(x, y, w, h)` stores `QRectF geometry`;
     - `unmapped()` makes the session emit `error`.
   - In `start()`, before the `virtualMonitor()` branch, when `window()` is set:
     - create the stream with `createWindowStream(window(), Metadata)`;
     - set `d->window = get_window_by_uuid(window())`;
     - connect `ScreencastingStream::closed` to `error`, so a closed window ends the RDP session. The
       monitor path never connects `closed`.
   - In `sendEvent` MouseMove with `d->window` set:
     ```cpp
     const QRectF g = d->latched.isValid() ? d->latched : d->window->geometry; // latch: see press
     if (g.size() != d->scaledFor) { d->scale = size().width() / g.width(); d->scaledFor = g.size(); }
     // ponytail: scale fixed until the window itself resizes, so a popup to the right/below keeps
     // clicks right; a popup left of/above the frame shifts the origin and clicks are off while it's open
     global = g.topLeft() + position / d->scale;
     ```
   - In MouseButtonPress with `d->window` set:
     - call `d->window->set_state(state_active, state_active)`;
     - **latch** `d->latched = geometry` until the matching release, so a title-bar drag inside the
       window stream doesn't run away when the mapped area moves under the pointer;
     - then call `button()`.
   - Leave monitor mode as it is.

The check for this patch is a manual one on the host, like a `tests/krdp-window.sh`, described in step
0 below. krdp has no unit test harness worth adding for one branch.

## 2. Host units (cc-share install)

- A new template, `control-center-window@.service`, runs `cc-share window-run %i`. The instance is
  `<frame>-w<k>`, with k from 0 to 4. Braces aren't safe in unit names, so the uuid stays out of the
  instance name.
- `cc-share window-run`:
  - reads the uuid from `~/.config/control-center/windows/<frame>-w<k>.uuid`, which the agent writes
    (the agent's sandbox already allows that directory);
  - reads slot, user and password from `frames/<frame>.json`, like `frame-run`;
  - runs `exec krdpserver --plasma --window "$uuid" --port $((3405 + 10*slot + k)) -u ... -p ... --certificate ...`.
- Ports 3405-3409, 3415-3419 and so on up to 3449: the monitor digits 5-9 of each Frame's block.
  They're inside the existing firewall range 3399-3449, so the firewall doesn't change. Add
  `ponytail: a host with more than 5 monitors collides with window ports; move windows to their own range plus a firewall rule then`.
- Having its own template keeps it out of `adopt()`'s and `unpair`'s `frame@<frame>-<digit>`
  patterns. `unpair` (agent and cc-share) also has to stop `control-center-window@<frame>-*`.

## 3. Agent commands (home/agent.py)

The commands follow the existing `{"cmd": ..., "op": ...}` JSON. An older agent answers
`unknown-command`, and the Frame shows "this host can't pop out windows (update cc-share)".

| Request | Reply |
|---|---|
| `{"cmd":"window","op":"list"}` | `{ok, windows:[{uuid, app, caption, x, y, w, h, minimized}]}` |
| `{"cmd":"window","op":"grabbed","index":m}` | `{ok, uuid, app, caption, w, h, gx, gy, deco}`, or `{ok, uuid:null}` |
| `{"cmd":"window","op":"start","uuid":u}` | `{ok, port, w, h, ready:true}`, `{ok:false, error:"gone"}`, `{ok:false, error:"busy"}` |
| `{"cmd":"window","op":"stop","uuid":u}` | `{ok, stopped}`, with the same viewer and idle rules as `session stop` |

- **`list`** needs no KWin script. It runs
  `gdbus call --session -d org.kde.KWin -o /WindowsRunner -m org.kde.krunner1.Match ""` to get the
  `0_{uuid}` ids, then `org.kde.KWin /KWin getWindowInfo` for each one. That's enough for the CLI
  spike. Filter out `type != 0` and `excludeFromCapture`.
- **`grabbed`** is for the gesture. It only answers for a window in **interactive move**, which
  doubles as the title-bar test: a text selection or a slider dragged off the edge never pops out.
  - The agent loads a one-shot KWin script through `/Scripting loadScript`, then `run`, then
    `unloadScript`, the same sequence as kwin.rs:112-125.
  - The script reads `w = workspace.activeWindow` and checks `w.move` and
    `w.output.name == <monitor m's output>`. It answers with the uuid, `frameGeometry`,
    `cursorPos - frameGeometry.topLeft` (gx, gy in logical px × output scale = stream px) and
    `deco = clientGeometry.y - frameGeometry.y`.
  - It returns the result by calling
    `callDBus("org.controlcenter.Agent", "/", "org.controlcenter.Agent", "Picked", json)`. The agent
    owns that bus name with `Gio.bus_own_name` on a GLib main-loop thread; `gi` is on both hosts.
  - Reuse cc-windows.js `kind()` (copy it) for the filter.
  - I left out the point-under-cursor hit test from the KWin report (`stackingOrder` reversed,
    `frameGeometry` contains the point) on purpose. The `w.move` check needs no coordinates.
- **`start`**:
  - checks the uuid with `getWindowInfo`; an empty reply or `minimized` gives `gone` or `minimized`;
  - reuses the window's k if it's already running, otherwise takes the first free k;
  - writes the `.uuid` file and starts `control-center-window@<frame>-w<k>`;
  - waits for the port exactly like `session_start`, sharing its lock, `START_S` and `sessions_max`
    (window sessions count);
  - records `self.windows[(frame, uuid)] = {k, port, ip, idle_since}`.
- **`idle_stop`** covers window sessions with the same rule.
- **The spike never minimizes or parks the window.** It stays where it is and shows in both the
  monitor panel and its own panel. Parking it off-output or using excludeFromCapture comes later (see
  the unknowns).

## 4. cc-home CLI

These go next to `machine session start|stop <monitor>` (cc-home ~641, 1598), through the same
`agent_for(...)` and `cl.call`. The output is one `@window` line, so rdp.rs can parse it like
`@session`:

```
cc-home machine window list <machine>
    @window uuid={...} app=org.kde.dolphin w=1398 h=900 caption=...
cc-home machine window grabbed <monitor>
    @window <monitor> uuid={...} w= h= gx= gy= deco=    |  @window <monitor> state=none
cc-home machine window start <monitor> <uuid>
    @window <monitor> uuid={...} port=3405 w= h= ready=1   |  state=gone|busy|unsupported
cc-home machine window stop <monitor> <uuid>
cc-home machine window pop <monitor> <uuid>      # spike 1: asks the running cc-panels (panels socket) to pop it out
```

- `<monitor>` names the source monitor panel, so the CLI knows the machine and the Frame's
  credentials. It's the same name `session start` takes.
- `pop` writes `pop <monitor> <uuid>` to the cc-panels control socket that `panels_socket()` already
  finds, so the Frame does the panel work. The CLI is the spike's trigger.

## 5. cc-panels changes

**Spike 1 (the CLI pop-out):**
- **main.rs**:
  - Add `const POPS: usize = 2`, a pool of panels `pop-1` and `pop-2` built like the spares. They're
    separate from SPARES, so Add machine never runs out.
  - Make `Fill(OnceLock<Viewer>)` refillable: `Fill(Mutex<&'static Viewer>)`, set with `Box::leak`.
    `Deref` copies the `&'static` out, so the call sites don't change. Add
    `ponytail: leaks one Viewer (~few hundred bytes) per pop-out`.
  - Add `Panel::empty()` so a pop panel can be cleared.
- **A new popout.rs**:
  - `Vec<Option<Pop { src, uuid, port }>>` by panel index;
  - `pop(src, uuid)`:
    1. take a free pop panel; if there isn't one, the HUD shows "no free pop-out slot";
    2. if the same uuid is already popped, move its panel instead;
    3. clone the source viewer, with `name = pop-N`, `port`, and w and h from `window start`;
    4. `fill` the panel, then `place`, then `connect(p, 0)`;
  - `place(P, head, mpp, w, h, gx, gy) -> pose`, a pure function: the same physical size as in the
    monitor panel, facing the head, with the grab point kept under P;
  - `is(i)`;
  - **one unit test**: the placement keeps the grab point under P, and the width equals `w*mpp`.
- **rdp.rs**:
  - `session(p, start)`: for a pop panel, run `machine window start|stop <src> <uuid>` instead of
    `session start|stop`. A reconnect restarts the same window; `state=gone` frees the pop.
  - `on_desktop_resize`: also call `p.set_size(w, h)`, so mouse input follows a stream that a popup or
    a window resize changed. The picture stretches to the old overlay width until the next placement,
    which is fine for the spike.
- **grab.rs**:
  - in `owner()`, give pop panels `ctls = 1` (Close);
  - `Ctl::Close` on a pop runs `window stop`, then `disconnect`, then clears it from popout. Nothing is
    destructive: the window stays on the host;
  - skip `save_home_pose` for pops. They aren't saved and are gone after a restart.
- **kvm.rs**: in `drag_to`, the panel a drag carries on to must not be a pop (`!popout::is(i)`),
  otherwise monitor coordinates would go into window coordinates.
- The control socket gets the `pop <monitor> <uuid>` command.
- Keyboard, wheel and `Panel::mouse` don't change: a pop is an ordinary `Source::Rdp` panel.

**Spike 2 (the gesture):** see section 6.

## 6. The gesture (needs spike 1 plus the agent's `grabbed`)

The change goes in `Kvm::land`, in the `keep` branch, which has the raw `Some((t, ru, rv))` from
`pl.hit`.
- `out` is how far the raw (ru, rv) goes past the `one_seat`-expanded lo/hi box. It's already
  computed and then thrown away.
- **Arm** when all of these hold:
  - only BUTTON1 is held;
  - the held panel is a monitor panel (not a pop, not a window slot);
  - no other panel is hit;
  - `out > POP_OUT` (`ponytail:` tuning knob, start at 0.12 m).
- **On arm**, a thread runs `cc-home machine window grabbed <monitor>` and the answer is cached in
  `kvm.pop`.
- **Disarm** when `out` drops back under the threshold or any panel is hit.
- **Armed and answered:**
  - the cursor floats at the unclamped point (`free = Some(point(t))`);
  - the remote keeps getting the clamped x,y, so KWin's move holds at the edge;
  - the HUD shows "release to pop out".
- **Release while armed and answered:**
  1. send **Esc**, then the BUTTON1 release, to the monitor. Esc cancels KWin's interactive move, so
     the window goes back where it was: no quick-tile at the edge, no window left half off the output;
  2. push `PopOut { src, answer, at: point(t) }` onto a queue, which the main loop takes like
     `bar_clicks`;
  3. the main loop calls `popout::pop` with the placement from `at`, gx and gy.
- **No answer by release:** the request is held for 300 ms, then dropped, and you get an ordinary
  edge clamp.
- **Check:** a unit test of the arm decision covering overshoot, buttons, hit and source kind.

## 7. First runnable spike, smallest first

- **Step 0, host only:**
  1. Build krdp 1.12 on .63.
  2. Take a uuid from `gdbus ... WindowsRunner Match ""`.
  3. Run `krdpserver --plasma --window '{uuid}' --port 3405 ...` by hand and connect with xfreerdp
     from another machine.
  4. **Pass:**
     - the window's picture shows;
     - clicks and keys land, including when another window covers it;
     - moving the window on the host keeps input right;
     - dragging its title bar inside the stream doesn't run away;
     - closing the window ends the session.

  This is a dev diagnostic, not a product path.
- **Spike 1, CLI pop-out :**
  1. Run `cc-home machine window list <machine>`.
  2. Run `cc-home machine window pop <monitor> '{uuid}'`.
  3. A floating panel shows up in front of you with that window, and mouse, keys and wheel work.
  4. The card's × closes it, and the window stays on the host.
  5. Closing the app closes the panel.

  That's end to end through the agent, without the gesture.
- **Spike 2, gesture:** drag a title bar past the monitor panel's edge and let go, and the panel
  appears at the release point with the grab point under the cursor.

## 8. Risks

- **Memory:** a krdpserver takes about 400-800 MB (agent.md), and each pop-out is one more process.
  `sessions_max` (default 4) counts window sessions. Later: one krdpserver that takes the uuid per
  connection, from RDP AlternateShell or LB info (FreeRDP `/shell:`).
- **Covered windows:** fake_input clicks hit whatever window is on top at that global point. The
  press activates the window first, but asynchronously, so the first click on a covered window can
  land on the one above it.
- **Popups:** a popup left of or above the frame shifts the stream's origin, so clicks are off while
  it's open. A popup to the right or below changes the stream size, which means a krdp ResetGraphics,
  and FreeRDP 3.31 has to handle a mid-session DesktopResize. The picture stretches until we place the
  panel again.
- **Minimized windows** go blank and take no input, so never minimize to hide.
- **Esc during a move:** an app might see the Esc if KWin's move has already ended, say on a slow
  round trip. In most apps that's harmless.
- **Overlays:** +2 per pop (picture and card). With POPS=2 that's about 62 of SteamVR's 128.
- **Decode:** one more H.264 decode on the Frame per pop. The attention.rs levels apply.
- **Spoofable fake_input:** KWin accepts any `authenticate`, but that's already true for the monitor
  sessions. Nothing new is exposed, and the window session uses the same per-Frame login.

## 9. Unknowns to test on the hosts

1. Does KWin 6.7.5 send frames for a fully covered window stream (`refOffscreenRendering`)? Does a
   window on another virtual desktop stream?
2. Window stream size against frameGeometry at scale 1.25 on DP-1 and 1.0 on DP-3: is
   `size().width() / g.width()` the output scale? Does the stream rescale when the window moves to
   another output?
3. Does `org_kde_plasma_window.geometry` update live during a move, and fast enough for the path
   without the latch?
4. Does Esc during an interactive move started through fake_input cancel it and put the window back?
5. Does `w.move` read true in a one-shot script during a fake_input title-bar drag? Is
   `workspace.activeWindow` the dragged window?
6. Does KWin quick-tile when the clamped pointer is held at an edge between two outputs (.63 DP-1's
   right edge)? Does the pointer cross to DP-3?
7. Does `excludeFromCapture` also blank the window's own `stream_window`? D-055 says krdp's
   FilteredSceneView leaves excluded windows out. If it doesn't blank it, that's the cheap way to hide
   the duplicate later.
8. Does FreeRDP 3.31 in cc-panels survive repeated ResetGraphics, like opening and closing a big menu
   ten times?
9. Do the agent's sandbox (ProtectSystem=strict, PrivateTmp) and KWin's `callDBus` reach the agent's
   session-bus name?

## 10. Owners

- **Host side:**
  - `krdp/window-stream.patch` and the PKGBUILD bump, built on .63 and .85;
  - the cc-share `control-center-window@` template, `window-run`, and the unpair cleanup;
  - the agent's `window list|grabbed|start|stop`, the `org.controlcenter.Agent` bus name, and
    idle_stop;
  - cc-home `machine window ...`;
  - step 0 and the host unknowns (section 9).
- **Frame side:**
  - cc-panels POPS, the refillable `Fill`, popout.rs and its test;
  - the rdp.rs session branch and `set_size` on resize;
  - grab.rs Close and no home save; the `drag_to` exclusion;
  - the control socket's `pop` command;
  - spike 2's gesture in kvm.rs and its test.

---

Files I read for the facts above (these are the Python and bash originals; see the note at the top):
- home/agent.py (`do`, `slot_port`, `session_start`/`stop`, `idle_stop`)
- home/pair.py (SLOTS 1-4)
- cc-share (unit templates ~241-283, `frame-run` ~485)
- docs/agent.md (firewall 3399-3449)
- cc-home (`session` ~641, verb dispatch ~1598)
- crates/cc-panels/src/rdp.rs (`on_desktop_resize` :62, `session` :284, `mouse` :460)
- crates/cc-panels/src/main.rs (`set_size` :176, `fill` :228, `Panel::mouse` :285)
- krdp/PKGBUILD

---

## Adversarial review

## Adversarial review: pulling remote windows out of RDP panels

I read the krdp-6.7.5 and KWin-6.7.5 sources (scratch copies that weren't kept) and the repo files: home/agent.py, home/pair.py, cc-share,
cc-home, rdp.rs, main.rs, kvm.rs, back.rs, gpu.rs, panels/cc-windows.js and docs/privacy.md. I also ran
read-only checks over ssh on two hosts. I didn't edit, install or change anything.

**Verdict:** the core approach holds. A per-window krdpserver on `stream_window`, mapped from
`org_kde_plasma_window` geometry with a latch, is sound. But the input model, the security scope, the
process lifecycle and several of the factual claims need fixing first.

### A. Claims that are confirmed
- KWin's `ScreencastManager::streamWindow` (screencastmanager.cpp:51-71) only checks the compositing
  type and `findWindow`. There's no per-window permission check.
- `X-KDE-Wayland-Interfaces` does need `org_kde_plasma_window_management`. It's on KWin's restricted
  list (wayland_server.cpp:138-144), next to fake_input and zkde_screencast.
- The stream covers `boundingRect`: the frameGeometry of the window plus its popups, × targetScale
  (windowscreencastsource.cpp:94-102, 202-209). Its origin moves when a popup opens left of or above
  the window.
- `org_kde_plasma_window.geometry` is frameGeometry, and it updates live on every
  `frameGeometryChanged` (window.cpp:1763).
- `set_state(active)` calls `workspace()->activateWindow(this, true)` (window.cpp:1800).
- `get_window_by_uuid` matches the exact string, and plasma uuids are `QUuid::toString()`, so they're
  braced (plasmawindowmanagement.cpp:267, 314).
- **Esc cancels a move (unknown #4, answered from the source):** `Window::keyPressEvent` Key_Escape →
  `finishInteractiveMoveResize(true)` restores the geometry, the maximize state and the quick-tile state
  (window.cpp:2559, 1092-1099). The host test only has to confirm that a fake_input key reaches the move
  filter.
- **Window closes:** the source emits `closed` when its last window closes, and the stream follows
  (screencaststream.cpp:332). krdp's monitor path never connects `closed`.
- **Resizes:** krdp handles a size change mid-session through `VideoStream::performReset` →
  ResetGraphics plus a new surface. `sizeChanged` → `AbstractSession::setSize`, so `size()` is the
  stream's pixel size and the `size().width()/g.width()` ratio is usable.
- **Scripting:** `w.move` (`isInteractiveMove`), `w.output`, `internalId`, `excludeFromCapture`,
  `workspace.activeWindow` and `callDBus` all exist in 6.7.5. kwin.rs and cc-windows.js already use the
  bus-name-plus-callDBus pattern.
- **Hosts:** both have `gi`, krdp 6.7.5-1.11 and kwin 6.7.5-1.1.

### B. Corrections (wrong or missing)
1. **Input on another desktop, or on minimized or covered windows.** The design says the stream "keeps
   working when the window is on another virtual desktop". The picture probably does, because
   `refOffscreenRendering` keeps the window from being suspended and the GL renderer doesn't check the
   root item's visibility. Input doesn't. fake_input injects at global coordinates, so clicks land on
   whatever's visible there on the current desktop, which is a different window.
   - Activating on press changes things on the host: `activateWindow` switches the desktop or
     unminimizes, then raises.
   - Keys always go to KWin's focused window. The design never makes the popped window focused before
     you type, so keystrokes typed into a pop can end up in another app.
   - **Fix (in krdp's `--window` mode):** track the window's active state from
     `org_kde_plasma_window.state`. While it isn't active, a press sends `set_state(active)` and is
     swallowed (click to focus). Buttons, wheel and keys only pass through while it's active. That one
     rule covers covered windows, misdirected keys, minimized windows and other desktops.
   - "Minimized windows go blank" isn't verified and is probably wrong. Leave it in the unknowns.
2. **Dialogs never appear in the pop.** `WindowScreenCastSource` only adds `isPopupWindow()`
   transients, so a dialog like a file chooser or a confirm box only opens on the monitor panel. Add it
   to the risks; for now the user handles it in the monitor panel.
3. **excludeFromCapture (unknown #7, answered):** window streams ignore it. Only FilteredSceneView,
   which output and region streams use, checks it (filteredsceneview.cpp:23). So `stream_window` would
   leak a window the user hid from screencasts, and the agent has to refuse those windows (section C).
   Using excludeFromCapture to hide the duplicate would work, but it reuses the user's privacy flag, and
   the agent's own refusal check would then reject that window. Use something else.
4. **Geometry is integer.** `org_kde_plasma_window.geometry` sends `frameGeometry().toRect()`, so "keep
   it as qreal in the mapping" can't apply in krdp. Expect about 1 logical px of error. Only the KWin
   script sees RectF.
5. **Braces.** `streamWindow` and `getWindowInfo` parse with QUuid and take either form; only
   `get_window_by_uuid` needs the braced one. Validate the uuid with `^\{[0-9a-f-]{36}\}$` in the agent
   and in `window-run`.
6. **`WindowsRunner Match ""` returns every window twice.** I checked on .63: each `0_{uuid}` shows up
   twice, because the empty term matches both the name loop and the desktop-name loop
   (windowsrunnerinterface.cpp:157-190). Dedupe the ids.
7. **Ports.** Slots are 1-4 (pair.py:402), so the window ports are 3415-19, 3425-29, 3435-39 and
   3445-49. 3405-3409 is slot 0, the shared login. That login is never paired and cc-home answers
   `port < 3410` without the agent, so a slot-0 panel can't pop out: answer `state=not-paired`. 3405 is
   fine for the manual step 0.
8. **`window start` has to run on a thread.** `commands()` only threads `session start`
   (agent.py:445). A 10 s `window start` would block that Frame's link, `grabbed` included.
9. **Orphaned servers after an agent restart.** `adopt()` only rebuilds `frame@` sessions, so window
   krdpservers (about 400-800 MB each) would never be idle-stopped.
   - Drop the `.uuid` file. Put k and the uuid in the instance name instead:
     `control-center-window@<frame>-<k>-<uuid without braces>`, which is safe in a unit name.
     `window-run` adds the braces back, and `adopt()` can rebuild `self.windows` from the unit list.
   - That also gets rid of the sandbox write.
10. **Unpair and Stop.** `unpair` (agent and cc-share) uses `list-unit-files`, which misses instances
    that were started but never enabled. That's already a bug for agent-started `frame@` units. Use
    `systemctl --user stop 'control-center-window@<f>-*'`, and the same for `frame@`. `cc-share down`
    (the kill switch, cc-share:346) has to stop `control-center-window@*` too.
11. **A closed window should end the process, not just the connection.** `SessionController` makes a
    new session per connection and `sessionError` only closes that connection, so the server keeps its
    port and its memory. In `--window` mode, call `QCoreApplication::exit(0)` on `closed`;
    `Restart=on-failure` won't restart it.
    - On the Frame, `rdp.rs run()` falls back to `p.v.port` when `session()` returns None, so it would
      reconnect to a dead port forever. `state=gone` has to end the loop.
12. **Resize on the Frame.** SteamVR keeps the texture's aspect at a fixed width, so the picture
    doesn't stretch. What goes stale is kvm's cached `place[k].height`, which hits use.
    - On resize, run `set_pose` again with the new aspect, as well as `set_size`.
    - A drag-resize on the host gives a burst of ResetGraphics (each one recreates the surface and the
      H.264). Add that to unknown #8, and throttle in krdp if it hurts.
13. **kvm's `out` isn't computed yet.** Today there's only the clamp; computing it is one line from
    ru/rv against lo/hi. Also, lo/hi reach to the middles of same-machine neighbours, so a pop can't
    arm toward a neighbouring monitor (that hits the neighbour and disarms). Write that down; it keeps
    cross-monitor drag working, which is good.
14. **Gesture timing.** Each `grabbed` call is distrobox-host-exec + Python cc-home + TLS and auth +
    loading, running and unloading a KWin script. Expect 0.5-1.5 s, so a 300 ms hold will mostly miss.
    Keep the host button held (the move clamped at the edge) until the answer comes back or about
    1.5 s pass, and only send Esc on a positive answer.
15. **Installing 1.12 replaces the krdp every monitor session uses**, and step 0 doesn't need it. KWin
    resolves interfaces through KApplicationTrader by the canonical Exec path (utils/serviceutils.h:26-49),
    which includes `~/.local/share/applications`. So a build-dir krdpserver plus a user-local `.desktop`
    file (`NoDisplay`, `Exec=<abs build path>`, the three interfaces; then kbuildsycoca6) can be tested
    without touching the package.
16. **The "spoofable fake_input" risk is framed wrong.** `authenticate` always succeeds
    (fakeinputbackend.cpp:118), but the interface is gated by the desktop file. The real new grant is
    this: adding plasma_window_management to the shared desktop file gives every krdpserver, monitor
    servers included, the full window list with captions, plus close, move and minimize on any window.
    That's acceptable because krdp already has fake_input, but say so.
17. **Smaller points.**
    - The crop fallback isn't "no host change": it needs the window rect from the agent, and keepAbove
      changes the host's window.
    - Parts of the stream that no window covers are transparent and encode as black, for example
      around popups and rounded corners.
    - Odd stream sizes (like 1747×1126) are an unknown for kpipewire's H.264 encoder.
    - `getWindowInfo` has no output field, so scoping uses x/y/w/h against the monitor rects.

### C. Security scope (missing from the design)
The Frame already sees the shared monitors and has fake_input. A window stream adds what those
monitors don't show: windows on unshared monitors, on other desktops, minimized, or excludeFromCapture,
plus every caption through `list`. The rules for the agent:
- **`start`** requires all of these, or it answers `{ok:false, error:"not-shared"}`:
  - type 0 and managed;
  - not `excludeFromCapture`;
  - not minimized;
  - on the current desktop;
  - frame geometry that intersects a shared monitor (`pair.shared_monitors()` plus kscreen positions
    and scale).
- **`list`** returns only those windows, and no captions from unshared monitors.
- Logs carry the app id, never the caption, in the same style as `print(f"agent: {frame} {cmd}")`.
- **Limits:** at most POPS=2 windows per Frame, counted in `sessions_max`, and only the per-Frame
  login.
- **Kill switches:** lock, unpair and `cc-share down` stop that Frame's window units.
- **Known gap** (mark it with `ponytail:`): the check only runs at start. If the window later moves to
  an unshared monitor, the picture still follows it; the active-gating in B1 at least keeps input
  honest. The upgrade is for krdp to end the stream when the window's geometry leaves the shared
  outputs.

### D. Simpler or cheaper options
- No `.uuid` file and no file for k; the instance name carries both (B9).
- Step 0 doesn't need the PKGBUILD bump (B15). Bump to 1.12 only once step 0 passes.
- Put off the crop fallback. It isn't zero host change, and it duplicates the gesture work.
- Later: one window server per Frame, since krdp already makes one session per connection. The uuid
  would come per connection from AlternateShell, which needs `start()` held until logon. Not for the
  spike.

### E. Revised first spike

**Step 0: host only (.63), dev build, nothing installed**

The patch is the design's krdp patch with these changes:
- `--window <uuid>` mode;
- `createWindowStream`;
- an `org_kde_plasma_window` handle for the integer geometry, the active-state event and `set_state`;
- mapping `g.topLeft() + pos * g.width()/size().width()`, with the latch held from press to release;
- input gated on active: an inactive press activates the window and is swallowed, and keys only pass
  while it's active;
- `closed` → `QCoreApplication::exit(0)`.

Build it in ~/src. Add a user-local desktop file whose Exec is the build binary, then run
kbuildsycoca6. Run it by hand on a free firewall port (for example one of an unpaired slot's window
digits) and connect from the Frame with xfreerdp or cc-view. Measure RSS while connected.

Pass criteria:
- the picture shows;
- clicks land after a click to focus, including when the window is covered;
- keys only go to it;
- moving the window on the host keeps input right;
- a title-bar drag inside the stream doesn't run away;
- a context menu to the right or below shows and clicks right;
- the window closing ends the process.

Record:
- whether minimized and other-desktop windows show a picture;
- what a dialog does;
- how drag-resize ResetGraphics behaves;
- odd sizes;
- the stream at scale 1.25 (DP-1) against 1.0 (DP-3).

**Spike 1: CLI pop-out through the agent**

Agent:
- `window list|start|stop` with the section C scope;
- `start` on a thread;
- the unit `control-center-window@<frame>-<k>-<uuidhex>` on port 3405+10·slot+k;
- `adopt()`, `idle_stop`, unpair and `cc-share down` cover window units;
- `list` dedupes the WindowsRunner ids.

cc-home: `machine window list|start|stop|pop`, with `@window` lines.

cc-panels:
- POPS=2 and the refillable `Fill`; `used()` and `fill()` touch `.0` today and need updating;
- popout.rs `place()` with its one test;
- the `rdp.rs` session branch, where `state=gone` ends the loop;
- `on_desktop_resize` → `set_size` + `set_pose`;
- Close on the card;
- `drag_to` leaves pops out;
- the control socket's `pop` command.

Pass: list, then pop; the panel works; × closes it; closing the app closes the panel; an agent restart
adopts the window units again; `cc-share down` kills them.

**Spike 2: the gesture, unchanged except:**
- compute `out` (B13);
- `grabbed` uses the one-shot script with `w.move` and `w.output`, answering through callDBus to a
  GLib-owned `org.controlcenter.Agent`;
- hold the host button until the answer comes back or about 1.5 s pass, and only send Esc on a
  positive answer (B14);
- one test of the arm decision.

**Unknowns still open:** #1 (a picture for minimized and other-desktop windows), #2, #3 (how fast
geometry updates live), #5, #6, #8 (now with resize bursts), #9, odd H.264 sizes, and whether a
fake_input Esc reaches MoveResizeFilter.


## Built so far (2026-10-02)

### Step 0 (on .63, by hand)

- `krdp/window-stream.patch` (on top of pointer-offset and clipboard) has:
  - `--window {uuid}`, with the braced uuid checked and not allowed with `--monitor`/`--virtual-monitor`;
  - `createWindowStream`;
  - a process-wide `org_kde_plasma_window_management` and a per-session `org_kde_plasma_window`
    (integer frame geometry, active state, `set_state`);
  - input mapped `g.topLeft() + pos × g.width()/size().width()`, with the geometry latched from a
    press to its release;
  - **click to focus** (B1): an inactive window's press activates it and is swallowed along with its
    release, and keys and wheel only pass while it's active;
  - the window unmapping ends the process, while its stream closing only ends that connection.
- **The window server is its own executable** (review W1): it's the build binary, granted
  `org_kde_plasma_window_management` by its own user-local `.desktop` file
  (`~/.local/share/applications/command-center-krdp-window.desktop`). The shared krdpserver desktop
  file, which the monitor servers use (slot 0 too), is unchanged.
- **Results:** the picture shows; a headless FreeRDP client on the Frame saw exactly the Dolphin
  window. The stream is the frame size × the output scale (1440×914 for 1152×731 at 1.25), so the
  mapping ratio is the scale. RSS was 87 MB idle, about 300 MB with a client, and 220–250 MB after it
  left. .63 and .85 have no output at scale 1.0, so I couldn't compare 1.25 against 1.0 there.
- **I fixed two krdp bugs on the way:** the server quit when its first client left (the stream closes
  on disconnect too; now only the window going away ends it), and it crashed in
  `handle_window_with_uuid` after a session ended (a per-session window-management object was freed
  while KWin was still sending it events; now there's one per process).
- **Interactive checks** (click to focus, keys, a moving window, a title-bar drag, menus, the window
  closing; minimized, other desktop, dialog, resize, odd sizes) happen through the pop panel in spike
  1, not by driving input into the user's desktop unattended.

### Spike 1, host side

- **Agent:** `window list` / `start` / `stop` as in section 3, with the §C scope (normal, not
  `excludeFromCapture`, not minimized, on the current desktop, on a shared monitor), read from KWin
  over D-Bus with `gi` (WindowsRunner ids deduped, `getWindowInfo`). `start` runs on its own thread.
  The unit is `control-center-window@<frame>-<k>-<uuid>` (k < 2 per Frame) on port 3405 + 10·slot + k.
  Window sessions share `sessions_max`, the idle stop and adoption with monitor sessions, and unpair
  stops window and monitor servers that were only started, never enabled (B10).
- **the review's D-056 points:**
  - **W1:** the window server's own executable and `.desktop` grant (above). `cc-share windows on`
    writes it for whichever window server is built.
  - **W2:** window streams are a host opt-in, off by default (`cc-share windows on|off|status`).
    `list` gives the app id and geometry, and captions only with
    `~/.config/control-center/windows-captions`.
  - **W3:** any process of the same user can spoof the `Picked` D-Bus callback (spike 2's `grabbed`).
    That's low risk, since `start` checks the window's scope again.
  - **W4:** `start` refuses with `busy` when free memory is under `window_min_free_mb` (host
    settings.json, default 1024), as well as past `sessions_max`.
  - **W6:** agenttest covers windows staying off without the opt-in, each scope refusal, captions
    only with their opt-in, bad uuids, the same window twice, the memory floor, a third pop-out, the
    rate limit, and unpair stopping window servers. selftest-nossh lists, starts and stops one through
    cc-home.
  - **W7:** `list` and `start` together are limited to `window_asks_per_min` (default 20) per Frame.
- **cc-share:** `window-run` checks its instance (Frame name, k, uuid) and the opt-in, then runs the
  window server. `down`, `unpair` and `uninstall` stop window servers.
- **cc-home:** `machine window list <machine|monitor>` and `start|stop|pop <monitor> <uuid>`, with
  `@window` lines as in section 4 (`state=not-paired` for a monitor on the shared login,
  `unsupported` for an older agent).

## My decisions

- "Let's expermint with dragging windows out of the rdp panels"; the vision: "if this could be a budget citrix server, this would be legendary".
- "maybe both, but I don't want to do full remoteapp out the gate by default, because a workstation could have a ton of windows opened": so the default is opt-in pop-out (only the windows you drag out), with full RemoteApp maybe later as an explicit mode.
