#!/usr/bin/env bash
# Build the stage the demo is recorded on: a throwaway HOME, a small
# plausible repository with uncommitted work in it, and agent settings that
# make the choreography land the same way every run.
#
#   scene.sh <stage-dir>
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
cat >"$APP_SUPPORT/splitlane-dev/splitlane.json" <<EOF
{
  "theme": "$APP_THEME",
  "font_size": ${DEMO_FONT_SIZE:-11},
  "window_decorations": "client"
}
EOF
# `window_state.rs` reads this path without the `-dev` suffix.
WINDOW_W="${DEMO_WINDOW_W:-1440}"
# 1000 rather than 900: at 900 an agent's transcript overflowed its pane by a
# couple of rows, and Claude Code redraws its welcome header wrongly when it is
# scrolled that little - a black band where the mascot's eyes should be.
WINDOW_H="${DEMO_WINDOW_H:-1000}"
printf '{ "width": %s, "height": %s }\n' "$WINDOW_W" "$WINDOW_H" \
  >"$APP_SUPPORT/splitlane/window-state.json"

# --- The restored session: one project, three Claude Code surfaces in a row --
# Three of one agent, not a mix: a second vendor needs its own paid account on
# the recording machine, and a pane showing that account's billing error is not
# a demo. The rail's point - which of several sessions wants you - is the same.
# Agent surfaces are what the rail lists and the state detector reads. They are
# seeded as a restored session because nothing else opens one without a key
# press: `splitlane up` types the agent's command into a shell pane, which the
# app rightly treats as a shell. Each Claude surface gets a fresh session id and
# has no transcript yet, so the launch mints it (`SessionBinding::resolve`).
seed_session() {
  local cache="$DEMO_HOME/Library/Caches/splitlane-dev"
  mkdir -p "$cache"
  python3 - "$cache/session-dev.json" "$REPO" <<'PY'
import json, sys, time, uuid
path, repo = sys.argv[1], sys.argv[2]
now = int(time.time() * 1000)

def agent(surface_id, slot, title, tag, session_id):
    return {
        "id": surface_id, "kind": "agent",
        "placement": {"type": "slot", "index": slot},
        "title": title, "cwd": repo, "created_at": now - slot * 1000,
        "agent": {"agent": tag, "thread_kind": "terminal", "terminal_agent": tag,
                  "session_id": session_id, "pinned": False, "title_user_set": False},
    }

surfaces = [
    agent(2, 0, "Claude Code", "claude_code", str(uuid.uuid4())),
    agent(3, 1, "Claude Code", "claude_code", str(uuid.uuid4())),
    agent(4, 2, "Claude Code", "claude_code", str(uuid.uuid4())),
]
leaf = lambda s: {"type": "pane", "surfaces": [{"surface_type": "agent", "surface_id": s["id"]}]}
json.dump({
    "version": 2,
    "active_workspace": 0,
    "projects": [{
        "id": 1, "title": "ledger", "cwd": repo, "is_expanded": True,
        "layout": {"type": "split", "direction": "vertical",
                   "children": [leaf(s) for s in surfaces]},
        "surfaces": surfaces,
    }],
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
cat >"$DEMO_HOME/.claude/settings.json" <<'EOF'
{
  "permissions": {
    "allow": ["Read", "Edit", "Write", "Glob", "Grep"]
  }
}
EOF

seed_session

echo "$REPO"
