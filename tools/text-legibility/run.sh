#!/usr/bin/env bash
# Small text through each codec krdp could send, scored against the original (docs/rdp-text.md).
#   tools/text-legibility/run.sh [out-dir]     in the dev container (ffmpeg, Python with PIL, gcc)
# H.264 4:2:0 is what krdp sends today. 4:4:4 is what AVC444 would give, which krdp doesn't
# offer. Progressive is krdp's other mode, RemoteFX, sent when the client doesn't ask for H.264.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$HOME/.cache/agent-scratch/text-legibility}
prefix=${FREERDP_PREFIX:-$here/../../panels/third_party/prefix}
mkdir -p "$out"
cd "$out"
python3 "$here/pattern.py" make ref.png
read -r w h < <(python3 -c 'from PIL import Image; print(*Image.open("ref.png").size)')

# H.264 at a near-lossless and a busier quality, 4:2:0 then 4:4:4, 30 frames so it settles
for crf in 18 28; do
  for pf in yuv420p yuv444p; do
    ffmpeg -v error -y -loop 1 -t 1 -r 30 -i ref.png -c:v libx264 -preset veryfast -tune zerolatency -crf $crf -pix_fmt $pf h264-$pf-crf$crf.mkv
    ffmpeg -v error -y -sseof -0.04 -i h264-$pf-crf$crf.mkv -frames:v 1 h264-$pf-crf$crf.png
    printf '%s: %s bytes a second\n' "h264-$pf-crf$crf" "$(stat -c %s h264-$pf-crf$crf.mkv)"
  done
done

# RemoteFX Progressive, one frame, through FreeRDP's own encoder and decoder
gcc -O2 -o rfx "$here/rfx.c" -I"$prefix/include/freerdp3" -I"$prefix/include/winpr3" -L"$prefix/lib64" -lfreerdp3 -lwinpr3
ffmpeg -v error -y -i ref.png -f rawvideo -pix_fmt bgr0 ref.bgrx
LD_LIBRARY_PATH=$prefix/lib64 ./rfx ref.bgrx progressive.bgrx "$w" "$h"
ffmpeg -v error -y -f rawvideo -pix_fmt bgr0 -s "${w}x$h" -i progressive.bgrx progressive.png

python3 "$here/pattern.py" score ref.png h264-yuv420p-crf18.png h264-yuv444p-crf18.png h264-yuv420p-crf28.png h264-yuv444p-crf28.png progressive.png
echo "pictures in $out"
