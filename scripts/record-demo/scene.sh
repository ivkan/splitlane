#!/usr/bin/env bash
# Build the stage the demo is recorded on: a throwaway HOME, a small
# plausible repository with uncommitted work in it, and agent settings that
# make the choreography land the same way every run.
#
#   scene.sh <stage-dir>
#   DEMO_SCENE=fleet scene.sh <stage-dir>
#
# DEMO_SCENE picks what is seeded: `main` (default) three agents in a row;
# `fleet` one lead session, named plan, with the fleet skill and leave to run
# `splitlane`; `host` the main row, with the session host on, so the
# sessions outlive the window. DEMO_CODEX=1 makes the middle agent of `main`
# and `host` Codex.
#
# Nothing here reads or writes the real home directory. (`record.sh` hands
# Claude's access token to the stage - never its refresh token.)
#
# Everything is pinned that can be: commit dates, author, window size,
# theme. What is not pinned is what the agents say, which is why the
# choreography waits on states rather than on text.

set -euo pipefail

STAGE="${1:?usage: scene.sh <stage-dir>}"
mkdir -p "$STAGE"
STAGE="$(cd "$STAGE" && pwd -P)"

SCENE="${DEMO_SCENE:-main}"
case "$SCENE" in
  main | fleet | host) ;;
  *) echo "scene.sh: DEMO_SCENE must be main, fleet or host" >&2; exit 2 ;;
esac
DEMO_CODEX="${DEMO_CODEX:-0}"
[[ "$SCENE" == fleet ]] && DEMO_CODEX=0
DEMO_HOME="$STAGE/home"
REPO="$DEMO_HOME/code/ledger"
mkdir -p "$DEMO_HOME" "$STAGE/tmp" "$REPO"

# --- Splitlane: debug build config, fixed window, dark theme -----------------
# `dirs::config_dir()` on macOS is `$HOME/Library/Application Support`.
APP_SUPPORT="$DEMO_HOME/Library/Application Support"
mkdir -p "$APP_SUPPORT/splitlane-dev" "$APP_SUPPORT/splitlane"
# DEMO_THEME=light records the light variant of the stills; the agents' own
# TUIs are switched with it, because a dark-theme TUI on a light pane is
# unreadable.
case "${DEMO_THEME:-dark}" in
  dark) APP_THEME="Harbor Dark"; CLAUDE_THEME="dark" ;;
  light) APP_THEME="Harbor Light"; CLAUDE_THEME="light" ;;
  *) echo "scene.sh: DEMO_THEME must be dark or light" >&2; exit 2 ;;
esac
# 11 pt rather than the default 13: three panes in a 1440-point window get
# about 50 columns each instead of 42, so the agents' TUIs wrap less. The
# README shows the frame scaled down, where terminal text is texture at any
# size; the rail and headers, which carry the point, do not follow this value.
#
# No session host, except in the scene about it: with one, the agents
# outlive the window the run kills at the end, keep writing into the stage,
# and the stage - which holds a copy of the access token - cannot be removed
# until the host is stopped too. lib.sh stops the stage's host on the way
# out; the other scenes do not need one. In the host scene, closing the
# window keeps the sessions (`on_quit`), which is what the scene shows.
if [[ "$SCENE" == host ]]; then
  HOST_SETTINGS='"session_host": true,
  "on_quit": "keep"'
else
  HOST_SETTINGS='"session_host": false'
fi
cat >"$APP_SUPPORT/splitlane-dev/splitlane.json" <<EOF
{
  "theme": "$APP_THEME",
  "font_size": ${DEMO_FONT_SIZE:-11},
  "window_decorations": "client",
  $HOST_SETTINGS
}
EOF
# `window_state.rs` reads this path without the `-dev` suffix.
# The fleet scene fills the site's wide frame (1920:988), so its window is
# wider and lower: a lead pane beside a grid of four.
if [[ "$SCENE" == fleet ]]; then
  WINDOW_W="${DEMO_WINDOW_W:-1600}"
else
  WINDOW_W="${DEMO_WINDOW_W:-1440}"
fi
# 1000 rather than 900: at 900 an agent's transcript overflowed its pane by a
# couple of rows, and Claude Code redraws its welcome header wrongly when it is
# scrolled that little - a black band where the mascot's eyes should be.
if [[ "$SCENE" == fleet ]]; then
  WINDOW_H="${DEMO_WINDOW_H:-824}"
