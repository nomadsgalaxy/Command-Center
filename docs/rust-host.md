# The host side in Rust

This started with D-017 and my message on 2026-10-02: "let's get as much as we can running rust
to make it as compatible as possible". That night the window-server build failed on the laptop.
The desktop's toolchain marks binaries x86-64-v4, the laptop is v3, and the laptop didn't have the
build tools anyway. On top of that, every host had to carry Python's cryptography, PySide6 and gi
just for pair.py, agent.py and tagshow.py. One Rust binary, built once, gets rid of all of that.

> **The Python references are gone** (2026-10-03: the agent, pairing, tag screens, cc-share's bash,
> cc-home and the scan), and they aren't part of this repo. The specs now are cc-host's
> `tests/conformance.rs` and `tests/pair.rs`, the goldens in `tests/fixtures/`, and cc-home's
> cross-tests.

> **The Frame's launch glue is Rust too.** On 2026-10-03 I said: "I want this to be full rust".
> `session/cc-launch`, `session/cc-rest`, `session/cc-desktop`, `cc-panels` and `cc-box` are links to
> the cc-home binary, which acts on the name it's run as (crates/cc-home/src/session.rs), the same
> as `cc-home desktop | rest | session | panels | box`. The behaviour, timeouts, log and its markers
> are the scripts'. `CC_DRY=1` prints what desktop, panels, rest and box would run. `install.sh` is
> only the bootstrap (the container, its packages, cc-home), and `cc-home install [desktop]` does
> the rest. cc-home has to be built for any of them to run, since `cc-home` is a link to its build.

## How it fits together

- **`crates/cc-proto`** (a library) is the wire protocol both ends share: SPAKE2 pairing, the
  agent's TLS + Ed25519 challenge, and the JSON messages. cc-host uses the server half. cc-panels
  uses the client half (it starts and stops sessions and windows directly, with no Python
  spawn), and cc-home does too.
- **`crates/cc-host`** (a binary, one per host architecture) is everything on the host: the agent
  (the pairing door and the TLS door on 3399, sessions, windows, tags), the key screen and tag
  screens drawn on Wayland without Qt, KWin over D-Bus (zbus), and the install, units and checks
  cc-share used to do. It speaks the same wire protocol and keeps the same files under
  ~/.config/control-center, so **existing pairings keep working (R10)**.
- **The build** is a static `x86_64-unknown-linux-musl` binary, cross-built on the Frame (aarch64)
  with a user-local rustup (`~/.local/share/cc-rust`), kept apart from the container's toolchain
  that cc-panels uses. musl's C runtime is Rust's own, so it doesn't need the distro's startup
  objects, which is what broke that night. TLS is rustls with the `ring` provider, so there's no
  cmake, and clang cross-compiles `ring`'s C and assembly. There's no tokio: plain std threads,
  like cc-panels.

## Platforms

On the platforms, I wrote: "this may run on apple hardware, arm hardware, x86 and x64 ... prefer
rust". So it's one portable core with a thin layer per platform:

| Target | Status | How it's built |
|---|---|---|
| Linux x86_64 (`x86_64-unknown-linux-musl`) | **builds and runs on both hosts** (static, no ISA marker) | cross from the Frame: user rustup, clang for ring, rust-lld |
| Linux aarch64 (`aarch64-unknown-linux-musl`) | **builds and runs on the Frame** (static) | same |
| Linux x86 32-bit (`i686-unknown-linux-musl`) | to add when a host needs it | same pattern |
| macOS arm64 / x86_64 (`aarch64-apple-darwin`, `x86_64-apple-darwin`) | later | on a Mac or a macOS CI runner (Apple's SDK can't be shipped for a Linux cross build); one universal binary |
| Windows x86_64 / arm64 | later | the `*-pc-windows-gnu`/`-msvc` targets, on Windows or CI |

- **The portable core** (every platform): cc-proto (pairing, the agent's TLS + challenge,
  messages), the agent's command loop, the session bookkeeping, the limits, and the files under
  the config directory.
- **The platform layer** (one module each):
  - the screen server: krdp today on KDE, maybe IronRDP later (R5, docs/rust-rdp-server.md);
  - window listing and window streams: KWin over D-Bus on KDE, ScreenCaptureKit and the
    Accessibility API on macOS, DXGI and UI Automation on Windows;
  - the key and tag screens: Wayland layer-shell on KDE, native windows elsewhere;
  - announcing: avahi on Linux, Bonjour on macOS and Windows;
  - service installation: systemd user units, launchd agents, or a Windows service or startup
    task.
- The Frame side (cc-panels, Rust) is aarch64 Linux today. cc-proto keeps its client half free of
  platform code, so other headsets' clients can use it (D-043).

## The review's D-057 (adopted)

- **R1, Python was the reference** until Rust passed the same tests: agenttest and the pair
  selftest became a black-box conformance suite against a real listening port, run against both,
  and golden transcripts pin the exact bytes (the signed string, HKDF labels, HMAC
  confirmations, JSON fields).
- **R2, the spake2 cross-test:** done, and retired with the Python, since both ends are Rust now.
  `tests/spake2-cross` checked that Rust `spake2` 0.4.0 (pinned `=0.4.0` in cc-proto) and the
  vendored python-spake2 0.9 derive the same key with Rust as A and as B, using pair.py's
  identities, and that a wrong password differs.
- **R3:** `ring` still compiles C and assembly, so the musl cross build gets verified from a
  clean container before anything relies on it.
- **R4:** releases are baseline x86-64 (no `target-cpu=native`). A v3 build only if it measurably
  helps.
- **R5, IronRDP** (a Rust RDP server on KWin's screencast, to replace the per-host krdp builds
  and the window-stream patch) is a spike with a go/no-go: capture, input, an H.264 encoder (C or
  hardware, which conflicts with a pure static musl binary), clipboard. krdp stays until it
  passes. The details are in docs/rust-rdp-server.md.
