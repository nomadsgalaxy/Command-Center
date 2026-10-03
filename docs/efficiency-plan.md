# Efficiency plan (audit 2026-10-02, measured)

This is Command Center's power plan for the Steam Frame: the ranked list from the power audit I
ran on 2026-10-02. Everything in it was measured on the Frame unless it says otherwise.

## The honest frame first

The short version: the Frame's battery can't do "a few hours" on its own, whatever Command Center
does. Here are the numbers behind that.

- **Current draw.** The headset draws about 19.5-20 W. I measured that on the VPH rail and it
  matches the battery-side upower readings. The battery holds 21.9 Wh (2824 mAh at 7.76 V), so
  at that rate it lasts about 1.1 h.
- **SteamVR plus the platform alone** draw about 12.5-14.5 W, which works out to about
  1.5-1.75 h. So even if CC cost 0 W, the internal battery could never reach "a few hours". 3 h
  would need an average of 7.3 W or less.
- **CC's own share** (cc-panels plus cc-desktop) is about 0.5-1.5 W. Treat that as a lower bound:
  I couldn't measure KWin's GPU use or CC's share of vrcompositor's overlay compositing, so the
  real figure may be closer to 2 W.
- **Target.**
  - CC adds at most 0.5 W over SteamVR alone when panels are visible and mostly static.
  - At most 0.2 W when every panel is hidden or the headset is off your head.
  - At most 1 W while a remote screen plays video.
  - Runtime: 21.9 / (13.5 + 0.5) gives about 1.5 h on the internal battery. A 45 W+ USB-PD power
    bank of about 74 Wh (20 Ah) at about 85% efficiency adds about 4.5 h, so **about 6 h in
    total**. The Frame already took 15 V/3 A over PD and still gained charge while drawing 20 W.
- **"A few hours" is a power-source decision (item 0). The CC work is about not eating into it.**
  All the CC code items together save about 0.3-0.8 W (2-4% of system draw), plus some indirect
  savings in heat and throttling that I haven't measured.

## Ranked items

I converted CPU percentages to watts at about 0.64-0.7 W per busy core. That rate came from a
saturated system, so read the watt figures as rough guides, not measurements.

**0. Power source and habits (not CC code). The biggest lever, and no code.**
- **Change:**
  - For long sessions, use a 45 W+ PD power bank.
  - On battery, don't build or run agents on the Frame (about 1-3 W, 170-490% CPU). Build on the
    desktop and copy the binary over.
  - Close any Steam UI page that's playing video. Its steamwebhelper "vpx tile worker" threads
    run at about 100% CPU, an estimated 0.7-1 W.
  - Turn the backlight down from 257/459. I haven't measured that saving, but the backlight is
    usually one of the biggest loads.
- **Saving:** 2-4 W without the pack. With the pack, about 6 h total.
- **UX cost:** a cable or pack on the strap, and a different workflow.
- **Verify:** compare ccpower.sh rows (below) with and without a build running, and with and
  without the Steam page open.

**1. Pause streams nobody can see.**
- **Files:**
  - crates/cc-panels/src/main.rs (the PollNextEvent loop)
  - rdp.rs
  - gpu.rs `upload()` (around line 100)
  - capture.rs
  - grab.rs:983-993 and control.rs:43 (the existing HideOverlay sites)
- **Change:**
  - Add `visible(p) = !HIDDEN && !p.away() && on_head && !game`.
  - Take on_head from VREvent 103/104 (TrackedDeviceUserInteractionEnded/Started) on device 0.
  - Take the game state from the taskbar's existing GetCurrentSceneProcessId check.
  - When visible flips, call the RDP `update->SuppressOutput(ctx, allow, &full_rect)`.
    pSuppressOutput is in update.h:183, and FreeRDP_SuppressOutput already defaults to TRUE.
  - For window slots, call `pw_stream_set_active(false/true)` in capture.rs.
  - Skip `upload()` when the panel isn't visible. That part is trivial, so it can ship before the
    rest.
  - Check that krdp on :3400/:3401 honours Suppress Output. If it doesn't, close the session
    after 60 s hidden and reconnect on show, with the reconnect loop we already have.
- **Saving:**
  - Two hidden desktops: about 12-14% of a core (about 0.08-0.1 W), plus about 135 KB/s of Wi-Fi
    receive, plus KWin's screencast work for the paused window streams.
  - Headset off, or a game in front: about 20-25% of a core (about 0.15-0.2 W), and more if KWin
    stops compositing those streams.
- **UX cost:** about 50-100 ms to the first frame when a panel shows again, or 1-2 s with the
  reconnect fallback. You won't notice the off-head pauses.
- **Verify:**
  - The status line should show `frames/s` at 0 for panels with `visible 0`.
  - `ss -ti` on :3400 should show bytes_received flat.
  - Run ccpower.sh for 5 min with 2 panels hidden and for 5 min off-head, before and after.
    The ccpanels_core column should drop by at least 0.10.

