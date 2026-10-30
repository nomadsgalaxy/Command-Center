# Phase 0: the headset pass

This is revision 4 of the checklist.

Phase 0 of the OpenXR plan ([openxr-plan.md](openxr-plan.md)) moved Command Center's OpenVR calls
behind interfaces. It was meant to change nothing, apart from a few fixes made on purpose: a
failed texture import leaves no handle, and a PipeWire or GBM file descriptor is only copied or
borrowed when it's a real one. The fakes and the golden tests show that, in the cases they cover,
the calls are the same ones, in the same order. They don't show that the whole program behaves
the same, or that real SteamVR answers the same way, so this is the check in the headset. It
takes about 15 minutes in one sitting. If a step fails, write down what you saw and carry on.

With the results, write down:

- the checklist's revision (above);
- the commit the build was made from, as whoever built it reports it (for the first pass,
  49db50d, built at 14:21 on 2026-10-30), next to the binary's time, which
  `stat -L -c %y /proc/$(pgrep -xn cc-panels)/exe` gives while the Desktop runs. The time only
  says when the file was written, not what it was built from;
- any restart of the Desktop you didn't cause, with the extra trace it wrote, apart from the
  rest;
- each step's result: passed, failed (with what you saw), skipped, or not available here (and
  why, such as no eye tracking). A log search that comes up empty is a pass only for a step you
  did. For a step you skipped, it says nothing.

Every log line below is quoted from the code. Where a step has no log line, it says so, and
you go by what you see.

## Before you start (2 minutes)

1. Turn the trace on, if it isn't already: `touch ~/.cache/control-center/vr-trace`. Each time
   the Desktop starts, it writes a new `~/.cache/control-center/vr-trace-<epoch>-<pid>.jsonl`,
   readable only by you ([vr-harness.md](vr-harness.md)).
2. Add a helper for the Desktop's control socket, `@controlcenter`. Only `gather` and
   `ungather` have a `cc-home` command, so the rest go through this:

   ```sh
   cc() { python3 -Ic 'import socket,sys;s=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM);s.bind("");s.settimeout(3);s.sendto(" ".join(sys.argv[1:]).encode(),"\0controlcenter");print(s.recv(65536).decode())' "$@"; }
   ```

3. With the build you have now still running, save what SteamVR holds:
   `cc surfaces > ~/cc-surfaces-before.txt`. Builds from main 50f6aef on have the `surfaces`
   and `universe` commands. On an older build, `cc surfaces` answers with an error instead.
   Skip this and the diff at the end, and compare by eye: the same panels, bars and windows, in
   front of each other the same way.
4. Note where the log ends, and which file it is, so the searches afterwards read only this run:

   ```sh
   echo "$(wc -l < ~/.cache/control-center/cc-panels.log) $(stat -c '%i-%W' ~/.cache/control-center/cc-panels.log)" > ~/cc-log-start.txt
   ```

5. Install the phase 0 build and open the Desktop. Once it's up, save its pid, which names its
   trace: `pgrep -xn cc-panels > ~/cc-pid.txt`.

## Two checks from the last build (3 minutes)

These came in just before phase 0 and haven't had their headset check yet.

1. **Bring all here.** Turn so that some panels are behind you, then press Right Ctrl + B.
   Every panel that's showing comes in front of you, about 1.4 m ahead, facing you, each the
   size and curve it was. Right Ctrl + Shift + B puts them all back. Then try the taskbar's
   Bring all here chip (four arrows pointing in at a panel) and `cc-home gather`, with
   `cc-home ungather` after each.
   - The log says `gather: <n> brought in front of you, not saved (...)`, then
     `ungather: <n> put back where they were`.
2. **The start menu's laser.** Open Plasma's start menu. With a controller, point at the top
   item, then one in the middle, then the bottom one, holding each for a moment. The highlight
   should land on the item you point at. If it doesn't, say which one it lands on instead for
   each: one item low, two items low, or on the right one.
   - There's no log line for this. The trace records the laser's ray with each of Plasma's
     pointer events, which "Afterwards" below uses.

## At start (just look)

