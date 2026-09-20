#!/usr/bin/env bash
# Cut a full-screen recording down to the README assets, using the timeline
# and window geometry `record.sh` wrote.
#
#   scripts/record-demo/process.sh <recording.mov> <run-dir> [offset-seconds]
#
# Writes into <run-dir>/assets, <theme> being the run's DEMO_THEME:
#   hero-<theme>.png  grid-diff-<theme>.png   stills, 1920 px wide
# and, for the dark run only:
#   social-preview.png                        1280x640, from the hero
#   demo.mp4                                  the clip for a README <video>
#   demo.gif                                  fallback, 960 px, 12 fps
#
# Sync: the recording's own creation timestamp (whole seconds) is matched
# against the timeline's wall clock. Every still is taken in the middle of a
# hold of four seconds or more, so that precision is enough; if a still lands on
# the wrong moment, pass an offset (positive = the recording started later than
# its timestamp says).

set -euo pipefail

MOV="${1:?usage: process.sh <recording.mov> <run-dir> [offset-seconds]}"
RUN_DIR="${2:?usage: process.sh <recording.mov> <run-dir> [offset-seconds]}"
OFFSET="${3:-0}"
OUT="$RUN_DIR/assets"
mkdir -p "$OUT"

command -v ffmpeg >/dev/null || { echo "ffmpeg required" >&2; exit 1; }
command -v magick >/dev/null || { echo "magick required" >&2; exit 1; }

# Everything numeric is resolved in one place and handed back as shell vars.
eval "$(python3 "$(dirname "${BASH_SOURCE[0]}")/sync.py" "$MOV" "$RUN_DIR" "$OFFSET")"

still() {
  ffmpeg -v error -y -ss "$2" -i "$MOV" -frames:v 1 \
    -vf "crop=$CROP,scale=1920:-2:flags=lanczos" "$OUT/$1"
  # The window's corners are rounded; the rectangle cut from the screen keeps
  # whatever desktop sits behind them. Clear them, so the frame sits cleanly
  # on either page theme.
  local size
  size="$(magick identify -format '%wx%h' "$OUT/$1")"
  magick "$OUT/$1" -alpha set \( -size "$size" xc:black -fill white \
    -draw "roundrectangle 1,1,$(( ${size%x*} - 2 )),$(( ${size#*x} - 2 )),22,22" \) \
    -alpha off -compose CopyOpacity -composite "PNG32:$OUT/$1"
}
still "hero-$THEME.png" "$S_HERO_DARK"
still "grid-diff-$THEME.png" "$S_GRID"

if [[ "$THEME" != "dark" ]]; then
  echo "assets in $OUT:"
  ls -l "$OUT" | awk 'NR>1 {printf "  %-24s %6.2f MiB\n", $9, $5/1048576}'
  exit 0
fi

# Social preview, 1280x640: a link card is seen small, so it carries the name
# and one sentence in type large enough to read there, beside the hero. The
# face is the app's own UI font; the ground is the rail's colour in the frame.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FONTS="$ROOT/src-app/assets/fonts"
GROUND="$(magick "$OUT/hero-$THEME.png" -format '%[pixel:p{10,600}]' info:)"
magick -size 1280x640 "xc:$GROUND" \
  \( "$OUT/hero-$THEME.png" -resize 764x \) -geometry +486+81 -composite \
  \( "$ROOT/assets/icons/splitlane-128.png" -resize 72x72 \) -geometry +56+150 -composite \
  -fill '#F2F3F5' -font "$FONTS/Geist-SemiBold.ttf" -pointsize 62 -annotate +52+300 'Splitlane' \
  -fill '#A9AEB6' -font "$FONTS/Geist-Regular.ttf" -pointsize 29 -interline-spacing 8 \
  -annotate +56+360 $'Coding agents side by side,\nand a rail that tells you\nwhich one is waiting for you.' \
  "$OUT/social-preview.png"

# The clip: from the moment the agents are ready to the grid, with the prompts going out played at
# 2x and the wait for the agents to finish at 4x.
ffmpeg -v error -y -i "$MOV" -filter_complex "
  [0:v]crop=$CROP,split=4[a][b][c][d];
  [a]trim=$T_AGENTS_READY:$T_WAITING,setpts=(PTS-STARTPTS)/2[s1];
  [b]trim=$T_WAITING:$T_ANSWERED,setpts=PTS-STARTPTS[s2];
  [c]trim=$T_ANSWERED:$T_ALL_IDLE,setpts=(PTS-STARTPTS)/4[s3];
  [d]trim=$T_ALL_IDLE:$T_CLIP_END,setpts=PTS-STARTPTS[s4];
  [s1][s2][s3][s4]concat=n=4:v=1:a=0,fps=30,scale=1920:-2:flags=lanczos,format=yuv420p[v]" \
  -map "[v]" -an -c:v libx264 -preset slow -crf 24 -movflags +faststart "$OUT/demo.mp4"

ffmpeg -v error -y -i "$OUT/demo.mp4" -filter_complex \
  "fps=12,scale=960:-2:flags=lanczos,split[x][y];[x]palettegen=stats_mode=diff[p];[y][p]paletteuse=dither=sierra2_4a" \
  "$OUT/demo.gif"

echo "assets in $OUT:"
ls -l "$OUT" | awk 'NR>1 {printf "  %-24s %6.2f MiB\n", $9, $5/1048576}'
