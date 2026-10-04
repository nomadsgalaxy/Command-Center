# VNC support (D-049)

I asked: "let's also look into supporting vnc protocols too". This started as a research-only
workflow on 2026-10-02: a design first, then an adversarial review checked against the code and
LibVNCServer master. Where the two disagree, the review wins, because it checked the sources.

The client side got built on 2026-10-03 (branch vnc), and the next section says what's there and how
to test it. Everything after it is the original design and review, kept as it was. Its line
references are to the code back then, when cc-home and cc-share were scripts; they're Rust now
(`crates/cc-home`, and cc-share is a link to `crates/cc-host`), so references like cc-home:575 or
cc-share:183 point at the old scripts, which aren't part of this repo.

## What's built

**The library.** `tools/build-libvncclient.sh <prefix> [<source dir>]` builds libvncclient from
LibVNCServer master at 42494999 (2026-07-06, after the Tight fix, review B1), static and client
only, with OpenSSL, libjpeg-turbo and zlib, and one patch: a `ccPacedRequests` flag that stops it
asking for the next update by itself (review A3). The commit, the patch and the cmake options only
live in that script, so the Fedora container and a SteamOS-native build run the same thing. It takes
the compiler and cmake settings from the environment, and it skips the build when the library is
newer than the patched source.
- Inputs: the prefix, an optional source dir (default `<prefix>/../libvncserver`), and git, cmake,
  a C compiler and the OpenSSL, libjpeg and zlib headers.
- Outputs: `<prefix>/lib64/libvncclient.a` and `<prefix>/include/rfb/*.h`.
- `cc-home install` runs it into `panels/third_party/vnc-prefix`, and install.sh adds
  `libjpeg-turbo-devel` to the container. `crates/vncclient-sys` binds the patched header with
  bindgen, and `VNC_PREFIX` points it somewhere else. Because it's static, cc-panels' RUNPATH
  doesn't change; it only gains libssl, libcrypto, libjpeg and libz as NEEDED.

**The panel source** (`crates/cc-panels/src/vnc.rs`), next to rdp.rs and in its shape:
- one session thread per panel, nice 10, reconnecting every 5 s, logging each failure once;
- encodings `tight zrle copyrect` (JPEG quality 7) and nothing else (B4), plus the cursor
  pseudo-encodings: the server sends the shape and I draw it into the framebuffer at the pointer
  (a soft cursor, with the pixels under it kept and put back);
- 32 bpp BGRX, the GPU buffers' XRGB8888, so no conversion. The rects libvncclient reports grow
  `p.stale` and feed D-045's `change`, and `gpu::write_frame` copies only that region, the same as
  RDP's (`gpu::write` is now a wrapper around it);
- **the pull model:** the next FramebufferUpdateRequest only goes out once the last picture is on
  its way to the GPU and the panel isn't Paused, away or hidden. So a Peripheral panel gets
  pictures at attention.rs's rate, and a Paused one gets none, and the host encodes nothing for it;
- input from kvm.rs, queued for the session thread because libvncclient isn't thread-safe: the
  button mask (RDP's left/right/middle to RFB's bits), the wheel as whole button 4/5/6/7 clicks
  with the fractions kept, and keys as QEMU extended keys when the server has them (KBDEXT
  converted to `0x80|sc`, A5), else US-layout X keysyms that follow Shift. A key comes up as the
  keysym it went down as;
- the pointer arrives in 0..1 of the panel, so a framebuffer that isn't the viewers.conf size
  still gets the right pixel (and the log says so);
- the poll loop checks libvncclient's buffer and `SSL_pending` before polling (C1).

**Security** (B2, B3):
- Without `tls=no` the only type offered is VeNCrypt. The password only goes out when TLS is up
  and the sub-type is X509VNC or X509Plain, whatever the server picked, and after connecting I
  check that again.
- The certificate has to match its pin: `pin=` on the line, or the paired machine's
  `cert_sha256`, the same pin format RDP uses. An unpinned one is trust on first use, like an
  unpaired RDP machine, and the log prints the `pin=` to add.
- A session that didn't ask for a password is refused, since a fake desktop would get every
  keystroke.
- `tls=no` also allows VNC's own password check and Apple's (ARD, type 30). macOS and TightVNC
  need that until there's a TLS forwarder.

**Config:** `proto=vnc` (default port 5900), `tls=yes|no` and `pin=<64 hex>` on a viewers.conf
line. cc-proto's OPTIONS knows them, so `cc-home machine add … proto=vnc` and `machine set` work.
The password comes from `passwords/<machine>`, the same store RDP uses. The card's tag shows
`host,port` as it does for RDP.

**Shared files I touched** (the rest is new): main.rs (`mod vnc`, and a VNC branch in `connect`,
`Panel::mouse`, `key`, `wheel`, `takes_input`), config.rs (`Viewer.vnc`), kvm.rs (`vnc: None` in
two test literals), gpu.rs (`write_frame`), cc-proto conf.rs (OPTIONS), cc-home install.rs,
install.sh.

