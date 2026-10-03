# Design: `cc_pointer`, a SteamVR laser steered by our own ray

The mouse drives our own ray in cc-panels, and on our panels that's all it needs: clicks go straight to RDP. But a lot of what you want to click in VR isn't ours: the SteamVR dashboard, Steam's menus, the desktop panel and other apps' overlays. SteamVR only lets a laser reach those, and a laser needs a controller. So the plan is a small driver of our own that shows SteamVR an invisible controller, aimed along our ray.

The decisions below came in over 2026-10-01, as stages ran. Each one replaces part of the design further down, so read them first.

> **My decisions (2026-10-01):** "ray traced method" = this design (the laser on foreign UI).
> Hand role: **the mouse borrows a hand** while it's in use, even from a resting controller (R-2:
> the physical mouse overrides other inputs unless idle), and gives it back when the mouse goes
> idle (30 s sleep) or cc-panels stops (watchdog 300 ms). This replaces the yield-only rule in
> "No hostage" point 2 and the controller-pickup release in point 3.
> **Stage 3 result and decision (2026-10-01):** leasing the left hand while the left controller
> was off worked (role taken, right controller untouched, released at once). Borrowing it while
> the left controller was on, at high priority, stripped BOTH real controllers' roles; the right
> one stayed role-less until a button press. My decision: **free hand only**: the laser leases
> the non-dominant hand only while no real controller holds it, at the lowest priority always.
> This supersedes the borrow rule below. With the left controller on, the mouse still works on
> our panels (direct RDP), just not on foreign UI.
> **Primary device (my call, later the same day):** whatever had the most recent physical button
> press is primary, and a press overrides the idle wait. A real controller's button press (seen
> as VREvent_ButtonPress from a real controller, or a laser click on our panels) makes the
> controller primary at once: kvm sleeps, so the lease and the borrowed hand end. A mouse click
> or deliberate motion makes the mouse primary again.
> **Dominant hand (my call):** the user picks their dominant hand (setting `dominant_hand` in
> ~/.config/control-center/settings.json, "right" by default). The mouse's laser only ever
> leases the OTHER hand, so the dominant controller always keeps its role and both hands exist.
> This replaces "free hand first, else right" in No hostage point 2. One SteamVR restart to load
> the driver is OK.

## Decision

This builds on **Design 2, the leased virtual controller**, with the judges' suggestions added.

- `kvm.rs` keeps its ray. That ray stays the only source of truth for where the pointer is.
- On our panels, moves, clicks and the wheel go straight to RDP. Nothing changes there.
- Off our panels, a small CC-owned SteamVR driver (`cc_pointer`) aims an invisible controller along the same ray. SteamVR's real laser then reaches the dashboard, Steam menus, the gamescope desktop panel and other apps' overlays.

**How I read "ray traced method":** our ray is the pointer, and the SteamVR laser follows it. If you meant "keep only our own ray and drop the laser", only Stage 0 applies (see Q1).

## Architecture

```
mouse ─evdev─> cc-panels (Rust)
                ├─ kvm.rs land(): our ray + hit test (unchanged)
                ├─ on our panel ──────────────> rdp::mouse/wheel  (R-1 path, unchanged)
                └─ laser.rs ─@cc_pointer lease─> vrserver: driver_cc_pointer.so (Rust, crates/cc-pointer)
                                                     └─ /pose/raw + buttons ─> vrcompositor laser ─> foreign overlays
```

### Driver (`crates/cc-pointer`, Rust, a dumb shim; manifest and resources in `driver/cc_pointer/`)

The driver runs inside vrserver, so a bug there takes SteamVR down. That's why it holds no
logic of its own: it shows whatever the last lease says, and cc-panels does all the thinking.

**Device**
- One Controller, `cc_pointer_0`, with an invisible render model.
- `ModelNumber`/`ControllerType` = `cc_pointer`. `DriverVersion` = `ccp/1`, which is the protocol gate.
- `Prop_ControllerHandSelectionPriority_Int32 = -1000000`.
- Starts disconnected with an `OptOut` role hint.

**Socket**
- Abstract datagram socket `@cc_pointer`.
- Datagrams from any other uid are dropped (`SO_PASSCRED` check).
- Two messages:
  - `L <seq> <L|R> px py pz qw qx qy qz <btnmask> <sx> <sy>`: a lease with the full state, in raw space.
  - `H`: hide now.
