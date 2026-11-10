#!/usr/bin/env bash
# A blog post's social preview: 1280x640 PNG in the style of
# public/social-preview.png (dark), with the post's title on the left and a
# crop of a real frame from the app on the right.
#
#   tools/post-preview.sh <slug> "<title>" [frame] [crop]
#
# frame defaults to public/video/hero-dark-poster.webp, crop (WxH+X+Y in the
# frame's pixels) to its right-hand pane, the one waiting for you. Writes
# public/blog/<slug>/preview.png. Needs ImageMagick 7.
set -euo pipefail

site="$(cd "$(dirname "$0")/.." && pwd)"
fonts="$site/../src-app/assets/fonts"
slug="$1"
title="$2"
frame="${3:-$site/public/video/hero-dark-poster.webp}"
crop="${4:-700x720+580+0}"
out="$site/public/blog/$slug/preview.png"

bg="#16181b"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The frame: cropped, scaled to 620 wide, rounded at the top, edged.
magick "$frame" -crop "$crop" +repage -resize 620x \
  \( +clone -alpha extract -fill black -colorize 100 -fill white \
     -draw "roundrectangle 0,0 %[fx:w-1],%[fx:h+20] 14,14" \) \
  -alpha off -compose CopyOpacity -composite "$tmp/frame.png"
magick "$tmp/frame.png" \
  \( +clone -alpha extract -morphology EdgeIn Diamond:1 -fill "#3a3c42" -opaque white -transparent black \) \
  -compose Over -composite "$tmp/frame.png"

# The title: as large as fits a 520x330 box, Geist SemiBold.
magick -background none -fill "#f2f3f5" -font "$fonts/Geist-SemiBold.ttf" \
  -interline-spacing -6 -size 520x330 -gravity west caption:"$title" "$tmp/title.png"

magick -size 1280x640 "xc:$bg" \
  \( "$site/public/img/logo-64.png" -resize 40x40 \) -geometry +64+76 -composite \
  -font "$fonts/Geist-SemiBold.ttf" -pointsize 26 -fill "#f2f3f5" -annotate +118+106 "Splitlane" \
  "$tmp/title.png" -geometry +64+150 -composite \
  -font "$fonts/Geist-Regular.ttf" -pointsize 22 -fill "#9a9ca4" -annotate +64+560 "splitlane.dev/blog" \
  "$tmp/frame.png" -geometry +620+64 -composite \
  -alpha off -depth 8 -strip "$out"

echo "wrote ${out#"$site/"}"
