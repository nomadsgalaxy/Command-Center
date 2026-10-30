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

On the thread that holds the fake's guard (`vr::fake::here()`), `shell_mouse` puts its flags and
point on `windows::SHELL_SENT` and returns: nothing goes to a session or fake input. Any other test
takes the real path, so ordinary unit tests still run the real code (with no session, it does
nothing). Once the fake is in, `Windows::start`, `Kwin::start` and
`Capture::start` panic, so a replay can't connect to KWin, start capture or a session, or send
real input (a test checks all four).

The session's other inputs a replay can reach (`wheel`, `leave`, `shell_outside`) do the same
onto `windows::SHELL_OTHER`, by name. The rest of what the taskbar's and Plasma's `events()` can
reach stays inside cc-panels: Kvm's state (`set_awake`, `type_to_shell`, which only lets go of
keys it's holding, and a replay holds none), grab.rs's holds, and Plasma's own presses. A chip's
click could do more (the power chip closes the Desktop), but a replayed frame has no chips.

`trace::fake_vr()` is what a replay test calls. It installs the fake with an empty script, clears
what the last replay left (Plasma's crop, Kvm's placements of both bars, `SHELL_SENT`), and
returns the guard. `VR` is a `OnceLock` and no test ever calls `vr::init`, which needs vrserver,
so the fake is the only thing set in it. Its tables are leaked and only reach the script through
`vr::fake::SCRIPT`, a static, so nothing a late poll touches is ever freed. Each install swaps the
script in under that lock while holding the guard, so a test never sees the last one's events or
poses (`each_install_gets_its_own_script`).

## Recording a trace

With `~/.cache/control-center/vr-trace` there when cc-panels starts (`touch` it, then restart the
Desktop), cc-panels writes a new `~/.cache/control-center/vr-trace-<epoch>-<pid>.jsonl` each time it
starts (`trace.rs`), so a restart never overwrites the last one, up to 64 MB. The log says which
file. It's made 0600, private to you, like `viewers.conf`.
Each line is one JSON object:

- `{"tick":"plasma","t":12.5,"dropped":0}` (or `"taskbar"`): an events() pass starting, with the
  seconds since the trace began and how many events were left out so far.
