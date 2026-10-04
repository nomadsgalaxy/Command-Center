# Packaging the host

I want Command Center managed the way the rest of a system is: installed, updated and removed by
the system's package manager, and easy for anyone to uninstall. On the host that means real distro
packages. Arch comes first, then Fedora and Debian/Ubuntu with the same layout. The Frame (SteamOS)
gets a systemd-sysext image, which cc-dev is designing separately.

> **Built so far:** the Arch package (`packaging/arch/PKGBUILD`, `makepkg -si` from the repo) and
> cc-host's packaged mode: it finds the package's krdp, leaves the units, launcher and grants to the
> package, moves an old user install out of the way, restarts itself after an update, and `check`
> and `uninstall` know about the package. `cc-host units <dir>` writes the units for a package
> build. The rest of this page is still the plan.

## What's wrong with how it installs today

- cc-host copies itself to ~/.local/share/control-center and writes systemd user units into
  ~/.config/systemd/user. No package manager knows about any of it, so updating means
  re-running an installer, and removing it means `cc-share uninstall` and trusting it got
  everything.
- The patched krdp isn't ours at all. share@ and frame@ run the system's `/usr/bin/krdpserver`,
  which had our patches only because someone ran `makepkg -si` with krdp/PKGBUILD by hand. The
  next `pacman -Syu` that updates krdp replaces it with upstream, and the pointer fix and the
  clipboard quietly disappear. window@ runs yet another hand-built copy in ~/src/krdp-window.

## The package: `command-center-host`

One package per distro, with the same files everywhere:

