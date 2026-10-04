#!/bin/bash
# Builds libvncclient for VNC panels (crates/cc-panels/src/vnc.rs): LibVNCServer master at a
# pinned commit, our one patch, static, client only. It's the one place the commit, the patch
# and the cmake options live, so the Fedora container (cc-home install) and a SteamOS-native
# build both run this and get the same library.
#   tools/build-libvncclient.sh <prefix> [<source dir>]
# Inputs:  <prefix>, where it installs; <source dir>, where the clone goes (default: beside the
#          prefix, <prefix>/../libvncserver). CC, CFLAGS, CMAKE_GENERATOR and the usual cmake
#          variables come from the environment. It needs git, cmake, a C compiler, and the OpenSSL,
#          libjpeg(-turbo) and zlib headers, found however that system finds them (no dnf here).
# Outputs: <prefix>/lib64/libvncclient.a and <prefix>/include/rfb/*.h, which vncclient-sys
#          binds (VNC_PREFIX=<prefix>). It does nothing when the library is newer than the patched source.
set -euo pipefail
# master, not 0.9.15: only master has the Tight decoder's out-of-bounds fix (2026-05-29), the
# SHA-256 certificate pin and the buffered-read fix (docs/vnc.md, review B1)
commit=42494999e6492aaab9c1db785ecd293ef10b3aed
prefix=$(realpath -m "${1:?usage: build-libvncclient.sh <prefix> [<source dir>]}")
src=$(realpath -m "${2:-$prefix/../libvncserver}")

[ -d "$src/.git" ] || git clone -q https://github.com/LibVNC/libvncserver.git "$src"
if [ "$(git -C "$src" rev-parse HEAD)" != "$commit" ]; then
  git -C "$src" fetch -q origin master
  git -C "$src" checkout -q --detach -f "$commit"
fi

# The patch: a flag that stops libvncclient asking for the next update after every update, so
# cc-panels asks only when the panel wants a picture (attention.rs). It's a field at the end of
# rfbClient, so the bindings have to come from this header.
python3 - "$src" <<'EOF'
import sys, pathlib
src = pathlib.Path(sys.argv[1])
for f, a, b in [
    ("include/rfb/rfbclient.h",
     "GetX509CertFingerprintMismatchDecision;\n} rfbClient;",
     "GetX509CertFingerprintMismatchDecision;\n\n        /** Command Center: don't ask for the next update after each one; the app asks itself */\n        rfbBool ccPacedRequests;\n} rfbClient;"),
    ("src/libvncclient/rfbclient.c",
     "    if (!SendIncrementalFramebufferUpdateRequest(client))\n      return FALSE;\n\n    if (client->FinishedFrameBufferUpdate)",
     "    if (!client->ccPacedRequests && !SendIncrementalFramebufferUpdateRequest(client))\n      return FALSE;\n\n    if (client->FinishedFrameBufferUpdate)"),
]:
    p = src / f
    t = p.read_text()
    if b in t:
        continue
    if a not in t:
        sys.exit(f"{p}: the VNC patch doesn't apply")
    p.write_text(t.replace(a, b, 1))
EOF

lib=$prefix/lib64/libvncclient.a
if [ -f "$lib" ] && [ "$lib" -nt "$src/src/libvncclient/rfbclient.c" ] && [ "$lib" -nt "$src/include/rfb/rfbclient.h" ]; then
  exit 0
fi
# Client only, static, OpenSSL for TLS and for Apple's (ARD) and MSLogon crypto. No server,
# GnuTLS, gcrypt, SASL, examples or tests (review A8). miniLZO is built in.
gen=()
command -v ninja >/dev/null && [ -z "${CMAKE_GENERATOR:-}" ] && gen=(-G Ninja)
cmake -S "$src" -B "$src/build" "${gen[@]}" -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" -DCMAKE_INSTALL_LIBDIR=lib64 \
  -DBUILD_SHARED_LIBS=OFF -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DWITH_LIBVNCSERVER=OFF -DWITH_LIBVNCCLIENT=ON \
  -DWITH_OPENSSL=ON -DWITH_GNUTLS=OFF -DWITH_GCRYPT=OFF -DWITH_SASL=OFF -DWITH_SYSTEMD=OFF -DWITH_FFMPEG=OFF \
  -DWITH_QT=OFF -DWITH_GTK=OFF -DWITH_SDL=OFF -DWITH_LIBSSHTUNNEL=OFF -DWITH_WEBSOCKETS=OFF -DWITH_EXAMPLES=OFF \
  -DWITH_TESTS=OFF -DWITH_LZO=OFF -DWITH_PNG=OFF -DWITH_XCB=OFF -DWITH_TIGHTVNC_FILETRANSFER=OFF >/dev/null
cmake --build "$src/build" --target install >/dev/null
touch "$lib" # newer than the patched source, so the next run skips it
echo "libvncclient $commit in $prefix"
