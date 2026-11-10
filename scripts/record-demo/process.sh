#!/usr/bin/env bash
# Cut a full-screen recording down to the README and site assets, using the
# timeline and window geometry a record script wrote.
#
#   scripts/record-demo/process.sh <recording.mov> <run-dir> [offset-seconds]
#
# Writes into <run-dir>/assets, <theme> being the run's DEMO_THEME. Every run:
#   window.mp4        the window at the recording's full resolution, and
#   marks.json        the timeline in seconds into window.mp4 - the source
#                     footage and cue sheet for any other edit
# The main scene (record.sh):
#   hero-<theme>.png  grid-diff-<theme>.png   stills, 1920 px wide
#   hero.mp4          the site's hero: prompts go out, an agent stops on a
#                     question (the wait sped up), the rail says so
#   state.mp4         the same clip, its top right quarter: the title bar's
#                     count and the waiting pane, for "How it knows"
#   grid.mp4          the row becomes a grid with the diff
#   history-<theme>.png  history.mp4
#                     the pointer on the test run's row in the rail, and
#                     the tooltip listing what was written in that session
#   and, for the dark run only:
#   social-preview.png                        1280x640, from the hero
#   demo.mp4                                  the clip for a README <video>
#   demo.gif                                  fallback, 960 px, 12 fps
# The fleet scene (record-fleet.sh):
#   fleet-<theme>.png fleet.mp4               the lead opening three sessions
# The host scene (record-host.sh):
#   host.mp4          close, nothing, reopen; the stretch with no window is
#                     painted in the window's ground colour (gap.py finds it
#                     in the frames). HOST_FILL=0 leaves the desktop in.
#
# The site's clips are 1920 px wide at high quality; site/tools/build-media.sh
# --video encodes them for the page, and site/README.md says where they go.
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
# clip <name> <extra-filter> <start:end:speed>...
# One clip of the window, its pieces each played at their own speed and
# joined. <extra-filter> runs on the joined clip ("null" for nothing).
clip() {
  local name="$1" extra="$2"; shift 2
  local n=$#
  local i=0 graph="[0:v]crop=$CROP,split=$n" joins="" piece a b speed
  for i in $(seq 0 $(( n - 1 ))); do graph+="[p$i]"; done
  graph+=";"
  i=0
  for piece in "$@"; do
    IFS=: read -r a b speed <<<"$piece"
    graph+="[p$i]trim=$a:$b,setpts=(PTS-STARTPTS)/${speed}[s$i];"
    joins+="[s$i]"
    i=$(( i + 1 ))
  done
  graph+="${joins}concat=n=$n:v=1:a=0,fps=30,$extra,scale=1920:-2:flags=lanczos,format=yuv420p[v]"
  ffmpeg -v error -y -i "$MOV" -filter_complex "$graph" -map "[v]" -an \
    -c:v libx264 -preset slow -crf 18 -movflags +faststart "$OUT/$name"
}

list_assets() {
  echo "assets in $OUT:"
  # shellcheck disable=SC2012 # names are ours, and the sizes are the point
  ls -l "$OUT" | awk 'NR>1 {printf "  %-24s %6.2f MiB\n", $9, $5/1048576}'
}

ffmpeg -v error -y -i "$MOV" -vf "crop=$CROP,format=yuv420p" -an \
  -c:v libx264 -preset slow -crf 16 -movflags +faststart "$OUT/window.mp4"
printf '%s\n' "$MARKS_JSON" >"$OUT/marks.json"

if [[ "$SCENE" == fleet ]]; then
  still "fleet-$THEME.png" "$S_FLEET"
  # The lead reading its task at speed, the sessions opening at twice it,
  # their work up to the question at three times, then the question held.
  clip fleet.mp4 null \
    "$T_AGENTS_READY:$T_PROMPT_SENT:1" "$T_PROMPT_SENT:$T_ALL_OPENED:2" \
    "$T_ALL_OPENED:$T_WAITING:3" "$T_WAITING:$T_CLIP_END:1"
  list_assets
  exit 0
