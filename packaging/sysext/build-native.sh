#!/usr/bin/env bash
# Builds the Frame side for SteamOS itself, in the SteamOS build container (steamos-buildenv.sh),
# so it runs on the host with no Fedora container: cc-home, FreeRDP 3.31.1 against SteamOS's own
# FFmpeg 7 and libpulse (sound and microphone, docs/audio.md), libvncclient, cc-panels against those, and the pointer driver. build.sh then packs the results into the sysext image.
#   packaging/sysext/build-native.sh
# It builds in place, into the checkout's usual paths (target/, panels/third_party/prefix), so a
# checkout built this way runs natively too. Don't run it in the checkout the Desktop is running
# from: it replaces that checkout's FreeRDP. Use a separate one (a worktree).
# panels/third_party/prefix/steamos-release says the prefix is native, and cc-box then runs
# commands directly instead of in the container (session.rs, boxed()).
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
root=$(cd "$here/../.." && pwd)
[ -n "${CC_STEAMOS:-}" ] || exec "$here/steamos-buildenv.sh" run "$here/build-native.sh" "$@"
cd "$root"
t0=$SECONDS
step() { printf '\n\033[1;36m// %s\033[0m\n' "$*"; } # Warp Cyan kicker

# Static musl, so it doesn't matter where it builds. SteamOS has no llvm-ar, and binutils' ar
# handles ring's objects just as well.
step "cc-home"
AR_aarch64_unknown_linux_musl=ar cargo build --release -q --target aarch64-unknown-linux-musl -p cc-home

step "FreeRDP 3.31.1"
fr=panels/third_party
[ -d "$fr/FreeRDP" ] || git clone -q --depth 1 --branch 3.31.1 https://github.com/FreeRDP/FreeRDP.git "$fr/FreeRDP"
# The H.264 threading patch, the same one cc-home install makes (install.rs).
sed -i 's/yuv_context_new(Compressor, 0)/yuv_context_new(Compressor, THREADING_FLAGS_DISABLE_THREADS)/' "$fr/FreeRDP/libfreerdp/codec/h264.c"
# And rdpsnd's drop limit, twice its latency (install.rs says why).
sed -i 's/maxDuration = duration \* 2 + rdpsnd->latency;/maxDuration = duration * 2 + rdpsnd->latency * 2;/' "$fr/FreeRDP/channels/rdpsnd/client/rdpsnd_main.c"
# The feature set comes from install.rs, so the container build and this one can't drift apart.
# That includes WITH_PULSE=ON, and SteamOS ships libpulse's headers. On top of it: no ICU (its soname changes every release), no uriparser (AAD only) and no VA-API
# encoder (a server feature, and Fedora's build never found libva anyway).
mapfile -t flags < <(sed -n '/^const FREERDP_FLAGS/,/^];/p' crates/cc-home/src/install.rs | grep -oE '"-D[^"]+"' | tr -d '"')
[ ${#flags[@]} -gt 30 ] || { echo "couldn't read FREERDP_FLAGS from install.rs" >&2; exit 1; }
cmake -S "$fr/FreeRDP" -B "$fr/FreeRDP/build-steamos" -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$root/$fr/prefix" -DCMAKE_INSTALL_LIBDIR=lib64 "${flags[@]}" \
  -DWITH_UNICODE_BUILTIN=ON -DWITH_URIPARSER=OFF -DWITH_VAAPI_H264_ENCODING=OFF >/dev/null
ninja -C "$fr/FreeRDP/build-steamos" >/dev/null
# Unlinked first, not overwritten, so a running cc-panels isn't using a file that changes under it.
rm -f "$fr"/prefix/lib64/lib{freerdp3,freerdp-client3,winpr3}.so*
ninja -C "$fr/FreeRDP/build-steamos" install >/dev/null
. /etc/os-release
echo "$VERSION_ID" >"$fr/prefix/steamos-release"
touch "$fr/prefix/with-pulse" # install.rs rebuilds a prefix without it

# tools/build-libvncclient.sh holds libvncclient's commit, patch and options, and it builds the
# same way here as in the container. Static, so cc-panels only gains SteamOS's libssl, libjpeg and libz.
step "libvncclient"
tools/build-libvncclient.sh "$fr/vnc-prefix"

step "cc-panels"
cargo build --release -q -p cc-panels

# cc-box runs it directly now that the prefix is native.
step "pointer driver"
driver/cc_pointer/build.sh

echo "built natively for SteamOS $VERSION_ID in $((SECONDS - t0)) s"
