# Packaging the Frame side as a systemd-sysext image

I want the Frame side of Command Center to be one file the system manages: it installs in one step,
survives SteamOS updates, updates cleanly and uninstalls by deleting that file. On the Frame I'm
going with systemd-sysext. The hosts get distro packages and Flatpak where it fits, which is a
separate doc.

Short version: **sysext works on the Frame today.** The only real blocker is that FreeRDP and its
libraries are built in the Fedora container, so they don't load on SteamOS. cc-panels itself, cc-home
and the pointer driver already do. Fixing that also gets rid of the distrobox runtime dependency.

Everything below was checked read-only on the Frame (SteamOS 0.3.0, build 20260922.6101926,
`VARIANT_ID=vr`, aarch64) on October 3, 2026. Nothing was enabled, merged or refreshed.

## 1. Does SteamOS on the Frame support sysext?

Yes. Here's what's there:

- **systemd 257** (`257.7-2.2-arch`). `systemd-sysext.service` is installed but disabled and
  inactive. `systemd-sysext status` shows `/usr` and `/opt` with no extensions merged.
- **Nothing uses it yet.** `/var/lib/extensions`, `/etc/extensions`, `/run/extensions` and
  `/usr/lib/extension-release.d` don't exist. Valve doesn't use sysext for anything.
- **The kernel can mount it:** squashfs (zstd, xz, lz4, zlib), erofs (lzma and deflate, no zstd) and
  overlayfs are all built in.
- **The host has the tools:** `mksquashfs` 4.6.1, `unsquashfs` and `systemd-dissect` are installed.
  `mkfs.erofs` isn't, on the host or in the container, so the image is squashfs.
- **`/etc/os-release`:** `ID=steamos`, `VERSION_ID=0.3.0`, no `SYSEXT_LEVEL`. That decides how
  the image matches the host (see the release file in section 3).
- **sysupdate is there too:** `/usr/lib/systemd/systemd-sysupdate` and its timer exist (not on PATH,
  and disabled). There's no `systemd-sysupdated` and no `updatectl`, so it's the command-line tool only.

### How Valve updates the OS, and what that does to /var/lib/extensions

Valve doesn't use sysext. SteamOS is A/B: `rootfs-A`/`rootfs-B` (10 GB each, btrfs, `ro=true`),
`var-A`/`var-B` (256 MB each) and one shared `home`. Updates go through RAUC and
`steamos-atomupd-client` into the other slot. `steamos-readonly` just toggles the btrfs `ro` property.

`/var` belongs to the slot, but `holo-sync-var` copies it across on every update. It rsyncs the
booted `/var` into the new slot's, excluding only `boot/`, `lib/dkms/`, `lib/modules/`, `lib/pacman/`,
`lib/NetworkManager/`, `lost+found/` and the `/etc` overlay. **So `/var/lib/extensions` survives
OS updates.**

`/etc` is an overlay whose upper layer lives in `/var/lib/overlays/etc`. An update throws away every
`/etc` change except what's listed in `/usr/lib/rauc/atomic-update-keep.conf` (plus drop-ins in
`/etc/atomic-update.conf.d/`). The keep list includes `/etc/systemd/system/*.wants/**`, so **enabling
`systemd-sysext.service` survives updates too**. Anything else we'd put in `/etc` (a sysupdate
config, a keyring) needs its own line in `/etc/atomic-update.conf.d/command-center.conf`.

Two limits fall out of this:

- **`/var` is small:** 224 MB, with 196 MB free. Today's image is 7.1 MB, and a host-ABI build should
  stay well under 20 MB, so two versions side by side fit fine.
- **The image can't live on `/home`.** `/home` is mounted `nofail`, so it isn't ordered before
  `local-fs.target`, and `systemd-sysext.service` runs `After=local-fs.target`. An image (or the target
  of a symlink) on `/home` might not be there at boot.

## 2. The ABI: building for SteamOS instead of Fedora

