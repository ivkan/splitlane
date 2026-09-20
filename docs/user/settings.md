# Settings

Settings is a full-window screen for the preferences you change most often.
Open it from the menu at the bottom of the side rail and choose **Settings**;
press `Esc` to leave it (the first `Esc` closes an open menu, if there is one).

The navigation on the left lists ten sections. The search box above it filters
them by name and by what they contain, so typing `scrollback` or `bypass` finds
the right section.

Every choice is saved to `splitlane.json` as soon as you make it, and applies
without a restart unless the row says otherwise. Many rows only state how
Splitlane behaves and have nothing to change; they are listed below so you know
what they mean. For keys that have no row here, see the
[configuration reference](configuration/schema.md).

## General

| Row | What it is | Key |
|---|---|---|
| Restore the last layout on launch | Always on: projects and their panes come back on the next launch. | none |
| Reattach agent sessions | Always on: a restored agent session resumes from the agent's own transcript on disk. | none |
| Confirm before closing a running session | Always on. | none |
| Save layout changes | Always on: the layout is written to `session.json` as it changes. | none |
| Check for updates | Whether Splitlane checks the GitHub releases feed at startup. | `check_for_updates` |
| Default editor | Auto-detect, Zed, Cursor, Windsurf, VS Code, Visual Studio or System default. Used when you open a file in your editor. | `external_editor` |
| Data directory | Where Splitlane keeps its data on this machine. | none |
| Permission mode | A fact: each agent decides when to ask for permission, and asks in its own terminal. | none |

## Appearance

| Row | What it is | Key |
|---|---|---|
| Theme | System, Harbor Dark, Harbor Light or One Dark, each shown with a preview of its colours. **Reset to default** returns to Harbor Dark. | `theme`, `theme_mode` |

How the choice is stored, and what System does, is on the
[Themes](themes.md) page.

## Shortcuts

A searchable table of every action and its key chord, including actions that
have no default chord (shown as Unassigned). Click a row, then press the new
chord to rebind that action; `Esc` cancels. **Reset to defaults** removes all
of your overrides. Writes `shortcuts`. Action names and the file format are on
the [Keybindings](keybindings.md) page.

## Terminal

These apply to every terminal, which includes agent sessions: an agent runs in
its own terminal.

| Row | What it is | Key |
|---|---|---|
| Shell | zsh, bash, sh or fish (PowerShell, Windows PowerShell, Command Prompt or Git Bash on Windows). New terminals only. | `default_shell` |
| Scrollback | 1,000 to 100,000 lines. New terminals only. | `terminal.scrollback_lines` |
| Mono font | The bundled default or any installed font, with type-ahead search. | `font_family` |
| Font size | 8 to 32 pt. | `font_size` |
| Line height | 1.0 to 2.5. | `line_height` |
| Cell width | 0.3 to 2.0. | `cell_width` |
| Font weight | Thin to Extra-black. | `font_weight` |
| Cursor shape | Vintage, Bar, Underline, Double underline, Filled box or Empty box. A program in the terminal can still change it. | `terminal.cursor_shape` |
| Cursor colour | A swatch, or the colour scheme's own cursor colour. | `terminal.cursor_color` |
| Integrated glyphs | Splitlane draws block-element characters itself. | `terminal.integrated_glyphs` |
| Colour emoji | Draws emoji in colour. | `terminal.color_emoji` |
| Jump to previous / next prompt | Shell integration, which prompt jumping relies on. The row shows the chords bound to the two jump actions. | `shell_integration` |
| Acrylic material | Windows only: a translucent backdrop behind terminal backgrounds. | `windows_terminal_material` |

## Agents

The top of the section is a table of what Splitlane can do with an agent -
from running it in a pane, which works for every supported agent, to following
a session across restarts - with how many agents each capability covers.

| Row | What it is | Key |
|---|---|---|
| New agent starts | Which agent the new-agent shortcut starts in a project that has not chosen its own, or Always ask. Only agents found on `PATH` are listed. A project's own choice is in its menu on the rail. | `default_agent` |
| One row per agent | Whether the agent is offered where you start a new agent. The row shows the command Splitlane runs and whether it was found on `PATH`. | `claude_code_button_visible`, `codex_button_visible`, and so on |

