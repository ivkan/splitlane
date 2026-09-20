# Review

The diff surface shows what a project's branch changes, in a pane beside the
agents that made the changes. It needs the project to be a Git repository.

## Opening it

- `⌘D` on macOS (`open_diff_view`; unassigned on Linux and Windows, where you
  can give it a chord in Settings -> Shortcuts).
- The `+N -N` counters on the project's row in the rail.
- **Review** in the Files panel's header.

The diff opens in a pane like any other surface. Once opened, the project gets a
**Changes** row in the rail; clicking it brings the diff back into view. Opening
it again while it is on screen just focuses its pane.

## What it compares

The diff is `merge-base(HEAD, base)..working tree`: everything the current
branch adds since it diverged from the base, including uncommitted edits and
untracked files. It shows your own changes and any agent's alike.

The base is chosen automatically, in this order: a local `develop` branch, the
remote's default branch (`origin/HEAD`), then `main`, `master`, `origin/develop`,
`origin/main`, `origin/master`. If none exists, pick one.

Detected renames show as one entry (`old -> new`). To keep memory bounded, some
files are listed without their contents: lockfiles (`Cargo.lock`,
`package-lock.json`, `bun.lockb`, `yarn.lock`, `pnpm-lock.yaml`,
`composer.lock`, `poetry.lock`, `Gemfile.lock`), files larger than 512 KiB,
and binary files. At most 200 changed files are shown; beyond that the diff
stops with a note that it was truncated.

The diff refreshes by itself when files in the working tree, `HEAD`, the index
or the base change.

## The header

From left to right:

- **Review** and the comparison chip, `vs <base>`. Click it to pick another
  revision: the list holds local and remote-tracking branches and tags, and has
  a filter field. A commit SHA or any other ref Git can resolve is accepted too.
- The status, `N files · uncommitted`.
- **Review** with an AI agent, and a button that opens a shell in the worktree
  (see below).
- **Collapse all** / **Expand all** folds every file in the diff to its header
  or opens them again.
- The layout chip, **unified** or **side by side**. `u` toggles it.
- `⤢` - give the review the whole window: zooms the diff's pane and closes the
  Files panel. Press it again to put both back.

## The file list

The column on the left of the diff is headed **Changes**, with the total lines
added and removed. It lists every changed file with its status and its own line
counts.

- Type in the filter field to narrow the list by path (case-insensitive).
  `Esc` clears it.
- The header button switches between a flat list and a folder tree.
- Clicking the header collapses or expands the list.

## Reading the diff

The footer shows the keys and where you are: `hunk 2 of 9`, with previous and
next buttons.

| Key | Does |
| --- | --- |
| `]` | Next hunk |
| `[` | Previous hunk |
| `u` | Toggle unified / side by side |
| `Esc` | Close a popover and return focus to the diff |
| `⌃⇧C` (macOS), `Ctrl+Shift+C` | Copy the hunk under the pointer as a unified diff |

These keys work while the diff has focus and no text field or terminal inside
it does. They can be rebound - see [Keybindings](keybindings.md).

Right-click in the diff for **Copy hunk** and **Copy file diff**. Copying a hunk,
from the menu or with the key, works in the unified layout with the pointer on
an added or removed line; in side by side, copy the whole file or switch to
unified.

Syntax colouring uses Tree-sitter grammars for Rust, JSON, shell, Python,
TypeScript and JavaScript (including JSX/TSX), TOML, Markdown, Go, YAML, CSS,
HTML, C, C++, Java and Ruby. Other files, and files over about 300 KB, are shown
without colouring.

## Asking an agent to review

**Review** in the header offers Claude Code, Codex, OpenCode and Pi; select one
or more and start. For each selected agent Splitlane opens a real terminal under
the diff, in the worktree's directory, and launches the agent there.

- The review prompt, naming the branch and its base, is typed into the agent's
  input but **not submitted** - you read it and press `Enter` yourself. It is
  also copied to the clipboard, in case the agent was not ready when it was
  typed; the terminal says `Prompt ready · ⌘V to paste` (`Ctrl+V`).
- The second and later agents get a more sceptical version of the prompt, so
  several reviewers do not just repeat each other.
- Close the review terminals before running Review again.

The delay before the prompt is typed is `review_prefill_delay_ms` in
`splitlane.json` (see [Configuration schema](configuration/schema.md)).

## See also

- [Layouts](layouts.md) - panes, zoom and where a surface opens
- [Keybindings](keybindings.md) - every default shortcut
- [User guide](index.md)
