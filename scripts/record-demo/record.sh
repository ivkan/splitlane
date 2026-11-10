#!/usr/bin/env bash
# Run the main demo choreography against a clean debug build, and write a
# timeline the post-processing step cuts the screen recording by. It is the
# source of the README's stills and clip and of three of the site's clips:
# hero (an agent stops on a question), state (the same, as a detail) and
# grid (the row becomes a grid with the diff). Two more scenes have scripts
# of their own: record-fleet.sh (one agent opening four sessions) and
# record-host.sh (the window closes and opens again, the sessions still
# there). The shared parts are in lib.sh.
#
#   scripts/record-demo/record.sh              # you record the screen (Cmd-Shift-5)
#   scripts/record-demo/record.sh --rehearse   # same run, no recording prompt
#   scripts/record-demo/record.sh --capture    # records the screen itself (ffmpeg)
#   DEMO_THEME=light scripts/record-demo/record.sh   # the light variant
#
# Then:
#   scripts/record-demo/process.sh <recording.mov> <run-dir>
#
# What can be checked without a window, a sign-in or a screen recording -
# the stage of each scene, the cuts on a synthetic recording, the clean-up -
# is checked by:
#   python3 -I scripts/record-demo/test_pipeline.py
#
# macOS only. What it needs, and why:
#
# - A debug build (`cargo build -p splitlane-app`). The debug build keeps its
#   own config, session and socket (`splitlane-dev`), and this script also gives
#   it a throwaway HOME and TMPDIR, so nothing of yours appears on screen and
#   nothing the demo does lands in your projects, ~/.claude or ~/.codex.
# - Real Claude Code, signed in with the access token from your own Keychain
#   entry and never with its refresh token, so nothing inside the stage can
#   rotate your login. The token is never printed. The run is refused when it
#   has less than an hour left. Two ways to hand it over, by DEMO_LIMITS:
#     show (default)  a stage `.claude/.credentials.json` holding the access
#                     token only. The rail's limits footer reads that file, so
#                     your real plan usage is in the frames - record when the
#                     session window is fresh, or an orange "nearly out"
#                     outshouts "waiting for you".
#     hide            CLAUDE_CODE_OAUTH_TOKEN in the environment and no file.
#                     The footer finds no account, but `claude` is on PATH,
#                     so it is drawn as "Claude limit · not signed in ·
#                     sign in" (limits_footer_has_something_to_say: a CLI
#                     user who has not signed in keeps the panel). True, and
#                     it reads as broken in a hero shot. Claude Code's
#                     banner then reads "Claude API".
#   The prompts are short; a run is a few turns of your plan's usage.
# - No Screen Recording or Accessibility permission for this script: every step
#   goes through the app's own IPC socket, and window bounds are readable
#   without either. The screen recording itself is made by you - or, with
#   --capture, by ffmpeg, which then needs Screen Recording for the app the
#   script runs from. It writes recording.mov into the run dir.
#
# - With DEMO_CODEX=1 (the default) the middle pane is Codex, not Claude Code,
#   signed in the way your own Codex is (see lib.sh: an API key is copied, a
#   ChatGPT sign-in is not). DEMO_CODEX=0 records three Claude Code panes.
#
# Every state on screen is one the app reaches by itself. The "waiting for
# you" in the hero is Claude Code's real permission prompt for `npm test`,
# because the stage allows file edits and deliberately does not allow Bash.

set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
demo_init main "${1:-}"

# The third surface's Claude session id, from the seeded session, before the
# app rewrites the file.
TESTS_SESSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["projects"][0]["surfaces"][2]["agent"]["session_id"])' "$SESSION_SEED")"
CLAUDE_PANES=$(( DEMO_CODEX == 1 ? 2 : 3 ))

demo_start_recording
demo_launch
mark app-ready
sleep 2

say "three agents in one project, restored as agent surfaces"
# Surface ids in slot order. The list comes back in layout order; the ids are
# written down once so every later step addresses the same pane.
wait_surfaces 3 30 || die "expected three surfaces, got ${#SURFACES[@]}"
mark agents-up

for sid in "${SURFACES[@]}"; do
  cli wait --match "$sid" --idle --for 2000 --timeout 60 >/dev/null || die "surface $sid never settled"
done
mark agents-ready
if [[ "$DEMO_CODEX" == 1 ]]; then
  check_codex_hooks_trusted || die "Codex is on its hook review screen, not at work (see scene.sh)"
fi
sleep 1

cli send "${SURFACES[0]}" --submit \
  "Add a --json flag to \`ledger export\` that prints the entries as a JSON array. Edit the code only; do not run any commands." >/dev/null