**2. The decode pipeline. The biggest steady-state saving while remotes change, done in stages,
cheapest first.**
- **2a. One line, do it now.** In rdp.rs's settings block, set
  `FreeRDP_ThreadingFlags = THREADING_FLAGS_DISABLE_THREADS` (0x1). Otherwise FreeRDP's
  yuv.c:234-237 runs an 8-thread WinPR pool; gdi/gfx.c:2035 reads the flag.
  - **Saving:** I measured 3.5-4 ms of CPU less per 3840x1080 frame (9.0-9.5 ms with the pool,
    4.6-5.8 ms serial). That's about 7% of a core at 18.8 frames/s, about 2% at 5 frames/s, and
    about 650 fewer wakes/s. Roughly 0.02-0.05 W.
  - **UX cost:** about 5 ms more latency per full 3840x1080 update on a big core, about 12 ms if
    the thread lands on the little cpu0.
  - **Verify:** `tools/pthr.sh <pid> 30` should show the 8 pool threads gone, and the status line
    should still show about the same frames/s on desk-wide while its content changes.
- **2b. Hardware H.264 decode on qcom-iris (/dev/video22).** Patch the bundled
  panels/third_party/FreeRDP/libfreerdp/codec/h264_ffmpeg.c:
  - Line 1092: behind an env toggle, use `avcodec_find_decoder_by_name("h264_v4l2m2m")` with
    AV_CODEC_FLAG_LOW_DELAY, and fall back to software if opening it fails.
  - Line 618: replace the "EAGAIN means no frame" handling with a bounded receive wait of about
    30 ms that uses poll() on the device instead of a 250 µs spin.
  - Convert NV12 to the 3-plane I420 the code expects (data[2] is NULL for NV12 today), or add an
    NV12 branch to prims.
  - **Saving:** decode CPU drops 2.7-7x (measured). That's about 5-7% of a core at the load I
    sampled, and more during video or scrolling. Roughly 0.05-0.1 W, on fixed-function silicon
    that's more efficient anyway.
  - **UX cost:** 0.5-1.2 s longer to the first frame on each connect or resize, and 1-4 ms per
    frame.
  - **Verify:** `perf record -e cpu-clock:u -p <pid>` for 10 s should show libavcodec falling from
    about 28-34% of samples to near 0. Also run ccpower.sh with a looping video on one remote,
    A/B for 5 min each.
- **2c. The end state.** Intercept the RDPGFX AVC420 SurfaceCommand in cc-panels, decode it with
  V4L2, VIDIOC_EXPBUF the capture buffer and ImportDmabuf it. Try iris's AB24 (RGBA) capture
  first, because it might need no GPU pass at all. Otherwise it needs a GL NV12-to-RGB pass, and
  since cc-panels has no GL today, that means adding a whole stack.
  - **Engineering cost:** AVC420 regions, the 1088-line padding crop, buffer lifetime while
    SteamVR still holds the texture, and a CPU path for the GFX commands that aren't H.264.
  - **Saving:** about 80% of cc-panels' CPU while remotes change, about 0.2 of a core. Roughly
    0.15-0.3 W, plus less DDR traffic. That figure includes 2a and 2b.
  - **UX cost:** none expected, but I haven't measured the latency iris's buffering adds.
  - **Verify:** the perf split (neon_YUV420ToX, libavcodec and memcpy all near 0), measured
    glass-to-glass latency, and ccpower.sh in the video scene.
  - **Only do 2c if** ccpower.sh after items 1-4 still shows cc-panels above about 0.1 core in
    normal use.

**3. Adaptive main-loop tick.**
- **Files:** main.rs:634. It sleeps a fixed 11 ms and does about 0.85 ms of work per tick
  (Grab::update is about 36% of the thread).
- **Change:**
  - Keep 11 ms while a laser is near, a grab, drag or animation is running, the pointer is awake,
    or any panel is dirty.
  - Otherwise tick at 33-50 ms, using the KVM "pointer asleep" and "idle 30 s" states that
    already exist.
  - Snap back to 11 ms on any VR event or KVM input.
  - Leave the card-redraw path to the stutter investigation ([stutter-plan.md](stutter-plan.md)).
- **Saving:** the main thread drops from about 7.7% of a core and 253 wakes/s to about 2-3%:
  about 5% of a core and about 160 wakes/s saved. Roughly 0.03-0.05 W.
- **UX cost:** up to about 22-40 ms on the first hover after idle.
- **Verify:** ccpower.sh with the pointer asleep for 5 min. ccpanels_wakes_s should drop by at
  least 150, and the per-thread sampler should show the main thread under 3%.

**4. Only sample gaze when gaze_lock is on.**
- **Files:** main.rs:629-632 calls `g.sample()` every tick. In gaze.rs that's UpdateActionState
  plus GetEyeTrackingDataRelativeToNow.
- **Change:** `if k.gaze_lock { g.sample() }`. The result is only read at kvm.rs:463/492, behind
  gaze_lock, which is off by default. The action manifest only has the gaze set, so nothing else
  depends on the call.
