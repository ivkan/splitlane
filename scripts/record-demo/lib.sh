# shellcheck shell=bash
# What every recording run shares: the stage, the agents' sign-in, the
# environment the app runs in, the screen capture, the timeline and the
# window. Sourced by record.sh, record-fleet.sh and record-host.sh, each of
# which is one scene's choreography.
#
#   source lib.sh
#   demo_init <scene> "$@"     # main | fleet | host; takes --rehearse / --capture
#   demo_start_recording
#   demo_launch                # the app, in front, its geometry written once
#   ...the scene...
#   demo_finish
#
# Why each piece is as it is - the throwaway HOME, the access token without
# its refresh token, the whitelisted environment - is written at the place
# it is done. The README of the pipeline is record.sh's header.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BIN="$ROOT/target/debug/splitlane"
REHEARSE=0
CAPTURE=0
APP_PID=""
CAPTURE_PID=""
GEOMETRY_WRITTEN=0

say() { printf '\033[1m>> %s\033[0m\n' "$*" >&2; }
die() { printf 'record-demo: %s\n' "$*" >&2; exit 1; }

# ffmpeg stops cleanly - finishing the file - on a `q` on its stdin; a signal
# can leave the recording without its index.
stop_capture() {
  [[ -n "$CAPTURE_PID" ]] || return 0
  printf 'q' >&7 2>/dev/null || true
  exec 7>&- 2>/dev/null || true
  wait "$CAPTURE_PID" 2>/dev/null || true
  CAPTURE_PID=""
  rm -f "$CAPTURE_FIFO"
}

# The pids of session hosts this run started: a `__splitlane-host` process
# holding the stage's host socket open. The stage is the only place that
# socket can be, so the hosts of an installed Splitlane - and of any other
# debug build - are out of reach. (Not by HOME in the environment: macOS
# does not show one process another's environment any more.)
stage_host_pids() {
  local sock="$1/tmp/splitlane-dev/splitlane-dev-host.sock" pid
  [[ -S "$sock" ]] || return 0
  for pid in $(lsof -t -- "$sock" 2>/dev/null | sort -u); do
    if ps -ww -o command= -p "$pid" 2>/dev/null | grep -qF -- "__splitlane-host"; then
      printf '%s\n' "$pid"
    fi
  done
}

# Ends the session hosts of this stage and, through their guards, the agents
# in them. A host left running would keep agents spending, and keep writing
# into a stage that holds a copy of the access token.
stop_stage_hosts() {
  local stage="$1" pids
  pids="$(stage_host_pids "$stage")"
  [[ -n "$pids" ]] || return 0
  # shellcheck disable=SC2086 # one pid per word
  kill $pids 2>/dev/null || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 0.5
    [[ -z "$(stage_host_pids "$stage")" ]] && return 0
  done
  # shellcheck disable=SC2046
  kill -9 $(stage_host_pids "$stage") 2>/dev/null || true
}

# Ends every process started from a file inside the stage. Codex starts a
# daemon of its own (`codex app-server --managed-daemon`) from a copy it
# unpacks under CODEX_HOME; it outlives the pane and the window, and would
# keep the stage's sign-in in use. The path names this stage alone.
stop_stage_processes() {
  local stage="$1" pids
  pids="$(pgrep -f -- "$stage/" 2>/dev/null || true)"
  [[ -n "$pids" ]] || return 0
  # shellcheck disable=SC2086 # one pid per word
  kill $pids 2>/dev/null || true
  sleep 1
  pids="$(pgrep -f -- "$stage/" 2>/dev/null || true)"
  # shellcheck disable=SC2086
  [[ -z "$pids" ]] || kill -9 $pids 2>/dev/null || true
}

