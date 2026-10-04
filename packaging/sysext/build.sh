#!/usr/bin/env bash
# Builds Command Center's Frame side as one systemd-sysext image (docs/packaging-frame.md): a
# read-only squashfs that systemd-sysext lays over SteamOS's /usr and /opt. It only builds and
# checks the file. It never installs, merges or refreshes anything.
#   packaging/sysext/build-native.sh      first: the builds, made for SteamOS (no container)
#   packaging/sysext/build.sh [out.raw]   default: packaging/sysext/out/command-center.raw
# Environment:
#   CC_ROOT     the checkout whose builds go in (default: this one)
#   CC_VERSION  the image's version (default: git describe of CC_ROOT)
# Exits non-zero (3 on the host) when the image is built, but something in it can't load on this SteamOS (the check lists it).
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
root=${CC_ROOT:-$(cd "$here/../.." && pwd)}
out=$(realpath -m "${1:-$here/out/command-center.raw}")

# mksquashfs, and the libraries the check looks for, are SteamOS's, so this runs on the host.
# distrobox-host-exec drops the environment, so it goes on the command line.
if ! grep -qx ID=steamos /etc/os-release 2>/dev/null; then
  command -v distrobox-host-exec >/dev/null || { echo "run this on the SteamOS host" >&2; exit 1; }
  exec distrobox-host-exec env CC_ROOT="$root" CC_VERSION="${CC_VERSION:-}" bash "$here/build.sh" "$out"
fi

name=$(basename "$out" .raw)
version=${CC_VERSION:-$(git -C "$root" describe --always --dirty 2>/dev/null || echo dev)}
lib=usr/lib/command-center
s=$(mktemp -d)
trap 'rm -rf "$s"' EXIT
chmod 755 "$s" # mktemp makes it 0700, and it becomes the image's root

# /usr/lib/command-center mirrors the checkout, so root() (the folder above target/) and the
# RUNPATH cc-panels already carries ($ORIGIN/../../panels/third_party/prefix/lib64) still work.
install -Dm755 "$root/target/aarch64-unknown-linux-musl/release/cc-home" "$s/$lib/cc-home"
install -Dm755 "$root/target/release/cc-panels" "$s/$lib/target/release/cc-panels"
ln -s cc-home "$s/$lib/cc-panels"
ln -s cc-home "$s/$lib/cc-box"
mkdir -p "$s/$lib/session" "$s/$lib/panels/third_party/prefix/lib64" "$s/$lib/crates/cc-panels/actions" "$s/usr/bin"
for c in cc-launch cc-desktop cc-rest; do ln -s ../cc-home "$s/$lib/session/$c"; done
for c in cc-home cc-panels; do ln -s "../lib/command-center/$c" "$s/usr/bin/$c"; done
cp -P "$root"/panels/third_party/prefix/lib64/lib{freerdp3,freerdp-client3,winpr3}.so.3* "$s/$lib/panels/third_party/prefix/lib64/"
# Says the build is native, so cc-box runs things directly (session.rs, native()). A container
# build has none, and the check below would fail it anyway.
[ -f "$root/panels/third_party/prefix/steamos-release" ] && install -m644 "$root/panels/third_party/prefix/steamos-release" "$s/$lib/panels/third_party/prefix/"
install -m644 "$root"/panels/cc-windows.js "$root"/panels/cc-restore.js "$s/$lib/panels/"
install -m644 "$root"/crates/cc-panels/actions/*.json "$s/$lib/crates/cc-panels/actions/"
strip --strip-debug "$s/$lib/cc-home" "$s/$lib/target/release/cc-panels" "$s/$lib"/panels/third_party/prefix/lib64/*.so.*.*

# The pointer driver goes where SteamVR's own drivers are. vrserver loads every folder in
# /opt/steamvr/drivers, so nobody runs vrpathreg.
drv=$s/opt/steamvr/drivers/cc_pointer
mkdir -p "$drv/bin/linuxarm64"
cp -r "$root"/driver/cc_pointer/cc_pointer/. "$drv/"
install -m755 "$root/driver/cc_pointer/build/driver_cc_pointer.so" "$drv/bin/linuxarm64/"

# KWin grants its screencast and fake input by the client's Exec path, which is now a system one.
apps=$s/usr/share/applications
mkdir -p "$apps"
cat >"$apps/org.controlcenter.panels.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Command Center panels
Exec=/$lib/target/release/cc-panels
NoDisplay=true
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1,org_kde_kwin_fake_input
EOF
# Shadows SteamOS's own entry while the image is merged, so the VR launcher's Desktop starts
# Command Center, and removing the image gives the stock one back (nothing per user is left behind).
cat >"$apps/deckard-nested-desktop.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Desktop
Comment=Command Center: your Plasma desktop and remote machines as VR panels
Exec=/$lib/session/cc-launch
Categories=Utility;
EOF

# sysext only merges an image whose release file is named after it and matches the host. SteamOS
# has no SYSEXT_LEVEL, so the match is ID and VERSION_ID: a new SteamOS minor (0.3 to 0.4) leaves
# the image unmerged instead of running it against libraries it wasn't built for.
. /etc/os-release
mkdir -p "$s/usr/lib/extension-release.d"
printf 'ID=%s\nVERSION_ID=%s\nARCHITECTURE=arm64\nIMAGE_ID=%s\nIMAGE_VERSION=%s\n' \
  "$ID" "$VERSION_ID" "$name" "$version" >"$s/usr/lib/extension-release.d/extension-release.$name"

mkdir -p "$(dirname "$out")"
mksquashfs "$s" "$out" -all-root -noappend -comp zstd -quiet -no-progress
echo "built $out ($(du -h "$out" | cut -f1), $name $version, for $ID $VERSION_ID)"
# Without root it can't mount the image to look inside, but it does check that it's one.
systemd-dissect --validate "$out" >/dev/null && echo "systemd-dissect: a valid image"

# Can everything in it load here? Each ELF's libraries have to be in the image, SteamOS's /usr/lib
# or SteamVR's runtime, and it can't want a newer glibc than SteamOS has.
host_glibc=GLIBC_$(ldd --version | sed -n 1p | grep -oE '[0-9.]+$')
dirs=("$s/$lib/panels/third_party/prefix/lib64" /usr/lib /opt/steamvr/bin/linuxarm64)
bad=0
while IFS= read -r f; do
  readelf -h "$f" >/dev/null 2>&1 || continue
  rel=${f#"$s"}
  for n in $(readelf -d "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
    found=
    for d in "${dirs[@]}"; do [ -e "$d/$n" ] && found=1 && break; done
    [ -n "$found" ] || { echo "  $rel needs $n, which SteamOS doesn't have"; bad=1; }
  done
  g=$(objdump -T "$f" 2>/dev/null | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1) || true # nothing for static cc-home
  if [ -n "$g" ] && [ "$(printf '%s\n' "$g" "$host_glibc" | sort -V | tail -1)" != "$host_glibc" ]; then
    echo "  $rel needs $g, SteamOS has $host_glibc"
    bad=1
  fi
done < <(find "$s" -type f)
[ $bad = 0 ] || { echo "the image won't run on this SteamOS: build it against SteamOS's libraries (docs/packaging-frame.md)"; exit 3; }
echo "every program and library in it can load on this SteamOS"
