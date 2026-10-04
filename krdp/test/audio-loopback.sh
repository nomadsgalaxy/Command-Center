#!/usr/bin/env bash
# Loopback test for audio.patch: krdp's Audio (src/Audio.cpp) against a real FreeRDP client, with
# PipeWire private to the test. Nothing live is touched: it makes its own runtime dir, PipeWire,
# PulseAudio socket and WirePlumber (with no hardware), and removes them on exit.
#   krdp/test/audio-loopback.sh <krdp-src> <krdp-build> <freerdp-prefix>
# <krdp-src> is a krdp 6.7.5 tree with all the patches applied, <krdp-build> its cmake build
# directory (for the generated headers) and <freerdp-prefix> a FreeRDP 3 with WITH_PULSE=ON and
# WITH_CLIENT=ON (the private prefix, panels/third_party/prefix in a build of this branch).
# The server side builds and runs here, on the host's own libraries (krdp's). The client runs in the
# Fedora container (CC_CLIENT_BOX, default `control-center`) when the prefix was built there, as
# the Desktop's is, because it links Fedora's libraries; CC_CLIENT_BOX= runs it here instead.
# It checks both directions with tones:
#   a 440 Hz tone played on the "host's" default output arrives at the "Frame's" speakers, and
#   an 880 Hz tone on the "Frame's" microphone arrives in the PipeWire source krdp publishes.
set -euo pipefail
src=$(realpath "$1") build=$(realpath "$2") prefix=$(realpath "$3")
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d /tmp/cc-audio.XXXXXX)
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; [ -n "${CC_KEEP:-}" ] && echo "kept $tmp" || rm -rf "$tmp"; }
trap cleanup EXIT
# Everything that talks to PipeWire runs with this, so nothing here can reach the real session
run=$tmp/run
pw=(env -u PULSE_SERVER -u PIPEWIRE_REMOTE -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR="$run" XDG_CONFIG_HOME="$tmp/config" XDG_STATE_HOME="$tmp/state" XDG_CACHE_HOME="$tmp/cache" HOME="$tmp/home")
mkdir -p "$run" "$tmp/config/wireplumber/wireplumber.conf.d" "$tmp/state" "$tmp/cache" "$tmp/home"
chmod 700 "$run"
box=${CC_CLIENT_BOX-control-center}
clientbox() { if [ -n "$box" ]; then XDG_RUNTIME_DIR=/run/user/$(id -u) distrobox enter "$box" -- "$@" 2>&1 | { grep -v 'level=warning' || true; }; else "$@"; fi; }

echo "// build the server and the client"
cxx=(g++ -std=c++20 -fPIC -O1 -I"$src/src" -I"$build/src" -I"$build" -I"$src")
qt=$(pkg-config --cflags Qt6Core Qt6Gui Qt6Network Qt6DBus) ; qtl=$(pkg-config --libs Qt6Core)
"${cxx[@]}" $qt $(pkg-config --cflags freerdp-server3 freerdp3 winpr3 libpipewire-0.3) \
  "$here/audio-server.cpp" "$src/src/Audio.cpp" "$build/src/krdp_logging.cpp" -o "$tmp/audio-server" \
  $(pkg-config --libs freerdp-server3 freerdp3 winpr3 libpipewire-0.3) $qtl -lpthread 2>&1 | grep -E "error|undefined|cannot" || true
[ -x "$tmp/audio-server" ] || { echo "the server didn't build" >&2; exit 1; }
clientbox gcc -w -O1 -I"$prefix/include/freerdp3" -I"$prefix/include/winpr3" "$here/audio-client.c" -o "$tmp/audio-client" \
  -L"$prefix/lib64" -lfreerdp-client3 -lfreerdp3 -lwinpr3 -Wl,-rpath,"$prefix/lib64"

echo "// private PipeWire, PulseAudio socket and WirePlumber (no hardware)"
cat >"$tmp/config/wireplumber/wireplumber.conf.d/90-test.conf" <<'WP'
wireplumber.profiles = {
  main = {
    monitor.alsa = disabled
    monitor.alsa-midi = disabled
    monitor.bluez = disabled
    monitor.bluez-midi = disabled
    monitor.libcamera = disabled
    monitor.v4l2 = disabled
  }
}
WP
"${pw[@]}" pipewire >"$tmp/pipewire.log" 2>&1 & pids+=($!)
for _ in $(seq 50); do [ -S "$run/pipewire-0" ] && break; sleep 0.1; done
"${pw[@]}" wireplumber >"$tmp/wireplumber.log" 2>&1 & pids+=($!)
"${pw[@]}" pipewire-pulse >"$tmp/pulse.log" 2>&1 & pids+=($!)
sleep 2
# Three null sinks: "host" is the host's default output, "frame" the Frame's speakers, "mic" has the
# Frame's microphone as its monitor
for n in host frame mic; do
  "${pw[@]}" pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=$n media.class=Audio/Sink object.linger=true audio.position=[FL FR] monitor.channel-volumes=true }" >/dev/null
