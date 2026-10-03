# Stutter fix plan (investigation 2026-10-02, verified findings)

This is the plan for the stutter in Command Center when several Frame windows are open. I found
two separate stutters, and neither one causes the other:

- **When windows open:** the cc-panels main loop stalls for 382-645 ms. Most of that time it's
  waiting for a CPU, because it runs at nice 10. On top of that, it redraws the taskbar 3-4 times
  and redraws cards.
- **All the time:** the input thread holds the KVM lock for 70-110 ms every 2.1 s.

Both stalls freeze panel content, the cursor, the laser and any panel you're carrying. They don't
freeze SteamVR's own compositing. I haven't measured the GPU side, but there's a new hint: the
GPU's clock-scaling stats show it at its top speed, 903 MHz, about 99% of the time. So it doesn't
have much headroom.

When I wrote this, another build had uncommitted edits to `grab.rs` and `taskbar.rs`. Fixes 5 and
6 touch those files, so they were meant to wait until that build landed.

## Fix 0: log slow ticks (do this first, about 15 lines)

- **Files:** `crates/cc-panels/src/main.rs`.
- **Change:** time each phase of a tick with `Instant`: the event poll, the uploads, the
  `KVM.lock()` wait, grab (cards and paint_order), taskbar, plasmabar, windows and kvm. When the
  whole tick takes over 20 ms, write one line such as
  `slow tick 412ms: kvm-lock 0 taskbar 31 grab 18 windows 6 …`. Also time `ImportDmabuf` and
  `SetOverlayRaw`, and log any call over 5 ms.
- **Effect:** none on its own. It's how every fix below gets checked, and it measures the three
  costs nobody has timed yet: imports, SetOverlayRaw and card redraws.
- **Risk:** none.
- **Verify:** open 3 windows and run `grep 'slow tick' ~/.cache/control-center/cc-panels.log`.

## Fix 1: stop running the main loop at nice 10 (one or two lines, the biggest win for the reported stutter)

- **Files:** at the time, `~/control-center/cc-box` line 22, the `~/control-center/cc-panels`
  wrapper line 57, and the RDP thread's start code. Both scripts are links to cc-home now; the
  nice handling lives in crates/cc-home/src/session.rs.
- **Change:** `cc-box` ran everything, cc-panels included, under `exec nice -n 10 …`. Make that
  `nice -n "${CC_NICE:-10}"` so builds stay gentle, and have the cc-panels wrapper call it with
  `CC_NICE=0`. To keep remote-screen decoding gentle too, the RDP threads lower their own
  priority when they start: `libc::setpriority(PRIO_PROCESS, gettid(), 10)`.
- **Expected effect:**
  - In the stalls I measured during opens, most of the time went to waiting for a CPU:
    435 ms = 54 ms running + 371 ms waiting, and 223 ms = 43 + 165.
  - Stalls during opens should drop from 382-645 ms to roughly the running part, about
    50-100 ms.
  - How much it helps depends on load. Load was about 35, partly from a cargo build at nice 10.
    The other competitors are XRService, vrcompositor, kwin and vivaldi.
- **Risk:** low. The main thread uses about 7.6% of one core. FreeRDP decoding (about half a
  core) stays at nice 10.
- **Verify:**
  - `ps -o pid,ni,comm -C cc-panels` should show the real binary at NI 0. When I wrote this,
    pid 694902 was at 10.
  - `ps -L -o tid,ni,comm -p <pid>` should show the decode threads at 10.
  - Run `tools/stall3.sh <pid> 30` while opening 3-4 windows quickly. Before the fix the max gap
    was 382-645 ms; the target is under about 100 ms.
  - In the headset, drag a panel or sweep the laser while windows open. The freezes should be
    much shorter.

## Fix 2: the KVM rescan holds the lock (about 10 lines, removes the steady 2.1 s hitch)

- **Files:** `crates/cc-panels/src/kvm.rs`: `input_loop` around lines 936-941 and `rescan()` at
  line 857.
- **Change:**
  - Only rescan when `/dev/input`'s modification time changes. Adding or removing nodes bumps it.
  - Keep a `HashSet<PathBuf>` of rejected paths so they're never opened again.
  - Open the nodes and run `our_kind` without holding `KVM.lock()`. Only take the lock to push
    the accepted devices.
  - Most of the cost is `close()`, which calls `evdev_release` and then `synchronize_rcu`. So
    skipping nodes we already rejected is what saves the time.