**Not done yet:** `rect=` crops for a whole-desktop server (macOS, TightVNC), the clipboard
(ClientCutText), the client TLS proxy for macOS/TightVNC, and the Machines window's Add machine
(it still writes RDP lines). And cc-home's `port - 3400` maths still runs on a VNC line, which is
harmless but meaningless.

### Testing it

`tools/vnc-test-server.sh` runs TigerVNC's Xvnc inside the control-center container on
127.0.0.1:5901 (VeNCrypt X509 with a self-signed certificate, VNC password `cc-test1`), with a blue
background, xev's window logging clicks and keys at 100,100, and an xterm. It needs
`tigervnc-server-minimal xterm xsetroot xev` from dnf (test only, not in install.sh).
- `vnc-test-server.sh test` connects to it with no headset and no panel. It checks that a wrong pin
  and a server without TLS get refused, gets a picture and checks its pixels, changes the
  background and checks nothing arrives until we ask, then clicks, types Shift+A and turns the
  wheel in xev's window, and reads xev's log.
- `vnc-test-server.sh start` leaves it running and prints the viewers.conf line, with the
  certificate's pin.

### The plan for real hosts

- **macOS (Screen Sharing).** System Settings > General > Sharing > Screen Sharing on, then (i):
  "VNC viewers may control screen with password", with a password of 8 characters at most (it's
  DES). The kickstart line can set the password but not control (A7), so I'll tick that by hand.
  macOS has no VeNCrypt, so for now it's `tls=no` on the LAN, which sends the password with VNC's
  weak challenge (or ARD's DH with an account password, which I'd rather not use). The proper fix
  is the TLS forwarder below. One framebuffer covers every display, so a second monitor needs
  `rect=`. Things I can only check on a real Mac: rect-limited requests, Retina scaling, and
  whether it takes QEMU keys (likely not, so the keysym table matters).
- **Windows Home (TightVNC 2.8 or UltraVNC).** TightVNC: Server > Configuration > Server: accept
  RFB connections on 5900, set the primary password, turn off the web access and file transfers,
  and under Access Control allow only the Frame's address (or loopback once there's a forwarder).
  UltraVNC: the same, plus MSLogon off. Both are `tls=no` for now, and one framebuffer for every
  monitor, so `rect=` again. Windows Pro stays RDP, with the one-session caveat in C5.
- **wlroots (Sway, Hyprland, labwc, Raspberry Pi OS).** One wayvnc per output,
  `wayvnc -o <output> 0.0.0.0 5900+i`, with `enable_auth=true`, `certificate_file`,
  `private_key_file`, `username` and `password` in its config (mode 0600). That's VeNCrypt X509, so
  it's the one host type that works with full TLS today: `proto=vnc pin=<its sha256>`, one line
  per output, and the panel is the output, so no `rect=`.
- **X11 desktops:** TigerVNC's x0vncserver with `-SecurityTypes X509Vnc -X509Cert -X509Key`
  (C7), the same as the test server.

### What cc-aux would build on the host side (cc-host)

1. **Detect** the session type: wlroots (wayvnc), X11 (x0vncserver), macOS, Windows, and keep
   KDE/GNOME on RDP.
2. **wayvnc on wlroots:** install it (pacman/apt/dnf), make a self-signed key and certificate per
   host (`openssl req -x509`, CN = the host name, SAN = its name and IP), write
   `~/.config/wayvnc/config` (mode 0600) with `enable_auth`, the certificate, key, username and
   the pairing's per-machine password, and a user unit `cc-wayvnc@<i>.service`
   (`wayvnc -o <output> 0.0.0.0 <5900+i>`, PartOf graphical-session.target), one per output.
3. **x0vncserver on X11:** the same certificate and password, a user unit per monitor with
   `-Geometry` and `-SecurityTypes X509Vnc`, DISPLAY and XAUTHORITY set.
4. **Announce it:** `proto=vnc` in the agent's answer and the mDNS TXT, and the monitor key as
   `mN=output,WxH,port[,@x,y]`, so the Frame's discover writes `proto=vnc`, the port and later
   `rect=`. The certificate's SHA-256 goes into the pairing reply, so trusted-hosts'
   `cert_sha256` pins it the way krdp's is pinned.
5. **Pairing:** the VNC password is a random per-machine one from pairing (8 characters for
   macOS/TightVNC), written to the host's config and to `passwords/<machine>` on the Frame.
