# Machine Agent

*My design, reviewed twice. It was written before it was built; cc-host is the implementation now.*

> **Now:** `cc-host` implements this design (Rust, crates/cc-host; docs/rust-host.md). The Python
> files it names (pair.py, agent.py, tagshow.py, third_party/spake2) were removed on 2026-10-03,
> but the decisions and limits below still hold.

The Frame controls a paired computer through one authenticated channel, the agent, built on the
keys pairing already exchanged. Through it the Frame finds a monitor's output (probe and align),
shows the align tags, starts and stops sessions and unpairs. The app establishes the krdp (or VNC)
connection itself, so nothing needs a login on the computer beyond pairing.

## 1. Shape

- **`control-center-agent`** is a systemd user unit on the host that runs `pair.py agent`. It
  lives in one Python file with pairing, so the host doesn't need a new language or new
  dependencies (`cryptography` and PySide6 are already there). It listens on **TCP 3399 all the
  time**.
- **One port, two doors.** The first byte of a connection decides which door it gets:
  - `0x16` (a TLS ClientHello) goes to **the agent**, which serves paired Frames only (section 2).
  - `{` goes to **pairing** (docs/pairing.md, unchanged), which is served only while a key is on
    screen. At any other time the connection is closed without an answer.
    The agent passes those bytes, unchanged, to the key screen's socket
    (`~/.config/control-center/pairing.sock`, mode 0600 in a 0700 directory, so no other local
    user can reach it), one connection at a time. The key's own rules stay in pair.py, which sees
    the bytes exactly as they were sent: junk isn't a try, and 3 wrong tries end it (M1). Without
    an agent, pair.py listens on 3399 itself, as before.
- **Frame side:** `home/agent.py` is a small client that cc-home and cc-panels use. It uses the
  stdlib `ssl` module, which can load the Ed25519 PEM keys, so it runs on the Frame's own Python
  without the container. That saves the 1–2 s cc-box start on every call.

## 2. Authentication: TLS 1.3 to the pinned host, then the Frame signs a challenge

**This changed from the first draft (tested 2026-10-02):** client certificates don't work here.
Python's `ssl` can't request a client certificate without validating it against a store, and
OpenSSL only accepts a self-signed client certificate if that exact certificate is in the store.
The host only has the Frame's key from pairing. It tried minting an anchor from that key, and
OpenSSL refused it (unknown CA). So instead, the Frame proves its key inside the TLS channel. Both
sides still pin by the exact Ed25519 key, and nothing needs re-pairing:

1. TLS 1.3 (`minimum_version`, `num_tickets = 0`, no early data). The host presents its
   certificate for `host-key`. The Frame checks that the certificate's public key equals the
   pinned `host_pk`, and if it doesn't, closes before sending anything.
2. The host sends `{"v":1, "challenge": <32 random bytes>}`.
3. The Frame answers `{"frame": <name>, "sig": Ed25519(frame-key, "cc-agent-auth/1\n" || challenge
   || host_pk || frame name)}`.
4. The host checks the name (H1) and the signature against `trusted-frames/<name>.pub`, then
   either answers `{"ok":true}` or closes. Commands are accepted only after that.

Hardening (review N1–N6):

- **Before `ok`, the host accepts only the one auth message:** at most 256 bytes, within 5 s, and
  no command is parsed before it. A failed, oversize or timed-out auth closes the connection and
  counts toward the per-address backoff and the connection cap.
- **The challenge** is 32 bytes from `secrets`. It's single use, tied to this connection, and
  expires with it (5 s).
- **The identity comes from the key, not the claim.** The name is checked against H1 before any
  file is touched. Then its key is looked up, the signature (over the bytes that include that
  name) is verified, and from there the host uses its own record of the name. An unknown name and
  a wrong signature get the same answer (closed, no reason), so there's no oracle for which
  Frames exist.
- **The signed string carries the protocol version** (`cc-agent-auth/1`).
- The signature is checked once per connection, so the per-command revocation re-check (3b) stays.
- TODO: when Python's `ssl` offers `export_keying_material`, add the TLS exporter to the signed
  string as channel binding.

