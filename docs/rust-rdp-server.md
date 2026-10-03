# Could a Rust RDP server replace krdp?

A feasibility look, written 2026-10-03. Nothing here is built.

**Short answer:** yes, but not soon, and not as pure Rust. IronRDP's server side (Rust,
MIT/Apache-2.0) handles the protocol itself: TLS and CredSSP, input, clipboard and display
control. Three pieces would still need C libraries at runtime:

- **The video codec.** There's no production-ready H.264 encoder in pure Rust, so it'd be
  openh264 or VA-API.
- **Probably reading PipeWire frames.** libpipewire, unless the young pure-Rust
  `pipewire-native` is good enough.
- **H.264 over RDP itself.** It goes through IronRDP's EGFX support, which is "experimental"
  and community-maintained.

The hosts already have these libraries, since krdp uses them, so a Rust server could load them
at runtime and still be one binary to copy. **My take:** not now. krdp works, and getting the
Frame stable comes first. After the final live test, a 1-2 week spike with clear pass criteria
(below) would tell us whether it's worth going further.

## What krdp does for us today

- **Capture:** `--plasma` gets each monitor through KWin's own screencast protocol
  (`zkde_screencast_unstable_v1`, through KPipeWire). A `.desktop` grant allows it, so nobody
  gets a permission prompt on every connect.
- **Encoding:** H.264 through KPipeWire (VA-API where the GPU has it, else software). The
  Frame's FreeRDP client (cc-panels) decodes it over EGFX.
- **Input:** KWin's fake-input protocol, under the same grant.
- **Our patches** (krdp/): the pointer offset, the clipboard, and window streams (`--window`,
  a per-host build of krdp).
- **How it runs:** one krdpserver process per monitor per Frame. The password goes on its
  command line, where `ps` shows it (the known L1). It needs Qt and KF6, and the patched builds
  have to be made on each host.

## What IronRDP's server gives, and what's missing

| Need | IronRDP | Gap / how it would be filled |
| --- | --- | --- |
| Protocol, TLS, CredSSP, channels | `ironrdp-server`, `ironrdp-acceptor` | none |
| Keyboard and mouse in | `RdpServerInputHandler` trait | KWin fake-input via `wayland-client` (pure Rust, as cc-host's tag screen already speaks Wayland), same `.desktop` grant as krdp; or libei/EIS later |
| Screen frames | `RdpServerDisplay` trait (we feed it) | KWin's screencast protocol (pure Rust over Wayland) hands out a PipeWire node; reading it needs libpipewire (`pipewire-rs`, dlopen) or `pipewire-native` (pure Rust, new) |
| H.264 to the Frame | EGFX, **experimental**, community-maintained | encoder: openh264 (Cisco's binary for patent cover) or VA-API (libva), both loaded at runtime; must interoperate with FreeRDP 3's AVC420 decoder (cc-panels) |
| Other codecs | bitmap, RemoteFX, NSCodec, QOI (core) | a fallback, too heavy for 5120x1440 at 60 Hz |
| Clipboard | `cliprdr` | the host side: Klipper over D-Bus (zbus, as cc-host already uses) or a data-control protocol |
| Cursor | pointer PDUs | the screencast's cursor metadata; our pointer-offset fix becomes plain code |
| Window streams | none | KWin's screencast can stream one window (krdp's `--window` patch does exactly this), so it's native in our own server |

Others have done this already: lamco-rdp-server and cosmic-ext-rdp-server are IronRDP-based
Wayland servers with H.264 over EGFX, so the path works. lamco goes through the desktop portals
(ScreenCast, RemoteDesktop), which on KDE means a permission dialog every session. We'd use
KWin's privileged protocols instead, like krdp does.

## What we'd gain

- **One process for every monitor and Frame,** inside cc-host or next to it. No Qt, and no
  patched krdp to build on every host, which is the friction that started the Rust port.
- **The password stays in memory,** never on a command line, which fixes L1. Each Frame's
  login gets checked in-process.
- **Window streams, pointer offset and clipboard become our code,** not patches carried
  against KDE's releases.
- **The same server core for D-043's mac and Windows hosts.** Only capture, encode and input
  differ per platform: ScreenCaptureKit and VideoToolbox on macOS, Desktop Duplication and Media
  Foundation on Windows.

## Risks

- **EGFX/H.264 in IronRDP is experimental.** Interop with FreeRDP's client and AVC444's text
  sharpness are unknowns.
- **krdp's KPipeWire path is already tuned.** Matching its latency and CPU at 5120x1440 is the
  real test.
- **H.264 needs a C encoder and its licensing.** openh264's patent cover comes only with
  Cisco's binary. VA-API depends on each host's GPU driver.
- **Effort:** the spike is 1-2 weeks. Getting to parity (input, cursor, clipboard,
  multi-monitor, window streams, the guard, reconnects) is roughly 4-8 more. That's an estimate,
  not a measurement.

## The spike, if I decide to do one (after the final test)

One monitor on .63: KWin screencast → PipeWire → H.264 (VA-API, openh264 as fallback) → IronRDP
EGFX → cc-panels connects as it does to krdp today. It passes only if all of these hold:

1. cc-panels connects and shows the monitor, with no change on the Frame side.
2. Latency from a keypress on the host to the panel in VR is at most krdp's.
3. Host CPU at the same picture quality is at most krdp's, at 5120x1440 / 60 Hz.
4. No permission prompt, and nothing installed but the binary (the C libraries it loads are the
   ones krdp already uses).

If any one of those fails, krdp stays, and so do the patches in krdp/.

## Sources

- IronRDP (Devolutions), the server crates and their codecs: https://github.com/Devolutions/IronRDP
- ironrdp-server API notes (EGFX "experimental", community-maintained): https://mintlify.wiki/Devolutions/IronRDP/api/ironrdp-server
- lamco-rdp-server (IronRDP, Wayland portals, H.264 AVC420/AVC444): https://download1.rpmfusion.org/nonfree/fedora/updates/44/SRPMS/repoview/lamco-rdp-server.html
- cosmic-ext-rdp-server: https://github.com/olafkfreund/cosmic-ext-rdp-server
- pipewire-native (a pure-Rust PipeWire protocol): https://docs.rs/pipewire-native
