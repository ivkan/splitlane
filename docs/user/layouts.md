# Layouts

A project's content area is made of panes. This page covers how panes are
added, arranged, closed and brought back, and what survives a restart.

Chords are written macOS first, then Linux and Windows. The full list is in
[Keybindings](keybindings.md).

## Projects

A project is a folder you opened. The rail lists every open project; the
content area shows the panes of the active one.

| Do this | macOS | Linux / Windows |
| --- | --- | --- |
| Add a project (pick a folder) | `⌘⇧O` | `Ctrl+Shift+O` |
| Switch to project 1-9 | `⌘1`-`⌘9` | `Ctrl+1`-`Ctrl+9` |
| Close the active project | `⌘⇧Q` | `Ctrl+Shift+Q` |

A project's context menu in the rail (right-click its row) starts sessions,
renames the project, switches its branch, runs a preset saved for it, lists its
dev servers and opens the folder in an editor or the file manager.

## Panes and what they hold

A pane holds one surface at a time: an agent session, a shell, the project's
diff or a file. Its header names what is in it; the `⋯` menu holds that
surface's actions and the `×` closes the pane.

Where a newly opened surface lands is decided the same way wherever you open it
from (a rail row, `+ agent`, the palette, `⌘D`, a file in the Files panel):

1. a pane already showing the same kind of surface;
2. otherwise an empty pane;
3. otherwise a new pane, if there is room for one (see below);
4. otherwise the focused pane, replacing what it shows.

Replacing a session does not stop it. It keeps running, and its rail row stays
one click away from a pane again.

## The three forms

The panes of a project are always in one of three forms:

- **Side by side** - a row of up to four panes.
- **Stacked** - a column of up to four panes.
- **Grid** - two rows of two.

Four panes is the maximum. With two or more panes, the project toolbar above
the content area shows a three-segment control - **Side by side**, **Stacked**,
**Grid** - to switch between them.

| Do this | Action | macOS | Linux / Windows |
| --- | --- | --- | --- |
| Side by side | `layout_even_horizontal` | `⌘⌥1` | `Ctrl+Alt+1` |
| Stacked | `layout_even_vertical` | `⌘⌥2` | `Ctrl+Alt+2` |
| Grid | `layout_grid` | unassigned | unassigned |
| Make all panes equal size | `split_equalize` | `⌘⇧=` | `Ctrl+Shift+=` |

Switching form rearranges the panes you have; it never starts or stops
anything. Two details:

- **Choosing Grid with fewer than four panes** places the open panes into cells
  and fills the rest with empty panes. The Grid segment is dimmed when the
  content area is too small for four cells of at least 320px each, with the
  reason in its tooltip.
- **Leaving a grid** drops the empty cells the grid created, so a grid holding
  two sessions becomes a row or column of two.

Dividers between panes can be dragged. In a grid the vertical divider moves both
rows together.

## Adding a pane

`⌥\` (`Alt+\`), or **Add pane** in the project toolbar, adds one empty pane
next to the others and focuses it. The empty pane shows the launcher.

Add pane is refused, and the control dims with the reason in its tooltip (a
chord shows it as a toast), when:

- **an empty pane is already open** - "An empty pane is already open";
- **the project has no panes** - open something from the empty area instead;
- **four panes are open** - "4 panes is the limit". A grid is always at the
  limit;
- **a new pane would leave any pane under 320px** along the row or column. If
  a 2x2 grid would still fit, the message says so: "No room for 3 side by side -
  Grid fits four".

`⌘⇧E` / `Ctrl+Shift+E` (`split_vertically`) and `⌘⇧D` / `Ctrl+Shift+D`
(`split_horizontally`) do the same thing as Add pane with a direction. The
direction only matters when going from one pane to two; after that, new panes
join the existing row or column.

## The launcher

A pane with nothing in it shows one filtered list in three parts:

- **Start something new** - a row for each supported agent found on your
  `PATH`, then **New shell**. Agents that are not installed are not listed.
- **Open sessions** - this project's sessions that are not currently in a pane.
- **Recent on disk** - this project's past agent sessions, read from each
  agent's own session store, which resume into the pane.

Type to filter, `↑` / `↓` to move, `Enter` to open the selection into this pane.
`Esc` closes the empty pane. The launcher only lists this project's sessions;
to reach another project's, use the command palette (`⌘K`, `Ctrl+Shift+P`).

## Moving focus and sessions

| Do this | Action | macOS | Linux / Windows |
| --- | --- | --- | --- |
| Focus the pane to the left / right / above / below | `focus_left` ... | `⌥←` `⌥→` `⌥↑` `⌥↓` | `Alt+Left/Right/Up/Down` |
| Return to the previously focused pane | `focus_previous_pane` | `⌃⇥` | `Ctrl+Tab` |
| Swap the focused pane with a neighbour | `swap_pane` | `⌘⇧S`, then `⌥`+arrow | `Ctrl+Shift+S`, then `Alt`+arrow |

Panes themselves are not dragged. To put a session in a particular pane:

- drag its rail row onto a pane, or onto the dashed strip that appears at the
  edge of the content area to open it in a new pane;
- or use the rail row's context menu: **Show in ‹what that pane holds›**, or
  **Show in a new pane** when there is room. In a grid the menu names the cell:
  top left, top right, bottom left, bottom right.

## Zoom

`⌘⇧Z` / `Ctrl+Shift+Z` (`toggle_zoom`) makes the focused pane fill the content
area; press it again to bring the other panes back. Zoom hides the other panes
and keeps them running. You cannot add a pane while zoomed.

In the diff, the `⤢` button in its header does a related thing: it zooms the
diff's pane and closes the Files panel, and restores both when pressed again.

## Closing panes and undo

- The `×` in a pane header, **Close pane** in its `⋯` menu, and `⌘⇧W` /
  `Ctrl+Shift+W` (`close_pane`) close a pane.
- **Collapse to this pane**, in a pane's `⋯` menu when more than one pane is
  open, closes all the others.
- Closing an agent session's pane does not delete the session; it stays in the
  rail. **Delete session** in the `⋯` menu or on the rail row stops it and
  removes it, after a confirmation.
- In a grid, closing a cell leaves an empty pane in its place, so the grid keeps
  its shape.
- Closing the last pane from its header leaves the project with no panes; the
  empty area offers ways to open something.

`⌘⇧T` / `Ctrl+Shift+T` (`undo_close_pane`) reopens the most recently closed
pane. In a grid it goes back into an empty cell.

## What is restored on launch

Splitlane saves the layout as you change it and restores it on the next launch:

- the open projects and which one was active;
- each project's form, pane sizes and what each pane was showing;
- agent sessions, which reattach from their transcripts on disk - a Claude Code
  session resumes into the same conversation;
- shells, which start again in their directory. Terminal scrollback is not
  saved, and a command that was running is not restarted;
- the diff, which is recomputed from git.

Empty panes are not restored, except inside a grid, where empty cells are part
of the form.

Debug builds (`cargo run`) keep their layout in a separate file from release
builds, so the two do not share open projects.
