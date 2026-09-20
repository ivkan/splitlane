# Keybindings

Every default shortcut, grouped by what it acts on, and how to change them.

Each row gives the action name - the string you use in the `shortcuts` map of
`splitlane.json` - and the chord on macOS and on Linux and Windows. Most chords
are the same key with `⌘` on macOS and `Ctrl` elsewhere. A few differ on
purpose, because the single-modifier `Ctrl`+letter chord would take a key away
from readline in the terminal (`Ctrl+K`, `Ctrl+B`, `Ctrl+F`, `Ctrl+N`) or from
the window manager (`Alt+Tab`).

"Unassigned" actions have no default chord. They are listed in Settings ->
Shortcuts, where you can give them one.

The **When** column says where a chord works: *anywhere* in the window, or only
while a particular kind of surface has keyboard focus.

## Panes and layout

| Action | Description | macOS | Linux / Windows | When |
| --- | --- | --- | --- | --- |
| `add_pane` | Add pane | `⌥\` | `Alt+\` | anywhere |
| `split_vertically` | Add a pane side by side | `⌘⇧E` | `Ctrl+Shift+E` | anywhere |
| `split_horizontally` | Add a pane stacked | `⌘⇧D` | `Ctrl+Shift+D` | anywhere |
| `close_pane` | Close pane | `⌘⇧W` | `Ctrl+Shift+W` | anywhere |
| `undo_close_pane` | Reopen the last closed pane | `⌘⇧T` | `Ctrl+Shift+T` | anywhere |
| `toggle_zoom` | Zoom the focused pane / restore | `⌘⇧Z` | `Ctrl+Shift+Z` | anywhere |
| `layout_even_horizontal` | Side by side | `⌘⌥1` | `Ctrl+Alt+1` | anywhere |
| `layout_even_vertical` | Stacked | `⌘⌥2` | `Ctrl+Alt+2` | anywhere |
| `layout_grid` | Grid (2x2) | unassigned | unassigned | anywhere |
| `split_equalize` | Make panes equal size | `⌘⇧=` | `Ctrl+Shift+=` | anywhere |
| `swap_pane` | Swap: press, then a focus arrow | `⌘⇧S` | `Ctrl+Shift+S` | anywhere |
| `focus_left` | Focus the pane to the left | `⌥←` | `Alt+Left` | anywhere |
| `focus_right` | Focus the pane to the right | `⌥→` | `Alt+Right` | anywhere |
| `focus_up` | Focus the pane above | `⌥↑` | `Alt+Up` | anywhere |
| `focus_down` | Focus the pane below | `⌥↓` | `Alt+Down` | anywhere |
| `focus_previous_pane` | Previous pane | `⌃⇥` | `Ctrl+Tab` | anywhere |
| `new_tab` | New shell in the focused pane, replacing what it shows | `⌘⌥T` | `Ctrl+Alt+T` | anywhere |
| `close_tab` | Close the focused pane's surface | `⌘W` | `Ctrl+W` | anywhere |

## Projects

| Action | Description | macOS | Linux / Windows | When |
| --- | --- | --- | --- | --- |
| `new_workspace` | Add a project (pick a folder) | `⌘⇧O` | `Ctrl+Shift+O` | anywhere |
| `close_workspace` | Close the active project | `⌘⇧Q` | `Ctrl+Shift+Q` | anywhere |
| `select_workspace_1` ... `select_workspace_9` | Switch to project 1-9 | `⌘1`-`⌘9` | `Ctrl+1`-`Ctrl+9` | anywhere |
| `next_workspace` | Next project | unassigned | unassigned | anywhere |
| `copy_workspace_path` | Copy the project's path | `⌃⌥⇧C` | `Ctrl+Alt+Shift+C` | anywhere |
| `reveal_workspace_in_file_manager` | Reveal in file manager | `⌃⌥R` | `Ctrl+Alt+R` | anywhere |
| `open_workspace_in_zed` | Open in Zed | `⌃⌥Z` | `Ctrl+Alt+Z` | anywhere |
| `open_workspace_in_cursor` | Open in Cursor | `⌃⌥C` | `Ctrl+Alt+C` | anywhere |
| `open_workspace_in_vscode` | Open in VS Code | `⌃⌥V` | `Ctrl+Alt+V` | anywhere |
| `open_workspace_in_windsurf` | Open in Windsurf | `⌃⌥W` | `Ctrl+Alt+W` | anywhere |

## Agents and sessions

| Action | Description | macOS | Linux / Windows | When |
| --- | --- | --- | --- | --- |
| `new_agent` | New agent session in the active project | `⌘N` | `Ctrl+Shift+N` | anywhere |
| `new_shell` | New shell in the active project | `⌃⌘N` | unassigned | anywhere |
| `new_home_agent` | New agent in the home directory | unassigned | unassigned | anywhere |
| `new_worktree` | New worktree in the active project | unassigned | unassigned | anywhere |
| `jump_next_waiting` | Jump to the next session waiting for you | `⌥⇥` | `Ctrl+Shift+J` | anywhere |
| `open_attention_queue` | Attention queue | `⌘⇧K` | `Ctrl+Shift+K` | terminal |
| `open_composer` | Send to agent | `⌘⇧Space` | `Ctrl+Shift+Space` | terminal |
| `toggle_broadcast_member` | Toggle pane in broadcast group | `⌘⇧B` | `Ctrl+Shift+B` | terminal |
| `open_broadcast_groups` | Broadcast groups | `⌘⇧M` | `Ctrl+Shift+M` | terminal |
| `copy_last_answer` | Copy the agent's last answer as Markdown | `⌘⌥C` | `Ctrl+Alt+C` | anywhere |
| `open_agents_thread_menu` | Session overflow menu | unassigned | unassigned | anywhere |

## Window and panels

| Action | Description | macOS | Linux / Windows | When |
| --- | --- | --- | --- | --- |
| `open_command_palette` | Command palette | `⌘K` | `Ctrl+Shift+P` | anywhere |
| `toggle_files_sidebar` | Toggle the Files panel | `⌘B` | `Ctrl+Alt+F` | anywhere |
| `open_diff_view` | Open the project's changes | `⌘D` | unassigned | anywhere |
| `open_multi_diff` | Open multi-worktree diff | unassigned | unassigned | anywhere |
| `open_launch_pad` | Launch pad | `⌘⇧L` | `Ctrl+Shift+L` | anywhere |
| `start_preset_1` | Start preset 1 | `⌘⇧1` | `Ctrl+Shift+1` | anywhere |
| `start_preset_2` | Start preset 2 | `⌘⇧2` | `Ctrl+Shift+2` | anywhere |
| `start_preset_3` | Start preset 3 | `⌘⇧3` | `Ctrl+Shift+3` | anywhere |
| `quit` | Quit | `⌘Q` | unassigned | anywhere |
| `close_window` | Close window | unassigned | unassigned | anywhere |
| `about` | About Splitlane | unassigned | unassigned | anywhere |
| `open_help` | Open help | unassigned | unassigned | anywhere |
| `copy` | Copy selection | unassigned | unassigned | anywhere |
| `paste` | Paste from clipboard | unassigned | unassigned | anywhere |
| `start_self_update` | Install the available update | unassigned | unassigned | anywhere |
| `dismiss_update` | Dismiss the update notice | unassigned | unassigned | anywhere |

`⌘D`, `⌃⌘N` and `⌘Q` exist only on macOS: on Linux and Windows `Ctrl+D` is
end-of-file in a shell, and `⌃⌘N` has no equivalent when `Ctrl` is the primary
modifier. On those platforms, open the diff from the rail, start a shell with
`+ shell`, or give the actions a chord in Settings -> Shortcuts.

## Terminal

These work while a terminal (a shell or an agent session) has focus.

| Action | Description | macOS | Linux / Windows |
| --- | --- | --- | --- |
| `terminal_copy` | Copy | `⌘C` or `⌃⇧C` | `Ctrl+Shift+C` |
| `terminal_paste` | Paste | `⌘V` or `⌃⇧V` | `Ctrl+Shift+V` |
| `scroll_page_up` | Scroll up one page | `⇧PageUp` | `Shift+PageUp` |
| `scroll_page_down` | Scroll down one page | `⇧PageDown` | `Shift+PageDown` |
| `jump_prev_prompt` | Jump to the previous prompt | `⌘⇧↑` | `Ctrl+Shift+Up` |
| `jump_next_prompt` | Jump to the next prompt | `⌘⇧↓` | `Ctrl+Shift+Down` |
| `toggle_copy_mode` | Toggle copy mode | `⌃⇧X` | `Ctrl+Shift+X` |
| `toggle_search` | Find in pane | `⌘F` | `Ctrl+Shift+F` |
| `font_size_increase` | Increase this pane's font size | `⌘=` | `Ctrl+=` |
| `font_size_decrease` | Decrease this pane's font size | `⌘-` | `Ctrl+-` |
| `font_size_reset` | Reset this pane's font size | `⌘0` | `Ctrl+0` |
| `clear_scroll_history` | Clear scroll history | unassigned | unassigned |
| `reset_terminal` | Reset terminal | unassigned | unassigned |

Plain `Ctrl+C` is never bound, so it always reaches the running process. On
Linux and Windows, `Ctrl+=`, `Ctrl+-` and `Ctrl+0` take those keys from the
shell; rebind them if you rely on readline's undo or digit arguments.

### Find bar in a terminal

While the find bar is open:

| Action | Description | macOS | Linux / Windows |
| --- | --- | --- | --- |
| `search_next` | Next match | `Enter` | `Enter` |
| `search_prev` | Previous match | `⇧Enter` | `Shift+Enter` |
| `dismiss_search` | Close the find bar | `Esc` | `Esc` |
| `toggle_search_regex` | Toggle regular expression | `⌥R` | `Alt+R` |
| `toggle_search_case` | Match case | unassigned | unassigned |
| `toggle_fleet_search` | Search every session's output | `⌥F` | `Alt+F` |

## File view

These work while a file (Markdown, text or other) is open in a pane.

| Action | Description | macOS | Linux / Windows |
| --- | --- | --- | --- |
| `markdown_scroll_page_up` | Scroll up one page | `⇧PageUp` | `Shift+PageUp` |
| `markdown_scroll_page_down` | Scroll down one page | `⇧PageDown` | `Shift+PageDown` |
| `markdown_find_open` | Open the find bar | `⌘F` | `Ctrl+Shift+F` |
| `markdown_copy` | Copy the selection or current match | `⌘C` or `⌃⇧C` | `Ctrl+Shift+C` |
| `open_file_in_editor` | Open the file in your editor (find bar closed) | `Enter` | `Enter` |
| `markdown_find_next` | Next match (find bar open) | `Enter` | `Enter` |
| `markdown_find_prev` | Previous match (find bar open) | `⇧Enter` | `Shift+Enter` |
| `markdown_find_dismiss` | Close the find bar | `Esc` | `Esc` |

## Diff

These work while the diff has focus and no text field or embedded terminal
inside it does. See [Review](review.md).

| Action | Description | Key |
| --- | --- | --- |
| `diff_next_hunk` | Next hunk | `]` |
| `diff_prev_hunk` | Previous hunk | `[` |
| `diff_toggle_view` | Toggle unified / side by side | `u` |
| `diff_toggle_sync` | Toggle scroll sync between columns | `s` |
| `diff_dismiss` | Close a popover / return focus to the diff | `Esc` |
| `copy_diff_hunk` | Copy the hunk under the pointer as a unified diff (unified layout) | `⌃⇧C` (macOS), `Ctrl+Shift+C` |

## The launcher

An empty pane's launcher is driven with fixed keys that are not actions and
cannot be rebound: type to filter, `↑` / `↓` to move, `Enter` to open, `Esc` to
close the pane.

## Changing a shortcut

### From Settings

Settings -> Shortcuts lists every action, including unassigned ones, grouped by
where it works and searchable by name. Click a row and press the new chord
(`Esc` cancels). **Reset to defaults** removes all your overrides. The screen
writes the same `shortcuts` map described below.

### In `splitlane.json`

Add a `shortcuts` object to `splitlane.json` (see
[Configuration schema](configuration/schema.md) for where the file lives). Each
entry maps a **chord** to an **action name**:

```json
{
  "shortcuts": {
    "ctrl-shift-g": "layout_grid",
    "alt-n": "new_shell",
    "secondary-shift-z": "none"
  }
}
```

Changes to the file apply while Splitlane is running.

**Chord syntax.** Modifiers and a key joined by `-` or `+` (`ctrl+shift+g` and
`ctrl-shift-g` are the same). Modifiers are `ctrl`, `alt`, `shift`, `cmd`, and
`secondary`, which means `cmd` on macOS and `ctrl` on Linux and Windows - use it
to write one entry that suits every platform. Modifier order does not matter.
Key names follow the defaults above: letters and digits, `enter`, `escape`,
`tab`, `space`, `left` / `right` / `up` / `down`, `pageup` / `pagedown`,
and punctuation such as `=`, `-`, `\`, `[`, `]`.

**How an override is applied:**

- Binding an action to a chord **replaces** that action's default chord; the
  default stops working.
- If another action had that chord by default, that default is removed, so a
  chord belongs to one action.
- The value `"none"` unbinds a chord without binding anything to it. It matches
  the default however you spell it - on Linux, `"ctrl+shift+d": "none"` removes
  the default `secondary-shift-d`.
- You cannot choose the context. An override works in the same place as the
  action's default: `toggle_search` only in a terminal, `diff_next_hunk` only
  in the diff, and so on.
- An unknown action name or a chord that does not parse is skipped and logged;
  the rest of the map still applies.