cleanup() {
  stop_capture
  [[ -n "$APP_PID" ]] && kill "$APP_PID" 2>/dev/null || true
  [[ -n "${STAGE:-}" ]] && stop_stage_hosts "$STAGE"
  [[ -n "${STAGE:-}" ]] && stop_stage_processes "$STAGE"
  [[ -n "${STAGE:-}" ]] || return 0
  # The agents' own processes can still be writing into the stage for a moment
  # after the app is gone, and the stage holds credential copies.
  for _ in 1 2 3 4 5; do
    sleep 1
    rm -rf "$STAGE" 2>/dev/null && break
  done
  # An exiting agent can recreate its working directory after the first pass.
  sleep 2
  stop_stage_processes "$STAGE"
  rm -rf "$STAGE" 2>/dev/null
  [[ -e "$STAGE" ]] && printf 'record-demo: could not remove %s - delete it by hand\n' "$STAGE" >&2
  return 0
}

mark() {
  local now
  now="$(python3 -c 'import time; print(int(time.time() * 1000))')"
  printf '{"mark":"%s","t":%s}\n' "$1" "$now" >>"$TIMELINE"
  say "mark: $1"
}

# demo_init <scene> [--rehearse|--capture]
demo_init() {
  SCENE="$1"
  case "${2:-}" in
    --rehearse) REHEARSE=1 ;;
    --capture) CAPTURE=1 ;;
    "") ;;
    *) die "unknown option ${2} (--rehearse or --capture)" ;;
  esac

  [[ "$(uname -s)" == "Darwin" ]] || die "macOS only"
  [[ -x "$BIN" ]] || die "no debug build at $BIN - run: cargo build -p splitlane-app"
  security find-generic-password -s "Claude Code-credentials" >/dev/null 2>&1 \
    || die "no Claude Code Keychain entry - sign Claude Code in first"
  CLAUDE_BIN="$(command -v claude)" || die "claude not on PATH"
  NODE_BIN="$(command -v node)" || die "node not on PATH (the demo repo's tests use it)"
  # One Codex pane in the main and host scenes shows the rail is not one
  # vendor's. DEMO_CODEX=0 records three Claude Code panes instead.
  DEMO_CODEX="${DEMO_CODEX:-1}"
  [[ "$SCENE" == fleet ]] && DEMO_CODEX=0
  if [[ "$DEMO_CODEX" == 1 ]]; then
    CODEX_BIN="$(command -v codex)" || die "codex not on PATH (or run with DEMO_CODEX=0)"
    [[ -f "$HOME/.codex/auth.json" ]] || die "Codex is not signed in (~/.codex/auth.json) - run codex login, or DEMO_CODEX=0"
  fi
  command -v ffmpeg >/dev/null || [[ $CAPTURE -eq 0 ]] || die "--capture needs ffmpeg"

  # Short, because the IPC socket path has to fit in sun_path.
  STAGE="$(mktemp -d /tmp/sl-demo.XXXXXX)"
  STAGE="$(cd "$STAGE" && pwd -P)"
  RUN_DIR="$ROOT/target/record-demo/$(date +%Y%m%d-%H%M%S)-$SCENE"
  mkdir -p "$RUN_DIR"
  TIMELINE="$RUN_DIR/timeline.jsonl"
  CAPTURE_FIFO="$RUN_DIR/capture.ctl"
  trap cleanup EXIT

  say "staging the $SCENE scene in $STAGE"
  REPO="$(DEMO_SCENE="$SCENE" DEMO_CODEX="$DEMO_CODEX" "$HERE/scene.sh" "$STAGE")"
  DEMO_HOME="$STAGE/home"
  SOCK="$STAGE/tmp/splitlane-dev/splitlane-dev.sock"
  SESSION_SEED="$DEMO_HOME/Library/Caches/splitlane-dev/session-dev.json"
  cp "$SESSION_SEED" "$RUN_DIR/session-seed.json"

  demo_sign_in_claude
  [[ "$DEMO_CODEX" == 1 ]] && demo_sign_in_codex

  # A whitelist, not the caller's environment: a shell inside Splitlane carries
  # SPLITLANE_WORKSPACE_ID and agent-session markers that must not reach the
  # stage.
  #
  # The agents and node come through a bin directory of their own, not the
  # directories they live in: those hold other agents' CLIs too (an
  # npm-installed `gemini` sits beside `node`), and the app runs each one it
  # finds to list its sessions, which holds up the diff pane by seconds.
  # `splitlane` is the debug build, so an agent in a pane that runs it talks
  # to this window (the pane's SPLITLANE_SOCKET_PATH says which).
  STAGE_BIN="$STAGE/bin"
  mkdir -p "$STAGE_BIN"
  ln -s "$CLAUDE_BIN" "$STAGE_BIN/claude"
  ln -s "$BIN" "$STAGE_BIN/splitlane"
  [[ "$DEMO_CODEX" == 1 ]] && ln -s "$CODEX_BIN" "$STAGE_BIN/codex"
  local tool
  for tool in node npm npx; do
    [[ -e "$(dirname "$NODE_BIN")/$tool" ]] && ln -s "$(dirname "$NODE_BIN")/$tool" "$STAGE_BIN/$tool"
  done
  PATH_DEMO="$STAGE_BIN:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
  stage_env=(
    env -i
    HOME="$DEMO_HOME" TMPDIR="$STAGE/tmp/" PATH="$PATH_DEMO"
    USER="$USER" LOGNAME="$USER" SHELL=/bin/zsh LANG=en_US.UTF-8
    CODEX_HOME="$DEMO_HOME/.codex"
    SPLITLANE_IPC_SCRIPTING=1
    # The detector's own account of every surface, so a run can be checked
    # against what the rail was actually told (app.log in the run dir).
    RUST_LOG="info,splitlane::agent_state=debug"
  )
  [[ "$DEMO_LIMITS" == hide ]] && stage_env+=(CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_TOKEN")
  return 0
}