fi

if [[ "$SCENE" == host ]]; then
  fill=null
  if [[ "${HOST_FILL:-1}" == 1 ]]; then
    eval "$(python3 "$(dirname "${BASH_SOURCE[0]}")/gap.py" "$MOV" "$CROP" "$T_CLOSE" "$T_CLOSED" "$T_SESSIONS_BACK")"
    # Times in the joined clip: its first piece is the work at twice the
    # speed, so the gap sits after that piece's length.
    lead="$(python3 -c "import sys; print(f'{(float(sys.argv[2]) - float(sys.argv[1])) / 2:.3f}')" "$T_PROMPTS_SENT" "$T_CLOSE")"
    from="$(python3 -c "import sys; print(f'{float(sys.argv[1]) + float(sys.argv[2]) - float(sys.argv[3]):.3f}')" "$lead" "$GAP_START" "$T_CLOSE")"
    to="$(python3 -c "import sys; print(f'{float(sys.argv[1]) + float(sys.argv[2]) - float(sys.argv[3]):.3f}')" "$lead" "$GAP_END" "$T_CLOSE")"
    fill="drawbox=x=0:y=0:w=iw:h=ih:color=$GROUND:t=fill:enable='between(t,$from,$to)'"
    echo "no window from $GAP_START s to $GAP_END s; painted $GROUND"
  fi
  clip host.mp4 "$fill" "$T_PROMPTS_SENT:$T_CLOSE:2" "$T_CLOSE:$T_CLIP_END:1"
  list_assets
  exit 0
fi

still "hero-$THEME.png" "$S_HERO_DARK"
still "grid-diff-$THEME.png" "$S_GRID"
# The site's hero: the prompts going out, the agents at work at three times
# the speed, then the question held until focus moves away.
clip hero.mp4 null "$T_AGENTS_READY:$T_PROMPTS_SENT:1" "$T_PROMPTS_SENT:$T_WAITING:3" "$T_WAITING:$T_JUMP:1"
# Its top right quarter is the detail the still in "How it knows" shows.
ffmpeg -v error -y -i "$OUT/hero.mp4" -vf "crop=iw/2:ih/2:iw/2:0,format=yuv420p" -an \
  -c:v libx264 -preset slow -crf 18 -movflags +faststart "$OUT/state.mp4"
clip grid.mp4 null "$T_ANSWER_SENT:$T_FILES:1"
still "history-$THEME.png" "$S_HISTORY"
clip history.mp4 null "$T_HISTORY_FROM:$T_HISTORY_END:1"

if [[ "$THEME" != "dark" ]]; then
  list_assets
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

# The clip: from the moment the agents are ready to the end of the tour (the
# follow-ups and the hover after it have clips of their own), with the prompts going out played at
# 2x and the wait for the agents to finish at 4x.
ffmpeg -v error -y -i "$MOV" -filter_complex "
  [0:v]crop=$CROP,split=4[a][b][c][d];
  [a]trim=$T_AGENTS_READY:$T_WAITING,setpts=(PTS-STARTPTS)/2[s1];
  [b]trim=$T_WAITING:$T_ANSWERED,setpts=PTS-STARTPTS[s2];
  [c]trim=$T_ANSWERED:$T_ALL_IDLE,setpts=(PTS-STARTPTS)/4[s3];
  [d]trim=$T_ALL_IDLE:$T_TOUR_END,setpts=PTS-STARTPTS[s4];
  [s1][s2][s3][s4]concat=n=4:v=1:a=0,fps=30,scale=1920:-2:flags=lanczos,format=yuv420p[v]" \
  -map "[v]" -an -c:v libx264 -preset slow -crf 24 -movflags +faststart "$OUT/demo.mp4"

ffmpeg -v error -y -i "$OUT/demo.mp4" -filter_complex \
  "fps=12,scale=960:-2:flags=lanczos,split[x][y];[x]palettegen=stats_mode=diff[p];[y][p]paletteuse=dither=sierra2_4a" \
  "$OUT/demo.gif"

list_assets
