# Scripting reference

The exact command-line, file-format and JSON-RPC surface. The guide that explains
how the pieces fit is [Scripting and automation](../scripting.md).

In method names and JSON fields, a **workspace** is a project on the rail and
`workspace` is its zero-based position in the rail. A **surface** is a terminal
(shell or agent session) with a numeric `surface_id`.

## CLI verbs

`splitlane <verb>` connects to the running instance and never opens a window.
`splitlane <verb> --help` prints the verb's flags. A first argument that looks
like a verb but is not one exits with code `2`.

| Verb | Flags | Method | Output |
| --- | --- | --- | --- |
| `ls` | `--human` | `surface.list` | JSON (default) or a table |
| `read <target>` | `--lines N`, `--offset N`, `--json`, `--raw` | `surface.read` | Text (default) or the JSON result |
| `search <target> <pattern>` | `--max N`, `--human` | `surface.search` | JSON (default) or `line: text` |
| `ps` | `--json` | `fleet.list` | Table (default) or JSON |
| `status <target>` | `--json` | `surface.status` | `state (tool)` line (default) or JSON |
| `answer <target>` | `--after N`, `--max-bytes BYTES`, `--out FILE`, `--raw`, `--json` | `surface.status`, then the agent's own conversation file | The answer, fenced (default), or JSON |
| `new` | `--name NAME`, `--cwd DIR` | `workspace.create` | JSON |
| `select <index>` | | `workspace.select` | JSON |
| `split <h\|horizontal\|v\|vertical>` | `--target SEL` | `surface.split` | JSON |
| `focus <target>` | | `surface.focus` | JSON |
| `send <target> <text>` | `--submit`, `--paste`, `--broadcast`, `--report-file PATH` | `surface.send_text` | JSON |
| `key <target> <keystroke>` | | `surface.send_keystroke` | JSON |
| `add --agent <name>` | `--name NAME`, `--prompt TEXT` or `--prompt-file PATH`, `--submit`, `--parked` or `--pane`, `--json` | `surface.add_agent` | `<surface_id>`, a tab and where the session went (default), or JSON |
| `park <target>` | `--json` | `surface.park` | A line (default) or JSON |
| `show <target>` | `--beside TARGET`, `--direction h\|v`, `--json` | `surface.show` | A line (default) or JSON |
| `close <target>` | `--stop-turn`, `--json` | `surface.close` | A line (default) or JSON |
| `interrupt <target>` | `--timeout SECS` (default 10), `--json` | `surface.interrupt`, then `surface.status` | `<surface_id>`, a tab and the outcome (default), or JSON |
| `wait` | `--match SEL` (required), one of `--pattern REGEX`, `--idle`, `--until turn-end`, `--until allowed`, `--state WORDS`; `--for MS`, `--timeout SECS`, `--any`, `--all`, `--after N`, `--through-waiting`, `--start-grace SECS` | `surface.read`, `surface.status`, `events.subscribe` | JSON |
| `watch` | `--surface SEL`, `--type TYPE` (repeatable), `--events-only` | `events.subscribe` | JSON lines |
| `up <file>` | `--dry-run` | `workspace.up` | JSON |
| `flow run <file>` | `--dry-run`, `--json` | `workspace.up`, `surface.split`, `surface.send_text`, `surface.read` | Progress lines, or a JSON report with `--json` |

`list_panes`, `read_pane` and `search_pane` are accepted as aliases of `ls`,
`read` and `search`.

Two more command families run without a running instance and edit agent
configuration files:

| Command | Page |
| --- | --- |
| `splitlane mcp install \| status \| uninstall` | [MCP bridge](../../mcp-bridge.md) |
| `splitlane hooks setup \| status \| uninstall` | [Agent hooks](../hooks.md) |

### Verb details

- `add` opens an agent session the way the launcher does, in the project of
  the pane it is called from. The session goes into an empty pane, or a new
  pane where one fits, and otherwise into the rail with no pane; it never
  takes the place of what a pane is showing. `--parked` asks for the rail,
  `--pane` for a pane or a refusal. A prompt is written once the agent is
  there to read it and is not submitted without `--submit`. The answer
  carries `runs_ended`, the number to pass to `wait --after`. One session
  has at most eight sessions it opened running at a time, and a session
  that was itself opened by one opens none.
- `park` takes a session out of its pane and leaves it running in the rail.
  `show` puts a session that has no pane into one: an empty pane, a new
  pane, or failing both the pane of another session the caller opened, the
  one that has gone longest without focus.
- `close` is "Delete session": it stops the session's process and drops its
  row. The conversation stays in the agent's own history. A session in the
  middle of a turn is closed only with `--stop-turn`.
