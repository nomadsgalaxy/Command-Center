# Tracking a Steam Deck in the headset

I want a Steam Deck's screen to sit on the Deck itself in VR. You hold the real Deck, and its panel is
exactly where the glass is and moves with it. This is the plan, and part 1 is built.

There are three parts:

1. **The motion stream** (built). The Deck's gyro and accelerometer reach the Frame over the agent.
2. **Position from the camera.** The Frame's camera sees a tag board on the Deck's real screen and
   works out where the Deck is.
3. **Fusion and the panel.** The motion stream smooths between camera fixes, and the Deck's panel
   follows the result.

## Part 1: the motion stream

A Deck's controller (a Valve 28de:1205 device) has an IMU. It sends a 64-byte report about every 4 ms, but
the motion fields stay zero until a setting turns the IMU on. cc-host does that, decodes the reports and
sends them to the Frame as batched `imu` events. The wire format, the limits and the way it puts the
controller's setting back are in [agent.md](agent.md#3c-imu-stream-a-steam-deck-s-motion-sensors). To try
it, run `cc-home machine imu <machine>` on the Frame and wave the Deck.

What I measured on a Deck lying on a table (so the numbers are the noise floor, and moving it is on you):

- **250 reports a second**, every counter value there, in 4 seconds of both a local run and a run through
  the agent from the Frame (0 lost). The agent batches them to about 90 events a second.
- **Accelerometer:** about (-0.03, +0.02, +1.00) g flat and face up, so z is out of the screen.
- **Gyro:** within +-0.2 deg/s at rest. That's the bias a fusion has to estimate.
- **Firmware orientation:** a unit quaternion whose tilt agrees with gravity to about a degree. Its heading starts
  at an arbitrary value and drifts, because a Deck has no compass. That's the main reason part 2 needs a camera.

## Part 2: where the Deck is

The IMU knows how the Deck is turned but not where it is. Position comes from the camera, which has to see
something on the Deck. I first thought of small tags in the screen's corners, but they'd sit on top of the
picture. So this is the design instead:

- **The Deck's apps live on a virtual monitor, and the real screen shows a tag board.** While a Deck is
  tracked, krdp streams a virtual output, so the Deck's windows and the picture you see in VR never touch the
  physical panel. The physical panel shows a full-screen board of AprilTags, big and high-contrast, which is
  what the camera reads. You never see it, because the VR panel covers the real Deck.
- **The Frame's camera reads the board** and gets the Deck's full pose relative to the head, which is
  the position and also a heading the IMU can't give.

What each piece needs:

1. **krdp's virtual monitor on the Deck.** krdpserver already has it: `--virtual-monitor WIDTHxHEIGHT@SCALE`
   (server/main.cpp), which goes through `PlasmaScreencastV1Session::virtualMonitor` to
   `Screencasting::createVirtualMonitorStream`. I checked that in krdp 6.4.3's source. I haven't checked
   it against the version and patches our packages build (packaging/arch/PKGBUILD), nor that Plasma 6.2 on
   SteamOS can create the output. That comes first, because everything else depends on it. The open
   questions are:
   - whether apps open on the virtual output or have to be moved there, since a KWin rule or a script might be needed;
   - what happens to the real output's other uses while it shows the board (the Deck's own screen is
     what a game uses in Gaming Mode, so this is a Desktop Mode feature only);
   - how `session start` picks "virtual" instead of "the monitor at this index" (a new argument on
     `session start`, probably `virtual=WxH`).
2. **cc-host shows the board on the real output**, through the tag screen it already has
   (`cc-host tagscreen`, docs/agent.md section 3a). Today that screen is an align: it draws at most 64 tags from
   parameters, always with the banner across the top, for at most 60 seconds, and Esc cancels it. A tracking
   board needs a layout made for a camera at arm's length (a few large tags, or a grid sized from the
   output's `mm` size that `monitors` reports), and a way to stay up for a whole session. I'd keep the
   banner and Esc, because they're what stops a paired Frame from locking a host screen, and give
   tracking its own grant: you turn it on for a host, it shows a notification, and the board closes when
   the connection does. The hourly cap of 5 minutes was made for aligning, so it needs a separate rule.
