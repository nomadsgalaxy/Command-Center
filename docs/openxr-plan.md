# Command Center on OpenVR and OpenXR: one core, two backends

Status: plan, 2026-10-29. Phase 0's first step, the runtime probe, has run on the Frame (section
1), and the boundary's design is in `crates/cc-panels/src/backend.rs`, for review before any code
moves. Nothing else here is built yet. Paths are relative to `crates/cc-panels/src/` unless they
start with `crates/`, `docs/` or `tools/`. Anything I haven't checked against the code, a runtime
or its source is marked **unverified**.

## 0. The short version

- Command Center is an overlay app that lives next to games. OpenXR has no general overlay model,
  so the split is by runtime, not by API: OpenVR overlays on SteamVR (unchanged), and OpenXR on
  Monado and WiVRn.
- On OpenXR, Command Center needs `XR_EXTX_overlay` to run beside a game. Without it, the only way
  to run is as a normal scene app that owns the whole view ("desktop mode"). That's never a silent
  fallback: the user picks it, because it takes the view away from whatever else is running.
- The Frame itself gets nothing from OpenXR. Its only runtime is SteamVR, whose OpenXR runtime
  doesn't advertise `XR_EXTX_overlay` (probed, section 1), and I know of no Monado driver for it.
  Index works today through OpenVR. Quest through Link, Virtual Desktop or ALVR works today too,
  because SteamVR runs on the PC.
- The OpenXR backend pays off for Linux PCs on Monado, for WiVRn (Quest streaming from Linux
  without SteamVR), and later for a native Quest app. It's also the only way to run where the
  OpenVR runtime is a layer on top of OpenXR (xrizer, WiVRn's default OpenVR layer,
  OpenComposite), because those don't implement `IVROverlay` (xrizer issue 99), so our OpenVR
  build can't run there at all. That sets the priority: this is a portability investment, not a
  Frame feature.
- About 210 `call!(...)` sites in 25 files touch OpenVR (counted with grep, `bin/cc-win-spike.rs`
  included). Roughly 60 percent are overlay placement, visibility and input-method calls that
  already go through `overlay.rs`, `surface.rs` or a handful of helpers. The hard parts are the
  texture path (`gpu.rs`, `capture.rs`), input (OpenXR has no mouse events), the keyboard, and the
  dashboard tab.
- Estimate: phase 0 about 10 days, the OpenXR MVP about 18 days, parity about 15 days. That's 6 to
  8 weeks for one person. The Vulkan texture slice gets re-estimated after its spike (section 4),
  and most of the risk is there and in what overlay support the runtimes really have.

## 1. The hard constraint: overlays

### What OpenXR gives us

In core OpenXR, one app owns one session, submits layers (projection, quad, cylinder and so on) in
`xrEndFrame`, and the runtime shows it as the foreground app. There's no standard way to say "draw
on top of whatever else is running".

`XR_EXTX_overlay` is an experimental Khronos extension (EXTX means it was never promoted). An app
creates its session with `XrSessionCreateInfoOverlayEXTX` and a `sessionLayersPlacement`, and the
runtime composites its layers over the main app's. There's an event for the main session's
visibility changing. Monado implements it. Unverified: whether it's on in distro builds, and the
exact input-focus rules for an overlay session.

### What each runtime has

`tools/xr-probe` lists what a runtime advertises, and nothing else: it only calls
`xrEnumerateInstanceExtensionProperties`, never `xrCreateInstance`, so it doesn't connect an app to
SteamVR or spend SteamVR's binding loads. Each runtime is asked in its own process with
`XR_RUNTIME_JSON` pointing at its manifest. Build and run it like this:

```
cd tools/xr-probe && cargo build --release && target/release/xr-probe
```

There are four levels, and each runtime only gets the ones that have actually been tried:

1. **Advertised:** the extension is in the runtime's list. The probe checks this.
2. **Session created:** an overlay session starts. This needs an instance.
3. **Composites beside a game:** its layers show over another app's.
4. **Usable input:** the overlay session gets controller actions while another app has focus.
   The laser depends on this.