Under **What an agent may do**:

| Row | What it is | Key |
|---|---|---|
| Bypass permissions | Launches Claude Code with `--permission-mode bypassPermissions`. This removes Claude Code's protection against prompt injection; use it only on machines you trust. | `claude_code_bypass_permissions` |
| Free access | Lets a lead agent submit prompts to your other panes over the JSON-RPC socket without `SPLITLANE_IPC_SCRIPTING=1`. Every such write is logged. | `ai_unrestricted` |
| Injection fence | Shown only while Free access is on. Keeps pane text that an agent reads wrapped as untrusted output. On by default; turning it off shows a warning. | `ai_injection_fence` |

## MCP

Registers Splitlane's MCP bridge with the agents installed on this machine, so
they can list, read and search Splitlane's panes. There is one row per detected
agent with its registration state, and one action whose label follows that
state: **+ register the bridge**, **Repair the bridge** or **Re-register the
bridge**. It writes each agent's own configuration file and touches only the
`splitlane` entry there - not `splitlane.json`. Details: [MCP bridge](../mcp-bridge.md).

## Skills

Cards for the skills found in `~/.claude/skills`, `~/.codex/skills` and
`~/.agents/skills`, each with a Copy button. Nothing here is written to
`splitlane.json`.

## Notifications

| Row | What it is | Key |
|---|---|---|
| Tell me | Only when something breaks / When an agent is waiting for me (default) / And when work has finished. There is no "off": a session that breaks always notifies. | `agent_panel.notify_level` |
| macOS permission | macOS only: what macOS will do with a notification. See below. | none |
| Agent needs input, Run finished, Session crashed, Agent stalled, Limit burning fast | Whether each kind of notification reaches you at the chosen level: **covered**, **silent** (below your level), **blocked** (the platform will not deliver it) or **unconfirmed** (the platform has not answered yet). Short runs never send "Run finished"; the row states the minimum length. | none |
| Play a sound | Never. | none |

Notifications are only about agents, never about shell output. A notification
is skipped when the session it is about is already on screen in the active
project and the window is active.

### Notifications on macOS

macOS decides separately whether a notification is delivered, and the
**macOS permission** row states its answer:

| Word | Meaning |
|---|---|
| checking | Splitlane has not read the answer yet. |
| not asked yet | macOS has not been asked. Splitlane asks when an agent session starts, once. |
| allowed | Notifications appear on screen. |
| no banners | Allowed, but banners are off in System Settings: notifications go to Notification Center only. |
| off everywhere | Allowed, but banners and Notification Center are both off, so nothing is shown anywhere. |
| denied | Only System Settings -> Notifications -> Splitlane can undo this. |
| unavailable | Splitlane is not running from a `Splitlane.app` bundle. |
| unknown | macOS gave an answer this build does not recognise. |

macOS delivers notifications only to an application bundle, so a binary
started directly (for example with `cargo run`) never shows one and reads
**unavailable**. Build a bundle with `scripts/bundle-macos.sh` and run that; it
does not need a Developer ID signature. While the permission is not asked yet,
off everywhere, denied or unavailable, the notification rows read **blocked**.

Linux and Windows have no such permission, and the row is not shown there.

## Presets

A read-only list of your presets, one row each with its number of panes, its
directory and its shortcut if it has one. Presets are created and edited in
the Launch pad (`⇧⌘L` on macOS, `Ctrl+Shift+L` on Linux and Windows). **Reveal presets folder** opens the folder
that holds the preset files, creating it if needed. See [Layouts](layouts.md).

## Limits

States how the usage-limit meter at the bottom of the rail works. Nothing here
can be changed.

| Row | Value |
|---|---|
| Show | The binding limit, one row per vendor. |
| Warn at | 85%; reached at 100%. |
| Read usage from | Claude CLI. |
| And from | Codex CLI, from its own session files. |
| Read limits every | 30 minutes while nothing is happening, 10 while a Claude session is working, 5 while a short window is burning fast. A window focus reads at most every 5 minutes, and `↻` in the footer reads on demand. |
| Forget a vendor after | 14 days without a reading. |