# Real Claude Code, signed in with the access token from your own Keychain
# entry and never with its refresh token, so nothing inside the stage can
# rotate your login. See record.sh's header for DEMO_LIMITS.
demo_sign_in_claude() {
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
}

# Codex, signed in the way your own Codex is. An API key is copied into the
# stage (mode 600) and goes with it. A ChatGPT sign-in is refused: its
# refresh token is single-use, so a copy that refreshed inside the stage
# would sign your own Codex out. Sign the stage's Codex in separately for
# that case (DEMO_CODEX_AUTH, below).
demo_sign_in_codex() {
  local auth="${DEMO_CODEX_AUTH:-$HOME/.codex/auth.json}"
  python3 - "$auth" "$DEMO_HOME/.codex/auth.json" <<'PY' || die "could not hand Codex its sign-in"
import json, os, sys
src, dst = sys.argv[1], sys.argv[2]
entry = json.load(open(src))
key = entry.get("OPENAI_API_KEY")
mode = entry.get("auth_mode") or ("apikey" if key else "chatgpt")
if os.path.realpath(src) == os.path.realpath(os.path.expanduser("~/.codex/auth.json")) and not key:
    sys.exit(f"record-demo: your Codex is signed in with {mode}, whose refresh token a copy "
             "could spend; sign a separate Codex home in (CODEX_HOME=<dir> codex login) and "
             "pass DEMO_CODEX_AUTH=<dir>/auth.json, or run with DEMO_CODEX=0")
os.makedirs(os.path.dirname(dst), exist_ok=True)
fd = os.open(dst, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
os.write(fd, json.dumps(entry).encode())
os.close(fd)
PY
}

cli() { "${stage_env[@]}" "$BIN" "$@"; }
rpc() { python3 "$HERE/rpc.py" "$SOCK" "$@"; }
focus() { rpc surface.focus "{\"surface_id\": ${SURFACES[$1]}}" >/dev/null; }

# Waits on the same fact the app's detector reads first: the status Claude Code
# writes for its own process (idle, busy, waiting). The rail is two seconds
# behind it at most. With a session id, only that surface's file counts;
# with `!<id>`, every surface's but that one.
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
        if session.startswith("!") and data.get("sessionId") == session[1:]:
            continue
        if session and not session.startswith("!") and data.get("sessionId") != session:
            continue
        if data.get("cwd") == repo and data.get("status") == want:
            hits += 1
    if hits >= count:
        sys.exit(0)
    time.sleep(0.5)
sys.exit(1)
PY
}

