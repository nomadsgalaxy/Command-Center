# Final test: the Frame side (Rust)

This is the Frame half of the live test; the host half is [final-test-host.md](final-test-host.md).
Everything here was already built and cross-tested offline:
- cc-home's `tests/cross.rs`: 177 command cases plus the align's, checked against what the Python
  tools did (recorded in tests/fixtures/, with the machine cases run against a real cc-host).
- cc-home's `tests/nossh.rs` (`cargo test -p cc-home --test nossh`).
- The cargo tests in cc-home, cc-proto, cc-scan and cc-panels.

What's left can only run live. Do it in one sitting, with me in the headset, before or after
the host steps. If a step fails, note it and carry on, unless the step says to stop.

**What's new on the Frame:**
- cc-home is one static Rust binary, with no Python.
- `cc-home hibernate` replaces session/cc-hibernate.
- cc-panels draws its tags, glyphs and cursor itself (assets.rs, no assets.py).
- cc-panels answers `tip` and `devices` (cc-tip and cc-roles are gone).
- align, calibrate, refit and pair --scan run on cc-scan.

**Before starting:** build it, and keep a way back.

```sh
cd ~/control-center   # main, with everything merged
./cc-box bash -c 'export RUSTUP_HOME=$HOME/.local/share/cc-rust/rustup CARGO_HOME=$HOME/.local/share/cc-rust/cargo
  PATH=$HOME/.local/share/cc-rust/cargo/bin:$PATH; cargo build --release --target aarch64-unknown-linux-musl -p cc-home'
./cc-box bash -c 'cargo build --release -p cc-panels'
file target/aarch64-unknown-linux-musl/release/cc-home   # must say "statically linked"
./cc-home list                                           # runs on the host (no usage text, no error)
# the whole config, kept private: home.json, viewers.conf, settings.json, trusted-hosts, passwords,
# the mirror fits
install -d -m 700 ~/cc-test-backup
(umask 077; tar -C ~/.config -czf ~/cc-test-backup/control-center-$(date +%F-%H%M).tgz control-center)
```

If a step leaves the config wrong, close the Desktop first and then restore it:
`tar -C ~/.config -xzf ~/cc-test-backup/control-center-<time>.tgz`.

**Ways back:** there's no Python fallback any more (I decided on 2026-10-03: "Yes, keep it deleted").
- The config: the backup tarball above.
- The code: reinstall the previous release.

## 1. Tags, glyphs and the cursor (assets.rs)

Make cc-panels draw them fresh: `rm -rf ~/.cache/control-center/assets`, then open the Desktop.

Pass when all of these look the way they did when Python drew them with Pillow:
- every remote panel's border tag reads "<host> · Monitor n", or its label;
- a window panel's tag reads its app name;
- the taskbar clock and its "+N" are sharp;
- the Machines window's typed text renders;
- the laser cursor is the Starlight dot with a dark rim.

Also check the log: `grep -c "assets:" ~/.cache/control-center/cc-panels.log` shouldn't count any "no font" lines.

**Heads up, steps 2 and 4 move the live panels on purpose,** so run them first, with me watching.

## 2. Spots and workspaces (cc-home, stage 1)

1. `cc-home list` shows the same spots as before.
2. Move a panel, then `cc-home save test`.
3. `cc-home apply home` puts every panel back. Right Ctrl + Home does too.
4. `cc-home apply test` moves it again.
5. `cc-home forget test`.
6. `cc-home workspace` lists them, and `cc-home network` names the current network.

Pass when the panels go where these commands say, and ~/.config/control-center/home.json keeps its
other spots (compare with the backup: `tar -xzOf ~/cc-test-backup/*.tgz control-center/home.json | diff - ~/.config/control-center/home.json`).

## 3. Machines (cc-home, stage 2)