- **Expected effect:** the 70-110 ms freeze every 2.1-2.2 s goes away. The idle max gap should
  fall from 103 ms to about 40 ms or less, and the idle p99 from 40 ms to under about 20 ms. A
  hotplug costs one ~100 ms scan, outside the lock.
- **Risk:** low. Watch for keyboards or mice plugged in later. The modification-time check
  catches new nodes, and a reused path gets its rejected entry cleared when the time changes.
- **Verify:**
  - Run tools/stall3.sh for 30 s while idle. There should be no ~70-110 ms `futex_do_wait` runs
    about 2.1 s apart.
  - Run `v_corr.py` (a scratch sampler from the investigation). The input thread should no
    longer enter D state at `synchronize_rcu_normal` every 2 s.
  - In the headset, the cursor's small hitch every 2 s should be gone. Plug in or re-pair the
    K250 keyboard and type to confirm it still works.

## Fix 3: keep the main thread off the A520 cores (one call, effect not proven)

- **Files:** `main.rs`, at startup, after the worker threads are spawned.
- **Change:** `sched_setaffinity(0, {2,3,4})` for the main thread only.
  - The whole user manager is limited to cpus 0-4, and so is vrcompositor.
  - cpu0-1 are the A520s (capacity 216). cpu2-4 are A720s (capacity 855).
- **Expected effect:** when the main thread happened to land on an A520, a taskbar redraw took
  59-158 ms; on an A720 it takes 9.5-32 ms. paint_order with 19 panels goes from 14.7 ms to about
  3 ms. Nobody has shown how often it actually lands there, though.
- **Risk:** low. It competes for cpu2-4 with vrcompositor, which is also limited to 0-4.
- **Verify:** add `psr` (field 39 of `/proc/<pid>/task/<tid>/stat`) to tools/stall3.sh's output,
  and check that no slow tick runs on cpu0-1.

## Fix 4: stop streaming panels that are hidden or out of view, and cap stream frame rates (about 20 lines, GPU)

- **Files:** `capture.rs` (`format()`, and a new `Cmd::Active`) and `windows.rs` (the away/HIDDEN
  paths around line 893).
- **Change:**
  - **(a) Pause:** call `stream.set_active(false)` while a panel is away (theater), HIDDEN, or out
    of view for more than 1 s, with hysteresis. KWin then stops rendering it off-screen and stops
    listening for damage. The buffers stay negotiated, so nothing gets imported again.
  - **(b) Cap:** add `VideoMaxFramerate`: 30 for window panels, 60 for the theater panel, 10-15
    for the Plasma bar stream.
- **Expected effect:**
  - One 2560x1440 window that keeps updating costs KWin about 2.3-2.9 ms per frame. That's about
    50 ms of GPU per second at 17-18 fps (Vivaldi, live), and up to about 150 ms/s (15%) at
    60 fps.
  - A paused stream costs 0.
  - The 30 cap halves the cost for 60 fps content.
  - It only helps windows whose content is updating. Idle windows send no frames anyway.
- **Risk:** medium. Leaving theater means one renegotiation and 4 imports, and I haven't tested
  KWin's pause/resume path here.
- **Verify:**
  - `pw-top` shows paused nodes doing no work, and cc-panels' status line `(N in)` drops to 0 for
    hidden panels.
  - In the headset, play a video in a window, enter theater on another window, or turn around.
    There should be less headset judder.

## Fix 5: card redraws (small first, then bigger)

- **Files:** `grab.rs`. Wait for the other build to land.
- **Change:**
  - **(a)** Skip the full Rest redraw when the card can't be seen.
  - **(b)** While a card is invisible, hold look-change redraws until it's visible again.
  - **(c)** Then move card drawing to a worker thread, or split the per-pixel loop across cores.
- **Expected effect:** this removes most of what Fixes 1-2 leave in the stalls during opens. It
  renders up to 1.5 MP per pixel on the main thread. The live cost per redraw is unmeasured;
  Fix 0 gives it.