The signature binds this host's key and a fresh challenge, and the Frame only signs inside a TLS
session it has pinned to that host. That means it can't be replayed somewhere else or relayed
through someone else's connection. (TLS 1.3 offers no channel binding in Python's `ssl`, so the
host pin does that job.)

For the record, here's the first draft's mutual-certificate design.

Pairing already left each side with the other's Ed25519 public key (`trusted-frames/<frame>.pub`
on the host, `trusted-hosts/<id>.json` `host_pk` on the Frame). The agent turns those into mutual
TLS without any certificate authority:

- At pairing (and once for existing pairings, see section 7), each side makes a **self-signed
  X.509 certificate for its existing Ed25519 key** (`frame-cert.pem`, `host-cert.pem`). The key
  never changes, so the pin is the key, not the certificate.
- **TLS 1.3 only**, with client certificates required. The host's trust store is the set of
  trusted Frames' certificates and nothing else. The Frame connects with `CERT_NONE`, then checks
  that the peer certificate's public key equals the pinned `host_pk`, and closes the connection
  before sending anything if it doesn't (`state=host-changed`).
- **Why TLS rather than our own handshake (or Noise):** there's no new cryptography to get right,
  both ends are in the stdlib or `cryptography`, and rustls supports exactly this (custom
  verifiers) for the Rust port. Noise_KK would be smaller, but Python has no maintained Noise
  library to vendor.
- **Prototyped (2026-10-02), with the server side as it is now:** stdlib `ssl` with Ed25519
  self-signed certificates made by `cryptography`. The paired Frame connected over TLS 1.3 with
  the host pin matching, and the host refused a stranger's certificate
  (CERTIFICATE_VERIFY_FAILED). The Frame's own Python (outside the container, OpenSSL 3.2.1) loads
  Ed25519 certificates and keys.
- (First draft) The Frame's identity came from its certificate's key. Now it comes from its
  signature, as above.

## 3. Commands

Commands are newline-delimited JSON over TLS, one request per line, with at most one connection
per Frame at a time:

    → {"v":1, "id":7, "cmd":"monitors"}
    ← {"id":7, "ok":true, "monitors":[{"index":0, "output":"DP-1", "width":5120, "height":1440,
       "mm":[1190,340], "x":0, "y":0, "rotation":1, "primary":true}]}
    ← {"id":8, "ok":false, "error":"no-such-monitor"}