# Whether every hook in the stage repo's `.codex/hooks.json` - written by the
# shim when the Codex pane started - is one scene.sh trusted in advance. A
# miss means Codex is showing its "Hooks need review" screen instead of
# working; the hash is the one in scene.sh's `codex_trust_splitlane_hooks`.
check_codex_hooks_trusted() {
  python3 - "$REPO/.codex/hooks.json" "$DEMO_HOME/.codex/config.toml" <<'PY'
import hashlib, json, re, sys, tomllib
hooks_path, config_path = sys.argv[1], sys.argv[2]
try:
    hooks = json.load(open(hooks_path))["hooks"]
except (OSError, ValueError, KeyError) as err:
    sys.exit(f"no Codex hooks.json to check ({err})")
state = tomllib.load(open(config_path, "rb")).get("hooks", {}).get("state", {})
no_matcher = {"stop", "user_prompt_submit"}
missing = []
for event, groups in hooks.items():
    key = re.sub(r"(?<!^)([A-Z])", r"_\1", event).lower()
    for gi, group in enumerate(groups):
        for hi, handler in enumerate(group.get("hooks", [])):
            identity = {"event_name": key, "hooks": [{"type": "command", "command": handler["command"],
                        "timeout": handler.get("timeout", 600), "async": handler.get("async", False)}]}
            if group.get("matcher") is not None and key not in no_matcher:
                identity["matcher"] = group["matcher"]
            want = "sha256:" + hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
            if state.get(f"{hooks_path}:{key}:{gi}:{hi}", {}).get("trusted_hash") != want:
                missing.append(f"{key}:{gi}:{hi} ({handler['command']})")
if missing:
    sys.exit("Codex hooks not trusted in advance: " + ", ".join(missing))
PY
}