- **Risk:** (a) and (b) are low. (c) is medium, because the textures have to go back to the main
  thread for SetOverlayRaw.
- **Verify:** the `grab` term in the `slow tick` lines drops, and so does the max gap
  tools/stall3.sh shows during opens.

## Fix 6: taskbar redraws

- **Files:** `taskbar.rs`. Wait for the other build to land.
- **Change:**
  - Cheapest first: merge full redraws that come within about 100 ms of each other. A window open
    changes the key 3-4 times: the chip appears, it goes live, its tag arrives, and Plasma's panel
    width changes.
  - Then redraw only the changed chip's `x0..x1` when hover, lit, dim or the label changes, and
    keep the full redraw for when the layout key changes: chip count, plasma_size, scale or theme.
- **Expected effect:**
  - Per window open on an A720: from 3-4 full redraws (about 30-60 ms) to 1-2 (10-30 ms).
  - A laser sweep on an A720: from 9.5-32 ms per chip boundary to about 1-3 ms. On an A520 it was
    59-158 ms.
  - SetOverlayRaw for the bar texture (up to about 1920x159x4 bytes) isn't measured yet; Fix 0
    measures it.
- **Risk:** low to medium (repainting only part of the bar).
- **Verify:** the `taskbar` term in the `slow tick` lines. In the headset, sweep the laser along
  the chips and watch the cursor stay smooth.

## Fix 7: smaller items (each 15 ms or less)

- **Pace the loop:** wait until the next frame deadline at the display's refresh rate (from the
  HMD's display frequency) instead of `sleep(11ms)` at `main.rs:634`. That fixes the steady beat
  in cursor and drag movement.
- **Skip hidden RDP uploads:** in `gpu.rs` `Buffers::upload`, skip the copy when the overlay is
  hidden or minimized. desk-wide was copying 16-19 frames/s while its status said `visible 0`.
  The copy is 8.6% of samples, and it holds `p.lock`, which blocks the RDP thread too.
- **paint_order:** compute `margin()` and `chrome()` once per panel instead of 684 times.
- **assets.py** (assets.rs now): write only the tag that was asked for, and stop rewriting
  `cursor.png` and `glyphs.rgba`.
- **The pointer embedded in window streams:** `stream_window(uuid, true)` makes KWin re-render
  the whole window on every pointer move over it, and that cost grows with the number of windows.
  Switching to cursor metadata means cc-panels has to draw the cursor itself, so it's a bigger
  change; do it later.
- **The Plasma bar:** use a fixed-size region stream or the metadata cursor mode. Low weight,
  since it doesn't grow with the number of windows.
- **Not worth doing:** turning off FreeRDP's `WITH_VERBOSE_WINPR_ASSERT` build option (it costs
  one branch per check), and paint_order's ray tests at today's panel count (about 0.6 ms per
  run).

## Still unknown, and the cheapest way to find out

1. **Does the whole headset judder, or only the panels and cursor?** Turn your
   head while windows open. If the room or background judders, it's the GPU or the compositor;
   if only the panels and cursor hitch, it's the cc-panels loop. That costs nothing.
2. **How long do ImportDmabuf, SetOverlayRaw and card redraws take?** Fix 0's timing lines, with
   no OpenVR tool needed.
3. **How busy is the GPU, and how much of that is KWin?** The GPU's clock-scaling stats in
   `/sys/class/devfreq/3d00000.gpu/trans_stat` show almost all its time at 903 MHz, which
   suggests it's busy most of the time, but that file can't split the load by process. A one-time
   `sudo` read of kwin_wayland's `/proc/<pid>/fdinfo` (look for `drm-engine-*`), before and after
   opening windows, gives KWin's share. vrcompositor's frame-timing log in
   `~/.local/share/Steam/logs` may show dropped frames during opens.
4. **How much of Fix 1's gain depends on load?** Re-run tools/stall3.sh with no cargo build
   running.
5. **How often does the main thread run on an A520?** Add the `psr` column to tools/stall3.sh.

The stall sampler from this investigation is in the repo now as `tools/stall3.sh`
([tools/README.md](../tools/README.md)). Nothing in `~/control-center` was edited or committed
during the investigation, no OpenVR program was run, and nothing was signalled.