This turned out better than I expected. I checked the newest glibc symbol each binary needs:

| File | Newest glibc | Libraries SteamOS is missing |
| --- | --- | --- |
| `cc-panels` | 2.39 | none |
| `cc-home` | static musl | none |
| `driver_cc_pointer.so` | 2.38 | none |
| `libfreerdp3.so.3.31.1` | 2.32 | FFmpeg 8 (`libavcodec.so.62` and 6 more) |
| `libfreerdp-client3.so.3.31.1` | ok | none (besides ours) |
| `libwinpr3.so.3.31.1` | **2.42** (`cfsetspeed`, `cfgetispeed`) | ICU 77 (4 libs), `liburiparser.so.1` |

SteamOS has glibc 2.39. So the Rust code already runs on SteamOS, and the whole problem is the
FreeRDP prefix and what it drags in: Fedora's FFmpeg 8, ICU 77 and uriparser. `build.sh` prints that
exact list (section 6).

### What SteamOS provides, and what we bundle

I looked up every library in `cc-panels`' dependency tree in the host's `/usr/lib`:

**From SteamOS, link against these:**

- glibc 2.39 (`libc`, `libm`), `libgcc_s.so.1`, `libstdc++.so.6`
- `libgbm.so.1`, `libdrm.so.2` (Valve's Mesa, `deckard-mesa` 26.3: these have to match the GPU driver, so
  they're never bundled)
- `libpipewire-0.3.so.0` (PipeWire 1.6.8: the client library has to match the daemon)
- `libpulse.so.0` and `libpulse-mainloop-glib.so.0` (PulseAudio's client library, which PipeWire's
  pulse server answers: FreeRDP's sound and microphone backends, docs/audio.md)
- `libssl.so.3`, `libcrypto.so.3` (OpenSSL 3.2.1: stable ABI across 3.x, and it gets security fixes
  from Valve)
- `libsystemd.so.0`, `libjson-c.so.5`, `libz.so.1`, `libexpat.so.1`
- `libopenvr_api.so` from `/opt/steamvr/bin/linuxarm64` (already in cc-panels' RUNPATH)
- **FFmpeg 7.0** (`libavcodec.so.61` and friends), with the software `h264` decoder and
  `h264_v4l2m2m`. Valve builds it `--enable-gpl --enable-libx264 --enable-libv4l2`.

**Bundle in the image:**

- FreeRDP 3.31.1 (`libfreerdp3`, `libfreerdp-client3`, `libwinpr3`), still with the h264.c threading
  patch. SteamOS ships FreeRDP **3.17.2** with the same soname (`.so.3`), so ours has to come first
  through our RUNPATH. It does, since RUNPATH beats the default paths, and cc-launch already strips
  Steam's `LD_LIBRARY_PATH`.

**Drop at build time:**

- ICU: build WinPR with `-DWITH_UNICODE_BUILTIN=ON`. ICU's soname changes every release (SteamOS
  has 74, Fedora 77), so it's the last thing I want to depend on.
- uriparser: `-DWITH_URIPARSER=OFF`. It's for AAD and URL handling, which we already turn off.
- FFmpeg's `avformat`, `avdevice` and `avfilter` only get pulled in by `WITH_VIDEO_FFMPEG`. SteamOS has
  them anyway, but I'll check whether we can turn it off and keep just `avcodec`, `avutil`, `swscale`
  and `swresample`.

### H.264

There are three ways to do it:

1. **Link SteamOS's FFmpeg 7.** We distribute no codec at all, since Valve already ships the H.264
   decoder in the OS, and their own `freerdp` package links the same libraries, so they have a reason to
   keep them. It also keeps the hardware path open: `h264_v4l2m2m` and `/dev/video-dec0` are right
   there (docs/efficiency-plan.md 2b). The risk is a SteamOS update that bumps FFmpeg's soname. The
   release file's `VERSION_ID` match covers a minor bump (section 3); inside 0.3.x, cc-launch should
   check that the image's libraries resolve and say "SteamOS changed FFmpeg, an update is coming"
   instead of failing silently.
2. **OpenH264 loaded at run time.** Build FreeRDP with `-DWITH_OPENH264=ON -DWITH_OPENH264_LOADING=ON`,
   no FFmpeg. cc-install downloads Cisco's `libopenh264` arm64 binary from Cisco on the user's say-so,
   which is what keeps it inside Cisco's patent license, and that's how Fedora's FreeRDP gets H.264.
   No soname risk, but it's software decode only, and I'd need to test that its decoder handles what
   krdp sends (it's fine with Constrained Baseline, Main and High take a test).
3. **Bundle a minimal FFmpeg** (LGPL, only the h264 decoder and swscale, around 3 MB). That makes us
   distributors of an H.264 decoder, the patent exposure that keeps Fedora from shipping it. I'd rather
   not.

**My pick is 1**, with 2 as the fallback if FFmpeg soname churn ever actually bites.

### The build environment

SteamOS on the Frame ships its own toolchain and headers: gcc 15.1, clang 19, cmake 3.29, ninja,
pkg-config, git, and `/usr/include` has `libavcodec`, `pipewire-0.3`, `gbm.h` and more. So the
cleanest match for the host ABI is the Frame's own root filesystem:

- **The SteamOS build container** is a podman image imported from the Frame's read-only rootfs
  (`/usr` plus a minimal `/etc`). Everything links against exactly what that SteamOS build ships,
  nothing newer. There's one per SteamOS version: `packaging/sysext/steamos-buildenv.sh` makes
  `localhost/cc-steamos:0.3.0`. It leaves out docs, translations, icons, fonts and firmware (1.7 GB
  nobody links against), and the ~40 root-only files in `/usr`. On the Frame it took **116 s** and the
  image is **5.8 GB** (podman's store, on `/home`). Rust isn't in the image: `steamos-buildenv.sh run`
  mounts the cc-rust rustup from `~/.local/share/cc-rust`, plus my home and `/opt/steamvr`
  read-only. SteamOS has no `llvm-ar`, so the musl build of cc-home uses binutils' `ar`.
- **I'm not using Arch Linux ARM.** Its glibc and FFmpeg are newer than SteamOS's, so it hits the same
  symbol-version problem as Fedora.
- **I'm not using Valve's SDK image.** There's no public SteamOS image for the Frame's `vr` variant
  that I know of. The holo repos in `/etc/pacman.conf` are marked not to share, so I won't build
  anything that depends on them.
- **For CI:** the image can't be published (it's Valve's OS), so release builds run on the Frame as a
  self-hosted aarch64 runner, or anywhere that has a private copy of the image.

Rust doesn't care which glibc it's built against, so the workspace builds as-is. The cc_pointer build
already checks `GLIBC_2.39` the same way.

**What this changes:** nothing at run time needs distrobox. cc-panels runs straight on the host. A
build made this way leaves `panels/third_party/prefix/steamos-release`, and when that's there,
`cc-box` runs its command directly (still niced) instead of in the container. That goes for the image
and for a checkout built natively. A checkout built in the container works the same as before.

## 3. The image layout

The prototype (section 6) builds exactly this. `/usr/lib/command-center` mirrors the checkout on
purpose, so `root()` and cc-panels' existing RUNPATH (`$ORIGIN/../../panels/third_party/prefix/lib64`)
keep working without changes.

```
/usr/bin/cc-home       -> ../lib/command-center/cc-home
/usr/bin/cc-panels     -> ../lib/command-center/cc-panels
/usr/lib/command-center/
  cc-home                                   static musl, the multi-call binary
  cc-panels, cc-box -> cc-home
  session/cc-launch, cc-desktop, cc-rest -> ../cc-home
  target/release/cc-panels                  RUNPATH /opt/steamvr/bin/linuxarm64:$ORIGIN/../../panels/third_party/prefix/lib64
  panels/cc-windows.js, cc-restore.js       the KWin scripts
  panels/third_party/prefix/lib64/          libfreerdp3, libfreerdp-client3, libwinpr3 (.so.3, 3.31.1)
  crates/cc-panels/actions/*.json           the SteamVR action manifest and bindings
/opt/steamvr/drivers/cc_pointer/            driver.vrdrivermanifest, bin/linuxarm64/driver_cc_pointer.so, resources/
/usr/share/applications/org.controlcenter.panels.desktop   KWin's screencast and fake-input grant
/usr/share/applications/deckard-nested-desktop.desktop     the VR launcher's "Desktop" → cc-launch
/usr/lib/extension-release.d/extension-release.command-center
```

The `target/release` path inside `/usr/lib` is ugly, I know. It keeps the first version a packaging
change and not a refactor, and it's easy to tidy into `lib/` and `libexec/` later.

### The SteamVR driver: no vrpathreg

SteamVR on the Frame runs from `/opt/steamvr`, which is on the OS image, and sysext merges `/opt` as
well as `/usr`. vrserver loads every folder in `/opt/steamvr/drivers` on its own (`cv`, `frame_hmd`,
`frame_controller` and the rest load from there; its log shows
`Loaded server driver cv ... from /opt/steamvr/drivers/cv/bin/linuxarm64/driver_cv.so`). So the
pointer driver goes into `/opt/steamvr/drivers/cc_pointer` and gets picked up system-wide. There's
nothing per user to register, and no SteamVR binding loads get spent on vrpathreg.

When moving an existing install over, the old registration has to go
(`driver/cc_pointer/install.sh uninstall`), or SteamVR loads both copies.

The hash-named folders were there so a loaded driver never gets replaced underneath vrserver. With
sysext, a running vrserver keeps the `.so` it mapped from the old image, and the new one loads on the
next SteamVR start, same as today. `install.sh check`'s "needs a SteamVR restart" logic carries over:
compare the mapped path's image against the merged one.

### The launcher entry

Right now `cc-home install desktop` writes a per-user override to
`~/.local/share/applications/deckard-nested-desktop.desktop`. If the image goes away, that file stays
behind and points at a missing binary, and the Desktop tile breaks. If the image ships the same file
name in `/usr/share/applications` instead, the overlay shadows SteamOS's own entry while the image is
merged, and removing the image puts the stock one back on its own. That's what the prototype does.

The catch is that while it's merged we hide any change Valve makes to that file. To opt out per user,
copy the stock entry into `~/.local/share/applications` (the user's copy wins).

### The release file and matching

`extension-release.command-center` holds:

```
ID=steamos
VERSION_ID=0.3.0
ARCHITECTURE=arm64
IMAGE_ID=command-center
IMAGE_VERSION=<git describe>
```

SteamOS has no `SYSEXT_LEVEL`, so systemd matches `ID`, then `VERSION_ID` exactly. Each build has its
own `BUILD_ID`, and `VERSION_ID` looks like the release line (the hotfix repo is `release/0.3.x`), so I
expect the image to keep working through 0.3 updates and stop merging at 0.4. I haven't watched it
across an update yet, though. That's the right failure: no Desktop tile instead of a
crashing cc-panels, until there's an image built for 0.4. `ID=_any` (supported, the strings are in
`libsystemd-shared-257`) would turn that guard off, and I'd only use it with option 1's library check
in cc-launch.

The file name has to match the image's name: `command-center.raw` (or `command-center.raw.v/`, see
section 4) needs `extension-release.command-center`.

### Per-user state

None of this goes in the image, and nothing changes here: `~/.config/control-center` (settings, the
Desktop's own `XDG_CONFIG_HOME`), `~/.cache/control-center` (`cc-panels.log`, `desktop-closed`,
`install.log`), `~/.local/share/control-center` (cc-host on hosts) and `/tmp/cc-desktop.log`. The
image is read-only, so every write already lands in the user's home.

The desktop runs as transient user units (`systemd-run --user --unit cc-desktop`), so the image doesn't
need unit files in `/usr/lib/systemd/user`. I'll add static ones only if something needs to start them
by name.

## 4. Install, update and uninstall

### Who can be root

`sudo -n true` says a password is required, and `passwd -S steamos` shows `NP` (no password set). It's
the Steam Deck situation: until the user sets a password with `passwd`, neither sudo nor pkexec can
authenticate. Valve's polkit actions (`org.valve.steamos.policy`) only cover their own helpers like
`steamos-update`, and there's nothing for sysext.

### First install (once, needs the password)

```
sudo install -Dm644 command-center.raw /var/lib/extensions/command-center.raw
sudo systemctl enable systemd-sysext.service   # survives OS updates via the keep list
sudo systemd-sysext refresh
```

cc-install does this through `pkexec` and asks first. It also cleans up a checkout install:
unregisters the vrpathreg driver, removes the `~/.local/bin` links and the per-user `.desktop`
overrides, and leaves `~/.config/control-center` alone.

**Signing:** the image runs as root, so a SHA256SUMS file from the same release isn't enough, since
anyone who can edit the release can edit both. Releases get a detached signature (`SHA256SUMS.gpg`),
and cc-install checks it against a key compiled into cc-install.

### Updates without a password after that

The image can ship its own polkit action, `/usr/share/polkit-1/actions/org.controlcenter.update.policy`,
with `allow_active=yes` for one root helper, `/usr/lib/command-center/cc-sysext-update`. That's the same
pattern as Valve's `steamos-update`. The helper downloads only from our release URL, checks the
signature, writes the new image to a temp file, then renames it into place. After the first install,
updates don't need the password.

Applying an update works like this:

- `systemd-sysext refresh` right away if the Desktop isn't running (`cc-desktop` inactive). Per
  "don't restart a closed Desktop", it never stops a running Desktop to do it.
- Otherwise it waits for the next boot. `systemd-sysext.service` merges whatever's in
  `/var/lib/extensions` at boot.
- The driver updates on the next SteamVR start either way.

### systemd-sysupdate, natively

It can do the whole download step, and I'd like it as phase 2. The transfer file ships inside the
image as its own component, so removing the image removes it too:

```
# /usr/lib/sysupdate.command-center.d/50-command-center.transfer
[Transfer]
Verify=yes

[Source]
Type=url-file
Path=https://github.com/nomadsgalaxy/Command-Center/releases/latest/download/
MatchPattern=command-center_@v.raw

[Target]
Type=regular-file
Path=/var/lib/extensions/command-center.raw.v/
MatchPattern=command-center_@v.raw
InstancesMax=2
```

Run it with `/usr/lib/systemd/systemd-sysupdate --component=command-center update`, from the helper or
a timer. Here's what I found:

- **GitHub's `releases/latest/download/`** works as the source, since sysupdate only needs a
  `SHA256SUMS` next to the files, and that already exists. Each release just has to list its own `.raw`.
- **Two versions can't sit loose in `/var/lib/extensions`,** because sysext would merge both. The
  `.raw.v/` directory solves that: since systemd 256, sysext picks the newest version inside it
  ("vpick"). **I haven't tried that on the Frame yet.** It's the first thing to test, and plain
  `command-center.raw` replaced by the helper works without it.
- **`Verify=yes` checks `SHA256SUMS.gpg`** against `/etc/systemd/import-pubring.gpg`, falling back to
  `/usr/lib/systemd/import-pubring.gpg`. That's one keyring shared with `systemd-pull` and importd.
  Ours would have to go in `/etc` (plus a keep-list line) or shadow the `/usr/lib` one from inside the
  image. Neither is lovely. Nothing on SteamOS uses that keyring today, but I'd still rather verify in
  our helper, which is why the helper is phase 1.

### Uninstall

```
sudo rm /var/lib/extensions/command-center.raw   # or the .raw.v directory
sudo systemd-sysext refresh
```

That's everything system-wide gone: the commands, the driver, both `.desktop` files and the polkit
action. `systemd-sysext.service` can stay enabled, since with nothing in the extension dirs its
conditions skip it. cc-install's Remove does this, then asks before deleting `~/.config/control-center`
and `~/.cache/control-center`. The Desktop tile is SteamOS's again on its own.

### OS updates

- **The image survives:** `/var/lib/extensions` is copied to the new slot.
- **The service stays enabled:** `*.wants/**` is on the keep list.
- **On a new SteamOS minor (0.4),** the release file stops matching, so it's left unmerged. cc-install,
  or the sysupdate timer, fetches the image built for that minor.
- **Developer mode:** `steamos-readonly disable` and `pacman` won't work while the image is merged,
  because the overlay keeps `/usr` read-only. Run `systemd-sysext unmerge` first, then `merge`
  afterwards.

## 5. Flatpak for the Frame side: no

I checked each piece against the sandbox:

- **SteamVR overlays:** `libopenvr_api` loads `vrclient.so` from `/opt/steamvr` and talks to vrserver
  through `/dev/shm` and sockets. That could be mapped in with `--filesystem=/opt/steamvr:ro`
  and shared IPC. Possible, but fragile.
- **The pointer driver:** vrserver loads it, outside any sandbox, so it has to be on the host anyway.
  It would still need a host path or vrpathreg.
- **The desktop session:** cc-home starts a headless KWin and Plasma from the host and manages them
  as user units. From a Flatpak that's `flatpak-spawn --host` for everything, which is a full sandbox
  escape and defeats the point.
- **KWin's grants:** the screencast and fake-input grant goes by `.desktop` id. That part could work
  for a Flatpak app id.
- **Input:** `/dev/input` needs `--device=all`.

The session and the driver are the deal-breakers. Flatpak stays the plan for host-side UI where it
fits, not the Frame.

## 6. Building it

Three scripts, and none of them install anything:

```
packaging/sysext/steamos-buildenv.sh   # once per SteamOS version: the build container
packaging/sysext/build-native.sh       # cc-home, FreeRDP, libvncclient, cc-panels, the driver
packaging/sysext/build.sh              # → packaging/sysext/out/command-center.raw
```

`build-native.sh` builds in place, into the checkout's usual paths, so don't run it in the checkout
the Desktop is running from: it replaces that checkout's FreeRDP. I use a worktree. It reads FreeRDP's
feature set from `FREERDP_FLAGS` in install.rs, so the two builds can't drift apart, and adds
`-DWITH_UNICODE_BUILTIN=ON -DWITH_URIPARSER=OFF -DWITH_VAAPI_H264_ENCODING=OFF`. The last one is
because SteamOS has libva and Fedora's build never found it, and it's a server feature anyway.
libvncclient comes from `tools/build-libvncclient.sh`, unchanged, since it already works with
whatever system it's in.

`build.sh` re-runs itself on the host through `distrobox-host-exec`, because it needs SteamOS's
`mksquashfs` and checks against SteamOS's libraries.

What I got (October 3, 2026, with other builds running on the Frame at the same time):

- **The native build:** 7 min 16 s from clean, 88 s when only cc-panels changed.
- **Every library comes from SteamOS** except our three FreeRDP 3.31.1 libraries: FFmpeg 7
  (`libavcodec.so.61`), OpenSSL 3.2, PipeWire 1.6.8, `libjpeg.so.8` and `libjansson.so.4` (FreeRDP
  picked jansson here, json-c in Fedora). There's no ICU or uriparser anymore. On the host,
  `ldd -r` finds every library and every symbol for `cc-panels`, the FreeRDP libraries and the
  driver.
- **Newest glibc symbol:** `cc-panels` 2.39, `libwinpr3` 2.34 (was 2.42), `libfreerdp3` 2.32,
  `libfreerdp-client3` 2.28, the driver 2.34. SteamOS has 2.39.
- **The image:** 7.5 MB, squashfs with zstd. `build.sh`'s check passes ("every program and
  library in it can load on this SteamOS"), and `systemd-dissect --validate` says `OK`. Without
  root it can't mount the image to look inside, so I checked the contents with `unsquashfs -ll` and
  the release file with `unsquashfs -cat`.
- **From the unpacked image,** `CC_DRY=1 cc-box echo hi` prints `nice -n 10 echo hi`: no distrobox.

H.264 still goes through FFmpeg's `WITH_VIDEO_FFMPEG`, which links `avformat`, `avdevice` and `avfilter`
too, and with SteamOS's FFmpeg cc-panels ends up loading about 170 libraries. They're all in
SteamOS, so it only costs load time. I'll measure that before trying to trim it.

I didn't install, merge or refresh the image, so it's untested on a live system.

### krdp on a SteamOS host

This is for a Steam Deck, or a Frame sharing its own screen. `krdp/build-steamos.sh` builds the krdp
from `packaging/arch/PKGBUILD` (6.7.5, the same checksum and patches) in the same container, into a
`/usr` tree for a host sysext. It installs nothing. From clean it takes about a minute.

SteamOS 0.3 has Plasma 6.2.5, Qt 6.8.0 and Frameworks 6.14, and krdp 6.7.5 asks for Qt 6.10 and
Frameworks 6.26. It builds once I lower those two minimums, with three more changes:

- **extra-cmake-modules 6.14** isn't on SteamOS. It's only needed to build, so it stays in the work dir.
- **qtkeychain 0.15** isn't on SteamOS either, and krdp links it, so it goes in
  `/usr/lib/command-center` with krdpserver's RPATH pointing there.
- **KPipeWire 6.2** has `setActive()` where 6.7 has `start()` and `stop()`, and it has no
  `setColorRange()`. The script swaps the calls and drops the colour range, so the H.264 stream is
  limited range. Colours might look a little flat. If they do, bundling KPipeWire 6.7 is the fix.

The examples are off (they hit the same KPipeWire calls). krdp links SteamOS's FreeRDP 3.17.2
server libraries, not ours. Everything resolves on the host, and the newest glibc symbol is 2.34. It
still needs a live test: a Frame panel showing a Deck's screen, the pointer patch, the clipboard and
a window stream.

## 7. What's left, and how long

| Work | Effort |
| --- | --- |
| ~~The SteamOS build container~~ (done) | |
| ~~FreeRDP 3.31.1 in it~~ (builds, `build.sh` check passes); testing H.264 against krdp live | ½ day |
| ~~Running from `/usr`~~ (`root()`, `cc-box`, `on_host()`); still to do: cc-launch's library check | ½ day |
| First live test: merge on the Frame, Desktop tile, KWin grant, driver loading from `/opt`, an OS update | 1 day, plus waiting for an OS update |
| cc-install's sysext path: pkexec install, the signed-update helper and polkit action, moving over from a checkout install, Remove | 2–3 days |
| Releases: signing, building the `.raw` on the Frame as a runner, publishing per SteamOS minor | 1–2 days |
| Phase 2: sysupdate transfer with `.raw.v/`, plus a timer | 1 day, after vpick is tested |

All in, it's about one and a half to two weeks of work, and the first live merge on the Frame is the
biggest unknown.

Open questions I want answered on the device:

- Does the VR launcher pick up an overlay-shadowed `deckard-nested-desktop.desktop` without a reboot
  (`kbuildsycoca6`)?
- Does `.raw.v/` vpick work for sysext on 257 here?
- How does `systemd-sysext refresh` behave while vrserver has the driver mapped?