- **R6:** a signed static binary per architecture (the release design is in docs/packaging.md).
- **R9:** later that same day I wrote: "I know python is powerful, but let's do what we can to
  port it over to rust". So nothing was meant to stay Python. After the host side, the order was
  cc-home (the Frame's CLI, which cc-panels already partly replaced by calling cc-proto directly),
  then the scan pipeline (home/scan.py, solve.py, pattern.py: ArUco detection, pose solving,
  camera fitting).
  - **The scan is crates/cc-scan, pure Rust.** There's no OpenCV, so it runs static on the Frame,
    and later on a Quest or a Mac, with nothing installed. OpenCV's pieces were ported from its
    4.13 source and checked against it on the recorded passthrough frames: grey, Gaussian, CLAHE,
    threshold and contours are pixel-identical, and on the same frames the detector finds the same
    6366 tags with corners within 0.01 px (p99) (`examples/stage-diff`, `aruco-compare`). The
    layouts match pattern.py value for value. The fit agrees with solve.py on synthetic scans with
    a known answer, curved and flat, with misreads and the mirror lag (centre < 2 mm,
    radius < 2%): cc-scan's tests/cross.rs, against pattern.py's and solve.py's outputs recorded
    in tests/fixtures/. scipy's least_squares became a robust Levenberg-Marquardt on the same
    soft_l1 cost, and solvePnP a homography pose refined on the pixels.
  - The camera is V4L2 mmap (the newest buffer held, a shot copies one). The head pose comes from
    cc-panels' `head` only (no cc-tip; tests use CC_PANELS_SOCKET). The HUD is drawn with fontdue.
    cc-home links it (`cc_scan::scan::Scan`, `cc_scan::solve::solve`, `cc_scan::pattern`). The live
    parts (the camera, the HUD in the headset) get tested at the end, with me.
  - **cc-home is all Rust** (crates/cc-home, stage 3): align/scan, refit, calibrate (cc-panels'
    `tip`; cc-tip and cc-roles are retired), and pair --scan. The wrapper runs only the musl build.
  - **The Frame-side Python is gone** (home/cc-home.py, scan.py, solve.py, pattern.py). What it
    printed and wrote was recorded in tests/fixtures/ first, and cargo test -p cc-home checks
    against that: tests/cross.rs has 165 command cases (the machine ones
    against a real cc-host), the align's placing and saving, and the align's steps on
    a fake camera (tag screens through cc-host serve --fake, job.json byte for byte as Python's
    write_job). install.sh builds cc-home and puts no OpenCV, NumPy, SciPy or Pillow in the
    container.
- **R10:** an upgrade path with no re-pairing: the same keys, ids, ports and files.

## Order

1. **The spake2 cross-test** (done), and **the musl cross build with ring** (R3, done: a
   rustls/ring binary built on the Frame runs on .63 and .85 as x86_64, and on the Frame as
   aarch64, both static).
2. **cc-proto:** the messages and the agent's client and server halves (TLS + challenge), plus
   the conformance suite (R1) against Python's agent and Rust's. **Done for the core (stage A):**
   `tests/agent-conformance python|rust` ran 16 black-box cases (the login's hostile cases, the
   doors, backoff, the cap, newest-wins, revocation, version/monitors) and both passed. It caught
   one incompatibility right away: ed25519-dalek writes PKCS#8 v2 key files, which Python's
   cryptography rejects, so cc-host writes v1. The suite is now
   `crates/cc-host/tests/conformance.rs`, Rust only, and it's the spec.
3. **The cc-host agent:** sessions, windows, tags (the command set agent.py had), then pairing.
   **Done:** conformance 20/20, and `cc-host pair` (the host's half: slots, the per-Frame login,
   lockout, the 30 s pause, SIGTERM, pairing.sock behind the agent) passed `CC_TESTHOST=<cc-host>
   cc-home selftest-pair` against the Python Frame and `tests/pair-host` (hostile names, junk,
   lockout). That's now `crates/cc-host/tests/pair.rs`, the Rust Frame against it.
4. **The key screen and tag screens on Wayland.** **Done:** they're drawn in `draw.rs` (fontdue,
   the bundled font; OpenCV reads the tags back) and shown by `screen.rs`, a pure-Rust Wayland
   client (no libwayland, no xkb) that puts a layer-shell overlay with the keyboard on the named
   output. `cc-host tagscreen <output> [--ask]` replaces tagshow.py, and the agent asks the host
   after 3 cancels. It was tested on a private headless KWin. The fractional-scale path isn't
   tested there, because kscreen-doctor can't set a scale in that session.
5. **Install, units and checks** (cc-share's job), then retiring pair.py, agent.py and tagshow.py
   on the hosts. **Done:** cc-share's commands are cc-host subcommands (`src/share.rs`: install,
   uninstall, check, fix, up, down, autostart, announce, pair's firewall check, unpair, frames, lock,
   windows, guard, frame-run, and so on), using kscreen-doctor's JSON through serde and sockets
   from /proc, with no bash, jq, ss or pkill. `cc-share` is now a link to cc-host (it acts on
   argv[0]), the units start cc-host directly, `check` is the checklist and `check --agent` is the
   probe. `crates/cc-host/tests/share.rs` (it was tests/share-cross) runs 14 scenarios against the
   bash it replaced: its runs were recorded in tests/fixtures/share/ before the bash was removed,
   on throwaway HOMEs with logging shims, and the output, tool calls and files all match.
   Installing is cc-host only: the `--python` install and the Python agent, pairing and tag
   screens are gone.
6. **cc-panels calls cc-proto directly.**
7. **The IronRDP spike** (R5).