- `interrupt` stops the turn a session is in and leaves the session open. It
  writes the agent's own interrupt key, not `Ctrl-C`, and reports one of:

  | Outcome | Exit | Meaning |
  | --- | --- | --- |
  | `interrupted` | `0` | The agent's own record shows the turn was stopped. The input line is empty: if the agent put the prompt back on it, Splitlane cleared it |
  | `not_running` | `1` | No turn was running. Nothing was sent |
  | `not_confirmed` | `4` | The key was sent and the run did not end as stopped: it finished by itself a moment earlier, or the time ran out |

  Only Claude Code has a known interrupt key; any other agent is refused
  with `no_interrupt`, exit `7`.
- `add`, `park`, `show`, `close`, `interrupt`, and `send` or `key` into
  another session, are decided by which pane the call comes from. Splitlane
  reads that from the process table - the calling process is a descendant
  of the pane's shell - and not from anything the caller says.

- `wait --match <target> --until allowed` waits for a person to answer
  whether the caller may send messages to a session they opened, after a
  `send` to it exited `9`. It takes one target and only `--timeout`, and
  exits `0` when they allowed it, `8` when they did not, and `4` when the
  time ran out with the question still standing - or with no question put.
- `split h` stacks panes, `split v` puts them side by side. Without `--target`
  the first pane of the active project is split.
- `read`: `--lines` defaults to 200 and is clamped to 1-4000 by the server;
  `--offset` counts lines back from the end. `--raw` sends `fenced: false`.
- `search`: case-insensitive substring match; `--max` defaults to 50, clamped
  to 1-1000.
- `send --broadcast` sends to every surface the selector matches and prints
  `{sent, failed, submitted}`; the exit code is `1` if any send failed.
- `send --report-file PATH` resolves `PATH` against the current directory,
  appends a report instruction to the text, and adds `report_file` and
  `report_sentinel` (`REPORT_DONE <path>`) to the output.
- `send --submit` toward an agent waits up to 3 s for a turn to start and
  exits `1` when it cannot confirm one; the output then carries `started` and
  `start_reason`. The confirmation is weak for an agent with no hook: any new
  output counts, including the echo of the text just sent. With `--submit`
  the output also carries `runs_ended`, the surface's `rail.runs_ended` read
  before the text was written, for `wait --until turn-end --after`.
- `wait --pattern` polls every 500 ms over the last 500 lines and matches only
  output that appeared after `wait` started. Prints
  `{matched, panes, matches: [{surface_id, lines}]}`. `--any` and `--all`
  accept a selector matching several surfaces; without them it must match one.
  If every watched surface closes, exits `1`.
- `wait --idle` returns once the surface prints nothing for `--for` ms (default
  1000). With `--pattern` as well, whichever happens first returns. Prints
  `{surface_id, idle, matched}`. Single target only; `--any`/`--all` are
  ignored.
