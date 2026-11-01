#!/usr/bin/env python3
"""Small-text legibility check (docs/rdp-text.md). Two jobs:

  pattern.py make OUT.png             draws the test picture: small text, coloured text, 1 px lines
  pattern.py score REF.png A.png ...  how far each decoded picture is from the reference

The score is the mean error per channel over the pixels near an edge (text and lines), where
chroma subsampling shows, and the PSNR of the whole picture. Lower error is crisper text.
"""
import sys
from PIL import Image, ImageDraw, ImageFont, ImageFilter, ImageChops

W, H = 960, 540
FONT = "/usr/local/share/fonts/noto/NotoSans-Regular.ttf"


def make(out):
    img = Image.new("RGB", (W, H), (255, 255, 255))
    d = ImageDraw.Draw(img)
    y = 8
    # black on white, then the colours that smear most under 4:2:0
    pairs = [((0, 0, 0), (255, 255, 255)), ((220, 0, 0), (255, 255, 255)), ((0, 0, 220), (255, 255, 255)),
             ((220, 0, 0), (0, 0, 160)), ((0, 160, 0), (200, 0, 200)), ((255, 255, 255), (36, 39, 45))]
    for fg, bg in pairs:
        d.rectangle([0, y, W, y + 64], fill=bg)
        for i, size in enumerate((9, 11, 13)):
            f = ImageFont.truetype(FONT, size)
            d.text((8, y + 2 + i * 20), f"{size}px The quick brown fox 0123456789 ilI1| {{}}[]();:", font=f, fill=fg)
        y += 68
    # 1 px lines, alternating colours, and a checker of single pixels
    for x in range(0, W, 2):
        d.line([x, y, x, y + 40], fill=(255, 0, 0) if x % 4 else (0, 0, 255))
    y += 44
    for yy in range(y, min(H, y + 40)):
        for x in range(W):
            if (x + yy) % 2:
                img.putpixel((x, yy), (0, 0, 0))
    img.save(out)


def score(ref, others):
    r = Image.open(ref).convert("RGB")
    # where the edges are: text and lines, grown by a pixel
    edges = r.convert("L").filter(ImageFilter.FIND_EDGES).point(lambda v: 255 if v > 24 else 0).filter(ImageFilter.MaxFilter(3))
    mask = list((edges.get_flattened_data() if hasattr(edges, "get_flattened_data") else edges.getdata()))
    n = sum(1 for m in mask if m)
    print(f"{'picture':<28} {'edge err R':>10} {'G':>6} {'B':>6} {'PSNR dB':>8}")
    for path in others:
        o = Image.open(path).convert("RGB").resize(r.size)
        diff = ImageChops.difference(r, o)
        sums = [0, 0, 0]
        sq = 0
        for (dr, dg, db), m in zip((diff.get_flattened_data() if hasattr(diff, "get_flattened_data") else diff.getdata()), mask):
            sq += dr * dr + dg * dg + db * db
            if m:
                sums[0] += dr
                sums[1] += dg
                sums[2] += db
        import math
        mse = sq / (3 * r.size[0] * r.size[1])
        psnr = 99.0 if mse == 0 else 10 * math.log10(255 * 255 / mse)
        print(f"{path.rsplit('/', 1)[-1]:<28} {sums[0] / n:>10.2f} {sums[1] / n:>6.2f} {sums[2] / n:>6.2f} {psnr:>8.2f}")


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "make":
        make(sys.argv[2])
    elif len(sys.argv) >= 4 and sys.argv[1] == "score":
        score(sys.argv[2], sys.argv[3:])
    else:
        sys.exit(__doc__)
