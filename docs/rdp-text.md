# Sharp text over RDP

Small and coloured text on a computer's panel smears: red on white goes pink at the edges, and thin
coloured lines blur. This is what's causing it, what I changed, and what else could help.

## The cause: H.264 at 4:2:0

krdp sends H.264 at 4:2:0, which keeps colour at a quarter of the resolution (half each way).
Black-on-white text survives that, since it's nearly all brightness. Coloured text, coloured
lines and the edges of anti-aliased text don't, because their detail is in the colour.

What I measured, with `tools/text-legibility/run.sh` (in the dev container). It draws small text
at 9, 11 and 13 px in six colour pairs, 1 px coloured lines and a pixel checker, runs it through
each codec, and scores the error near edges (0 to 255, lower is sharper) and the PSNR of the
whole picture:

| Codec | Edge error R | G | B | PSNR |
| --- | --- | --- | --- | --- |
| H.264 4:2:0, CRF 18 (near lossless) | 13.5 | 10.6 | 19.2 | 24.0 dB |
| H.264 4:2:0, CRF 28 | 14.8 | 11.5 | 20.4 | 23.8 dB |
| H.264 4:4:4, CRF 18 | 2.1 | 1.2 | 1.9 | 43.0 dB |
| H.264 4:4:4, CRF 28 | 4.7 | 3.9 | 4.7 | 35.0 dB |
| RemoteFX Progressive (FreeRDP's own encoder) | 1.6 | 0.9 | 1.6 | 43.5 dB |

The quality setting barely moves 4:2:0 (13.5 against 14.8), because the loss is the subsampling,
not the bitrate. So turning krdp's quality up doesn't fix it. Full colour (4:4:4) cuts the edge
error about six times. H.264 here is x264 standing in for krdp's encoder, so treat its numbers as
the shape of the problem, not krdp's exact output. The Progressive row is FreeRDP's real codec,
the same one krdp calls.

## What krdp offers

I read krdp's source for 6.7.5 (what we patch) and 6.8.0 (what the desktop host runs now):

- **No AVC444.** Both set `GfxAVC444` and `GfxAVC444v2` off on the server (`RdpConnection.cpp`),
  and the encoder is `H264Baseline` through KPipeWire, which is 4:2:0 only. So turning on AVC444 on
  our side changes nothing with krdp.
- **RemoteFX Progressive is the other mode.** krdp picks it (`VideoStream::onCapsAdvertise`) when
  the client doesn't offer H.264, or with `KRDP_DISABLE_H264=1` on the server. It's full colour,
  and it's what the table's last row measures.
- **Quality:** krdp seeds its adaptive quality from the client's connection type, 95 for LAN,
  which we already send. The cap is the server's own `--quality` setting. The client has no other
  knob.

## What I changed

- **AVC444 and AVC444v2 are on** (`rdp.rs`) with H.264, so a server that can send 4:4:4 does. FreeRDP
  3.31 decodes both, and our `h264.c` patch only changes the YUV converter's threading, which the
  4:4:4 path shares. With krdp nothing changes but the capability set it confirms: offering AVC444
  makes our client send the version 10 sets too, so krdp confirms the highest of those (10.7, unless FreeRDP filters it) instead of 8.1. That needs a live check.
- **Each panel logs what it got:** "graphics caps 0x…, flags 0x…" when the server confirms, and
  "the server sends H.264 4:2:0 (AVC420)" (or whichever) on the first frame and on any change.
- **`"rdp_codec": "progressive"`** in `~/.config/control-center/settings.json` asks for RemoteFX
  Progressive instead of H.264, for every computer, from the next connect. It's there to try
  side by side; it isn't the default.

## What Progressive costs

- **Bandwidth:** the test picture's first frame is 135 KB, against about 65 KB for 30 frames of
  H.264 at CRF 18. After that, krdp only sends the damaged tiles, so static text is cheap and
  scrolling or video is expensive.
- **CPU on the computer:** krdp's Progressive path encodes from a raw frame in software, with no
  hardware encoder. That's fine for a document and heavy for a 5120x1440 screen playing video.
- **CPU on the Frame:** FreeRDP decodes it in software, as it does H.264 now.

So it suits a screen of text, and H.264 suits video. A per-panel choice would be the useful form
of it, once it's been seen live.

## The other levers

- **The overlay's sampling.** SteamVR samples a panel's texture with plain filtering, and I haven't
  found an overlay flag or texture setting that makes it sharper. A texture much bigger than the
  panel's pixels on screen would need mipmaps to avoid shimmer, and I don't know whether SteamVR
  uses the mip levels of a texture an app submits. Not checked.
- **Pixel density.** A window panel is 1400 px a metre by default (`window_px_per_m`), so a pixel is
  about 0.7 mm, about 0.04° at a metre: roughly 24 pixels a degree. If the Frame's display is
  around 20 pixels a degree in the middle (my estimate, not measured), 9 px text is about as small
  as the headset can show, whatever the codec. A computer's screen panel is sized by its monitor,
  so its density depends on how big you make it.

## Still to do

- On the desktop host (Crab): check the log says AVC420 with krdp 6.8, that nothing else changes
  with the version 10 capability sets, then the same small-text pattern on screen with
  `"rdp_codec": "progressive"` and without, side by side in the headset.
- Decide whether Progressive becomes a per-panel choice.