| cmd | args | does | stage |
|---|---|---|---|
| `version` | | `version`, `host_id`, `host`, `login`, `protocols` (["rdp"], later "vnc"), `features` (a list of what this host offers beyond the basics; `"imu"` means a Steam Deck's controller is here, section 3c) | 1 |
| `monitors` | | shared outputs: index, output, native size, mm, layout | 1 |
| `tags` | `show <index> {bg, tags:[[id,x,y,side],...]}` / `hide` | the host **draws** the align screen itself from parameters (section 3a), full screen on that output; answers `escaped` if Esc was pressed on the host | 1 |
| `status` | | this Frame's sessions and their ports, and the guard's state | 1 |
| `unpair` | | removes this Frame on the host (its units, frames/, trusted-frames/): the Frame side of `cc-home machine unpair` | 1 |
| `session` | `start <index> [proto=rdp]` / `stop <index>` | starts this Frame's slot server for that monitor on demand; answers `{port, cert_sha256}` once it listens | 2 |
| `imu` | `start [hz]` / `stop` | streams a Steam Deck's motion sensors as `imu` events, in batches of `hz` a second (section 3c); only on a host whose `version` lists `"imu"` | 1 |
| `session` | `proto=vnc` | the same with a VNC server (docs/vnc.md) | 3 |

**The agent enforces these rules, not the Frame:**

- Every command acts only on **this Frame's** things: its slot's units and ports. Monitor indexes
  are checked against the shared list, and unit names are built from the validated Frame name and
  an integer index, so nothing from the wire reaches a shell.
- Limits: 16 KiB per line and a 10 s read timeout. Unknown commands, fields or versions get
  `ok:false`. There's one connection per Frame, and **the newest wins**: a new authenticated
  connection from a Frame closes its older one, so a half-open connection can't block the next
  (M-F).
- No passwords cross this channel. Sessions keep the per-Frame krdp login from pairing.
- Logging records the command name and the Frame name, never the content of a tag image or
  anything secret.

### 3a. Tags from parameters, not images (review A1, A2)

The host never decodes an image from a Frame. `tags show` carries what pattern.py already
computes:

    {"bg": "white" | "wait", "tags": [[id, x, y, side], ...]}   (pixels of that output)

- `bg: "wait"` is the light-grey "scanning another monitor" screen, with the host's own text.
- The host draws each tag with QPainter from a **bit table of DICT_4X4_250** (250 × 16 bits, in
  pair.py next to KEY_TAG_BITS), with the same quiet zone as pattern.py. The Frame keeps
  pattern.py and still checks that its layout reads back.
- Before drawing, the host checks for: at most 64 tags; ids 0–249; side ≥ 12 px; every tag inside
  the output; no two tags overlapping; and no tag, with its quiet zone (a quarter of its side),
  inside the banner's band.
- **Banner (A2):** the host always draws "Command Center is aligning this screen · Esc to cancel"
  in a band across the top of the output (`max(24, h // 36)` px high; pattern.py lays its tags out
  below it). The parameters can't draw anything else (no text, no images), so a paired Frame
  can't fake a lock screen.
- **One at a time, 60 s at most:** only one Frame at a time holds a host's tag screens. It gets at
  most one per monitor, so an align of two monitors on one host shows both. Each screen closes
  after 60 s unless it's shown again, and Esc, `tags hide`, the Frame's disconnect, or the agent
  stopping all close it.
- **Esc only cancels.** As I put it: "a user can reset the process without blocking a frame, as
  they could hit escape by accident". Esc closes that screen, the Frame's step ends as
  `@skipped why=escaped`, and the Frame shows "cancelled on <host>; Align again?". It can start
  again right away, with a minimum gap of 3 s between a cancel and the next show (no flashing,
  E2). For that 1 s the screen turns light grey with "Cancelled. Esc again: block this Frame".
- **Blocking is deliberate.** A second Esc within 1 s (E4: one accidental Esc never counts twice;
  Shift+Esc works too, on Linux only, since it's Task Manager on Windows) asks "Block this Frame
  for 10 minutes? Enter = block, Esc = no". After 3 cancels within 5 minutes, the host asks the
  same, with "keep allowing" as the default, which also applies after 20 s without an answer
  (E5). Prompts are drawn on top and take the keys (E3). `cc-share lock` stays the kill switch,
  and `cc-share unlock` lifts any block.
- **The cap still stops abuse:** at most 5 minutes of tags per rolling hour per Frame (E1;
  `tags_cap_min` in the host's settings.json). An align needs under a minute.

### 3b. Revocation and what the host user sees (review A3, M-D)

- **Every accepted connection gets a fresh TLS context**, built from the current
  `trusted-frames/`. Each command re-checks that the Frame is still trusted.
- `cc-share unpair <frame>` signals the agent (SIGHUP), and that Frame's live connections close
  and its sessions stop at once.
- **The host user sees** a desktop notification when a session starts ("<frame> is viewing
  DP-1"), and the banner while tags are up. `cc-share lock` pauses the agent: every command is
  refused and running sessions stop until `cc-share unlock`. `cc-share frames` lists each Frame's
  last use and whether the shared login (slot 0) is still active.

### 3c. IMU stream (a Steam Deck's motion sensors)

A Steam Deck has a gyro and an accelerometer in its controller, and the Frame can ask for them
because a paired Deck's cc-host reads them through hidraw (`crates/cc-host/src/imu.rs`). It's the
first piece of tracking a Deck in the headset (docs/deck-tracking.md). A host without that
controller doesn't offer it: `version` lacks `"imu"` in `features`, and `imu` answers `no-imu`.

    → {"v":1, "id":3, "cmd":"imu", "op":"start", "hz":90}
    ← {"id":3, "ok":true, "hz":90, "sample_hz":250, "accel_per_g":16384, "gyro_per_dps":16.384, "quat_one":32768,
       "fields":["seq","t_us","ax","ay","az","gx","gy","gz","qw","qx","qy","qz"]}
    ← {"event":"imu", "n":1, "samples":[[1496899, 15990, -414, 381, 16377, 0, 0, 0, -1666, 420, -276, -32722], ...]}
    → {"v":1, "id":4, "cmd":"imu", "op":"stop"}
    ← {"id":4, "ok":true, "stopped":true}

- **What a sample is.** The controller sends a report every 4 ms (250 Hz). Each sample is one
  row of integers, in the order `fields` says, so nothing is rounded and the lines stay small:
  - `seq` is the controller's own packet counter. It goes up by one per report, so a gap in it means a
    lost sample, and `seq` times 4 ms is the sensor's clock.
  - `t_us` is when the host read it, in microseconds since the stream started. Use it to line the
    sensor's clock up with the Frame's own. It carries the read's delay, a few milliseconds at most.
  - `ax ay az` is the accelerometer: divide by `accel_per_g` for g (it reads about +1 g on z when the Deck
    lies flat and face up).
  - `gx gy gz` is the gyro: divide by `gyro_per_dps` for degrees per second.
  - `qw qx qy qz` is the firmware's own fused orientation: divide by `quat_one`. It maps the Deck's axes
    to a world with z up and **an arbitrary heading**, since nothing in a Deck can sense north, and its
    heading drifts. The tilt (pitch and roll) is anchored by gravity and doesn't. It's all zeros for a
    moment after the stream starts.
  - The axes are the Deck's own and right-handed: x to the right edge, y to the top edge (away from you when
    you hold it) and z out of the screen.
  The decoding and the scales live in `cc_proto::imu`, with the report layout in `imu.rs`'s header comment,
  so the Frame and the host can't disagree.
- **Batching.** The host sends one `imu` event every `1/hz` second, with every sample read since the last one
  (about 3 at 90 Hz). `hz` is 10 to 125 and defaults to 90, which keeps the TLS link to roughly 90 events and
  20 KB a second. `n` counts events from 1.
- **One stream per host, and it belongs to the connection that started it.** A second Frame gets `busy`. The
  stream ends on `stop`, when that connection closes, when the Frame is unpaired, and when the agent locks, so
  a Frame that disappears can't leave the sensor on. Starting again from the same Frame changes the rate.
- **The controller's setting is put back.** The IMU is off until a setting turns it on (feature report `0x87`,
  setting 48, IMU_MODE, with the orientation, raw accel and raw gyro bits). The host reads the old value first and
  turns on only the bits it needs, so if Steam already has the gyro on, it changes nothing. When the stream
  ends it writes the old value back, unless something else changed the setting in the meantime. The Steam client
  can keep reading the same hidraw node the whole time. If cc-host is killed in the middle of a stream, nothing
  can put the setting back, so the IMU stays on until the controller resets. That only costs a little power,
  and the next stream finds it already on and leaves it that way.
- Errors: `no-imu` (no Deck controller), `busy`, `bad-rate`, `bad-op`, and `imu-failed: <why>` when the node can't
  be opened or the controller doesn't answer (the user needs to be allowed to open `/dev/hidrawN`, which a
  SteamOS login already is).
- To watch it, run `cc-home machine imu <machine>` on the Frame. It prints the heading, pitch and roll in degrees
  and the rates four times a second, so you can wave the Deck and see it follow. On the Deck itself,
  `cc-host imu-probe [seconds]` does the same locally, with no Frame, and puts the setting back.

## 4. Install (`cc-share install`): idempotent, with a checklist

`cc-share install <monitors>` does what it can as the user. For each item, it prints whether it
was done, was already in place, or what the user must run:

1. **Dependencies:** krdpserver, avahi-publish, kscreen-doctor, jq, and python3 with
   `cryptography` and PySide6. If something's missing, it prints the package command for the
   detected distro (pacman, dnf, apt, zypper) but doesn't run it.
2. **Files:** cc-share, pair.py, tagshow.py and `third_party/` (spake2, the font) go into
   `~/.local/share/control-center`. Each is copied as a new file and renamed into place, never
   rewritten in place, because a running bash unit would read the changed script.
3. **Units:** share@ (slot 0, while the shared login is kept), frame@ (per Frame), guard,
   announce (only if it was on, or with `--announce`), and agent, then a daemon-reload. Nothing
   that's running restarts unless its unit file changed, and the guard only while no viewer is
   connected.
4. **Linger:** if `loginctl show-user $USER -p Linger` says no, it prints `loginctl enable-linger`.
   It isn't strictly needed (Plasma's own session starts the units), so this is a note, not a step.
