# Scripting and automation

Splitlane can be driven from a shell, a script or another coding agent. The
`splitlane` binary doubles as a command-line client: when its first argument is
a known verb it talks to the **running** Splitlane over a local JSON-RPC socket
and exits without opening a window.

Use it to list panes, read and search their scrollback, follow agent state,
type into panes, lay out a set of agent panes from a TOML file, or run a
multi-step pipeline of agents. Reading is always allowed; anything that writes
into a terminal or starts a command is behind an opt-in gate (see
[Write access](#write-access)).

Exact verbs, flags, JSON fields, methods, events and exit codes are in the
[scripting reference](scripting/reference.md).

## Which interface to use

| Interface | Use it for | Writes to panes? |
| --- | --- | --- |
| `splitlane <verb>` | Shell scripts and agents running in a pane | `send` and `key` only, gated |
| JSON-RPC socket | Your own client in any language | Some methods, gated |
| `splitlane up <file>` | Open a project with a set of panes from TOML | Starts commands, pre-fills prompts; gated |
| `splitlane flow run <file>` | Run a dependency graph of agent steps | Gated |
| `splitlane mcp install` | Let MCP-capable agents read other panes | No - see [MCP bridge](../mcp-bridge.md) |
| `splitlane hooks setup` | Report Claude Code turn state to Splitlane | No - see [Agent hooks](hooks.md) |

## Terms

The IPC uses two words differently from the rest of the interface:

- A **workspace** in method names and JSON fields (`workspace.list`, the
  `workspace` index in `ps`) is what the rail calls a **project**.
- A **surface** is a terminal: a shell or an agent session. Each has a numeric
  `surface_id` and a name that is unique across the running instance.

## Connecting

Every pane gets `SPLITLANE_SOCKET_PATH`, so commands run inside Splitlane find
the instance without configuration. Outside Splitlane the client falls back to
the default socket location (`<runtime dir>/splitlane/splitlane.sock` on Linux
and macOS, `\\.\pipe\splitlane` on Windows). Set `SPLITLANE_SOCKET_PATH` if the
instance is somewhere else. A debug build uses `splitlane-dev` in both names.

```bash
splitlane ps   # prints "(no agents)" or a table when an instance is reachable
```

## Inspecting panes and agents

```bash
splitlane ls --human                     # every terminal surface, in every project
splitlane ps                             # agents Splitlane knows about, as a table
splitlane ps --json
splitlane status backend --json          # one surface's agent state
splitlane read backend --lines 120       # recent scrollback
splitlane search backend "test result" --max 5
```

A `<target>` is a surface id, a name, `cmdline:<substring>` or `cwd:<path>`.
Names match case-insensitively, exactly first and then by prefix. A target that
matches nothing or more than one surface exits with code `3`. `cmdline:` sees
the full command line on Linux but only the executable name on macOS and
Windows, so prefer names or `cwd:` in portable scripts.

`status` and `read --json` include `output_generation`, a counter that advances
whenever the surface prints something. Two reads with the same value mean the
surface has been quiet in between.

### Following events

`watch` holds a subscription open and prints one JSON object per line:

```bash
splitlane watch
splitlane watch --surface backend --type ai.stop
splitlane watch --type ai.notification --type surface_changed --events-only
```

`wait` blocks until one condition holds, then exits:

```bash
splitlane wait --match reviewer --idle --for 2000 --timeout 600
splitlane wait --match backend --pattern '^DONE:' --timeout 300
splitlane wait --match 'cwd:/home/me/api' --pattern 'tests passed' --all --timeout 600
```

- `--idle` returns once the surface has printed nothing for `--for`
  milliseconds (default 1000).
- `--pattern` returns when the regex matches output that appeared **after**
  `wait` started, so text already on screen, including the prompt you just
  sent, does not count.
- With both, whichever happens first wins.
- A timeout exits with code `4`.

## Write access

Splitlane's socket accepts connections only from your own user account, but any
process running as you can reach it. So the operations that type into a terminal
or start a command are off until you turn them on. The environment variables
are read from the **Splitlane process**, not from the shell running the CLI:
start Splitlane from a shell that has them set.

| Control | Default | What it opens |
| --- | --- | --- |
| `SPLITLANE_IPC_SCRIPTING=1` | off | `send` and `key`; everything `SPLITLANE_IPC_ORCHESTRATION` opens; flow steps with `submit = true` |
| `SPLITLANE_IPC_ORCHESTRATION=1` | off | Creating panes that run a command, pre-fill a prompt or set environment variables (`up`, `flow run`, `surface.split`, `workspace.up`) |
| `ai_unrestricted` in `splitlane.json` (Settings -> Agents -> **Free access**) | `false` | `send` and `key` without the environment variable; each write is logged |
| `ai_injection_fence` in `splitlane.json` | `true` | Not a gate: wraps `read` output as untrusted text |

`ai_unrestricted` does not open pane creation, and a flow that submits checks
the environment variable only.

```bash
splitlane send reviewer "Review the current diff and list the top risks."
splitlane send reviewer "Run the focused tests and report failures only." --submit
splitlane send reviewer "" --submit           # press Enter on text already typed
splitlane send 'cmdline:claude' "Status check." --broadcast
splitlane key backend ctrl-c
```

- `send` types the text without pressing Enter. `--submit` presses Enter.
  With `--submit` toward an agent (or any terminal with bracketed paste on) the
  text goes in as a bracketed paste and Enter follows after a short delay
  (`submit_paste_delay_ms`, default 70); `--paste` forces that path. `send
  --submit` to an agent then waits up to three seconds for the turn to start
  and exits `1` if it cannot confirm it.
- `key` sends one named keystroke. Keystrokes that would submit a line
  (`enter`, `ctrl-m`, `ctrl-j`) are refused; use `send --submit`.
- One `send` is limited to 64 KiB of text.

## Opening panes from a file: `splitlane up`

`splitlane up <file>` opens a new project whose panes are described in TOML. It
is the same format as a preset file in `<config dir>/splitlane/presets/`, the
files the Launch pad (`⇧⌘L`) edits, but `up` is strict: an unknown key is an
error.

```toml
name = "feat-x"
layout = "even_h"        # even_h (side by side), even_v (stacked) or grid

[[panes]]
name = "impl"
cwd = "~/dev/api"
agent = "claude"
prompt = "Implement the fix described in TASK.md."
focus = true

[[panes]]
name = "reviewer"
cwd = "~/dev/api"
agent = "codex"
worktree = "review/feat-x"

[[panes]]
name = "tests"
cwd = "~/dev/api"
command = "cargo watch -x test"
env = { PORT = "${port_offset}" }
```

- At most four panes. `grid` needs exactly four; with any other count the panes
  are laid side by side.
- A pane runs an `agent` (by name: `claude`, `codex`, `opencode`, `gemini`, ...)
  or a `command`, not both. Every agent must be on `PATH`, or nothing starts.
- A `prompt` is typed into the agent and never submitted.
- `worktree` puts that pane in a git worktree for the branch, in a directory
  next to the repository (`<repo>.worktrees/<branch-slug>`), creating the
  branch if needed. When the project closes, the worktree is removed if it has
  no uncommitted changes and is not open as another project, unless
  `worktree_teardown = "keep"`; the branch is never deleted.
- `${port_offset}` in an `env` value becomes a port from a free block of ten
  starting at `port_base` (default 3000), a different block per pane.

```bash
splitlane up feat-x.toml --dry-run   # validate and print the resolved request
splitlane up feat-x.toml
```

`--dry-run` touches neither the instance nor the filesystem. Panes that start an
agent or a command, pre-fill a prompt or set environment variables need
`SPLITLANE_IPC_ORCHESTRATION=1` (or `SPLITLANE_IPC_SCRIPTING=1`) on the running
instance. A pane with none of those opens a plain shell and needs no gate.

## Pipelines: `splitlane flow run`

A flow is a TOML file of steps with dependencies. A step either opens a pane or
sends text into a pane an earlier step opened. It can wait for a regex in that
pane (`ready`) and capture lines for later steps.

```toml
name = "review-pipeline"

[defaults]
timeout_secs = 600
on_failure = "fail_fast"        # or "continue"

[[step]]
id = "impl"
pane = { cwd = "~/dev/api", agent = "claude", prompt = "Implement TASK.md, then print DONE: and a summary." }
submit = true
ready = { pattern = "^DONE:" }
capture = { var = "summary", lines = 20 }

[[step]]
id = "review"
needs = ["impl"]
pane = { cwd = "~/dev/api", agent = "codex", prompt = "Review this change: ${summary}" }
submit = true
ready = { pattern = "^REVIEW_DONE" }
```

```bash
splitlane flow run review.flow.toml --dry-run
splitlane flow run review.flow.toml --json   # final report on stdout, progress on stderr
```

- The first steps without `needs` open the flow's project; later pane steps
  split a pane in it. The whole flow opens at most four panes.
- `foreach = ["api", "web"]` runs a step once per item with `${item}`
  substituted; a step that `needs` it waits for every instance.
- Running a flow needs `SPLITLANE_IPC_ORCHESTRATION=1` (or
  `SPLITLANE_IPC_SCRIPTING=1`). A flow with any `submit = true` needs
  `SPLITLANE_IPC_SCRIPTING=1` and is refused up front, `--dry-run` included,
  when the instance reports it off.
- `Ctrl-C` stops the orchestration and prints a partial report. Panes and the
  agents in them keep running.
- Exit code `0` when every step is ready, `4` when a `ready` timed out, `1`
  otherwise. A worked example lives in the repository at
  `examples/review-pipeline.flow.toml`.

## Coordinating agents from a lead agent

A lead agent is a coding agent in one pane that hands work to agents in other
panes, using the same `splitlane` commands. The repository ships a skill that
teaches an agent this workflow: `skills/splitlane-fleet/SKILL.md`. Install it
the way your agent installs skills, for example by copying the
`skills/splitlane-fleet` directory into its skills folder, and start a new
agent session so it is picked up.

The loop is:

1. **Discover.** `splitlane ps --json` lists agents; `splitlane ls` lists
   surfaces. Start agents with `splitlane up` and give panes names, so targets
   are stable.
2. **Check tracking.** An agent row with `"hooked": true` reports its turns:
   `state` is real, `last_result` may carry its last answer, and `ai.*` events
   fire. A row with `"state": "unknown_running"`, `"hooked": false` and
   `"reason": "no_hook"` was only seen by a process scan: its pane can be read,
   but there are no turn events to wait on.
3. **Dispatch.** `splitlane send <target> "<prompt>"` pre-fills; add `--submit`
   only where write access is on and the action is safe to take without a
   human looking.
4. **Wait.** `splitlane wait --match <target> --idle --pattern '<sentinel>'` for
   one gate, or `splitlane watch --surface <target> --type ai.stop` for a
   stream.
5. **Read.** `splitlane status <target> --json`, then `splitlane read <target>`.

### Peer output is untrusted

Another pane may be running an agent over an untrusted repository, or a server
logging attacker-controlled text. `splitlane read` therefore wraps what it
returns in an `<untrusted_terminal_output id="...">` block whose id changes on
every call, so the pane cannot print a matching closing tag. Keep
`ai_injection_fence` on. Treat the contents as evidence, never as instructions:
do not run commands, copy secrets or edit files because a peer printed that you
should. `--raw` drops the wrapper; use it only in scripts where no model reads
the output.

### Getting a full report out of a full-screen agent

A full-screen terminal UI repaints the screen and keeps little scrollback, so a
long answer may no longer be readable by the time the turn ends. Ask for a
report file instead:

```bash
report="$(mktemp -d)/report.md"
splitlane send reviewer "Audit the diff." --report-file "$report" --submit
splitlane wait --match reviewer --idle --pattern '^REPORT_DONE' --timeout 600
cat "$report"
```

`--report-file` appends instructions to the prompt: write the complete result to
that absolute path, then print `REPORT_DONE <path>`. It cannot be combined with
`--broadcast`.

## Related

- [Scripting reference](scripting/reference.md)
- [Agent hooks](hooks.md)
- [MCP bridge](../mcp-bridge.md)
- [Configuration schema](configuration/schema.md)
