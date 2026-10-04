#!/usr/bin/env bash
# Our patched krdp, built for a SteamOS host (a Steam Deck, or a Frame sharing its own screen),
# in the SteamOS build container (packaging/sysext/steamos-buildenv.sh). The PKGBUILD next to this
# stays the one place for the version, the checksums and the patches; this reads them from it.
#   krdp/build-steamos.sh [destdir]   default: krdp/steamos/root
# The result is a /usr tree in destdir (krdpserver, libKRdp, the KCM, plus qtkeychain), ready to
# go in a host sysext image. It installs nothing.
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
mkdir -p "$work"
cd "$work"
t0=$SECONDS

ecm=6.14.0       # SteamOS 0.3's Frameworks
qtkeychain=0.15.0
. "$here/PKGBUILD" # pkgver, source, sha256sums

fetch() { [ -f "$2" ] || curl -fsSL -o "$2" "$1"; }

# extra-cmake-modules, the version that matches SteamOS's Frameworks
fetch "https://download.kde.org/stable/frameworks/${ecm%.*}/extra-cmake-modules-$ecm.tar.xz" ecm.tar.xz
rm -rf "ecm-$ecm" && tar xf ecm.tar.xz && mv "extra-cmake-modules-$ecm" "ecm-$ecm"
cmake -S "ecm-$ecm" -B ecm-build -G Ninja -DCMAKE_INSTALL_PREFIX="$work/ecm" -DBUILD_TESTING=OFF -DBUILD_HTML_DOCS=OFF -DBUILD_MAN_DOCS=OFF -DBUILD_QTHELP_DOCS=OFF >/dev/null
cmake --build ecm-build --target install >/dev/null

# qtkeychain, against SteamOS's libsecret
fetch "https://github.com/frankosterfeld/qtkeychain/archive/refs/tags/$qtkeychain.tar.gz" qtkeychain.tar.gz
rm -rf "qtkeychain-$qtkeychain" && tar xf qtkeychain.tar.gz
cmake -S "qtkeychain-$qtkeychain" -B qtkeychain-build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
  -DBUILD_WITH_QT6=ON -DBUILD_TRANSLATIONS=OFF -DBUILD_TEST_APPLICATION=OFF >/dev/null
cmake --build qtkeychain-build >/dev/null
DESTDIR="$dest" cmake --install qtkeychain-build >/dev/null

# krdp: the PKGBUILD's tarball, checked against its sha256, and its patches in its order
tarball=$pkgname-$pkgver.tar.xz
fetch "https://download.kde.org/stable/plasma/$_dirver/$tarball" "$tarball"
echo "${sha256sums[0]}  $tarball" | sha256sum -c --quiet
rm -rf "$pkgname-$pkgver" && tar xf "$tarball"
for s in "${source[@]}"; do
  case $s in *.patch) patch -d "$pkgname-$pkgver" -p1 -s <"$here/$s" ;; esac
done
qt=$(sed -n 's/^Version: //p' /usr/lib/pkgconfig/Qt6Core.pc)
sed -i -e "s/^set(QT_MIN_VERSION .*/set(QT_MIN_VERSION \"$qt\")/" -e "s/^set(KF6_MIN_VERSION .*/set(KF6_MIN_VERSION \"$ecm\")/" "$pkgname-$pkgver/CMakeLists.txt"
cmake -S "$pkgname-$pkgver" -B krdp-build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr -DBUILD_TESTING=OFF \
  -DCMAKE_PREFIX_PATH="$work/ecm;$dest/usr" >/dev/null
cmake --build krdp-build >/dev/null
DESTDIR="$dest" cmake --install krdp-build >/dev/null
echo "krdp $pkgver for SteamOS $CC_STEAMOS in $dest ($(du -sh "$dest" | cut -f1), $((SECONDS - t0)) s)"