# Writes the surface list into the run dir and the first <n> ids, in layout
# order, into SURFACES, once the active project has <n> panes holding them.
wait_surfaces() {
  local n="$1" timeout="${2:-30}" sid
  local tries=$(( timeout * 2 ))
  for _ in $(seq 1 "$tries"); do
    rpc surface.list >"$RUN_DIR/surfaces.json" 2>/dev/null \
      && python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if len(d["surfaces"]) >= int(sys.argv[2]) and d["pane_count"] >= int(sys.argv[2]) else 1)' "$RUN_DIR/surfaces.json" "$n" \
      && break
    sleep 0.5
  done
  SURFACES=()
  while read -r sid; do SURFACES+=("$sid"); done < <(python3 -c 'import json,sys; [print(s["surface_id"]) for s in json.load(open(sys.argv[1]))["surfaces"][:int(sys.argv[2])]]' "$RUN_DIR/surfaces.json" "$n")
  [[ ${#SURFACES[@]} -eq $n ]]
}

demo_start_recording() {
  if [[ $CAPTURE -eq 1 ]]; then
    say "recording the main display into $RUN_DIR/recording.mov"
    mkfifo "$CAPTURE_FIFO"
    # The creation time is stamped when the file starts, to the microsecond;
    # sync.py lines the timeline up against it.
    ffmpeg -hide_banner -loglevel error -f avfoundation -capture_cursor 0 \
      -framerate 30 -pixel_format nv12 -i "Capture screen 0:none" \
      -c:v h264_videotoolbox -b:v 40M -metadata creation_time=now \
      -y "$RUN_DIR/recording.mov" <"$CAPTURE_FIFO" >"$RUN_DIR/capture.log" 2>&1 &
    CAPTURE_PID=$!
    exec 7>"$CAPTURE_FIFO"
    sleep 2
    kill -0 "$CAPTURE_PID" 2>/dev/null || die "ffmpeg did not start recording (see $RUN_DIR/capture.log)"
  elif [[ $REHEARSE -eq 0 ]]; then
    cat >&2 <<EOF

  Start the screen recording now: Cmd-Shift-5, "Record Entire Screen", Record.
  The demo window opens in the centre of the screen, ${DEMO_WINDOW_W:-1440}x${DEMO_WINDOW_H:-1000}, and is brought
  to the front. Do not click anything until the script says "done" - the
  assets are cut by screen rectangle, so whatever covers the window is in them.

  Press Enter once the recording is running.
EOF
    read -r _
  fi
  mark start
}

# Launches the debug build and waits for its socket. The geometry the cut
# uses is the first launch's; a later launch (the host scene's reopen) is
# checked against it.
demo_launch() {
  local log="$RUN_DIR/app.log"
  [[ $GEOMETRY_WRITTEN -eq 1 ]] && log="$RUN_DIR/app-$(date +%H%M%S).log"
  say "launching the debug build"
  (cd "$REPO" && exec "${stage_env[@]}" "$BIN") >"$log" 2>&1 &
  APP_PID=$!
  for _ in $(seq 1 60); do [[ -S "$SOCK" ]] && rpc system.ping >/dev/null 2>&1 && break; sleep 0.5; done
  rpc system.ping >/dev/null 2>&1 || die "the app never answered on its socket (see $log)"
  sleep 2
  # In front of everything else, the installed app included: the cut is by
  # screen rectangle, so anything overlapping the window ends up in the assets.
  swift "$HERE/window-bounds.swift" activate "$APP_PID" || say "could not bring the demo window to the front"
  # The screen recording draws the pointer; the corner is outside the window.
  swift "$HERE/window-bounds.swift" park-pointer || say "could not move the pointer out of the frame"
  sleep 1

  local wx wy ww wh dw dh ds
  read -r wx wy ww wh < <(swift "$HERE/window-bounds.swift" "$APP_PID") || die "no window for pid $APP_PID"
  if [[ $GEOMETRY_WRITTEN -eq 0 ]]; then
    read -r dw dh ds < <(swift "$HERE/window-bounds.swift" display)
    cat >"$RUN_DIR/geometry.json" <<EOF
{ "scene": "$SCENE", "theme": "${DEMO_THEME:-dark}", "codex": $([[ "$DEMO_CODEX" == 1 ]] && echo true || echo false),
  "window": { "x": $wx, "y": $wy, "width": $ww, "height": $wh },
  "display": { "width": $dw, "height": $dh, "scale": $ds } }
EOF
    GEOMETRY_WRITTEN=1
  else
    python3 - "$RUN_DIR/geometry.json" "$wx" "$wy" "$ww" "$wh" <<'PY' || say "the reopened window is not where the first one was - the cut will be off; see geometry.json"
import json, sys
w = json.load(open(sys.argv[1]))["window"]
now = [float(v) for v in sys.argv[2:6]]
sys.exit(0 if all(abs(a - b) < 2 for a, b in zip(now, [w["x"], w["y"], w["width"], w["height"]])) else 1)
PY
  fi
}

demo_finish() {
  git -C "$REPO" diff --stat >"$RUN_DIR/repo-diff.txt" 2>&1 || true
  if [[ $CAPTURE -eq 1 ]]; then
    stop_capture
    say "done - run dir: $RUN_DIR; next:"
    printf '\n    scripts/record-demo/process.sh %s/recording.mov %s\n\n' "$RUN_DIR" "$RUN_DIR" >&2
  else
    say "done - run dir: $RUN_DIR"
    if [[ $REHEARSE -eq 0 ]]; then
      cat >&2 <<EOF

  Stop the recording now (the stop button in the menu bar), then:

    scripts/record-demo/process.sh <path-to-recording.mov> $RUN_DIR

EOF
    fi
  fi
}