3. **Pictures.** Every panel shows its picture, with no blinking and no black panels. The log
   has `<name>: import ok` for each window panel, and no `import fail` or
   `SteamVR won't import a <w>x<h> buffer`.
4. **The mouse's laser.** The log has
   `laser: dominant hand Right, the mouse's laser uses the other one while it's free` (or Left)
   and `laser: cc_pointer is there`.
5. **The room and the frame rate.** `cc universe` answers `ok` and a number other than 0, and
   the log has no `workspaces: SteamVR has no room yet`. The frame rate has no log line, so turn
   your head and watch for stutter.

## One thing each (8 minutes)

6. **Showing, hiding and stacking.** `cc hide`, then `cc show`: everything goes and comes back.
   `cc machines show`: the Machines window is in front of the panels behind it, and the laser
   reaches it. `cc machines hide` closes it.
7. **Carrying.** Carry a panel by its card with a controller and drop it. It follows the
   controller without lagging or jumping and stays where you let go. The log has
   `<name>: card pressed by device <n> on <part>`.
8. **The last device pressed drives.** With the mouse awake, press a card with a controller.
   The mouse's cursor goes to sleep and the controller takes over. In the `card pressed by
   device <n>` line, n is the controller.
9. **The wrist bar's hand.** `cc taskbar wrist`. The bar sits on the wrist of the hand you don't
   point with and fades in when you look at it. The log has
   `taskbar: wrist on device <n> (Left hand)`. In Preferences, switch the pointing hand. The bar
   moves to the other wrist, and the log has `prefs: pointing hand Left`, then
   `taskbar: wrist on device <m> (Right hand)`. Switch it back, then `cc taskbar fixed`, or
   whichever mode you use.
10. **The keyboard.** Press the keyboard button on a panel's bar and type a few letters. They
    reach the panel, and closing the keyboard leaves no key held. The log has
    `<name>: SteamVR keyboard open`, then `<name>: SteamVR keyboard closed ...`.
11. **A toast.** `cc notify toast phase 0` shows SteamVR's toast. One that works logs nothing.
    One that doesn't logs `notify: no dashboard tab or IVRNotifications, ...` or
    `notify: SteamVR's toast failed (...)`.
12. **Gaze.** Look at another panel and move the mouse: the cursor goes to the panel you're
    looking at. Right Ctrl + G turns gaze lock on and off, and the log says
    `gaze lock on: the pointer stays on the panel you look at`, then `gaze lock off`. Without eye
    tracking, the log says `gaze: no eye tracking; the pointer goes where the mouse takes it`.
13. **Taking the headset off.** Take it off and put it back on. The log has `headset: off`,
    then `headset: on`. Nothing is logged at start, so a wrong reading at start shows up as a
    `headset: off` you didn't cause.
14. **A game.** Start a VR game, then quit it. The log has
    `game: a VR game is running, so the Desktop hides and waits`, then
    `game: the Desktop is back`, and the panels come back where they were.
15. **Relocalization.** You can't cause this on purpose. If it happens, the log has
    `tracking relocalized (<n>): ...` or `tracking: universe <a> -> <b>`, and the panels all
    move together.

## The mouse's laser off the panels (3 minutes)

Point the mouse at SteamVR's own UI, outside our panels.

16. **Right Ctrl + D** opens SteamVR's dashboard, and again closes it.
17. **A click** on a dashboard tile opens it. Off our panels the cursor's dot hides, since
    SteamVR draws its own.
