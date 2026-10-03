# Moving panels: recommendation

Panels could only be moved with Right Ctrl gestures or by saving and recalling spots. You couldn't just grab one with a controller and put it somewhere, the way you can with a SteamVR window. This is how I planned to add that.

> **My decisions (2026-10-01):** our own grab bar with SteamVR's feel (not dashboard
> overlays). Releasing a dragged panel **auto-saves it to the "home" spot**, replacing "No
> auto-save on release". Stage 0 (the grab-bar check) runs first.
> **Edges too (my call):** besides the bar, the panel's edges grab it: a frame overlay per panel
> (`controlcenter.frame.<name>`), a few cm larger than the panel on every side, 2 mm behind it,
> transparent in the middle, so the laser hits the picture in the middle (clicks go to the
> remote as now) and the frame at the edges. A trigger press on the frame starts the same rigid
> carry as the bar; it fades in on hover like the bar. Placed by `set_pose` with the bar; curved
> like the panel. Corners stay plain grab for now (resize from corners is stage 4, on request).

**What was built since.** The bar and the edge frame ended up as one overlay per panel, the
card: a rounded frame drawn as a whole, sitting just behind the picture, with the grab bar and
the tag painted in. Where a laser lands on the card says whether it's the bar, an edge or a
corner. One overlay instead of separate pieces means one depth and no joins, and a panel costs
SteamVR only two of the 128 overlays every app shares. Corners resize, and there's a curve
button beside the bar. `crates/cc-panels/src/grab.rs` has the details. The rest of this doc is
the plan as it was written.

## Winner: build our own grab bar on top of SteamVR's laser input

This is the "Grab bar" design, with a few ideas taken from the hybrid design.

**Why not SteamVR's own grab bar.** SteamVR only gives a floating-window frame to dashboard overlays that have `VisibleInDashboard` set. Once the dashboard owns a panel, we can't set or read its position any more, no event tells us when it moves, and the dashboard forgets the position when it restarts. That would break spots, presets and the `place` command. So instead I copy the native behaviour on the world overlays we already have.

**Ideas taken from the other designs:**
- **Bar size:** scale it with the panel's width and distance (never under about 1.7°) instead of a fixed size based on 4°.
- **Release backstop:** each tick, read the dragging device's trigger with `GetControllerState`.
- **Games keep their controllers:** turn `MakeOverlaysInteractiveIfVisible` off on panels and bars while `GetCurrentSceneProcessId() != 0`.
- **Zero-lag fallback:** if the carry lags, attach the panel with `TrackedDeviceRelative` for the length of the drag.
- **Parked experiment:** keep the dashboard-overlay probe as a separate, optional test.

## What you'll see and do

- **Showing the bar.** Point a controller laser at a panel. A light grey pill fades in about 7.5 cm below the panel's bottom edge. It sits at 55% opacity and goes fully opaque when the laser is on it. It stays about 0.4 s after the laser leaves, so you can slide down onto it.
- **Grab.** Pull the trigger on the bar. You feel a short haptic tick and the panel locks to that controller. Moving or twisting the controller moves or tilts the panel, the same way a SteamVR window behaves.
- **Push and pull.** While holding the trigger, push the joystick up or down to move the panel away or closer, between 0.25 and 5 m.
- **Release.** Let go of the trigger and the panel stays where it is. There's no snapping.
- **Remote machine.** It never sees the grab click or the release.
- **Two hands.** Each controller can carry a different panel at the same time.
- **Right Ctrl gestures.** These keep working as before, and the bar follows them.
- **Mouse.** The virtual-controller mouse laser grabs the bar with no extra code, because it arrives as just another tracked device.

## How it works

1. **A bar overlay for each panel.** Each panel gets an extra overlay, `controlcenter.bar.<name>`.
   - Its texture is a 256×24 pill, drawn by `crates/cc-panels/src/assets.rs`.
   - It uses Mouse input with `SendVRDiscreteScrollEvents`, `SortOrder 110`, and is shown at alpha 0.
   - Click stabilization is **off**, so the laser isn't pulled back to the bar's old spot while it moves.
