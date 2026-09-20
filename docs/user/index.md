# Splitlane user guide

Splitlane is a native desktop app for running several CLI coding agents side by
side - Claude Code, Codex, OpenCode, Gemini and others - and seeing at a glance
which one is working, which has finished and which is waiting for you. Every
agent runs in a real terminal, exactly as it would in your own shell, with your
own accounts. Splitlane is written in Rust on Zed's GPUI and runs on macOS,
Linux and Windows.

There are no release builds yet. Build it from source as described in
[Installation](installation.md).

## The model in one minute

- **Projects.** A project is a folder you opened. The rail on the left lists
  your projects; each one expands to the sessions running in it. Add one with
  `⌘⇧O` (`Ctrl+Shift+O` on Linux and Windows), switch with `⌘1`-`⌘9`
  (`Ctrl+1`-`Ctrl+9`).
- **Panes.** The active project's content area holds up to four panes, arranged
  as a row, a column or a 2x2 grid. See [Layouts](layouts.md).
- **Surfaces.** A pane shows one thing at a time: an agent session, a shell,
  the project's diff, or a file.
- **The launcher.** A pane that holds nothing shows a filtered list: agents
  found on your `PATH`, this project's sessions that are not in a pane, and its
  past sessions on disk. You choose what goes into the pane from inside it.

When Splitlane is started from a terminal, the current directory opens as a
project. Started from the desktop with nothing to restore, it shows a welcome
screen with **Open a folder**.

## Agents and their status

Each session row in the rail carries one of five status words: `starting`,
`running`, `waiting for you`, `idle` or `failed`. How reliable that word is
depends on the agent:

- **Claude Code and Codex** are read from their own session records and the
  process tree under the pane, so a permission prompt, a running tool call and
  a finished turn are told apart.
- **Gemini, Cursor, OpenCode, Pi, Hermes Agent, Grok, CodeBuddy and Qoder**
  report through lifecycle hooks installed for the length of the session (see
  [Hooks](hooks.md)).
- **Copilot, Kiro, Factory, Antigravity, Openclaw and Amp** expose nothing
  Splitlane can read, so it only knows the process is running and when it
  exits.

A run that finished while you were not looking stays marked until its pane is
on screen. `⌥⇥` (`Ctrl+Shift+J`) jumps to the next session waiting for you, and
the chip in the title bar opens the list of sessions that want attention.

You answer an agent in its own terminal. Any other CLI runs in an ordinary shell
pane.

`⌘N` (`Ctrl+Shift+N`) starts a new agent session in the active project. Which
agent it starts is set per project from the project's context menu in the rail
(`⌘N runs Claude Code`, ..., `⌘N always asks`), with a fallback in
Settings -> Agents.

### Notifications

Desktop notifications are sent only for sessions you cannot currently see - a
session counts as seen when the window is active and the session is showing in
a pane of the active project. Settings -> Notifications has three levels:
**Only when something breaks**, **When an agent is waiting for me** (the
default) and **And when work has finished**. There is no "off" level, so a
failure is always reported.

### Leaving while an agent is working

