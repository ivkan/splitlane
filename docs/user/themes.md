# Themes

Splitlane ships three themes. There is no custom theme format: the theme is
chosen by name from the bundled set.

| Name | |
|---|---|
| `Harbor Dark` | Dark. The default. |
| `Harbor Light` | Light counterpart of Harbor Dark. |
| `One Dark` | Dark, based on the One Dark palette. |

A theme colours the terminal (background, foreground, cursor, selection, the
ANSI palette), the rest of the interface, and the syntax highlighting in the
diff view. The selected-text colour is adjusted when the theme loads so that
selected text stays readable against the selection background.

## Choosing a theme

Use **Settings -> Appearance -> Theme**, or **Themes** in the menu at the
bottom of the side rail, which opens a searchable list. The Settings menu
offers four entries:

| Entry | What is saved |
|---|---|
| System | `"theme_mode": "system"` and `"theme"` set to `Harbor Light` or `Harbor Dark`, whichever matches the OS appearance. |
| Harbor Dark | `"theme_mode": "dark"`, `"theme": "Harbor Dark"` |
| Harbor Light | `"theme_mode": "light"`, `"theme": "Harbor Light"` |
| One Dark | `"theme": "One Dark"`, and `theme_mode` removed |

With **System**, Splitlane follows the OS: when the appearance changes, it
switches between Harbor Light and Harbor Dark and saves the new name to
`theme`. System only ever chooses between the two Harbor themes.

**Reset to default** removes both keys, which gives Harbor Dark.

## In splitlane.json

The theme that is drawn is the one named by `theme`:

```json
{ "theme": "Harbor Light" }
```

- The name is matched case-insensitively.
- An absent or `null` `theme` gives Harbor Dark.
- An unknown name also gives Harbor Dark, and logs `Unknown theme '<name>',
  using default`.

See the [configuration reference](configuration/schema.md) for `theme`,
`theme_mode` and `terminal.cursor_color`, which overrides only the terminal
cursor colour.

## Reloading

A theme change applies without a restart, whether it comes from Settings or
from editing `splitlane.json` by hand. Splitlane watches the directory that
holds `splitlane.json` and reloads the theme when the file changes, waiting
300 ms after the last change so that a burst of writes from one save reloads
once. If the file watcher cannot be started, Splitlane instead checks the
file's modification time at most every 500 ms.
