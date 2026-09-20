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
| `new` | `--name NAME`, `--cwd DIR` | `workspace.create` | JSON |
| `select <index>` | | `workspace.select` | JSON |
| `split <h\|horizontal\|v\|vertical>` | `--target SEL` | `surface.split` | JSON |
| `focus <target>` | | `surface.focus` | JSON |
| `send <target> <text>` | `--submit`, `--paste`, `--broadcast`, `--report-file PATH` | `surface.send_text` | JSON |
| `key <target> <keystroke>` | | `surface.send_keystroke` | JSON |
| `wait` | `--match SEL` (required), `--pattern REGEX`, `--idle`, `--for MS`, `--timeout SECS`, `--any`, `--all` | `surface.read`, `events.subscribe` | JSON |
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
  `start_reason`.
- `wait --pattern` polls every 500 ms over the last 500 lines and matches only
  output that appeared after `wait` started. Prints
  `{matched, panes, matches: [{surface_id, lines}]}`. `--any` and `--all`
  accept a selector matching several surfaces; without them it must match one.
  If every watched surface closes, exits `1`.
- `wait --idle` returns once the surface prints nothing for `--for` ms (default
  1000). With `--pattern` as well, whichever happens first returns. Prints
  `{surface_id, idle, matched}`. Single target only; `--any`/`--all` are
  ignored.
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
| `1` | Runtime failure: instance unreachable, method refused by a gate, handler error, surface closed, flow failed or aborted |
| `2` | Usage error, including an unknown verb |
| `3` | Target not found or ambiguous. `watch` and `wait --idle` also use `3` when they cannot open the event stream |
| `4` | `wait` timed out; a flow `ready` barrier timed out |
| `130` | `wait --idle` interrupted with `Ctrl-C` |

## Gates

The environment variables are read from the Splitlane process.

| Operation | Allowed when |
| --- | --- |
| Reads: `ls`, `read`, `search`, `ps`, `status`, `watch`, `wait` | Always |
| `new`, `select`, `focus`, `split` without spawn fields | Always |
| `send`, `key` (`surface.send_text`, `surface.send_keystroke`) | `SPLITLANE_IPC_SCRIPTING=1` or `ai_unrestricted: true` |
| `up`, `surface.split`, `workspace.up` with `command`, `prompt`, `context` or `env` | `SPLITLANE_IPC_ORCHESTRATION=1` or `SPLITLANE_IPC_SCRIPTING=1` |
| `flow run` | `SPLITLANE_IPC_ORCHESTRATION=1` or `SPLITLANE_IPC_SCRIPTING=1` |
| A flow with any `submit = true` | `SPLITLANE_IPC_SCRIPTING=1` (checked before the flow starts, also with `--dry-run` when the instance is reachable) |

Only the value `1` enables a variable. A refused call returns JSON-RPC error
`-32601` with a message naming the variable.

Always refused: a `surface.send_keystroke` whose keystroke contains CR or LF or
would produce one (`enter`, `ctrl-m`, `ctrl-j`), and `surface.send_text` longer
than 64 KiB.

### Related settings

| `splitlane.json` key | Default | Effect |
| --- | --- | --- |
| `ai_unrestricted` | `false` | Opens `send`/`key` without the environment variable. Settings -> Agents -> Free access |
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

`state` values: `thinking`, `waiting_for_input`, `finished`, `errored`,
`stalled`; `fleet.list` adds `unknown_running` for agents seen only by the
process scan. `surface.status` on a surface with no tracked agent returns
`{surface_id, state: "idle", hooked: false, output_generation}`.

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
| `worktree_teardown` | string | `"auto"` | `auto` removes the worktree on close when clean; `keep` leaves it. Needs `worktree` |

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

| Method | Params | Result |
| --- | --- | --- |
| `system.ping` | | `{pong: true}` |
| `system.capabilities` | | `{scripting, orchestration, methods}` |
| `system.identify` | | `{name, version, protocol}` |
| `workspace.list` | | `{workspaces: [{index, title, cwd, panes, active}]}` |
| `workspace.current` | | `{index, title, cwd, panes, layout}` |
| `workspace.create` | `name` (default `"Terminal"`), `cwd`, `layout` (layout tree) | `{index, title, panes}` |
| `workspace.select` | `index` | `{selected}` |
| `workspace.close` | `index` (default active) | `{closed}`; refuses to close the last project |
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
| `heartbeat` | | 30 s without other frames |
| `dropped` | `count` | The subscriber fell behind and `count` events were discarded |

`ts` is milliseconds since the Unix epoch. `types` accepts the seven `ai.*`
names and `surface_changed`; anything else is rejected with `-32602`. Each
subscriber queues up to 1024 events. After `dropped`, re-read state with
`splitlane ps --json` or `splitlane status <target> --json`.