6. **macOS (later):** the kickstart password, a check that Screen Sharing with control is on (and
   the "Remote Management on, Screen Sharing off" state after a reboot), and the TCC screen
   recording right, which it can only check and explain. Then a TLS forwarder (stunnel or socat
   with mTLS against the Frame's pairing certificate, B5) and a pf anchor so 5900 only answers
   loopback.
7. **Windows Home (later):** a PowerShell setup: TightVNC's silent MSI, LoopbackOnly, and stunnel as
   the forwarder.

On the Frame side, the macOS/TightVNC forwarder then needs a small TLS client in cc-panels, or
libvncclient's `rfbClientConnect`/`rfbClientInitialise` split with our own socket (C4). I'd take
that once a forwarder exists to test against.

## The design and review (2026-10-02)

### 1. Why add VNC, and where RDP stays the default

**The hosts VNC opens up:**
- **macOS.** This is the main reason: macOS has no RDP server, but its built-in Screen Sharing speaks
  RFB to third-party clients.
- **wlroots desktops** (Sway, Hyprland, labwc, Raspberry Pi OS) through wayvnc. It runs one instance
  per output, so it maps 1:1 onto today's one-krdp-per-monitor layout.
- **Windows Home**, which has no RDP host, through TightVNC.
- **Plain X11 desktops** through x11vnc or x0vncserver.

**RDP stays the default** on:
- KDE (krdp already works, per monitor, with H.264 and no prompt)
- GNOME (g-r-d's RDP backend; its VNC only sends the primary monitor)
- Windows Pro, Enterprise and Education (built-in RDP)

RDP keeps H.264, TLS everywhere, audio and a rich clipboard. VNC's codecs (Tight/JPEG, ZRLE) are fine
for desktop work, but not for 4K video: I estimated a full repaint at 55–85 ms of CPU on the Frame.

**VNC does have one advantage here:** RFB is pull-based, so a panel that's out of view can just stop
asking for updates. That saves network and host encoding, which krdp can't do because it ignores
Suppress Output (main.rs:951).

### 2. Client and how it plugs in

**Library: libvncclient from LibVNCServer master, pinned at a commit on or after 2026-05-29** (not
0.9.15: see the review, B1). It gets vendored into a `VNC_PREFIX` the same way FreeRDP is (CMake with
OpenSSL, JPEG, zlib and LZO on; server, examples and SDL off), with our own bindgen over
`rfbclient.h`. It's the only library that covers all of:
- Tight/ZRLE/CopyRect
- ExtendedDesktopSize
- VeNCrypt X509
- ARD type 30
- QEMU extended keys
- per-rect callbacks that map 1:1 onto `on_end_paint`

**A decision for you: the licence.** libvncclient is GPL-2.0+. The design assumed the repo source
could stay MIT while a distributed cc-panels binary became a GPL combined work (GPLv3 in practice,
because FreeRDP is Apache-2.0). The repo isn't MIT any more, it's OCL v1.1 + SWAtt v1 (NOTICE.md), so
this question needs a fresh look against that licence before any code gets written.
- If the binary has to stay under the repo's own licence, the fallback is vnc-rs (MIT/Apache). It
  costs a Tokio runtime plus about 300 lines of our own auth (VeNCrypt through rustls, ARD DH).
- gtk-vnc (LGPL) is the licence-safe option that already has full auth. The downside is a GLib main
  loop on every panel thread.

**The seam** (from the code map): one new `crates/cc-panels/src/vnc.rs` next to rdp.rs.
`Source::Rdp` keeps meaning "remote", so the taskbar, control, machines, grab and attention don't
change.

- **Dispatch:** add a `proto` field to `Viewer` (config.rs:15/49, `NONE` at main.rs:97) and branch on
  it in five places: `connect` (main.rs:637, which spawns `vnc::run`), and `Panel::mouse`, `key`,
  `wheel` and `takes_input` (main.rs:284–326).
- **gpu.rs:** `write(p, &rdpGdi)` becomes `write(p, &Frame{data, stride, w, h, x0, y0})`, and
  rdp.rs:363 builds the Frame from the gdi. `x0,y0` is the crop offset that shifts the source rect in
  `copy_rect` (about 40 lines).
- **The thread (`vnc::run(p, password)`):**
  - nice 10;
  - `rfbGetClient(8,3,4)` with the format set to 32 bpp, red shift 16, green 8, blue 0,
    little-endian. That's BGRX in memory, which matches XRGB8888, so there's no conversion;
  - `MallocFrameBuffer` hooked, so the framebuffer is (re)allocated under `p.lock`;
  - `rfbInitClient`, then `p.connected = true`;
  - **the loop:** `poll()` on `cl->sock` plus `GetEventFileDescriptor(p.wake)` with the timeout from
    `gpu::wait(p)`; then `ResetEvent`; then drain the input queue; then, under `p.lock`, run
    `HandleRFBServerMessage`; then `gpu::write`;
  - it reconnects while `p.live()`, backing off 5 s. On exit it sets `connected = false`, nulls the
    handle under `p.lock` (same as `detach`) and calls `rfbClientCleanup`;
  - `rdp::poke` works unchanged, because `p.wake` is still the WinPR event.
- **Damage and the D-045 change measure:**
  - `GotFrameBufferUpdate(x,y,w,h)` clips the rect to the panel's crop, grows `p.stale[0..2]`, and adds
    the area to a per-update counter.
  - `FinishedFrameBufferUpdate` calls `p.change.fetch_max(min(1000, area*1000/(cw*ch)))` and sets
    `p.dirty`. That's the same ~10 lines as rdp.rs:34–45, with the crop's w×h.
- **Attention:** send the next incremental `FramebufferUpdateRequest`, limited to the panel's rect,
  only when `attention::due` says so, and never turn on ContinuousUpdates. Then a Paused VNC panel
  really does stop costing anything.
- **Input:** libvncclient isn't thread-safe, and the session thread also writes requests. So
  `Panel::mouse`, `key` and `wheel` push into a `Mutex<Vec<VncInput>>` on the panel and `poke(p)`, and
  the session thread drains it and sends. `takes_input` is `p.connected`. No library patch needed, at
  the cost of about one poll round-trip of latency.
  - **Pointer:** keep a button mask. `PTR_FLAGS_BUTTON1/2/3` map to mask bits 1/3/2
    (left/right/middle). DOWN sets the bit, otherwise it's cleared. MOVE resends the mask at the new
    x,y.
  - **Wheel:** divide by 120 and undo main.rs:315's horizontal sign flip. Each notch is a press and
    release of button 4/5, or 6/7 for horizontal.
  - **Keys:** if the server advertises QEMU Extended Key Event (-258), send
    `SendExtendedKeyEvent(keysym, kvm::scancode(code))`, which reuses kvm.rs. Otherwise use a new
    evdev→keysym table (US layout). Key repeat resends the down.
- **Resize:** `MallocFrameBuffer` gets called again on DesktopSize or ExtendedDesktopSize. Reallocate
  under `p.lock`, and call `p.set_size` only when the panel has no `rect=`. This also shows up a gap
  that RDP already has: `set_size` is never called for a remote.
- **Cursor:** for the milestone, let the server draw the cursor into the frame (wayvnc `-r`) and don't
  advertise the cursor pseudo-encodings. A client-side cursor overlay can come later.
- **Clipboard:** not in the milestone. Later, move `CLIP` and its fan-out out of rdp.rs and add a
  branch that sends ClientCutText to VNC panels.
- **Several monitors in one framebuffer** (macOS, TightVNC): the milestone uses one connection per
  panel, each one asking only for its `rect=`. That costs a full framebuffer per panel (about 33 MB at
  4K) and maybe duplicate capture on the server. One shared connection driving N panels breaks the
  one-thread-per-panel model, so I'd leave it until measurements say we need it.

### 3. viewers.conf, discovery and pairing

**viewers.conf** gets an option, not a `vnc://` URL, because cc-home already keeps unknown `key=val`
options and its regex requires `user@host:port`.
```
mac1-0  user@mac.local:5900  0  2560x1440  proto=vnc rect=0,0,2560,1440
sway-1  user@pi.local:5901   1  1920x1080  proto=vnc
```
- No `proto` means rdp. With `proto=vnc` the default port is 5900.
- `rect=x,y,w,h` crops a combined desktop. `user` is only used for ARD or VeNCrypt Plain.
- cc-home's `port - 3400` (cc-home:575, :1041) and machines.rs's `3400 + n` (:194) have to skip
  `proto=vnc` and use `screen`/`rect` instead.

**mDNS TXT** (cc-share:183–193):
- Add `proto=rdp|vnc`. When it's missing, assume rdp, so existing hosts don't change.
- Extend the monitor key to `mN=output,WxH[,port][,@x,y]`: a per-instance port for wayvnc and x11vnc,
  and an offset for a combined framebuffer. cc-home's `discover()` can then write `rect=` straight from
  that.

**Pairing** ([pairing.md](pairing.md)):
- Use per-machine secrets, as already planned (`passwords/<machine>`).
- Add `certs/<machine>.pem`, the host's self-signed certificate, pinned at pairing. libvncclient uses
  it as `x509CACertFile`, which works as a pin.
- The pairing reply carries `proto`. The VNC password is a random value from pairing, not the user's
  account password, which keeps pairing.md:90 true.

### 4. Host setup in cc-share, per OS

**The rule:** never send a VNC password or session over the LAN without TLS. Either the server does
VeNCrypt X509 itself, or it binds to loopback and sits behind a TLS forwarder.

- **Linux, wlroots (the first target):**
  - Install `wayvnc` (pacman/apt).
  - cc-share generates a self-signed key and certificate (`openssl req -x509`) and writes
    `~/.config/wayvnc/config` with `enable_auth=true`, `certificate_file`/`private_key_file`,
    `username` and `password`.
  - It adds a user unit `cc-wayvnc@.service`
    (`wayvnc -o <output> -S <sock-i> -r 0.0.0.0 5900+i`, PartOf graphical-session.target) and
    announces `proto=vnc`.
  - wayvnc does VeNCrypt X509 itself, so there's no forwarder.
- **Linux, X11:** x11vnc `-clip xineramaN -rfbport 5900+N` with SSL/VeNCrypt on the same certificate
  (x11vnc's VeNCrypt flags still need checking), or x0vncserver `-Geometry`. Either runs as a user unit
  with DISPLAY and XAUTHORITY set.
- **KDE:** krdp stays. krfb-virtualmonitor (a virtual "VR monitor" with no prompt) is an option later,
  but it has no TLS, so it would go behind the forwarder.
- **GNOME:** use g-r-d's RDP backend.
- **macOS (phase 2):**
  - `kickstart -activate -configure -access -on -clientopts -setvnclegacy -vnclegacy yes -setvncpw -vncpw <pairing pw> -restart -agent -privs -all`
  - Limit the client to auth type 2 (`clientAuthSchemes`), so the account password and ARD are never
    used.
  - The session isn't encrypted for non-Apple clients, so it needs a TLS forwarder on the host
    (`socat openssl-listen` / stunnel from brew, later the CC host agent) plus a pf anchor that blocks
    5900 except from loopback. On the Frame side, a small TLS proxy in cc-panels (the openssl crate
    against the system OpenSSL 3.2.1) pumps bytes to libvncclient over a socketpair or a localhost
    port.
  - Since 12.1 the TCC screen-capture right can't be granted by a script. cc-share can only check it
    and tell the user which box to tick.
  - It also has to catch the "Remote Management on, Screen Sharing off" state after a reboot (seen on
    macOS 26.6.1).
- **Windows Home (phase 3):** a PowerShell counterpart to cc-share: a silent TightVNC MSI install,
  `-sharefull`, LoopbackOnly, and a stunnel service as the TLS forwarder, with the same client proxy as
  macOS. Windows Pro uses RDP.

### 5. Risks, unknowns and the first milestone

**Risks and unknowns:**
- **Licence:** GPL libvncclient against the repo's licence (section 2). Settle it before writing code.
- **CPU decode cost on the Frame:** Tight/JPEG at 4K against today's hardware H.264 path. The
  milestone measures it; it's the main performance unknown.
- **Pacing with libvncclient:** I believe `HandleRFBServerMessage` sends its own incremental request
  after every update. If it does, the vendored copy needs a one-line patch so attention.rs can pace the
  requests (there are no request hooks: review, A3).
- **Pinning:** does `x509CACertFile` pinning with a self-signed certificate also enforce a hostname
  match? If it does, issue the certificate for `<host>.local` and its IP.
- **macOS:** does it honour rectangle-limited requests, do all the displays show up as one
  framebuffer, and does ExtendedDesktopSize report the screen layout? All three need a real Mac.
- **Keyboard:** macOS and TightVNC don't have QEMU extended keys, so the keysym fallback depends on the
  keyboard layout.
- **Memory:** one connection per panel on a combined desktop costs extra memory and extra capture on
  the server.
- **Unattended start:** macOS's TCC, its reboot fragility and krfb's prompts are host-side failures
  that cc-share can only detect.

**The smallest end-to-end milestone (one Linux host plus the Frame):**
1. Build libvncclient into `VNC_PREFIX` on the Frame.
2. Write `vnc.rs` with the loop, framebuffer, damage/change, pointer and wheel. Keys go through QEMU
   extended keys only.
3. Add the `proto=vnc` option and the five dispatch branches, plus the `Frame` refactor in gpu.rs.
4. **Host:** one Linux box running wayvnc with VeNCrypt X509 and the pinned certificate. If neither
   host runs wlroots, use headless sway (`WLR_BACKENDS=headless sway` plus wayvnc) on a KDE host,
   which leaves its desktop alone.
5. **Configure it by hand:** copy the certificate and a viewers.conf line over manually. No cc-share,
   mDNS or pairing changes yet.

**Pass criteria:**
- the panel shows up and updates
- the laser and keyboard control the host
- a panel that's out of view stops asking for updates
- D-045's `change` moves when the content changes
- a log line with the per-update decode time at 4K

### 6. Rough sizes

| Piece | Size |
|---|---|
| Vendored libvncclient build, build.rs and bindgen | S (about 0.5–1 day) |
| gpu.rs `Frame` refactor plus crop offset | S (about 40 lines) |
| config `proto`/`rect` plus 5 dispatch branches | S (about 60 lines) |
| vnc.rs loop, framebuffer, damage, pacing, resize, connect/disconnect | M (about 300–350 lines) |
| Input queue, button mask, wheel, extended keys | S–M (about 120 lines); the keysym fallback table adds about 100 |
| **Milestone total** | about 1 week |
| cc-home and machines.rs port maths, `discover()` for `proto` and `rect` | S |
| cc-share Linux (wayvnc/x11vnc units, certificate generation, TXT `proto`) | S–M |
| Pairing: certificate pin plus per-machine VNC password | S, on top of the pairing work in progress |
| Client TLS proxy plus host forwarder (macOS, TightVNC, krfb) | M (about 150 lines plus host scripts) |
| macOS cc-share plus testing on a real Mac | M |
| Windows PowerShell setup | M–L |
| Clipboard moved out of rdp.rs, plus ClientCutText | S–M |

Not proposed for now:
- **One shared connection for N panels:** L. Only if per-panel connections cost too much on a Mac.
- **wayvnc's H.264 decode:** M–L. Only if Tight's decode cost turns out too high.

Key files: crates/cc-panels/src/{rdp.rs,gpu.rs,main.rs,kvm.rs,config.rs},
crates/cc-panels/src/control/machines.rs,
cc-home, cc-share,
docs/pairing.md.


## What we'd give up against RDP (our side)

- **krdp's patches have no VNC counterpart to carry over, and don't need one:**
  - `krdp/pointer-offset.patch` fixes the pointer position per monitor in krdp. A per-output VNC
    server (wayvnc) maps pointer coordinates natively. A whole-desktop server (macOS, TightVNC) needs
    the client to add the panel's `rect=` offset to every pointer event.
  - `krdp/clipboard.patch` syncs krdp's clipboard. VNC's ServerCutText/ClientCutText is Latin-1 text
    only. UTF-8 needs the Extended Clipboard pseudo-encoding, which only some servers support
    (TigerVNC, recent TightVNC, not macOS), so non-ASCII text might not make it across.
- **H.264:** our RDP path is software libavcodec (review, A1). VNC's Tight/JPEG may keep up at
  2560x1440, but 4K video will likely cost more CPU. The milestone measures it.
- **Monitors per server:** krdp serves one session per monitor. macOS, TightVNC and g-r-d's VNC serve
  one framebuffer for the whole desktop, so each panel crops its `rect=` out of it (one connection per
  panel in the milestone, a shared connection later: review, C5). wayvnc runs one instance per output.
- **Suppress Output:** VNC can really stop sending to a paused panel (pull pacing), while krdp ignores
  Suppress Output. That one's a gain, not a loss.
- **Audio, drive and printer redirection:** RDP-only, and we don't use them today.

## Status

The review's corrections stand. I only edited the design above where it named 0.9.15 and the request
hooks that don't exist. The client milestone is built (see "What's built" above); the hosts aren't.

---

## Adversarial review: VNC design for Command Center

I checked the design against the code two commits later (after the pairing review and the prefs
window), so line numbers have moved a little; Suppress Output,
for example, is now at main.rs:953. I also checked it against the LibVNCServer git tree (a scratch
clone, master as of 2026-07-06, tag LibVNCServer-0.9.15), the wayvnc man page and Apple's
kickstart notes. Nothing was edited, installed or run.

### A. Wrong claims

1. **"Today's hardware H.264 path" is wrong.** install.sh:70 builds FreeRDP with
   `-DWITH_FFMPEG=ON -DWITH_VIDEO_FFMPEG=ON` and no VAAPI, inside a Fedora 44 distrobox. So RDP H.264
   is software libavcodec plus a CPU YUV conversion, and rdp.rs:323 notes that the colour conversion
   is serialised. Tight/JPEG (libjpeg-turbo NEON) is up against software H.264, not a hardware
   decoder. Benchmark both on the same content before calling VNC "not fine for video". The 55–85 ms
   figure is an unmeasured estimate.

2. **The licence isn't a blocker.** No cc-panels binary is distributed: install.sh:79 runs
   `cargo build --release` on the device, from source. GPL obligations start with binary
   distribution, and the MIT source tree isn't affected. The binary already links RPM Fusion FFmpeg
   (install.sh:48), which is a GPL build itself. Drop "decide before any code"; vnc-rs and gtk-vnc
   aren't needed for licence reasons.

   (Note from the docs pass on 2026-10-03: the source tree is OCL v1.1 + SWAtt v1 now, not MIT, and
   the Frame still builds cc-panels from source in its container. The point about binary
   distribution still applies, but the compatibility with the new licence hasn't been checked.)

3. **"The 0.9.15 request hooks" don't exist.** rfbclient.c:2572 always calls
   `SendIncrementalFramebufferUpdateRequest` at the end of every FramebufferUpdate, and it does that
   before `FinishedFrameBufferUpdate`, so no callback can stop it. Attention pacing needs one of:
   - a vendored one-line patch (a client flag that skips that call), or
   - a hack: clear the FramebufferUpdateRequest bit in `client->supportedMessages`, which
     `SendFramebufferUpdateRequest` checks, and set it only around our own calls. That's fragile,
     because a SupportedMessages rect overwrites the bits.

   The patch is the honest choice. 0.9.15 does add `rfbClientSetUpdateRect()` (sub-rect requests),
   but the full framebuffer is still allocated (`client->width*height`).

4. **ExtendedDesktopSize doesn't give you a screen layout.** rfbclient.c:2170 keeps only the last
   screen (`client->screen = screen`), and master hasn't changed that. So "whether ExtendedDesktopSize
   reports the screen layout" on macOS is moot without a patch. The layout has to come from the host
   via TXT `@x,y`, which the design already proposes, so take ExtendedDesktopSize off the "only library
   that covers" list. Also, NewFBSize triggers a full non-incremental request that ignores
   `updateRect` (around line 2152).

5. **QEMU extended keys don't "reuse kvm.rs" as is.** `kvm::scancode` marks extended keys as
   `0x100 | sc` (KBDEXT, kvm.rs:87). RFB's QEMU keycode wants an e0-prefixed key as `0x80 | sc` (e0 48
   becomes 0xC8, for example). That needs a conversion step, with Pause/PrtSc as special cases.

6. **The wheel step is wrong as written.** Panel::wheel (main.rs:315) gets raw notches, and the ×120
   and the horizontal sign flip happen inside the Rdp arm. A VNC arm branches before that, so there's
   nothing to "divide by 120 / undo". It does need to add up fractional notches into whole button
   4/5/6/7 clicks.

7. **The macOS kickstart line doesn't give control.** Since Mojave, Screen Sharing turned on through
   kickstart is observe-only, and since 12.1 control needs the user to turn it on in System Settings
   (or MDM). cc-share can set the VNC password, but it has to tell the user to turn on Screen Sharing
   and "VNC viewers may control screen with password". Also, the legacy VNC password is DES with at
   most 8 characters, so a random pairing password has at most about 48 bits.

8. **Build details:**
   - The build runs inside the Fedora distrobox (install.sh step 1), not "on the Frame" against the
     system OpenSSL 3.2.1. Add `libjpeg-turbo-devel` (and LZO, or use the bundled miniLZO) to
     install.sh's package list.
   - Turn these off explicitly: `WITH_LIBVNCSERVER`, `WITH_GNUTLS` (otherwise GnuTLS gets picked for
     TLS when it's found), `WITH_GCRYPT`, `WITH_SASL`, `WITH_SYSTEMD`, `WITH_FFMPEG`, `WITH_QT`,
     `WITH_GTK`, `WITH_SDL`, `WITH_LIBSSHTUNNEL`, `WITH_WEBSOCKETS`, `WITH_EXAMPLES`, `WITH_TESTS`.
   - Build it static (`BUILD_SHARED_LIBS=OFF`).
   - ARD/MSLogon don't need gcrypt; OpenSSL works as the crypto backend.

9. **`set_size` is called for a remote, but only once.** `Panel::fill` (main.rs:231) calls it at fill
   time. The real gap is resizing: `on_desktop_resize` never calls it.

### B. Security gaps (the most important part of this review)

1. **Don't vendor 0.9.15.** It has a pre-auth heap out-of-bounds write in the Tight decoder that any
   rogue or MITM server can reach with one FramebufferUpdate. The fix is the master merge of
   2026-05-29 ("Merge commit from fork", tight.c, numRows clamp). Master also has a Tight gradient
   overflow fix (2026-05-06) and UltraZip bounds checks, and no release has them yet. Vendor master at
   a pinned commit on or after 2026-05-29.

2. **VeNCrypt can be downgraded to a cleartext password.** `clientAuthSchemes` only filters the
   top-level type (rfbclient.c:523, `!subAuth`). Inside VeNCrypt, ReadVeNCryptSecurityType
   (tls_openssl.c:579–599) accepts NoAuth, VncAuth and plain `rfbVeNCryptPlain`. A spoofed mDNS host or
   a MITM can offer VeNCrypt with Plain and get the username and password in cleartext. The fixes we
   need:
   - Set `SetClientAuthSchemes({rfbVeNCrypt})`.
   - In `GetCredential` and `GetPassword`, return NULL unless
     `client->subAuthScheme == rfbVeNCryptX509Plain` and `client->tlsSession != NULL`.
     `subAuthScheme` is set before the credential callback.
   - After `rfbInitClient`, assert the same thing, and never send input to a session without TLS. A
     NoAuth fake desktop would capture keystrokes.

3. **Pinning:** 0.9.15's OpenSSL path uses `x509CACertFile` plus
   `X509_VERIFY_PARAM_set1_host(serverHost)`. pairing.md says discover's addr is the IP, so a
   self-signed certificate would have to match that host string, and `set1_host` does DNS-name
   matching. Master (2026-02-08) adds SHA-256 fingerprint pinning
   (`rfbTLSExpectedFingerprintIndex` plus `GetX509CertFingerprintMismatchDecision`). Use that, so it
   matches pairing.md's krdp model ("certificate pinned via the sealed fingerprint"). It's one more
   reason to vendor master.

4. **Shrink what the decoder exposes.** Set `appData.encodingsString = "tight zrle copyrect"`, so
   ultra, ultrazip, zywrle, hextile, rre and the rest are never advertised.

5. **The host TLS forwarder (macOS, TightVNC, krfb) needs client authentication.** Without it, any
   device on the LAN can reach the weak 8-character DES VNC auth through the forwarder. Use mTLS with
   the Frame's pairing certificate (`socat ... verify=1 cafile=`, or stunnel `verify=2`).

6. **wayvnc's `config` holds the password in plaintext.** Make it mode 0600. It binds 0.0.0.0, and
   pairing.md rules out silent-sudo firewall changes, so document that exposure.

### C. Missing work and runtime pitfalls

1. **The poll loop will stall under TLS.** `ReadFromRFBServer` buffers into `client->buf`, and OpenSSL
   holds whole records, so `poll(cl->sock)` misses data that's already readable. Master fixed the same
   bug in `WaitForMessage` on 2026-03-09. Before polling, check
   `client->buffered > 0 || SSL_pending(client->tlsSession) > 0`.

2. **`HandleRFBServerMessage` blocks inside a message until all its bytes arrive.** A multi-MB 4K
   update over Wi-Fi holds `p.lock` that whole time. That's only acceptable because input is queued
   (the design's queue). No other `p.lock` user blocks the main loop (only rdp.rs:66/353/399 take it),
   but say so explicitly.

3. **The "decode time" pass metric can't be measured the way it's written.** libvncclient interleaves
   socket reads with decoding. Log `CLOCK_THREAD_CPUTIME_ID` around `HandleRFBServerMessage` (CPU, not
   wall time), plus bytes per update.

4. **The client TLS proxy for macOS and TightVNC might be simpler than a pump thread.** Master's
   `rfbClientConnect`/`rfbClientInitialise` split (2025-03-03) lets you hand libvncclient your own end
   of a socketpair. Check that before writing a separate proxy.

5. **The one-connection-per-monitor model breaks for more than VNC.** Windows Pro RDP allows one
   session per user, so a second per-monitor connection kicks the first one off. g-r-d's desktop
   sharing should be checked for the same limit. So "one shared connection driving N panels with
   `rect=` crops" is needed for most non-KDE hosts, RDP included, not an optional L item. Keep it out
   of the milestone, but don't list Windows Pro as simply "RDP stays default".

6. **wayvnc:** `enable_auth=true` requires `certificate_file`, `private_key_file` and `password` (man
   page). Master has `-a/--desktop` (all outputs in one framebuffer), but per-output instances are
   still the right choice. wayvnc only runs on wlroots compositors (no KWin or Mutter screencopy), and
   none of my current hosts (KDE) would use it, so the Linux milestone only validates the
   client.

7. **A simpler X11/test option:** TigerVNC's Xvnc or x0vncserver does VeNCrypt X509Plain natively
   (`-SecurityTypes X509Plain -X509Cert -X509Key`) and supports QEMU extended keys. Prefer it to
   x11vnc, whose VeNCrypt flags the design leaves unverified. It's also a fine test host if headless
   sway turns out fiddly.

### D. Simpler and YAGNI cuts for the milestone

- Drop `rect=` and the crop offset. Per-output wayvnc means the panel is the framebuffer, so
  `x0,y0=0`, and `gpu::write` only needs a `Frame{data,stride,w,h}` instead of `&rdpGdi`. Add the crop
  in the macOS phase.
- Drop the keysym fallback table (already put off), the cursor pseudo-encodings and the clipboard, as
  the design already says.
- Drop vnc-rs and gtk-vnc from the discussion, since the licence reason for them is gone (A2).

### Revised first milestone (one Linux host plus the Frame)

1. **Vendor libvncclient from master, pinned at a commit on or after 2026-05-29** (the Tight OOB fix,
   fingerprint pinning, the buffered-read fix). Build it static into `VNC_PREFIX` inside the Fedora
   distrobox via install.sh: OpenSSL on; GnuTLS, gcrypt, SASL, systemd, server, examples and tests
   off; add libjpeg-turbo-devel. Apply one local patch, a flag that skips the automatic incremental
   request at rfbclient.c:2572. Use bindgen over `rfbclient.h`.
2. **Write `vnc.rs`** with:
   - the 32 bpp BGRX format and the encodings `"tight zrle copyrect"`
   - auth limited to VeNCrypt, with `GetCredential` refusing unless the session is X509Plain and TLS
     is up, and SHA-256 fingerprint pinning
   - a `poll` loop that checks `client->buffered`/`SSL_pending` first
   - `MallocFrameBuffer` under `p.lock`
   - `GotFrameBufferUpdate`/`FinishedFrameBufferUpdate` driving the stale rects and D-045's `change`
   - incremental requests sent only when `attention::due`, and none while Paused
   - a queued input path: the button mask; a whole-notch wheel with fractional accumulation; QEMU
     extended keys with KBDEXT converted to `0x80|sc`
3. **Wiring:** a `proto` field on `Viewer` and branches in `connect`, `mouse`, `key`, `wheel` and
   `takes_input`, plus a `gpu::write(&Frame)` refactor without the crop.
4. **Host:** headless sway plus wayvnc (per output, `enable_auth`, a self-signed certificate, `-r`) on
   one KDE box, or TigerVNC Xvnc with X509Plain. Copy the certificate fingerprint and the viewers.conf
   line over by hand.
5. **Pass criteria** (the original five, plus two security checks):
   - the panel shows up and updates
   - the laser and keyboard control the host
   - a panel that's out of view sends no requests (check the host's traffic)
   - `change` moves when the content changes
   - per-update thread-CPU time and bytes are logged at 4K, next to the same content over krdp with
     software H.264
   - a server offering only None/VncAuth/Plain, or a wrong certificate, is refused before any
     credential or input is sent
   - Tight decoding is fuzzed with the patched commit (a smoke test only)

Sizes: the milestone is still about a week. The libvncclient patch, the auth guard and the TLS
buffered check add about half a day, and dropping the crop saves about the same.

Key paths:
- install.sh (lines 36–79)
- crates/cc-panels/src/rdp.rs (34–45, 300–400)
- crates/cc-panels/src/gpu.rs (242–297)
- crates/cc-panels/src/main.rs (175, 231, 284–326, 953)
- crates/cc-panels/src/kvm.rs (86)
- the LibVNCServer source (libvncclient/rfbclient.c, tls_openssl.c, sockets.c), from a scratch clone that wasn't kept

Sources: https://raw.githubusercontent.com/any1/wayvnc/master/wayvnc.scd, https://scriptingosx.com/2018/09/apple-remote-desktop-screen-sharing-and-mojave/, https://www.macminivault.com/faq/switching-from-remote-management-to-screen-sharing/, https://github.com/LibVNC/libvncserver (NEWS.md, commit log since LibVNCServer-0.9.15).
