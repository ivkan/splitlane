<div align="center">
  <img src="assets/icons/splitlane-128.png" alt="Splitlane logo" width="96" height="96" />
  <h1>Splitlane</h1>
  <p><strong>Run Claude Code, Codex and other coding agents side by side,<br />and see at a glance which one is waiting for you.</strong></p>
  <p>
    <a href="https://github.com/ivkan/splitlane/releases/latest"><img src="https://img.shields.io/github/v/release/ivkan/splitlane?label=download&sort=semver" alt="Download the latest release" /></a>
  </p>
  <p>
    <a href="#install">Install</a> ·
    <a href="#how-it-knows">How it knows</a> ·
    <a href="#what-is-in-the-window">What is in the window</a> ·
    <a href="#next-to-its-neighbours">Neighbours</a> ·
    <a href="#faq">FAQ</a> ·
    <a href="#docs">Docs</a>
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="assets/images/hero-light.png" />
  <img src="assets/images/hero-dark.png" alt="Splitlane with three Claude Code sessions in one project. The rail on the left lists them under the project, in a group named Work; the right-hand pane shows a permission prompt, its header reads 'waiting for you', and the title bar reads '1 agent waiting'." width="100%" />
</picture>
<p align="center"><sub>Three agents in one project. One is working, one has finished, and one is asking to run <code>npm test</code> - the rail, its pane header and the title bar all say so.</sub></p>

https://github.com/user-attachments/assets/8dbb4e22-822f-4928-90e0-1b394e192eb1

Splitlane is a native desktop app for supervising several CLI coding agents at
once. Every agent runs in a real terminal pane, exactly as it would in your own
terminal. What Splitlane adds is the part a terminal cannot tell you: which
session is working, which has finished, and which is stopped on a question for
you - read from the agents themselves, not guessed from terminal activity.

