#!/usr/bin/env bash
# One agent working through others: a Claude Code session named `plan` is
# asked to open three sessions in Splitlane and hand each a task. It does so
# with the `splitlane` commands the fleet skill teaches it; the sessions
# appear under it in the rail and in the empty panes, each header saying
# `opened by plan`, and the one told to run `npm test` stops on Claude
# Code's permission prompt - the "waiting for you" the clip ends on.
#
#   scripts/record-demo/record-fleet.sh [--rehearse|--capture]
#   DEMO_THEME=light scripts/record-demo/record-fleet.sh
#
# Then scripts/record-demo/process.sh <recording.mov> <run-dir>, which
# writes fleet.mp4 and a still, fleet-<theme>.png.
#
# Needs and handles everything record.sh does (see its header); Codex is not
# used. The window is 1600x824 points, the site's wide frame, so the display
# must be at least that large. Three, not four: a layout holds at most four
# panes (`layout::MAX_PANES`), and the lead takes one. What the lead writes is up to the lead: the
# script waits on states (sessions open, one waiting), never on text, and
# gives up with a message when a state does not come. Four Claude Code
# sessions run at once, which costs more of the plan's usage than the main
# scene.

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
demo_init fleet "${1:-}"

LEAD_SESSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["projects"][0]["surfaces"][0]["agent"]["session_id"])' "$SESSION_SEED")"

# The agent sessions of the active project, as a count: the rail's own list,
# so a shell never counts and a session opened into the rail (no pane left)
# does.
session_count() {
  rpc rail.list 2>/dev/null | python3 -c '
import json, sys
print(sum(1 for s in json.load(sys.stdin)["projects"][0]["sessions"] if s.get("agent")))
' 2>/dev/null || echo 0
}

# What the lead's pane shows, kept for the review when a state does not come.
keep_lead_screen() {
  rpc surface.read "{\"surface_id\": ${SURFACES[0]}, \"lines\": 120}" >"$RUN_DIR/lead-screen.json" 2>/dev/null || true
}

demo_start_recording
demo_launch
mark app-ready
sleep 2

say "the lead alone; what it opens takes panes beside it"
# No layout is laid out in advance: an empty pane is opened as a shell, and
# `splitlane add` adds a pane per session up to the ceiling of four
# (`layout::MAX_PANES`), so the lead and three sessions fill the window.
wait_surfaces 1 30 || die "the lead surface never came up"
cli wait --match "${SURFACES[0]}" --idle --for 2000 --timeout 60 >/dev/null || die "the lead never settled"
mark agents-ready
sleep 1

cli send "${SURFACES[0]}" --submit \
  "Open three Claude Code sessions in Splitlane in this project and give each its task. tests: run npm test and report which test fails and why, changing no files. api-docs: write docs/API.md describing what each module in src/ exports, reading files with your file tools and running no commands. lint: read src/ with your file tools and list inconsistencies in naming and style, running no commands and changing no files. Pass each task on in these words. Open all three first, then wait for them, and finish with a two-line summary. Use only splitlane commands, each one on its own, with nothing chained to it." >/dev/null
mark prompt-sent

say "waiting for the lead to open its sessions"
deadline=$(( $(date +%s) + 240 ))
opened=0
while (( $(date +%s) < deadline )); do
  count="$(session_count)"
  if (( count >= 2 && opened == 0 )); then mark first-opened; opened=1; fi
  (( count >= 4 )) && break
  sleep 1
done
(( count >= 4 )) || { keep_lead_screen; die "the lead opened $((count - 1)) of three sessions in 240 s (see lead-screen.json, or app.log)"; }
mark all-opened
rpc surface.list >"$RUN_DIR/surfaces.json"

say "waiting for an opened session to ask for permission"
wait_claude_status waiting 1 300 "!$LEAD_SESSION" || { keep_lead_screen; die "no opened session reached 'waiting' in 300 s (see lead-screen.json)"; }
mark waiting
sleep 4   # the rail's pass (2 s) plus the announce movement (480 ms)
mark fleet-hold
sleep 6
# Lead blocked on a prompt of its own (it ran something besides splitlane)
# is a different story from the one the clip tells; say so for the review.
wait_claude_status waiting 1 1 "$LEAD_SESSION" && say "note: the lead itself is waiting for permission - check the frames"
mark end
sleep 2

demo_finish
