# Design 2 (final): every Frame window is its own Command Center panel, with a floating taskbar

**Where this stands now (2026-10-03).** I wrote this design on 2026-10-01 against an earlier
nested Plasma session (a KWin with four outputs, WL-0 to WL-3, and a wrapper script around it),
and the stages below record what I built against it. Command Center runs its own nested Plasma
session now, so a few things in the plan read differently today. The code is the reference for these:

- **The session** is Command Center's own: KWin with `--virtual --output-count 3`
  (`crates/cc-home/src/session.rs`). Virtual-0 holds Plasma's panel and its popups, Virtual-1
  every program window, and Virtual-2 is the pointer's park. Wherever the plan says WL-0 to WL-3,
  the park (WL-3) is now Virtual-2 (`windows.rs` `PARK`).
- **Discovery** looks for the plasmashell with `XDG_RUNTIME_DIR=/run/user/1000/cc-desktop`, and
  falls back to `~/.cache/control-center/desktop-session.env` (`crates/cc-panels/src/session.rs`).
- **Focus and the edge barrier** aren't backed up and restored by a wrapper any more. cc-home
  sets `FocusPolicy=ClickToFocus`, `EdgeBarrier=0` and `CornerBarrier=false` in the session's own
  kwinrc when it makes that session.
- **Window slots** are 16, at 2 overlays each (the picture, and its card with the bar and tag
  drawn in), per `windows.rs` `SLOTS`.
- **Tags and glyphs** are drawn by `crates/cc-panels/src/assets.rs` in Rust. assets.py is gone.
- **The `cc-panels` wrapper** is a link to cc-home now, not a bash script.
- **Desktop mode** (the first note below) isn't built: there's no `frame_desktop` setting in the
  code yet.

