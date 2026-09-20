#!/usr/bin/env bash
# Build (once) and run the eye-check helper.
#
# The binary lands at a stable cached path so it is built once rather than per
# call. It needs no special permission: every subcommand raises Splitlane and
# confirms it is frontmost before looking the window up, which is what makes the
# lookup reliable - an off-screen window is simply not enumerated.
#
#   scripts/eyecheck/eyecheck.sh window
#   scripts/eyecheck/eyecheck.sh click 64 440
#   scripts/eyecheck/eyecheck.sh dclick 640 111  # inline rename
#   scripts/eyecheck/eyecheck.sh rclick 64 440   # a context menu
#   scripts/eyecheck/eyecheck.sh type 'Add a file hello.txt'
#   scripts/eyecheck/eyecheck.sh enter
#
# A frame, by window id rather than by screen rect, so a moved or overlapped
# window still photographs correctly:
#
#   read -r id x y w h < <(scripts/eyecheck/eyecheck.sh window)
#   screencapture -x -o -l "$id" shot.png
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "eyecheck: macOS only - it drives CGEvent and the window server." >&2
  exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="$here/eyecheck.swift"
out="${SPLITLANE_EYECHECK_BIN:-$HOME/.cache/splitlane-eyecheck/eyecheck}"

mkdir -p "$(dirname "$out")"
if [[ ! -x "$out" || "$src" -nt "$out" ]]; then
  echo "eyecheck: building $out" >&2
  swiftc -O "$src" -o "$out"
fi

exec "$out" "$@"
