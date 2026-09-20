#!/usr/bin/env bash
# Run the README demo choreography against a clean debug build, and write a
# timeline the post-processing step cuts the screen recording by.
#
#   scripts/record-demo/record.sh              # you record the screen (Cmd-Shift-5)
#   scripts/record-demo/record.sh --rehearse   # same run, no recording prompt
#   DEMO_THEME=light scripts/record-demo/record.sh   # the light variant
#
# Then:
#   scripts/record-demo/process.sh <recording.mov> <run-dir>
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
#     hide            CLAUDE_CODE_OAUTH_TOKEN in the environment and no file:
#                     the footer finds no account and is not drawn. Claude
#                     Code's banner then reads "Claude API".
#   The prompts are short; a run is a few turns of your plan's usage.
# - No Screen Recording or Accessibility permission for this script: every step
#   goes through the app's own IPC socket, and window bounds are readable
#   without either. The screen recording itself is made by you.
#
# Every state on screen is one the app reaches by itself. The "waiting for
# you" in the hero is Claude Code's real permission prompt for `npm test`,
# because the stage allows file edits and deliberately does not allow Bash.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BIN="$ROOT/target/debug/splitlane"
REHEARSE=0
[[ "${1:-}" == "--rehearse" ]] && REHEARSE=1

say() { printf '\033[1m>> %s\033[0m\n' "$*" >&2; }
die() { printf 'record-demo: %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "macOS only"
[[ -x "$BIN" ]] || die "no debug build at $BIN - run: cargo build -p splitlane-app"
security find-generic-password -s "Claude Code-credentials" >/dev/null 2>&1 \
  || die "no Claude Code Keychain entry - sign Claude Code in first"
CLAUDE_BIN="$(command -v claude)" || die "claude not on PATH"
NODE_BIN="$(command -v node)" || die "node not on PATH (the demo repo's tests use it)"

# Short, because the IPC socket path has to fit in sun_path.
STAGE="$(mktemp -d /tmp/sl-demo.XXXXXX)"
STAGE="$(cd "$STAGE" && pwd -P)"
RUN_DIR="$ROOT/target/record-demo/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$RUN_DIR"
TIMELINE="$RUN_DIR/timeline.jsonl"
APP_PID=""

cleanup() {
  [[ -n "$APP_PID" ]] && kill "$APP_PID" 2>/dev/null || true
  # The agents' own processes can still be writing into the stage for a moment
  # after the app is gone, and the stage holds credential copies.
  for _ in 1 2 3 4 5; do
    sleep 1
    rm -rf "$STAGE" 2>/dev/null && break
  done
  # An exiting agent can recreate its working directory after the first pass.
  sleep 2
  rm -rf "$STAGE" 2>/dev/null
  [[ -e "$STAGE" ]] && printf 'record-demo: could not remove %s - delete it by hand\n' "$STAGE" >&2
  return 0
}
trap cleanup EXIT

mark() {
  local now
  now="$(python3 -c 'import time; print(int(time.time() * 1000))')"
  printf '{"mark":"%s","t":%s}\n' "$1" "$now" >>"$TIMELINE"
  say "mark: $1"
}

say "staging scene in $STAGE"
REPO="$("$HERE/scene.sh" "$STAGE")"
DEMO_HOME="$STAGE/home"
DEMO_LIMITS="${DEMO_LIMITS:-show}"
[[ "$DEMO_LIMITS" == show || "$DEMO_LIMITS" == hide ]] || die "DEMO_LIMITS must be show or hide"
mkdir -p "$DEMO_HOME/.claude"
CLAUDE_TOKEN="$(security find-generic-password -s "Claude Code-credentials" -w 2>/dev/null |
  python3 -c '
import json, os, sys, time
entry = json.loads(sys.stdin.read())
oauth = entry["claudeAiOauth"]
left = (oauth["expiresAt"] / 1000 - time.time()) / 60
if left < 60:
    sys.exit(f"Claude access token has {left:.0f} min left; run any claude command to refresh it, then retry")
if sys.argv[1] == "show":
    # The refresh token stays in the Keychain: without it nothing here can
    # rotate the login.
    oauth.pop("refreshToken", None)
    oauth.pop("refreshTokenExpiresAt", None)
    fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, json.dumps(entry).encode())
    os.close(fd)
else:
    print(oauth["accessToken"])
' "$DEMO_LIMITS" "$DEMO_HOME/.claude/.credentials.json")" || die "could not read the Claude access token"
SOCK="$STAGE/tmp/splitlane-dev/splitlane-dev.sock"
# The third surface's session id, from the seeded session, before the app
# rewrites the file.
TESTS_SESSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["projects"][0]["surfaces"][2]["agent"]["session_id"])' "$DEMO_HOME/Library/Caches/splitlane-dev/session-dev.json")"

