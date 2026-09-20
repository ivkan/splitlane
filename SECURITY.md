# Security Policy

## Supported versions

Splitlane has no release builds yet. Security fixes land on `main`; once
releases exist, fixes will go into the latest release.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately through GitHub:
[Report a vulnerability](https://github.com/ivkan/splitlane/security/advisories/new)
(the "Security" tab of the repository, then "Report a vulnerability").

Please include:

- a description of the issue and its impact,
- steps to reproduce, or a proof of concept,
- the version or commit, your OS, and on Linux the display server (Wayland or
  X11) where relevant.

You should get an acknowledgement within a week. Once a fix is ready it is
published, and the report is credited unless you prefer to stay anonymous.

## Scope

The areas most relevant to Splitlane's threat model:

- the local **JSON-RPC IPC server** (Unix socket or named pipe) and the methods
  it exposes, in particular anything that writes into a pane;
- the **MCP bridge** (`list_panes`, `read_pane`, `search_pane`) and how pane
  output is marked as untrusted content;
- the **agent shim and hooks** that report agent state back to the app;
- the **in-app updater**: download, signature verification and install;
- any path where terminal or agent output reaches a privileged surface, such as
  desktop notifications or opening a link.

Vulnerabilities in the agent CLIs themselves (Claude Code, Codex and others)
should be reported to their vendors.
