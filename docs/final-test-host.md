# Final test: the host side (Rust)

> **There's no Python fallback any more.** `cc-share install --python` and the Python agent were
> removed on 2026-10-03, before this live test ran. The way back is the per-host tarball taken
> before step 1 (below), or reinstalling the previous release.

Everything here was built and cross-tested offline already: the unit tests, cc-host's
`tests/conformance.rs`, `tests/pair.rs` and `tests/share.rs`, cc-home's `tests/nossh.rs`, and
cc-scan's `tests/cross.rs`. What's left can only run live. I'll run it in order, in one sitting,
at the desk, on two hosts: the desktop at .63 (monitors 0 1) and the laptop at .85 (monitor 0). Each
step says what to run and what passing looks like. If a step fails, note it and keep going,
unless the step says to stop.

Before starting, build on the Frame and make sure there's a way back.

```sh
cd ~/control-center   # main, with everything merged
cargo build --release --target x86_64-unknown-linux-musl -p cc-host     # the hosts' binary
cargo build --release -p cc-scan --example solve                         # step 9
```

For a way back that doesn't depend on a bundle (review F1), take a tarball on each host of
everything the install replaces, before step 1:

```sh
for h in user@203.0.113.63 user@203.0.113.85; do
  ssh $h 'tar czf ~/cc-host-before-$(date +%Y%m%d-%H%M).tgz -C ~ .local/share/control-center .local/bin/cc-share $(cd ~ && ls -d .config/systemd/user/control-center-*)'
done
```

To revert, unpack it over ~ (`tar xzf ~/cc-host-before-*.tgz -C ~`), then run `systemctl --user
daemon-reload` and restart the units. Reinstalling the previous release works too.

Restarting the guard or the agent while the Frame is streaming drops the viewers for a few
seconds (review F3). Steps 1, 2 and 4 do that, so only run them when I'm ready for it.

The install below is the developer path (ssh, scp). This list doesn't test the signed public
release (D-052, docs/ssh-free.md §2): that stays untested until the repo is public (review F5).

## 1. An old-unit host goes through the cc-share link (.85, before reinstalling it)

Its units still call `~/.local/bin/cc-share guard|frame-run|announce-run`. Put the new cc-host in
place, make cc-share a link to it (which is what install does), and restart the guard:

```sh
scp target/x86_64-unknown-linux-musl/release/cc-host user@203.0.113.85:.local/share/control-center/cc-host.new
ssh user@203.0.113.85 'cd ~/.local; mv share/control-center/cc-host.new share/control-center/cc-host;
  ln -sfn ~/.local/share/control-center/cc-host bin/cc-share;
  systemctl --user restart control-center-guard control-center-agent; sleep 2;
  systemctl --user status control-center-guard | grep -E "Active|cc-host|cc-share"'
```

Pass: the guard is `active (running)` and its process is `~/.local/bin/cc-share guard`, which is
cc-host answering to the cc-share name with the same commands. Then open the Desktop on the
Frame: work-laptop's panel should stream (frame@ ran `cc-share frame-run` → cc-host → krdpserver).

## 2. Install with cc-host on both hosts

It's just the binary now. It installs itself to ~/.local/share/control-center and links
~/.local/bin/cc-share to it.

```sh
for h in "user@203.0.113.63 0 1" "user@203.0.113.85 0"; do set -- $h; host=$1; shift
  scp -q target/x86_64-unknown-linux-musl/release/cc-host $host:.cache/cc-host-install
  ssh $host "export XDG_RUNTIME_DIR=/run/user/\$(id -u) WAYLAND_DISPLAY=wayland-0; chmod +x ~/.cache/cc-host-install;
    ~/.cache/cc-host-install install $*; rm -f ~/.cache/cc-host-install;
    ls -l ~/.local/bin/cc-share; grep -h ExecStart ~/.config/systemd/user/control-center-*.service"
done
```

Pass: `~/.local/bin/cc-share -> ~/.local/share/control-center/cc-host`. The checklist shows `ok`
for krdpserver, kscreen-doctor, cc-host, both units running, `agent answering on 3399`, the
certificate and the host id, with no `python3` line. Every unit's ExecStart is
`%h/.local/share/control-center/cc-host ...` (share@ is still krdpserver via `/bin/sh`). The
existing cert.pem is kept (RSA), so pairings keep working.

## 3. Paired Frames' servers (frame-run)

Open the Desktop on the Frame. cc-panels starts the sessions through the agent.

```sh
ssh user@203.0.113.63 'systemctl --user list-units "control-center-frame@*" --no-legend; ~/.local/bin/cc-share frames'
```

Pass: `control-center-frame@frame-0` and `-1` are running, and desk-wide and desk-portrait are
streaming on the Frame. `cc-share frames` shows
`frame slot=1 last-used=<today> sessions=monitor 0,monitor 1`.

## 4. The guard (.63: DP-1 is 5120x1440, over krdp's 4.28 MP)

First, check that DP-1 is really at its full mode. The stale-stream incident left it at
3840x1080 with no guard file (review F2):

```sh
ssh user@203.0.113.63 'kscreen-doctor -o | sed "s/\x1b\[[0-9;]*m//g" | grep -A8 "DP-1"; ls ~/.config/control-center/guard-* 2>/dev/null'
# if DP-1 is lowered and there's no guard file: put it back by hand (the mode id of 5120x1440@120 from the list)
ssh user@203.0.113.63 'kscreen-doctor output.DP-1.mode.<id>'
```

Then:

```sh
ssh user@203.0.113.63 'journalctl --user -u control-center-guard -f -o cat'   # leave it running
```

