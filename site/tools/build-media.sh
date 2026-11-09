#!/usr/bin/env bash
# Regenerates the site's images, icons and fonts in site/public from the
# repository's own sources (assets/images, assets/icons, src-app/assets/fonts).
# The output is committed, so a deploy needs none of these tools; run this
# only when a source changes.
#
# Needs: cwebp, ffmpeg, magick (ImageMagick), pyftsubset (fonttools with
# brotli).
#
#   site/tools/build-media.sh
#       images, icons and fonts
#   site/tools/build-media.sh --video <source.mp4> <name> [poster-second]
#       one product video: public/video/<name>.mp4 and <name>-poster.webp
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
site="$(dirname "$here")"
repo="$(dirname "$site")"
out="$site/public"
img="$out/img"

# Videos: H.264 for every browser, no audio, and a poster taken at the given
# second (1 by default). Sources are recordings kept outside the repository.
if [[ "${1:-}" == "--video" ]]; then
  src="$2" name="$3" at="${4:-1}"
  mkdir -p "$out/video"
  ffmpeg -v error -y -i "$src" -an -vf "scale=1280:-2" -c:v libx264 -profile:v high \
    -pix_fmt yuv420p -crf 30 -preset slow -tune stillimage -movflags +faststart "$out/video/$name.mp4"
  poster="$(mktemp -t splitlane-poster).png"
  ffmpeg -v error -y -ss "$at" -i "$src" -frames:v 1 -vf scale=1280:-2 "$poster"
  cwebp -quiet -q 80 "$poster" -o "$out/video/$name-poster.webp"
  rm -f "$poster"
  ls -la "$out/video/$name.mp4" "$out/video/$name-poster.webp"
  exit 0
fi

mkdir -p "$img" "$out/fonts"

# Screenshots: one WebP per width, the widest no larger than the source.
webp() { # source name width...
  local src="$1" name="$2"; shift 2
  for w in "$@"; do
    cwebp -quiet -q 80 -m 6 -resize "$w" 0 "$src" -o "$img/$name-$w.webp"
  done
}
for theme in light dark; do
  webp "$repo/assets/images/hero-$theme.png" "hero-$theme" 960 1440 1920
  webp "$repo/assets/images/grid-diff-$theme.png" "grid-diff-$theme" 960 1440 1920
  webp "$repo/assets/images/phone-list-$theme.png" "phone-list-$theme" 300 590
  webp "$repo/assets/images/phone-prompt-$theme.png" "phone-prompt-$theme" 300 590
done
webp "$repo/assets/images/orchestration-dark.png" orchestration-dark 960 1440 1920

# A detail of the hero for the section on agent state: the title bar's count
# and the pane that is waiting for you, at the source's own pixels.
for theme in light dark; do
  magick "$repo/assets/images/hero-$theme.png" -crop 960x667+960+0 +repage png:- |
    cwebp -quiet -q 82 -m 6 -o "$img/state-$theme-960.webp" -- -
done

# Social preview, linked from every page's og:image by absolute URL.
cp "$repo/assets/images/social-preview.png" "$out/social-preview.png"

# Icons.
magick "$repo/assets/icons/splitlane-16.png" "$repo/assets/icons/splitlane-32.png" \
  "$repo/assets/icons/splitlane-48.png" "$out/favicon.ico"
cp "$repo/assets/icons/splitlane-32.png" "$out/favicon-32.png"
magick "$repo/assets/icons/splitlane-256.png" -resize 180x180 "$out/apple-touch-icon.png"
cp "$repo/assets/icons/splitlane-64.png" "$img/logo-64.png"

# Fonts: the app's own Geist, cut to Latin, punctuation and the key symbols
# the page shows (arrows, Cmd, Option, Shift, Tab).
ranges="U+0020-007E,U+00A0-00FF,U+2010-2027,U+2030-203A,U+2190-2193,U+21E5,U+21E7,U+2318,U+2325"
font() { # source name
  pyftsubset "$repo/src-app/assets/fonts/$1" --flavor=woff2 --layout-features='*' \
    --unicodes="$ranges" --output-file="$out/fonts/$2.woff2" 2>/dev/null
}
font Geist-Regular.ttf geist-400
font Geist-Medium.ttf geist-500
font Geist-SemiBold.ttf geist-600
font GeistMono-Regular.ttf geist-mono-400
cp "$repo/src-app/assets/fonts/Geist-OFL.txt" "$out/fonts/OFL.txt"

du -sh "$img" "$out/fonts"
