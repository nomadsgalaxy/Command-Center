# Command Center: architecture map

This is the map I keep so nobody (me included) has to re-read the whole tree before a change.
Read it first, then the one module you need, then the code. Every module starts with a `//!`
header that says what it owns, and those headers are kept up to date, so they're the next stop
after this file.

Anchors are `file:function`, not line numbers, so they survive edits. Paths are from the repo
root. When this file and the code disagree, the code wins, and this file should get fixed.

## What runs where

```
 Steam Frame (SteamOS, aarch64)                      Each host (Linux + KDE Plasma on Wayland)
 ─────────────────────────────                       ──────────────────────────────────────────
 SteamVR ── cc_pointer driver (.so in vrserver)       cc-host serve   agent, TLS on 3399
   │            ▲ lease datagrams @cc_pointer           │  starts ──► krdpserver (patched krdp)
 cc-panels ─────┘  (the VR Desktop, in the container)   │             RDP on 3400 + 10·slot + monitor
   │  RDP/VNC ───────────────────────────────────────►  │  tag screens (align), pairing key screen
   │  agent calls (cc_proto::agent) ─────────────────►  cc-host guard / announce (mDNS) / units
   │  KWin script + screencast + fake_input
 cc-desktop  headless KWin + Plasma (systemd user unit)
 cc-home     CLI + launcher glue (static musl, on the SteamOS host side)
```

### The Frame

- **cc-panels** (`crates/cc-panels`): the Desktop. Every remote monitor (RDP through FreeRDP,
  or VNC through libvncclient) and every window of the Frame's own Plasma session is a SteamVR
  overlay panel, with the taskbar, our own mouse/keyboard (a software KVM), the laser, the
  clipboard hub and the Machines/Preferences/Workspace windows. It runs inside the
  `control-center` distrobox container, because that's where FreeRDP, FFmpeg (H.264), GBM and
  PipeWire live.
- **cc-home** (`crates/cc-home`): one static aarch64-musl binary that's both the CLI (spots,
  workspaces, machines, pairing, align, hibernate) and the launch glue. `cc-box`, `cc-panels`
  (repo root) and `session/cc-launch`, `cc-desktop`, `cc-rest` are symlinks to it, and it
  dispatches on argv[0] (`crates/cc-home/src/session.rs:dispatch`). It runs on the SteamOS host,
  not in the container, because it needs systemd, podman, nmcli, the camera and avahi.
- **cc-pointer** (`crates/cc-pointer`, packaged by `driver/cc_pointer/`): a SteamVR driver that
  adds an invisible controller. cc-panels steers it along the mouse ray so SteamVR's own laser can
  reach UI we don't own (the dashboard, Steam's menus). All its logic lives in cc-panels.
- **The headless session** (`cc-desktop`): Plasma in a headless KWin, started by
  `cc-home session` as its own systemd user unit so it outlives cc-panels. It has its own
  `XDG_CONFIG_HOME` (`~/.config/control-center/desktop`) and runtime dir
  (`$XDG_RUNTIME_DIR/cc-desktop`), so it never touches the Frame's own Plasma config.

### Hosts

- **cc-host** (`crates/cc-host`): one static musl binary (x86_64 or aarch64). `cc-host serve` is
  the agent on 3399 that paired Frames talk to. It also shows the pairing key and the align's tag
  screens, and as `cc-share` (a link to it) it installs units, the firewall rule, the guard and
  mDNS announcing.
- **krdp** (`krdp/*.patch`): KDE's RDP server with four patches of mine (clipboard, pointer
  offset, a window-stream mode, and audio: `src/Audio.cpp`, the host's sound out and the Frame's
  microphone in, docs/audio.md). The Arch package (`packaging/arch/PKGBUILD`) is the one place
  for its version and patches. `krdp/build-steamos.sh` builds it for a SteamOS host.
- **cc-scan** (`crates/cc-scan`): not a host program. It's the camera-scan library cc-home uses
  on the Frame (mirror camera, ArUco detection, tag layouts, the fit). It sits here because
  hosts draw the tags it reads.

### Shared

