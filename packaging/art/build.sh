#!/usr/bin/env bash
# Makes the launcher tile's Steam library art from packaging/command-center.svg with rsvg-convert,
# into packaging/art/ (committed, so building the image or installing needs no renderer). The
# sizes are Valve's (the comments in /usr/share/applications/deckard-nested-desktop.desktop):
#   capsule.png        600x900    the tile
#   hero.png           3840x1240  the background behind the logo
#   logo.png           1280x720   the logo over the hero (transparent)
#   store-capsule.png  1280x720   the horizontal capsule
# Run it again after changing the logo. It uses Noto Sans Bold for the name (OFL, on SteamOS).
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
svg="$here/../command-center.svg"
command -v rsvg-convert >/dev/null || { echo "needs rsvg-convert (librsvg)" >&2; exit 1; }
# the logo's shapes, without its <svg> wrapper (a 32 x 32 box)
mark=$(sed -e 's/^.*<svg[^>]*>//' -e 's/<\/svg>.*$//' "$svg")
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT

bg() { # w h: the dark gradient and a faint violet and cyan glow, in Command Center's own colours
  cat <<SVG
<defs>
  <linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#0e141b"/><stop offset="1" stop-color="#1b2433"/></linearGradient>
  <radialGradient id="v" cx="0.8" cy="0.15" r="0.7"><stop offset="0" stop-color="#a48bff" stop-opacity="0.35"/><stop offset="1" stop-color="#a48bff" stop-opacity="0"/></radialGradient>
  <radialGradient id="c" cx="0.1" cy="0.95" r="0.7"><stop offset="0" stop-color="#7df9ff" stop-opacity="0.25"/><stop offset="1" stop-color="#7df9ff" stop-opacity="0"/></radialGradient>
</defs>
<rect width="$1" height="$2" fill="url(#g)"/><rect width="$1" height="$2" fill="url(#v)"/><rect width="$1" height="$2" fill="url(#c)"/>
SVG
}
logo() { # x y size: the mark, with a soft shadow under it
  printf '<ellipse cx="%s" cy="%s" rx="%s" ry="%s" fill="#000" opacity="0.35"/>' "$(echo "$1 + $3 / 2" | bc -l)" "$(echo "$2 + $3 * 1.04" | bc -l)" "$(echo "$3 * 0.42" | bc -l)" "$(echo "$3 * 0.05" | bc -l)"
  printf '<g transform="translate(%s %s) scale(%s)">%s</g>' "$1" "$2" "$(echo "$3 / 32" | bc -l)" "$mark"
}
name() { # x y size text: the name centred on x, white with a dark outline like Valve's tiles
  printf '<text x="%s" y="%s" font-family="Noto Sans" font-weight="700" font-size="%s" text-anchor="middle" fill="#fff" stroke="#0e141b" stroke-width="%s" paint-order="stroke" letter-spacing="2">%s</text>' "$1" "$2" "$3" "$(echo "$3 / 12" | bc -l)" "$4"
}
render() { # name w h: reads the SVG body on stdin
  { printf '<svg xmlns="http://www.w3.org/2000/svg" width="%s" height="%s" viewBox="0 0 %s %s">' "$2" "$3" "$2" "$3"; cat; echo '</svg>'; } >"$t/$1.svg"
  rsvg-convert -w "$2" -h "$3" "$t/$1.svg" -o "$here/$1.png"
}

{ bg 600 900; logo 130 130 340; name 300 650 100 COMMAND; name 300 765 100 CENTER; } | render capsule 600 900
{ bg 3840 1240; logo 2380 220 800; } | render hero 3840 1240
{ logo 480 70 320; name 640 560 96 "COMMAND CENTER"; } | render logo 1280 720
{ bg 1280 720; logo 150 170 380; name 880 380 62 "COMMAND CENTER"; } | render store-capsule 1280 720
echo "made $(ls "$here"/*.png | wc -l) images in $here"