else
  WINDOW_H="${DEMO_WINDOW_H:-1000}"
fi
printf '{ "width": %s, "height": %s }\n' "$WINDOW_W" "$WINDOW_H" \
  >"$APP_SUPPORT/splitlane/window-state.json"

# --- The restored session: one project, three agent surfaces in a row ---------
# Claude Code on both sides; the middle one is Codex with DEMO_CODEX=1, to
# show the rail is not one vendor's. Codex needs its own account on the
# recording machine (lib.sh hands it over), and a pane showing that account's
# billing error is not a demo - so DEMO_CODEX=0 keeps three Claude Code.
# The fleet scene seeds one Claude Code surface, `plan`, which opens the rest
# itself. Agent surfaces are what the rail lists and the state detector reads. They are
# seeded as a restored session because nothing else opens one without a key
# press: `splitlane up` types the agent's command into a shell pane, which the
# app rightly treats as a shell. Each Claude surface gets a fresh session id and
# has no transcript yet, so the launch mints it (`SessionBinding::resolve`).
seed_session() {
  local cache="$DEMO_HOME/Library/Caches/splitlane-dev"
  mkdir -p "$cache"
  python3 - "$cache/session-dev.json" "$REPO" "$SCENE" "$DEMO_CODEX" <<'PY'
import json, os, sys, time, uuid
path, repo, scene, codex = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1"
now = int(time.time() * 1000)

def agent(surface_id, slot, title, tag, session_id, named=False):
    binding = {"agent": tag, "thread_kind": "terminal", "terminal_agent": tag,
               "pinned": False, "title_user_set": named}
    # Claude Code takes the id it is given (`--session-id`); Codex mints its
    # own, and the app binds it when Codex reports it.
    if session_id:
        binding["session_id"] = session_id
    return {
        "id": surface_id, "kind": "agent",
        "placement": {"type": "slot", "index": slot},
        "title": title, "cwd": repo, "created_at": now - slot * 1000,
        "agent": binding,
    }

if scene == "fleet":
    surfaces = [agent(2, 0, "plan", "claude_code", str(uuid.uuid4()), named=True)]
else:
    middle = (agent(3, 1, "Codex", "codex", None) if codex
              else agent(3, 1, "Claude Code", "claude_code", str(uuid.uuid4())))
    surfaces = [
        agent(2, 0, "Claude Code", "claude_code", str(uuid.uuid4())),
        middle,
        agent(4, 2, "Claude Code", "claude_code", str(uuid.uuid4())),
    ]
leaf = lambda s: {"type": "pane", "surfaces": [{"surface_type": "agent", "surface_id": s["id"]}]}

# A few more projects, so the rail shows groups: `ledger` with two others
# under "Work", and a folded "Side projects". They are empty folders beside
# the repository; nothing runs in them.
def neighbour(pid, title, group):
    cwd = os.path.join(os.path.dirname(repo), title)
    os.makedirs(cwd, exist_ok=True)
    return {"id": pid, "title": title, "cwd": cwd, "is_expanded": False, "group": group}

json.dump({
    "version": 2,
    "active_workspace": 0,
    "groups": [
        {"id": 1, "name": "Work"},
        {"id": 2, "name": "Side projects", "collapsed": True},
    ],
    "projects": [{
        "id": 1, "title": "ledger", "cwd": repo, "is_expanded": True, "group": 1,
        "layout": {"type": "split", "direction": "vertical",
                   "children": [leaf(s) for s in surfaces]},
        "surfaces": surfaces,
    },
        neighbour(5, "billing-api", 1),
        neighbour(6, "web-app", 1),
        neighbour(7, "dotfiles", 2),
        neighbour(8, "blog", 2),
    ],
}, open(path, "w"), indent=2)
PY
}

# --- The repository ----------------------------------------------------------
git_demo() {
  GIT_AUTHOR_NAME="Demo" GIT_AUTHOR_EMAIL="demo@example.com" \
  GIT_COMMITTER_NAME="Demo" GIT_COMMITTER_EMAIL="demo@example.com" \
  GIT_AUTHOR_DATE="2026-09-01T10:00:00Z" GIT_COMMITTER_DATE="2026-09-01T10:00:00Z" \
  HOME="$DEMO_HOME" git -C "$REPO" "$@"
}