sleep 1.5
# Codex reads a file by running a command, which its stage settings let it
# do in its sandbox without asking; Claude Code reads it with a tool.
if [[ "$DEMO_CODEX" == 1 ]]; then
  cli send "${SURFACES[1]}" --submit \
    "Read src/parse.js and explain in two sentences what it does. Do not change any files." >/dev/null
else
  cli send "${SURFACES[1]}" --submit \
    "Read src/parse.js and explain in two sentences what it does. Do not run any commands." >/dev/null
fi
sleep 1.5
cli send "${SURFACES[2]}" --submit \
  "Run npm test and tell me which test fails and why. Do not change any files." >/dev/null
mark prompts-sent

# The pane border, the header fill and the rail row's accent bar follow focus.
sleep 1.5
focus 0; sleep 1.5
focus 1; sleep 1.5
focus 2
mark focus-walk

say "waiting for the permission prompt in 'tests'"
wait_claude_status waiting 1 180 "$TESTS_SESSION" || die "the tests agent never reached 'waiting' in 180 s"
mark waiting
sleep 4   # the rail's pass (2 s) plus the announce movement (480 ms)
# DEMO_HOLD=1 keeps the stage as it is here - one agent waiting, two at
# work or done - until the hold file is removed, so a paired phone can be
# pointed at it.
if [[ -n "${DEMO_HOLD:-}" ]]; then
  touch "$RUN_DIR/hold"
  say "holding: stage $STAGE, socket $SOCK; rm $RUN_DIR/hold to go on"
  while [[ -e "$RUN_DIR/hold" ]]; do sleep 1; done
fi
mark hero-dark
sleep 4

# The keyboard way to the agent that is waiting: focus goes elsewhere first,
# then the same action as its key brings it back.
say "jumping to the waiting agent with jump_next_waiting"
focus 0
sleep 1.5
mark jump
rpc app.dispatch_action '{"action": "jump_next_waiting"}' >/dev/null
sleep 2.5

say "answering the prompt in its own pane"
cli send "${SURFACES[2]}" "1" >/dev/null
mark answered

say "waiting for all three to finish"
wait_claude_status idle "$CLAUDE_PANES" 240 || say "the Claude Code sessions did not all reach idle in time; continuing"
if [[ "$DEMO_CODEX" == 1 ]]; then
  cli wait --match "${SURFACES[1]}" --idle --for 2000 --timeout 120 >/dev/null \
    || say "the Codex session did not settle in time; continuing"
fi
sleep 4
mark all-idle

# send_answer reads the source session's transcript. A Codex pane restored
# without a session id has none Splitlane can read, so with Codex in the middle
# the answer comes from the test run on the right instead.
if [[ "$DEMO_CODEX" == 1 ]]; then ANSWER_FROM=2; else ANSWER_FROM=1; fi
say "sending an answer from pane $ANSWER_FROM to the agent on the left"
# The menu first, as a person would open it; then the entry, which closes it.
# The answer lands on the left agent's input line and is not submitted.
focus "$ANSWER_FROM"
sleep 1.5
mark answer-menu
rpc app.dispatch_action '{"action": "open_agents_thread_menu"}' >/dev/null
sleep 2.5
mark answer-send
rpc surface.send_answer "{\"surface_id\": ${SURFACES[$ANSWER_FROM]}, \"to_surface_id\": ${SURFACES[0]}}" >/dev/null
sleep 4
mark answer-sent

say "layout: the row becomes a grid, and a fourth pane opens holding the diff"
focus 0
sleep 1.5
rpc workspace.restore_layout '{"layout":{"type":"split","direction":"horizontal","children":[{"type":"split","direction":"vertical","children":[{"type":"pane","surfaces":[]},{"type":"pane","surfaces":[]}]},{"type":"split","direction":"vertical","children":[{"type":"pane","surfaces":[]},{"type":"pane","surfaces":[{"surface_type":"diff"}]}]}]}}' >/dev/null
# The diff surface computes off the render thread; hold until it has drawn.
sleep 7
mark grid
sleep 3
focus 2; sleep 1.5
focus 0; sleep 1.5

say "the file tree, then the command palette"
rpc app.dispatch_action '{"action": "toggle_files_sidebar"}' >/dev/null
sleep 1
mark files
sleep 4
rpc app.dispatch_action '{"action": "toggle_files_sidebar"}' >/dev/null
sleep 1.5
rpc app.dispatch_action '{"action": "open_command_palette"}' >/dev/null
sleep 0.8
mark palette
sleep 3.5
rpc app.dispatch_action '{"action": "open_command_palette"}' >/dev/null
sleep 1.5
mark end
sleep 2   # a tail for the cut, before the window closes

demo_finish
