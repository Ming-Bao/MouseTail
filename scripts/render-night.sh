#!/bin/sh
# Renders website/art/night.svg (from make-night.py) into the images the website shows:
# night.webp for the big panels and night-small.webp for the cards and the laptop's screen.
# Drawing the SVG live is slow (its grain and shadow filters), so the site uses these instead.
#
#     scripts/render-night.sh
#
# Needs a Chromium-based browser (Chrome, Brave or Chromium; or set BROWSER), cwebp and sips.
set -eu
cd "$(dirname "$0")/../website/art"

browser="${BROWSER:-}"
if [ -z "$browser" ]; then
  for app in "Google Chrome" "Brave Browser" "Chromium"; do
    if [ -x "/Applications/$app.app/Contents/MacOS/$app" ]; then
      browser="/Applications/$app.app/Contents/MacOS/$app"
      break
    fi
  done
fi
[ -n "$browser" ] || { echo "no Chromium-based browser found; set BROWSER" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Twice the SVG's own 1600 × 1000, so the grain is drawn finely.
"$browser" --headless=new --hide-scrollbars --force-device-scale-factor=2 --window-size=1600,1000 \
  --screenshot="$tmp/night.png" "file://$PWD/night.svg" >/dev/null 2>&1

sips -Z 2400 "$tmp/night.png" --out "$tmp/2400.png" >/dev/null
sips -Z 1200 "$tmp/night.png" --out "$tmp/1200.png" >/dev/null
cwebp -quiet -q 90 -sharp_yuv -preset photo -sns 0 -f 0 "$tmp/2400.png" -o night.webp
cwebp -quiet -q 85 -sharp_yuv -preset photo "$tmp/1200.png" -o night-small.webp
ls -l night.webp night-small.webp