5. **Firewall:** the same as now: printed, or run under a visible sudo prompt with `--firewall`.
   It opens 3399–3449, and the marker file records that it was opened.
6. **Host id and keys:** host-id, host-key and host-cert are made if missing and never replaced.
7. **Self-check:** units active; the agent answering on localhost (a TLS handshake that it
   refuses, which proves it's listening and speaking TLS); kscreen outputs readable; krdp's
   certificate present. It prints one line per check: ✓, or what to fix.
8. **Pairing:** the last thing it says is to run `cc-share pair` and enter the code on the Frame.
   From there, discovery, pairing, connecting, align, refit, the camera, the guard, Esc and
   unpair all run over avahi, pairing, the agent and krdp.

cc-host installs itself: it copies itself to `~/.local/share/control-center`, links
`~/.local/bin/cc-share` to that copy (swapped in with a rename), and makes the krdp certificate
in Rust (`crates/cc-host/src/hostcert.rs`), so the host needs no `openssl`.

## 5. Frame side

- **cc-home:**
  - `probe` uses `monitors`.
  - Align's ready() uses `monitors` (output, mm) and `tags show/hide`.
  - `machine unpair` also sends `unpair`.
  - The agent is the only path. An unpaired machine is told to Pair, and a host without a
    reachable agent is told to run `cc-share install`.
- **cc-panels (stage 2):** before connecting a paired monitor, it asks for `session start` and
  uses the returned port and certificate. On disconnect, or after N minutes without a frame shown,
  it sends `session stop` (that's the M4 memory cost: about 800 MB per krdpserver). Autoconnect
  does the same at start.
- **Protocol preference (stage 3):** `proto=rdp|vnc` is set per machine (trusted-hosts) or per
  monitor (viewers.conf), chosen in the Machines window from what `version` reports. The defaults
  follow docs/vnc.md: RDP on KDE, GNOME and Windows Pro, and VNC elsewhere.

## 5a. Sessions on demand (stage 2; design for review)

**Why (measured 2026-10-02, read-only, on .63):** while nobody watches, krdpserver costs memory,
not CPU. The two slot-0 servers with no viewer held 725 MB and 687 MB at 0% CPU (memory kept from
earlier sessions), and the two with the Frame connected held about 400 MB at 0–12%. So a paired
Frame's servers start when it connects and stop after it leaves, because always-on is what costs.

**Agent commands:**

- `session start <index>` starts this Frame's slot server for that monitor
  (`control-center-frame@<frame>-<index>`, `systemctl --user start`), then waits up to 10 s until
  its port (3400 + 10k + index) accepts a connection on localhost. It answers `{port, ready:true}`,
  or `ok:false` with `no-such-monitor`, `not-paired` (no slot), or `not-ready` (krdp didn't come
  up, with its last journal line in `detail`). Starting one that's already running just answers.
- `session stop <index>` stops it, unless a viewer is still connected (then the idle stop takes
  care of it). It answers `{stopped: true|false}`.
- **Idle stop:** every 30 s the agent checks each running session's port for established
  connections, and stops any session that has had none for `session_idle_min` (host
  settings.json, default 10). An agent restart finds running slot units and adopts them.
- `status` lists this Frame's sessions: index, port, running, viewers, idle seconds.
- The scope is the same as every command's: only this Frame's slot, the index checked against the
  shared monitors, and unit names built from the validated Frame name and an integer.

**Host side changes:**

- Pairing stops enabling frame@ units at login (`enable` goes, and the agent starts them).
  `cc-share install` disables the login start of existing ones but leaves running ones alone.
- **The guard** restarts shares with `systemctl --user try-restart`, so a mode change never starts
  a stopped session (a plain `restart` on the pattern would).
- `cc-share frames` shows each Frame's running sessions.

**Frame side, the interface for cc-panels:** one CLI call per connect. It's simple, and it takes
about 150 ms (Python start plus the TLS handshake and challenge):

    cc-home machine session start <viewer>   → "@session <viewer> port=<p> ready=1", exit 0
                                               or "@session <viewer> state=<why>" and a message, exit 1
    cc-home machine session stop <viewer>    → "@session <viewer> stopped=1|0", exit 0

- A viewer on a paired machine (`machine=` with a host_pk) gets the call. Slot-0 and hand-added
  viewers don't (their share@ servers are always on), and the command answers `ready=1` for them
  at once, so cc-panels can call it for every remote without deciding.
- cc-panels calls `start` before connecting (at autoconnect and on Connect) and connects to the
  returned port. On Disconnect it calls `stop`, and the idle stop covers a crash or a lost network.
- If no agent is reachable, the answer is `state=no-agent` with "run cc-share install on it", and
  cc-panels shows it and doesn't connect. A session that doesn't come up
  answers `state=not-ready`.

**Built (2026-10-02), with the review's notes:**

- A host runs at most `sessions_max` sessions (settings.json, default 4); past that, it answers
  `busy` (Q1).
- A start runs in its own thread, so a slow one doesn't hold up that Frame's other commands (Q4).
- The idle stop counts only connections from the Frame's own address, so the host's viewer or a
  scan can't keep a session alive (Q5). An adopted session, whose Frame's address isn't known yet,
  counts any non-local viewer.
- Parallel starts of one session start it once, with `@session <viewer> state=starting` lines
  while waiting.
- The agent logs each start's time ("session N up in X ms"). A cold start over about 3 s would
  argue for keeping the most-used session warm (measured live once cc-panels calls it).

**Tests:** agenttest gets: start (the unit started, port waited for), start again (no second
start), stop with a viewer (deferred), idle stop after the timeout (shortened), another Frame's
index refused, and an agent restart adopting a running session. The selftest gets start and stop
through cc-home.

## 6. Stages (each shippable)

1. **Install checklist; the agent with version, monitors, tags, status and unpair; align and probe
   through the agent.** There's no Frame certificate: the Frame signs with its pairing key (section 2).
   Units stay as they are (frame@ enabled at pairing). D-050's host id rides along: `version`
   returns it, so `machine migrate` can exist after all, over the authenticated channel.
2. **Sessions on demand.** frame@ units are no longer enabled at pairing; `session start/stop`
   runs them, and cc-panels drives it. First, measure idle krdpserver CPU and memory (still
   pending) to pick the idle timeout.
3. **VNC** per docs/vnc.md, chosen per machine or monitor.

## 7. Migration

- Existing hosts get the agent at the next `cc-share install` (the files, the unit, host-cert).
  Pairings stay valid, since the host already has `trusted-frames/<frame>.pub`.
- Existing Frames don't need anything new: they sign with the `frame-key` from pairing and already
  have the host's `host_pk`, so there's no re-pairing. (Signing needs `cryptography`, which the
  Frame's own Python has, 42.0.5, so the client doesn't need the container.)
- If a paired Frame's host has no agent yet, the connection fails and cc-home says the host needs
  `cc-share install` (align: `@skipped why=no-agent`).

## 8. Decisions (first review, 2026-10-02; the second one's pending)

1. **TLS 1.3 only.** Both sides check the peer by an **exact Ed25519 public-key match**, never by
   chain or hostname. The host checks the challenge signature against `trusted-frames/<name>.pub`
   (section 2), and the Frame checks the TLS certificate's key against `host_pk`. Certificates
   can be remade at any time, because the key is what's pinned.
2. **One port.** The first-byte peek and the handshake each time out after 2 s. There are at most
   4 connections at once, and a per-address backoff after failed handshakes (1 s, doubling to
   60 s).
3. **The PNG comes from the Frame.** The agent checks its bytes (at most 2 MB) and its pixel size
   from the PNG header (at most the monitor's) before decoding. Tags close on Esc and when the
   agent stops.
4. **Slot 0 stays** (desk-wide and desk-portrait aren't paired yet). Once every monitor the user
   uses is paired, `cc-share retire-shared` is offered after asking the user. It's never removed
   automatically.
5. **Idle stop:** measured first. Until then, a slot's server stops 10 minutes after its last Frame
   disconnects (`session_idle_min` in the host's settings).
6. **The agent runs as the user, never as root.** Its unit uses systemd hardening wherever that
   doesn't break krdp control: `NoNewPrivileges=yes`, `PrivateTmp=yes`, `ProtectSystem=strict`
   with `~/.config/control-center`, `~/.cache/control-center` and the user's runtime directory
   read-write.
7. **Logs:** every command with the Frame's name, never payloads (tag images, anything secret).

### The second review (D-051), all in

- **A1:** tags from parameters (section 3a), so the host decodes no images.
- **A2:** an uncoverable banner, one show at a time, a 60 s limit (3a).
- **A3:** a fresh context per connection; unpair closes connections and stops sessions (3b).
- **M-A:** `minimum_version = TLSv1_3`, `num_tickets = 0` (no resumption), no early data.
- **M-B:** certificates valid from 2000-01-01 to 9999-12-31 (the key is the pin, so expiry only
  breaks things), plus a clock-shifted test.
- **M-C:** 3399 is now always open, so the firewall rule admits private ranges only (10/8,
  172.16/12, 192.168/16), and the timeouts, cap and backoff stay. **This is not subnet-only:** on
  a large private network or over a VPN, every private address can reach the agent's door
  (though still only paired Frames get past it).
- **M-D:** a notification, `cc-share lock`, last-used times (3b).
- **M-E:** the hardened unit is checked with Qt drawing.
- **M-F:** newest wins per Frame.
- **Lows and slot 0:** `cc-share frames` and the Machines window say "shared login still active".
- **An independent security review** before this ships beyond my own machines.

### Agent self-test (`pair.py agenttest`, on localhost)

1. A stranger's signature is refused, with the same answer for an unknown name and a wrong key.
   A signature for another host_pk (a relay) is refused, and so is a reused challenge. No answer
   in 5 s closes the connection; a name that doesn't match the key is refused; an oversize auth
   message closes; and a stranger holding a TLS session open is cut off at 5 s. The Frame refuses
   a host that isn't the pinned one.
2. A revoked Frame (unpaired while connected) is cut off and refused at once.
3. An oversize line closes the connection.
4. Bad tag parameters are refused: too many, out of bounds, overlapping, in the banner band, a bad
   id.
5. A second connection from the same Frame wins, and the first closes.
6. A wrong first byte is closed without an answer, and so is a slow peek.
7. A certificate made under a clock shifted years either way still connects.
8. The connection cap and the backoff after failed handshakes.
9. Esc cancels without blocking: a re-show 3 s later works, and one sooner gets `wait`. A double
   Esc then Enter blocks for 10 minutes, and `unlock` lifts it. Three cancels in 5 minutes ask
   the host, and no answer keeps allowing. The hourly cap.
