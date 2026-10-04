#!/usr/bin/env bash
# Our patched krdp, built for a SteamOS host (a Steam Deck, or a Frame sharing its own screen),
# in the SteamOS build container (packaging/sysext/steamos-buildenv.sh). The Arch package
# (packaging/arch/PKGBUILD) stays the one place for krdp's version, checksum and patches; this
# reads them from it.
#   krdp/build-steamos.sh [destdir]   default: krdp/steamos/root
# The result is a /usr tree in destdir with the Arch package's layout, so one cc-host works on
# every distro: krdpserver and krdpserver-window (the window build needs its own name for its KWin
# grant) and libKRdp.so.6 all in /usr/lib/command-center, next to qtkeychain, with RPATH there. A
# private libKRdp means a SteamOS that someday ships krdp never gets its copy shadowed by ours. It's
# ready to go in a host sysext image, and it installs nothing.
#
# SteamOS 0.3 has Plasma 6.2.5 with Qt 6.8.0 and Frameworks 6.14, and krdp 6.7.5 asks for Qt 6.10
# and Frameworks 6.26. Nothing it uses needs them, so I lower those two minimums to what SteamOS
# has. Two things SteamOS doesn't ship: extra-cmake-modules (only needed to build, so it goes in
# the work dir) and qtkeychain (krdp links it, so it goes in the tree).
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
[ -n "${CC_STEAMOS:-}" ] || exec "$here/../packaging/sysext/steamos-buildenv.sh" run "$here/build-steamos.sh" "$@"
work=$here/steamos
dest=$(realpath -m "${1:-$work/root}")
rm -rf "$dest" && mkdir -p "$work"
cd "$work"
t0=$SECONDS

ecm=6.14.0       # SteamOS 0.3's Frameworks
qtkeychain=0.15.0
pkgbuild=$here/../packaging/arch/PKGBUILD
ver=$(sed -n 's/^_krdpver=//p' "$pkgbuild")
sum=$(grep -oE -m1 "[0-9a-f]{64}" "$pkgbuild")
read -ra patches < <(sed -n 's/^  for p in \(.*\); do$/\1/p' "$pkgbuild")
[ -n "$ver" ] && [ ${#patches[@]} -gt 0 ] || { echo "couldn't read krdp's version and patches from $pkgbuild" >&2; exit 1; }
lib=/usr/lib/command-center

fetch() { [ -f "$2" ] || curl -fsSL -o "$2" "$1"; }

# extra-cmake-modules, the version that matches SteamOS's Frameworks
fetch "https://download.kde.org/stable/frameworks/${ecm%.*}/extra-cmake-modules-$ecm.tar.xz" ecm.tar.xz
rm -rf "ecm-$ecm" && tar xf ecm.tar.xz && mv "extra-cmake-modules-$ecm" "ecm-$ecm"
cmake -S "ecm-$ecm" -B ecm-build -G Ninja -DCMAKE_INSTALL_PREFIX="$work/ecm" -DBUILD_TESTING=OFF -DBUILD_HTML_DOCS=OFF -DBUILD_MAN_DOCS=OFF -DBUILD_QTHELP_DOCS=OFF >/dev/null
cmake --build ecm-build --target install >/dev/null

# qtkeychain, against SteamOS's libsecret
fetch "https://github.com/frankosterfeld/qtkeychain/archive/refs/tags/$qtkeychain.tar.gz" qtkeychain.tar.gz
rm -rf "qtkeychain-$qtkeychain" && tar xf qtkeychain.tar.gz
cmake -S "qtkeychain-$qtkeychain" -B qtkeychain-build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr -DCMAKE_INSTALL_LIBDIR=lib/command-center \
  -DBUILD_WITH_QT6=ON -DBUILD_TRANSLATIONS=OFF -DBUILD_TEST_APPLICATION=OFF >/dev/null
cmake --build qtkeychain-build >/dev/null
DESTDIR="$dest" cmake --install qtkeychain-build >/dev/null

# krdp: the PKGBUILD's tarball, checked against its sha256, and its patches in its order
tarball=krdp-$ver.tar.xz
fetch "https://download.kde.org/stable/plasma/$ver/$tarball" "$tarball"
echo "$sum  $tarball" | sha256sum -c --quiet
rm -rf "krdp-$ver" krdp-build && tar xf "$tarball"
for p in "${patches[@]}"; do patch -d "krdp-$ver" -Np1 -s -i "$here/$p.patch"; done
# KPipeWire 6.2 has setActive() where 6.7 has start() and stop(), and no colour range setting.
# ponytail: without setColorRange the encoder sends limited range (16-235), so colours may look a
# little flat; bundling KPipeWire 6.7 fixes it if it shows.
if ! grep -q "void start()" /usr/include/KPipeWire/pipewirebaseencodedstream.h; then
  sed -i -e 's/encodedStream->start()/encodedStream->setActive(true)/' -e 's/encodedStream->stop()/encodedStream->setActive(false)/' \
    -e '/encodedStream->setColorRange(/d' "krdp-$ver/src/VideoStream.cpp"
fi
qt=$(sed -n 's/^Version: //p' /usr/lib/pkgconfig/Qt6Core.pc)
sed -i -e "s/^set(QT_MIN_VERSION .*/set(QT_MIN_VERSION \"$qt\")/" -e "s/^set(KF6_MIN_VERSION .*/set(KF6_MIN_VERSION \"$ecm\")/" "krdp-$ver/CMakeLists.txt"
cmake -S "krdp-$ver" -B krdp-build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr -DBUILD_TESTING=OFF -DBUILD_EXAMPLES=OFF \
  -DKDE_INSTALL_LIBDIR=lib -DKDE_SKIP_RPATH_SETTINGS=ON -DCMAKE_INSTALL_RPATH=$lib -DCMAKE_PREFIX_PATH="$work/ecm" -DQt6Keychain_DIR="$dest$lib/cmake/Qt6Keychain" >/dev/null
cmake --build krdp-build >/dev/null
rm -rf "$work/krdp-root"
DESTDIR="$work/krdp-root" cmake --install krdp-build >/dev/null
install -Dm755 "$work/krdp-root/usr/bin/krdpserver" "$dest$lib/krdpserver"
install -Dm755 "$work/krdp-root/usr/bin/krdpserver" "$dest$lib/krdpserver-window"
cp -P "$work"/krdp-root/usr/lib/libKRdp.so.6* "$dest$lib/"
echo "krdp $ver for SteamOS $CC_STEAMOS in $dest ($(du -sh "$dest" | cut -f1), $((SECONDS - t0)) s)"
