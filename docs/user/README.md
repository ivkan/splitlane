# Splitlane documentation

Splitlane is a native terminal workspace for running command-line coding agents
side by side. These pages are the user guide. The source lives in this
repository, so you edit a page by editing its file here.

## User guide

- [Overview](index.md): what Splitlane is, and a tour of the window: the rail,
  projects, panes and the surfaces a pane can hold.
- [Installation](installation.md): building from source, prerequisites for each
  platform, a macOS app bundle, and where settings are stored.
- [Layouts](layouts.md): projects and panes, the row, column and grid forms,
  and the launcher in an empty pane.
- [Keybindings](keybindings.md): the default shortcuts and how to override them.
- [Review](review.md): the diff surface, for reading a project's changes.
- [Settings](settings.md): a tour of the Settings screen, section by section.
- [Themes](themes.md): the bundled themes and how to switch between them.
- [Configuration schema](configuration/schema.md): every key in `splitlane.json`.
- [Scripting](scripting.md): the `splitlane` CLI, the JSON-RPC socket,
  `splitlane up`, flow files, and one agent coordinating the others.
- [Scripting reference](scripting/reference.md): exact verbs, methods, events
  and exit codes.
- [Hooks](hooks.md): agent lifecycle hooks.
- [Troubleshooting](troubleshooting.md): launch and GPU problems, configuration,
  shortcuts, themes, macOS prompts, logs, and filing an issue.

## Platform and integration notes

- [MCP bridge](../mcp-bridge.md): letting agents read other panes' output,
  read-only.
- [Windows notes](../WINDOWS.md): supported versions, limitations and known
  upstream problems.

## For contributors

- [Architecture](../../ARCHITECTURE.md): how the codebase is laid out.
- [Contributing](../../CONTRIBUTING.md): building, the checks a pull request
  runs, and how to send one.
- [Debugging terminal rendering](../debugging-rendering.md): the latency and
  pixel probes.
- [Design decisions](../internals/design-decisions.md): the rules behind panes,
  the rail, focus and menus, and why each one exists.
- [Agent state and the rail](../internals/agent-state-and-the-rail.md): how an
  agent's status is detected, and how notifications and the Activity chip use it.