- The driver keeps only the last lease and when it arrived, so a lost packet can't strand a button or a role.

**RunFrame, a pure function of the last lease**

A lease counts as **wanted** only when all of these hold:
- it's less than 300 ms old;
- at least 20 s have passed since Init (startup guard);
- the pose is finite;
- it isn't inside the 2 s reconnect back-off after a watchdog drop.

Then:
- **Wanted but not connected:** set the role hint to the leased hand this frame, and connect on the next frame.
- **Not wanted but connected:** in the same frame, set `OptOut`, disconnect, and zero every button and scalar.
- **Buttons:** presses are latched edges. A tap is held down for at least one frame, so the driver doesn't need hold timers.
- **No maths and no libm.** cc-panels sends a finished quaternion. A post-build `objdump -T` check still fails the build on any symbol newer than GLIBC_2.39.

**Bindings** (its own copies, shipped with the driver)
- Laser pointer bound to `/pose/raw`.
- trigger / b / x = left / right / middle click; joystick = scroll; system = ToggleDashboard.
- **`switchlaserhand` is on `a` only**, so the first click is never swallowed.
- The `steam.client` binding is empty.

### cc-panels, new `laser.rs` (about 250 lines plus tests)

**Lease sender.** A dedicated 20 ms timer thread sends the latest snapshot.
- It stops sending if the render loop's snapshot is older than 1 s, so a hung client still lets the watchdog fire.
- A render hitch shorter than 1 s doesn't cause a disconnect, reconnect and binding reload any more.

**Pose**
- Read the HMD pose in Standing and in RawAndUncalibrated: `toRaw(v) = R·S⁻¹·v` (standing space back to the raw tracking space SteamVR expects from a driver).
- Laser origin, on the line from the eye to the cursor:
  - On our panel: `max(0, min(0.95·d, d−0.15))`.
  - Elsewhere: 0.25 m from the eye, and SteamVR does the hit test.
- No `vrcmd` polling and no FindOverlay key list.

**Laser state:** Off → Claiming → Healthy | Degraded
1. `show` (start leasing), wait 300 ms, then press `a` for one latched frame.
2. 400 ms later, the laser is **Healthy only if `GetPrimaryDashboardDevice() == ours`.**
3. Otherwise retry the pulse once, then go to **Degraded**: stop leasing and tint the dot amber.
4. On Degraded, scan the last 64 KB of `vrserver.txt` once for `Too many binding loads`. If it's there, stay Degraded until the vrserver pid changes.

**Click routing** (`kvm.rs` `key()` and wheel)
- The target is captured at button-down and kept until button-up.
- **Target is our panel:** RDP, unchanged.
- **Not ours and Healthy:** set the lease bits, or scroll ±1.
- **Otherwise:** drop the click (it was already dropped before), with the dot amber.

**Avoiding double clicks.** While kvm is engaged, `main.rs laser()` drops every overlay-mouse event on our panels, because kvm already sent those clicks to RDP. This doesn't depend on the undocumented `trackedDeviceIndex`.

**Lasermode overlay** (a 4×4 trick)
- Shown only while the laser is Healthy and kvm is engaged. This keeps laser-mouse mode on when the dashboard is closed.
- Hidden the rest of the time, so games keep their controllers.

**Dot**
- Hidden when the ray is off our panels and the laser is Healthy, so SteamVR's own cursor shows.
- Shown everywhere else.

**Dashboard toggle:** the plan was the Home key. As built, it's Right Ctrl + D, which sends `system` for one latched frame while the laser is Healthy (kvm.rs). Right Ctrl + Home recalls the "home" spot instead.

**Wrapper and lifecycle**
- cc-panels sends `H` to `@cc_pointer` at startup, on QUIT, in `GiveBack::drop` and from a panic hook.

## Why the two hard requirements hold