mkdir -p "$REPO/src" "$REPO/test"
cat >"$REPO/package.json" <<'EOF'
{
  "name": "ledger",
  "version": "0.3.0",
  "description": "Export a plain-text ledger to CSV",
  "type": "module",
  "bin": { "ledger": "src/cli.js" },
  "scripts": { "test": "node --test" }
}
EOF
cat >"$REPO/README.md" <<'EOF'
# ledger

Reads a plain-text ledger and exports it.

    ledger export journal.txt > out.csv
EOF
cat >"$REPO/src/parse.js" <<'EOF'
// One entry per line: `2026-09-01  Coffee  -3.50  food`
export function parseLedger(text) {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"))
    .map((line) => {
      const [date, payee, amount, category = ""] = line.split(/\s{2,}/);
      return { date, payee, amount: Number(amount), category };
    });
}
EOF
cat >"$REPO/src/format.js" <<'EOF'
export function formatAmount(value) {
  return value.toFixed(2);
}

export function csvField(value) {
  const text = String(value);
  return text.includes(",") ? `"${text}"` : text;
}
EOF
cat >"$REPO/src/export.js" <<'EOF'
import { csvField, formatAmount } from "./format.js";

export function toCsv(entries) {
  const rows = entries.map((e) =>
    [e.date, csvField(e.payee), formatAmount(e.amount), e.category].join(","),
  );
  return ["date,payee,amount,category", ...rows].join("\n");
}
EOF
cat >"$REPO/src/cli.js" <<'EOF'
#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { parseLedger } from "./parse.js";
import { toCsv } from "./export.js";

const [command, file] = process.argv.slice(2);
if (command !== "export" || !file) {
  console.error("usage: ledger export <file>");
  process.exit(2);
}
console.log(toCsv(parseLedger(readFileSync(file, "utf8"))));
EOF
cat >"$REPO/test/format.test.js" <<'EOF'
import { test } from "node:test";
import assert from "node:assert/strict";
import { csvField, formatAmount } from "../src/format.js";

test("formats two decimals", () => {
  assert.equal(formatAmount(3.5), "3.50");
});

test("quotes a field containing a quote", () => {
  assert.equal(csvField('The "Good" Cafe'), '"The ""Good"" Cafe"');
});
EOF
# The agents' hook shim writes `.claude/` (and `.codex/`) into the project; without
# this line they fill the diff surface with files nobody changed.
cat >"$REPO/.gitignore" <<'EOF'
node_modules/
.claude/
.codex/
EOF
cat >"$REPO/journal.txt" <<'EOF'
2026-09-01  Coffee, Blue Door  -3.50  food
2026-09-01  Salary  2400  income
2026-09-02  The "Good" Cafe  -12.10  food
EOF
git_demo init -q -b main
git_demo add -A
git_demo commit -q -m "ledger: parse and export to CSV"

# Uncommitted work already in progress, so the diff surface has something to
# show before any agent has touched the tree.
cat >"$REPO/src/parse.js" <<'EOF'
// One entry per line: `2026-09-01  Coffee  -3.50  food`
export function parseLedger(text) {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"))
    .map((line, index) => {
      const [date, payee, amount, category = ""] = line.split(/\s{2,}/);
      if (Number.isNaN(Number(amount))) {
        throw new Error(`line ${index + 1}: bad amount "${amount}"`);
      }
      return { date, payee, amount: Number(amount), category };
    });
}
EOF