- **cc-proto** (`crates/cc-proto`): the wire protocol, both halves: the agent client and server
  login, SPAKE2 pairing, and the Frame's config files (`conf.rs`: viewers.conf, trusted-hosts,
  home.json, workspaces). cc-panels, cc-home and cc-host all link it.
- **cc-install** (`crates/cc-install`): the terminal installer the `install` script downloads.
- **Bindings**: `openvr-sys` (OpenVR C API, pinned header), `freerdp-sys` (FreeRDP 3 from
  `panels/third_party/prefix`), `vncclient-sys` (static libvncclient from
  `panels/third_party/vnc-prefix`). `cc-hello` is a 35-line OpenVR smoke test.

## Per crate

### cc-panels (`crates/cc-panels/src`)

| File | What it owns |
| --- | --- |
| `main.rs` | `Panel` (one per overlay: remotes, spares, window slots), start-up (`run`), the main loop and its pacing (`Pace`), routing input to RDP/VNC/window (`Panel::mouse/key/wheel`), overlay laser events (`laser`), `close_desktop`, `connect`/`disconnect` of a remote. |
| `vr.rs` | OpenVR's C API tables, the `call!` macro, overlay helpers, head pose, the main loop's wake. |
| `geometry.rs` | Poses and placements in standing space (x right, y up, -z forward), matrices, curved hits. |
| `grab.rs` | Each panel's card (Breeze-like frame, tabs, grab bar, knobs): drawing, hit-testing, carrying, resizing, curving, snap-back, the `Extra` slot the control windows share, saving the "home" spot on release. |
| `back.rs` | Dim backs for panels seen from behind, one flat overlay each. |
| `kvm.rs` | Our mice and keyboards (evdev grab, `input_loop` thread `cc-input`): the 3D pointer ray (`mv`, `land`), click to type (`set_engaged`, `type_to`), Right Ctrl chords, gaze lock, text fields for the control windows. |
| `laser.rs` | The SteamVR laser off our panels: which hand to lease, raw-space poses, the lease protocol to cc_pointer (`lease_thread`, thread `cc-lease`), `Beam` for the dot. |
| `gaze.rs` | Eye tracking through the eyetracking action (which panel you look at). |
| `attention.rs` | Per-panel stream rates: full where you look, a few fps in view, paused when hidden, minimized, during a game or with the headset off (D-042). |
| `gpu.rs` | GBM buffers imported into SteamVR (`Buffers::present`), `write`/`write_frame` from decode threads, `show_raw` (two buffers, no blink) for UI overlays. |
| `rdp.rs` | One FreeRDP session per remote panel (`run`, reconnect loop), `session` (asks the agent for this Frame's server), `agent` (one agent call), input out (`mouse`, `key`, `wheel`), cliprdr attach. |
| `vnc.rs` | One libvncclient session per `proto=vnc` panel, same panel/GPU/wake model as rdp.rs (`run`, `connect`, keysyms, pinning). |
| `popout.rs` | Remote windows popped out into a spare panel (`pop`). Parked (docs/remote-windows.md). |
| `session.rs` | The cc-desktop session over Wayland: finding it (`discover`), the screencast (`stream_window`, `stream_output`), fake_input (`pointer_to`, `button`, `key`, `axis`). |
| `capture.rs` | One PipeWire thread for every screencast stream, DMA-BUF import on the main thread (`Shown::tick`). |
| `kwin.rs` | The session's KWin over its D-Bus: loads `panels/cc-windows.js`, owns `org.controlcenter.Panels` (`Event`, `Next` long-poll), `restore` on exit with `panels/cc-restore.js`. |
| `windows.rs` | Frame windows as panels: the 16-slot pool, window events (`event`), `adopt`, popups on their parent's panel (`place_popup`), minimize, theater mode, sizes from `window_px_per_m`, input into the session, pose memory (`win-poses.json`). |
| `plasmabar.rs` | Plasma's real dock and its popups as one overlay (`controlcenter.plasma`), cropped from an output stream, with laser/mouse hits. |
| `taskbar.rs` | Our taskbar frame and chips (machines, Workspace, gear, power), Fixed/Follow/Wrist placement, carrying it. |
| `control.rs` | The control socket `@controlcenter` (`open`, `handle`, `command`). Its `//!` header is the command reference. |
| `control/machines.rs` | The Machines window: rows per machine, Add machine form, discover, pair (in-process, cc_proto), align (runs `cc-home machine align` on the host), reanchor offer. |
| `control/prefs.rs` | The Preferences window: settings.json's user settings, applied on click. |
| `control/workspace.rs` | The Workspace window: rows per workspace, Use/Load/Save as/Rename/Forget, machines per workspace; `switched` reconnects to match. |
| `control/ui.rs` | The drawing kit those windows share (chrome, buttons, switches, typed text), window placement. |
| `control/hud.rs` | The camera scan's status strip and tag outlines in the headset. |
| `theme.rs` | Reads the session's Plasma colour scheme (KConfig cascade) into the tokens everything paints with; `poll` redraws on a theme switch. |
| `assets.rs` | Draws the cursor, border tags, glyphs and UI labels into `~/.cache/control-center/assets`. |
| `config.rs` | The Frame's files as cc-panels sees them: `Viewer` and viewers.conf parsing, home.json spots, workspaces, settings, passwords, pins, autoconnect. |
| `clipboard/mod.rs` | The clipboard hub (`HUB`): who owns the copy, echo suppression, lazy fetch on paste. |
| `clipboard/cliprdr.rs` | The machines' side: FreeRDP's cliprdr channel per RDP session, file transfer staging. |
| `clipboard/frame.rs` | The Frame session's side: wlr-data-control on its own Wayland connection. |
| `clipboard/formats.rs` | Format conversions between the session's MIME types and RDP's. |
| `bin/cc-win-spike.rs` | Stage 0 of window panels, kept as a spike. Not part of the Desktop. |

### cc-home (`crates/cc-home/src`)

| File | What it owns |
| --- | --- |
| `main.rs` | `USAGE` (the CLI reference), argv[0] dispatch into session.rs, spots (`save`, `apply`, `reanchor`), `workspace`, `autoconnect`/`write_autoconnect`, `network`, the control-socket client (`Panels`, `CC_PANELS_SOCKET`). |
| `session.rs` | Launch glue: `desktop` (cc-launch), `panels` (the cc-panels wrapper), `rest_now` (cc-rest), `session` (cc-desktop), `boxed` (cc-box), the `kwin_wayland_wrapper` and `flatpak-bwrap` shims, `CC_DRY=1`. |
| `hibernate.rs` | Saves which apps were open at a deliberate close and reopens them next launch. |
| `install.rs` | `cc-home install` (FreeRDP, libvncclient, cc-panels, the pointer driver, `~/.local/bin` links, the KWin grant `.desktop`), `install desktop`, `install remove`. |
| `machine.rs` | `cc-home machine ...`: viewers.conf edits, discover (mDNS), `pair`, unpair, session/window commands through the agent. |
| `scan.rs` | Everything with the camera: `scan` (align), `refit`, `calibrate`, `pair_scan`. |
| `tests/cross.rs` | Byte-for-byte checks against the old Python's recorded output. |

### cc-host (`crates/cc-host/src`)

| File | What it owns |
| --- | --- |
| `main.rs` | Subcommand dispatch (`serve`, `pair`, `tagscreen`, `cert`, `check`, `units`, cc-share's commands). Config dir is `~/.config/control-center` or `$CC_CONF`. |
| `agent.rs` | The agent: the two doors on 3399 (`{` = pairing, `0x16` = TLS), host key/id/cert, login, JSON-lines `command`. |
| `work.rs` | What Frames ask for: monitor sessions (`session_start/stop`, idle stop, adoption), window streams, tag screens (`tags`, `check_tags`). |
| `imu.rs` | A Steam Deck's motion sensors through hidraw: finds the controller, turns the IMU on and puts the setting back, decodes the reports. `imu-probe` is its local check. The agent's `imu` command (work.rs) streams it (docs/agent.md 3c, docs/deck-tracking.md). |
| `pair.rs` | `cc-host pair`: the 6-digit key (and the address tags beside it), slots (max 4 Frames), lockout, `reply`. |
| `share.rs` | Everything cc-share was: `install`, `up`/`down`, `check`, `guard`, `announce`, firewall, `frames`, `unpair`, `uninstall`, units. |
| `platform.rs` | The per-platform layer (`Linux` = KDE + systemd user units, `Fake` for tests). |
| `draw.rs`, `aruco.rs`, `screen.rs` | Tag/key screens drawn into a buffer, the ArUco bit table, and the pure-Rust Wayland layer-shell that shows them. |
| `hostcert.rs` | krdp's fixed TLS certificate and `check --agent`. |

### cc-proto, cc-scan, cc-install, cc-pointer

- `cc-proto/src/agent.rs`: the agent client (TLS 1.3, Ed25519 pin, `call_once`, `Client`),
  Frame key, `trusted`. `server.rs`: the host half of the login. `pair.rs`: SPAKE2 pairing
  (`frame_pair`, `host_one`). `lan.rs`: the host's route-source address and the private-address check. `conf.rs`: viewers.conf editing, `write_pairing`, unpairing,
  home.json (`read_home`, `write_home`), workspaces (`choose_workspace`, `load_workspace`,
  `TEMPORARY`), a Python-compatible `Json`.
- `cc-scan/src`: `camera.rs` (/dev/video99, the VR mirror), `aruco.rs` + `contours.rs` +
  `image.rs` + `dict.rs` (OpenCV's detector, ported pixel-exact), `pattern.rs` (tag layouts),
  `solve.rs` (camera intrinsics and monitor poses), `lag.rs`, `hud.rs` (drawn here, shown by
  cc-panels), `panels.rs` (control-socket client), `scan.rs` (a scan session for cc-home).
- `cc-install/src`: `main.rs` (flags, running steps), `plan.rs` (`look` gathers facts, the rest
  is pure data), `ui.rs` (the terminal screens).
- `cc-pointer/src/lib.rs`: the driver itself (`HmdDriverFactory`, vtables by hand, the
  `@cc_pointer` lease socket, a 300 ms watchdog). `driver/cc_pointer/` has `build.sh`,
  `install.sh` (vrpathreg, hashed folders) and the driver's resources.

## Key flows

### Desktop launch, close, rest and hibernate

1. The SteamVR launcher's "Desktop" entry runs `session/cc-launch`
   (`~/.local/share/applications/deckard-nested-desktop.desktop`, written by
   `install.rs:desktop`). That's `session.rs:desktop`.
2. It waits out a cc-desktop that's still stopping, then starts the session unit if it's down
   (`session_unit` → `systemd-run` → `cc-home session` → `session.rs:session_body`: runtime dir,
   KWin config, the `kwin_wayland_wrapper` shim for headless outputs, `dbus-run-session
   startplasma-wayland`).
3. It waits up to 60 s for plasmashell (`plasma_env`), runs `cc-home hibernate restore`, then
   `cc-home autoconnect --write` (needs nmcli, so it's host-side).
4. It runs `<root>/cc-panels --for 0` (`session.rs:panels`): through `cc-box` with `CC_NICE=0`
   into the container, logging to `~/.cache/control-center/cc-panels.log`, refreshing
   `desktop-session.env` every 5 s (the container can't read other processes' environ).
5. `cc-panels` (`main.rs:run`) takes the `cc-panels.lock` flock, draws assets, inits SteamVR and
   GBM, picks the workspace for the tracked universe (`config::choose_workspace`), makes panels
   (remotes, spares, 16 window slots), starts the clipboard, connects autoconnect remotes a
   second apart, starts `windows`, the input and lease threads and the control socket, then loops.
6. A deliberate close (power chip, Right Ctrl + Esc, `quit`, SteamVR's Quit for a game) calls
   `main.rs:close_desktop`, which writes `~/.cache/control-center/desktop-closed`. A signal quits
   without it.
7. Back in `session.rs:desktop`, if `desktop-closed` exists it runs `rest_now`: `hibernate save`,
   `systemctl --user stop cc-desktop`, and `rest_nice` programs to nice 10. A crash or signal
   keeps the session up, and the wrapper's `finish` puts KWin windows back (`cc-panels --restore`).

### A frame from a remote PC to a panel

RDP:
1. `main.rs:connect` shows the overlay and spawns `rdp::run` for the panel.
2. `rdp.rs:session` asks the host's agent for this Frame's server (`session start`), unless
   it's slot 0 (`port < 3410`) or unpaired. The host starts `krdpserver` on
   `3400 + 10·slot + monitor` (`cc-host work.rs:session_start`) and answers with the port.
3. FreeRDP connects with the pinned certificate (trusted-hosts), decodes H.264 with FFmpeg into
   its GDI buffer, and `on_end_paint` marks the panel dirty.
4. The RDP thread calls `gpu::write` when the panel is due (attention level, `gpu::wait`).
   Hidden or away panels skip the copy and stay dirty.
5. The main loop's `Buffers::present` shows the finished GBM buffer through its imported shared
   texture. It never waits on the RDP thread.

VNC is the same shape: `vnc.rs:run` → libvncclient update callbacks → `gpu::write_frame`.
`viewers.conf`'s `proto=vnc` picks it (docs/vnc.md).

### Mouse, laser and keyboard to a panel

1. `kvm.rs:input_loop` (thread `cc-input`) grabs our mice/keyboards from evdev. Motion turns the
   ray (`mv`), and `land` finds what it hits every frame: a panel, its card, an `Extra` window,
   Plasma's bar, or nothing.
2. Mouse ray on our panel: buttons and motion go out through `Panel::mouse` → `rdp::mouse`,
   `vnc::mouse` or `windows::mouse` (fake_input via `session.rs`). A remote panel only ever
   reaches its own machine (R-3).
3. Mouse ray off our panels: `laser.rs` leases cc_pointer on the free hand
   (`lease_thread`, every 20 ms to `@cc_pointer`), so SteamVR's laser works the dashboard and
   other overlays. `main.rs:update_pointer` keeps the cursor and the `Beam`.
4. Controller lasers on our panels arrive as overlay mouse events: `main.rs:laser` drains
   `PollNextOverlayEvent`, gives drags to `grab.rs:panel_event`, and sends the rest through
   `Panel::mouse`. A controller press takes the pointer from the mouse (`controller_pressed`).
5. Click to type: a click on a panel engages the keyboards (`kvm.rs:set_engaged`, `type_to`). A
   click off our panels, the dashboard opening, the Desktop hidden or a game gives them back.
   Keys go through `Panel::key` (RDP scancodes, VNC keysyms, or fake_input).

### Window panels

1. `kwin.rs:run` (thread `cc-kwin`) connects to the session's bus, owns
   `org.controlcenter.Panels` and loads `panels/cc-windows.js` into its KWin.
2. The script reports windows with `Event(json)` (`hello`, `add`, `remove`, `ready`, `shell`,
   `saved`, `minimized`, `raised`, ...) and long-polls `Next()` for commands. It saves each window's state first, so
   `kwin.rs:restore` can put it back on exit.
3. `windows.rs:Windows::tick` → `event` → `fill` → `adopt` gives a window a slot and a place
   (its last pose, a hibernated app's pose, its app's spot, or ahead of you).
4. `open` asks KWin's screencast for the window (`session.rs:stream_window`). On
   `StreamEvent::Created(node)` `tick_slot` opens it in `capture.rs`, and each tick imports the
   newest DMA-BUF into the overlay.
5. Popups (menus, drop-downs) get a slot only while their parent has one, placed on the parent's
   panel (`place_popup`). Tooltips are dropped. Plasma's own panel and popups go to
   `plasmabar.rs` as `shell` events instead.

### Pairing (docs/pairing.md)

1. On the host, `cc-share pair` (`cc-host pair.rs:main`) shows a 6-digit key and hands the agent's
   `{` door to its socket. The key screen (`draw.rs:key_screen`) also shows the host's LAN address
   as four tags, one per octet (`cc_proto::lan::route_source`).
2. On the Frame, the Machines window's Pair (`control/machines.rs:pair`, in-process) or
   `cc-home machine pair <addr> <key>` (`machine.rs:pair`) connects to 3399 and runs
   `cc_proto::pair::frame_pair`: SPAKE2 on the key, a transcript, HKDF keys, mutual confirmation,
   sealed exchange of the Frame's Ed25519 key and the host's slot, login and ports.
3. `cc_proto::conf::write_pairing` writes `trusted-hosts/<id>.json`, `passwords/<id>` and the
   viewers.conf lines. The host keeps `trusted-frames/<frame>.pub` and `frames/<frame>.json`.
4. "Pair by looking" runs `cc-home machine pair --scan` on the host side (`scan.rs:pair_scan`),
   which reads the key off the host's screen with the camera. With no address and no single host
   announcing, it reads the address off the screen's tags too (`cc_scan::read_addr`) and refuses
   anything that isn't a private IPv4 address. The Machines window offers it from Add machine,
   found hosts or not.

### Align and scan (docs/apriltag-mapping.md)

1. Machines → Align runs `cc-home machine align <name> --progress` on the SteamOS host
   (`machines.rs:on_host`, distrobox-host-exec when inside the container).
2. `scan.rs:scan` opens the mirror camera (cc-scan `camera.rs`) with the HUD, gets each monitor
   ready (`ready`, agent `tags`), lays out tag ids per monitor (`cc_scan::pattern::frame`) and
   has each host show them (`cc-host work.rs:tags_show`).
3. It hides the panels (`hide` on the control socket), captures while you move your head,
   solves (`cc_scan::solve`), then `fit` places each monitor, saves it in the "scanned" spot
   (the previous one kept as "before-align") and makes it home.
4. `@event` lines feed the Machines window's status. `@reanchor` offers "move everything with
   it" (`cc-home reanchor`). `cc-home refit` re-fits the mirror camera first if needed.

### Workspaces (docs/workspaces.md)

1. At start `config::choose_workspace` → `cc_proto::conf::choose_workspace` picks by the SteamVR
   universe: a dedicated one bound to this room, else the network hint or the active one when
   there's no room, else a new room binds an unbound active one, else Temporary.
2. Each panel's spot comes from the active workspace's `spots.home`. Moving a panel saves there
   (`grab.rs:save` → `Panel::save_spot`).
3. Use/Load/Save as in the Workspace window or `cc-home workspace ...`. Load
   (`conf::load_workspace`) copies a layout into Temporary, moved rigidly in front of you, and
   never changes the saved workspace. `workspace reload` on the socket and
   `control/workspace.rs:switched` reconnect to match its machines.

### The clipboard hub

1. `clipboard::start` runs the Frame side (`frame.rs:thread`, wlr-data-control). Each RDP
   session attaches a cliprdr channel when it connects (`rdp.rs:on_channel_connected` →
   `cliprdr::attach`).
2. A copy anywhere calls `Hub::remote_announce` or `Hub::frame_announce`. The hub announces it
   everywhere else and moves nothing until a paste asks (`Hub::want`). Machine text is fetched
   right away to tell echoes apart, since each monitor is its own krdpserver on one host clipboard.
3. Files from a machine stay a descriptor until pasted, then get staged in
   `~/.cache/control-center/clipboard/`.

### Installer and packaging

- One-liner: `install` (sh) downloads `cc-install` from the release, checks SHA256SUMS, runs it.
  On a Frame, `cc-install` clones/pulls `~/control-center` and runs `install.sh container`,
  `install.sh cc-home`, `cc-home install` and `cc-home install desktop`
  (`crates/cc-install/src/plan.rs`). On a host it downloads `cc-host` and runs `cc-host install`.
- Frame build from source: `install.sh` (container, cc-home with the cc-rust toolchain) then
  `install.rs:build` (FreeRDP 3.31.1 into `panels/third_party/prefix`, `tools/build-libvncclient.sh`
  into `vnc-prefix`, `cargo build --release` in the container, the pointer driver).
- Frame sysext (docs/packaging-frame.md): `packaging/sysext/steamos-buildenv.sh` makes a podman
  image from SteamOS's own `/usr`. `build-native.sh` builds everything in it in place (and writes
  `panels/third_party/prefix/steamos-release`, which makes `cc-box` run natively), and
  `build.sh` packs `/usr/lib/command-center` into a sysext `.raw`.
- Host package (docs/packaging.md): `packaging/arch/PKGBUILD` (`command-center-host`: cc-host +
  patched krdp), built and signed by CI (`.github/workflows/arch.yml`, `ci-build.sh`).
  `.github/workflows/release.yml` builds static musl cc-install and cc-host for both arches on a
  `v*` tag. The signing key's public half is `packaging/command-center.asc`.

## Files on disk

**Frame, `~/.config/control-center/`** (always under `$HOME`, never `XDG_CONFIG_HOME`):

| File | Format / owner |
| --- | --- |
| `viewers.conf` | One remote monitor a line: `<name> <user>@<host>:<port> <screen> <w>x<h> [key=value ...]` (curve, radius, autoconnect, machine, label, proto, pin, tls). Parsed in `cc-panels config.rs:parse_viewers` and `cc_proto::conf::viewers`, edited by `conf::set_options`. `viewers.removed` keeps removed lines. |
| `home.json` | `{"workspace", "workspaces": {name: {spots, universe, primary, known_networks, machines}}}`. Spots map a spot name to `{panel key: pose}`. See `cc_proto::conf::read_home` and docs/workspaces.md. |
| `settings.json` | User settings (taskbar, window_px_per_m, align, rest_nice, session_links, ...): `config::settings`, `control/prefs.rs`. |
| `trusted-hosts/<id>.json`, `passwords/<id>`, `password`, `frame-key` | Pairing results and this Frame's Ed25519 key (`cc_proto::conf`, `agent::frame_key`). `pair-tries.json` limits retries. |
| `mirror-camera.json` | The mirror camera fit (`cc-home refit`). |
| `hibernate-allow` | Apps hibernate may reopen. |
| `desktop/` | The headless session's own `XDG_CONFIG_HOME` (kwinrc, autostart, ...). |

**Frame, `~/.cache/control-center/`**: `cc-panels.log` (rotated), `cc-panels.lock`,
`desktop-closed`, `desktop-session.env`, `autoconnect.txt`, `workspace-hint.txt`, `assets/`
(cursor, tags, glyphs), `win-poses.json`, `kwin-restore.json`, `desktop-hibernate.json*`,
`clipboard/`, `scan/`, `pairscan/`, `install.log`.

**Host, `~/.config/control-center/`**: `host-key`, `host-id`, `host-cert.pem`, `cert.pem` and
`key.pem` (krdp's), `trusted-frames/`, `frames/`, `shared`, `announce`, `agent-locked`,
`settings.json`. Binaries and krdp builds live in `~/.local/share/control-center/` (or
`/usr/lib/command-center` when packaged).

**The control socket**: `@controlcenter`, an abstract datagram socket, replies to the sender.
The command list is the `//!` header of `crates/cc-panels/src/control.rs`. Clients (cc-home,
cc-scan) honour `CC_PANELS_SOCKET=<name>`. cc-panels itself always binds `@controlcenter`.

## Protocols

- **Agent**: TLS 1.3 on 3399, certificate pinned to the host's Ed25519 key from pairing, then the
  Frame signs a single-use challenge, then versioned JSON lines. Spec in docs/agent.md; code in
  `cc_proto::agent`, `cc_proto::server`, `cc-host agent.rs`/`work.rs`; the black-box spec test
  is `crates/cc-host/tests/conformance.rs`.
- **Pairing**: SPAKE2 on the 6-digit key, HKDF keys, mutual confirmation (docs/pairing.md,
  `cc_proto::pair`, `crates/cc-host/tests/pair.rs`).
- **RDP**: FreeRDP client to krdp. Clipboard over cliprdr (`clipboard/cliprdr.rs`,
  `krdp/clipboard.patch`). Slot 0 is the shared login on 3400+m; paired Frames get
  3400 + 10·slot + m (slots 1–4). Sound and microphone over rdpsnd and audin, on one session per
  machine (`config::carries_audio`, `rdp.rs`, `krdp/audio.patch`, docs/audio.md).
- **RFB**: libvncclient to any VNC server, VeNCrypt X509 with a pinned certificate (docs/vnc.md).
- **cc_pointer lease**: text datagrams on `@cc_pointer` (format in `crates/cc-pointer/src/lib.rs`,
  docs/laser-pointer-design.md).
- **KWin script**: D-Bus `Event`/`Next` on `org.controlcenter.Panels` (`panels/cc-windows.js`).

## Where to look

| I want to change... | Start in |
| --- | --- |
| What a control-socket command does, or add one | `cc-panels/src/control.rs:command` (and its `//!` header) |
| The main loop, its rate or start-up order | `cc-panels/src/main.rs:run`, `Pace` |
| How a panel's card, grab bar or resize behaves | `cc-panels/src/grab.rs` (docs/panel-move-design.md) |
| Pointer motion, click to type, Right Ctrl chords | `cc-panels/src/kvm.rs` (`land`, `key`, `set_engaged`) |
| The laser off our panels, hand choice | `cc-panels/src/laser.rs`, `crates/cc-pointer` |
| Frame window panels, popups, theater mode | `cc-panels/src/windows.rs`, `panels/cc-windows.js` |
| Plasma's bar in VR | `cc-panels/src/plasmabar.rs` |
| Our taskbar's chips or placement | `cc-panels/src/taskbar.rs` |
| Frame rates, pausing, power | `cc-panels/src/attention.rs`, `gpu.rs` (docs/efficiency-plan.md) |
| RDP connect/reconnect, sessions per Frame | `cc-panels/src/rdp.rs:run`, `cc-host/src/work.rs` |
| VNC | `cc-panels/src/vnc.rs` (docs/vnc.md) |
| Clipboard | `cc-panels/src/clipboard/` |
| Colours and look | `cc-panels/src/theme.rs`, `grab.rs` painting (docs/plasma-look-design.md) |
| Machines / Preferences / Workspace windows | `cc-panels/src/control/{machines,prefs,workspace}.rs`, `control/ui.rs` |
| viewers.conf, trusted-hosts, home.json formats | `cc-proto/src/conf.rs` (and `cc-panels/src/config.rs`) |
| A `cc-home` command | `cc-home/src/main.rs` (`USAGE`, `main`), then `machine.rs` or `scan.rs` |
| Desktop launch, close, rest, the session | `cc-home/src/session.rs`, `hibernate.rs` |
| Align, refit, the camera | `cc-home/src/scan.rs`, `crates/cc-scan` |
| Pairing | `cc-proto/src/pair.rs`, `cc-host/src/pair.rs`, `cc-home/src/machine.rs:pair` |
| The host agent's commands | `cc-host/src/agent.rs:command`, `work.rs` |
| cc-share (host install, firewall, guard, announce) | `cc-host/src/share.rs` |
| The Frame build steps | `install.sh`, `cc-home/src/install.rs` |
| The installer | `crates/cc-install` |
| Packaging | `packaging/arch/`, `packaging/sysext/`, `krdp/` |
| Measuring CPU/power | `tools/README.md` |

## Other docs

- Host side: [agent.md](agent.md), [pairing.md](pairing.md), [rust-host.md](rust-host.md),
  [privacy.md](privacy.md) (research),
  [rust-rdp-server.md](rust-rdp-server.md) (feasibility only).
- Frame UI: [window-panels-design2.md](window-panels-design2.md) (the window panels as built),
  [plasma-look-design.md](plasma-look-design.md), [panel-move-design.md](panel-move-design.md),
  [laser-pointer-design.md](laser-pointer-design.md), [config-window.md](config-window.md),
  [workspaces.md](workspaces.md).
- Streams: [vnc.md](vnc.md), [efficiency-plan.md](efficiency-plan.md),
  [stutter-plan.md](stutter-plan.md), [remote-windows.md](remote-windows.md) (parked).
- Align: [apriltag-mapping.md](apriltag-mapping.md).
- Packaging and tests: [packaging.md](packaging.md), [packaging-frame.md](packaging-frame.md),
  [final-test-frame.md](final-test-frame.md), [final-test-host.md](final-test-host.md).
- Superseded: [window-panels-taskbar-plan.md](window-panels-taskbar-plan.md) didn't ship.