**R-1 (clicks never depend on role or bindings).**
- The route is decided at button-down from our own hit test. On our panels the path is ray → `land()` → `rdp::mouse`, and it never reads roles, bindings, laser state or the driver. If the driver is missing, blocked, Degraded, has no role, or the binding budget is used up, our panels click exactly as before.
- Foreign UI has no public route except the laser. There, a dead laser is caught by the `GetPrimaryDashboardDevice` check and shown as an amber dot instead of failing silently.
- **Likely root cause of the dead clicks I was seeing:** a virtual pointer that toggled its device 56 times in 50 minutes which used up SteamVR's binding-load limit (`lc=212`, then "Too many binding loads"). This design avoids that in four ways:
  - one connect per wake;
  - at most one connect per 10 s; past that it goes Degraded and never holds the role longer;
  - a reconnect back-off after any watchdog drop;
  - no helper restarts and no vrcmd polling.

**No hostage.** The driver must never keep a hand from you.
1. **Watchdog:** with no lease for 300 ms, the driver sets OptOut, disconnects and zeroes all buttons. This covers kill -9, SIGSTOP, `systemctl stop` and a hung render loop.
2. **Borrow while the mouse is in use (my decision, R-2; since replaced by "free hand only", see the decisions above):** while kvm is awake it leases the
   non-dominant hand (dominant_hand setting, right by default, so the left), even from a resting
   real controller, with a high hand-selection priority while connected. It never
   holds a role while the mouse is idle: kvm sleep (30 s without mouse use) ends the lease.
3. **Release** on any of these:
   - kvm sleep (the mouse went idle);
   - headset idle or standby;
   - cc-panels stops, hangs or crashes (the 300 ms watchdog);
   - SteamVR gives the role to a real device anyway (`TrackedDeviceRoleChanged` /
     `PrimaryDashboardDeviceChanged` naming one): we don't fight for it, because a reconnect
     loop would burn binding loads; the next lease attempt waits for the 10 s rate limit.
   Not a release trigger any more: picking up a controller while the mouse is in use (R-2: the
   mouse wins unless idle). Stage 3 checks whether SteamVR's arbitration lets the borrow work.
4. **Fail-fast:** no role within 1 s of `show` means stop leasing and back off for 10 s.
5. **Startup:** the driver refuses leases for 20 s after Init, and a disconnected device is always OptOut. Nothing can hold a hand while the Steam UI loads.
6. **Install:** a loaded `.so` is never overwritten.

## Build and install

- The driver build script is `driver/cc_pointer/build.sh`. It builds `crates/cc-pointer` (a cdylib) in the `control-center` container and copies it to `build/driver_cc_pointer.so`. The crate hand-writes the C++ vtables of the local `/opt/steamvr/tools/hellovr_vulkan_linux/src/openvr/headers/openvr_driver.h` (SDK 2.1.0): it implements `IServerTrackedDeviceProvider_004` and `ITrackedDeviceServerDriver_005`, and calls `IVRServerDriverHost_006`, `IVRProperties_001`, `IVRDriverInput_003` and `IVRDriverLog_001`.
- It was C++ until 2026-10-03 (a D-017 exception). I checked the Rust port with `crates/cc-pointer/tests/harness.rs`: both `.so`s, driven through a fake runtime, made identical call sequences.
- The install step (now `driver/cc_pointer/install.sh`):
  1. `hash = sha256(.so + resources)[:12]`, then copy to `~/.local/share/control-center/cc_pointer-<hash>/`.
  2. If that directory is already registered, do nothing.
  3. Otherwise run `vrpathreg adddriver <new>` and `vrpathreg removedriver <old>`.
  4. Prune only directories that are no longer registered and not mapped in `/proc/<vrserver>/maps`.
  5. Print "restart SteamVR to load".
- The script never restarts SteamVR.
- At start, cc-panels turns off the laser path (and only that path) when:
  - the device is missing;
  - `DriverVersion` isn't `ccp/1`;
  - `driver_cc_pointer.blocked_by_safe_mode` is set.
- openvr-sys gains declarations for `GetPrimaryDashboardDevice`, `GetTrackedDeviceIndexForControllerRole` and `FindOverlay`, if they're missing.

## Staged plan

**Stage 1. Driver**
- Do: write the driver (now `crates/cc-pointer`; lease, watchdog, back-off, startup guard, hint-then-connect, latched edges, uid check, priority, version), the resources and `build.sh`.
- Test: the build passes the objdump check. A standalone `cc-ptr-poke` test client sends leases and then stops, and the roles probe shows the role released within 300 ms. A lease sent within 20 s of Init is ignored.