- `{"ev":"plasma","type":300,"dev":3,"age":0,"x":…,"y":…,"button":…,"cursor":…}`: a pointer move,
  press or release. A scroll has `dx`, `dy`, `scale` and `cursor` instead, and focus coming or
  going has nothing past `age` (SteamVR's own age of the event).
- `{"pose":3,"m":[[…],[…],[…]]}`: a laser's pose as `laser_pose` gave it (`null`: not tracked).
  That's the laser itself, the render model's tip offset included, and the fake has no render
  model, so `laser_pose` hands it back unchanged in a replay (`a_replayed_pose_is_the_laser_it_recorded`).
- `{"plasma":{"crop":…,"panel":…,"bounds":…,"world":…,"width":…,"aspect":…,"curve":…,"clickable":…}}`:
  Plasma's overlay, each time tick() places it somewhere new.

Only the events a replay needs go in, with only their own fields: pointer moves, presses and
releases, focus coming and going, and scrolls. Anything else, keyboard events included, is left
out and only counted. Even so, a trace is a record of what you did with the lasers and where you
pointed, so treat it as sensitive: it's only written when you ask for it, it stays on the Frame,
and you should delete the flag file and every `vr-trace-*.jsonl` when you're done, since each start
makes a new one.

It's written through a 1 MB buffer flushed at most once a second, so the main loop writes one block
a second, not one a pass, and at 64 MB it stops with one line in the log. The writes are still on
the main loop, so until that's been measured, treat frame timing with a trace on as possibly a
little off. The buffer's also flushed however cc-panels ends: a normal quit, closing
the Desktop, SteamVR's Quit, SIGTERM or SIGINT, a panic, and the exits after a drawing thread's
panic. All of that is best effort. A SIGKILL or a power cut loses what's still buffered. If a write
or a flush fails (a full disk, say), the log says so once, and the trace is missing lines from there
on. And the flush on the way out (`trace::flush`) doesn't wait for the trace's lock, so if another
thread holds it right then, the last pass is lost too, however cc-panels is ending. A panic while the lock's held doesn't stop the trace: every lock of it
takes it anyway.

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

A trace whose last line was cut off (the cap, or a crash mid-write) still replays: a line that
isn't whole JSON is skipped.

**Time.** Plasma's and the taskbar's `events()` don't read the clock, except one place: the power
chip's second click has to come within its arm time (`press_power`), and a replay has no chips.
Each event's own `age` is replayed as recorded. Anything later that times holds or taps from
`Instant::now()` wouldn't replay faithfully, and would need the trace's `t` fed in as a clock.

## The taskbar's loop

`Taskbar::replay(trace, grab, windows)` does the same for the taskbar's `events()`. The trace's
`{"taskbar":{"bar":…,"width":…,"aspect":…,"curve":…,"plasma":[w,h]}}` lines (written when the
frame's placement changes) lay the frame out around Plasma's bar and set Kvm's placement of it.
Its `plasma` lines set Plasma's bar up through `windows`. It needs a `Windows`, and
`Windows::for_test` makes one with no KWin connection, no screen capture and no session
(`Kwin::idle`, `Capture::idle`). `Grab::with_backdrop(n, 0)` makes the grab side without SteamVR.

`a_laser_on_plasmas_bar_through_the_frame_replays_onto_the_panel` is its first test: a laser with
no pose on the frame over Plasma's bar moves, presses and releases on the bar's point.

## Golden tests

A golden test (`golden.rs`) runs a script through the same stand-ins and writes down what came out
at each step: the OpenVR calls `vr::fake` logged (`take_calls`), what went to the session
(`SHELL_SENT`, `SHELL_OTHER`), and what the code is holding. The transcript is compared with a
committed file in `crates/cc-panels/golden/`, and any difference fails the test. They're there so
phase 0 of the OpenXR plan (docs/openxr-plan.md) can move code without changing behaviour.

grab.rs's golden tests use `stage::fake::OverFake`, which is today's `overlay::Live` and
`stage::Live` over the fake, with only the panel list answered by the test. Devices go in the
fake's script with `stage::fake::put(dev, pose, off)`: a pose is tracked, none is on but untracked,
and `off` is switched off. A controller type (`types`, for cc_pointer) and hand roles (`roles`) go
in the script too.

An ordinary run never writes a fixture, not even a missing one. When a change in behaviour is
meant, rewrite them on purpose and commit the diff with the change:

```
CC_GOLDEN_WRITE=1 cargo test -p cc-panels golden
```

Numbers are written to 5 decimals with no `-0`, so the last bits of a float, which can differ
between x86_64 and aarch64, don't count, and anything from an unordered collection is sorted
before it's written. The stand-in has no vsync unless a script gives it one (`vsync`: seconds since vsync, the display's
Hz and vsync to photons), so `to_photons` is usually 0 and poses are read at now. The fake's poses
don't move with the time asked for, but it records each time (`pose_times`), and
`golden_carry_reads_poses_at_the_predicted_time` pins it.

Recorded isn't blessed. A fixture pins what the code does today, and where that may be a bug, the
test writes a `FLAG:` line into it, so the fixture says so and a fix shows as that line going.

## Not done yet

- **Chips.** The trace doesn't carry the taskbar's chips, so a replayed frame has Plasma's bar and
  no chips. A chip bug needs the chips' widths in the `taskbar` line.
- **grab.rs and osk.rs**, the same way, after that.
- **CI.** The replay tests are ordinary cc-panels tests, so they run in its CI job, which doesn't
  block a merge yet (docs/ci.md).