- **Saving:** about 180 IPC calls/s and about 1% of a core. The eye tracker itself keeps running,
  because it's SteamVR's (ETComputeThread starts before CC does).
- **UX cost:** the status line shows "looking at -" while the lock is off.
- **Verify:** perf on the main thread should show Gaze::sample and Kvm::gaze (12.8% of the
  thread) gone.

**5. Event-driven helper threads.**
- **Files and changes:**
  - kwin.rs:46: replace the `Timer::after(20ms)` loop of up to 500 passes with an
    `event_listener::Event` (5.4.2 is already in Cargo.lock through zbus), notified in
    Kwin::send. `next()` awaits it or a 10 s timer.
  - laser.rs:294: park on a Condvar while SNAP.hand is None and HIDE has been sent. Keep the
    20 ms period while leasing, because the driver's watchdog needs it.
  - rdp.rs:302/328: raise the 100 ms timeouts to 1000 ms.
  - kvm.rs:942: a 500 ms poll, or a deadline taken from the next k.tick, and rescan /dev/input
    every 10 s or on inotify.
- **Saving:** about 250-300 wakes/s (async-io plus zbus about 150-220, the lease about 50, RDP
  about 30 down to 10-15). Under 1.5% of a core. The watts are tiny; the real benefit is that the
  cores and the cluster wake less often.
- **UX cost:** none. Shutdown can take up to 1 s longer.
- **Verify:** tools/pthr.sh over 30 s should show async-io, zbus and the lease thread near
  0 wakes/s at idle.

**6. Config for the cc-desktop session.**
- **Files:** at the time, session/cc-desktop, next to its kwriteconfig6 lines. session/cc-desktop
  is a link to cc-home now, and those settings live in crates/cc-home/src/session.rs.
- **Change:**
  - `kdeglobals [KDE] AnimationDurationFactor=0`.
  - `kwinrc [Plugins] blurEnabled=false`, `contrastEnabled=false`, `slidingpopupsEnabled=false`.
  - `baloofilerc [Basic Settings] Indexing-Enabled=false`. Unlike the main session's, the CC
    session's baloo didn't exclude a network drive mount, and it shared the LMDB index with
    another session's indexer at the time.
  - Drop the DiscoverNotifier autostart.
- **Saving:** about 0 at idle, since the wallpaper is static. It cuts animation and blur frames
  while you interact, roughly tens of mW (unmeasured). It also removes the risk of a Drive crawl
  and of index contention.
- **UX cost:** popups appear instantly with no blur, and there's no file search inside
  cc-desktop.
- **Verify:** watch the cc-desktop service's CPU while opening and closing menus for 60 s, and
  check that baloo_file's /proc/<pid>/io stays flat. The change takes effect on your next
  cc-desktop start, not on a restart an agent triggers.

**Deferred:** a lower refresh rate (say 30 Hz) on the virtual outputs. KWin 6.2 has no knob for
it and kwinoutputconfig.json pins 60 Hz, so it would need a patch to KWin's virtual backend.

## Suggested order

1. 2a, 4 and the "skip upload" part of 1. They're one-liners and can ship together.
2. The rest of item 1.
3. Items 3 and 5.
4. Item 6.
5. 2b.
6. 2c, only if the measurements still justify it.

## Measurement script: tools/ccpower.sh (read-only, tested)

I wrote it during the audit, and it lives in the repo now as `tools/ccpower.sh`
([tools/README.md](../tools/README.md) has the current usage).

- **Usage:** `ccpower.sh LABEL [SECS=300]`. It appends one row to ~/ccpower.csv (set
  CCPOWER_OUT to write somewhere else).
- **What it records:**
  - Mean VPH, CPU-cluster (apc0+1+2) and GPU rail watts.
  - Battery-side watts (|I|·V from max1720x).
  - cc-panels' cores and voluntary wakes/s, counted by PID. The binary runs in the cc-box podman
    cgroup, which builds share, so cgroup accounting would be wrong.
  - cc-desktop.service's cores, from its cgroup's cpu.stat.
  - CPU temperature, fan speed, battery status and charge level.
- **Smoke test:** VPH 20.08 W, cc-panels 0.152 core at 430 wakes/s, cc-desktop 0.160 core.
- **Protocol:**
  - Unplug if you can, since charging adds heat and changes throttling.
  - No agents or builds, Steam video closed, a fixed brightness and scene.
  - 5 min per condition, repeated A B A B. Rail noise is about ±0.5 W, so judge each single change
    by the ccpanels_core and wakes columns, and only judge watts on cumulative stages.
  - Scenes:
    - S0: SteamVR only. You stop CC yourself; an agent must not.
    - S1: CC with every panel visible and static.
    - S2: two panels hidden.
    - S3: the headset off your head.
    - S4: a remote playing video.
  - **Pass:** VPH(S1) − VPH(S0) ≤ 0.5 W, S2 and S3 ≤ 0.2 W, S4 ≤ 1 W.