**Stage 2. Install, then one SteamVR restart (your action)**
- Do: the versioned install step.
- Test: `vrserver.txt` shows `cc_pointer` loaded and `cc_pointer_vrcompositor.json` loaded. Running the install again does nothing.

**Stage 3. Spike on the new driver**
- Do: drive it from the poke client.
- Test, and record the answers to:
  - Do resting Frame controllers keep their roles?
  - Does priority -1000000 yield to a real controller that powers on?
  - Does the claim show up as `GetPrimaryDashboardDevice == ours`?
  - Do the GamepadMode_Right flags change during clicks?

**Stage 4. `laser.rs`, pure parts**
- Do: `toRaw`, the origin rule, lease formatting, the hand pick and the state machine.
- Test: unit tests for:
  - a standing↔raw round trip with a 1.6 m offset;
  - claim ok → Healthy, claim failed twice → Degraded;
  - rate limit;
  - both hands held → no lease.

**Stage 5. Wiring**
- Do: hooks in `set_engaged`, `key()`/wheel (target captured at button-down), the `laser()` drop, the lease thread, the lasermode overlay, the dot, and the startup/exit/panic hides.
- Test, live:
  - our panels click with the driver turned off;
  - the dashboard, Steam menu and desktop panel click through the laser;
  - a drag that starts on a panel never reaches the laser;
  - no double clicks.

**Stage 6. Acceptance**
- Test:
  - `kill -9 cc-panels` → the real controller works within 0.5 s;
  - picking up a controller hands the laser back;
  - 1 h of wake/sleep cycling adds only a few `lc` per wake, with no "Too many binding loads";
  - with a forced Degraded state the dot is amber and our panels are unaffected.

**Stage 7. Docs**
- Do: add a D-0xx entry for the C++ shim exception (the driver has been Rust since 2026-10-03, see above). Record the invariants: never replace a loaded `.so`, take a role only when it's free, the watchdog is mandatory.

## Risks

- **Yield priority and resting-controller roles were unverified (Stage 3 decided).** If the Frame controllers keep both roles while resting, the mouse reaches foreign UI only when a controller is off or asleep. Our panels are unaffected either way.
- **The binding-load limit is undocumented** (we don't know the threshold or how it decays). Even one connect per wake adds to it. Hitting it is now detected and visible, but only a SteamVR restart has been seen to clear it.
- **Visible beam off our panels.** With the origin 0.25 m from the eye, the beam should read as a short stub, but parallax may show a line. `laserRayWidthScale` can't be changed live.
- **Left out on purpose:**
  - Special cases for scene-graph overlays (dock, footer) and the SteamVR Settings page: SteamVR does the foreign hit test, so they should just work. I'll add workarounds only if they turn out to be needed.
  - Drag/tilt placement of foreign windows: I'll add it when someone asks for it.
- **Gamepad-mode capture (`modalGamepadAndLaser`)** could still swallow laser clicks on Steam UI. The claim check can't see this; Stage 3's flag logging decides whether it needs a fix.
- **The 20 s startup guard is a guess.** The upgrade path is to wait until `valve.steam.gamepadui.main` exists.
- **A driver bug crashes vrserver.** So it's kept small: no exceptions, no libm, a bounded parser, one socket thread.

## Questions only you can answer

All three were answered by the decisions at the top.

1. **"Ray traced method":** should the mouse also drive the SteamVR laser on foreign UI (this design), or only our own ray on our panels (Stage 0 only)?
2. **Both controllers on:** yield only, meaning foreign UI is reachable by mouse only when a controller is off or asleep (this is the default), or may the mouse borrow a resting controller's hand and hand it back the moment that controller moves (some hostage risk)?
3. **Restart:** is one SteamVR restart OK to load `cc_pointer` (Stage 2)?

Key files:
- `crates/cc-panels/src/kvm.rs`
- `crates/cc-panels/src/laser.rs`
- `crates/cc-panels/src/main.rs`
- `crates/cc-pointer/` (the driver)
- `driver/cc_pointer/` (its manifest, resources, `build.sh` and `install.sh`)
- `/opt/steamvr/tools/hellovr_vulkan_linux/src/openvr/headers/openvr_driver.h`
