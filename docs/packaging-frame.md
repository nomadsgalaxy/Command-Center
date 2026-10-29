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
  session/cc-launch, cc-desktop, cc-rest, cc-tab -> ../cc-home
  command-center.vrmanifest                 the dashboard tab's SteamVR app manifest → session/cc-tab
  packaging/command-center.png              the tab's icon
  target/release/cc-panels                  RUNPATH /opt/steamvr/bin/linuxarm64:$ORIGIN/../../panels/third_party/prefix/lib64
  panels/cc-windows.js, cc-restore.js       the KWin scripts
  panels/third_party/prefix/lib64/          libfreerdp3, libfreerdp-client3, libwinpr3 (.so.3, 3.31.1)
  crates/cc-panels/actions/*.json           the SteamVR action manifest and bindings
/opt/steamvr/drivers/cc_pointer/            driver.vrdrivermanifest, bin/linuxarm64/driver_cc_pointer.so, resources/
/usr/share/applications/org.controlcenter.panels.desktop   KWin's screencast and fake-input grant
/usr/share/applications/command-center.desktop             our own tile in the VR launcher → cc-launch (beside SteamOS's Desktop)
/usr/share/applications/steam/command-center/              its Steam library art: capsule, hero, logo, store-capsule, icon
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

**When SteamVR doesn't run from `/opt/steamvr`.** The Frame can run it from elsewhere (`steamvr
setpath`, or a Steam-managed copy that mounts under `~/.local/share/Steam/steamapps/common`), and
vrserver then never looks in `/opt/steamvr/drivers`. So `cc-home desktop` (session/cc-launch) checks
at each start. `steamvr path --ignore-pending` prints `runtime[0]` of
`~/.config/openvr/openvrpaths.vrpath`, resolved, so it reads that file instead of running anything,
and it opens no OpenVR connection. If that path isn't `/opt/steamvr`, the image's driver is there and
it isn't in `external_drivers` yet, it runs `vrpathreg adddriver /opt/steamvr/drivers/cc_pointer`,
once, with `XDG_CONFIG_HOME=$HOME/.config` (inside the Desktop it points elsewhere, and vrpathreg
would write a registry SteamVR never reads). vrpathreg only edits that file, so it spends none of
SteamVR's binding loads. What it did goes to stderr and `~/.cache/control-center/launch.log`, with a
note to restart SteamVR once. `cc-home install remove` takes the registration back out. The logic
is in `crates/cc-home/src/steamos.rs`, as pure functions with tests.

The hash-named folders were there so a loaded driver never gets replaced underneath vrserver. With
sysext, a running vrserver keeps the `.so` it mapped from the old image, and the new one loads on the
next SteamVR start, same as today. `install.sh check`'s "needs a SteamVR restart" logic carries over:
compare the mapped path's image against the merged one.

### The dashboard tab's manifest

Command Center's tab in SteamVR's dashboard is there even while the Desktop is closed. SteamVR
starts a small process for it with itself: `/usr/lib/command-center/command-center.vrmanifest`
says app `controlcenter.tab` is a dashboard overlay (`is_dashboard_overlay`) that runs
`session/cc-tab`, which runs `cc-panels --tab` (crates/cc-panels/src/tab.rs). The path is fixed, so
it means the same thing in every image version.

A manifest only counts once SteamVR has it in its appconfig (`~/.config/openvr/config/appconfig.json`),
and that's per user, so the image alone can't do it. The Desktop's start checks
`IsApplicationInstalled` inside the OpenVR connection it opens anyway, and if SteamVR doesn't have
the manifest (a first start, or after a `steamreset`) it adds it with `AddApplicationManifest` and
turns on its auto-launch with `SetApplicationAutoLaunch`. Auto-launch is only set then, so turning
it off in SteamVR's settings sticks. Nothing opens a connection of its own for this at launch,
because every connection spends SteamVR's binding loads. A checkout install (`cc-home install`)
writes the same manifest with its own paths and registers it right away if SteamVR is running, with
one connection.

`packaging/command-center.vrmanifest.in` is the one template, like the tile's.

### The launcher entry

Command Center has its own tile, `command-center.desktop`, and SteamOS's Desktop is left alone. An
earlier version shadowed Valve's `deckard-nested-desktop.desktop` (the image) or overrode it per user
(`cc-home install desktop`), which meant installing Command Center took the stock Desktop away and hid
any change Valve made to it. Both are gone: `install desktop` removes the old override if it's ours,
and the image ships only its own file.

The entry carries the same keys SteamOS's Desktop uses for the Frame's non-Steam launcher:
`X-Steam-Library-Capsule` (600x900), `X-Steam-Library-Hero` (3840x1240), `X-Steam-Library-Logo`
(1280x720), `X-Steam-Library-StoreCapsule` (1280x720) and `X-Steam-Controller-Template=Desktop`. It
leaves out `X-Steam-Special=Desktop`, which marks SteamOS's own entry ("for Steam Frame's non-Steam
app launcher to pick this out"), so two entries don't claim it.

`packaging/command-center.desktop.in` is the one template. The image fills it with system paths and
puts the art in `/usr/share/applications/steam/command-center/`, next to where Valve keeps its own;
`cc-home install desktop` fills it with the checkout's paths and leaves the art in
`packaging/art/`. Removing the image (or `cc-home install remove`) removes the tile, and nothing of
Valve's changed. `packaging/art/build.sh` draws the images from `packaging/command-center.svg` with
`rsvg-convert` (and `bc`) in the logo's own colours, with no Steam marks (Valve's branding rules), and
the PNGs are committed so installing needs no renderer.

### The release file and matching

`extension-release.command-center` holds:

```
ID=_any
ARCHITECTURE=arm64
IMAGE_ID=command-center
IMAGE_VERSION=<git describe>
```

systemd-sysext loads an image only if its release file matches the running OS: `ID` (or `_any`,
which matches every OS), then `SYSEXT_LEVEL` if both sides have one, else `VERSION_ID`, but neither
is compared under `_any`; and `ARCHITECTURE`, if it's set. `IMAGE_ID` and `IMAGE_VERSION` aren't
matched, they only say which image it is. So ours is matched on `ID=_any` and `ARCHITECTURE=arm64`
only, and cc-launch refuses to start from an image without both.

SteamOS has no `SYSEXT_LEVEL`, so with `ID=steamos` systemd matches `VERSION_ID` exactly, and that
moves every few days (0.3.0 on Oct 3, 0.4.5 on Oct 7). I stamped it that way first, expecting it to
last through a release line, and an image stamped 0.3.0 was silently ignored on 0.4.5. So the image is
`ID=_any`, which skips the version match, and `ARCHITECTURE=arm64` still keeps it off anything else.

What actually breaks a native build is a library it was built against changing, so that's checked
where it matters. `cc-home desktop` (session/cc-launch) compares the SteamOS version the build was
made on (`panels/third_party/prefix/steamos-release`, which build-native.sh writes and build.sh
ships in the image) with the running `VERSION_ID`. The same version needs nothing more. A different
one runs `ldd` over cc-panels, and if everything still loads it starts and says so ("SteamOS is
0.4.6 now, and this was built on 0.4.5. Everything it needs still loads, so it starts."). If
something doesn't load (an FFmpeg or glibc bump), it stops and names what's missing and what to do
(run the installer again). The note goes to stderr and `~/.cache/control-center/launch.log`, since
the VR launcher doesn't show cc-launch's output. The code is `crates/cc-home/src/steamos.rs`.

The file name has to match the image's name: `command-center.raw` (or `command-center.raw.v/`, see
section 4) needs `extension-release.command-center`. build.sh always names it that, whatever the
output file is called, so `command-center_0.9.1.raw` carries `extension-release.command-center` too.
The vpick test on 2026-10-29 used the same rule (`extension-release.cc-n9-test` inside each
`cc-n9-test_<N>.raw`).

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
cd / && sudo systemd-sysext refresh            # from a directory root can read (see below)
sudo systemctl try-restart polkit.service      # so polkit sees org.controlcenter.update
```

This first one needs the password, once: the image brings the polkit action that makes later
updates passwordless, and polkitd doesn't know it until it restarts. Without the restart, `cc-home
update` asks for a password until the next reboot.

cc-install does this through `pkexec` and asks first. It also cleans up a checkout install:
unregisters the vrpathreg driver, removes the `~/.local/bin` links and the per-user `.desktop`
overrides, and leaves `~/.config/control-center` alone.

**Signing:** the image runs as root, so a SHA256SUMS file from the same release isn't enough, since
anyone who can edit the release can edit both. Releases get a detached signature, `SHA256SUMS.sig`
(release.yml makes it with `gpg --detach-sign`), and the update helper checks it with `gpgv`
against `/usr/lib/command-center/release-key.gpg`, the public half of `packaging/command-center.asc`
that build.sh puts in the image. That one name and that one key are used everywhere. The `install`
script and cc-install don't check the signature yet: they only check what they download against
`SHA256SUMS`, so for the first install the signature isn't checked by anything.

### Updates without a password after that

`cc-home update` updates the image with no password, from the latest release. It asks polkit to
run `/usr/lib/command-center/cc-sysext-update` (action `org.controlcenter.update`, in the image at
`/usr/share/polkit-1/actions/org.controlcenter.update.policy`), which is allowed for the user at
the device only: `allow_active=yes`, `allow_inactive` and `allow_any` no. That's the same pattern
as Valve's `steamos-update`. The helper is a copy of cc-home under its own name (static, no
libraries; a copy rather than a link, so polkit's `exec.path` and its own name both match, and as
root under pkexec cc-home refuses to be anything else), and as root it does one thing, in this order (`crates/cc-home/src/update.rs`):

1. Its work directory, `/var/lib/command-center/update`: root's, 0700, a real directory and not a
   symlink, emptied first; anything in it that isn't a plain file stops the update.
2. `SHA256SUMS` and `SHA256SUMS.sig` from the release URL, which is fixed in the helper. curl only
   speaks HTTPS, also on redirects, with a time limit and a size cap on each file.
3. `gpgv --keyring /usr/lib/command-center/release-key.gpg` checks the signature. That keyring
   is our release key, the public half of `packaging/command-center.asc`, put in the image by
   build.sh; no system keyring is used or touched. Nothing in SHA256SUMS counts before this passes.
4. The one `command-center_<version>.raw` the signed list names (letters, digits and `._+~-`
   only, so never a path). If that version's already a file in `command-center.raw.v/` and it's
   the one loaded, the helper stops there: "Command Center is up to date". If it's there but not
   loaded (an earlier run found the Desktop open), it skips to step 6. Otherwise it's downloaded
   the same way and checked against its SHA-256.
5. Then `systemd-sysupdate --component=command-center --verify=no update` copies it into
   `/var/lib/extensions/command-center.raw.v/`, with the transfer file in
   `/usr/lib/sysupdate.command-center.d/50-command-center.transfer` (source: the work directory,
   `InstancesMax=2`). `--verify=no` because steps 3 and 4 did the checking. A plain
   `command-center.raw` from the first install is removed once this version is in `.raw.v/`. sysupdate
   copies the image, so the work directory is emptied after it.
6. `systemd-sysext refresh` only with the Desktop's lock taken (`/run/user/<uid>`, for the user
   pkexec says asked; exclusive, never waiting) and held across the check and the refresh, so a
   Desktop that starts meanwhile waits for it, and only if no other process's program is under
   `/usr/lib/command-center` or a `/usr/bin/cc-*` (the dashboard tab's runs with the Desktop
   closed), apart from the helper, its pkexec and the `cc-home update` that ran pkexec. Nothing
   further up is skipped, so a cc-box from the image above the terminal counts. Otherwise the new image
   is loaded at the next boot, or by running `cc-home update` again once those have closed. It never
   stops anything. One gap is left: SteamVR starting the dashboard tab's process in the few
   milliseconds between the check and the refresh, since the tab doesn't take that lock.
   sysupdate never installs an older version than the newest one installed unless it's asked for
   one, which the helper never does.
7. If that refresh brought a new or changed polkit action (the `.policy` file's bytes differ from
   before), `systemctl try-restart polkit.service`, since polkitd only reads actions when it
   starts. If the restart fails, the helper says so and exits with an error, even though the image
   is loaded, because the next update would ask for a password.

Before any of that, its first act is `chdir("/")`, and it checks `/var/lib/command-center` is a real
directory, root's, that no one else can write (the image's `tmpfiles.d` entry makes it at boot,
and the helper makes it if it isn't there yet). Otherwise it does nothing. It holds
`/var/lib/command-center/update.lock` for the whole run, so a second helper stops with "an update
is already running". A Desktop that starts while the helper holds the Desktop's lock says
"Command Center is updating; starting in a moment" in its log and waits.

The helper takes no arguments and reads no paths from anywhere, and every program it runs is
called by its full path with an empty environment. Signing a release stays a step done by hand,
with the key's secret half, which nothing here touches: each release needs `SHA256SUMS.sig`, a
detached signature over `SHA256SUMS` made with that key.

Tried on the Frame (systemd 257, as the steamos user against a scratch `--root`): the transfer
from a local directory works, and `update` with no version installs nothing older than the newest
installed ("No update needed.").

Tested on the Frame on October 29, 2026, with a throwaway squashfs extension (`cc-n9-test`,
removed after):

1. vpick works on 257. With `cc-n9-test_1.raw` and then `cc-n9-test_2.raw` in
   `/var/lib/extensions/cc-n9-test.raw.v/`, the refresh said `Using extensions 'cc-n9-test_2.raw'`:
   the newest wins, so the `.raw.v/` layout is the one to use.
2. polkit (`allow_active=yes`): `pkcheck --action-id … --process` from a remote login session said not
   authorized; for cc-panels' process in the headset's own session (session 2, `Remote=no`,
   `Active=yes`) it said authorized, with no prompt. The catch: polkitd didn't see an action
   that arrived by a sysext merge ("not registered") until `systemctl restart polkit`. That's why
   the first install restarts it and the helper does too when the `.policy` changes (step 7).

Still to do with a real image: one full `cc-home update` from a signed release.

### Troubleshooting

- **`Failed to read metadata …: Permission denied` from systemd-sysext:** it opens the current
  directory first, and root can't read a user's FUSE mount (`~/Cloud`, say). Run it from `/`
  (`cd / && sudo systemd-sysext refresh`). The helper and cc-install's hint already do.
- **`cc-home update` asks for a password:** polkitd hasn't read the action yet. Run
  `sudo systemctl try-restart polkit.service` once, or restart the Frame.
- **Going back to the previous version:** `InstancesMax=2` keeps the one before. Remove the newest
  file in `/var/lib/extensions/command-center.raw.v/`, then `cd / && sudo systemd-sysext refresh`.
  The next `cc-home update` installs the newest release again.

### When the image isn't loaded

If the image is in `/var/lib/extensions` but systemd-sysext didn't load it, nothing of Command
Center is there to run, the launcher tile included, so it can't say so itself. The installer
(cc-install, which runs from its own download) checks for that and says how to load it:
`cd / && sudo systemd-sysext refresh && sudo systemctl try-restart polkit`, or a restart. When the image is loaded, `cc-home desktop`
(cc-launch) checks that its extension-release file is ours before it starts anything.

### Uninstall

```
sudo rm /var/lib/extensions/command-center.raw   # or the .raw.v directory
cd / && sudo systemd-sysext refresh
```

That's everything system-wide gone: the commands, the driver, both `.desktop` files and the polkit
action. SteamVR's per-user list still names the tab's manifest, so run `cc-home install remove`
first: with SteamVR running it removes it (`RemoveApplicationManifest`), and with SteamVR off it
takes the one line out of appconfig.json. A path SteamVR can't find again just isn't listed. `systemd-sysext.service` can stay enabled, since with nothing in the extension dirs its
conditions skip it. cc-install's Remove does this, then asks before deleting `~/.config/control-center`
and `~/.cache/control-center`. The Desktop tile is SteamOS's again on its own.

### OS updates

- **The image survives:** `/var/lib/extensions` is copied to the new slot.
- **The service stays enabled:** `*.wants/**` is on the keep list.
- **On a new SteamOS version,** the image still merges (`ID=_any`), and cc-launch checks that the
  libraries it was built against still load (see the release file above). If they don't, it says so and
  cc-install, or the sysupdate timer, fetches an image built for that SteamOS.
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

For a release, `CC_RELEASE=1 CC_VERSION=0.9.1 packaging/sysext/build.sh out/command-center_0.9.1.raw`.
In release mode build.sh refuses a version that isn't from a tag (`v0.9.1` on the checkout's
HEAD), a `-dirty` one, one that doesn't start with a digit, and a checkout with uncommitted
changes, because sysupdate orders the images by version. Without `CC_RELEASE` the version is
`git describe`, which is fine for testing.

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

- Does the VR launcher show `command-center.desktop`, with its art, without a reboot
  (`kbuildsycoca6`)? Does a second Desktop-template entry confuse it?
- ~~Does `.raw.v/` vpick work for sysext on 257 here?~~ Yes (October 29, 2026; see section 4).
- How does `systemd-sysext refresh` behave while vrserver has the driver mapped?
