# Configuration reference: splitlane.json

Every key Splitlane reads from `splitlane.json`, with its type, default and
effect. Every key is optional. The Settings screen writes most of the common
ones for you (see [Settings](../settings.md)); this page is for everything,
including the keys that have no row there.

A machine-readable [JSON Schema](../../../schemas/splitlane.schema.json) lives
in the repository. Point your editor at it for completion and typo checks:

```json
{
  "$schema": "https://github.com/ivkan/splitlane/raw/main/schemas/splitlane.schema.json"
}
```

## File location

| Platform | Path |
|---|---|
| Linux | `$XDG_CONFIG_HOME/splitlane/splitlane.json` (usually `~/.config/splitlane/splitlane.json`) |
| macOS | `~/Library/Application Support/splitlane/splitlane.json` |
| Windows | `%APPDATA%\splitlane\splitlane.json` |

**A debug build uses a different directory.** A binary built without
`--release` (a plain `cargo build` or `cargo run`) reads and writes
`splitlane-dev` instead of `splitlane` in every path above - for example
`~/.config/splitlane-dev/splitlane.json`. The same applies to the presets
folder and the saved layout. A debug build and a release build of Splitlane
therefore share no settings, projects or presets. If a setting you changed
"does not apply", check which of the two directories the binary you are
running uses.

The file does not need to exist. Splitlane creates it the first time you change
something in Settings.

## How the file is read

- **Unknown keys are ignored.** The JSON Schema marks them as errors so an
  editor flags typos, but the app itself loads the file anyway.
- **A bad value does not take the rest of the file with it.** A value of the
  wrong type (say `"font_size": "big"`) is ignored and
  that setting falls back to its default; everything else still applies.
- **Changes apply while Splitlane runs.** Splitlane watches the file and applies
  a valid save without a restart. If a save leaves the file as invalid JSON, the
  running app keeps the settings it already had. At startup, a file that is not
  valid JSON is ignored as a whole and every key takes its default.
- **Some keys are read only at a particular moment.** Where that is the case the
  table says so: at startup (a restart is needed), or when a terminal is created
  (existing terminals keep the old value).

## Top-level keys