# A whitelist, not the caller's environment: a shell inside Splitlane carries
# SPLITLANE_WORKSPACE_ID and agent-session markers that must not reach the
# stage.
PATH_DEMO="$(dirname "$CLAUDE_BIN"):$(dirname "$NODE_BIN"):/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
stage_env=(
  env -i
  HOME="$DEMO_HOME" TMPDIR="$STAGE/tmp/" PATH="$PATH_DEMO"
  USER="$USER" LOGNAME="$USER" SHELL=/bin/zsh LANG=en_US.UTF-8
  SPLITLANE_IPC_SCRIPTING=1
  # The detector's own account of every surface, so a run can be checked
  # against what the rail was actually told (app.log in the run dir).
  RUST_LOG="info,splitlane::agent_state=debug"
)
[[ "$DEMO_LIMITS" == hide ]] && stage_env+=(CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_TOKEN")
cli() { "${stage_env[@]}" "$BIN" "$@"; }
rpc() { python3 "$HERE/rpc.py" "$SOCK" "$@"; }
focus() { rpc surface.focus "{\"surface_id\": ${SURFACES[$1]}}" >/dev/null; }

# Waits on the same fact the app's detector reads first: the status Claude Code
# writes for its own process. The rail is two seconds behind it at most.
# With a session id, only that surface's file counts.
wait_claude_status() {
  local want="$1" count="$2" timeout="$3" session="${4:-}"
  python3 - "$DEMO_HOME/.claude/sessions" "$REPO" "$want" "$count" "$timeout" "$session" <<'PY'
import glob, json, sys, time
root, repo, want, count, timeout, session = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), float(sys.argv[5]), sys.argv[6]
deadline = time.time() + timeout
while time.time() < deadline:
    hits = 0
    for path in glob.glob(f"{root}/*.json"):
        try:
            data = json.load(open(path))
        except Exception:
            continue
        if session and data.get("sessionId") != session:
            continue
        if data.get("cwd") == repo and data.get("status") == want:
            hits += 1
    if hits >= count:
        sys.exit(0)
    time.sleep(0.5)
sys.exit(1)
PY
}

if [[ $REHEARSE -eq 0 ]]; then
  cat >&2 <<'EOF'

  Start the screen recording now: Cmd-Shift-5, "Record Entire Screen", Record.
  The demo window opens in the centre of the screen, 1440x1000, and is brought
  to the front. Do not click anything until the script says "done" - the
  assets are cut by screen rectangle, so whatever covers the window is in them.

  Press Enter once the recording is running.
EOF
  read -r _
fi

mark start
say "launching the debug build"
(cd "$REPO" && exec "${stage_env[@]}" "$BIN") >"$RUN_DIR/app.log" 2>&1 &
APP_PID=$!
for _ in $(seq 1 60); do [[ -S "$SOCK" ]] && break; sleep 0.5; done
[[ -S "$SOCK" ]] || die "the app never opened its socket (see $RUN_DIR/app.log)"
sleep 2
# In front of everything else, the installed app included: the cut is by screen
# rectangle, so anything overlapping the window ends up in the assets.
swift "$HERE/window-bounds.swift" activate "$APP_PID" || say "could not bring the demo window to the front"
# The screen recording draws the pointer; the corner is outside the window.
swift "$HERE/window-bounds.swift" park-pointer || say "could not move the pointer out of the frame"
sleep 1

read -r WX WY WW WH < <(swift "$HERE/window-bounds.swift" "$APP_PID") || die "no window for pid $APP_PID"
read -r DW DH DS < <(swift "$HERE/window-bounds.swift" display)
cat >"$RUN_DIR/geometry.json" <<EOF
{ "theme": "${DEMO_THEME:-dark}",
  "window": { "x": $WX, "y": $WY, "width": $WW, "height": $WH },
  "display": { "width": $DW, "height": $DH, "scale": $DS } }
EOF
mark app-ready
sleep 2

say "three agents in one project, restored as agent surfaces"
# Surface ids in slot order. The list comes back in layout order; the ids are
# written down once so every later step addresses the same pane.
for _ in $(seq 1 60); do
  rpc surface.list >"$RUN_DIR/surfaces.json" 2>/dev/null \
    && python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["pane_count"] == 3 else 1)' "$RUN_DIR/surfaces.json" \
    && break
  sleep 0.5
done
SURFACES=()
while read -r sid; do SURFACES+=("$sid"); done < <(python3 -c 'import json,sys; [print(s["surface_id"]) for s in json.load(open(sys.argv[1]))["surfaces"][:3]]' "$RUN_DIR/surfaces.json")
[[ ${#SURFACES[@]} -eq 3 ]] || die "expected three surfaces, got ${#SURFACES[@]}"
mark agents-up

for sid in "${SURFACES[@]}"; do
  cli wait --match "$sid" --idle --for 2000 --timeout 60 >/dev/null || die "surface $sid never settled"
done
mark agents-ready
sleep 1

cli send "${SURFACES[0]}" --submit \
  "Add a --json flag to \`ledger export\` that prints the entries as a JSON array. Edit the code only; do not run any commands." >/dev/null
sleep 1.5
cli send "${SURFACES[1]}" --submit \
  "Read src/parse.js and explain in two sentences what it does. Do not run any commands." >/dev/null
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
mark hero-dark
sleep 4

say "answering the prompt in its own pane"
cli send "${SURFACES[2]}" "1" >/dev/null
mark answered

say "waiting for all three to finish"
wait_claude_status idle 3 240 || say "the three sessions did not all reach idle in time; continuing"
sleep 4
mark all-idle

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
mark end
sleep 2   # a tail for the cut, before the window closes

git -C "$REPO" diff --stat >"$RUN_DIR/repo-diff.txt" 2>&1 || true
say "done - run dir: $RUN_DIR"
if [[ $REHEARSE -eq 0 ]]; then
  cat >&2 <<EOF

  Stop the recording now (the stop button in the menu bar), then:

    scripts/record-demo/process.sh <path-to-recording.mov> $RUN_DIR

EOF
fi