| Runtime | `XR_EXTX_overlay` | Evidence |
|---|---|---|
| SteamVR 2.18.2 (build 1791331621), the Frame | Not advertised. Levels 2 to 4 not tested (no instance) | Probed 2026-10-29, below. It matches what a Monado developer said on the Khronos forum, and there's an open Steam feature request for it |
| Monado | Implemented; the level isn't tested here | It's not installed on the Frame or this machine. Probe on a PC that has it |
| WiVRn | Very likely: it shares Monado's IPC and multi-client code, and its README points users to WayVR, which needs the extension. Not tested | It isn't installed here either |
| ALVR | Not applicable: its PC side is a SteamVR driver, so overlays there are OpenVR | Architecture |
| Quest native (Meta's runtime) | No, to my knowledge | **Unverified** |

The probe on the Frame, 2026-10-29: the manifest is `~/.config/openxr/1/active_runtime.json`, a link
to `/opt/steamvr/steamxr_linuxarm64.json` (name "SteamVR", library `bin/linuxarm64/vrclient.so`).
The loader is SteamVR's own `/opt/steamvr/bin/linuxarm64/libopenxr_loader.so`. The runtime's
version needs an instance, so it comes from vrclient's log. Under `strace` the probe made no
`socket` or `connect` calls and started nothing. Its only write was vrclient's own log,
`~/.local/share/Steam/logs/xrclient_xr-probe.txt`. It advertises 49 extensions:

| Extension | SteamVR (Frame) |
|---|---|
| `XR_EXTX_overlay` | no |
| `XR_KHR_composition_layer_cylinder` | no |
| `XR_KHR_composition_layer_color_scale_bias` | no |
| `XR_KHR_vulkan_enable2` | yes, v4 |
| `XR_EXT_eye_gaze_interaction` | yes, v2: advertised, support not checked |
| `XR_EXT_user_presence` | yes, v1: advertised, support not checked |
| `XR_VALVE_frame_controller_interaction` | yes, v1 |

The rest: `XR_KHR_vulkan_enable`, `XR_KHR_opengl_enable`, `XR_KHR_opengl_es_enable`,
`XR_KHR_convert_timespec_time`, `XR_MNDX_egl_enable`, `XR_KHR_binding_modification`,
`XR_KHR_composition_layer_depth`, `XR_KHR_locate_spaces`, `XR_KHR_visibility_mask`,
`XR_KHR_generic_controller`, `XR_EXT_active_action_set_priority`, `XR_EXT_dpad_binding`,
`XR_EXT_frame_composition_report`, `XR_EXT_hand_interaction`, `XR_EXT_hand_tracking`,
`XR_EXT_hand_tracking_data_source`, `XR_EXT_hand_joints_motion_range`,
`XR_EXT_hp_mixed_reality_controller`, `XR_EXT_local_floor`, `XR_EXT_palm_pose`, `XR_EXT_uuid`,
`XR_EXT_view_configuration_views_change`, `XR_EXT_render_model`, `XR_EXT_interaction_render_model`,
`XR_EXT_interaction_profile_battery_state_display`, `XR_EXT_frame_synthesis`, `XR_EXT_debug_utils`,
`XR_FB_display_refresh_rate`, `XR_FB_swapchain_update_state`, `XR_FB_foveation`,
`XR_FB_foveation_vulkan`, `XR_FB_foveation_configuration`, `XR_FB_space_warp`,
`XR_HTC_vive_cosmos_controller_interaction`, `XR_HTC_vive_focus3_controller_interaction`,
`XR_HTC_vive_wrist_tracker_interaction`, `XR_HTCX_vive_tracker_interaction`, `XR_MND_headless`,
`XR_META_performance_metrics`, `XR_META_recommended_layer_resolution`,
`XR_META_foveation_eye_tracked`, `XR_META_vulkan_swapchain_create_info`, `XR_VALVE_analog_threshold`,
`XR_VALVE_timing_utils` and `XR_VALVE_app_space_delta_pose`.

What it means:

- SteamVR stays OpenVR-only for Command Center, as planned. Without overlays there's no way to run
  beside a game through its OpenXR runtime.
- SteamVR's OpenXR runtime doesn't have cylinder layers or colour scale and bias either. That only
  matters for desktop mode on SteamVR, which is out of the first cut anyway (see the decision
  below). On Monado and WiVRn, curved panels and fades depend on what the probe finds there.
- The Frame controller's interaction profile, eye gaze and user presence are all there, so phase
  2's bindings, gaze and worn state have something to use when a runtime has them too.

Advertised isn't supported: whether the Frame's eye tracking and presence sensor are really behind
those two needs an instance, to read `XrSystemEyeGazeInteractionPropertiesEXT` and
`XrSystemUserPresencePropertiesEXT`. The probe also doesn't look in Flatpak Steam's paths
(`~/.var/app/com.valvesoftware.Steam/`), so a SteamVR installed that way needs its manifest passed
by hand.

Still to probe, on a PC with Monado and with WiVRn: the same list, then levels 2 to 4 with an
overlay session next to `hello_xr`, and whether those runtimes expose
`XR_VALVE_frame_controller_interaction`. If WiVRn turns out not to have `XR_EXTX_overlay`, the MVP
there is desktop mode only.

### Prior art: WayVR (formerly wlx-overlay-s)

WayVR (wayvr-org/wayvr) is a Rust app with an OpenVR and an OpenXR backend, picked by cargo
features and at start-up. From its public source:

- The OpenXR backend is one Vulkan overlay session that submits panels as quad and cylinder layers
  in STAGE space, blended with source alpha. Colour scale and bias, cylinder layers and dpad
  bindings are optional.
- It has no foreground fallback. `init_xr()` requires `XR_EXTX_overlay` and gives up without it
  ("Will not use OpenXR").
- It talks to the Monado service over Monado's IPC (libmonado) for chroma key and the playspace
  mover. That's a Rust reference for scene-app detection in phase 2.
- Both backends share the pointing code. It ray-casts controller poses against its own panels and
  then drives the Wayland or uinput side, so it doesn't depend on SteamVR's per-overlay mouse
  events. It ships its own on-screen keyboard instead of SteamVR's, and it reads controllers
  through OpenXR actions or an OpenVR action manifest behind one input layer.

What to take from it: the shape (shared hit-testing, our own keyboard, a thin per-API surface
writer). I'll read its current source in detail before phase 1, rather than work from a summary.

### Decision

The backend is chosen at start: `--backend openvr|openxr|auto`.

- `auto` tries OpenVR when a `vrserver` is up (`VR_IsRuntimeInstalled`, `VR_IsHmdPresent`), and
  OpenXR otherwise. On a Monado-only machine there's no SteamVR to ask, so that check has to fail
  softly into OpenXR, not stop. When the OpenVR runtime is a layer over OpenXR (xrizer,
  OpenComposite), `auto` picks OpenXR, since `IVROverlay` isn't there.
- On OpenXR, Command Center asks for `XR_EXTX_overlay`. If the runtime doesn't have it, it doesn't
  quietly take over the view: it says so in the log and the Machines window, and runs as a scene
  app only if the user has chosen desktop mode (a setting, or answering a prompt).
- Desktop mode on SteamVR's OpenXR runtime is out of the first cut, since OpenVR does everything
  better there. Desktop mode is for runtimes without the overlay extension.

## 2. Inventory of OpenVR use, and the OpenXR answer

Counts are from `grep -o 'call!(<table>, <fn>'` over `crates/cc-panels/src`. The tables are
fetched in `vr.rs:41-63` (`ov`, `sys`, `ipc`, `input`, `rm`, `apps`, `notes`).

### 2.1 Overlay create, show, order, flags
| OpenVR | Where | OpenXR |
|---|---|---|
| `CreateOverlay` (1), `DestroyOverlay` (2), via `surface::create/destroy` and `vr.rs:100` | surface.rs, vr.rs | Nothing needed: a surface is our own struct (pose, size, curve, swapchain, visible). The swapchain is created lazily |
| `ShowOverlay` (20), `HideOverlay` (19), `IsOverlayVisible` | everywhere | Include or leave out the layer in `xrEndFrame` |
| `SetOverlaySortOrder` (15), `GetOverlaySortOrder` (2) | taskbar, grab, back, hud | The order of the layer array. We sort surfaces ourselves |
| `SetOverlayFlag` (22 sites) | VROverlayFlags: ShowCursor, SendVRScrollEvents, VisibleInDashboard, EnableControlBar and others | Mixed. The cursor: we draw it (there's already a cursor overlay, `overlay.rs:5`). Scroll events, the control bar and VisibleInDashboard are SteamVR-only |
| `SetOverlayAlpha` (8), `SetOverlayColor` (2) | back, hud, taskbar | `XR_KHR_composition_layer_color_scale_bias` where the runtime has it (SteamVR's OpenXR runtime doesn't, section 1). Otherwise bake it into the texture. Measure how often fades change alpha before choosing, since baking means a re-upload per step |
| `SetOverlayName`, `SetOverlayFromFile` (`vr.rs:119`) | icons | Load the PNG into a swapchain ourselves (assets.rs already rasterises) |

### 2.2 Placement, shape
| OpenVR | Where | OpenXR |
|---|---|---|
| `SetOverlayTransformAbsolute` (`vr.rs:134`, `overlay.rs:150`) | every panel | The quad or cylinder layer's `pose` in a STAGE reference space (or LOCAL_FLOOR, core in 1.1) |
| `SetOverlayTransformTrackedDeviceRelative` (`overlay.rs:155`, docking) | grab.rs, taskbar | The layer's `space` is an action space on the grip or aim pose. Head-relative is VIEW space |
| `SetOverlayWidthInMeters` (5) | overlay.rs | `XrCompositionLayerQuad::size`, or the cylinder's `radius*centralAngle` |
| `SetOverlayCurvature` (1), `geometry::bend` | overlay.rs | `XR_KHR_composition_layer_cylinder`: radius, centralAngle, aspectRatio. The maths in `geometry.rs` (`bend`, `radius_for`) inverts cleanly. Panels curved top to bottom (`PanelFacts.vert`, `stage.rs:21`) are cylinders turned a quarter, the same trick as today. Without the extension (SteamVR's OpenXR runtime), panels are flat |
| `SetOverlayTexelAspect` (get and set) | overlay.rs | Folded into the quad size or the cylinder's aspectRatio |
| `SetOverlayTextureBounds` (2) | overlay.rs, back.rs (crops, plasmabar) | `XrSwapchainSubImage.imageRect`, in pixels rather than UV. The backend converts |
| `SetOverlayMouseScale` (7, get and set) | overlay.rs | Not needed: our own hit test gives pixels directly |

### 2.3 Textures
| OpenVR | Where | OpenXR |
|---|---|---|
| `ipc ImportDmabuf` + `SetOverlayTexture` with `TextureType_SharedTextureHandle` (`gpu.rs:196,250,323`, `capture.rs:538,500`) | The whole video path. SteamVR's private IPC resource manager takes GBM dmabufs with "no size limit" (`gpu.rs:3`) | **Nothing: we write it.** Vulkan (`XR_KHR_vulkan_enable2`): import the dmabuf as a `VkImage` (`VK_EXT_external_memory_dma_buf`, `VK_EXT_image_drm_format_modifier`) and `vkCmdCopyImage` it into the acquired swapchain image when the panel is dirty. A GPU copy per dirty frame is cheap. Fallback: upload through a staging buffer. See the contract below |
| `UnrefResource` (4) | gpu.rs, capture.rs | Destroy the VkImage and its memory, after the GPU work that uses them has finished |
| `SetOverlayRaw` (5) and `ClearOverlayTexture` | UI cards drawn on the CPU (`grab.rs:791`, `back.rs:200`, `capture.rs:461`, `laser.rs:387`, 4x4 pixels) | Write the pixels into a staging buffer and copy them to the swapchain image. SetOverlayRaw's blink (it blanks the overlay until SteamVR takes the new picture) doesn't happen on OpenXR, since swapchains are double-buffered anyway |

How textures are owned today, on OpenVR (gpu.rs, capture.rs, back.rs). This is what the
`Textures` trait has to keep in phase 0; it isn't a design for the Vulkan side:

- **Import doesn't take the caller's fd.** `ImportDmabuf` gives back a handle, and SteamVR holds
  its own reference to the buffer behind it. gpu.rs closes the GBM buffer's fd straight after the
  import. capture.rs imports with its own dup of PipeWire's fd (PipeWire closes its copy when it
  removes the buffer), and drops that dup with the frame. The handle lives until `UnrefResource`.
- **When the core may write a buffer again:** a tick after `SetOverlayTexture` replaced it on
  screen. SteamVR never says when it's done reading; the core assumes it's done by its next frame.
  gpu.rs keeps two buffers per panel and holds the replaced one for a tick (`held`), and the RDP
  thread doesn't write it until then. capture.rs holds the buffer on screen and the one before it
  back from PipeWire, and gives older ones back. A refused `SetOverlayTexture` leaves the old one
  showing (`unflip`).
- **Who says the producer is ready:** for a PipeWire dmabuf, the core polls the fd for its
  implicit fence (POLLIN) and doesn't present until it's readable; it keeps the frame for the next
  tick otherwise. Our own GBM buffers are written on the CPU between map and unmap, so they're
  ready when the copy returns. There's no explicit fence anywhere.
- **How the core learns a release is complete:** it doesn't. `UnrefResource` returns at once, and
  the core destroys the GBM buffer right after. A UI overlay's resize keeps the old pair one tick
  longer (`raw_tick`). A panel's resize frees both buffers at once, the one on screen included.
  That's how it is today, and if it ever shows as a blank frame, the fix is its own branch.
- **A handle can be on two overlays:** a panel's back shows the panel's handle (`back::shown`).

These are assumptions that OpenVR happens to tolerate, not guarantees: SteamVR never says when it's
done with a buffer. For OpenXR, each has to be tested or designed out, the shared front and back
handle included, rather than carried over.

The Vulkan side gets its own contract after this one is pinned, covering the same five points
(lifetime, reuse, readiness, release, sharing), plus a failed import falling back to a staging
upload and saying so once.

The spike's exit test is repeated acquire, present and release, then a resize, a reconnect and a
teardown, on each target GPU and runtime. The slice is estimated after that spike, not up front.

### 2.4 Input
| OpenVR | Where | OpenXR |
|---|---|---|
| `SetOverlayInputMethod = Mouse` (12 sites) and `PollNextOverlayEvent` (10 sites): SteamVR hit-tests the laser and sends MouseMove, ButtonDown and Up, Scroll and FocusLeave in overlay-local coordinates | main.rs:444 (`laser`), grab.rs:1378, taskbar.rs:932, plasmabar.rs:457, hud.rs:152, dashtab.rs:282, osk.rs:282, back.rs:234, ui.rs:311 | **Nothing: we write it.** OpenXR gives controller poses (`aim/pose`) and button actions only, so we ray-cast ourselves. `geometry.rs` already has `hit_in`, `hit_card`, `nearest`, `bar_hit` and `at_pixel`, so it's mostly plumbing. It produces the same neutral pointer events |
| `GetDeviceToAbsoluteTrackingPose` (5) for 64 devices, `GetTrackedDeviceIndexForControllerRole`, `GetControllerRoleForTrackedDeviceIndex` | grab.rs:1051, vr.rs, taskbar.rs:1266, laser.rs:415 | `xrLocateSpace` on the hand, aim and grip action spaces at `predictedDisplayTime`. Device indices become `Hand::{Left,Right}` plus `Head` |
| `GetStringTrackedDeviceProperty` (controller type, `vr.rs:208`), `is_real_controller` | filters out our virtual pointer | Not needed on OpenXR (there's no cc-pointer). A hand is real by construction |
| The render model's "tip" lookup, `rm GetComponentStateForDevicePath` (`vr.rs:223` `laser_pose`) | the laser's origin | The `aim` pose, which is what aim means. Per-device tip offsets go away |
| The action manifest: `SetActionManifestPath`, `GetActionHandle`, `GetActionSetHandle`, `UpdateActionState`, `GetEyeTrackingDataRelativeToNow` (`gaze.rs:45-68`; `crates/cc-panels/actions/cc_panels_actions.json`) | gaze only | OpenXR actions: an action set plus suggested bindings per interaction profile. Gaze: `XR_EXT_eye_gaze_interaction` (`/user/eyes_ext/input/gaze_ext/pose`), which SteamVR's OpenXR runtime advertises (section 1); unverified on Monado and WiVRn. Controller button actions are new, since the SteamVR path gets buttons from `VREvent_ButtonPress` plus the user binding (below) |
| Haptics, `TriggerLaserMouseHapticVibration` (`stage.rs:62`) | the dock arming buzz | `xrApplyHapticFeedback` on a haptic output action per hand |
| The SteamVR user binding for the bumper (`~/steamvr/input/...`, the vrserver web API, docs/steamvr-integration.md "The shoulder button is right click") | install time | SteamVR-only. On OpenXR we suggest our own bindings per profile, so there's no external file |
| `sys PollNextEvent` button presses (`main.rs:1103`, `ButtonPress`) | "a controller was used" | Derived from action state changes |

### 2.5 Keyboard
| OpenVR | Where | OpenXR |
|---|---|---|
| `ShowKeyboardForOverlay`, `GetKeyboardText`, `HideKeyboard` (osk.rs:72,228; workspace.rs:610-678; machines.rs:1380-1842), and the KeyboardCharInput, Done and Closed events | typing to a remote, text fields in Machines and Workspace | **Nothing in core.** `XR_FB_keyboard_tracking` tracks a physical keyboard on Meta's runtimes only. We draw our own keyboard (the key strip for Esc, Ctrl, Alt and Super already exists in osk.rs). See 5.2 |

### 2.6 Dashboard tab, shell, notifications, apps
| OpenVR | Where | OpenXR |
|---|---|---|
| `CreateDashboardOverlay`, `IsDashboardVisible`, `IsActiveDashboardOverlay`, `GetPrimaryDashboardDevice`, `VREvent_DashboardActivated/Deactivated`, `VREvent_OverlayClosed` (`vr.rs:110`, dashtab.rs:224-291, main.rs:1108) | the Command Center tab and its close button | **SteamVR-only.** Replaced by a hub panel summoned by a gesture (5.3) |
| `apps`: `AddApplicationManifest`, `SetApplicationAutoLaunch`, `IdentifyApplication`, `IsApplicationInstalled`, `RemoveApplicationManifest` (tab.rs:73-168), `packaging/command-center.vrmanifest.in` | autostart of the tab process | **SteamVR-only.** On Monado and WiVRn: a systemd user unit or XDG autostart. How WiVRn launches apps from the headset is **unverified** |
| `apps GetCurrentSceneProcessId` (taskbar.rs:862, tab.rs:120) and `handoff.rs`'s flat-game detection | "a game is running", power saving, the taskbar | Monado: list the clients and their state over its IPC (libmonado, which WayVR uses). Where the runtime can't tell us, the answer is "unknown", not "no game", and `handoff::flat_game` (gamescope focus, not OpenVR) still runs |
| `notes` IVRNotifications toast (notify.rs:158-172) | the fallback only; the main path is D-Bus | Our own toast surface, a small panel above the taskbar. The D-Bus path is already API-neutral |
| `VREvent_Quit` | close_desktop | `XrEventDataSessionStateChanged` to `STOPPING` or `EXITING`, or `InstanceLossPending` |

### 2.7 Space, tracking, session
| OpenVR | Where | OpenXR |
|---|---|---|
| `GetDeviceToAbsoluteTrackingPose` for the head, `to_photons()` (`vr.rs:149`, GetTimeSinceLastVsync) | prediction for carried panels | `xrWaitFrame` gives `predictedDisplayTime`, and spaces are located at it. `to_photons` becomes a field of the frame |
| `Prop_DisplayFrequency_Float` (main.rs:1028) | pacing | `predictedDisplayPeriod` from `xrWaitFrame`, or `XR_FB_display_refresh_rate` (SteamVR's OpenXR runtime has it; unverified elsewhere) |
| `Prop_CurrentUniverseId_Uint64` (main.rs:909,1121) | room identity for saved spots (`cc_proto::conf::room`) | **Nothing: we decide it.** OpenXR has no universe id. See the room rule in phase 1 |
| `VREvent_StandingZeroPoseReset` (relocalization, main.rs:1118) | machines.relocalized, grab.relocalized | `XrEventDataReferenceSpaceChangePending` for the STAGE space (it has `poseInPreviousSpace`) |
| `TrackedDeviceUserInteractionStarted/Ended` (the headset worn, main.rs:1135 on), `vr::head_worn` | pausing streams, power | `XR_EXT_user_presence` where it's there (SteamVR's OpenXR runtime has it). Session state is a separate thing from being worn (section 3) |
| `VR_InitInternal2(Overlay)` (`vr.rs:46`) | init | `xrCreateInstance` and `xrCreateSession` (with the overlay struct), creating the Vulkan device, then `xrPollEvent` to begin and end the session |
| The main loop parks between ticks and is woken (vr.rs `MAIN`, `wake`; `Pace`) | main.rs | See the frame loop in phase 1 |

## 3. Proposed backend boundary

This builds on the two seams that already exist (`Overlays` at `overlay.rs:80`, `Stage` at
`stage.rs:25`, with fakes at `overlay.rs:fake` and `stage.rs:fake`). They become the public face
of a `Backend`, the missing parts are added, and every `call!` moves behind it.

The layout (one crate to start; it splits into `cc-vr-core`, `cc-vr-openvr` and `cc-vr-openxr` only
if build times hurt):

```
crates/cc-panels/src/
  vr/                     the boundary (backend-neutral)
    mod.rs                Backend trait object, backend(), Handle -> SurfaceId, Dev
    surface.rs            surface state we own (visible, order, alpha, color, flags)
    event.rs              PointerEvent, SessionEvent, KeyboardEvent (neutral enums)
    pointing.rs           ray casting against surfaces (OpenXR, phase 1)
  vr/openvr/              today's code, moved
    mod.rs  (vr.rs init, tables, call!)  overlay.rs(Live)  stage.rs(Live)
    texture.rs (ImportDmabuf, SetOverlayTexture, SetOverlayRaw)
    input.rs (events->PointerEvent, gaze action, haptics)
    keyboard.rs  shell.rs (dashboard tab, manifest, notes)  pointer.rs (cc_pointer lease, laser.rs)
  vr/openxr/              new
    mod.rs (instance, session, frame loop)  layers.rs  vulkan.rs  texture.rs
    input.rs (actions, hand spaces, haptics)  session.rs (events, space changes)
```

Neutral types, which replace the OpenVR ones the core sees today:

- `SurfaceId` replaces `vr::Handle = VROverlayHandle_t`, used as `overlay::Handle` in about 24
  files.
- `Dev = Head | Controller(Ctl) | Mouse`. `Ctl` is the backend's own id for a controller: the
  tracked-device index on OpenVR, one per hand path on OpenXR. `Stage::hand(ctl)` says which hand
  it is right now. The device inventory below is why it isn't `Hand(Left|Right)`: today's code
  tells controllers apart by index, a controller can have no hand role or share one, and narrowing
  would change behaviour in phase 0. The room isn't a `Dev`, so the taskbar's `u32::MAX` becomes
  `Option<Dev>`.
- `Place`, `At` and `apply` stay as they are; they're already API-free (`overlay.rs:48-130`).
  `At::Device` takes a `Dev`.
- `PointerEvent { kind: Enter | Move | Down(btn) | Up(btn) | Scroll{dx, dy, smooth} | Leave, x, y,
  from: Dev, age }`, with x and y in the surface's pointer units from the bottom left, as `m.x,
  m.y` are today, so `geometry::surface_point/from_top` (used in `main.rs:468-470`) stay in the
  core. Enter is there because grab.rs treats FocusEnter like a move, and `smooth` because the
  cards take only discrete scrolls. A surface's queue gives `SurfaceEvent::{Pointer, Keyboard,
  Closed}`, since SteamVR's keyboard events and the tab's close come through the same queue.
- `SessionState` is what the runtime's events say about the session: running, visible and
  focused, three separate states. Worn is a fourth thing from somewhere else (OpenVR's interaction
  events, or `XR_EXT_user_presence`), and never folded in. Whether to draw isn't a session state:
  `begin_frame` returns a `Frame` with `should_render` and `predicted`, the time poses are read for
  (`vr::to_photons` on OpenVR today), and `Stage::device` and `laser` are at that time.
  `predicted` is OpenVR's prediction horizon, seconds from now as an f32. The OpenXR backend keeps
  the runtime's own `predictedDisplayTime` for every locate and every submit in that frame, and
  never rebuilds it from the float.
- `scene_app_running()` returns `Option<bool>`, where `None` means the backend can't tell.

The traits, in `crates/cc-panels/src/backend.rs` (the design: nothing uses it yet), compose into
one `trait Backend`, and `Stage` keeps extending `Overlays`:

1. `Surfaces`: `create`, `create_dashboard` (`None` without a dashboard), `destroy`, `show`,
   `hide`, `visible`, `order`, `order_of`, `alpha`, `color`, `flag`, `input`, `rename`,
   `picture_file`. The flags are the five SteamVR ones the code sets today: interactive, discrete
   scroll, ignore alpha, and the control bar and its close. It takes in `surface::create/destroy`,
   the 22 `SetOverlayFlag` sites, the 20 and 19 show and hide sites and the 12 input-method sites.
2. `Overlays` (exists): width, curvature, texel, bounds, mouse_scale, place_room, place_device.
   `mouse_scale` stays for now: SteamVR's mouse coordinates depend on it, and phase 0 doesn't
   change them. OpenXR's ray casting scales its hits to the same units.
3. `Textures`: `import(&Dmabuf) -> TexHandle`, `present(surface, TexHandle)`, `release(TexHandle)`,
   `present_pixels(surface, &[u8], w, h)` and `clear`. `gpu.rs` keeps the GBM buffers and the
   stale tracking (core); only the `Texture_t`, `ImportDmabuf` and `SetOverlayTexture` parts move.
4. `Pointing`: `next(surface) -> Option<SurfaceEvent>`. It has two contracts, kept apart:
   - **Phase 0, the OpenVR adapter, preserves today's events and the core's recovery.** SteamVR
     does the hit testing, and the adapter translates each `VREvent_t` into one `SurfaceEvent`, in
     order, with the same coordinates (so `main.rs:444`'s logic matches the neutral kind instead
     of `sys::EVREventType_*`). It never makes one up: no Up SteamVR didn't send, no Leave, no
     clamping. Where SteamVR sends no Up (a controller switched off), grab.rs's `LOST` recovery
     stays as it is, and the golden tests pin it. Any behaviour fix is its own branch.
   - **Phase 1, the target both backends meet:**
     - **Capture:** a Down on a surface captures that pointer for that button. Its moves keep
       going to that surface, clamped to its edge, until the Up, even once the ray has left it.
       That's how a drag goes past a panel's edge.
     - **Hover:** a surface gets Enter, then moves while it's hit and nothing has captured that
       pointer. When the hit goes to another surface, the old one gets Leave first.
     - **Scroll:** goes to the hovered surface, or the captured one during a capture.
     - **Endings:** focus lost with a button held, the device lost mid-drag and a surface
       destroyed while it holds a capture all end the capture. The holder, if it still exists,
       gets Up and then Leave, so nothing stays held.

     OpenXR ray-casts the hands' `aim` poses (and the mouse ray) against the visible surfaces,
     takes clicks from action state, and has to meet these itself. Whether the OpenVR adapter
     grows them too is decided once the golden tests show where SteamVR's events differ.
5. `Stage` (exists): `device/laser/head/head_pose/controllers/hand/in_hand/connected/pulse`.
   `controllers()` returns `Vec<Ctl>`, and `pulse` takes a surface, as now (SteamVR buzzes the hand
   whose laser last worked it). `panel` and `move_pointer` aren't the runtime's, so they stay on the
   core's side of today's `Stage`.
6. `Session`: `next_event() -> SessionEvent::{Quit, State(SessionState), Worn(bool), Relocalized,
   RoomChanged(id), DashboardShown(bool), ControllerUsed(ctl)}`, `display_period()`,
   `universe_id() -> Option<u64>`, `scene_app_running() -> Option<bool>`, and
   `begin_frame() -> Frame`/`end_frame()` (no wait on OpenVR). OpenVR compares universes itself and sends
   Relocalized or RoomChanged, as main.rs does now.
7. `Keyboard`: `open(surface, mode, multiline, hint, initial, user) -> bool`, `text()`, `close()`.
   OpenVR wraps `ShowKeyboardForOverlay`, and its events come through `Pointing` as
   `SurfaceEvent::Keyboard`, as SteamVR sends them now. OpenXR returns false, and the core opens
   its own (5.2).
8. `Shell`: `has_dashboard()`, `dashboard_visible()`, `our_tab_active(main)`, `toast(text)`.
   Autostart stays in tab.rs, which runs before any backend.
9. `Input`: `gaze() -> Option<(V3,V3)>`. Haptics stay `Stage::pulse` and "a controller was used"
   is `SessionEvent::ControllerUsed`, since that's how they reach the code today.

### Where the design differs from the first sketch

The reviewed design (`backend.rs`) changed these from the sketch above it, on purpose:

- **`Dev` is `Head | Controller(Ctl) | Mouse`** for phase 0, not `Hand(Left|Right)`, for the
  reasons in the device inventory below.
- **`Overlays` keeps `mouse_scale`** in phase 0. Taking it out is a phase 1 item, when OpenXR's ray
  casting can give pixels directly.
- **`Pointing::next(surface) -> Option<SurfaceEvent>`**, with SteamVR's keyboard events folded
  into the same queue as `SurfaceEvent::Keyboard`, because that's where SteamVR sends them. Our own
  keyboard (phase 2) is a surface of ours, not a runtime's, so it needs its own seam when it comes.
- **Haptics are `Stage::pulse(surface)`, not `Input::haptic(dev, amp, ms)`.** The one haptic call
  today (`stage.rs`, `TriggerLaserMouseHapticVibration`) always uses the same strength and length,
  and no caller varies them. If one ever does, it gets a pulse by device.
- **"A controller was used" is `SessionEvent::ControllerUsed(ctl)`, not `Input::buttons(dev)`,**
  since it comes in as an event today (`VREvent_ButtonPress`).
- **`Shell` is `has_dashboard`, `dashboard_visible`, `our_tab_active` and `toast`**, which are the
  questions control/dashtab.rs and notify.rs ask. Autostart stays in tab.rs.
- **`Surfaces` has `visible`, `order_of`, `rename`, `picture_file` and `create_dashboard`** as
  well, because today's code reads visibility and sort order back (`IsOverlayVisible`,
  `GetOverlaySortOrder`), names overlays, loads icons from files, and makes the dashboard tab.
- **`Stage`'s `panel` and `move_pointer` stay in the core.** `Live` answers them from the panel list
  (`crate::panel(i)`: its overlay, picture size and curve) and sends a move to the panel's session,
  with no SteamVR call in either, so they aren't the runtime's.
- **`SessionState` has no `should_render`,** and `to_photons` is `Frame::predicted` (above).

Some of today's behaviour the golden tests show isn't in any trait, and phase 1 mustn't change it
without saying so:

- **The bottom-right corner stretches about the centre.** A drag on it is `Mode::Stretch` (the
  fixture logs "stretching"), and only the width changes: the panel's matrix stays where it was,
  so the panel grows and shrinks around its middle (`golden_resize`). OpenXR's own ray casting
  has to keep that, not anchor the opposite corner.

### Device inventory (what `Dev` has to cover)

From reading the code on 2026-10-29. Nothing in cc-panels handles a device turning on or off, or
a role changing, as an event: there's no TrackedDeviceActivated, Deactivated or RoleChanged
handler. Everything copes by reading poses and roles each frame.

- **The head** is index 0 everywhere: `vr::head`, `head_predicted`, `head_raw`, `head_worn`, the
  universe id, the worn events (only index 0 counts), and `At::Device(Hmd, ..)` for the HUD and
  grab's head docking.
- **Controllers by index:** laser events carry `trackedDeviceIndex`. grab.rs presses and releases
  by it (`press`, `end_by`), keeps it in `Drag.dev`, and snapshots `Drag.others` for docking when a
  drag starts, without refreshing it. The taskbar, prefs, workspace, machines and plasmabar cards
  make the last controller pressed the primary device (`is_real_controller`, then
  `set_awake(false)`). `plasmabar` and grab keep `released: Vec<u32>`.
- **Controllers by role:** the wrist taskbar asks `GetTrackedDeviceIndexForControllerRole` every
  frame, so it follows a swapped or renumbered controller, and shows "no controller" when the role
  is Invalid. laser.rs asks it to find a free hand for the mouse.
- **Controllers by render-model name:** `vr::tip(hand)` decides the hand from "left" in the model
  name, which is a third way and could disagree with the role.
- **All controllers:** `Stage::controllers()` scans 1 to 63 for connected real controllers, in
  index order, as a set.
- **Not real controllers:** our cc-pointer and other apps' virtual pointers (`*_pointer`
  controller types), trackers and base stations are filtered out by `is_real_controller`, which
  only takes class Controller. cc-pointer can hold a hand role for a moment while the mouse leases
  a hand, so a role lookup can return it. laser.rs finds it by type and rechecks every 2 s.
- **The mouse** is the sentinel index 64 (`grab::MOUSE`), with `MOUSE_RAY` as its pose, and
  control/ui.rs's synthesized events use it too.
- **The room** is `u32::MAX` in the taskbar and plasmabar placements.
- **Tracking lost:** only `bPoseIsValid` and `bDeviceIsConnected` are read, never
  `eTrackingResult`. A drag or a docked panel whose device loses its pose ends after `LOST`; a
  shorter blink survives (tested in grab.rs).
- **Switched off mid-drag:** no Up arrives, so the pose goes invalid and `LOST` ends the drag.
  Back on with a new index, the old drag has already ended, and a new press starts a new one.
- **Renumbered or swapped mid-session:** the role lookups follow it. `Drag.dev`, `Drag.others`
  and a device-relative overlay keep the old index until they're next set.
- **Two controllers with one role:** the role lookup returns one of them. The index paths work
  with both.
- **Traces:** trace.rs records and replays raw indices.

So `Ctl` keeps the index, and every one of these keeps working unchanged in phase 0. Making a hand
the identity is OpenXR's business, where the hand path is all there is.

The modules that move, with the lines that matter:

- `vr.rs`, all of it, into `vr/openvr/mod.rs`. The `call!` macro (`vr.rs:26`) stays private to
  that module.
- `overlay.rs`: `Live` (`:114-153`) to `vr/openvr/overlay.rs`; `Place`, `At` and `apply` stay.
- `stage.rs`: `impl Stage for Live` (`:42-86`) to `vr/openvr/stage.rs`.
- `gpu.rs:196,210,250,323` and `capture.rs:452-538` (ImportDmabuf, UnrefResource,
  SetOverlayTexture) to `Textures`, and `capture.rs:461,500` too.
- `grab.rs`: `pose_of`, `laser_of`, `poses` and `connected` (`:1051` and around it), the card
  event loop (`:1378`) and the raw uploads (`:791,1043`).
- `laser.rs` and `crates/cc-pointer` are OpenVR-only. They stay, built only for the OpenVR
  backend. Their pure maths (`pick_hand`, `to_raw_*`, `lease`) is unit-tested and stays where it
  is.
- `gaze.rs`: `Gaze::new/sample/release` become each backend's `Input::gaze`.
- `osk.rs`: the SteamVR keyboard half (`:72`, `:228`, the event routing at `:282`) goes to
  `Keyboard`. The key strip (sticky Ctrl, Alt and Super) is core.
- `tab.rs` and `control/dashtab.rs` go to `Shell` (OpenVR), and `notify.rs:158-172`'s fallback to
  `Shell::toast`.
- `main.rs`: init (through `vr::init`), the SteamVR event loop (`:1103-1160`) to
  `Session::poll_events` with a handler for the neutral `SessionEvent`, the laser routine
  (`:440-520`) to `Pointing`, and the display frequency (`:1028`) and universe (`:909`) to
  `Session`.
- The callers with 1 to 6 sites each (`back.rs`, `plasmabar.rs`, `taskbar.rs`, `control/*.rs`,
  `control.rs`, `windows.rs`, `attention.rs`) only change `sys::EVREventType_*` and
  `call!(ov, ...)` to the neutral calls.

A test keeps it honest, the same trick as the existing one that reads the sources to check every
overlay goes through `surface::create` (docs/ARCHITECTURE.md, the `surface.rs` row): it fails if
`openvr_sys`, `call!(` or `openxr::` appears outside `vr/openvr/` and `vr/openxr/`. Cargo features
`openvr` and `openxr` are both on by default, and the Frame's package builds with `openvr` only, to
keep `libopenxr_loader` and Vulkan out of the system image. `bin/cc-win-spike.rs` builds
`overlay.rs` and `back.rs` directly (`overlay.rs:7`); it moves onto the OpenVR backend's module
path, with a test that it still builds and runs its overlay setup against the fake.

The core that doesn't change, and never sees either API: `geometry.rs`, `kvm.rs`, `rdp.rs`,
`vnc.rs`, `windows.rs` (window panels), the clipboard, `config.rs` and the layout and workspaces,
`surface.rs`'s registry data (not `create`), `theme.rs`, `assets.rs`, `session.rs`, `capture.rs`'s
PipeWire side, `kwin.rs`, `power.rs`, `handoff.rs`, `crates/cc-proto` and `crates/cc-host`.

## 4. Phases

### Phase 0: refactor behind the boundary, OpenVR unchanged (about 10 days)

The goal is the same behaviour on SteamVR, byte for byte. Today's OpenVR event and capture path
stays as it is: SteamVR still hit-tests the laser and sends the mouse events, and the OpenVR backend
only translates them. Shared ray casting comes with phase 1, not here. Phase 0 adds no dependency to
cc-panels.

1. (1 d) The runtime probe, `tools/xr-probe`, its own crate outside the workspace. Done on the
   Frame (section 1). Monado and WiVRn wait for a PC that has them.
2. (1 d) The design, reviewed before any code moves: the trait definitions and neutral types from
   section 3, the device inventory, and the golden-test list below.
3. (2 d) The `SurfaceId` and `Dev` newtypes, and the mechanical replacement of `u32` device indices
   and `vr::Handle`, with `Dev::Controller(Ctl)` keeping the index. `At::on` stays as the room or
   device switch, taking `Option<Dev>` (None is the room). Done: they live in `ids.rs` (built into
   cc-win-spike too), `vr::dev` and `vr::index` map OpenVR's indices (0 the headset, 64 the mouse),
   and the event loops convert at the edge. Logs and fixtures print devices by index, unchanged.
4. (2 d) The neutral `PointerEvent` and `SessionEvent`, which the OpenVR backend translates to.
   The ten `PollNextOverlayEvent` loops (listed in 2.4) match the neutral enum.
   Done: they live in `events.rs` (built into cc-win-spike too). `vr::surface_event` turns each
   overlay event into one `SurfaceEvent`, and `vr::session_event` turns SteamVR's own events into
   `SessionEvent`s, reading the universe on a relocalization as main.rs did. The loops still poll
   OpenVR themselves (the traits come in step 5), but they translate first and match only the
   neutral kinds. The Extra windows get theirs from `ui::next_event`, the desktop mouse's too.
   Nothing is made up or clamped: events a loop ignored before are still skipped, the logs still
   print devices by index, and every golden fixture is unchanged.
   Two readers take a pointer's x and y on kinds where they aren't a position, as they did
   before: a card's hover on Enter (grab.rs `card_part`) and Plasma's last point for a device
   with no laser pose (plasmabar.rs `laser_point`'s fallback), which a scroll or a focus change
   sets too. Both are candidates for a behaviour branch of their own; until then x and y stay on
   `PointerEvent` for every kind (events.rs says so).
5. (2 d) The `Textures`, `Keyboard`, `Shell` and `Input` traits with OpenVR implementations. Code
   moves; it isn't rewritten.
   Done: each trait moved out of `backend.rs` (which re-exports it) into the file that holds its
   OpenVR side, a `Live` like `overlay::Live`: `texture.rs`, `keyboard.rs` and `shell.rs`, and
   `gaze.rs` for `Input`, whose `Gaze` is that side already. capture.rs no longer touches OpenVR.
   The same calls go out, with the same arguments, in the same order. The changes to the design,
   all so nothing changes:
   - `Textures::present` and `present_pixels`, and `Keyboard::open`, return `Result<(), Refused>`,
     because callers log SteamVR's error. `Refused` looks the name up only when it's printed.
   - `Textures::import` returns an `Option` (ImportDmabuf only says yes or no). A failed import
     leaves no handle: what SteamVR left in it is dropped, so nothing lets go of a handle it was
     never given. That's the one intended edge change.
   - `Keyboard::open` takes the most characters, and `text` hands back SteamVR's 256 bytes.
   - `Shell::toast` takes the surface it hangs off and says why it didn't show, with its own
     `NoteRefused`, since notification errors aren't numbered like overlay errors.
   - `Input` has `pause`, for while a VR game runs (not `release`, which `Textures` has).

   The polling came on its own branch after that (5f), in `poll.rs`: `Pointing` and
   `SessionEvents`, the queue split off `Session` (whose frames, universe and scene app haven't
   moved), with `Live` (the overlays' queues) and `Queue` (SteamVR's own) on the OpenVR side.
   - `SessionEvent` gains `Other(u32)`, the backend's own number for an event nothing acts on, so
     it differs from an empty queue (None). Nothing matches on its value. main.rs's drain counts
     every event and keeps the last one's type (`last_event`, an OpenVR-numbered diagnostic;
     phase 1 gets its own), and does nothing else with `Other`. No match on `SessionEvent` has a
     wildcard.
   - The overlay loops call `poll::Live.next(surface)`, and the taskbar's `trace::Traced`, which
     writes the trace as before. Plasma's loop still reads SteamVR's raw event, since an event
     that isn't a pointer still moves its last point (the two readers above); it moves with that
     fix. tab.rs's loop, which runs before any backend, and cc-win-spike's own queue stay raw too.
6. (1 d) The source-scan test that nothing outside `vr/openvr` touches OpenVR, and the cargo
   features.
7. (1 d) Tests and a pass in the headset.

Each step is its own small branch, and has to pass the same checks before and after:

- The existing unit tests (`overlay::fake::Writes`, `stage::fake::Fake`, the kvm and grab tests),
  untouched.
- The golden tests (below), replaying identically.
- The headset-free harness's replays (docs/vr-harness.md), including recorded traces from the
  headset.
- At the end of the phase, docs/final-test-frame.md in the headset, and the control socket's
  `surfaces` dump compared before and after.

The golden tests come first, before any code moves. Each one writes down, step by step, what
today's code did: the OpenVR calls it made (the headset-free harness's stand-in, `vr::fake`, keeps
a log of them, docs/vr-harness.md), what it sent to the computer (the session's pointer, wheel and
leave, and the keys), and what it was holding (a press, a drag and whether its device is lost). The
transcript has to match a fixture in `crates/cc-panels/golden/`, line for line. The fixtures were
written by the code before the move, and an ordinary run never writes them, a missing one
included: only `CC_GOLDEN_WRITE=1` does, for a change in behaviour that's meant, and the fixture's
diff goes in with that change (`golden.rs`).

Plasma's bar's scripts go through its real event loop and `vr::fake`. grab.rs's go through
`stage::fake::OverFake`: today's `overlay::Live` and `stage::Live` over `vr::fake`, with only the
panel list (`panel`, `move_pointer`, the core's) answered by the test. So the code phase 0 moves is
inside what the fixtures pin, not stood in for. The scripts, with the ones that are in marked:

1. **Carry** (in, `golden_carry`): the bar taken with a controller, carried through three poses,
   pushed away a notch and pulled back two, then let go.
   `golden_carry_reads_poses_at_the_predicted_time` is a carry with SteamVR's vsync running, which
   pins the time poses are asked for (`vr::to_photons`, `Frame::predicted` in the boundary).
2. **Resize** (in, `golden_resize`): the bottom-right corner dragged out, back in past where it
   started, and let go.
3. **Dock** (in, `golden_dock`): a carry held at the other hand through the dwell until it arms
   (the buzz, at its one fixed strength), let go there so it docks, then that hand moves 30 cm and
   the panel goes with it.
4. **Focus lost with a button held** (in, on Plasma's bar): press, then FocusLeave with no Up,
   an empty pass, Enter and a move, then the Up. Today the session gets a leave and the press
   stays held until the Up. Recorded isn't blessed: the fixture flags it (a `FLAG:` line), since if
   SteamVR never sends that Up, the computer keeps the button down. It stays a tracked risk until a
   trace recorded in the headset (press, look away, release) shows whether it does, and a fix, if
   it needs one, is its own branch. A panel's version is still to write.
5. **Device lost mid-drag** (in, grab.rs): a carry whose controller goes untracked for 400 ms and
   comes back, then goes for longer than `LOST` and the panel's put down. Then a controller under
   another index picks it up, the old index's late Up ends nothing, and the new one's ends it.
6. **Surface gone with a capture outstanding** (in, on Plasma's bar): press, then the bar hides as
   it does when its window closes, which lets go of the press where the pointer is. The late Up
   and a stray move then send nothing. A window panel's version is still to write.
7. **Roles swap mid-drag** (in, grab.rs with the wrist taskbar): the right hand picks a panel up,
   then the left and right roles swap. The wrist taskbar asks for the off hand's controller every
   frame, so it moves to the other controller, while the drag keeps the index it started with
   and only that index's Up ends it.
8. **The mouse and a controller** (in, on Plasma's bar,
   `golden_the_mouse_then_a_controller`): the mouse has the pointer, so a laser's move is dropped.
   A press from our virtual pointer (cc_pointer) doesn't take over; a real controller's does, and
   goes through. This covers Plasma's bar only, not main.rs's panel path (`laser`,
   `controller_pressed`): that needs a panel in a test, which there's no way to make yet.
9. **Session** (in, `golden_steamvr_events`): Quit, the dashboard opening and closing, a button on
   a real controller and on our virtual pointer, a relocalization in the same universe and into
   another, and the headset off and on (and a controller's interaction, which doesn't count). The
   handling was inline in main.rs's loop, so the golden was written first against a copy of that
   loop as it stood (`old_loop` in main's tests). Then the handling was pulled out into
   `steamvr_events`, extraction only, and the same events through both write the same fixture.
   This was phase 0's first step.
10. **Recorded traces:** each trace from the headset (docs/vr-harness.md), replayed, including the
   session recorded with Plasma's debug log on, once it's in.

### Phase 1: OpenXR MVP on Monado and WiVRn (about 18 days)

Before it starts, three things get decided and written down: the Vulkan texture contract (2.3),
the layer budget, and the interim room rule.

The layer budget: OpenXR only promises 16 composition layers per frame (`maxLayerCount` can be
more), and a busy desktop can have more overlays than that once the cards, backs, bars, cursor and
HUD are counted. So phase 1 decides which surfaces get a layer each and which share one (drawn
together into one swapchain), what happens past the runtime's limit, and how `Surfaces::order`
becomes the order of the layer array, since OpenXR draws later layers over earlier ones.

The room rule. The room rule: OpenXR has no universe id, so on OpenXR every saved spot belongs to one
default room until there's something better. Candidates for later are a runtime's own map id
(Monado's SLAM, unverified), a persisted spatial anchor (`XR_MSFT_spatial_anchor`,
`XR_FB_spatial_entity`, unverified support), or workspaces the user names. Relocalization on those
runtimes might move the room without telling us.

The frame loop: while a session is running, call `xrWaitFrame`, `xrBeginFrame` and `xrEndFrame`
every frame, and use `shouldRender` to skip the drawing, not the frame. The loop doesn't sleep while
the session runs. Focus is the runtime's input state, and submitting empty frames doesn't guarantee
it. The session state machine (running, visible and focused from the events, should-render with each
frame, all kept apart from worn) is part of the MVP, not phase 2. Taking `mouse_scale` out of
`Overlays` belongs here too, once the ray casting gives pixels directly. What an idle session costs in power, submitting empty frames, gets
measured, not assumed.

What counts as "works":

- An instance, a Vulkan session, and an overlay session when the runtime has `XR_EXTX_overlay`.
  Without it, a scene app only if the user chose desktop mode (3 d).
- The frame loop above, with quad and cylinder layers, sort order, show and hide, and placement in
  STAGE (2 d).
- Vulkan dmabuf import of the RDP, VNC and window panels' buffers, copied into the swapchain when
  dirty. This is the big one, estimated after its spike (2.3).
- The `SetOverlayRaw` equivalent for UI cards (cards, taskbar, back, HUD), through a staging copy
  (2 d).
- Our own ray casting from the `aim` poses, following the `Pointing` rules in section 3: the
  trigger is left click, the thumbstick click is right click, the stick scrolls, and FocusLeave
  works. The mouse is a pointer too (3 d). That also gives drag and resize, which use the trigger
  and the stick and no grip (docs/steamvr-integration.md).
- The mouse ray, and the KVM's engaged and typing-follows-click logic, are already in the core
  (kvm.rs), so a physical keyboard and mouse work on the first run with no keyboard of ours (0 d).
- Hand and head pose reads, and `Stage` for OpenXR (1 d).
- Testing on Monado's simulated HMD on a PC, then WiVRn with a Quest if one's available (2 d).

Not in the MVP: our keyboard, the dashboard replacement, gaze, haptics, room identity beyond the
default room, the docking buzz, notifications.

Done when: a remote monitor panel, a window panel, move, resize, curve and snap, and taskbar clicks
all work, and a relaunch puts the panels back where the user left them in the default room.

### Phase 2: parity (about 15 days, in this order)

1. Our own on-screen keyboard (4 d), see 5.2.
2. A hub panel and a summon gesture, replacing the dashboard tab (2 d), see 5.3.
3. Haptics and button actions, with suggested bindings for Quest Touch, Index, the Vive wand, the
   Frame controller (`XR_VALVE_frame_controller_interaction`, where the runtime has it) and a
   generic profile (2 d).
4. Toasts (0.5 d), the relocalization event (0.5 d), and worn state to pausing (1 d).
5. Room identity: user-named workspaces, and a spatial anchor later (1 d for the manual part).
6. Scene-app detection over Monado's IPC, with WayVR's libmonado use as the reference (1 d).
7. Gaze through `XR_EXT_eye_gaze_interaction` where it's there (1 d).
8. An autostart unit for Monado and WiVRn, and packaging (1 d).
9. Docking to a hand or the head (device-relative through action spaces; only the reanchoring
   maths differs) (1 d).

### Later: other targets

- Index on Linux: the OpenVR path works now. It needs the Index controller profile in the SteamVR
  binding, and the laser tip checked. That's 1 to 2 days and no architecture.
- Quest native: an Android process, with no PipeWire, GBM or KWin, and hardware-decoded video as
  `AHardwareBuffer`. The core is only portable if `kvm`, `rdp` and `vnc`, `geometry` and the layout
  stay free of Linux-only dependencies. Today cc-panels is one crate with `pipewire`,
  `wayland-client` and `gbm` in it (`crates/cc-panels/Cargo.toml`), so splitting a `cc-core` out is
  a follow-up to this plan.
- Windows and macOS servers aren't affected: `cc-host` and `cc-proto` are separate crates.

## 5. What the SteamVR-specific parts become

### 5.1 cc-pointer (crates/cc-pointer, 819 lines in lib.rs, plus laser.rs)

It's a SteamVR driver with a virtual controller (C++ vtables from Rust), so that SteamVR's own laser
can click UI we don't own: the dashboard, Steam's menus, other apps' overlays
(crates/cc-pointer/src/lib.rs:1-20, docs/laser-pointer-design.md). OpenXR has no SteamVR dashboard
or foreign overlay to click, and no driver model, so it means nothing there. It stays as it is,
built and loaded only for the OpenVR backend. On OpenXR the mouse is just another pointer in
`vr/pointing.rs` (`Dev::Mouse`, already modelled as `grab::MOUSE`), hit-tested against our own
surfaces. The mouse can't click another overlay app running next to us on Monado. That's a
limitation we accept.

### 5.2 SteamVR's keyboard (osk.rs, workspace.rs, machines.rs)

OpenXR has no system keyboard, so we draw one: a surface rendered by the same card code as the
taskbar, QWERTY with shift and symbols, on the panel being typed into. It also takes over the text
fields in Machines and Workspace. It could serve SteamVR users later too, but SteamVR's keyboard
stays the default there. People already know it, and it hasn't been tested in the headset yet
anyway (docs/steamvr-integration.md). The key strip with sticky Ctrl, Alt and Super is core
already; it needs to work without SteamVR's keyboard being open.

### 5.3 Dashboard tab and shell

Monado and WiVRn have no dashboard. The Machines, Preferences and Workspace windows already exist
as surfaces, so a summon gesture (a hold on both thumbsticks, say, or a menu-button chord, bound per
interaction profile) puts the hub 0.6 m ahead, the way those windows open now. The tab's close
button becomes the hub's own quit. Some SteamVR-only things just don't exist on OpenXR: the control
bar (the `steamvr_bar` setting), the `.vrmanifest` autostart (packaging/command-center.vrmanifest.in),
and the binding-load budget with its vrserver web API dance.

### 5.4 Bindings

OpenVR doesn't change: the SteamVR action manifest and the user's bumper binding. On OpenXR,
Command Center declares an action set (click, right click, scroll, grab, menu, haptic out, the aim
and grip poses) and suggests bindings for each profile it knows. Users rebind in the runtime's own
tool, if it has one.

## 6. Risks

1. **Overlay support is the ceiling.** If WiVRn lacks `XR_EXTX_overlay`, OpenXR there means desktop
   mode only, and Command Center can't run beside a game. The probe on a PC with WiVRn answers it.
   EXTX extensions are experimental and can change or be dropped.
2. **Vulkan dmabuf import** across Mesa on x86 and aarch64 (Turnip and others), and modifiers (the
   GBM buffers are linear, `gpu.rs:3-4`). Linear is the safe case, but drivers differ. That's why
   the slice gets a spike and a contract before an estimate.
3. **Frame pacing.** Our loop parks and wakes on demand (`vr.rs:79-92`, `Pace`), and OpenXR wants a
   steady wait, begin and end every frame while the session runs. That changes power use, which
   gets measured. The Frame's power saving (power.rs) is OpenVR-only anyway.
4. **Input fidelity.** Our own ray casting has to feel like SteamVR's laser: hover, capture while
   dragging past the edge, scroll. The grab and resize code (grab.rs, about 3200 lines) is built
   around SteamVR's events plus device poses, which is why the `Pointing` rules live in the trait
   and the golden tests cover the endings.
5. **Phase 0 churn.** 25 files and about 210 call sites, and the `u32` device index is the widest
   leak. The golden tests, the replays and small branches are the guard.
6. **Test hardware.** The Frame can't run Monado (no driver that I know of). Monado's simulated HMD
   tests rendering and rays, but not comfort. WiVRn needs a Quest and a Linux PC. Without that
   setup, phase 1 is blind past the simulator.
7. **Room identity** has no OpenXR answer. Saved spots keyed by `cc_proto::conf::room(universe)`
   use the default room, and relocalization on those runtimes might move the room silently.
8. **Scope drift.** Quest-native and Windows clients are different projects (Android, D3D). They
   shouldn't shape the first cut beyond keeping `vr/` and the core separable.

## 7. What isn't checked yet

- Monado and WiVRn weren't probed: neither is installed on the Frame or on this machine.
- Nothing past "advertised" was tried on any runtime, since that needs an instance. SteamVR's
  OpenXR runtime would only matter for desktop mode, which is out of the first cut.
- I didn't read the bodies of grab.rs, taskbar.rs, plasmabar.rs or the control windows beyond the
  call sites, so the phase 0 figures for those callers are estimates.
- `ash`'s version, and whether a runtime accepts swapchain images we copy dmabufs into without
  extra sync, wait for the Vulkan spike. The probe uses the `openxr` crate 0.22.