With the Desktop open (so a viewer is connected), pass looks like this: the journal says
`viewer connected: DP-1 mode <id> -> <id>`, `~/.config/control-center/guard-DP-1` exists, the
wide monitor shows the lower mode, and its panel on the Frame comes back within seconds (the
shares restart). Close the Desktop: about 15 s later the journal says `restored DP-1` and
`no viewers: restored`, the file is gone, and the monitor is back at 5120x1440. Then open the
Desktop again so it lowers again, and run `systemctl --user restart control-center-guard`. Pass:
the stop restores the mode first (the signal path), and the new guard lowers it again.

## 5. Idle sessions stop (the agent's viewers, now from /proc)

Close the Desktop and wait session_idle_min (10 min), plus a minute.

Pass: `cc-share frames` on .63 shows `sessions=none`, and `journalctl --user -u
control-center-agent` shows the sessions stopped as idle. The agent read the Frame's connections
from /proc/net/tcp, which is what's new here.

## 6. Announcing (avahi)

```sh
ssh user@203.0.113.85 'cc-share announce status; cc-share announce on'
avahi-browse -rpt _controlcenter._tcp | grep 203.0.113.85     # on the Frame
```

Pass: the TXT has `host=<the laptop's hostname>`, `monitors=1`, `m0=eDP-1,1920x1080`, `version=1` and
`pair=0`. Then run `cc-share pair` on .85, so the key screen shows. Within about 3 s the TXT says
`pair=1`. Press Esc: the key screen goes, and the TXT is back to `pair=0`. Afterwards, put
announcing back the way it was (`cc-share announce off` if it was off).

## 7. The tag screen and key screen, drawn by cc-host (Wayland)

During an align from the Frame (cc-home's Rust align), check that:
- each shared monitor shows the tags full screen, with the grey banner across the top;
- on a monitor where a tag screen is up, Esc gives the light grey "Cancelled" screen, and the
  Frame reports that monitor skipped (`escaped`);
- Esc again within 1 s asks "Block this Frame for 10 minutes?", and Esc answers no;
- `cc-share stop` closes a tag screen left up.

If there's a monitor scaled to 125% or 150%, check that the key screen (`cc-share pair`) is
sharp, not blurred. That's the fractional-scale path, which hasn't been tested yet.

## 8. krdp with an ECDSA certificate (new hosts get one)

This uses a throwaway server, so nothing paired gets touched (on .63, at the desk). The password
is random and thrown away after, because krdp takes it on the command line where `ps` shows it
(the known L1). The port is 3490, outside the 3399-3449 range the firewall opens. The client
connects over loopback, and a trap stops the server whatever happens (review F4):

```sh
T=$(mktemp -d); PW=$(head -c 12 /dev/urandom | base64 | tr -d '/+=')
CC_CONF=$T ~/.local/share/control-center/cc-host cert
/usr/bin/krdpserver --plasma --monitor 1 --port 3490 -u test -p "$PW" --certificate $T/cert.pem --certificate-key $T/key.pem & S=$!
trap 'kill $S 2>/dev/null; rm -rf $T' EXIT INT TERM
sleep 2; timeout 20 xfreerdp3 /v:127.0.0.1:3490 /u:test /p:"$PW" /cert:ignore    # or sdl-freerdp3
kill $S; wait $S 2>/dev/null; rm -rf $T; trap - EXIT INT TERM
ss -Hltn 'sport = :3490'    # nothing: the server is gone
```

Pass: the client window shows the portrait monitor (a mirror of itself), and krdpserver logs no
TLS, key or licensing error. If it fails, cc-host cert goes back to RSA. Existing hosts aren't
affected either way, since they keep their RSA cert.pem.

## 9. The Rust fit against solve.py's on the kept real scans (the review's gate)

solve.py is gone, but its fits of the real scans kept in
~/.cache/control-center/scan-regression-*/ are recorded in tests/fixtures/solve-real/. On the
Frame:

```sh
./cc-box tests/solve-real
```

Pass: it prints `solve-real: N real scan job(s) compared` and the test is ok. Each monitor gets
the same shape from both, centres within 6 mm (the real fits' 2-6 mm), and a radius within 5%.
After the first real align, keep that scan's folder too (~/.cache/control-center/scan, with the
`hidden-<time>/` the job points at) as regression data (review F7).

## 10. Revoking, locking, stopping (review F6)

On .85, with the Desktop open:

- `cc-share lock`: the Frame's agent calls are refused right away (`machine` commands from the
  Frame say locked), and any tag screen closes. After `cc-share unlock`, the calls work again.
- `cc-share down`: every unit stops (`systemctl --user list-units 'control-center-*'` shows none
  running), and the Frame's panel for that host goes away. `cc-share up` brings it back.
- A revoked Frame is refused right away: run `cc-share unpair frame` on the host. The Frame's
  agent connection drops within a second (SIGHUP), its login fails, and these are gone:
  `~/.config/control-center/frames/frame.json`, `trusted-frames/frame.pub`, and any running
  `control-center-frame@frame-*`. Pairing again is the optional step below.
- Unpairing from the Frame instead (`cc-home machine unpair <id>`) should have the same result on
  the host, done through its agent.

Optional, after the steps above and with a backup first: pair the Frame again end to end with
the Rust host. On the Frame, run
`cp -a ~/.config/control-center/trusted-hosts ~/.config/control-center/viewers.conf <backup dir>`.
Then run `cc-share pair` on .85, and use Machines › Add on the Frame (type the key, or look at
it). Pass: it pairs, the panel streams, and `cc-share frames` lists the Frame again.

## 11. Afterwards

`cc-share check` is all `ok` on both hosts, and `cc-share frames` lists the Frame. The deletions
are already decided: the Python references, cc-view and the legacy C++ cc-panels build are gone.