| Key | Type | Default | Notes |
|---|---|---|---|
| `$schema` | string | none | Editor-only pointer to the JSON Schema. Ignored by the app. |
| `$schemaVersion` | string | `1.0.0` | Version of the file format. A different value does not stop the file from loading. |
| `default_shell` | string or null | platform default | Shell for new terminals. Unix: configured -> `$SHELL` -> `/bin/sh`. Windows: configured -> `pwsh.exe` -> `powershell.exe` -> `%ComSpec%` -> `C:\Windows\System32\cmd.exe` -> `cmd.exe`. Running terminals keep their shell. |
| `theme` | string or null | `Harbor Dark` | `Harbor Dark`, `Harbor Light` or `One Dark`, matched case-insensitively. An unknown name draws `Harbor Dark` and logs a warning. See [Themes](../themes.md). |
| `theme_mode` | string or null | none | `light`, `dark` or `system`. With `system`, Splitlane switches `theme` between `Harbor Light` and `Harbor Dark` to follow the OS appearance, including when it changes while the app is running. Settings writes this key together with `theme`. |
| `font_family` | string or null | `JetBrainsMono Nerd Font Mono` (bundled) | Terminal font. Accepts the aliases `.SplitlaneMono` and `JetBrainsMono NFM` for the bundled default, `.SplitlaneSans` for the bundled Geist, the other bundled families (`Geist Mono`, `Geist`, `Lilex`, `IBM Plex Mono`, `IBM Plex Sans`), or an installed family. |
| `font_fallbacks` | array of strings or null | none | Families tried, in order, for glyphs `font_family` lacks - for example a Nerd Font for prompt icons. |
| `font_size` | number or null | `10.0` | Terminal font size in points, `8.0` to `32.0`. |
| `font_weight` | string or null | `normal` | `thin`, `extra_light`, `light`, `semi_light`, `normal`, `medium`, `semi_bold`, `bold`, `extra_bold`, `black`, `extra_black`. |
| `line_height` | number or null | `1.2` | Line height multiplier, `1.0` to `2.5`. |
| `cell_width` | number or null | `0.6` | Cell width multiplier, `0.3` to `2.0`. |
| `option_as_meta` | boolean or null | `false` on macOS, `true` elsewhere | Alt/Option sends an ESC prefix (Meta). Leave it off on macOS if you type characters with Option. Read when a terminal is created. |
| `shell_integration` | boolean or null | `true` | Injects a small snippet into the shell's startup so it reports its working directory (OSC 7) and marks prompts (OSC 133), which is what prompt jumping uses. `false` starts the shell exactly as it would outside Splitlane. |
| `window_decorations` | string or null | `client` | `client` draws Splitlane's own title bar; `server` asks the OS or compositor for its frame. Read at startup. |
| `window_backdrop` | string or null | `auto` | `auto`, `mica`, `blurred`, `acrylic`, `transparent`, `opaque` or `off`. On Windows, `auto` uses Mica where the system supports it and an opaque window otherwise. Read at startup. The `SPLITLANE_WINDOW_BACKDROP` environment variable overrides it for one launch. |
| `windows_terminal_material` | boolean or null | `false` | Windows only: terminal backgrounds become transparent so the window backdrop shows through. Ignored on other platforms. |
| `windows_chrome_material` | boolean or null | `false` | Windows only: lets the window backdrop show through the side rail. Ignored on other platforms. |
| `macos_chrome_material` | boolean or null | `false` | macOS only: puts the native sidebar material behind the side rail. Ignored on other platforms, and when `window_backdrop` is `opaque`, `off` or `transparent`. |
| `external_editor` | string or null | `auto` | What "open this file in the editor" uses (Ctrl/Cmd-click on a `path:line:col` in a terminal, or Enter on a file view). `auto`: `$VISUAL`, then `$EDITOR`, then the first of `code`, `cursor`, `zed`, `subl`, `code-insiders`, `windsurf`, `hx`, `nvim`, `vim`, `emacs` found on `PATH`, then the OS opener. `system`: the OS opener only (the line number is lost). Any other value is a program name tried first; if it is not installed, `auto` applies. |
| `default_agent` | string or null | none | Which agent the new-agent shortcut starts in a project that has not chosen its own: an agent tag (`claude_code`, `codex`, `opencode`, `pi`, `hermes`, `grok`, `amp`, `cursor`, `gemini`, `kiro`, `antigravity`, `copilot`, `codebuddy`, `factory`, `qoder`, `openclaw`) or `ask`. Absent means Splitlane asks. Written by Settings -> Agents. |
| `claude_code_bypass_permissions` | boolean or null | `false` | Launches Claude Code with `--permission-mode bypassPermissions`, which turns off its permission prompts and offers no protection against prompt injection. Affects Claude Code only. |
| `ai_unrestricted` | boolean or null | `false` | Lets a caller over the JSON-RPC socket submit prompts to agent panes (`surface.send_text` with `submit: true`) without `SPLITLANE_IPC_SCRIPTING=1`. Every such write is logged. Checked on every call. |
| `ai_injection_fence` | boolean or null | `true` | Wraps terminal text returned by `surface.read` in an untrusted-output marker, so text in another pane cannot pose as instructions to an agent reading it. Independent of `ai_unrestricted`. |
| `agent_stall_detection` | boolean or null | `true` | Marks an agent session as stalled when it has been working with no activity for longer than `agent_stall_threshold_secs`, and notifies once per stall. |
| `agent_stall_threshold_secs` | integer or null | `60` | Silence, in seconds, before a session counts as stalled. Clamped to `30`-`86400`. The check runs every 30 seconds, so detection can lag by up to that much. |
| `review_prefill_delay_ms` | integer or null | `2000` | How long the diff view's review waits before typing its prompt into a freshly started agent. The prompt is also copied to the clipboard. Clamped to `250`-`10000`. |
| `submit_paste_delay_ms` | integer or null | `70` | Minimum pause between pasting text into an agent and pressing Enter to submit it (Send to agent, `splitlane send --submit`). Raise it if a slow agent swallows the Enter. Clamped to `10`-`5000`. |
| `check_for_updates` | boolean or null | `true` | `false` stops Splitlane from contacting the GitHub releases feed at startup. |
| `shortcuts` | object | `{}` | Keybinding overrides, key chord -> action name, for example `{ "secondary-shift-a": "add_pane" }` (`secondary` is Cmd on macOS and Ctrl elsewhere). Map a chord to `"none"` to unbind it. Applied while running. Action names: [Keybindings](../keybindings.md). |
| `terminal` | object or null | see below | Terminal renderer and PTY settings. |
| `agent_panel` | object or null | see below | Notification level, plus keys that currently have no effect. |
| `telemetry` | object or null | see below | Usage-reporting consent. |
| `tool_permissions` | object | `{}` | Accepted; currently has no effect. See below. |
| `commands` | array | `[]` | Legacy preset storage, read once for migration. See below. |