| Path | What |
| --- | --- |
| `/usr/bin/cc-host` | the agent, pairing, tag screens, setup and checks |
| `/usr/bin/cc-share` | a symlink to cc-host (it acts on the name it's run as), so the documented commands keep working |
| `/usr/lib/command-center/krdpserver` | our patched krdp (pointer offset, clipboard, window streams), its own name and path |
| `/usr/lib/command-center/krdpserver-window` | the same build under a second name, so window streams get their own KWin grant (W1) |
| `/usr/lib/command-center/libKRdp.so.6*` | krdp's library, private: both binaries find it through RPATH, never the system's |
| `/usr/share/applications/com.commandcenter.krdpserver.desktop` | NoDisplay; `Exec=/usr/lib/command-center/krdpserver`, `X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input,zkde_screencast_unstable_v1` |
| `/usr/share/applications/com.commandcenter.krdpserver-window.desktop` | the same for the window build, plus `org_kde_plasma_window_management` |
| `/usr/lib/systemd/user/control-center-*.service` | share@, frame@, window@, agent, guard, announce |
| `/usr/share/applications/command-center-host.desktop` | the app menu launcher (start, and Stop in its menu) |
| `/usr/share/licenses/command-center-host/` | the license files, including krdp's LGPL |

Notes on that layout:

- **It never conflicts with the distro's krdp.** Our files have different names and paths, and
  our libKRdp lives in our private directory, so `pacman -Syu` updates both cleanly. Someone can
  have the distro's krdp installed for its own desktop sharing next to ours.
- **The KWin grants are ours.** KWin decides which programs get its privileged protocols (fake
  input, screencast, window management) from a `.desktop` file whose `Exec=` is the program's
  path. krdp's own file names `/usr/bin/krdpserver`, so our binaries need their own. That's also
  why window streams use a second binary: its grant includes window management, and the monitor
  servers shouldn't have it (W1).
- **We only install what we run** from krdp's build: the binary and the library. Not its KCM,
  its app-org.kde.krdpserver.service, its preset or its icons, since those belong to the distro's
  krdp.
- **On Fedora and Debian,** private executables conventionally go in `/usr/libexec/command-center`.
  The units and grant files get the path at build time, and cc-host looks in both places, so one
  codebase covers all three layouts.

## What `cc-host install` becomes

The package puts the files in place. `cc-host install <monitors>` (or `cc-share install`) is the
per-user step after it, because a package runs as root and can't set up someone's session:

- It picks the shared monitors (asking if none are given), makes the password and krdp's
  certificate under ~/.config/control-center, enables and starts the user units, prints the
  firewall hint, and ends with the pairing walkthrough, as it does now.
- It no longer copies binaries or writes unit files. On a machine installed the old way, it
  removes ~/.local/share/control-center/cc-host, the old unit files in ~/.config/systemd/user (they
  would shadow the packaged ones) and the old ~/.local/bin/cc-share. Pairings, keys and the host
  id in ~/.config/control-center stay, so no Frame has to pair again (R10).
- `cc-share check` reports the package version, and that the patched krdp is the one running.

## Updates

- **The package manager updates the files.** The running user services keep the old binaries
  until they restart, so the agent and guard notice when their own executable was replaced
  (`/proc/self/exe` ends in "(deleted)") and exit with code 75 at the next idle moment. Their units
  restart them with the new binary. Sessions a Frame is using finish first; the agent only does
  this when no viewer is connected. That way an update takes effect without anyone logging out,
  and without the package touching anyone's session.
- **A krdp update from the distro doesn't matter to us,** since ours is a separate build. When we
  move to a newer krdp, our package rebuilds it with our patches.

## Uninstalling

- `pacman -R command-center-host` (or `dnf remove`, `apt remove`) removes every file the package
  installed.
- The package's removal script prints what stays: the per-user state in ~/.config/control-center
  (pairings, keys, settings) and the unit links in ~/.config/systemd/user. Running
  `cc-share uninstall` first stops the services, unpairs the Frames and removes those links;
  deleting ~/.config/control-center removes the rest.
- A package can't reach into user sessions, so the running services keep going until logout,
  using files that are already gone. The removal script says that too.

## How Arch users get updates

| Option | How it works | For | Against |
| --- | --- | --- | --- |
| **AUR** (`command-center-host`) | a PKGBUILD on aur.archlinux.org; an AUR helper builds it on the user's machine | the place Arch users look first; no hosting | everyone compiles krdp and cc-host themselves (base-devel, Rust, a few minutes); updates only with an AUR helper; not usable on SteamOS |
| **Our own pacman repo** on GitHub Releases | CI (GitHub Actions, in an archlinux container) builds the package and a signed repo database on every release, and uploads them to a fixed release per architecture (`arch-repo-x86_64`, `arch-repo-aarch64`). Users add a `[command-center]` section with that release as its `Server` and our signing key | prebuilt binaries; plain `pacman -Syu` updates; signed packages and database; free hosting | a one-time step to add the repo and trust our key; we own the signing key and the CI |
| **Our own repo on the framecc Pages site** | the same files, served from GitHub Pages | a nicer URL | Pages has size limits and slower publishing than release assets |

**My recommendation:** our own signed pacman repo on GitHub Releases as the main channel, so updates
arrive with the normal `pacman -Syu`. Alongside it, an AUR package (`command-center-host-bin`)
that just installs the release's prebuilt package, so people searching the AUR find it. The
one-line installer (`install` script) adds the repo and the key, then installs the package.

Decided since:

- **aarch64 from the start.** Every release builds x86_64 and aarch64 (Arch Linux ARM). Each
  architecture gets its own release, `arch-repo-x86_64` and `arch-repo-aarch64`, so the
  `[command-center]` section uses `Server = https://github.com/nomadsgalaxy/Command-Center/releases/download/arch-repo-$arch`.
  x86_64 builds in an `archlinux:base-devel` container. aarch64 builds on GitHub's
  `ubuntu-24.04-arm` runners in the official Arch Linux ARM root (its tarball through
  `docker import`), so there's no unofficial image and no cross build.
- **One signing key.** A dedicated Command Center key signs the packages, the repo databases and
  the release's SHA256SUMS (docs/ssh-free.md §2). It's kept in 1Password, and CI gets it as a
  GitHub Actions secret. Nothing else ever holds it.

### Making the signing key (once, by hand)

This runs on a trusted machine with `gpg`, `op` (signed in) and `gh` (logged in). The key never
touches a command line or the disk outside a throwaway GPG home, and it has no passphrase,
because CI signs unattended. 1Password and GitHub's secret store are what protect it.

```sh
export GNUPGHOME="$(mktemp -d)"
gpg --batch --passphrase '' --quick-gen-key 'Command Center Signing Key' ed25519 sign never
FPR=$(gpg --list-keys --with-colons | awk -F: '/^fpr/ {print $10; exit}')

# The private key: into 1Password, then into the repo's Actions secrets. Both read it from stdin.
gpg --armor --export-secret-keys "$FPR" | op document create - --title 'Command Center signing key' --file-name command-center-signing.asc --vault <vault>
gpg --armor --export-secret-keys "$FPR" | gh secret set CC_SIGNING_KEY --repo nomadsgalaxy/Command-Center

# The public half and the fingerprint aren't secret: they go in the repo and the README.
gh variable set CC_SIGNING_FPR --body "$FPR" --repo nomadsgalaxy/Command-Center
gpg --armor --export "$FPR" > packaging/command-center.asc
echo "$FPR"

rm -rf "$GNUPGHOME"; unset GNUPGHOME
```

Then commit `packaging/command-center.asc`, put the fingerprint in the README, and publish both on
the website. The installer adds the key with `pacman-key --add` and `pacman-key --lsign-key
<fingerprint>`, so a user trusts this one key only, and only for our repo's packages.

If the key ever leaks, make a new one the same way, replace the secret, and ship the new public key
in a package signed by the old one before revoking it.

## SteamOS hosts

SteamOS's root is read-only, so pacman packages don't install there in the normal way. A Steam
Deck used as a host gets the same systemd-sysext image approach as the Frame, with
`command-center-host`'s files in it. The sysext design is cc-dev's.

## Fedora, then Debian and Ubuntu (designed now, built later)

- **Fedora:** a spec file with the same layout (`/usr/libexec/command-center`), built in COPR
  (`dnf copr enable nomadsgalaxy/command-center`, then `dnf install command-center-host`). COPR
  builds from source, so the spec vendors the Rust crates (`cargo vendor`) and pins krdp's
  tarball by checksum. Updates come with `dnf upgrade`.
- **Debian and Ubuntu:** a `.deb` with the same layout, built in CI, in our own signed apt
  repository (GitHub Pages or release assets, with a `Release` file signed by our key). A
  Launchpad PPA is the alternative for Ubuntu, but it builds from source with no network, so
  everything would have to be vendored, and it only covers Ubuntu. Our own repo covers both.
- **Debian and Ubuntu need Plasma 6** (krdp 6.x needs KF6 and KWin 6): that's Debian 13 and
  newer, and Ubuntu 24.10 and newer.

## A Flatpak for the host: not viable

Honestly, a Flatpak can't do what the host needs today:

- **KWin's privileged protocols.** krdp gets fake input and screencast, and the window build gets
  window management, because KWin finds an installed `.desktop` file whose `Exec=` matches the
  program. A sandboxed app doesn't get those. It would have to use the RemoteDesktop and
  ScreenCast portals, which ask permission every session (KDE can remember a screencast choice,
  but not reliably for remote input), and which krdp's `--plasma` mode doesn't use.
- **systemd user units.** A Flatpak can't install or control units. The agent starts and stops a
  krdp server per monitor per Frame (frame@, window@), which needs `systemctl --user` outside the
  sandbox. The background portal can only autostart the app itself.
- **The firewall.** Opening 3399-3449 needs `sudo firewall-cmd` or `ufw`, which the sandbox can't
  run, and shouldn't.
- **The guard.** It changes monitor modes through kscreen-doctor. That might work over D-Bus with
  the right permissions, but it's the least of the problems.

A Flatpak could make sense later for one setup only: a host that serves through the portals, with
a server living inside one process (the IronRDP idea in docs/rust-rdp-server.md). That's a
different product from what krdp gives us, so it's not part of this plan.

## The host app (a GUI for desktops and laptops)

The package also ships a small app, **Command Center Host**, so nobody has to use a terminal to
share monitors, pair a Frame or update. It's a front end only: every button runs cc-host's
existing logic (the same code as `cc-share install`, `pair`, `unpair`, `lock`, `check`), so the
app and the command line can't disagree.

### Stack

- **Slint**, in Rust. On KDE it uses Slint's Qt backend, so it looks like any other Plasma app
  (Breeze, the system font, dark mode). Elsewhere it falls back to Slint's own renderer with its
  Fluent style. Qt is already on every KDE host, since krdp needs it.
  - iced and egui are pure Rust too, but neither looks native on Plasma. egui's immediate mode
    suits tools more than a settings app.
  - Slint's royalty-free license allows a desktop app like this, as long as the About page
    credits it ("Made with Slint").
- **A tray icon** through StatusNotifierItem (the `ksni` crate, over D-Bus). It works on KDE,
  XFCE, Cinnamon and most others. GNOME needs the AppIndicator extension, so the tray is
  optional: the app is a normal window first.
- **Packaging:** `/usr/bin/cc-host-app`, its `.desktop` entry in the app menu, and an optional
  autostart entry (off by default) for the tray. It's in the same `command-center-host`
  package.

### What it shows

```
+--------------------------------------------------------------------+
| Command Center Host                                    [ Stop ]    |
|                                                                    |
|  * Running, version 0.4.0 (the newest)                             |
|    Announced on the network - Firewall open for the Frame          |
|                                                                    |
|  Monitors                                                          |
|  +------------+  DP-1     5120 x 1440   [x] Share                  |
|  | (preview)  |                                                    |
|  +------------+  DP-3     1440 x 2560   [x] Share                  |
|                                                                    |
|  Frames                                                            |
|   frame      paired 2 days ago - streaming monitors 0 and 1        |
|                                                     [ Unpair ]     |
|                                                                    |
|  [  Pair a Frame  ]        [ Pause sharing ]                       |
|                                                                    |
|  Settings - Diagnostics - About                                    |
+--------------------------------------------------------------------+
```

- **The status line** comes from `cc-share check`: running or stopped, the package version and
  whether a newer one exists, announcing on or off, the firewall, and whether the patched krdp
  is the one running. A problem shows as one plain sentence with the fix ("The firewall blocks
  the Frame. Open it" opens the sudo prompt).
- **Monitors:** each enabled output with its name and native size, and a Share checkbox.
  Changing them is `cc-host install <monitors>`. The preview is a small screenshot through KWin's
  screenshot D-Bus interface when the app is allowed it; otherwise there's no picture, just the
  name and size.
- **Frames:** each paired Frame with when it was last used and which monitors it's streaming
  (`cc-share frames`), and Unpair (`cc-share unpair`, which cuts it off at once).
- **Pair a Frame** opens the same full-screen key that `cc-share pair` shows today. The app
  doesn't draw its own, so the Frame can still read the key by looking.
- **Pause sharing** is `cc-share lock` and `unlock`. The privacy cover goes next to it when it
  exists.
- **Settings:** start at login (`autostart on/off`), announce on the network (`announce on/off`),
  window streams (`windows on/off`), the clipboard size cap (`clipboard_max_mb`) once the
  clipboard lands, and the update choice below.
- **Diagnostics:** the full `cc-share check` output, and **Copy logs**, which puts the last
  journal lines of the agent and the units on the clipboard for a bug report.

The UI copy is plain and friendly: "Sharing 2 monitors. Your Frame can connect." rather than
unit names.

### Updates

The package repository stays the source of truth. The app only checks and hands off:

- **It checks** our repo's metadata (the `arch-repo-$arch` release, or COPR's or the apt repo's), at
  most once a day, and shows **Update available (0.4.1)** with what's new from the release
  notes.
- **Update** goes through **PackageKit**, the same D-Bus service Discover and GNOME Software use,
  so it works on any desktop and asks for the password the way the system normally does.
  - **On Fedora and Debian/Ubuntu** that updates our package alone, which is safe there.
  - **On Arch it can't.** Updating one package against a system that isn't up to date is a
    partial upgrade, which Arch doesn't support (our package is built against current Arch
    libraries). So on Arch, Update runs a full system update through PackageKit (or opens
    Discover where it's installed) and says so first.
- **Automatic updates** are a setting, off by default. On Fedora and Debian they can be a systemd
  timer in the package that updates our package alone. On Arch I'd leave them off: an unattended
  full upgrade is a risk the user should choose deliberately, and Arch's own tooling is the
  right place for that.
- **After an update** nobody needs to log out: the agent and guard restart themselves on their
  new binaries when no viewer is connected, as described under Updates above. The app shows
  "Updated. Command Center restarted itself." when it sees the new version running.

### Removing it

Settings has **Remove Command Center**. It explains what happens first:

1. It runs `cc-share uninstall`: the services stop, the Frames are unpaired, and the unit links
   are removed.
2. It offers to delete ~/.config/control-center (pairings, keys, settings), off by default.
3. It removes the package through PackageKit (the system asks for the password).

The command line route (`cc-share uninstall`, then `pacman -R command-center-host`) does the same.

### Not in the first version

- A preview that updates live. One still picture is enough to tell monitors apart.
- Managing several Frames' permissions individually, beyond Unpair.
- A Windows or macOS version. That comes with those hosts (D-043), and Slint runs there too.

## The krdp patches stay as they are

Each package build downloads krdp's release tarball, checks its checksum, applies
krdp/pointer-offset.patch, clipboard.patch and window-stream.patch, and builds it into our
private directory. The clipboard work (files, HTML, images, the echo guard) goes on in
clipboard.patch unchanged and ships with the next package.

## Order of work

1. This design, reviewed.
2. The Arch package: a PKGBUILD in `packaging/arch/`, cc-host changes (the install step, the
   krdp path lookup, the self-restart on update, the migration from the old layout), and the
   uninstall messages.
3. CI: build the package and the signed repo on release, upload to `arch-repo-$arch`, and the installer
   adds the repo.
4. The clipboard patch, built and tested in the package.
5. The host app (Slint): status, monitors, pairing, Frames, settings, diagnostics, then updates
   through PackageKit and removal.
6. Fedora (COPR), then Debian and Ubuntu (apt repo).
