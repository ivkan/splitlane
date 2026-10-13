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
| `splitlane-host` | `crates/splitlane-host/` | The session host: owns PTYs and their output logs outside the window process and answers their terminal queries, plus the client that talks to it |
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

## The session host

A terminal session today lives in the window's process: quit the app, update
it or crash it, and every agent running in a pane ends with it. The session
host is the process meant to take that ownership over. **By default the window
does not use it:** every pane owns its PTY as described above. On macOS a
window started with `SPLITLANE_SESSION_HOST=1` opens its panes in the host
instead, which is described under [A pane in the host](#a-pane-in-the-host).
That switch moves where a pane's PTY lives and nothing else yet: quitting the
window still ends its sessions.

The host is the app binary started as `splitlane __splitlane-host`. It leaves
`main` before anything of the app is initialised and never loads GPUI; the
code is the `splitlane-host` crate, which depends on neither. It is Unix-only
for now. On Windows the crate compiles and its entry points report that the
host is not supported there yet.

**Socket.** The host listens on `splitlane-host.sock` (`splitlane-dev-host.sock`
for a debug build) in the directory of the instance's IPC socket. The
directory is mode 0700, the socket 0600, and each connection's peer user id is
checked on top. The file name is fixed per build profile and not derived from
the IPC socket's name, so a debug build run inside a release's pane does not
end up holding the release's sessions. A host keeps an exclusive lock on
`<socket>.lock` while it runs: when several callers start a host at once, one
takes the socket and the rest exit.

**Sessions.** A session is one PTY and the child started in it, as a session
leader in its own process group. The child gets exactly the environment the
client sent and nothing from the host's own, because the host may have been
started by another release or from another terminal. For each session the host
starts the same watcher process the app starts for the PTYs it owns, keyed on
the host's pid, so a host that is killed outright still takes its children
with it.

**Attaching is replaying a log.** The host does not hand out a screen. It holds
each session's raw output in a bounded ring (8 MiB by default), and a client
that attaches asks for that log from a position, feeds it through its own
terminal emulator, and then receives new bytes as they arrive. Output and
resizes share one sequence: one number per byte, one per resize record, so a
resize is replayed exactly between the bytes it fell between. A client that
follows a session is nothing but a position in the log. The thread reading the
PTY never waits for a client and queues nothing for one; a client that falls
behind the ring is told which range it lost and continues from the oldest
byte still held. When all logs together exceed their shared limit, the largest
one is cut first.

**The ring is cut only where the output can be.** Every byte passes through a
small scanner on its way into the log. It follows the grammar all terminal
emulators share - escape sequences, strings, UTF-8 - without being one, and
knows two things at any point: whether the stream may be cut there, and which
terminal modes are in effect. The head of the ring only moves to a point that
is outside every escape sequence, every multi-byte character and every
synchronized update, so a replay never starts with half of one. Output that
offers no such point for longer than the ring - one enormous string, an update
that is never closed - is let grow to twice the ring's size and then cut
anyway; the client is told that its stream starts torn.

**A late client gets the modes, and asks for the picture.** A program switches
things on once and expects them to stay: the alternate screen, bracketed
paste, mouse reporting, application cursor keys, the keyboard protocol, the
cursor's shape, the title. A replay that starts in the middle of the ring has
missed all of it. So whenever a stream starts later than its client asked, at
attach or after the client fell behind, the host first sends a *preamble*:
bytes that put any emulator into the modes in effect at that point, with an
explicit set or reset for every one of them so that nothing depends on the
state the emulator was in. The host does not try to reconstruct the screen.
The client replays what the ring still holds and then asks the host to
`redraw`, which resizes the terminal by one row and back; a full-screen
program repaints on that as it would for any resize. Both steps are ordinary
resizes, recorded in the log.

**The host answers the program.** Programs ask their terminal questions and
wait: where the cursor is, what the terminal supports, what its colours are.
Some ask on every resize. With no window attached there would be nobody to
answer, so the host always answers, from an emulator of its own that follows
each session's output; a window's emulator only draws. The answers must not
differ from what the window would have said - a program would otherwise be
told one thing at start-up and find another once a window shows it - so the
host uses, on each platform, the emulator the window uses there, built with
the same settings. On macOS that is `alacritty_terminal`, and a test in the
app asks a pane's terminal and the host's the same questions and compares.
What only a window can know, its colours and the size of a cell in pixels, a
client tells the host; until one has, those questions go unanswered, as in a
terminal that does not support them. Answers go to the program without ever
holding up the thread that reads its output: one the program is not reading
its input for is dropped.

One question is answered without the emulator, on every platform: whether the
terminal reads the program's status reports (OSC 7501). Agents that report
their own state ask it at start-up and stay silent when nobody answers. The
host finds the question in the output itself and puts its answer ahead of
anything the emulator says to the same bytes, because the program reads the
first reply it gets as the verdict.

Still open:

- **Linux has no answering emulator yet.** The window there uses libghostty,
  the host does not link it, and so on Linux the host answers no queries. The
  emulator sits behind a small trait; adding one touches nothing else.
- **The picture comes from the program.** After a trim, what is on screen is
  whatever the program repaints. A program that does not repaint on a resize
  shows only what it prints from then on, and scrollback older than the ring
  is gone for good.
- **Only the modes listed above are restored.** A scroll region, tab stops or
  a character set that a program set once, before the trim, are not.

**The wire contract has its own version.** Sessions are supposed to survive an
update, so a host and the window attached to it are routinely different
releases, and tying the protocol to the release version would refuse exactly
that pairing. Each side instead states the contract number it speaks and the
oldest it accepts, and they talk when each one's number is at least the
other's minimum. Frames are length-prefixed; control frames are JSON whose
readers ignore unknown fields and default missing ones, and output and input
travel as binary frames. New fields, new requests and new skippable events are
therefore added without cutting anybody off; the rules are in the module
documentation of `crates/splitlane-host/src/protocol.rs`.

**Lifetime.** The host exits when asked to, on SIGTERM or SIGINT, and by itself
after ten seconds with no running session and no connected client. A session
that has exited stays listed with its log until a client forgets it, but does
not keep the host alive once nobody is connected. Its log goes to `host.log`
in the app's cache directory.

### A pane in the host

On macOS, a window started with `SPLITLANE_SESSION_HOST=1` opens every new
pane in the host. The variable is read once, at start; it is an environment
variable and not a setting because the path is new and a setting is a promise
to keep. Linux and Windows ignore it: the host has no answering emulator
there.

The emulator, the view and the renderer of such a pane are the ones every
pane uses. What differs is the two ends of the byte stream:

- **The shell is the host's child.** The window asks the host to start the
  shell it would have started itself: the same argv, the same directory, and
  the same environment sent whole - a snapshot of the window's own, with what
  the emulator's spawn adds and removes, and the pane's variables on top. A
  test runs `env` in both kinds of pane and compares.
- **Output arrives as stream events** and is fed through the same scanners and
  the same parser, in the same order, as output read from a PTY. The child's
  end arrives as the events a local child's end produces.
- **The window's emulator does not answer the program.** It sees the same
  questions in the stream, and its replies are dropped where it emits them,
  because the host has given them. What only the window knows - its colors and
  the size of a cell - it tells the host when a pane is opened and again when
  the theme, the cursor color or the cell size changes.
- **Input and resizes are queued** for a thread of the pane's own, because a
  request to the host waits for its reply and the thread that draws the window
  must not. Input waits there up to a limit and is refused beyond it.

Each pane has a connection of its own for its stream, its input and its
resizes. Starting, ending and forgetting sessions goes over one connection the
whole window shares, because a pane's own can be held up: input a program
does not read is not taken off the socket by the host, and what the pane says
afterwards, a resize included, waits behind it. Ending the session never does.

A host that cannot be reached, has no room for another connection or refuses
the shell leaves the pane to the window, which opens it with a PTY of its own
as it would without the switch; the host is then left alone for half a minute
so that a window restoring many panes waits for it once. After the host has
started the shell there is no falling back - that would be a second shell - so
if the pane cannot be attached the session is ended and the pane shows the
error.

Closing a pane ends its session: the host is asked to terminate the process
group, and to forget the session once the child is reaped. The window also
starts its own watcher for every session's child, so a window that quits, is
killed or is replaced by an update takes its sessions with it, exactly as it
does for the PTYs it owns.

A terminal can also be attached to a session that was already running. It
then replays the session's log first, and a replay rebuilds the grid without
acting on anything: the output is as old as the session, so the window is not
woken, nothing is copied to the clipboard, no bell rings, and of the titles,
working directories and command marks the log holds only the last is kept.
The host says where its log ended when the attach was made, and that position
is where old output ends and new begins. The code and its tests exist; nothing
in the window attaches this way yet.

Not there yet:

- **Sessions do not outlive the window.** Keeping them when the window quits,
  and finding and reattaching them when it starts, is what the switch is a
  step towards.
- **A pane is told the window's colors when it is drawn.** After a theme
  change, a pane that is not on screen answers color queries with the old
  theme until it is shown again.
- **Clearing the scrollback clears the window's copy.** The host's log still
  holds it.

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