### Agent visibility

One key per agent decides whether the agent is offered where you start a new
agent: the launcher in an empty pane and the new-agent menus. Each is
`boolean or null`: `true` always offers the agent, `false` never does, and
`null` or an absent key offers it only when its command is found on `PATH`.
Settings -> Agents writes these keys.

| Key | Agent |
|---|---|
| `claude_code_button_visible` | Claude Code |
| `codex_button_visible` | Codex |
| `opencode_button_visible` | OpenCode |
| `pi_button_visible` | Pi |
| `hermes_agent_button_visible` | Hermes Agent |
| `grok_button_visible` | Grok |
| `amp_button_visible` | Amp |
| `cursor_button_visible` | Cursor |
| `gemini_button_visible` | Gemini |
| `kiro_button_visible` | Kiro |
| `antigravity_button_visible` | Antigravity |
| `copilot_button_visible` | Copilot |
| `codebuddy_button_visible` | CodeBuddy |
| `factory_button_visible` | Factory |
| `qoder_button_visible` | Qoder |
| `openclaw_button_visible` | Openclaw |

## terminal

| Key | Type | Default | Notes |
|---|---|---|---|
| `terminal.backend` | string | `auto` | Which terminal emulator engine runs new terminals: `auto`, `ghostty` or `alacritty`. Read when a terminal is created. See [Terminal backend](#terminal-backend). |
| `terminal.scrollback_lines` | integer or null | `10000` | Scrollback history in lines, `100` to `100000`. Read when a terminal is created. Agent session terminals use at most `10000`, whatever the value. |
| `terminal.cursor_shape` | string or null | `block` | `vintage`, `block`, `beam`, `underline`, `double_underline` or `hollow`. A program running in the terminal can still change it. Read when a terminal is created. |
| `terminal.cursor_blink` | string or null | `terminal_controlled` | `on`, `off`, or `terminal_controlled` (the program decides). Read when a terminal is created. |
| `terminal.cursor_color` | string or null | the theme's cursor colour | `#RRGGBB` or `#RGB`. |
| `terminal.ligatures` | boolean or null | `false` | Draws programming ligatures for fonts that have them. |
| `terminal.integrated_glyphs` | boolean or null | `true` | Splitlane draws block-element characters itself instead of using the font's glyphs, so they join without gaps. |
| `terminal.color_emoji` | boolean or null | `true` | Draws emoji in colour. |
| `terminal.env` | object or null | none | Environment variables added to every new terminal. `TERM`, `COLORTERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, `SHLVL` and the `SPLITLANE_WORKSPACE_ID`, `SPLITLANE_SURFACE_ID`, `SPLITLANE_SOCKET_PATH`, `SPLITLANE_BIN_DIR` variables cannot be overridden, and `LD_*` / `DYLD_*` keys are dropped. |
| `terminal.scroll_multiplier` | number or null | `1.0` | Mouse-wheel speed for scrollback, `0.1` to `10.0`. Has no effect while a program has mouse reporting or the alternate screen on. Read when a terminal is created. |

```json
{
  "terminal": {
    "backend": "auto",
    "scrollback_lines": 20000,
    "cursor_shape": "beam",
    "env": { "EDITOR": "nvim" }
  }
}
```

### Terminal backend

Splitlane has two terminal engines: libghostty and Alacritty's
`alacritty_terminal`. What `terminal.backend` resolves to depends on the
platform:

| Platform | `auto` | `ghostty` | `alacritty` |
|---|---|---|---|
| Linux | libghostty | libghostty | Alacritty |
| Windows x64 (MSVC toolchain) | libghostty | libghostty | Alacritty |
| macOS, and every other Windows target | Alacritty | Alacritty | Alacritty |

libghostty is only available when the binary was built with the
`libghostty-linux` or `libghostty-windows` Cargo feature; both are on by
default. Without the feature, every value resolves to Alacritty. If libghostty
fails to start a terminal, that terminal falls back to Alacritty. A value other
than the three above also resolves to Alacritty, with a warning in the log.

The log states the outcome for each terminal in a line beginning
`Terminal backend selected:`, with the requested and the effective backend.

## agent_panel

| Key | Type | Default | Notes |
|---|---|---|---|
| `agent_panel.notify_level` | string or null | `Waiting` | Which desktop notifications you get. `Broke`: only when something breaks (a session crashed or stalled, a usage limit will run out before it resets). `Waiting`: also when an agent is waiting for you. `Finished`: also when a run has finished. There is no "off". Settings -> Notifications writes this key. |
| `agent_panel.notify_when_agent_waiting` | string or null | none | Older setting, read only when `notify_level` is absent: `PrimaryScreen` or `AllScreens` counts as `Finished`, `Never` as `Broke`. |
| `agent_panel.max_content_width` | integer or null | `760` | Accepted; currently has no effect. |
| `agent_panel.thinking_display` | string or null | `Auto` | Accepted (`Auto`, `Preview`, `AlwaysExpanded`, `AlwaysCollapsed`); currently has no effect. |
| `agent_panel.profiles` | object | `{}` | Accepted; currently has no effect. Each named entry may set `agent`, `model`, `mode`, `effort` (strings) and `tools` (array of strings). |
| `agent_panel.default_profile` | string or null | none | Accepted; currently has no effect. |

```json
{ "agent_panel": { "notify_level": "Finished" } }
```

## tool_permissions

Accepted and currently has no effect. The shape is an object keyed by tool kind,
each entry holding `always_allow` and `always_deny` arrays of strings:

```json
{ "tool_permissions": { "read": { "always_allow": [], "always_deny": [] } } }
```

## telemetry

`telemetry.enabled` is tri-state: `null` or absent is unanswered, `false` is
opted out, `true` is opted in. Nothing is sent unless it is `true`, and even
then only from a build that had a `POSTHOG_API_KEY` set at compile time; a
build from source without that variable sends nothing. Setting any of the
environment variables `SPLITLANE_NO_TELEMETRY`, `DO_NOT_TRACK` or
`NO_TELEMETRY` turns reporting off regardless of the config.

```json
{ "telemetry": { "enabled": false } }
```

## commands

`commands` is where presets used to be stored. Presets are now TOML files in
the `presets` folder next to `splitlane.json` (`<config dir>/splitlane/presets/`,
or `splitlane-dev/presets/` for a debug build), edited in the Launch pad. See
[Layouts](../layouts.md).

The only thing Splitlane does with `commands` is a one-time migration: at
startup, if `commands` holds entries with a `workspace` object and the presets
folder has no presets in it, each such entry is written out as a preset file
and a message in the window says so. The key itself is left untouched, and nothing
writes to it. Entries without a `workspace` are ignored.

For reference, the shape the migration reads:

- A command entry has `name` (required), `description`, `keywords` (array of
  strings), and either `workspace` or `command` (a shell command string).
- A `workspace` has `name` (falls back to the entry's `name`), `cwd`,
  `layout_preset` (`even_h`, `even_v` or `grid`; anything else becomes
  `even_h`), `color` (six hex digits) and `layout`.
- A `layout` node has a `type`: a `pane` node holds `surfaces`, an array; a
  `split` node has `direction` (`horizontal` or `vertical`), `children`, and
  optionally `ratio` or `ratios`. The migration takes the
  surfaces in order and ignores the tree's shape; the arrangement comes from
  `layout_preset`.
- Each surface becomes one pane of the preset. It carries over `name` (or
  `custom_name`, which wins), `cwd`, `agent`, `command`, `prompt`, `env` and
  `focus`. The keys `surface_type`, `path`, `scrollback`, `font_size` and
  `surface_id` are accepted and not carried over.

## Example

```json
{
  "$schema": "https://github.com/ivkan/splitlane/raw/main/schemas/splitlane.schema.json",
  "theme": "Harbor Light",
  "theme_mode": "light",
  "font_family": "JetBrainsMono NFM",
  "font_size": 12.0,
  "external_editor": "zed",
  "shortcuts": { "secondary-shift-a": "add_pane" },
  "terminal": {
    "scrollback_lines": 50000,
    "cursor_blink": "off"
  },
  "agent_panel": { "notify_level": "Waiting" },
  "gemini_button_visible": false
}
```
