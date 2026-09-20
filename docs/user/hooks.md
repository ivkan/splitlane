# Agent hooks

Coding agents running in Splitlane report what they are doing - a prompt was
submitted, a tool started, the turn ended, the agent is asking something - by
calling a small program, `splitlane-ai-hook`, from their own hook mechanism. It
reads the event, sends one `ai.*` message to the running Splitlane over the
local socket, and always exits `0`, so an agent is never broken by Splitlane
being closed.

Those events feed:

- `splitlane ps`, `splitlane status` and `splitlane watch` (see
  [Scripting](scripting.md));
- desktop notifications;
- the status on the rail for agents other than Claude Code and Codex. For those
  two, Splitlane reads the session's own transcript and process to decide the
  status; a hook event only makes it re-check sooner.

## How hooks get installed

Every pane has Splitlane's agent wrapper directory at the front of `PATH`. When
you start one of the sixteen supported agent CLIs in a pane, the wrapper finds
the real binary, writes that agent's hook configuration, runs the agent, and
removes the configuration again when the agent exits. Nothing is written when
the wrapper cannot reach a Splitlane socket, and a configuration left behind by
a wrapper that was killed is cleaned up on the next launch.

For Claude Code you can also install the hooks once, in your user settings:

```bash
splitlane hooks setup       # install persistent hooks for Claude Code
splitlane hooks status      # report what is installed
splitlane hooks uninstall   # remove only Splitlane's entries
```

## Which agents report events

| Agent | Hook configuration the wrapper writes for the session | Events |
| --- | --- | --- |
| Claude Code | `./.claude/settings.local.json` | UserPromptSubmit, Notification, Stop, PreToolUse, PostToolUse |
| Codex | `./.codex/hooks.json`; on Linux and macOS also `hooks = true` under `[features]` in `~/.codex/config.toml` unless already set | SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop |
| CodeBuddy | `./.codebuddy/settings.local.json` | UserPromptSubmit, Notification, Stop, PreToolUse, PostToolUse |
| Qoder | `./.qoder/settings.local.json` | UserPromptSubmit, PreToolUse, PostToolUse, Stop |
| Gemini CLI | `~/.gemini/settings.json` | BeforeAgent, AfterAgent, BeforeTool, AfterTool |
| Cursor (`cursor-agent`) | `~/.cursor/hooks.json` | beforeSubmitPrompt, stop, preToolUse, postToolUse |
| OpenCode | `<config>/opencode/plugins/splitlane-status.ts`, listed in `opencode.json` (`<config>` is `$XDG_CONFIG_HOME` or `~/.config`) | chat.message, tool.execute.before, tool.execute.after, session.idle, permission.asked |
| Pi | `~/.pi/agent/extensions/splitlane-status.ts` | agent_start, agent_end, tool_execution_start, tool_execution_end |
| Hermes | A marked `hooks:` block appended to `~/.hermes/config.yaml` | pre_llm_call, post_llm_call, pre_tool_call, post_tool_call |
| Grok | `~/.grok/hooks/splitlane.json`, a file owned entirely by Splitlane | UserPromptSubmit, PreToolUse, PostToolUse, Stop |

Paths starting with `./` are relative to the directory the agent was started
in. On Windows, Codex in interactive mode uses `./.codex/hooks.json` without the
feature flag, and `codex exec` is followed through its JSON output instead.

Amp, Kiro, Antigravity, Copilot CLI, Factory Droid and Openclaw get no hooks.
They still run in panes and the wrapper still reports when they exit, but
there are no turn events; when Splitlane's process scan spots one,
`splitlane ps` lists it as `unknown_running` with `reason: "no_hook"`.

The wrapper leaves your own entries alone. It recognises its own entries by the
`splitlane-ai-hook` program name, and it skips an agent rather than overwrite a
configuration it cannot safely edit: a symlinked configuration directory, a
user-level JSON file that does not parse, an `~/.hermes/config.yaml` that
already has a top-level `hooks:` key, or an OpenCode setup that only has
`opencode.jsonc`. The OpenCode and Pi plugins do nothing unless
`SPLITLANE_SOCKET_PATH` is set, so they are inert outside Splitlane.

## Persistent Claude Code hooks

`splitlane hooks setup` adds Splitlane's entries to Claude Code's user settings,
`~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR/settings.json` when that
variable is set). They call `splitlane-ai-hook` at a fixed path that survives
rebuilds:

| Platform | Path |
| --- | --- |
| Linux | `~/.local/share/splitlane/bin/splitlane-ai-hook` |
| macOS | `~/Library/Application Support/splitlane/bin/splitlane-ai-hook` |
| Windows | `%LOCALAPPDATA%\splitlane\bin\splitlane-ai-hook.exe`, called through PowerShell |

A debug build uses `splitlane-dev` in place of `splitlane` in these paths.

The command extracts the program to that path first. Entries are marked with
`_splitlane_managed`; the previous file is copied to `settings.json.bak` before
a change, writes are atomic, and a settings file that is not valid JSON is left
untouched.

When working persistent hooks are present, the wrapper does not write
`./.claude/settings.local.json` and removes one it left earlier, so each event
is reported once. If the persistent entries point at a program that cannot be
run, the wrapper ignores them and installs the per-session hooks instead.
`splitlane hooks status` reports `installed`, `stale` (the entries point at a
different path; run `setup` again), `needs repair` or `not installed`.

Only Claude Code has a persistent install. For every other agent the
per-session hooks above are the only mechanism.

Exit codes: `0` on success, including "Claude Code not detected"; `1` when an
install, removal or status check failed; `2` for a missing or unknown
subcommand.

## Exit and interrupt handling

The wrapper waits for the agent instead of replacing itself with it, which is
what lets it report two things the agents do not report themselves:

- **Exit.** When the agent process ends, the wrapper sends `ai.exit` with the
  real exit code and then `ai.session_end`, so a session that was mid-turn does
  not stay "running".
- **Ctrl+C.** Interrupting a turn does not end the agent or fire its `Stop`
  hook. The wrapper sends `ai.stop` for each Ctrl+C, while the agent itself
  still receives the interrupt.

It also keeps an agent from outliving Splitlane. On Linux the agent is started
with a parent-death signal, so the kernel kills it when the wrapper's parent
dies. On macOS the wrapper watches for its parent to disappear and kills the
agent.

## Troubleshooting

Set `SPLITLANE_HOOK_LOG` to a file path before starting Splitlane. The app, the
wrapper and `splitlane-ai-hook` each append a line per step, which shows whether
hook configuration was written for an agent and where the chain stopped.
