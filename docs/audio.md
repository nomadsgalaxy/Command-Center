# Sound and microphone

An RDP session carries two things besides the picture: the host's sound, which plays on the Frame,
and the Frame's microphone, which shows up on the host as a microphone any app can pick. They go
together, so a machine either has both or neither.

The camera isn't done. It will be its own task (RDPECAM, a third channel next to these two), and
nothing here gets in its way: FreeRDP's camera channel is still off in the client build, and the
choice of which session carries what (below) is the same choice the camera will need.

## How it goes

- **Host to Frame** is rdpsnd. krdp captures the host's default output (the monitor of its default
  sink, through libpipewire), and sends 48 kHz 16-bit stereo PCM, about 1.5 Mbit/s. On the Frame,
  FreeRDP's rdpsnd plays it through its PulseAudio backend, which PipeWire answers.
- **Frame to host** is audin, a dynamic channel. FreeRDP's audin records the Frame's default
  microphone through PulseAudio and sends 48 kHz 16-bit mono PCM. krdp publishes it as a PipeWire
  source called "Steam Frame microphone (Command Center)", so it's in the device list of every app.
- PCM only. Opus would cut the bandwidth, but it needs an encoder on the host and a decoder on the
  Frame, for a LAN that doesn't need it. If a Wi-Fi link ever does, it's a format added to both
  lists, and nothing else changes.

krdp does nothing for audio until the client joins the rdpsnd channel (`Audio.cpp`, `update()`), so a
session whose client didn't ask costs nothing. The capture starts when the client has answered with
the formats it plays, and the microphone's source appears with the first sound. Both are gone when
the session closes.

## One session per machine

desk-wide and desk-portrait are two monitors of one machine, and each is its own RDP session with
its own krdpserver. If both carried audio you'd hear everything twice and the microphone would be
claimed twice. So the Frame picks one (`config::carries_audio`, `rdp.rs`): the lowest-numbered
screen of the machine's monitors that are shown and not failing to connect. The server offers the
channels in every session, since the client decides.

Because FreeRDP loads its channels when it connects, the choice is made then. When it changes (the
carrying monitor was closed, or came back), the monitor that gains or loses it reconnects once,
right away, which is a second or two of its picture. A monitor that drops and comes back by itself
keeps the audio, so a Wi-Fi blip doesn't make the other one reconnect twice.

Pop-out windows, VNC monitors and Frame window panels never carry audio.

## Turning it off

`audio=no` on a monitor's line in viewers.conf turns off sound and microphone for that whole machine
(one line saying it is enough, since the choice is per machine). It's on by default. cc-home keeps
unknown options when it rewrites the file, so `cc-home machine set desk-wide audio=no` works, and
`audio=` drops it.

## PulseAudio on the Frame

The Desktop session has its own runtime dir (`/run/user/<uid>/cc-desktop`), and `cc-home session`
links the Frame's PulseAudio socket into it. cc-panels also sets `PULSE_SERVER` to
`unix:/run/user/<uid>/pulse/native` at start-up, when it isn't set already and the socket exists, so
it doesn't depend on that link. PipeWire's own PulseAudio server is what answers.

## Building it

- **Client**: FreeRDP gets `WITH_PULSE=ON` (the flag list in `crates/cc-home/src/install.rs`, which
  `packaging/sysext/build-native.sh` reads too). It needs libpulse's headers: `pulseaudio-libs-devel`
  in the Fedora container, and SteamOS already has them. `install.rs` rebuilds FreeRDP once on its
  own when the prefix has no `with-pulse` file. `WITH_OPUS` stays off.
- **Host**: `krdp/audio.patch`, the fourth patch in the PKGBUILD, adds `src/Audio.cpp` and needs
  libpipewire (already there for KPipeWire). `krdp/build-steamos.sh` picks it up from the PKGBUILD.

### Rebuilding the Desktop's own FreeRDP

The Desktop runs from `~/control-center/panels/third_party/prefix`, which is still the build without
PulseAudio until you do this, and until then cc-panels logs "Loaded fake backend for rdpsnd" and plays
nothing. It only replaces FreeRDP and cc-panels, so it doesn't touch the pointer driver or SteamVR.
Run it after the branch is merged, and before the Desktop's next start:

```sh
# once per container; install.sh's package list has it now, so a new container already does
~/control-center/cc-box sudo dnf install -y pulseaudio-libs-devel
~/control-center/cc-box bash -c 'cd ~/control-center &&
  mapfile -t flags < <(sed -n "/^const FREERDP_FLAGS/,/^];/p" crates/cc-home/src/install.rs | grep -oE "\"-D[^\"]+\"" | tr -d "\"") &&
  cd panels/third_party/FreeRDP &&
  cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$HOME/control-center/panels/third_party/prefix "${flags[@]}" &&
  ninja -C build &&
  rm -f ../prefix/lib64/lib{freerdp3,freerdp-client3,winpr3}.so* &&
  ninja -C build install && touch ../prefix/with-pulse &&
  cd ~/control-center && cargo build --release -p cc-panels'
```

The old libraries are unlinked before the new ones go in, so the running cc-panels keeps using the ones
it has mapped until it restarts. `readelf -d panels/third_party/prefix/lib64/libfreerdp-client3.so`
should list `libpulse.so.0` as NEEDED, and `readelf -d target/release/cc-panels | grep RUNPATH` should
still show the SteamVR path and the prefix. `./install.sh` does the same rebuild by itself, because
`install.rs` sees there's no `with-pulse` file, but it also re-registers the pointer driver, so use the
commands above on a running Desktop. On a native SteamOS build (`packaging/sysext/build-native.sh`) the
flags come from the same place and nothing else is needed.

## Testing

- `cargo test -p cc-panels` has the unit tests for `audio=no` and for the one-session-per-machine
  choice.
- `krdp/test/audio-loopback.sh` runs krdp's real `Audio.cpp` against a real FreeRDP client, with a
  private PipeWire (no hardware, its own runtime dir). It plays a 440 Hz tone on the "host's" output
  and checks it comes out of the "Frame's" speakers, then plays 880 Hz into the "Frame's" microphone
  and checks it comes out of krdp's source.
- Only live: the real Frame speakers and microphone, a real host's default sink, and krdp inside a
  real KDE session..