It is written in Rust on [Zed's GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui),
runs on macOS, Linux and Windows, and keeps everything local: the agents are
the CLIs you already use, with your own accounts.

> **Status: early.** Splitlane is used daily on macOS (Apple Silicon). Linux
> (x86_64, aarch64) and Windows (x64) are built and tested in CI but have had
> far less hands-on use. Release builds exist for macOS and Linux; Windows is
> built from source for now - see [Install](#install).

## How it knows

A status dot you cannot trust is worse than none, so each agent's state comes
from the best source that agent offers - and the sources are not equal:

- **Claude Code and Codex** are read directly from their own session records -
  Claude Code's status file and transcript, Codex's rollout - plus the process
  tree under the pane. A permission prompt,
  a running tool call and a finished turn are told apart from what the agent
  recorded, not from whether the terminal is printing.
- **Eight more** - Gemini, Cursor, OpenCode, Pi, Hermes Agent, Grok, CodeBuddy
  and Qoder - report through hooks that a small shim on the pane's `PATH`
  installs for the length of the session.
- **The last six** - Copilot, Kiro, Factory, Antigravity, Openclaw and Amp -
  expose no hook surface Splitlane can use. For them it knows only that the
  process is running and when it exits, so their row can say `running` while
  the agent is in fact waiting for you.

Five words, everywhere a session is listed: `starting`, `running`,
`waiting for you`, `idle`, `failed`. A finished run you have not looked at yet
stays marked until its pane is on screen, so a pile of uncollected results does
not go quiet. You answer an agent in its own terminal; there is no second
approval UI to keep in sync.

## What is in the window

<picture>
  <source media="(prefers-color-scheme: light)" srcset="assets/images/grid-diff-light.png" />
  <img src="assets/images/grid-diff-dark.png" alt="Splitlane in a two-by-two grid: three agent panes and the project's diff with its changed-files column." width="100%" />
</picture>
<p align="center"><sub>The same project as a grid of four: the three agents and the diff of what they changed.</sub></p>

- **Panes.** A project's panes are a row, a column, or a grid of four. `⌥\`
  adds a pane; an empty pane lists the agents on your `PATH`, this project's
  open sessions and its past sessions, and you pick one inside the pane.
- **The rail.** One list of projects, each expanding to its sessions with their
  status. `⌥⇥` jumps to the next session waiting for you.
- **Groups.** Projects can sit under named labels in the rail. Fold a group
  away, or keep its project names out of notifications.
- **Hand an answer over.** A session's last answer goes to another agent on
  screen from the pane's menu, onto its input line; you press Enter. `⌘⌥C`
  copies it instead.
- **Diff.** A project's uncommitted changes, with a file list, a filter and a
  revision picker, in a pane next to the agents that made them.
- **Files.** A tree of the project with git change marks (`⌘B`).
- **Limits.** Your Claude and Codex plan usage in the rail, with when it resets.
- **Sessions survive a restart.** Claude Code sessions resume into the same
  conversation.
- **Notifications** when a session is waiting, has failed, or (optionally) has
  finished - only for sessions you cannot currently see.
- **Presets.** A saved set of panes, each with its own directory and agent,
  started from the Launch pad (`⇧⌘L`) or with `splitlane up`.
- **For agents.** A read-only MCP bridge (`splitlane mcp install`) lets one
  agent read another pane's output, and a local CLI and JSON-RPC socket
  (`splitlane ls`, `read`, `send`, `wait`, `watch`) script the app. Anything
  that types into a pane is off until you enable it.

Supported agents: Claude Code, Codex, OpenCode, Gemini, Cursor, Copilot, Amp,
Pi, Hermes Agent, Grok, Kiro, Antigravity, CodeBuddy, Factory, Qoder and
Openclaw. Any other CLI runs in an ordinary shell pane.

Keys above are macOS; Linux and Windows use `Ctrl`-based equivalents that do
not collide with readline. The full list is in Settings → Shortcuts and
[docs/user/keybindings.md](docs/user/keybindings.md).

## Next to its neighbours

Several tools put coding agents side by side, and three of them come up often
next to this one. Everything said about them here is taken from each project's
own documentation as of October 2026. All three change quickly, so check their
docs before relying on it.

| | Where the agents run | How it learns an agent needs you | Platforms | License |
|---|---|---|---|---|
| Splitlane | Terminal panes in a desktop app | The agent's own session records for Claude Code and Codex, hooks for eight more; for the rest, only whether the process is alive | macOS, Linux; Windows from source | GPL-3.0-or-later |
| [herdr](https://github.com/herdrdev/herdr) | Panes inside the terminal you already use | Rules matched against what the agent draws on screen; hooks or the agent's own reports for some agents | Linux, macOS, Windows | Apache-2.0 |
| [cmux](https://github.com/manaflow-ai/cmux) | Terminal panes in a native macOS app | Terminal notification sequences, a `cmux notify` command and hooks it installs | macOS | GPL-3.0-or-later |
| [Conductor](https://www.conductor.build) | Its own chat interface, one git worktree per workspace | Its documentation does not say | macOS | Closed source |

The third column is the one Splitlane was built around. A permission prompt and
an idle prompt can look alike on screen, and an agent's interface changes from
one release to the next. Claude Code and Codex each keep a record of their own
session, so Splitlane reads that record and the process tree under the pane,
and leaves the screen out of it. Here's the catch: that works for exactly two
agents, and each has to be checked again when its record format changes. herdr
reports state for more agents than Splitlane does.

Where the neighbours are ahead:

- **herdr** keeps terminals running in a background server after you close it
  or lose an SSH connection, and lists several SSH machines together. Splitlane
  does neither. It is local only, and closing it stops every process it
  started. A Claude Code conversation resumes on the next launch; a turn that
  was running does not.
- **cmux** puts a scriptable browser next to the terminal. Splitlane has no
  browser.
- **Conductor** builds the whole workflow on worktrees, with a setup script and
  a run script for each workspace. Splitlane's `+ worktree` is thinner: a
  branch, a checkout and one setup command.

Where Splitlane goes a different way on purpose:

- Agents run in ordinary terminals, the same as outside the app. Conductor
  puts them in its own chat interface and keeps its terminal mode experimental.
- herdr and cmux both give agents an API that can send input to another pane.
  In Splitlane the MCP bridge can only read, and the CLI commands that type
  into a pane stay off until you turn them on.
- The diff of what the agents changed and the project's file tree are in the
  same window.

If you work inside a terminal or over SSH, herdr or plain tmux will fit better.
If you want a worktree per task behind a chat interface on a Mac, that is
Conductor. Splitlane is for a few agents in ordinary terminals on your own
machine, where the question is which one is waiting.

## Install

Download the file for your platform from the
[latest release](https://github.com/ivkan/splitlane/releases/latest).

| Platform | File | Notes |
|---|---|---|
| macOS (Apple Silicon) | `splitlane-<version>-aarch64-apple-darwin.dmg` | The image and the app in it are signed and notarized: open the image and drag Splitlane to Applications, or run `brew install --cask ivkan/splitlane/splitlane`. Intel Macs are not built yet. |
| Debian, Ubuntu | `splitlane-<version>-<arch>.deb` | `sudo apt install ./splitlane-<version>-<arch>.deb` |
| Fedora, openSUSE | `splitlane-<version>-<arch>.rpm` | Import the signing key once (check its fingerprint first, see [keys/](keys/README.md)), then install: `sudo rpm --import https://raw.githubusercontent.com/ivkan/splitlane/main/keys/splitlane-release.asc`, then `sudo dnf install ./splitlane-<version>-<arch>.rpm` or `sudo zypper install ./splitlane-<version>-<arch>.rpm`. zypper refuses the package without the key. |
| Other Linux | `.AppImage` or `.tar.gz` | The AppImage runs as is after `chmod +x`. The tarball unpacks to `splitlane.app/`; `splitlane.app/install.sh` installs it under `~/.local` without sudo. |
| Windows | - | No signed installer yet; [build from source](#build-from-source). |

`<arch>` is `x86_64` or `aarch64` (not `amd64`/`arm64`). Linux builds need
glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36 and later) and a
Vulkan driver. Every file has a `.sha256` and a minisign
`.minisig` beside it, and the `.deb` and `.rpm` are also GPG-signed;
[keys/README.md](keys/README.md) has the public keys and the commands to check
them. If the AppImage will not start because FUSE is missing, run it with
`--appimage-extract-and-run`.

Splitlane asks the release feed for a newer version at startup and every four
hours after that, for as long as it stays open. When there is one it appears
in the title bar, and a click installs it (through the package manager for a
`.deb` or `.rpm`). A Homebrew install updates the
same way, since the cask only puts the app in place; `brew upgrade --cask
splitlane` works too. `"check_for_updates": false` in `splitlane.json` turns
the check off.

### Build from source

Install Rust with [rustup](https://rustup.rs); the exact toolchain is pinned in
[rust-toolchain.toml](rust-toolchain.toml) and is picked up automatically. Then
the platform's build tools:

- **macOS** (Apple Silicon): Xcode. GPUI compiles its Metal shaders with
  `xcrun metal`, and the Metal compiler ships with Xcode, not with the Command
  Line Tools alone.
- **Linux** (x86_64, aarch64): Vulkan and the Wayland/X11 development
  libraries. On Debian or Ubuntu:

  ```bash
  sudo apt-get install libvulkan-dev libwayland-dev libxkbcommon-dev \
    libxkbcommon-x11-dev libx11-dev libxcb1-dev libxcb-render0-dev \
    libxcb-shape0-dev libxcb-xfixes0-dev libfontconfig1-dev libfreetype6-dev
  ```

- **Windows** (x64): Visual Studio Build Tools with the C++ workload (MSVC).

```bash
git clone https://github.com/ivkan/splitlane.git
cd splitlane
cargo run --release -p splitlane-app
```

The binary is `target/release/splitlane` (`splitlane.exe` on Windows). A
first build takes several minutes: GPUI and its dependencies are compiled from
source.

## FAQ

**Why not tmux with one agent per pane?**
tmux shows you the panes; it cannot tell you which agent is blocked on a
question. If you work over SSH or want something headless, use tmux. Splitlane
is for supervising agents on your own machine, where knowing who is waiting is
the whole job.

**Is it an Electron app?**
No. It is Rust on GPUI, rendering with Metal on macOS, Vulkan on Linux and
DirectX on Windows.

**Does it run agents for me, or in the cloud?**
Neither. The agents are the CLIs installed on your machine, signed in with your
own accounts, running in ordinary terminals. Splitlane never sends a prompt on
its own; scripted input is off until you enable it.

**Can other agents read my terminals?**
Only agents you register with the MCP bridge, and only to read. Output it hands
to an agent is marked as untrusted content.

**Does it phone home?**
Only if you opt in. Telemetry never includes terminal contents, paths or
prompts, and `SPLITLANE_NO_TELEMETRY=1` turns it off regardless of settings.

## Docs

- [User guide](docs/user/index.md) - installation, layouts, configuration, themes
- [Scripting](docs/user/scripting.md) - the CLI, `splitlane up` and the JSON-RPC socket
- [MCP bridge](docs/mcp-bridge.md) - letting agents read other panes
- [Windows notes](docs/WINDOWS.md) - support matrix and caveats
- [Architecture](ARCHITECTURE.md) and [design decisions](docs/internals/design-decisions.md) - for contributors
- [Changelog](CHANGELOG.md) - what changed in each release
- [Contributing](CONTRIBUTING.md) - building, the checks a pull request runs, and how to send one
- [AGENTS.md](AGENTS.md) - instructions for coding agents working on this repository

## Acknowledgements

Splitlane is based on [arthjean/paneflow](https://github.com/arthjean/paneflow)
by Arthur Jean, and is distributed under the same license. It has been modified
since 2026.

The interface is built on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui),
the UI framework of the [Zed](https://zed.dev) editor, by Zed Industries
(Apache-2.0). Some code is adapted from Zed: a few pieces from GPUI
(Apache-2.0) and more from Zed's own crates (GPL-3.0-or-later); see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Bundled icons and fonts keep their own licenses; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Product names, logos and
brands shown in Splitlane (agent CLIs, editors, programming languages) are
trademarks of their respective owners and are used only to identify those
tools. Splitlane is not affiliated with or endorsed by them.

## License

[GPL-3.0-or-later](LICENSE)
