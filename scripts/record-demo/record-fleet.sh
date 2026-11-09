#!/usr/bin/env bash
# One agent working through others: a Claude Code session named `plan` is
# asked to open four sessions in Splitlane and hand each a task. It does so
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
# must be at least that large. What the lead writes is up to the lead: the
# script waits on states (sessions open, one waiting), never on text, and
# gives up with a message when a state does not come. Five Claude Code
# sessions run at once, which costs more of the plan's usage than the main
# scene.

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
demo_init fleet "${1:-}"

LEAD_SESSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["projects"][0]["surfaces"][0]["agent"]["session_id"])' "$SESSION_SEED")"

# The surfaces in a pane of the active project, as a count. Not
# `pane_count`: the layout already has five panes, four of them empty.
surface_count() {
  rpc surface.list 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin)
print(sum(1 for s in d["surfaces"] if s.get("workspace") == d["workspace"] and s.get("scope") == "workspace"))
' 2>/dev/null || echo 0
}

demo_start_recording
demo_launch
mark app-ready
sleep 2

say "the lead on the left, four empty panes beside it"
# A third of the width for the lead, a two-by-two grid for what it opens.
# The opened sessions take the empty panes, in order.
rpc workspace.restore_layout '{"layout":{"type":"split","direction":"vertical","ratios":[0.34,0.66],"children":[{"type":"pane","surfaces":[]},{"type":"split","direction":"horizontal","children":[{"type":"split","direction":"vertical","children":[{"type":"pane","surfaces":[]},{"type":"pane","surfaces":[]}]},{"type":"split","direction":"vertical","children":[{"type":"pane","surfaces":[]},{"type":"pane","surfaces":[]}]}]}]}}' >/dev/null
wait_surfaces 1 30 || die "the lead surface never came up"
cli wait --match "${SURFACES[0]}" --idle --for 2000 --timeout 60 >/dev/null || die "the lead never settled"
mark agents-ready
sleep 1

cli send "${SURFACES[0]}" --submit \
  "Open four Claude Code sessions in Splitlane in this project and give each its task. tests: run npm test and report which test fails and why, changing no files. api-docs: write docs/API.md describing what each module in src/ exports. lint: read src/ and list inconsistencies in naming and style, changing no files. fixtures: add test/fixtures/bad-amount.txt, a journal whose second line has an amount that is not a number. Open all four first, then wait for them, and finish with a two-line summary. Use only splitlane commands." >/dev/null
mark prompt-sent

say "waiting for the lead to open its sessions"
deadline=$(( $(date +%s) + 240 ))
opened=0
while (( $(date +%s) < deadline )); do
  count="$(surface_count)"
  if (( count >= 2 && opened == 0 )); then mark first-opened; opened=1; fi
  (( count >= 5 )) && break
  sleep 1
done
(( count >= 5 )) || die "the lead opened $((count - 1)) of four sessions in 240 s (see the lead's pane, or app.log)"
mark all-opened
rpc surface.list >"$RUN_DIR/surfaces.json"

say "waiting for an opened session to ask for permission"
wait_claude_status waiting 1 300 "!$LEAD_SESSION" || die "no opened session reached 'waiting' in 300 s"
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
