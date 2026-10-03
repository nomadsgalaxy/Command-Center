#!/bin/bash
# Registers the cc_pointer driver with SteamVR. Each build goes in its own folder named by a hash
# of its files, so a driver SteamVR has already loaded never gets replaced underneath it (that
# breaks its laser until SteamVR restarts). It's safe to re-run, an unchanged build is a no-op.
# SteamVR only loads drivers when it starts, and this never restarts it.
#   install.sh            register this build (and unregister older ones)
#   install.sh uninstall  unregister every cc_pointer build
#   install.sh check      whether the running SteamVR has this build loaded (exit 1: it needs a restart)
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
base=$HOME/.local/share/control-center
# SteamVR reads ~/.config/openvr, but inside Command Center's Desktop XDG_CONFIG_HOME points
# somewhere else, and vrpathreg would quietly write a registry SteamVR never reads.
reg() { XDG_CONFIG_HOME=$HOME/.config LD_LIBRARY_PATH=/opt/steamvr/bin/linuxarm64 /opt/steamvr/bin/linuxarm64/vrpathreg "$@" 2> >(grep -v 'is not an array' >&2); }
registered() { reg show 2>/dev/null | grep -o "$base/cc_pointer-[0-9a-f]*" | sort -u; }
loaded() { for p in $(pgrep -x vrserver); do grep -ho "$base/cc_pointer-[0-9a-f]*" "/proc/$p/maps" 2>/dev/null || true; done | sort -u; }

if [ "${1:-}" = uninstall ]; then
  for d in $(registered); do reg removedriver "$d"; echo "unregistered $d"; done
  echo "restart SteamVR to unload it"
  exit 0
fi

if [ "${1:-}" = check ]; then
  r=$(registered)
  [ -n "$r" ] || { echo "the pointer driver isn't registered"; exit 1; }
  pgrep -x vrserver >/dev/null || { echo "SteamVR isn't running: it loads the pointer when it starts"; exit 0; }
  loaded | grep -qxF "$r" && { echo "SteamVR has this pointer loaded"; exit 0; }
  echo "SteamVR is running without this pointer: it loads it at its next start"
  exit 1
fi

so=$here/build/driver_cc_pointer.so
[ -f "$so" ] || { echo "build it first: $here/build.sh" >&2; exit 1; }
hash=$( (cat "$so"; find "$here/cc_pointer" -type f -print0 | sort -z | xargs -0 cat) | sha256sum | cut -c1-12)
dest=$base/cc_pointer-$hash
if registered | grep -qx "$dest"; then
  echo "cc_pointer $hash already registered"
else
  rm -rf "$dest.tmp"; mkdir -p "$dest.tmp/bin/linuxarm64"
  cp -r "$here/cc_pointer/." "$dest.tmp/"
  cp "$so" "$dest.tmp/bin/linuxarm64/"
  rm -rf "$dest"; mv "$dest.tmp" "$dest"
  for old in $(registered); do reg removedriver "$old"; done
  reg adddriver "$dest"
  echo "registered cc_pointer $hash; restart SteamVR to load it"
fi
# Clean up older builds nobody uses any more (not registered and not loaded by a running SteamVR).
keep=$( (registered; loaded) | sort -u)
for d in "$base"/cc_pointer-*; do
  [ -d "$d" ] && [ "$d" != "$dest" ] && ! grep -qx "$d" <<<"$keep" && rm -rf "$d" && echo "removed old $d"
done
true