2. **One place sets every pose.** `Kvm::set_pose` (kvm.rs) already runs for every move. At its end it now also places the bar, at `on_surface(0, -(h/2+0.075), 0.003)`, with its width from ChromeSize and its curvature matched to the panel.
3. **Drag state.** The main thread owns a `drags: Vec<Option<Drag{dev, rel}>>`.
   - **Start:** a Left `MouseButtonDown` on a bar sets `dev = ev.trackedDeviceIndex` and `rel = inv_rigid(pose[dev]) · place[i].matrix()`, then plays the haptic.
   - **Every 11 ms tick:** read all device poses in one call, then `set_pose(i, pose[dev]·rel)`.
   - **Scroll while dragging:** scale the head-to-panel vector by `1+0.08·dy`, clamp it, then recompute `rel`.
   - **End:** the drag ends on any of these:
     - a `MouseButtonUp` from that device on any of our overlays;
     - the trigger reading released;
     - `TrackedDeviceDeactivated`;
     - `DashboardActivated`.
4. **Changes to `laser()`.** While a device is dragging, its events go nowhere near RDP. The mouse-wins rule keeps gating only RDP forwarding.
5. **New helpers.** `geometry.rs` gets `mul` and `inv_rigid`, with a round-trip test. `vr.rs` gets `poses()` and `haptic()`.

## Presets and spots keep working

- `place[i]` stays the only copy of each panel's pose, and every path writes it through `set_pose`. So `cc-home save` captures a dragged position exactly, roll included.
- Recalling a spot or preset clears that panel's drag first, so the recall wins over a drag in progress.
- On release, the panel's pose is saved into the "home" spot (my decision), the same way `cc-home save` stores it (by viewer name).
- `home.json` and the `place` command don't change.

## Staged plan

| Stage | Work | Test |
|---|---|---|
| 0 (10 min) | No code. With the dashboard closed, drag an existing world-overlay grab bar on the Frame with each controller. | It passes if all of these hold: the press grabs; the joystick pushes while the trigger is held; the laser stays on the bar during the carry; releases land in 20 of 20 fast swings. If any fail, the design changes before I write code. |
| 1 | Bar and edge-frame overlays, their placement in `set_pose`, their fade, and showing/hiding them with the panel. | The bar tracks the panel through `place`, a preset, and Right Ctrl swing and scale, on flat and curved panels. |
| 2 | Drag Vec, rigid carry, push and pull, the release paths and trigger backstop, swallowing events to RDP, recall cancelling a drag, haptic. | Carry with each hand; two panels at once; release over another panel; recall a preset mid-drag; `cc-home save t` then `cc-home t` gives the same pose. The remote machine logs no clicks. |
| 3 | Gate the interactive flag on the scene process, plus the `TrackedDeviceRelative` carry if stage 2 lags. | Start a VR game and check the controllers aren't captured. Swing fast and check the panel doesn't trail. |
| 4 (on request) | Curve-toggle button, resize corner (the corner moves along the diagonal, centre fixed), snapping to a hand within 0.4 m. | Matches the native footer: curve on and off, and resize from any direction. |

Stages 1 to 3 also include the docs on cc-panels and the grab bar.

## Risks

- **A lost release could leave a panel stuck to your hand.** The trigger backstop, ending on Up from any of our overlays, and ending on device deactivation cover it. Stage 0 measures how often it would happen.
- **The carry can lag a frame behind the controller** because of the 11 ms loop and texture uploads. The fallback is `TrackedDeviceRelative` during the drag.
- **The invisible bar still takes clicks** in its small strip under each panel. It never covers remote-desktop content, so I think that's fine.
- **Overlay magnetism could pull the laser off a moving bar.** Stabilization is off on the bar, and stage 0 confirms whether that's enough.
- **The drag tick takes the KVM lock** that the input thread also uses. It only holds it briefly, the same pattern as `update_pointer`.

## Questions for you

1. Is "feels like SteamVR, drawn by us" enough? The alternative is that you need SteamVR's literal frame: its own footer, dock-to-hand and theater mode. That only comes with dashboard overlays, which give up spots. I'd test it as a separate probe, not build it.
2. Should releasing a dragged panel auto-save it to the active spot, or should saving stay explicit with `cc-home save`? (Answered above: it auto-saves to "home".)