1. In the Machines window, list, rename a monitor and rename it back, and connect and disconnect one remote.
2. `cc-home machine probe <host>` gives the agent's answer.
3. `cc-home machine window list <machine>` lists the host's windows.

Pass when each one works as before, and viewers.conf/trusted-hosts only change as asked
(`git diff --no-index` against a copy taken first).

## 4. Calibrate with the controller tip (cc-panels `tip`)

Run `cc-home calibrate desk-wide right`. Hold the controller's tip on each of the three corners and press Enter.

Pass when:
- each corner prints "right tip at x y z";
- the size printed is within 2 cm of the real picture;
- the panel lands on the monitor.

Undo with `cc-home apply home` if it doesn't.

## 5. Align (cc-home stage 3 on cc-scan)

Run Machines → Align, or `cc-home machine align`. It's the same run as the host doc's step 7.

Pass when:
- the HUD strip shows each step;
- tag screens come up on every monitor;
- Esc on a host skips that monitor with "closed on that machine", and the others go on;
- the fit places all three monitors within the usual 2–6 mm;
- `~/.cache/control-center/scan/job.json` exists (keep the folder as regression data, the host doc's step 9).

A monitor skipped for the tag limit must say "that machine's tag-screen limit for this hour is used up", not "escaped".

## 6. Refit

Run `cc-home refit`. Pass when the board shows up in the room, you get through the three places,
and the mirror-camera fit is written (it prints its residual).

## 7. Pair by looking (pair --scan)

This one's optional, since it re-pairs a host. Use the laptop: run `cc-share pair` on .85, then
`cc-home machine pair --scan --replace` on the Frame, and look at the key.

Pass when the key is read, the HUD stays up through the pairing, and
`cc-home machine probe work-laptop` answers afterwards.

## 8. Hibernate (cc-home hibernate) and a closed Desktop's footprint

1. Open Konsole, Dolphin and a Flatpak browser in the Desktop and place their panels.
2. Close the Desktop with the taskbar's power chip.

Pass, after the close, when:
- `grep hibernate: ~/.cache/control-center/cc-panels.log | tail -2` shows "saved 3 app(s)";
- `ls -l ~/.cache/control-center/desktop-hibernate.json` shows mode `-rw-------`;
- `systemctl --user is-active cc-desktop` says inactive;
- with `rest_nice` set in settings.json, `ps -o ni= -p $(pgrep -x <name> | head -1)` says 10 for each name;
- `free -m` shows roughly 1.3 GB or more available than with it open.

3. Open the Desktop again.

Pass when:
- the log says "restored 3 of 3";
- each app comes back, once, onto its old panel spot;
- `desktop-hibernate.json.placed` exists.

Some apps aren't reopened on purpose, and they're logged as left out, not failed: an autostart
that opens anyway (a password manager's tray, say), Plasma's own windows, dialogs, and any app with neither a .desktop
entry nor its program in `~/.config/control-center/hibernate-allow`. Note which ones and carry on.

4. Close the Desktop again with no app windows open, then: `jq '.apps|length'
   ~/.cache/control-center/desktop-hibernate.json.prev` must still say 3 (an empty close never
   wipes the last list with apps).

Note what came back and what didn't (terminal sessions, browser tabs, open documents).

## 9. Autoconnect at open

With the Desktop you opened in step 8, pass when the remotes marked autoconnect=yes connect and
the others wait, dimmed, on the taskbar. That's `cc-home autoconnect --write`, which is Rust now
and runs from session/cc-launch.

## Results

Fill this in during the test: the step, pass or fail, and the log line or output that shows it.

| Step | Result | Evidence |
|---|---|---|
| Start (static build, backup) | | |
| 1 Tags, glyphs, cursor | | |
| 2 Spots and workspaces | | |
| 3 Machines | | |
| 4 Calibrate | | |
| 5 Align | | |
| 6 Refit | | |
| 7 Pair by looking | | |
| 8 Hibernate | | |
| 9 Autoconnect | | |
