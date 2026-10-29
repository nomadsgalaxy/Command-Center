# Testing without the headset

Some bugs only show in the headset: the start-menu lasers not following Kickoff, a press landing
on the wrong chip. This is how cc-panels' event loops run in a test instead, from events recorded
in the headset or written by hand.

## Where the seam is

Everything cc-panels asks SteamVR goes through `call!`, which calls through OpenVR's own function
tables (`vr::Vr`: `ov`, `sys`, `rm`, ...). Those tables are plain structs of function pointers, so
a test can fill them with its own functions. `vr::fake` does that: `install(script)` puts tables
in place of vrserver's, once for the test process, and every `call!` then runs unchanged.

- `PollNextOverlayEvent` hands out the script's events in order.
- `GetDeviceToAbsoluteTrackingPose` gives the script's poses. There's no render model, so
  `laser_pose` returns the pose as it is: a trace records the laser itself.
- Creating an overlay hands out a new handle, and the flag, sort order, mouse scale, input
  method, show and hide calls just succeed.
- Anything else is left empty. Its `call!` panics on the empty slot, which is how a new replay
  finds the next function it needs. Fill it in then, not before.

The `Stage` and `Overlays` traits already fake placement and poses for grab.rs and kvm.rs. What
they didn't cover is the event path, polling and the laser's pose, and that's what this adds.
`fake::install` returns a guard, since there's one SteamVR per process, so fake-SteamVR tests take
turns.

What the loop sends on goes to `windows::SHELL_SENT` in tests (every `shell_mouse` call: its
flags and point), so a test can check where a laser's press landed.

## Recording a trace

With `~/.cache/control-center/vr-trace` there when cc-panels starts (`touch` it, then restart the
Desktop), cc-panels writes `~/.cache/control-center/vr-trace.jsonl` (`trace.rs`), up to 64 MB.
Each line is one JSON object:

- `{"tick":"plasma"}` or `{"tick":"taskbar"}`: an events() pass starting.
- `{"ev":"plasma","type":…,"dev":…,"age":…,"data":[6 numbers],"x":…,"y":…}`: one polled event.
  `data` is the event's union exactly as SteamVR gave it; `x` and `y` are only there to read.
- `{"pose":3,"m":[[…],[…],[…]]}`: a laser's pose as `laser_pose` gave it (`null`: not tracked).
- `{"plasma":{"crop":…,"panel":…,"bounds":…,"world":…,"width":…,"aspect":…,"curve":…,"clickable":…}}`:
  Plasma's overlay, each time tick() places it somewhere new.

Delete the flag file afterwards. The trace has every laser event, which shows where you pointed,
but nothing you typed.

## Replaying one

`Plasma::replay(trace)` (plasmabar.rs, tests only) goes through a trace in order:

- a placement line sets the overlay up the way tick() left it: the crop, bounds, Kvm's placement,
  shown;
- a pose line updates the fake's poses;
- each `plasma` pass loads its events into the fake and runs the real `events()` once, the way
  they were polled.

`a_recorded_trace_replays_through_the_real_event_loop` is the first one, written by hand: Kickoff
open, a laser's move, press and release on an item, and the press lands on the item. A trace from
the headset becomes a test the same way. Put the lines in a test, replay them, and assert on
`SHELL_SENT`.

## The taskbar's loop

`Taskbar::replay(trace, grab, windows)` does the same for the taskbar's `events()`. The trace's
`{"taskbar":{"bar":…,"width":…,"aspect":…,"curve":…,"plasma":[w,h]}}` lines (written when the
frame's placement changes) lay the frame out around Plasma's bar and set Kvm's placement of it.
Its `plasma` lines set Plasma's bar up through `windows`. It needs a `Windows`, and
`Windows::for_test` makes one with no KWin connection, no screen capture and no session
(`Kwin::idle`, `Capture::idle`). `Grab::with_backdrop(n, 0)` makes the grab side without SteamVR.

`a_laser_on_plasmas_bar_through_the_frame_replays_onto_the_panel` is its first test: a laser with
no pose on the frame over Plasma's bar moves, presses and releases on the bar's point.

## Not done yet

- **Chips.** The trace doesn't carry the taskbar's chips, so a replayed frame has Plasma's bar and
  no chips. A chip bug needs the chips' widths in the `taskbar` line.
- **grab.rs and osk.rs**, the same way, after that.
- **CI.** The replay tests are ordinary cc-panels tests, so they run in its CI job, which doesn't
  block a merge yet (docs/ci.md).