- `wait --until turn-end` returns when the agent's turn is over, reading
  [`rail`](#rail) rather than output. It checks, in this order:

  | Condition | `outcome` | Exit |
  | --- | --- | --- |
  | The surface closed | `closed` | `1` |
  | The surface has no `rail` | `no_rail` | `7` |
  | A run past the baseline has ended and the status is `idle` | `finished`; `failed` when that run's `last_outcome` is `failed`; `interrupted` when it is `interrupted` - the turn was stopped before it finished, by a person or by `interrupt`, and there is no answer to read | `0`, `6`, or `10` |
  | `rail.exited`, or the status is `failed` | `failed` | `6` |
  | The tier is `T3` | `no_turn_signal` after `--start-grace`, or `degraded` at once if the surface was on a higher tier earlier in this wait | `7` |
  | The status is `waiting` | `waiting`, with the question in `message` when known | `5` |
  | No baseline was given, no turn was seen in flight, and `--start-grace` has passed | `no_turn` | `1` |
  | `--timeout` passed | `timeout` | `4` |

  The baseline is `--after N`; without it, `rail.runs_ended` as it is when
  the wait starts. Pass `--after` with the `runs_ended` printed by
  `send --submit`, so a turn that ends before `wait` starts is not missed.
  `--start-grace` defaults to 10 seconds.

  Exit `5` means the agent asked a person a question. It is not an answer to
  wait for and not the end of the turn: answer it in the agent's own pane.
  `--through-waiting` keeps waiting instead, for a script that runs with a
  person at the keyboard.

  A finish is reported 3 to 6 seconds after the agent stops, because the rail
  confirms it for 3 seconds first.

  On tier `T3` the wait is refused, not answered from silence. Use
  `wait --idle --pattern <sentinel>` or `send --report-file` there.

  Prints `{outcome, targets: [{surface_id, outcome, status, tier, runs_ended,
  last_outcome, message}]}`. With `--all` every surface the selector matches
  is waited on until it reaches an outcome of its own, and the exit code is
  the worst one: `6`, then `5`, `7`, `1`, `4`, `0`. With `--any` the first
  surface to reach an outcome ends the wait and is the only one listed.
- `wait --state WORDS` returns when `rail.status` is one of the
  comma-separated words (`starting`, `running`, `waiting`, `idle`, `failed`),
  on any tier and with no confirmation of its own. Prints the same object
  with `outcome: "matched"`. `--any` and `--all` work as above.
- `answer` prints the agent's answer to the newest prompt, as the Markdown it
  was written in. An answer to an earlier prompt is never printed in its
  place. It reads the agent's own record of the conversation - Claude
  Code's transcript, Codex's rollout file - and never the terminal's
  scrollback, where a full-screen agent keeps no history. The CLI reads the
  file itself, using `rail.agent`, `rail.session_id` and `rail.cwd` from
  `surface.status`, so it must run as the user who owns those files, on the
  same machine as Splitlane. Running Splitlane on Windows and the CLI inside
  WSL is not supported.

  | `outcome` | Exit | Meaning |
  | --- | --- | --- |
  | `found` | `0` | The answer is printed |
  | `not_yet` | `1` | No answer yet, or with `--after N` no run past `N` has ended. Wait with `wait --until turn-end` |
  | `turn_in_flight` | `1` | The status is anything but `idle` or `failed`, so the newest answer on disk is the previous turn's |
  | `no_transcript` | `1` | The session has no conversation on disk; nothing was sent to it yet |
  | `unreadable` | `1` | The conversation file exists and could not be read |
  | `turn_aborted` | `1` | A person stopped the newest turn before it answered |
  | `turn_failed` | `6` | Codex: the turn ended in an error, printed on standard error and in `error` |
  | `no_reader` | `7` | The agent is not Claude Code or Codex, or the surface has no `rail`. Use `send --report-file` instead |

  The answer is wrapped in `<untrusted_terminal_output ... kind="answer"
  id="...">`, like `read`: it is text another agent wrote. `--raw` prints it
  bare. An answer longer than `--max-bytes` (default 16384) is written whole
  to a new file in the temporary directory, and only its first lines are
  printed; the path is on standard error and in `file`. `--out FILE` writes
  the whole answer to `FILE` whatever its length. Either file is wrapped the
  same way as the output, and bare with `--raw`. For an outcome other than `found`,
  nothing is printed on standard output unless `--json` is given.

  This is not `last_result` in `status`, which is a short summary some hooks
  report and is absent for most agents.
- `wait --until` and `wait --state` are woken by the `surface.rail` event on
  Linux and macOS and poll `surface.status` every 500 ms on Windows.
- `wait --timeout` defaults to 300 seconds.
- `watch` runs until interrupted (`Ctrl-C` exits `0`). `--events-only` hides
  `subscribed`, `heartbeat` and `dropped` frames.

## Selectors

| Form | Matches |
| --- | --- |
| `42` | The surface with that `surface_id` (any all-digit argument is an id) |
| `backend` | Name, case-insensitive: exact matches first, otherwise name prefix |
| `cmdline:<substr>` | Case-insensitive substring of the foreground command. Full argv on Linux, executable name only on macOS and Windows |
| `cwd:<path>` | Current directory equal to `<path>` or inside it |

No match, or several matches where one is required, exits with code `3`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Runtime failure: instance unreachable, method refused by a gate, handler error, surface closed, flow failed or aborted. `wait --until turn-end`: no turn was in flight. `answer`: no answer to give yet |
| `2` | Usage error, including an unknown verb |
| `3` | Target not found or ambiguous. `watch` and `wait --idle` also use `3` when they cannot open the event stream |
| `4` | `wait` timed out; a flow `ready` barrier timed out |
| `5` | `wait --until turn-end`: the agent is waiting for a person |
| `6` | `wait --until turn-end`: the run failed, or the agent exited. `answer`: the turn ended in an error |
| `7` | `wait --until turn-end`, `wait --state`: the surface has no source that can answer (tier `T3`, or no `rail`). `answer`: no reader for that agent's conversation. `interrupt`: no interrupt key is known for that agent |
| `8` | A rule refused the call and nothing was done. The message names the reason; see [Refusals](#refusals). Repeating the call gets the same answer |
| `9` | The target is a session a person opened, and the person is being asked whether the caller may send it messages. Nothing was written; `wait --until allowed` waits for the answer |
| `10` | `wait --until turn-end`: the turn was stopped before it finished. The agent is idle and takes the next prompt; there is no answer to read |
| `130` | `wait --idle` interrupted with `Ctrl-C` |

## Gates

The environment variables are read from the Splitlane process.

| Operation | Allowed when |
| --- | --- |
| Reads: `ls`, `read`, `search`, `ps`, `status`, `watch`, `wait`, `answer` | Always |
| `new`, `select`, `focus`, `split` without spawn fields | Always |
| `send`, `key`, `interrupt` (`surface.send_text`, `surface.send_keystroke`, `surface.interrupt`) | `SPLITLANE_IPC_SCRIPTING=1`. From a pane with nothing set: always when the target is a session the caller opened with `add`, and after a person said yes when it is an agent session they opened in the same project |
| `add` (`surface.add_agent`) | From a pane: always. From outside a pane: `SPLITLANE_IPC_ORCHESTRATION=1`, and `SPLITLANE_IPC_SCRIPTING=1` to submit the prompt |
| `park`, `show`, `close` | From a pane: for a session the caller opened. From outside a pane: `SPLITLANE_IPC_ORCHESTRATION=1` |
| `app.dispatch_action`, `surface.send_answer` | `SPLITLANE_IPC_SCRIPTING=1` |
| `up`, `surface.split`, `workspace.up` with `command`, `prompt`, `context` or `env` | `SPLITLANE_IPC_ORCHESTRATION=1` or `SPLITLANE_IPC_SCRIPTING=1` |
| `flow run` | `SPLITLANE_IPC_ORCHESTRATION=1` or `SPLITLANE_IPC_SCRIPTING=1` |
| A flow with any `submit = true` | `SPLITLANE_IPC_SCRIPTING=1` (checked before the flow starts, also with `--dry-run` when the instance is reachable) |

Only the value `1` enables a variable. A refused call returns JSON-RPC error
`-32601` with a message naming the variable.

Always refused: a `surface.send_keystroke` whose keystroke contains CR or LF or
would produce one (`enter`, `ctrl-m`, `ctrl-j`), and `surface.send_text` longer
than 64 KiB.

### Refusals

A call a rule refuses returns JSON-RPC error `-32004` with the reason under
`error.data.reason`, and the CLI exits `8` (`7` for `no_interrupt`).

| Reason | Meaning |
| --- | --- |
| `not_from_a_pane` | The caller is in no pane of this app and the launch variable for the call is not set |
| `not_yours` | The target is not a session the caller opened |
| `self` | The target is the caller's own session |
| `waiting` | The target is waiting for a person. Only a person answers that; a key from another session would be an answer |
| `worker_ceiling` | The caller already has eight sessions it opened running |
| `opened_session_cannot_open` | The caller was itself opened by a session |
| `no_room` | A pane was required and there is none to give |
| `other_project` | `close`: the target is in another project |
| `turn_in_flight` | `close`: the target is in the middle of a turn or waiting for a person; pass `--stop-turn` to close it anyway |
| `unseen_by_person` | `close`: the target finished a turn a person started and they have not looked at it. `--stop-turn` does not lift this |
| `no_interrupt` | `interrupt`: no interrupt key is known for the target's agent |
| `person_declined` | A person was asked whether the caller may send messages to the target, and said no. They are not asked again until Splitlane restarts, unless they hand the session over from its row's menu |

A write into an agent session a person opened, before they have answered,
is not a refusal: it returns `-32005` with reason `asked_person` and exit
`9`. The person is asked in the calling session's own pane, once for the
pair; repeating the call while the question stands asks nothing new.

### Related settings

| `splitlane.json` key | Default | Effect |
| --- | --- | --- |
| `ai_injection_fence` | `true` | Default for `surface.read`'s `fenced` param |
| `submit_paste_delay_ms` | `70` (clamped 10-5000) | Delay before Enter after a bracketed-paste send |
| `agent_stall_threshold_secs` | `60` (clamped 30-86400) | A `thinking` session with no hook activity for this long becomes `stalled` |

## Pane environment

Every terminal Splitlane starts carries:

| Variable | Value |
| --- | --- |
| `SPLITLANE_SOCKET_PATH` | Path of the instance's socket or named pipe |
| `SPLITLANE_SURFACE_ID` | This surface's id |
| `SPLITLANE_WORKSPACE_ID` | Internal id of the project or agent session owning the terminal (not the `workspace` index) |

A pane created with a `context` param also gets `SPLITLANE_CONTEXT_FILE`, the
path of a file holding that text.

## Result fields

### `surface.list`

`{pane_count, workspace, surfaces: [...]}`, where `pane_count` and `workspace`
describe the active project. One entry per surface across all projects:

| Field | Meaning |
| --- | --- |
| `surface_id` | Surface id |
| `name` | Unique name: the custom name if set, otherwise derived from the command or title |
| `title` | Terminal title |
| `cwd` | Current directory, when known |
| `cmd` | Foreground command, when known |
| `workspace` | Project index, or `null` for an agent session loaded but not in a pane |
| `scope` | `workspace`, or `agents_thread` for an agent session not in a pane |
| `driven_by` | The surface id of the session a person let drive this one, otherwise `null` |

### `surface.read`

| Field | Meaning |
| --- | --- |
| `text` | Scrollback text, wrapped in `<untrusted_terminal_output ... id="...">` when fenced |
| `lines` | Lines returned |
| `total_lines` | Lines retained |
| `eof` | `true` when the window reaches the oldest retained line |
| `output_generation` | Counter that advances when the surface prints |
| `truncated` | `true` when the text was cut to fit one IPC frame |

### `surface.search`

`{matches: [{line, text}], truncated}`.

### `fleet.list` and `surface.status`

`fleet.list` returns `{agents: [...]}`, sorted by project, then agent, then pid.
An empty fleet is `{"agents": []}`.

| Field | `fleet.list` | `surface.status` | Meaning |
| --- | --- | --- | --- |
| `pid` | yes | | Agent pid; `null` for scan-only rows |
| `tool` | yes | yes | Agent binary name: `claude`, `codex`, `opencode`, `gemini`, ... |
| `state` | yes | yes | See below |
| `hooked` | yes | yes | `true` when the state comes from hook events |
| `reason` | yes | | `null` when hooked, `no_hook` for scan-only rows |
| `surface_id` | yes | yes | Surface id, when known |
| `surface_name` | yes | | Surface name, when known |
| `workspace` | yes | | Project index |
| `active_tool_name` | yes | yes | Tool the agent is running, when reported |
| `message` | yes | yes | Question or notification text, when reported |
| `last_result` | yes | yes | Last turn's result, when the agent reports one |
| `waiting_ms` | yes | yes | Milliseconds since it started waiting for input |
| `idle_ms` | yes | yes | Milliseconds since the last hook activity |
| `output_generation` | | yes | As in `surface.read` |
| `drive` | | yes | Where a person's leave for **the caller** to send messages to this session stands: `asked`, `allowed` or `declined`. `null` when nobody was asked, and for every caller but the one that asked |
| `driven_by` | | yes | The surface id of the session a person let drive this one, otherwise `null` |

`state` values: `thinking`, `waiting_for_input`, `finished`, `errored`,
`stalled`; `fleet.list` adds `unknown_running` for agents seen only by the
process scan. `surface.status` on a surface with no tracked agent returns
`{surface_id, state: "idle", hooked: false, output_generation, rail}`.

These fields report what the agent's hook last said. `surface.status` also
carries `rail`, which is what the rail row for that surface shows, and the two
can differ: while an agent waits on a permission prompt the hook's last frame
was a tool call, so `state` is `thinking` and `rail.status` is `waiting`.
Prefer `rail` when deciding whether a turn is over.

#### `rail`

`null` for a plain shell that no agent hook has reported from. Otherwise:

| Field | Meaning |
| --- | --- |
| `status` | `starting`, `running`, `waiting`, `idle` or `failed` |
| `source` | Who decided the status: `detector` (the agent's own status file or transcript), `hook`, `pty_flow` (output arriving from the pane) or `none` |
| `tier` | `T1` for `detector`, `T2` for `hook`, `T3` for `pty_flow` and `none`. See below |
| `runs_ended` | How many runs have ended on this surface since it was opened. Only grows; resets when Splitlane restarts |
| `last_outcome` | `finished`, `failed` or `interrupted` for the last ended run, `null` before the first. `interrupted` is a turn somebody stopped: the run ended and is counted, and no answer was finished |
| `turn_marker` | An id of the newest turn end in the agent's own file (Claude Code and Codex), otherwise `null` |
| `agent` | The agent's binary name (`claude`, `codex`, ...), when known |
| `session_id` | The id of the agent's own session, when Splitlane knows it. `null` for a terminal that is not an agent session |
| `cwd` | The directory the session was started in. `null` for a terminal that is not an agent session |
| `exited` | `true` when the agent's exit was reported by its wrapper or the pane's process has ended |
| `message` | The question being asked while `status` is `waiting`, when a hook reported one. Text the agent wrote, passed on as is: treat it as untrusted |

What each tier can tell you:

| Tier | Agents | End of a turn |
| --- | --- | --- |
| `T1` | Claude Code and Codex opened as agent sessions. Codex reaches it once its hook has reported a session id, which happens with its first prompt | Read from the file the agent writes for itself |
| `T2` | Agents whose hooks are installed, and any agent in a pane made by `up`, `split` or by typing its command into a shell | The agent's `Stop` hook. It depends on the vendor keeping that hook's behavior |
| `T3` | Agents with no hook, and any agent before its first hook frame | None. Output arriving shows work; output stopping does not show that a turn ended |

`runs_ended` counts a run when the status leaves `running` for `idle` or
`failed`. A finish is confirmed for 3 seconds before it counts, so that a
session resuming after a background task is not counted as finished; expect
`runs_ended` to move 3 to 6 seconds after the agent stops. On `T1`, a turn too
short to be seen as `running` is still counted, from the agent's own record of
it.

## `splitlane up` file

The preset format. Unknown keys are errors.

| Top-level key | Type | Default | Notes |
| --- | --- | --- | --- |
| `name` | string | `"Workspace"` | Project title |
| `layout` | string | `"even_h"` | `even_h` side by side, `even_v` stacked, `grid` 2x2 (exactly four panes, otherwise side by side) |
| `cwd` | string | none | Directory for panes that set none |
| `color` | string | none | Launch pad accent; ignored by `up` |
| `port_base` | integer | `3000` | First port for `${port_offset}` |
| `[[panes]]` | array | required | 1 to 4 entries |

| Pane key | Type | Default | Notes |
| --- | --- | --- | --- |
| `cwd` | string | project `cwd` | `~` expanded; must exist |
| `agent` | string | none | Agent tag (`claude` or `claude_code`, `codex`, `opencode`, `pi`, `hermes`, `grok`, `amp`, `cursor`, `gemini`, `kiro`, `antigravity`, `copilot`, `codebuddy`, `factory`, `qoder`, `openclaw`; `-` and `_` are interchangeable). Must be on `PATH`. Not with `command` |
| `command` | string | none | Command to run. Not with `agent` |
| `prompt` | string | none | Typed into the pane once its output settles; never submitted |
| `focus` | bool | `false` | Focus this pane |
| `env` | table of strings | none | Merged over `terminal.env`; `${port_offset}` is the only variable |
| `name` | string | none | Surface name (trimmed, control characters removed, 64 characters max; duplicates get `-2`, `-3`) |
| `worktree` | string | none | Branch for a git worktree at `<repo>.worktrees/<branch-slug>`. Needs `cwd` inside a repository; may not start with `-` |
| `copy_env` | bool | `true` | Copy the repository's top-level git-ignored `.env*` files into a new worktree. Needs `worktree` |
| `setup` | string | none | Command run in a new worktree before the pane starts; failure warns only. Needs `worktree` |
| `setup_timeout_secs` | integer | `300` | Limit for `setup`. Needs `worktree` |
| `worktree_teardown` | string | `"auto"` | `auto` removes the worktree on close when it holds only committed files (an ignored file keeps it); `keep` leaves it. Needs `worktree` |

`${port_offset}` is replaced, per pane that uses it, with the first port of a
free block of ten at or above `port_base`.

`up --dry-run` prints the `workspace.up` request it would send. A real run
prints `{index, title, panes, surface_ids, labels}`, and removes any worktree it
created if the request fails.

## `splitlane flow run` file

Unknown keys are errors.

| Top-level key | Type | Default | Notes |
| --- | --- | --- | --- |
| `name` | string | `"flow"` | Project title |
| `layout` | string | `"even_h"` | As in `up` |
| `port_base` | integer | `3000` | As in `up` |
| `[defaults]` | table | | `timeout_secs`, `on_failure` (`fail_fast` or `continue`) |
| `[[step]]` | array | required | |

| Step key | Type | Notes |
| --- | --- | --- |
| `id` | string | Required, unique, no `[` or `]`. Default pane name for a pane step |
| `needs` | array of ids | Dependencies. Needing a `foreach` step waits for all its instances |
| `foreach` | array of strings | One instance per item, ids `step[item]`, default pane names `step-item` |
| `pane` | table | Pane keys from `up`. Exactly one of `pane` and `send` |
| `send` | table | `{target, text, submit}`. `target` is a step id or a selector. A send step needs at least one dependency |
| `ready` | table | `{pattern, timeout_secs}`. Regex over the pane's last 500 lines; a timeout is required here or in `[defaults]` |
| `capture` | table | `{var, lines}`. Captures 1-500 lines when `ready` matches; `var` is `[A-Za-z0-9_]+`. Needs `ready` |
| `submit` | bool | Pane steps only: submit the prompt. Default `false` |

Variables:

| Variable | Allowed in |
| --- | --- |
| `${item}` | `foreach` steps: `pane.cwd`, `pane.name`, `pane.worktree`, `pane.env`, `pane.prompt`, `send.target`, `send.text`, `ready.pattern` |
| `${port_offset}` | `pane.env` |
| `${var}` | `send.text`, and `pane.prompt` of a step with `submit = true`. The capturing step must be a dependency |
| `${var.item}` | The same places, for a capture made by a `foreach` step |

The flow is validated before anything runs: unknown dependencies, cycles,
missing timeouts, invalid regexes, undefined or out-of-scope variables,
duplicate pane names, and more than four panes are all errors. At least one
pane step must have no `needs`.

`--json` report: `{flow, status, aborted, steps: [{id, status, duration_ms,
surface_id, error}]}`. `status` is `ready`, `failed` or `aborted`; a step's
status is `PENDING`, `RUNNING`, `READY`, `FAILED` or `SKIPPED`. Exit code `0`
when every step is ready, `4` if a barrier timed out, otherwise `1`.

## JSON-RPC connection

| Property | Value |
| --- | --- |
| Linux, macOS | Unix socket `<runtime dir>/splitlane/splitlane.sock`, runtime dir from `$XDG_RUNTIME_DIR`, the platform runtime dir, `$TMPDIR` (macOS), then `<cache dir>/run` |
| Windows | Named pipe `\\.\pipe\splitlane` |
| Override | `SPLITLANE_SOCKET_PATH` (absolute path) |
| Debug builds | `splitlane-dev` in place of `splitlane` in the directory, file and pipe names |
| Framing | One JSON-RPC 2.0 message per line |
| Requests per connection | Several on Unix; one on Windows |
| Access | Same user only: socket mode `0600` and a peer-uid check on Unix, a restricted security descriptor on Windows. No network listener |
| Limits | 256 KiB per request line; 16 request connections and 16 event subscriptions at once; 30 s idle timeout; 5 s dispatch timeout |

A message without `id` is a notification and gets no reply.

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"system.capabilities"}' \
  | nc -U "$SPLITLANE_SOCKET_PATH"
```

## JSON-RPC methods

Methods that take a surface accept `surface_id`; `surface.read`,
`surface.search`, `surface.status` and `surface.rename` also accept an exact
`name`. With neither, the first terminal of the active project is used, except
by `surface.focus`, which requires `surface_id`.

A parameter that names a target (`surface_id`, `to_surface_id`, `index`) must
be a non-negative JSON number. Any other type, a string such as `"42"` or
`null` included, is refused with `-32602` rather than read as "not given".
Leave the key out to get the default.

| Method | Params | Result |
| --- | --- | --- |
| `system.ping` | | `{pong: true}` |
| `system.capabilities` | | `{scripting, orchestration, methods}` |
| `system.identify` | | `{name, version, protocol}` |
| `workspace.list` | | `{workspaces: [{index, title, cwd, panes, active}]}` |
| `workspace.current` | | `{index, title, cwd, panes, layout}` |
| `workspace.create` | `name` (default `"Terminal"`), `cwd`, `layout` (layout tree) | `{index, title, panes}` |
| `workspace.select` | `index` (required) | `{selected}` |
| `workspace.close` | `index` (required) | `{closed}`; refuses to close the last project |
| `workspace.restore_layout` | `layout` (layout tree) | `{restored, panes}` for the active project |
| `workspace.up` | `name`, `layout` (`even_h`, `even_v`, `grid`), `panes: [{cwd, command, prompt, focus, env, name` or `label, context, profile}]` | `{index, title, panes, surface_ids, labels}` |
| `surface.list` | | See above |
| `surface.read` | `lines`, `offset`, `fenced` | See above. An `offset` past the oldest line is `-32602` |
| `surface.search` | `pattern` (required), `max_matches` | `{matches, truncated}` |
| `surface.status` | | See above |
| `surface.rename` | `new_name` (empty or absent clears it) | `{renamed, name}` |
| `surface.focus` | `surface_id` | `{focused, surface_id, workspace, scope}` |
| `surface.split` | `direction` (`horizontal` or `vertical`, required), `cwd`, `command`, `prompt`, `env`, `name` or `label`, `context`, `profile` | `{split, direction, panes, surface_id}` |
| `surface.send_text` | `text`, `submit` (default `false`), `paste` (default: automatic) | `{sent, length, submitted, paste, submit_mode, agent_target, agent_tool, terminal_bracketed_paste}` |
| `surface.send_keystroke` | `keystroke` (for example `escape`, `ctrl-c`, `alt-f`) | `{sent}` |
| `surface.add_agent` | `agent` (required), `name`, `prompt`, `submit`, `placement` (`auto`, `parked` or `pane`) | `{surface_id, thread_id, agent, tier, session_id, runs_ended, placement, placement_reason, opened_by}` |
| `surface.park` | `surface_id` or `name` | `{parked, surface_id}` |
| `surface.show` | `surface_id` or `name`, `beside_surface_id`, `direction` | `{shown, surface_id, displaced_surface_id}` |
| `surface.close` | `surface_id` or `name`, `stop_turn` | `{closed, surface_id, thread_id}` |
| `surface.interrupt` | `surface_id` or `name` | `{sent, surface_id, rail_status_at_send, runs_ended, last_outcome}`. Answers as soon as the key is written; the stop is confirmed by `rail.runs_ended` passing the value given here with `rail.last_outcome` `interrupted` |
| `app.dispatch_action` | `action`: an action name from [Keybindings](../keybindings.md) (for example `jump_next_waiting`, `toggle_files_sidebar`) | `{dispatched, action}`. The action runs on the next frame, where the same key press would land; the result does not say whether anything answered it |
| `surface.send_answer` | `surface_id` (an agent session), `to_surface_id` (another agent pane on screen in the same project) | `{sent, surface_id, to_surface_id}`. The same as the pane menu's "Send last answer to": the answer lands on the other agent's input line and is not submitted |
| `fleet.list` | | See above |
| `events.subscribe` | `surfaces` (array of ids), `types` (array of event names) | A stream; see below |
| `ai.session_start`, `ai.prompt_submit`, `ai.tool_use`, `ai.notification`, `ai.stop`, `ai.exit`, `ai.session_end` | Hook payload | Sent by `splitlane-ai-hook`; see [Agent hooks](../hooks.md) |

### Errors

| Code | Meaning |
| --- | --- |
| `-32700` | Parse error |
| `-32600` | Invalid request, or the request line is too long |
| `-32601` | Unknown method, or a method refused by a gate |
| `-32602` | Invalid params, including an unknown surface or event type |
| `-32001` | Connecting process belongs to another user |
| `-32002` | The app did not answer within 5 seconds |
| `-32003` | The pane did not take the input: its process has exited, or its input queue is full. Nothing was sent |
| `-32004` | A rule refused the call; `error.data.reason` names it. See [Refusals](#refusals) |
| `-32005` | A person is being asked whether the caller may write into the target; `error.data.reason` is `asked_person`. Nothing was written |
| `-32000` | Busy, too many connections or subscriptions, or shutting down |

Some older failures (for example `workspace.select` out of range) come back as
`{"error": "<message>"}`; the server converts those into error envelopes too.

## Events

`events.subscribe` keeps the connection open and writes one JSON object per
line. `splitlane watch` prints the same lines. Omitting `surfaces` or `types`
means all.

| `type` | Fields | Sent when |
| --- | --- | --- |
| `subscribed` | `id` | First frame |
| `ai.session_start` | `workspace_id, pid, tool, state, surface_id, message, active_tool_name, ts` | An agent session starts |
| `ai.prompt_submit` | same | A prompt is submitted |
| `ai.tool_use` | same | The agent starts or finishes a tool call |
| `ai.notification` | same | The agent asks for input or attention |
| `ai.stop` | same | A turn ends |
| `ai.exit` | same | The agent process exits |
| `ai.session_end` | same | The session ends |
| `surface_changed` | `surface_id, output_generation, ts` | A surface's `output_generation` advanced (checked every 50 ms) |
| `surface.rail` | `surface_id, thread_id, status, source, runs_ended, last_outcome, ts` | A surface's `rail.status` or `rail.runs_ended` changed. Also sent once for each surface when it is first seen |
| `heartbeat` | | 30 s without other frames |
| `dropped` | `count` | The subscriber fell behind and `count` events were discarded |

`ts` is milliseconds since the Unix epoch. `types` accepts the seven `ai.*`
names, `surface_changed` and `surface.rail`; anything else is rejected with
`-32602`. `thread_id` is the internal id of the surface's row in the rail,
or `null` when it has none. Each
subscriber queues up to 1024 events. After `dropped`, re-read state with
`splitlane ps --json` or `splitlane status <target> --json`.
