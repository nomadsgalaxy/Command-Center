#!/usr/bin/env bash
# The SteamOS build container (docs/packaging-frame.md, "The build environment"): a podman image
# made from this SteamOS's own read-only /usr, so whatever builds in it links against exactly the
# libraries the Frame has (glibc 2.39, FFmpeg 7, Qt 6.8), nothing newer. SteamOS ships the
# toolchain and the headers itself (gcc 15, clang 19, cmake, ninja, pkg-config, git), so nothing gets
# installed into it. Rust comes from the cc-rust rustup in ~/.local/share/cc-rust, mounted in.
# It only reads the host. Nothing on the host changes except podman's own image store.
#   steamos-buildenv.sh                 make the image (localhost/cc-steamos:<VERSION_ID>), if missing
#   steamos-buildenv.sh rebuild         make it again (after a SteamOS update)
#   steamos-buildenv.sh run CMD ARGS... run CMD in it, in the current directory, as me, with my home
#                                       and SteamVR's runtime (read-only) mounted at the same paths
# One image per SteamOS version: 0.3.0 makes cc-steamos:0.3.0 and 0.4 will make its own.
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)

# podman, and the rootfs this copies, are the host's. distrobox-host-exec drops the environment
# and the working directory, so both go on the command line.
if ! grep -qx ID=steamos /etc/os-release 2>/dev/null; then
  command -v distrobox-host-exec >/dev/null || { echo "run this on the SteamOS host" >&2; exit 1; }
  exec distrobox-host-exec env -C "$PWD" CC_RUST="${CC_RUST:-}" CC_NICE="${CC_NICE:-}" bash "$here/steamos-buildenv.sh" "$@"
fi
. /etc/os-release
image=localhost/cc-steamos:$VERSION_ID
export XDG_RUNTIME_DIR=/run/user/$(id -u)
export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus # podman needs the real user bus

make_image() {
  local t0=$SECONDS s
  s=$(mktemp -d)
  trap 'rm -rf "$s"' RETURN
  # /usr is the OS. Of /etc only what a build reads: the loader's paths, the certificates (git and
  # rustup fetch over https), users and groups. The rest of the rootfs is runtime state.
  # Docs, translations, icons, fonts, firmware and kernel modules are 1.7 GB nobody links against.
  # About 40 files in /usr are root-only (setuid helpers, factory /etc), and they stay out.
  echo "copying SteamOS $VERSION_ID's /usr into $image (a few minutes)..."
  mkdir -p "$s"/{tmp,var/tmp,root}
  chmod 1777 "$s/tmp" "$s/var/tmp"
  tar -cf - --ignore-failed-read --warning=no-failed-read \
      --exclude=usr/share/{doc,man,info,locale,help,icons,fonts,wallpapers,sounds} \
      --exclude=usr/lib/{firmware,modules,debug} \
      -C / bin lib sbin usr etc/{ld.so.conf,ld.so.conf.d,ld.so.cache,ssl,ca-certificates,passwd,group,nsswitch.conf,os-release,profile,profile.d} \
      -C "$s" --owner=0 --group=0 tmp var root 2>/dev/null |
    podman import -q --change 'ENV PATH=/usr/local/bin:/usr/bin LANG=C.UTF-8' \
      --message "SteamOS $VERSION_ID ($BUILD_ID) /usr, for building Command Center" - "$image" >/dev/null
  echo "made $image: $(podman image inspect -f '{{.Size}}' "$image" | numfmt --to=iec) in $((SECONDS - t0)) s"
}

case ${1:-} in
  "") podman image exists "$image" || make_image ;;
  rebuild) make_image ;;
  run)
    shift
    podman image exists "$image" || make_image
    rust=${CC_RUST:-$HOME/.local/share/cc-rust}
    # --userns=keep-id: files it writes are mine. --network=host: cargo and git fetch.
    # nice 10 by default, the same as cc-box, since the headset is rendering VR too.
    flags=(--rm --userns=keep-id --network=host --security-opt label=disable
      -v "$HOME:$HOME" -v /opt/steamvr:/opt/steamvr:ro -w "$PWD"
      -e HOME="$HOME" -e RUSTUP_HOME="$rust/rustup" -e CARGO_HOME="$rust/cargo"
      -e PATH="$rust/cargo/bin:/usr/bin" -e CC_STEAMOS="$VERSION_ID")
    [ -t 0 ] && flags+=(-it)
    exec nice -n "${CC_NICE:-10}" podman run "${flags[@]}" "$image" "$@"
    ;;
  *) echo "usage: steamos-buildenv.sh [rebuild | run CMD ARGS...]" >&2; exit 2 ;;
esac
