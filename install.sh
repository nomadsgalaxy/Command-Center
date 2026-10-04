#!/bin/bash
# Build Command Center on the Steam Frame from this checkout. The one-line install (the `install`
# script beside this one, and cc-install) clones the repo and runs this a stage at a time. You can
# also run it by hand to build a checkout as it is. It's safe to re-run, since it only creates
# what's missing.
#   install.sh container  distrobox in ~/.local (SteamOS's root stays read-only) and the
#                         control-center container (Fedora toolbox), with the build tools, RPM
#                         Fusion's full FFmpeg (H.264) and GBM; the SteamVR runtime is linked in
#                         at /opt/steamvr
#   install.sh cc-home    cc-home, a static musl binary (it runs on the Frame's host too), with a
#                         user-local rustup kept apart from the container's toolchain
#   install.sh desktop    the VR launcher's "Desktop" starts Command Center (cc-home install desktop)
#   install.sh            container, cc-home, then `cc-home install` (crates/cc-home/src/install.rs):
#                         FreeRDP 3.31.1 against that FFmpeg, cc-panels, the pointer driver, and
#                         the commands in ~/.local/bin
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
stage=${1:-all}
case $stage in
  all | container | cc-home) ;;
  desktop) exec "$here/cc-home" install desktop ;;
  *) echo "usage: install.sh [container|cc-home|desktop]" >&2; exit 2 ;;
esac
box=${CC_CONTAINER:-control-center}
export XDG_RUNTIME_DIR=/run/user/$(id -u)
# podman needs the real user bus, and a desktop session may be on a private one (cc-box is)
export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus
step() { printf '\n\033[1;36m// %s\033[0m\n' "$*"; }  # Warp Cyan kicker
# This is cc-box before cc-home exists. distrobox enter starts the container itself (and waits
# for its first-boot setup), and nice keeps the builds gentle the way cc-box does.
inbox() { nice -n 10 "$HOME/.local/bin/distrobox" enter "$box" -- "$@"; }

if [ "$stage" != cc-home ]; then
step "container"
if [ ! -x "$HOME/.local/bin/distrobox" ]; then
  curl -fsSL https://raw.githubusercontent.com/89luca89/distrobox/main/install | sh -s -- --prefix "$HOME/.local"
fi
podman container exists "$box" 2>/dev/null ||
  "$HOME/.local/bin/distrobox" create --yes --name "$box" --image registry.fedoraproject.org/fedora-toolbox:44
packages=(
  gcc-c++ cmake ninja-build pkgconf-pkg-config git curl
  openssl-devel json-c-devel zlib-devel cjson-devel uriparser-devel libusb1-devel libicu-devel systemd-devel  # FreeRDP
  mesa-libgbm-devel libdrm-devel                                                             # GPU buffers
  rust cargo clang-devel llvm                                                                # the Rust workspace (bindgen; ring for musl)
  pipewire-devel plasma-wayland-protocols wayland-devel                                       # Frame windows as panels (capture.rs, session.rs)
  libjpeg-turbo-devel                                                                        # VNC panels' Tight/JPEG (libvncclient, vnc.rs)
)
inbox bash -s "${packages[@]}" <<'EOF'
set -euo pipefail
rpm -q rpmfusion-free-release >/dev/null 2>&1 ||
  sudo dnf install -y -q "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm"
rpm -q ffmpeg-devel >/dev/null 2>&1 || sudo dnf install -y -q --allowerasing ffmpeg ffmpeg-devel  # H.264, not ffmpeg-free
# A rootless container can't trigger udev, so some %post scriptlets fail and dnf reports a
# failed transaction even though every package installed. So trust rpm -q, not dnf.
{ sudo dnf install -y -q "$@" 2>&1 || true; } | { grep -vE "already installed|^Nothing to do|^$" || true; }
rpm -q --whatprovides "$@" >/dev/null || { rpm -q --whatprovides "$@" | grep "^no package" >&2; exit 1; }  # zlib-devel is zlib-ng-compat-devel
[ -e /opt/steamvr ] || sudo ln -s /run/host/opt/steamvr /opt/steamvr
EOF
fi
[ "$stage" = container ] && exit 0
step "cc-home"
# cc-home is a static musl build (docs/rust-host.md), and the cc-home link points at it.
inbox bash -s "$here" <<'EOF'
set -euo pipefail
export RUSTUP_HOME=$HOME/.local/share/cc-rust/rustup CARGO_HOME=$HOME/.local/share/cc-rust/cargo
export PATH=$CARGO_HOME/bin:$PATH
[ -x "$CARGO_HOME/bin/rustup" ] || curl -fsSL https://sh.rustup.rs | sh -s -- -y -q --no-modify-path --profile minimal
rustup target list --installed | grep -qx aarch64-unknown-linux-musl || rustup target add aarch64-unknown-linux-musl
cd "$1" && cargo build --release -q --target aarch64-unknown-linux-musl -p cc-home
EOF
[ "$stage" = cc-home ] || exec "$here/cc-home" install