done
sleep 1
HOSTID=$("${pw[@]}" pw-dump | python3 -c 'import json,sys; print([o["id"] for o in json.load(sys.stdin) if o.get("info",{}).get("props",{}).get("node.name")=="host"][0])')
"${pw[@]}" wpctl set-default "$HOSTID"

python3 - "$tmp" <<'PY'
import math, struct, sys, wave
for name, hz in (("host", 440), ("mic", 880)):
    w = wave.open(f"{sys.argv[1]}/{name}.wav", "wb"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(48000)
    w.writeframes(b"".join(struct.pack("<hh", *[int(12000 * math.sin(2 * math.pi * hz * i / 48000))] * 2) for i in range(48000 * 12)))
    w.close()
PY

echo "// krdp's Audio and a client"
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$tmp/key.pem" -out "$tmp/cert.pem" -subj /CN=test -days 1 2>/dev/null
"${pw[@]}" QT_LOGGING_RULES='org.kde.krdp*=true' "$tmp/audio-server" 33890 "$tmp/cert.pem" "$tmp/key.pem" 14 >"$tmp/server.log" 2>&1 & pids+=($!)
sleep 1
clientbox env HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/config" XDG_RUNTIME_DIR="$run" PULSE_SERVER=unix:"$run/pulse/native" PULSE_SINK=frame PULSE_SOURCE=mic.monitor ${CC_CLIENT_ENV:-} \
  "$tmp/audio-client" 33890 11 >"$tmp/client.log" 2>&1 & pids+=($!)
sleep 4
# the "host's" sound, a "user's" voice, and recordings of where each should arrive
"${pw[@]}" pw-cat --playback --target host "$tmp/host.wav" & pids+=($!)
"${pw[@]}" pw-cat --playback --target mic "$tmp/mic.wav" & pids+=($!)
"${pw[@]}" pw-cat --record --target frame -P stream.capture.sink=true --rate 48000 --channels 2 --format s16 "$tmp/frame.wav" & rec1=$!
"${pw[@]}" pw-cat --record --target host -P stream.capture.sink=true --rate 48000 --channels 2 --format s16 "$tmp/hostmon.wav" & rec3=$!
"${pw[@]}" pw-cat --record --target cc_frame_microphone --rate 48000 --channels 1 --format s16 "$tmp/micout.wav" & rec2=$!
sleep 3
[ -z "${CC_DURING:-}" ] || eval "$CC_DURING" || true # for poking at it by hand
sleep 2
"${pw[@]}" pw-dump >"$tmp/dump.json" 2>/dev/null || true
kill -INT $rec1 $rec2 $rec3 2>/dev/null || true
wait $rec1 $rec2 $rec3 2>/dev/null || true
sleep 5 # the server and the client finish on their own
echo "--- server"; cat "$tmp/server.log"; echo "--- client"; cat "$tmp/client.log"

python3 - "$tmp" <<'PY'
import math, struct, sys, wave
def tone(path, hz, ch):
    w = wave.open(path); n = w.getnframes(); d = struct.unpack("<%dh" % (n * w.getnchannels()), w.readframes(n))[::w.getnchannels()]
    d = d[len(d) // 4:]  # skip the start
    if len(d) < 4800: return 0.0, 0.0, 0
    def g(f):
        c = [math.cos(2 * math.pi * f * i / 48000) for i in range(len(d))]; s = [math.sin(2 * math.pi * f * i / 48000) for i in range(len(d))]
        return math.hypot(sum(x * y for x, y in zip(d, c)), sum(x * y for x, y in zip(d, s))) / len(d)
    return g(hz), g(hz * 1.5), max(abs(x) for x in d)
ok = True
for path, hz, what in (("hostmon.wav", 440, "the host's default output as recorded"), ("frame.wav", 440, "the host's sound at the Frame's speakers"), ("micout.wav", 880, "the Frame's microphone in krdp's source")):
    on, off, peak = tone(f"{sys.argv[1]}/{path}", hz, 0)
    good = on > 5000 and on > 100 * off and peak < 12500 # the sent level, undistorted
    ok &= good
    print(("PASS" if good else "FAIL"), what, f"({hz} Hz: {on:.0f}, off-tone {off:.0f}, peak {peak})")
sys.exit(0 if ok else 1)
PY