# --- Claude Code: onboarding done, folder trusted, edits allowed, Bash asks ---
# Remote Control is off: on an account that has it, every session would print
# its claude.ai/code/session_... link into the pane - and into the assets.
# Bash is deliberately NOT allowed: the permission prompt it raises is the real
# "waiting for you" the hero frame shows. Nothing is simulated.
mkdir -p "$DEMO_HOME/.claude"
cat >"$DEMO_HOME/.claude.json" <<EOF
{
  "hasCompletedOnboarding": true,
  "theme": "$CLAUDE_THEME",
  "remoteControlAtStartup": false,
  "bypassPermissionsModeAccepted": false,
  "projects": {
    "$REPO": {
      "hasTrustDialogAccepted": true,
      "hasCompletedProjectOnboarding": true,
      "allowedTools": []
    }
  }
}
EOF
# `defaultMode` is spelled out because newer Claude Code starts in auto mode,
# which runs `npm test` without asking - and the prompt is the scene.
#
# The fleet scene's lead opens and runs sessions with `splitlane`, so that
# one command is allowed; every session it opens shares these settings, and
# the one told to run `npm test` still stops to ask.
ALLOW='"Read", "Edit", "Write", "Glob", "Grep"'
[[ "$SCENE" == fleet ]] && ALLOW="$ALLOW, \"Bash(splitlane:*)\""
cat >"$DEMO_HOME/.claude/settings.json" <<EOF
{
  "permissions": {
    "defaultMode": "default",
    "allow": [$ALLOW]
  }
}
EOF

# The fleet skill teaches the lead the `splitlane` commands. A release
# writes it at every start; a debug build never does, so the stage gets the
# repository's copy.
if [[ "$SCENE" == fleet ]]; then
  ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  mkdir -p "$DEMO_HOME/.claude/skills/splitlane-fleet"
  cp "$ROOT/skills/splitlane-fleet/SKILL.md" "$DEMO_HOME/.claude/skills/splitlane-fleet/SKILL.md"
fi

# Codex (0.158) runs a project's hooks only once they are trusted, and asks
# about every new one: a full-screen "Hooks need review" at start, which
# would hold the pane on that screen and leave the rail saying "waiting for
# you" for the wrong reason. A person answers it once ("Trust all") and Codex
# writes one `trusted_hash` per hook into config.toml; this writes the same
# lines up front, for the five hooks Splitlane's shim puts into the
# project's `.codex/hooks.json` when the pane starts.
#
# What the shim writes, per event: one group with no matcher and one handler,
# `<bin dir>/splitlane-ai-hook <Event>` with a 5 s timeout, where the bin dir
# is the one the app extracts its helpers into under the stage's cache
# (`ai_hooks::extract`, one directory per release version). What Codex
# hashes (codex-rs/hooks/src/engine/discovery.rs, `hook_hash`): the hook's
# event key, its matcher (none for Stop and UserPromptSubmit) and the
# normalized handler, as JSON with sorted keys and no spaces, sha256. Checked
# against what Codex 0.158 itself wrote after "Trust all". record.sh compares
# these with the hooks.json the shim actually wrote, once Codex is up.
codex_trust_splitlane_hooks() {
  local root version
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")"
  python3 - "$REPO" "$DEMO_HOME/Library/Caches/splitlane-dev/bin/$version/splitlane-ai-hook" <<'PY'
import hashlib, json, sys
repo, hook = sys.argv[1], sys.argv[2]
print("\n[hooks.state]")
for event, key in [("SessionStart", "session_start"), ("UserPromptSubmit", "user_prompt_submit"),
                   ("PreToolUse", "pre_tool_use"), ("PostToolUse", "post_tool_use"), ("Stop", "stop")]:
    identity = {"event_name": key,
                "hooks": [{"type": "command", "command": f"{hook} {event}", "timeout": 5, "async": False}]}
    digest = hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    print(f'\n[hooks.state."{repo}/.codex/hooks.json:{key}:0:0"]\ntrusted_hash = "sha256:{digest}"')
PY
}

# --- Codex: folder trusted, never asks, works inside its sandbox -------------
# The middle agent only reads a file, which Codex does by running a command;
# `never` with the workspace-write sandbox runs that without a prompt, so
# the one "waiting for you" on screen stays Claude Code's. lib.sh puts the
# sign-in beside this file.
if [[ "$DEMO_CODEX" == 1 ]]; then
  mkdir -p "$DEMO_HOME/.codex"
  cat >"$DEMO_HOME/.codex/config.toml" <<EOF
approval_policy = "never"
sandbox_mode = "workspace-write"
check_for_update_on_startup = false

[projects."$REPO"]
trust_level = "trusted"
EOF
  codex_trust_splitlane_hooks >>"$DEMO_HOME/.codex/config.toml"
fi

seed_session

echo "$REPO"
