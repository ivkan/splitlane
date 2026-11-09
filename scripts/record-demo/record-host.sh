#!/usr/bin/env bash
# Sessions outlive the window: three agents at work, the window closes, and
# Splitlane started again finds every session in its pane, still working.
# The stage turns the session host on and sets `on_quit` to `keep`, so the
# app's own Close Window action ends the window's process and leaves the
# agents running in the host - nothing is killed or faked to get there.
#
#   scripts/record-demo/record-host.sh [--rehearse|--capture]
#
# Then scripts/record-demo/process.sh <recording.mov> <run-dir>, which
# writes host.mp4. While no window runs the screen shows whatever is behind
# it; the cut paints that stretch in the window's own ground colour.
#
# Needs and handles everything record.sh does (see its header), Codex in the
# middle pane included. The tasks are edits that take a minute or so, long
# enough to outlast the close and the start; if every agent had finished by
# the time the window is back, the script says so and the take is worth
# repeating. On the way out lib.sh stops this stage's session host, and with
# it the agents; a host of an installed Splitlane is never touched.

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
demo_init host "${1:-}"
CLAUDE_PANES=$(( DEMO_CODEX == 1 ? 2 : 3 ))

demo_start_recording
demo_launch
mark app-ready
sleep 2

wait_surfaces 3 30 || die "expected three surfaces, got ${#SURFACES[@]}"
for sid in "${SURFACES[@]}"; do
  cli wait --match "$sid" --idle --for 2000 --timeout 60 >/dev/null || die "surface $sid never settled"
done
mark agents-ready
sleep 1

cli send "${SURFACES[0]}" --submit \
  "Add a --since <date> option to \`ledger export\` that keeps only entries on or after that date, and document it in README.md. Edit the code only; do not run any commands." >/dev/null
sleep 1
if [[ "$DEMO_CODEX" == 1 ]]; then
  cli send "${SURFACES[1]}" --submit \
    "Add tests for csvField to test/format.test.js covering commas, quotes and line breaks. Do not run the tests." >/dev/null
else
  cli send "${SURFACES[1]}" --submit \
    "Add tests for csvField to test/format.test.js covering commas, quotes and line breaks. Do not run any commands." >/dev/null
fi
sleep 1
cli send "${SURFACES[2]}" --submit \
  "Write docs/FORMAT.md describing the journal format src/parse.js accepts, with three examples. Do not run any commands." >/dev/null
mark prompts-sent

say "waiting until the agents are at work"
wait_claude_status busy "$CLAUDE_PANES" 60 || say "not every Claude Code session reported busy; closing anyway"
sleep 4
mark close

say "closing the window with the app's own action"
FIRST_PID="$APP_PID"
# The window's process ends while answering, so the reply may never come.
rpc app.dispatch_action '{"action": "close_window"}' >/dev/null 2>&1 || true
for _ in $(seq 1 40); do kill -0 "$FIRST_PID" 2>/dev/null || break; sleep 0.5; done
kill -0 "$FIRST_PID" 2>/dev/null \
  && die "the window's process did not end - was the window only hidden? (session_host and on_quit in the stage's splitlane.json)"
APP_PID=""
mark closed

# The record of what ran while no window did: the CLI answers from the
# session host then, and says the window is closed.
cli ps --json >"$RUN_DIR/ps-window-closed.json" 2>"$RUN_DIR/ps-window-closed.err" || true
[[ -n "$(stage_host_pids "$STAGE")" ]] || die "no session host is running for the stage - the sessions ended with the window"
sleep 3

mark reopen
demo_launch
mark reopened
wait_surfaces 3 30 || die "the reopened window has ${#SURFACES[@]} of three sessions"
mark sessions-back
wait_claude_status busy 1 1 \
  || say "note: no Claude Code session was still working when the window came back - the take shows them idle; repeat it"
sleep 8
mark end
sleep 2

demo_finish