3. **cc-scan reads it continuously.** Align is a one-shot today: it opens the mirror camera, collects tags, solves
   and quits. Tracking needs a loop that detects the board in each frame (cc-scan's ArUco port is
   already there), solves the board's pose with the known tag sizes and the camera's fit, and adds the
   head's pose at the frame's time (cc-panels' `head` command, and `lag.rs` has the camera's delay) to get the
   Deck in the room. It should run at the camera's rate and share the camera with the HUD the way scan does,
   and the camera setting stays at its default (never write `enableCamera=false`).

## Part 3: fusing it, and the panel

- **The filter.** The IMU runs at 250 Hz and the camera gives a fix at a few tens of Hz, with a delay.
  A filter (an error-state Kalman filter, or a complementary filter first if that's enough) keeps the
  Deck's pose, its velocity and the gyro's bias. It integrates the gyro between fixes, uses gravity for tilt,
  and takes the camera's pose to correct position, heading and the bias. The camera is the only source of
  heading, so while the board is out of view the heading drifts slowly and the tilt doesn't.
- **Clocks.** The stream has two clocks per sample, the controller's counter (`seq` times 4 ms, exact spacing
  and no jitter) and the host's read time (`t_us`). The filter should use `seq` for spacing, and fit `t_us` to
  the Frame's clock once to place samples against camera frames. A camera frame's time then picks the
  right IMU state to correct. A lost report shows as a gap in `seq`.
- **Calibration.** A few fixed offsets need measuring once: the IMU's frame against the board's, the
  camera against the IMU in time (a first guess comes from comparing the camera's rotation rate with the
  gyro's), and the room's up against gravity. SteamVR's room has gravity-up, so it only
  leaves a heading to solve, and a camera fix gives it. The Frame's inside-out tracking turns
  and shifts the room when it relocalizes, so I'd store the Deck's pose against the head and the room's anchor each time,
  not keep a room-absolute pose for a long time.
- **Losing the board.** Hands cover it, and the Deck can leave the camera's view. The IMU can bridge a
  short loss (about 200 ms, because integrating acceleration twice drifts fast), then the panel should
  stop following and stay where it was, with a visible cue, and take the next fix back gently so it
  doesn't jump.
- **The panel follows the pose.** The Deck's panel is placed from the fused pose plus a fixed offset (the panel's
  rectangle is the virtual output's picture, laid on the real screen's glass). It needs a "rides the Deck"
  mode in cc-panels, where the placement is driven by a stream of poses instead of the saved spot, and
  the laser pointer and touch have to agree with it while it moves (the pointer driver's offsets have to
  follow the panel).
- **Portability.** None of this needs a Frame-only assumption except the camera. A Quest or an Index would
  need another way to see the board, and the stream and the filter would be the same.

## What's built, and what's next

Built: `crates/cc-host/src/imu.rs` (hidraw, the report decode, the enable and restore), `crates/cc-proto/src/imu.rs`
(the sample, the scales, orientation to heading, pitch and roll), the agent's `imu` command and `version`'s
`features`, `cc-home machine imu`, and `cc-host imu-probe` for a Deck on its own.

Tried on a real Deck: the motion stream runs at 250 readings a second with none lost, and it follows the
Deck through tilts, rolls, turns and a flip (turn rates past 130°/s, gravity matching the orientation).

The virtual monitor works too. With the Deck's patched krdp 6.4.3 started as `--plasma --virtual-monitor
1280x800@1`, a client connecting made KWin 6.4.3 add an output, `Virtual-1280x800@1`, to the right of the
Deck's own screen at x = 1280, and it went away when the session ended. krdp 6.4 only takes NLA, so a test
client needs `/sec:nla`.

This is paused here. When it picks up again, in order:
1. The Deck's Frame session streams the virtual monitor while tracking is on (cc-host passes
   `--virtual-monitor`).
2. A fourth krdp backport: 6.4.3 maps pointer positions as if the streamed output started at 0,0, so on
   the virtual output at x = 1280 clicks would land on the real screen. krdp 6.5 maps them onto the output's
   own geometry.
3. Apps open on the real screen, so a KWin rule or script moves them to the virtual output.
4. cc-host's tag screen gets a full-screen tracking board for the real output.
5. cc-scan reads the board continuously, the filter fuses it with the motion stream, and the panel follows.
