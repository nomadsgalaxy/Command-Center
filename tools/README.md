# Measuring cc-panels: before and after

I use these three tools to check whether a change actually made cc-panels cheaper to run. They only read: they never connect to SteamVR, so they don't spend any of its binding-load budget, and they don't signal anything. Run them on the Frame while cc-panels is running.

First, find the real binary's PID. The `cc-panels` wrapper is a bash script with the same name, so skip it:

```sh
P=$(for p in $(pgrep -x cc-panels); do case $(readlink /proc/$p/exe) in */target/release/cc-panels) echo $p ;; esac; done | head -1)
```

## ccpower.sh: power and CPU per scene

```sh
tools/ccpower.sh LABEL [SECS=300]
```

It runs for SECS seconds (5 minutes by default) and adds one row to `~/ccpower.csv` (set `CCPOWER_OUT` to write somewhere else). The columns are:

- `vph_W`: the headset's whole draw.
- `cpu_W` and `gfx_W`: the CPU cluster and GPU rails.
- `bat_W`: the battery side.
- `ccpanels_core`: cc-panels' CPU, in cores.
- `ccdesktop_core`: cc-desktop's CPU, in cores.
- `ccpanels_wakes_s`: cc-panels' voluntary wakes per second.
- `cpu_C`, `fan`, `bat_status`, `cap`: CPU temperature, fan speed, battery status and charge.

**How to run it fairly:**

- Unplug if you can, because charging changes heat and throttling.
- Don't run builds or agents, close any Steam page that's playing video, and keep the backlight fixed.
- Keep the same scene for the whole run.
- Repeat A B A B. Rail noise is about ±0.5 W, so judge a single change by `ccpanels_core` and `ccpanels_wakes_s`, and only trust watts for a whole stage.

**Scenes** (use these as labels, with a suffix such as `S2-before` or `S2-after`):

| Scene | Setup |
|-------|-------|
| S0 | SteamVR only. Stop cc-panels yourself. |
| S1 | Every panel visible and static. |
| S2 | Two panels hidden. |
| S3 | The headset off your head. |
| S4 | A remote playing video. |
| S5 | Idle, with the pointer asleep: the adaptive tick. |

**Pass:** VPH(S1) − VPH(S0) is 0.5 W or less, S2 and S3 add 0.2 W or less, and S4 adds 1 W or less.

## stall3.sh: how long the main loop stops

```sh
tools/stall3.sh $P 30
```

It watches the main thread for 30 s and prints one line for each gap of more than 45 ms between its wakes:

```
12:01:02.345 gap 412 ms: on-CPU 54 ms, waiting for a CPU 358 ms; states seen {('R', '0'): 3}
```

- **on-CPU:** the loop was working.
- **waiting for a CPU:** it was ready to run, but another task had the core (nice levels, load).
- **states seen:** what it was blocked in. For example, `futex_do_wait` means a lock, such as the KVM lock the input thread holds.

**About the idle tick:** an idle loop now waits 40 ms between ticks, which is under the 45 ms threshold, so idle ticks print nothing. If you see a string of gaps just over 45 ms, the tick is running late.

**Before/after:** compare the largest gap while you open 3 or 4 windows quickly, and the gaps while idle.

## pthr.sh: CPU and wakes per thread

```sh
tools/pthr.sh $P 30
```

It prints one line for each thread that's busy (over 0.05%) or wakes more than once a second, busiest first:

```
pid 12345 total 9.8% wakes 180/s over 30.0s
  4.10%     40/s tid=12345 cc-panels        wchan=futex_wait_queue sys=202
```

The `wakes` column is the one to watch for idle work. These threads have names:

| Thread | What it is |
|--------|------------|
| `cc-panels` (tid = pid) | The main loop. |
| `cc-input` | Our keyboards and mice. |
| `cc-lease` | The laser lease. |
| `cc-kwin` | The link to the KWin script. |
| `async-io` and zbus's | zbus's own threads. |

The other `cc-panels` threads are RDP sessions, FreeRDP's own threads, and PipeWire.

**Before/after:** run it for 30 s with the pointer asleep and nothing moving (S5). You should see:

- **The main thread:** 25 ticks/s, down from about 85. Each tick also waits on a few SteamVR calls, so this comes to roughly a third of the 253 wakes/s it had before.
- **`cc-lease`:** about 1/s, down from 50.
- **`cc-input`:** about 2/s, down from 10.
- **`async-io` and zbus's threads:** near 0, down from 150–220 together.
- **Each RDP thread:** about 1/s plus its frames, down from 10 plus its frames.

The `ccpanels_wakes_s` column in ccpower.sh should drop by at least 150.