> **Desktop mode (my call, 2026-10-01):** "let's also allow users to just use a standard desktop
> panel, with child panels acting as monitor 2,3,4, etc". The plan: a setting `frame_desktop` in
> settings.json, either `"windows"` (this design: a panel per window) or `"desktop"`. In desktop
> mode, monitor 1 is a violet panel streaming the session's output (zkde_screencast
> `stream_output`). "Add monitor" creates a KWin virtual output (`stream_virtual_output(name,
> size, scale)`, the same thing KDE's "extend to a virtual display" does), streamed into its own
> violet child panel, tagged MONITOR n, with the card, grab bar and spot. Windows move between
> them as on any multi-monitor desktop. It reuses the same capture, input (fake_input absolute in
> that output's global geometry) and card code, with no per-window tracking. S0 adds a
> `--virtual 1920x1080` check.
>
> Lifetime: a virtual output only lives as long as our screencast stream. KWin removes it when
> the stream closes or cc-panels exits, and moves its windows onto the remaining outputs (I
> inferred this from KWin's screencast plugin; S0 checks it). So nothing is left behind on exit.
> On the next start cc-panels re-creates the saved monitors (settings: `monitors` count and
> sizes) and moves each window back to the monitor it was on (window uuid/app → monitor index,
> kept in ~/.cache/control-center/win-poses.json) with the same KWin script.

> **My decisions (2026-10-01):** zoom is grip + corner drag (a plain corner drag resizes the
> window). The edge-barrier test may change the session's kwinrc with a backup (restore it after, or
> keep it off if that helps). On the stock desktop, cc-panels may change ~/.config/kwinrc's focus
> policy while it runs, backed up and restored when it exits.

## The idea

Every program window in the nested KWin 6.2.5 session gets its own Command Center panel, with the
violet card (`#A48BFF`), grab bar, border tag and spot. Pixels come from KWin's own screencast
protocol and go into SteamVR as DMA-BUFs. Input goes back through `fake_input`.

A floating taskbar carries the Apps launcher, one chip per window and per remote machine, and a
clock. Where it sits is set in `~/.config/control-center/settings.json` → `"taskbar": "fixed" |
"follow" | "wrist"`.

If stage 0 had failed, the fallback was design 1: gamescope's per-window dashboard overlays,
which I'd already proven in the headset.

What this rests on:

- Sources: KWin and xdg-desktop-portal-kde 6.2.5, my earlier input and windows research, and the
  cc-panels source.
- "Verified" means I read it in source or ran a read-only check. "Inferred" means it still needs
  a live run.

---

## 1. Architecture

All the work happens in cc-panels, inside cc-box. There's **no helper process on the host**,
because cc-box runs with `PidMode=host` and the binary lives under `/home`.

| Piece | Where | Job |
|---|---|---|
| `session.rs` | cc-panels, its own thread | **Discovery.** Picks the `plasmashell` whose `XDG_RUNTIME_DIR` matches the target session (it's `/run/user/1000/cc-desktop`). Reads `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`, `DISPLAY` and `XAUTHORITY` from its `/proc/<pid>/environ`. Inside cc-box that file is off limits ("Permission denied"), so the session's environment is also written to a file in `~/.cache/control-center/` (`desktop-session.env`), and discovery falls back to it. **It never calls `set_var`** (edition 2024, many threads).<br>**Wayland.** Connects with `UnixStream::connect(rt/wl)` and `Connection::from_socket`, through a `wp_security_context_v1` socket (§12). Binds `zkde_screencast_unstable_v1` (v≤3), `org_kde_kwin_fake_input` and `zwlr_data_control_manager_v1`. This thread only runs `blocking_dispatch`. The other threads call `fake_input.*` and then `conn.flush()` directly; there's no command channel.<br>**Reconnect.** Retries every 5 s. |
| `capture.rs` | cc-panels, one PipeWire `MainLoop` thread | Holds one `pw::Stream` per shown window and hands frames to the main thread (§3). |
| `kwin.rs` | cc-panels, zbus on the nested bus | Owns the bus name `org.controlcenter.Panels`. The script reports with `Event(s json)`. `Next() -> s` is a long poll that **always answers within 10 s**, with `""` when there's nothing to send. On every connect it calls `unloadScript("cc-windows")`, then `loadScript` and `run`. |
| `panels/cc-windows.js` | Inside KWin | **Reports** `add`, `remove`, `geom` (clientGeometry), `caption`, `activated`, `raised` and `minimized`, and sends `add` for every existing window when it loads.<br>**Carries out** `place`, `raise`, `activate`, `close`, `restore` and `park`.<br>**Saves** each window's original `{noBorder, frameGeometry, minimized, onAllDesktops}` when it adopts it.<br>**Stops** polling on a D-Bus error, so it never spins after cc-panels exits. |
| `taskbar.rs` | cc-panels main loop | §10 |
| `org.controlcenter.panels.desktop` | `~/.local/share/applications` | The grant for the restricted interfaces (§12). |

Threading rule: **OpenVR is only called on the main thread**, and **`pw_stream_queue_buffer` is
only called on the PipeWire thread**.

## 2. Panel model

- **New field.** `Panel` gets `src: Source`:
  - `Rdp` keeps today's fields.
  - `Window { slot: Mutex<Option<Win>> }`, where `Win = {uuid, key, cg, parent, kind, hidden}`.
- **Fixed pool.** At startup cc-panels creates a fixed number of window slots after the krdp
  panels. The count moved around: this section first said 32 window + 8 popup slots, S1 shipped
  16, 16 hit SteamVR's 128-overlay cap live and I dropped it to 8. Once a slot went
  from 4 overlays to 2 (the card, bar and tag drawn into one), it went back to 16, which is what
  `windows.rs` has now. The 128 is shared by every app (Steam's dashboard too). Popup slots don't
  exist yet (S3b); each would cost 1 overlay (no card) out of the same 128, so size them against
  what's left.
  - Window slots get `grab::create(name, VIOLET, None)`, and `VIOLET` becomes `pub`.
  - Popup slots have no card.
  - Everything that works by panel index stays the same: `Grab`, `kvm.place`, paint order and
    `@controlcenter`.
  - Code note: `// ponytail: fixed pool; overlays made on demand if it's too few.`
- **A full pool never loses a window.** Extra windows go on an `unslotted` list, the taskbar
  shows a "+N" chip, and `windows` lists them. When a slot frees up, the oldest unslotted window
  takes it.
- **Size changes at runtime.**
  - Add `Panel.size: [AtomicU32; 2]` and `p.size()`.
  - Replace the roughly 12 uses of `p.v.w` / `p.v.h` in kvm.rs and main.rs: laser hits,
    `aim_at`, `ppd`, the mouse scale and `Placement::from_matrix` (`rdp.rs:276-277` keeps its own).
  - Recompute `kvm.place[i]` whenever the size changes.
  - `tag_aspect` becomes an atomic too.
- **Saved spots.**
  - The key is `app:<desktopFileName>`, or `app:<resourceClass>` when the desktop file name is
    empty.
  - Only the first open instance of an app uses its `app:` spot. Later ones cascade 8 cm from it.
  - Live poses also go to `~/.cache/control-center/win-poses.json` as uuid → pose, so restarting
    cc-panels with the windows still open puts every panel back exactly where it was.

## 3. Data flow for one window

**Capture**

1. The script sends `add`.
2. cc-panels calls `stream_window(uuid, pointer=1)`. Pointer 1 means hidden, because our laser
   draws its own.
3. KWin answers `created(node)`. KWin and cc-box share one PipeWire daemon, so the node id works
   as it is.
4. cc-panels calls `pw_stream_connect(INPUT, node, AUTOCONNECT)` **without MAP_BUFFERS**,
   offering two formats:
   - (a) `BGRx` with modifier `CHOICE_ENUM_Long(LINEAR, LINEAR)`, flagged
     `MANDATORY|DONT_FIXATE`. That's how OBS does it.
   - (b) `BGRx` with no modifier, which gives shared memory (MemFd).
5. **Negotiation.** Expect two `param_changed` rounds, because KWin fixes the modifier itself.
   Only set `Buffers{DmaBuf|MemFd, 4}` once the modifier is fixed.
   - The negotiated format is `XRGB8888` LINEAR, which is exactly what gpu.rs already imports.
   - Transparent areas show as black (inferred).
6. **`add_buffer`** (PipeWire thread). If `n_datas == 1` and the buffer is a DMA-BUF, post a
   request to the main thread to import `{fd, chunk.offset, chunk.stride, modifier}`. Otherwise
   `mmap` the MemFd.
7. **`process`** (PipeWire thread).
   - Dequeue the newest buffer and put it in that slot's `Mutex<Option<Frame>>`.
   - If an older frame was still waiting there, queue it back.
8. **Main loop tick.**
   - `ImportDmabuf` new buffers once each and cache the handle per `*mut pw_buffer`. **Never
     close the fd**: PipeWire owns it.
   - Call `poll(fd, POLLIN, 0)` to wait for the implicit-sync fence. If it isn't ready, skip to
     the next tick.
   - Call `SetOverlayTexture`.
   - Keep the shown buffer and the one before it. Send older ones back through a `pw::channel`
     so the PipeWire thread queues them.
9. **MemFd fallback.** `gbm_bo_map` plus copying the VideoDamage rows. Only generalise
   `Buffers::upload`, to take `src: &[u8], stride`. Window buffers live in a separate
   `WinBuffers { by_buf: HashMap<*mut pw_buffer, Handle>, shown: [Option<_>; 2] }`.
10. **Buffer removal.** `remove_buffer` posts an `UnrefResource` request to the main thread.
11. **Resize.** KWin re-announces the format and the buffers get replaced. Refit `p.size()` and
    the mouse scale to the stream's W×H.

**Cost control**

Every stream makes its window render offscreen at the output's refresh rate, which isn't free. So:

- Hiding a panel sends `close` on the zkde stream and drops the `pw::Stream`. Showing it opens a
  new stream.
- Freeing a slot always sends `close`.

**Coordinate mapping**

- The stream covers exactly `clientGeometry × preferredBufferScale`. Decorations and CSD shadows
  are clipped out.
- OpenVR mouse coordinates are in stream pixels from the bottom-left, so:
  - `gx = cg.x + mx/scale`
  - `gy = cg.y + (H − my)/scale`
- Scale is 1 today.

## 4. Input and focus

**Pointer**

- Use `fake_input`:
  - call `authenticate` once (without it every event is dropped)
  - `pointer_motion_absolute`
  - `button` (`BTN_LEFT=0x110`)
  - `axis` at 15 units per notch, with a `CC_WHEEL` calibration knob
- The laser and the physical mouse (`kvm.mv`, when `k.active` is a window slot) use the same
  mapping.

**Raise gate (there's no stacking model)**

KWin scripts have no stacking-changed signal, so I don't model the stacking order at all.
Instead, a press only goes through once its window is known to be on top:

- **Hover.** When the laser or mouse enters a different window panel, send `raise W`. The script
  calls `workspace.raiseWindow(w)` and answers `raised`. This is skipped while a popup of the
  hovered window is open.
- **Press.**
  - If `last_raised == W`, or the click target is a transient or popup of W, send the press.
  - Otherwise set a per-slot `pending_press` and send `raise W`.
  - When `raised W` arrives, send the motion again and then the button.
  - If it hasn't arrived after 3 ticks (33 ms), **drop the press** and show the press glow.
    Losing a click is better than the wrong window getting it.
  - The main loop never blocks.
- **Resetting.** `last_raised` is cleared on every `add`, `remove` and `activated`.

**Focus**

- A session with `FocusFollowsMouse` and `DelayFocusInterval=0` lets our pointer motion steal
  focus, so cc-home sets `ClickToFocus` in Command Center's session.
- With ClickToFocus, KWin's default click action already activates and raises the clicked
  window. So there's no extra `activate` before a click; only taskbar chips use `activate`.

**Keyboard**

- Keys go to the panel the user last clicked (`kvm.kbd`).
- When that's a window slot: `keyboard_key(evdev, state)`, without `scancode()`.
- Kernel repeat events (value 2) are dropped, because the Wayland client repeats keys itself.
- Pressed keys are tracked and all released when `kbd` changes or cc-panels exits.
- Modifiers work, because the keys go through KWin's xkb, which uses
  the session's kxkbrc. That layout has to match the one krdp uses (an S2 check).

**Pointer park**

- **The park output stays empty** (WL-3 then, Virtual-2 now). Windows only go on the other
  outputs.
- When the laser and mouse leave every window panel, the pointer moves to the park's centre, so
  tooltips close and hover states clear.

**Edge barrier**

- KWin applies the edge barrier even to absolute motion: 100 px at an edge, and up to 2000 px at
  a corner with `CornerBarrier` on.
- So it has to be off: `[EdgeBarrier] EdgeBarrier=0, CornerBarrier=false`. cc-home sets it in the
  session's own kwinrc.
- Each motion is sent once; there's no double send.

## 5. Layout inside the session

- **Where windows go.** The script places each window at the top-left of the `MaximizeArea` of
  the output that fits it. In the four-output session that was:
  - WL-2 (3840×1080) for wide windows
  - WL-1 (1440×2560) for tall ones
  - WL-0 otherwise
- **Overlap is fine.** Capture ignores stacking, and the raise gate (§4) keeps input correct.
- **Window state.**
  - `noBorder=true` only saves output space, and only applies to windows with server-side
    decorations.
  - Windows stay unmaximized, because a maximize change resets `noBorder`.
  - While a window is `fullScreen` or maximized from inside the app (F11 and the like), `place`
    does nothing and the panel just follows `cg`.
- **Resizing at 1:1 pixels.**
  - Panel width in metres = `cg.w / window_px_per_m`. The density comes from settings.json,
    default 1400, and it's a calibration knob.
  - **A plain corner drag** resizes freely on both axes. The texture stretches live, and every
    150 ms and on release cc-panels sends `place{w,h}`, capped at the output's `MaximizeArea`. A
    bigger window moves to a bigger output.
  - **A corner drag while holding grip** scales that panel's density instead (zoom), and the
    density is saved with its spot.
  - Either way, the client's reported `clientGeometry` decides the final size: the panel snaps to
    `cg / density`.
- **Ceiling.** A window could be at most 3840×1080 or 1440×2560 in the four-output session, and
  1280×800 on the stock desktop.

## 6. Lifecycle

**Adopt or ignore**

| Window | Becomes |
|---|---|
| `managed && normalWindow`, and either `!skipTaskbar` or at least 200×150 in size | Window slot |
| plasmashell, krunner, cc-view's paused viewers (`resourceClass` `cc-view-<name>`, `desktopFileName` `com.freerdp.FreeRDP`, R-3) | Ignored |
| Dialog, or managed with `transientFor` set | Window slot, placed 5 cm in front of its parent panel |
| popupMenu, dropdownMenu, comboBox, tooltip, X11 unmanaged | Popup slot (§7) |
| Desktop, dock, notification, OSD | Ignored. `// ponytail: mirror notifications on the taskbar later` |

**When a window is adopted**

1. The script saves its original state, then sets `minimized=false` (also on every later
   `minimizedChanged`), `onAllDesktops=true`, `noBorder` and `place`.
2. cc-panels takes a slot and starts the stream.
3. The overlay is shown on the **first imported frame**, so there's no black flash.
4. The pose comes from the first of these that exists: the grace map, `win-poses.json`, the
   `app:` spot, or 1.2 m ahead of the head with an 8 cm cascade.
5. The tag is fetched (§9).

**Idle windows**

KWin only sends frames on damage, so a window that isn't drawing sends nothing. If no frame has
arrived 300 ms after `created`, the script nudges the window (a 1 px resize and back).

**Closing**

- `remove`, or the stream's `closed`, does all of this:
  - unref the handles
  - send `close` on the zkde stream
  - hide the overlay
  - free the slot and adopt the next unslotted window
- Child dialogs and popups go with their parent.
- The card's Close button sends `close`, and the script calls `w.closeWindow()`. A save prompt
  shows up as a dialog panel.

**Hiding**

- **The KWin window is never minimized**, because its stream would go blank. (The Plasma
  taskbar work later changed this; see "Plasma taskbar as built" in §16.)
- "Minimize" hides our overlay and closes the stream. The chip stays, dimmed.
- `summon` on `@controlcenter`, or a long-press on Apps, fans every window panel out in front of
  the user.

**Lost connection**

- Window slots stay frozen on their last frame for **15 s**.
- When the script loads again it re-sends `add` with the same UUIDs, and those go back into the
  same slots with the same poses.
- After 15 s the slots are freed, and their poses stay in `win-poses.json`.
- A refused grant is logged once, a red "no window access" tag goes on the taskbar, and the
  panels stay frozen. They're never dropped.

**Exit (never lose windows)**

1. `GiveBack::drop` in main.rs sends `restore`. The script puts back every saved `{noBorder,
   frameGeometry, minimized, onAllDesktops}` and unloads itself.
2. The script mirrors the saved states to cc-panels with `callDBus`, and cc-panels writes them to
   `~/.cache/control-center/kwin-restore.json`.
3. For crashes and `kill -9`, the wrapper turns that file into a one-shot `restore.js` and loads
   it.
4. That makes the `--for` time limit (default 2 min) safe, because every exit restores.

## 7. Popups, menus and tooltips

**Main path: crop from an output stream**

- Each nested output that holds windows gets one `stream_output` (pointer hidden), always on and
  only updated on damage.
- A popup slot shows the imported output texture through `SetOverlayTextureBounds`, cropped to
  the popup's rectangle. There's no per-popup stream or import, so popups appear on the next
  frame.
- This also covers X11 override-redirect menus, and menus that hang past the parent's edge,
  because popups sit in the popup layer above everything.

**Placement**

- Each popup is placed with `SetOverlayTransformOverlayRelative` to its parent: offset
  `(popup.cg − parent.cg)/density`, plus 3 mm toward the viewer.
- A popup slot uses Mouse input mapped to the popup's own `cg`. Tooltips have no input.
- The xdg_popup grab dismisses the popup on outside clicks.
- `// ponytail: flat popup on curved parent; project onto the curve if it bothers anyone.`

**Fallback**

- If S0 showed output streams stop updating while the session's own screens were hidden, the plan was
  `stream_window(popup uuid)` per popup instead.
- The overlay would then show on its first frame, and I'd measure the time from click to visible
  menu.

**Depends on S0** confirming that popups show up in `windowAdded` with their geometry.

## 8. Clipboard bridge

- `zwlr_data_control_manager_v1` on the seat. It isn't restricted and needs no focus.
- **When the session's selection changes:**
  1. read `text/plain;charset=utf-8`
  2. convert it to UTF-16
  3. store it in `rdp.rs` `CLIP` with `from = FRAME` (`usize::MAX`)
  4. offer it to every krdp
- **When a krdp announces a clipboard** (`from != FRAME`), cc-panels sets a data-control source
  with the text.
- The `from` field stops echo loops. KWin already keeps Xwayland `:2` in sync.
- It only carries text. This is the one deliberate bridge between the two kinds of panel (R-3).

## 9. Tags and the violet card

- **The card, grab bar, corners, glow and paint order** are the existing `grab::create(name,
  VIOLET, tag)` plus `Grab`. The only grab.rs change is the free-aspect resize for window slots.
- **The comment at grab.rs:84-86** ("popped-out window: Magenta") gets corrected: windows are
  violet, and magenta stays unused.
- **The tag** shows the app's name from the `.desktop` file's `Name=`.
  - The plan rendered it once per app with `panels/assets.py` and cached it in
    `~/.cache/control-center/assets/`. That's `assets.rs` now.
  - A small caption line goes under it only when two open panels share an `app:` key.
  - The live caption also goes on the taskbar chip and shows on hover.

## 10. Taskbar

- **One overlay**, `controlcenter.taskbar`, built from the grab.rs pieces (`draw`/`over`/`mix`/
  `set_raw`, Mouse input, hit-testing on `e.data.mouse`), and added to `kvm.nearest`.
- **Contents:**
  - **Apps**, which opens the launcher (§11)
  - **cyan chips**, one per remote machine (`show_panels`/`spot`)
  - **violet chips**, one per window slot:
    - icon from `Icon=` (a hicolor PNG, otherwise a text tile)
    - a click shows the panel (if needed) and activates the window; a second click hides it
    - a long-press offers Close
    - hidden windows are dimmed, and the `kbd` window's chip is lit
  - **"+N"** for unslotted windows
  - **clock**
- **No new font crate.** The plan rendered each chip with its tag, and the clock glyphs `0-9:`
  once (assets.py and PIL with PROXON then; assets.rs now).
- **Repaints** on hover, on add/remove/caption changes and once a minute.
- **Hides** while a VR game runs (`GetCurrentSceneProcessId() != 0`).

| `"taskbar"` | Behaviour |
|---|---|
| `fixed` (default) | Locked in the world, 0.25 m below the lowest krdp panel at its home spot, centred under their span.<br>Recomputed when a spot is applied or a panel is dropped, never during a drag.<br>Carrying it by an edge saves `spots.home.taskbar`, which wins from then on.<br>With no krdp panels, it takes the `follow` pose once at startup and freezes there. |
| `follow` | 0.7 m out, 0.45 m below eye height, tilted 25° toward you, yaw only.<br>When your head yaw is more than 35° off for over 0.6 s, or you've walked more than 0.5 m, it eases over about 0.4 s (critically damped) to face you again. |
| `wrist` | `SetOverlayTransformTrackedDeviceRelative` on the non-dominant controller (`laser.rs::dominant_hand()`), 0.22 m wide, over the back of the wrist.<br>Fades in when `dot(wrist normal, toward head) > 0.6`. |

- **Reading the setting:** at startup.
- **Switching:** `taskbar fixed|follow|wrist` on `@controlcenter` switches live and writes
  settings.json.
- `// ponytail: no file watch; hand edits apply on restart.`

**Merged taskbar as built (2026-10-02).** This replaces the contents list above, and "our bar
8 px under Plasma's" in the Plasma taskbar note in §16. The details are in
[plasma-look-design.md](plasma-look-design.md) (c).

- **One frame, one overlay of ours.** Plasma's real taskbar sits on top, in a well. Plasma's
  overlay is 2 mm in front and sorts at 191. Under it is our chip row: cyan remote machines,
  violet windows, "+N". The curve knob sits under the chips.
- **Width.** The frame is as wide as the wider of Plasma's fit-content panel and the chips.
- **Units.** It's drawn in Plasma's logical pixels and painted in the panel's colours (theme.rs
  `shell`).
- **Chips** look like Plasma 6's task manager. There's no tile at rest. Hover gives an accent
  tint and outline, and the keyboard's chip gets a stronger tint. An accent indicator line runs
  along the tile's bottom: 40% of its width, the full width for the keyboard's chip, none for a
  hidden panel.
- **Manipulation** works like a panel's:
  - An edge carries it (fixed only).
  - A corner resizes both rows about the centre (`taskbar_scale`).
  - The knob, held, bends it (`taskbar_curve`, 3 cm a joystick notch).
- **Removed:** the grip, size, curve, Apps and clock chips. The clock is Plasma's, and so is
  Kickoff.
- **Unchanged:** fixed, follow and wrist, and every hiding rule.

## 11. Launcher

- **Grid entries** are the `.desktop` files from `$XDG_DATA_DIRS`, minus NoDisplay, Hidden and
  Steam entries, with Exec field codes stripped.
- **A launch** runs this from inside cc-box:

  ```
  XDG_RUNTIME_DIR=/run/user/1000 systemd-run --user --unit=app-cc-<id>-<n> --collect -E <plasmashell env…> <Exec>
  ```

  - It's a transient **service**, so the host's user manager starts it in the host namespace.
  - It leaves out `SteamAppId`/`SteamGameId`.
  - It keeps running after cc-panels exits.
- **For scripting:** `launch <desktop-id>` on `@controlcenter`.
- New windows arrive through `add`, so launches need no special handling.

## 12. Build, install and permission

**Grant**

- **Main path:** `wp_security_context_v1` with `set_sandbox_engine("cc")`,
  `set_app_id("org.controlcenter.panels")` and `commit`, then connect to that socket.
  - KWin then looks up the grant by desktop id and ignores the exe path.
  - That survives rebuilds while it's running, and avoids the container `/usr` trap.
- **Fallback:** the exe-path match. The same `.desktop` file serves both, so it has to be named
  exactly that.
- **Missing globals.** A refused grant isn't an error: the globals just aren't advertised. So
  log that explicitly.

**install.sh**

- Add `pipewire-devel` to the cc-box package list. It was the only missing package; clang-devel
  and mesa-libgbm-devel were already installed.
- Write `~/.local/share/applications/org.controlcenter.panels.desktop`:
  ```
  [Desktop Entry]
  Type=Application
  Name=Command Center panels
  Exec=$HOME/control-center/target/release/cc-panels
  NoDisplay=true
  X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1,org_kde_kwin_fake_input
  ```
- Run the host's `kbuildsycoca6` with the nested session's environment if a session is running.
- The entry is hidden (`NoDisplay`), but it also enters the normal desktop's menu cache.

**Dependencies** (`crates/cc-panels/Cargo.toml`, all built in cc-box)

- `wayland-client = "0.31"`
- `wayland-protocols-plasma = { version = "0.3", features = ["client"] }`
- `wayland-protocols-wlr = { version = "0.3", features = ["client"] }`
- `wayland-protocols` (`staging`, for security-context)
- `pipewire = "0.9"`. Its API is `MainLoopRc`/`StreamBox`; adapt from the pipewire-rs `streams`
  example.
- `zbus = "5"`

**Binary location**

- It has to live under `/home`, because a binary in the container's `/usr` would resolve to the
  host's `/usr`.

**Focus and edge barrier**

cc-home sets `Windows/FocusPolicy=ClickToFocus`, `EdgeBarrier/EdgeBarrier=0` and
`EdgeBarrier/CornerBarrier=false` in the session's kwinrc when it makes the session, so nothing
has to be backed up or restored around a cc-panels run.

## 13. R-3

The two kinds of panel never mix:

- Input dispatch branches on `src`: `Rdp` goes to `rdp::mouse`/`rdp::key`, `Window` goes to
  `fake_input`.
- Nothing ever moves a window into an Rdp slot, or the reverse.
- cc-view's viewers (class `cc-view-<name>`, app `com.freerdp.FreeRDP`) are never adopted.
- The launcher only targets the nested KWin.
- The clipboard is the one deliberate bridge, and it only carries text.

## 14. The stock desktop

This was the plan on 2026-10-01 for running on SteamOS's stock nested desktop (S6). It isn't
today's session (see the top of this doc), but it explains why the plan was shaped the way it was.

| | Stock `steamos-nested-desktop` (S6) |
|---|---|
| Discovery | `…/nested_plasma`, private bus |
| PipeWire, grant | One shared daemon; `~/.local/share/applications` |
| Outputs | **One 1280×800 output**: every window is capped there, and the raise gate is essential. No park output, so the pointer parks on Plasma's panel. Bigger later: `stream_virtual_output` or a larger `--width/--height` (open). |
| FocusPolicy / EdgeBarrier | First try `options.focusPolicy` in memory from a script; if that can't, edit `~/.config/kwinrc` while cc-panels runs, backed up and restored on exit (my decision). |
| Hiding the original | gamescope shows the desktop as one window; hiding it is open |

## 15. Open risks (live only)

1. **Freezing while hidden.** Do windows (and output streams) keep updating while the
   session's own screens are hidden? Hiding them might stop the frame callbacks to the nested outputs.
2. **Buffer acceptance.** KWin's `testCreateDmaBuf` and SteamVR's `ImportDmabuf` with KWin's
   LINEAR buffers (stride).
3. **Tearing.** Implicit-sync tearing on zink and Turnip. The `poll` fence should cover it.
4. **Popups in `windowAdded`.** Whether popups, including X11 override-redirect ones, show up
   there with their geometry.
5. **Sycoca refresh.** Whether the grant is picked up without restarting KWin. Security context
   still needs the KService cache.
6. **Raise latency.** The `raised` round trip under load. If presses get dropped often, raise
   the 3-tick budget.
7. **Window behaviour.** CSD and GTK windows with `place`; single-instance apps already running
   in another session.
8. **Stock desktop.** The output size; hiding gamescope's desktop window.

## 16. Staged plan, with a live check for each stage

### S0: spike (prepared beforehand, run in 30 minutes or less)

**Code.** I wrote `session.rs` and `capture.rs` in their final form first. `src/bin/cc-win-spike.rs`
pulls them in with `#[path]` and, given `<uuid> <cg_x> <cg_y>`:

- prints the globals and their versions, and which grant path succeeded
- calls `stream_window(uuid, 1)` and prints `node N`
- negotiates and prints `format WxH mod 0x… dmabuf|memfd`
- imports and prints `import ok|fail`
- prints `first frame +Nms` (from `stream_window` to the first imported frame)
- creates a 1.0 m overlay, `controlcenter.spike`, 1.2 m ahead, with Mouse input
- on a click: `authenticate`, one `pointer_motion_absolute`, then `button`, and prints
  `click x,y`
- with `--output`, also streams WL-2 and prints `output frames/s`
- with `--watch`, logs every `windowAdded` with its type and geometry (the script calls
  `callDBus` back to the spike)

**Running it.** It ran in a host terminal with SteamVR running and cc-panels not running:

1. Take the nested session's environment from its plasmashell's `/proc/<pid>/environ`.
2. Build `cc-win-spike` in the container and install the grant
   (`org.controlcenter.panels.desktop`, `Exec=` pointing at the spike), then `kbuildsycoca6`.
3. Open a Konsole and get its UUID from KWin's `/WindowsRunner`.
4. Make it borderless at WL-2's top-left corner with a one-off KWin script, which doubles as the
   edge-barrier test.
5. Hide the session's own screens, as the real run would.
6. Run the spike on the UUID with `--output --watch`.
7. For the edge-barrier A/B, back up kwinrc, set `EdgeBarrier=0` and `CornerBarrier=false`,
   `qdbus6 org.kde.KWin /KWin reconfigure`, and click again.

**In the headset**

1. Run `top` in Konsole before step 5, then watch the panel.
2. Click Konsole's **New Tab** button, which is within 50 px of the WL-2 corner. Do it once before
   step 7 and once after.
3. Open Konsole's **File** menu by laser and hover over a toolbar button.
4. Leave a second Konsole idle, then start a spike on it (step 6 with its UUID).

**Pass**

- `top` keeps updating at its normal rate while the screens are hidden.
- The log shows `dmabuf mod 0x0` and `import ok`. memfd with a working picture is a **soft
  pass**, and S1 then uses the copy path.
- `first frame` is logged. That decides whether per-popup streams work as the fallback.
- After step 7 the click lands and a tab bar appears. Before step 7 it most likely falls short;
  log the result either way.
- `--watch` logs the menu and tooltip, with their geometry, and `output frames/s` is above 0
  while the screens are hidden. Together these choose the popup path.
- The idle Konsole shows its contents within 1 s, without a nudge. If not, enable the nudge.

**If it fails**

- No screencast global: run `kbuildsycoca6` in a shell with the nested environment, and retry.
  If it still fails, restarting the nested session decides it.
- Frozen while hidden: S1 has to keep one host frame callback alive (open risk 1).
- Tearing: check the `poll` fence is in place.

**Cleanup:** restore the kwinrc backup and reconfigure, show the screens again, remove the
spike script, and close both Konsoles. Keep the `.desktop` file; install.sh rewrites its `Exec=`
later.

### S1 to S6

| Stage | Work | Live check |
|---|---|---|
| **S1** Display | `Source` enum, fixed pool, unslotted list<br>`Panel.size` atomics, `session.rs`, `capture.rs`, `kwin.rs`<br>`cc-windows.js` with adopt, save, `place`, `noBorder` and `restore`<br>exit restore and `restore.js`, the grace map and `win-poses.json`<br>`windows` command, violet card, xfreerdp excluded, grab.rs comment fixed | 3 Konsoles and Dolphin give 4 violet panels within 1 s. Closing them removes the panels. A minimized window and one on another desktop both show up. An idle Dolphin shows at once. `windows` matches. Krdp panels are unchanged (R-3). A resize from inside the session refits the panel with no stale frames. **Exit:** every window is back where it was, with its border and state. **`kill -9`, then `cc-panels stop`:** same result. Restart: panels return to their exact poses. |
| **S2** Input | `fake_input`, raise gate with `pending_press`, hover raise<br>key tracking, repeat drop, pointer park on WL-3<br>wrapper's FocusPolicy and EdgeBarrier backup and restore | Two overlapping Konsoles: click B's toolbar while A is on top, and it lands in B. Type into A while hovering B, and the keys go to A. Click a krdp panel, then type, and the keys go to the remote (R-3). Wheel scrolls; Ctrl+C works; a click 50 px from an output edge lands. In an X11 app, holding a key repeats at the normal rate. kxkbrc matches the krdp layout. Leaving all panels clears hover. **Exit:** kwinrc is back to its original values. |
| **S3a** Resize | Free-aspect resize, grip zoom, card Close, assets.py tags with a caption line for duplicates | Dragging a corner reflows Konsole's columns and the text stays sharp. Grip-drag zooms, and the zoom persists. Close on the card closes the window, and the save prompt appears as a dialog panel. |
| **S3b** Popups | Output-crop popups (or per-popup streams, if S0 chose that) | The File menu appears at the right spot on the next frame (log the time from click to visible). Clicking an item works. A tooltip appears over a toolbar button. A click outside dismisses the menu. |
| **S3c** Dialogs | Dialog placement rule | Settings opens as its own violet panel in front of its parent. |
| **S4** Taskbar | `taskbar.rs`: chips, "+N", dimmed hidden chips, clock<br>the three placements, `taskbar` and `summon` commands | `taskbar fixed/follow/wrist` switches live and updates settings.json. `fixed` with no krdp panels falls back as described. Chips track windows, and the `kbd` chip is lit. Hide and show through a chip. `summon` brings back every panel. The bar hides during a VR game. A 33rd window shows "+1". |
| **S5** Clipboard + launcher | data-control bridge, Apps grid, `launch` via `systemd-run` | Copy in Konsole and paste on a remote PC, and the reverse, with one log line per copy. Launch Dolphin from the grid and a panel appears. Kill cc-panels: Dolphin keeps running and comes back on restart. |
| **S6** Stock desktop | `nested_plasma` discovery, 1280×800 cap<br>in-memory FocusPolicy test (else the backed-up kwinrc edit), park point, hiding gamescope's desktop | The S1 to S5 checks on the stock desktop. `~/.config/kwinrc` is back to its original values after exit. The grant isn't visible in the KDE menu. Anything still open goes in the docs. |

**S1 as built (2026-10-01).** `windows.rs` (slots, streams, sizes, poses) and `kwin.rs` (bus,
script, restore). Where it differs from the plan:

- **16 window slots, not 32.** SteamVR caps overlays at 128 (`k_unMaxOverlayCount`) and a slot
  took 4 then. Popup slots wait for S3b.
- A corner drag keeps the panel's aspect (free aspect is S3a) and resizes the window to match.
- The exit restore is a one-shot `panels/cc-restore.js` that cc-panels loads itself (on every
  exit, and on the next start after a crash), or the wrapper does (`cc-panels --restore`). It's
  not a message to the live script.
- The tag is the app's `.desktop` Name (else the last part of its id), drawn by assets.py at the
  time (assets.rs now).

**S2 as built (2026-10-02).** Input lives in `windows.rs` (`mouse`, `key`, `wheel`, `leave`,
one shared `Input` behind a mutex, since the input loop and the lasers both drive it).
`Panel::mouse`/`key`/`wheel` in main.rs branch on `src` (R-3), and kvm.rs now tracks held keys as
evdev codes. Where it differs from the plan:

- **One pending press, not one per slot**, since there's only one session pointer.
- A press waits **100 ms, not 33**, for its raise (`CC_RAISE_MS`), because the script's long poll
  alone answers in 20 ms steps. A dropped press is only logged; there's no press glow yet.
- `last_raised` survives an `activated` of that same window. ClickToFocus raises what it
  activates, so otherwise every second click would wait.
- Each raise carries a sequence number that the script echoes. Queueing a raise clears
  `last_raised`, and only the latest raise's answer counts (a new window, a removal or another
  window's activation voids raises in flight). That way a quick A→B→A pointer sweep can't let a
  press into A while `raise B` is still ahead of it.
- A laser click on a window panel also moves typing there (`kvm.type_to`; krdp panels are
  unchanged). Wheel: 15 units a notch (`CC_WHEEL`).
- The pointer parks on WL-3 when the mouse is off every panel or on a krdp one, or the laser
  leaves a window panel (FocusLeave). It never parks while a button is held there.
- A freed slot that had the keyboard lets its held keys up (Ctrl after Ctrl+W) and keeps typing
  on the session (whose focus moved on), never on a krdp panel nobody clicked.
- The wrapper kept the three kwinrc values in `~/.cache/control-center/kwinrc-keys.bak`
  (`<unset>` for a missing key; S0's `kwinrc.bak` was a whole-file copy), kept an existing backup
  (a crash's) as the original, and read the session's `XDG_CONFIG_HOME` and bus from its
  plasmashell. (cc-home sets these values in the session's own config now.)

**S4 as built (2026-10-02).** `taskbar.rs`, one overlay.

- Chip labels are assets.py's tags (a window's app name; a remote machine's viewer name,
  `tag-chip-<name>`), and the clock and "+N" use its new `glyphs.rgba` (`0-9:+`, fixed cells).
  No icons, captions on hover or long-press Close yet.
- **Minimize** is a third window control (v ^ x) and the chip's click. `Panel::away()`
  (minimized, or hidden for another panel's theater) hides picture and card through grab.rs's
  theater path, so leaving theater never brings a minimized panel back, and kvm can't land on
  it. The stream keeps running while hidden, so it comes back at once; closing it per §3 waits
  until the cost is measured.
- Showing a panel from its chip ends any theater, moves typing to it and, for a window, sends
  `activate` (`workspace.activeWindow`).
- The **mouse** reaches the bar through `kvm.bar`. The cursor floats on it (unless a nearer panel
  covers it: the Frame draws by depth, not sort order), and its left clicks go straight to the
  taskbar, not through SteamVR's laser. With gaze lock on, looking at the bar lets the gaze's
  panel go.
- In theater mode, **follow** and **wrist** show (they're nearer than the 2 m backdrop), and
  **fixed** hides, since it usually sits behind the backdrop.
- The wrist bar is 0.22 m wide but never under 2.5 cm high (`WRIST_H`): when it's crowded, it
  widens.
- **fixed** uses the krdp panels' live places, recomputed at startup, on `place` of a krdp panel
  (a spot applied) and when one is dropped. `place taskbar ...` (cc-home apply) places it, and
  its grip carries it and saves `spots.home.taskbar`.
- **follow** sits 0.7 m out and 0.45 m under the gaze (yaw and pitch), retargeting past 3° or
  2 cm and easing after it.
- **wrist** sits like a watch along the off hand's forearm (the hand that isn't pointing), facing
  out of the back of the hand. `taskbar_wrist` [x, y, z, tilt°] in settings.json is the
  calibration knob (x is mirrored on the right hand).
- `summon` un-minimizes everything, ends theater and un-hides. It doesn't fan panels out.
- A VR game is detected with `IVRApplications::GetCurrentSceneProcessId` (this header has it
  there, not on IVRCompositor).

**Plasma taskbar as built (2026-10-02).** The session's real KDE panel (plasmashell's floating
dock on WL-0, 891x50, centred at the bottom) is one more overlay, `controlcenter.plasma`
(plasmabar.rs). Our chip bar stays and sits under it.

*Pixels.* Two streams feed the one overlay:

- The dock's own `stream_window`, while there's only the bar to show. Its frames only come when
  the panel changes.
- A `stream_output` of the panel's output (the S0 path), for the first frame (an idle window
  sends none) and while a popup is open. It copies every repaint of WL-0, where most windows and
  theater's video sit, so it only runs when it has to. It's shown through
  `SetOverlayTextureBounds`, cropped to the dock window's frameGeometry.

The overlay switches to the window's stream on that stream's next frame after the popup closes,
and then the output's stream closes. A stream that fails or closes is retried after 2 s (the
output's) or 30 s (the window's; until then the output's carries it). While plasmashell has other
windows open on that output (Kickoff, the tray, task previews, the calendar, menus, tooltips),
the crop is the union of the panel and them, clipped to the output, and the overlay moves by the
crop's middle minus the panel's. That keeps the bar where it is while the overlay grows (upward,
usually). Only new frames go up (`capture::Shown`). The stream runs while the bar is shown and
closes 3 s after it hides, so a wrist glance doesn't reopen it.

*Geometry.* cc-windows.js reports plasmashell's dock as `shell` role `panel`, and its other
windows (not its desktops, notifications or OSDs) as role `popup`, each with `shown` and
frameGeometry on add, geometry change, `hiddenChanged` and removal. They're never adopted.

*Placement.* The settings.json `taskbar` mode's pose (taskbar.rs, same math) is now the anchor.
Plasma's bar goes there and our bar 8 px (at its scale) under it in the bar's own frame, so the
two move together in fixed, follow and wrist. Plasma's pixels are our bar's size (its metres over
its 80 px), its sort order is the taskbar's 190, and it shows and hides with our bar (VR game,
every panel hidden, fixed in theater, the wrist's fade).

*Input.* Its mouse scale is 1x1, so laser events are fractions of the overlay, mapped through the
current crop to the session's global pixels (`plasmabar::at`). `windows::shell_mouse` sends
fake_input motion and buttons with no raise gate.

- A laser press on it is always released, even if the bar hides first.
- A mouse button held there keeps the cursor on it, clamped to its edges, the same as for
  panels. The mouse cursor floats on it like on our bar (`kvm.plasma`, and nearer panels cover
  it), and its moves, clicks and wheel go to the session there.
- A click on it sends typing to the session's focused window (`kvm.kbd_shell`: Kickoff's search).
- While a popup is open, a press anywhere else clicks the park output (WL-3's desktop) so Plasma
  closes it: on a krdp panel, a card, our bar, or a window panel. A window panel needs it too,
  because its window is usually on WL-0 under the popup and a raise can't lift it past Plasma's
  layer.
- On a window panel, a point under the panel or an open popup (`plasmabar::covers`) gets no
  pointer motion, and a press there is dropped.

*Minimize* (this replaces §6's "never minimized"):

- v and a chip minimize the KWin window too (`minimize`), a chip brings it back (`activate`
  un-minimizes), and `summon` sends `unminimize`.
- The script reports `minimizedChanged` (it no longer forces it back), and minimized windows are
  adopted hidden.
- A minimized panel's stream is closed, since it would go blank.
- The panel only comes back on KWin's un-minimize event, never ahead of it. A chip, `summon` or
  the app's own full screen ask KWin, and only a window KWin wouldn't minimize is shown directly.
  Then a new stream opens, and the panel shows on its first frame where it was. Another panel's
  theater ends, as with a chip.
- cc-restore no longer touches minimized: what the user minimized stays minimized on exit.
- Overlays: +1.

Still open (needs a live run): popups as KWin script windows with geometry (risk 4); the bounds'
v direction, and SteamVR taking the overlay's aspect and mouse from the bounds; the output's
first frame without damage; the park click closing Kickoff. Also whether KWin streams the dock (a
layer-shell window) by its internalId, and whether that stream matches the output's crop (its
size, a floating panel's margins, translucency under IgnoreTextureAlpha).

## 17. Questions for you

1. **Zoom gesture.** Grip-drag on a corner rescales a window panel (bigger text) instead of
   reflowing it. Is grip the right modifier, or would you prefer Right Ctrl?
2. **WL-3 as the pointer park.** I decided this by default (not asked; to revisit if three outputs
   prove too few): yes.
3. **Stock desktop focus.** If a KWin script can't change the focus policy in memory, should
   cc-panels edit your real `~/.config/kwinrc`, with a backup and restore, or should the stock
   desktop live with focus-follows-mouse quirks?