Closing Splitlane stops every process it started, so no agent keeps spending
tokens unwatched. The conversation survives - the next launch reopens each
session with `--resume` - but a turn that was executing does not. So quitting
(`⌘Q`, the window's close button, or a restart into an update) asks first
whenever an agent is mid-turn, and names the sessions. Nothing is asked when
every session is idle, waiting for you, or still starting.

## Diff and files

- **The diff.** A project's changes against its base branch, including
  uncommitted and untracked files, open as a pane with a changed-file list, a
  filter and a revision picker. Open it with `⌘D` on macOS, from the `+N -N`
  counters on the project's rail row, or from the Files panel. See
  [Review](review.md).
- **The Files panel.** `⌘B` (`Ctrl+Alt+F`) toggles a tree of the project on the
  right. Changed files carry `M` / `A` marks, ignored entries are drawn dimmed
  and hidden (dot) files are not listed. Clicking a file opens it in a pane; a
  row's context menu has **Copy Path**, **Copy Relative Path**, **Reveal in
  file manager** and an open command.
- **The file surface.** Markdown is rendered; other text files are shown
  read-only with line numbers and syntax colouring; anything else gets a
  summary card. `Enter` opens the file in the editor chosen in Settings ->
  General. In a terminal, `⌘`-clicking (`Ctrl`-clicking) a Markdown path opens
  it in a pane, and a code path such as `src/main.rs:42:7` opens in that editor.

## Starting work

- **From the rail.** Each project row has `+ agent`, `+ shell` and
  `+ worktree` (the last needs a git repository).
- **Worktrees.** `+ worktree` asks for a branch name, bases the new branch on
  the project's current branch and creates the checkout in
  `<repo>.worktrees/<branch>`. It can run a setup command (remembered for the
  project), start an agent session in the new worktree, and remove the worktree
  when the project closes.
- **Presets.** A preset is a saved set of panes, each with its own directory
  and agent, stored as one TOML file per preset in
  `<config dir>/splitlane/presets/`. The Launch pad (`⌘⇧L`, `Ctrl+Shift+L`)
  lists them, edits them and can save the current layout as a new one;
  `⌘⇧1`-`⌘⇧3` start the first three. `splitlane up` reads the same files - see
  [Scripting](scripting.md).

## Dev servers

Splitlane watches for listening TCP ports owned by the processes under each
pane (read from the operating system on Linux, macOS and Windows), and matches
terminal output against the startup banners of common servers - Next.js, Vite,
Nuxt, Remix, Astro, Webpack and Angular on the frontend; Express, Fastify,
uvicorn, Flask, Django, Rocket, Actix, Axum, Gin, Fiber, Puma, Tomcat, Laravel
and Spring on the backend. A frontend server gets a chip in the status bar that
opens it in the browser, and the project's context menu lists every detected
port.

## Scripting and agents reading panes

- **CLI and JSON-RPC.** The `splitlane` binary is also a command-line client for
  a running app over a local socket: `splitlane ls`, `read`, `send`, `wait`,
  `watch`, `up` and `flow`. Anything that types into a pane is off until you
  enable it. See [Scripting](scripting.md) and the
  [reference](scripting/reference.md).
- **MCP bridge.** `splitlane mcp install` registers a read-only MCP server with
  the agents it finds, so one agent can list, read and search another pane's
  output. See [MCP bridge](../mcp-bridge.md).

## Privacy and network access

Splitlane has no account and no backend. The agents in your panes talk to their
own providers directly. The app itself contacts the network in three cases:

- **Telemetry** is off unless you set `"telemetry": { "enabled": true }` in
  `splitlane.json`; there is no prompt that turns it on. When enabled, events
  carry no terminal contents, prompts or paths. Setting any of the environment
  variables `SPLITLANE_NO_TELEMETRY`, `DO_NOT_TRACK` or `NO_TELEMETRY` (to any
  value) disables it regardless of the config. A build made without a PostHog
  key at compile time has nowhere to send events.
- **Update checks** query the GitHub releases API for this repository. Turn
  them off with `"check_for_updates": false` or in Settings -> General.
- **Claude plan limits** in the rail are read from Anthropic's usage endpoint
  with the login your Claude Code CLI already stored on this machine.

## System requirements

- **macOS** on Apple Silicon (Metal).
- **Linux** on x86_64 or aarch64, with a Vulkan driver, Wayland or X11.
- **Windows** x64 (DirectX). See [Windows notes](../WINDOWS.md).

macOS is where Splitlane gets daily use; Linux and Windows are built and tested
in CI but have had less hands-on use.

## More

- [Installation](installation.md) - building from source on each platform
- [Layouts](layouts.md) - projects, panes, the three forms, the launcher
- [Keybindings](keybindings.md) - every default shortcut and how to change it
- [Review](review.md) - the diff surface
- [Settings](settings.md) and [Themes](themes.md)
- [Configuration schema](configuration/schema.md) - every `splitlane.json` key
- [Hooks](hooks.md) - agent lifecycle hooks
- [Troubleshooting](troubleshooting.md)
- Source and issues: [github.com/ivkan/splitlane](https://github.com/ivkan/splitlane)