18. **The wheel** scrolls the dashboard's library a step at a time.
    For 16 to 18, the `kvm: ... sent <m> moves, <c> clicks, <k> keys, <w> wheel` line counts the
    clicks and the wheel, and there's no `laser: SteamVR didn't give it the dashboard pointer`.
19. **Losing the laser with a button held.** Hold the left button on a dashboard slider or
    scroll bar and drag it a little. Still holding it, pick up the controller in the hand you
    don't point with: it takes that hand, so the mouse's laser goes. Before you let go, move the
    mouse, and watch whether the slider stops as soon as the laser goes or keeps following.
    Then let go of the button, put the controller down, and point the mouse at the dashboard
    again. Nothing should still be dragging, and the next click should work as usual. Write the
    two down separately: whether the slider stopped when the laser went, and whether the next
    click worked. There's no log line for this. If clicks off the panels stop working after it,
    look for `binding loads` in the log. Each time the laser reconnects, it spends one of
    SteamVR's binding loads, but the line is written only when the budget runs out, not on each
    reconnect, so a hit means it really ran out.

## Afterwards (1 minute)

- Put this run's part of `~/.cache/control-center/cc-panels.log` in one file. The log moves to
  `cc-panels.log.1` when the Desktop starts with it past 5 MB, so which file has your saved
  line count is told by its inode and the time it was made (`%i-%W`), not its length. The inode
  alone isn't enough: once the old file is deleted, a new one can get its number.

  ```sh
  cd ~/.cache/control-center
  read n id < ~/cc-log-start.txt
  if [ "$(stat -c '%i-%W' cc-panels.log)" = "$id" ]; then
    tail -n +$((n + 1)) cc-panels.log
  elif [ "$(stat -c '%i-%W' cc-panels.log.1)" = "$id" ]; then  # it moved when the Desktop started
    tail -n +$((n + 1)) cc-panels.log.1; cat cc-panels.log
  else  # it moved twice, so the Desktop started again during the pass
    cat cc-panels.log.1 cc-panels.log
  fi > ~/cc-run.log
  grep -n '^=== cc-panels ' ~/cc-run.log
  ```

  Each start writes `=== cc-panels <date> <time> ===` first, and this run begins at the first one
  at or after the time you opened the Desktop. Lines above it are the old Desktop shutting down.
  A second one later means the Desktop started again during the pass: write that down as a
  restart. Then search from your run's line, putting its number in for `<line>`:

  ```sh
  sed -n '<line>,$p' ~/cc-run.log |
    grep -nF -e panicked -e 'import fail' -e "won't import" -e "laser: SteamVR didn't" \
      -e 'binding loads' -e 'no cc_pointer driver' -e 'toast failed' \
      -e 'no SteamVR keyboard' -e "couldn't be typed" -e 'taskbar: wrist: no'
  ```

  Nothing should come up. Each search covers these steps, and counts only for the ones you did:
  - `panicked`: all of them;
  - `import fail` and `won't import`: 3;
  - `laser: SteamVR didn't`, `binding loads` and `no cc_pointer driver`: 4 and 16 to 19.
    `binding loads` is logged only when SteamVR's binding-load budget runs out, not on each
    reconnect, so a hit means it really ran out;
  - `toast failed`: 11;
  - `no SteamVR keyboard` and `couldn't be typed`: 10;
  - `taskbar: wrist: no`: 9.
- In this run's trace, `~/.cache/control-center/vr-trace-*-$(cat ~/cc-pid.txt).jsonl`, picked
  by the pid you saved rather than by which is newest. A restart during the pass writes another
  trace under a new pid: keep it, and note it with the restart.
  - `grep -c '"ev"'` isn't 0: the lasers' events were read and recorded;
  - `grep -c '"pose"'` isn't 0, and `grep -c '"m":null'` (a laser that wasn't tracked) is small;
  - for the start menu, the `"plasma"` lines say where Plasma's overlay was, and each Plasma
    pointer event has its laser's `"pose"` line. Together they give where the ray hit the
    overlay, to compare with the point SteamVR reported.
  - Keep the file: it can be replayed as a test ([vr-harness.md](vr-harness.md)).
- `cc surfaces > ~/cc-surfaces-after.txt`, then
  `diff ~/cc-surfaces-before.txt ~/cc-surfaces-after.txt`. The same panels, bars and windows
  should be there, in the same order, with the same flags and widths. Where they are can differ,
  if you moved things.
- When you're done, stop new traces: `rm ~/.cache/control-center/vr-trace`. If the pass was
  clean, also delete the traces (but one you keep as a replay), the two surfaces files,
  `~/cc-log-start.txt`, `~/cc-pid.txt` and `~/cc-run.log`. If anything failed or wasn't clear, keep them all until it's been looked
  into.
