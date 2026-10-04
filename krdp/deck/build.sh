#!/usr/bin/env bash
# Builds a krdp a Steam Deck can be controlled through, as a systemd-sysext folder.
#
# SteamOS 3.8 ships krdp 6.4.3, which shows the screen but never takes input, for three reasons
# krdp 6.5 fixed. input-thread.patch runs input on krdp's own thread (6.4 runs it on FreeRDP's
# and drops every event). fake-input.patch authenticates with KWin, which ignores fake input
# otherwise, and sends pointer positions and scrolls as wl_fixed, which puts the pointer where
# you point instead of near the top-left corner.
#
# It builds in an Arch container pointed at SteamOS's own package repos, so it links against
# exactly the Deck's libraries. The result is ./command-center: krdpserver and libKRdp.so.6 in
# /usr/lib/command-center (RPATH there, so the system's krdp is untouched), the .desktop that
# gets it KWin's fake-input grant, and an extension-release file pinned to this SteamOS version.
# After a SteamOS update the extension doesn't load and the stock krdp runs again, until this is
# built again. It installs nothing. Install it with:
#   sudo cp -a command-center /var/lib/extensions/ && sudo systemctl enable --now systemd-sysext
#   sudo systemd-sysext refresh
# Run it on the Deck, which needs podman (SteamOS has it) and a few GB free.
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD
image=docker.io/library/archlinux:base-devel
podman pull -q $image >/dev/null
grep -vE '^\s*(#|$)' /etc/pacman.conf | sed 's#^DBPath.*##' > pacman.conf
cp /etc/pacman.d/mirrorlist mirrorlist
mkdir -p keyrings && cp /usr/share/pacman/keyrings/* keyrings/
podman run --rm -v "$here":/w:z $image bash -euxc '
  cp /w/pacman.conf /etc/pacman.conf; cp /w/mirrorlist /etc/pacman.d/mirrorlist
  cp /w/keyrings/* /usr/share/pacman/keyrings/
  pacman-key --init >/dev/null; pacman-key --populate archlinux holo >/dev/null
  pacman -Syuu --noconfirm --overwrite "*" >/dev/null
  pacman -S --noconfirm --needed --overwrite "*" krdp cmake ninja extra-cmake-modules plasma-wayland-protocols qt6-wayland patch >/dev/null
  pacman -Q krdp qt6-base freerdp kpipewire
  cd /w && rm -rf krdp-6.4.3 build root
  [ -f krdp.tar.xz ] || curl -fsSL -o krdp.tar.xz https://download.kde.org/stable/plasma/6.4.3/krdp-6.4.3.tar.xz
  tar xf krdp.tar.xz && patch -d krdp-6.4.3 -p1 < input-thread.patch && patch -d krdp-6.4.3 -p1 < fake-input.patch
  lib=/usr/lib/command-center
  cmake -S krdp-6.4.3 -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr -DBUILD_TESTING=OFF -DCMAKE_INSTALL_RPATH=$lib >/dev/null
  cmake --build build
  DESTDIR=/w/stage cmake --install build >/dev/null
  r=/w/command-center; rm -rf $r; install -d $r$lib $r/usr/share/applications $r/usr/lib/extension-release.d
  install -m755 /w/stage/usr/bin/krdpserver $r$lib/krdpserver
  cp -P /w/stage/usr/lib/libKRdp.so.6* $r$lib/
  printf "[Desktop Entry]\nType=Application\nName=Command Center monitor server\nExec=$lib/krdpserver\nNoDisplay=true\nX-KDE-Wayland-Interfaces=org_kde_kwin_fake_input,zkde_screencast_unstable_v1\n" > $r/usr/share/applications/com.commandcenter.krdpserver.desktop
  . /usr/lib/os-release 2>/dev/null || true
'
. /etc/os-release
printf 'ID=steamos\nVERSION_ID=%s\n' "$VERSION_ID" > command-center/usr/lib/extension-release.d/extension-release.command-center
chmod -R a+rX command-center
readelf -d command-center/usr/lib/command-center/krdpserver | grep -E 'RUNPATH|RPATH' || true
find command-center -type f | sort
echo BUILD-OK
