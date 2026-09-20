# Splitlane Architecture

Splitlane is a native GPU-accelerated terminal workspace for running CLI coding
agents in parallel. One user-facing Rust binary, no web runtime: the UI is
built on [Zed's GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui),
consumed from the upstream Zed repository at one pinned revision;
terminal emulation is provided by a pinned, statically linked
`libghostty-vt` backend by default on Linux and Windows x64 MSVC. Upstream
[`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) remains the
macOS backend and the explicit cross-platform rollback. Splitlane owns backend
selection, PTY lifecycle orchestration, rendering, and integration with agent
tracking, IPC, the MCP bridge, and self-update.

This document describes how the pieces fit together. It is aimed at
contributors and at anyone curious how you build a multiplexing terminal app
without Electron.

## Workspace layout

The repo is a Cargo workspace with one binary crate and a set of small,
focused library crates:

| Crate | Path | Purpose |
|---|---|---|
| `splitlane-app` | `src-app/` | The GPUI application and `splitlane` CLI entrypoint: UI, panes, PTY sessions, IPC server, self-update |
| `splitlane-libghostty-sys` | `crates/splitlane-libghostty-sys/` | Raw Ghostty ABI plus verification and linking of the pinned static archive |
| `splitlane-terminal-ghostty` | `crates/splitlane-terminal-ghostty/` | Safe Rust interface over Ghostty terminal state, input, search, selection, and owned render snapshots |
| `splitlane-ghostty-smoke` | `crates/splitlane-ghostty-smoke/` | Package-level native smoke binary for Ghostty, PTY I/O, resize, and shutdown verification |
| `splitlane-config` | `crates/splitlane-config/` | Config schema, tolerant JSON loader, file watcher |
| `splitlane-shim` | `crates/splitlane-shim/` | PATH shim wrapping 16 known agent CLIs so Splitlane can observe their lifecycle |
| `splitlane-ai-hook` | `crates/splitlane-ai-hook/` | The hook binary agent CLIs invoke to report session events back over IPC |
| `splitlane-ipc-client` | `crates/splitlane-ipc-client/` | Blocking JSON-RPC client for the local IPC socket (shared by the MCP bridge and the CLI) |
| `splitlane-mcp` | `crates/splitlane-mcp/` | Stdio MCP server exposing read-only pane access (`list_panes`, `read_pane`, `search_pane`) |
| `splitlane-mcp-install` | `crates/splitlane-mcp-install/` | GPU-free install engine for the MCP bridge: per-agent detection, idempotent config merge, backup + atomic write |
| `splitlane-process` | `crates/splitlane-process/` | Bounded external-process execution (wall-clock deadline + stdout cap) shared across crates |
| `splitlane-acp` | `crates/splitlane-acp/` | Legacy Claude/Codex identity enum plus the `CLAUDECODE` environment scrub |
| `splitlane-telemetry` | `crates/splitlane-telemetry/` | Opt-in telemetry plumbing (no event leaves the machine unless consent resolves to `true`) |

`src-app` is the default workspace member, so bare `cargo run` starts the
desktop app instead of becoming ambiguous across helper binaries. The split is
deliberate: anything that runs *outside* the GUI process (shim, hook, MCP
bridge, MCP installer logic) must stay GPU-free and tiny, so it lives in its
own crate and never links GPUI.

## Thread model

```
┌─────────────────────────────────────────────────────────┐
│ Main thread - GPUI event loop                           │
│   owns all Entity state, rendering, input dispatch      │
└─────────────────────────────────────────────────────────┘
        ▲                    ▲                    ▲
        │ Backend events     │ mpsc (50ms poll)   │ channel
┌───────┴────────┐  ┌────────┴───────┐  ┌─────────┴────────┐
│ Terminal       │  │ IPC thread     │  │ Watcher threads  │
│ workers        │  │ JSON-RPC 2.0   │  │ config, theme,   │
│ Ghostty or     │  │ socket server  │  │ git state        │
│ Alacritty      │  │                │  │                  │
└────────────────┘  └────────────────┘  └──────────────────┘
```

- **Main thread**: the GPUI event loop. All UI state lives in `Entity<T>`
  values mutated through GPUI contexts; there are no locks around UI state.
- **Terminal workers**: the backend is selected before the shell child is
  created. Each Ghostty session owns a `splitlane-ghostty-runtime` worker and a
  PTY reader; Windows also uses a dedicated ConPTY closer so pipe drainage
  cannot block teardown. Alacritty sessions retain their `EventLoop` I/O
  thread and shared terminal grid. Both implementations publish
  backend-neutral events to the view and owned render snapshots through
  `TerminalSessionBackend`.
- **IPC thread**: accepts connections on a Unix socket (Linux/macOS) or named
  pipe (Windows). Stateless methods reply in place; stateful methods are
  dispatched to the main thread through a bounded channel and drained by the
  50 ms app poll loop.
- Blocking work (git subprocesses, filesystem walks, fleet-wide search) is
  pushed to background executors - registering a recursive file watcher or
  scanning a monorepo on the render thread is how you get a
  "not responding" window, so the codebase treats the main thread as
  render-only.

## Keystroke → pixel

The full input/output pipeline, end to end:

```
KeyDownEvent
  → TerminalView::handle_key_down()
  → Ghostty structured input or Alacritty escape-sequence input
  → selected backend writer → PTY → shell / agent CLI
  → output bytes → libghostty-vt engine or Alacritty VTE / Term grid
  → TerminalBackendEvent → sync() → cx.notify()
  → TerminalSessionBackend::render_content() → owned neutral Content
  → TerminalElement::prepaint()
  → TerminalElement::paint()     - quads + shaped glyph runs
  → GPU (Vulkan on Linux, Metal on macOS, DirectX on Windows)
```

The first Ghostty wakeup on Linux can render immediately. Windows Ghostty and
Alacritty wakeups are coalesced into the 4 ms event batch. This keeps the
renderer backend-independent without imposing the same scheduling policy on
different PTY implementations.

`TerminalElement` (`src-app/src/terminal/element/`) is the one place Splitlane
implements GPUI's low-level `Element` trait directly instead of composing
divs: terminal rendering wants per-cell control over background quads, glyph
runs, cursor shapes, underlines and hyperlink hitboxes. Everything else in the
app (rail, pane headers, settings, diff viewer) is regular GPUI flex layout.

Debug builds can trace the whole pipeline: `SPLITLANE_LATENCY_PROBE=1` stamps a
keystroke at ingress and reports time-to-pixel.

## Dual terminal engines behind one boundary

`TerminalSessionBackend` is the renderer-facing facade for both engines.
Standard Linux builds and supported Windows x64 MSVC builds resolve
`terminal.backend = auto` to Ghostty. macOS, builds without a verified native
Ghostty feature, and explicit rollback sessions use upstream
`alacritty_terminal`. The choice applies to new sessions only.

Ghostty's raw ABI and static archive linking live in
`splitlane-libghostty-sys`; `splitlane-terminal-ghostty` exposes the safe Rust
interface. Alacritty imports remain confined to an explicit allowlist. Neither
engine leaks borrowed terminal state into GPUI: the rest of the app consumes
Splitlane-owned points, mode flags, cells, events, and `Content` snapshots.

A Ghostty startup failure may fall back to Alacritty only before the shell
child exists. Once a child has been spawned, Splitlane never starts a second
child or switches the live session to another engine.

## Agent lifecycle tracking

The feature that makes Splitlane more than a tiling terminal: it knows what
the agents inside its panes are doing.

Each agent session is a PTY in a pane, and its status - `starting`,
`running`, `waiting for you`, `idle` or `failed` - comes from the best source
that agent offers:

```
Claude Code, Codex
  └─ the agent's own session records (status file, transcript, rollout)
     + the process tree under the pane
       └─ re-read every 2 s, and at once when a hook fires

other agents with hooks (Gemini, Cursor, OpenCode, …)
  └─ launched through a PATH shim (splitlane-shim)
       └─ agent hooks fire splitlane-ai-hook on lifecycle events
            └─ ai.* JSON-RPC notifications over the local socket

agents with no hook surface
  └─ process start and exit only
```

- **Records first.** For Claude Code and Codex the app reads what the agent
  itself wrote - a pending permission prompt, an unfinished tool call, a
  closed turn - and asks the process tree whether a tool is still executing.
  Terminal output is never used to guess state. The rule lives in
  `src-app/src/agent_state.rs` and has no I/O, so it is unit-tested directly.
- **Shim and hooks.** Launching an agent from Splitlane puts a shim directory
  first in `PATH`. The shim records the real PID and process start time (so
  PID reuse cannot confuse it), then execs the real binary. Agents that support
  lifecycle hooks report `session_start`, `prompt_submit`, `tool_use`,
  `notification`, `stop`, `exit` and `session_end` through the `ai.*` IPC
  namespace.
- **Everything is observable.** The same state drives the rail, pane headers,
  the title bar's attention chip and desktop notifications, and is available
  to your own tooling over IPC.

Splitlane never sends a prompt on its own. Text reaches an agent when you type
it, or through the scripting path, which is off until you enable it.

## IPC and the MCP bridge

A JSON-RPC 2.0 endpoint (Unix socket at `$XDG_RUNTIME_DIR/splitlane/`, named
pipe on Windows) exposes `workspace.*`, `surface.*`, `fleet.*`, `events.*`,
and `ai.*` namespaces - enough to script workspace creation, read panes, send
text behind the scripting gate, and subscribe to agent events. The `splitlane`
CLI (`splitlane up`, `splitlane flow`, `splitlane watch`, `splitlane wait`) is
built on the same socket.

The MCP bridge re-exposes a read-only slice of this to agents themselves:
`splitlane mcp install` registers a stdio MCP server with Claude Code, Codex,
Gemini CLI and opencode, giving any agent the ability to *read* (never write)
other panes' scrollback. An agent debugging a failing dev server can read the
server pane's output directly instead of asking you to paste it. The bridge
binary ships embedded in the main binary and is extracted to a stable path at
launch, so there is nothing extra to install.

Ingress is treated as untrusted: session and config files are validated
structurally (layout budgets, ratio clamps, id alphabets) before they touch
app state.

## Self-update

Each install format has its own update path (AppImage swap, tarball swap,
package download, macOS app replacement, Windows MSI relay), all driven by one
in-app updater. Update artifacts are verified with
[minisign](https://jedisct1.github.io/minisign/) signatures and the client
**fails closed**: an unsigned or tampered artifact is rejected, never installed.
macOS builds add Developer ID / notarization checks with Team ID pinning;
Windows MSI updates add `WinVerifyTrust` before `msiexec` runs.

## Telemetry (opt-in, fail-closed)

Telemetry is **disabled by default**, and nothing in the app prompts for it: no
event is sent unless `telemetry.enabled` is set to `true` in `splitlane.json`. `SPLITLANE_NO_TELEMETRY=1`,
`DO_NOT_TRACK`, or `NO_TELEMETRY` override everything unconditionally. The full
client lives in `crates/splitlane-telemetry/`; app-level emitters live in
`src-app/src/app/telemetry_events.rs`. The event surface covers app lifecycle,
update funnel, telemetry re-enable, and session-corruption events, with no
terminal content, no paths, and no prompts.

## Cross-platform strategy

One codebase, three first-class targets. Platform-specific code is gated
behind `#[cfg(target_os)]` with a working path (or a documented stub) for the
other two platforms:

| Concern | Linux | macOS | Windows |
|---|---|---|---|
| GPU | Vulkan | Metal | DirectX |
| Windowing | Wayland + X11 | AppKit | Win32 |
| Terminal engine | `libghostty-vt` by default, Alacritty rollback | Alacritty | `libghostty-vt` by default on x64 MSVC, Alacritty rollback |
| PTY | `portable-pty` for Ghostty, `alacritty_terminal::tty` for rollback | `alacritty_terminal::tty` | ConPTY via `portable-pty` for Ghostty, `alacritty_terminal::tty` for rollback |
| IPC | Unix socket | Unix socket | Named pipe |
| Packaging (planned) | `.deb` / `.rpm` / AppImage / tarball | signed + notarized `.dmg` | signed `.msi` |

There are no release builds yet; the packaging above is what the release
workflow produces once signing identities exist. Linux, macOS Apple Silicon and
Windows x64 are built and tested in CI; macOS Intel and Windows ARM64 are not.
See [`README.md`](README.md#install) and [`docs/WINDOWS.md`](docs/WINDOWS.md)
for the support matrix.

## Performance discipline

Performance work is measured rather than assumed: heaptrack diffs for memory
work, `cargo flamegraph` for CPU work, and a keystroke-latency probe in debug
builds. The render thread never does blocking I/O; scans and searches that
touch the filesystem or many panes run on background executors and report
back through events.

## Building

```bash
cargo build --release    # LTO thin, strip, codegen-units=1
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See the [README](README.md#build-from-source) for per-platform build
instructions and system dependencies.
