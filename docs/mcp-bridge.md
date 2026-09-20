# MCP bridge (`splitlane-mcp`)

Let an MCP-capable CLI agent running in a Splitlane pane read the terminal
output of other panes, so you can say *"check the logs in the cargo-run pane"*
instead of selecting, copying and pasting by hand.

`splitlane-mcp` is a small stdio [MCP](https://modelcontextprotocol.io) server.
The agent starts it as a subprocess, and it forwards each call to Splitlane's
local JSON-RPC socket. It is **read-only**: it can list, read and search
surfaces, and has no way to type into or control them.

Source: `crates/splitlane-mcp/`.

## Tools

| Tool | Arguments | Returns |
|------|-----------|---------|
| `list_panes` | none | The surfaces in scope (`surface_id`, `name`, `title`, `cwd`, `cmd`, `workspace`, `workspace_id`, `thread_id`, `scope`) and the bridge's scope. Call this first to find what to read. |
| `read_pane` | `target`, `lines?` (default 200, max 4000), `offset?` (lines back from the end) | The surface's scrollback as text. |
| `search_pane` | `target`, `pattern`, `max_matches?` (default 50, max 1000) | Matching lines with their line numbers. The match is a case-insensitive substring, not a regex. |

`target` is either a JSON number, taken as the `surface_id`, or a string, taken
as a name and matched exactly, then case-insensitively, then as a unique prefix.
A name that matches several surfaces, or none, returns an error listing the
candidates. A digit string such as `"7"` is a name, not an id.

The bridge also offers each surface as an MCP resource,
`pane://surface/{surface_id}/content`, for agents that read resources.

> **Security.** Everything the bridge returns, including pane names and titles,
> is wrapped in an `<untrusted_terminal_output>` block with a per-call id, and
> the server instructions tell the agent to treat it as data. A pane may show
> attacker-controlled text (a server logging a crafted string, a hostile
> repository). There is no write or keystroke tool by design.

## Scope

When the bridge's environment has `SPLITLANE_WORKSPACE_ID` (every pane does),
it only returns surfaces of the project that pane belongs to. The variable
holds an id, not a position: a shell carries its project's id, and an agent
carries the id of its own session record, which the bridge maps to the project.
Each surface reports both as `workspace_id` and `thread_id`; `workspace` is
the project's position in the sidebar and plays no part in the check.

Set `SPLITLANE_MCP_SCOPE=all` (or `global`) in the agent's environment when an
agent should read every surface in the instance, across projects.

## Install

The bridge binary is built into `splitlane`. Each time Splitlane starts, and when
you run `splitlane mcp install`, it is written to a fixed path that does not
change when you rebuild:

| Platform | Path |
| --- | --- |
| Linux | `~/.local/share/splitlane/bin/splitlane-mcp` |
| macOS | `~/Library/Application Support/splitlane/bin/splitlane-mcp` |
| Windows | `%LOCALAPPDATA%\splitlane\bin\splitlane-mcp.exe` |

A debug build uses `splitlane-dev` in place of `splitlane` in these paths.

To register the bridge with every supported agent found on your machine:

```bash
splitlane mcp install
```

It looks for Claude Code, Codex, Gemini CLI and opencode, writes a `splitlane`
entry into each one it finds, and reports per agent:

```text
claude-code: installed (/home/you/.local/share/splitlane/bin/splitlane-mcp)
codex: installed (/home/you/.local/share/splitlane/bin/splitlane-mcp)
gemini: skipped (not detected)
opencode: skipped (not detected)
```

Running it again changes nothing when the entries are already current. It only
touches the `splitlane` entry, leaving other MCP servers and settings alone,
and copies a file it edits to `<file>.bak` first. The same command is available
in Settings -> MCP.

```bash
splitlane mcp status      # report the state per agent; writes nothing
splitlane mcp uninstall   # remove the splitlane entry from every agent
```

`status` reports one of: *not detected*, *installed*, *detected but not
installed*, *stale path* (the entry points somewhere else; run `install` again),
or *needs repair* (the entry exists but does not have the expected shape, for
example it is disabled).

Exit codes: `0` on success, including when no agent is detected; `1` when an
agent failed or the bridge binary is missing; `2` for a missing, unknown or
extra argument.

Where each entry goes:

| Agent | File | Entry |
| --- | --- | --- |
| Claude Code | `~/.claude.json` | `mcpServers.splitlane`. Uses `claude mcp add -s user` when `claude` is on `PATH`, otherwise edits the file |
| Codex | `$CODEX_HOME/config.toml`, or `~/.codex/config.toml` | `[mcp_servers.splitlane]`. Uses `codex mcp add` when `codex` is on `PATH`, otherwise edits the file |
| Gemini CLI | `~/.gemini/settings.json` | `mcpServers.splitlane` with `trust: true` |
| opencode | `$OPENCODE_CONFIG`; otherwise `opencode.jsonc` or `opencode.json` in `$OPENCODE_CONFIG_DIR` or the global opencode config directory | `mcp.splitlane` |

## Manual configuration

`splitlane mcp install` is the recommended path. To wire it by hand, point the
agent at the extracted binary above, or build one with
`cargo build -p splitlane-mcp --release` (`target/release/splitlane-mcp`) and use
that absolute path.

> Agent config formats change often. The shapes below are the ones
> `splitlane mcp install` writes; check them against your agent's current
> documentation.

The bridge finds Splitlane through `SPLITLANE_SOCKET_PATH`, which every pane
sets, falling back to the default socket location. The entries carry no
environment block, so start the agent from a Splitlane pane.

### Claude Code

```bash
claude mcp add -s user --transport stdio splitlane -- /absolute/path/to/splitlane-mcp
```

Or in `~/.claude.json` under `mcpServers.splitlane`:
`{"type": "stdio", "command": "/absolute/path/to/splitlane-mcp", "args": []}`.

### Codex CLI

`$CODEX_HOME/config.toml` when `CODEX_HOME` is set, otherwise
`~/.codex/config.toml`:

```toml
[mcp_servers.splitlane]
command = "/absolute/path/to/splitlane-mcp"
args = []
```

### Gemini CLI

`~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "splitlane": {
      "command": "/absolute/path/to/splitlane-mcp",
      "args": [],
      "trust": true
    }
  }
}
```

`trust: true` skips Gemini's confirmation prompt for each call. Set it to
`false` if you want to confirm each read.

### opencode

`opencode.jsonc` or `opencode.json` in opencode's global config directory (or
the file named by `OPENCODE_CONFIG`). The key is `mcp`, not `mcpServers`, and
`command` is an array:

```json
{
  "mcp": {
    "splitlane": {
      "type": "local",
      "command": ["/absolute/path/to/splitlane-mcp"],
      "enabled": true
    }
  }
}
```

## Example

In an agent running inside Splitlane:

> *"List my panes, then read the last 100 lines of the cargo-run pane and tell
> me why the build failed."*

The agent calls `list_panes`, sees a surface named `cargo-run`, then
`read_pane(target="cargo-run", lines=100)`.

For the command-line equivalents (`splitlane ls`, `read`, `search`) see
[Scripting](user/scripting.md).
