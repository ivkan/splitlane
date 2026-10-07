# Interface decisions - panes, the launcher, the rail, headers and menus

Why Splitlane's surfaces behave the way they do. Each block states a rule, the
case that forced it, and - where one is known - what would re-open it. Read the
relevant block before changing the behaviour it covers, and add to this file
rather than scattering the reasoning into comments.

The agent-status half of the interface (status words, the detector,
notifications, the `Activity` chip) is in
[agent-state-and-the-rail.md](agent-state-and-the-rail.md).

## The find bar is one bar in two places

`⌘F` (`KEY_FIND_IN_PANE`) opens a 30px strip across the top of the focused
surface's body - accent `⌕`, the query in mono, "3 of 12", `↑` `↓`, and
`Aa` / `.*`. A terminal has both toggles; a rendered document has `Aa` only,
because its search is a substring scan over a harvested text corpus and a regex
over that corpus would match across a heading and the paragraph under it. `Aa`
is a flag on both VT backends and governs both matching modes at once - a plain
search stops folding case, a regex loses its `i`.

The bar is an **overlay**, not a layout row: giving it real height would reflow
the PTY, so every fullscreen TUI would repaint on `⌘F` and repaint again on
`Esc`. The diff surface has no find bar - it has hunk navigation, and its
`] [ u s` keys are named in its own footer.

## Panes, the launcher, and what Add pane does

**A pane holding nothing shows the launcher, and that is what Add pane opens.**
`⌥\` and the Add pane button put another pane on screen, at an equal share,
holding it, and the choice happens *inside* the pane rather than in front of
it - no popover, no guessed session, and focus on the new pane, because the
new pane is the question. The launcher is **one filtered list in three parts**
(`app/launcher.rs`): every agent on PATH, then this container's surfaces that
are in no pane, then its sessions on disk. `↑` `↓` `enter` work as they do in
`⌘K`; `Esc` closes the pane, which is the whole of undoing a pane you just
added - nothing was opened, so there is nothing to undo. Empty panes get no rail
row and are not restored: the rail is the inventory of what exists, and an
empty pane is an invitation.

**`Add pane` adds, and that is all it does.** A control that adds with one pane
and collapses with several is a mode without a settings row: its third press
closes two live sessions, a destructive act with no name of its own. So the
control has no lit state either (a highlighted "do" button reads as "on", and a
control that only adds has no "on" to mean; the orientation segment beside it
already says whether a split exists), and `⌥\` does not toggle. Collapsing on
purpose is **`Collapse to this pane`** in the pane's `⋯` menu, shown only with
more than one pane: the command needs a target, and a menu opened from a pane
header has already named one by being where it is. It has no chord - it is
rare, destructive, and one slip from the gesture that adds. An empty pane draws
no `⋯`, so the launcher can never be what is kept.

**At the limit the control stays in place and dims**, with the reason in its
tooltip. The general rule: *a control gated by a continuous quantity may not
appear and disappear as that quantity changes.* `MIN_PANE_FOR_SPLIT` is a width
and a width is dragged, so a button that vanished at one window size and
returned at another would make the toolbar jump under the cursor - and take away
the only thing that could explain it. There are **five** refusals and five
sentences (`targeting::PaneRefusal`), written there rather than at the call
sites so that the dimmed button and the chord's toast cannot come to disagree
about what the limit is.

**Adding a pane evens the row out: every pane gets `1/N`.** The insert used
to hand the new pane half of the one it was added beside, which is right for
the second pane by accident - the only pane's share is 1, and half of it is
0.5 - and wrong from the third on: 50/50 became 50/25/25, and a person
straightened all three by hand after every add. The rule lives in
`layout::tree::insert_sibling`, the one insert every door goes through (the
chords, the button, the launcher, the drop strip, IPC `surface.split`,
moving an agent surface in), so no caller re-splits evenly on its own.

A divider the person dragged **does not survive an add**, and that is
accepted on purpose. Adding a pane is a new arrangement, and a ratio set for
two panes says nothing about three: keeping it means inventing the newcomer's
width out of somebody's share, which is the rule that made the third pane
narrow. It is also the only rule the width refusal can **predict**:
`refuse_another_pane` sees the panes area and a count, and `(w - gaps) / n`
is exactly what each pane gets. Kept proportions would make the answer depend
on which divider had been dragged, and the button would allow a split whose
narrowest pane is under the threshold it just checked. The grid is not
touched: it is four cells, which is the ceiling, so nothing is ever inserted
into it, and its two shared ratios stay the person's.

**The refusal and the result measure the same width.** `panes_area` is read
by a canvas laid **inside** the panes area's padding. Hung on the padded
element it measured the padding box, `2 x LG` more than the panes are given,
so even with equal shares a window up to 20px too narrow was allowed a pane
that came out a few pixels under 320. The larger error hid it: with 50/25/25
the result was off by a quarter of the row, not by a few pixels.

**A refusal may name the form that would fit** - `No room for 3 side by side -
Grid fits four` - and it is still a refusal, because it does not change the
arrangement, it says which control does. Only the *width* refusal gets it (a
ceiling is not something another arrangement solves), and only where a 2x2
actually fits: where it does not, the plain wording comes back, because a
suggestion the reader cannot act on is worse than silence. `grid_fits` asks
**both axes**, which is the difference from a row - and it meets the row rule
exactly at the bottom end, since a grid cell and one of two side-by-side panes
are the same `(w - DIVIDER_PX) / 2`.

**A refusal names what is already on screen before it names a ceiling.** The
first question asked is not about a limit at all: `An empty pane is already
open`, whenever a pane holding nothing is on screen. `4 panes is the limit` is
true, and reads as false to somebody looking at two empty cells; telling them a
number when the answer is "it is right there" spends their attention on
arithmetic. The condition is the **empty pane, not the grid** - a row of three
with one launcher gets the same sentence for the same reason - so the order is:
empty pane on screen, then the ceiling, then width-with-grid, then width. It is
also the only question that has to look inside a pane, which is why
`refuse_another_pane_now` takes an `App` and the pure `refuse_another_pane` does
not. A launcher drawn **over** a live surface does not count: the question is
whether an empty pane exists, not whether a launcher is on screen.

**The action's config key is `add_pane`, and `toggle_split` still resolves to
it.** Renaming an action silently unbinds every `shortcuts` override written
against the old key. `registry::RENAMED` is the way out: one name is written
down, listed and shown, and the old one is a door onto it.

**The name in a launcher row is `flex_1`, and that is load-bearing.** Its width
comes from the flex algorithm, not from measuring the text: a content-basis
label beside a `flex_1` spacer settles, inside this scrolling column, at a width
the pane no longer has - about 125px whatever the row said. Side by side the
pane was narrow enough for that to pass; stacked, at the window's full width,
every row drew as a single ellipsis. The `⌘K` palette has the right shape and is
the one to copy: the label grows, and no spacer competes with it.

The pane builds none of that list - it knows a `workspace_id` and not a
directory - so the rows are pushed once per frame beside the surface facts and
the pick comes back as `PaneEvent::LauncherPick`. The third part is a
**blocking** session read and runs off the render thread, generation-guarded,
like the palette's History tab. Both list parts are **this container only**: a
surface belongs to a directory, and crossing projects is what `⌘K` is for.

**The launcher and the palette are keyboard-driven.** A pane opened by a
keystroke that can only be finished with a mouse gives back what the keystroke
was for. The highlight tone is `list_selection`, one step off `subtle`, plus a
2px accent bar on the leading edge. Both halves matter - if the highlight were
`subtle` it would be *exactly what hover paints*, and the row the arrows are on
would read the same as a row under the pointer.

One trap it cost: GPUI builds its key-dispatch path out of **focusable** nodes,
so an ordinary `div` carrying `on_key_down` between the root and the focused
field is not in it and the arrows never arrive. `Pane::launcher_anchor` is
tracked and never focused, purely to be that node.

**The arrow run walks what the query produced** (`last_arrow_row`): a row the
current query selected is in the run; a block that stands regardless of the
query is not, and is reached by click or by its own shortcut. With an empty
query that is the palette's three COMMANDS rows; with a query typed it matches
the whole action registry, and those actions **are** what the query selected.

**The agent rows are ordered, not cut**: preferred agent, then most recently
launched, then never used, alphabetical inside a tie. Recency is read from
`Thread::created_at` on the container's own records - already persisted, and
read synchronously so the list does not re-sort itself a moment after it opens.
Its bound: a session whose record was deleted stops counting, so an agent used
once and cleaned up reads as never used.

## The rail's rows

**The caret folds a project and the row selects it, and those are two
affordances.** If one click both toggled the fold and made the container
active, folding a project away - the commonest reason to touch that row while
working in another one - would move the content area onto the project being
folded. The row selects and opens (`is_expanded = true`, never false);
`disclosure_caret` toggles and stops both the press and the release, so it
changes nothing but the fold. Opening another project's caret to look at its
sessions does not cost the panes you are in.

**A row says what kind on the left and how it is doing on the right, and the
accent belongs to the right one.** `surface_type_glyph` paints `◆` agent, `▷`
shell and `◈` diff in **`dim`** - one neutral step, everywhere a type glyph
appears: rail rows, the pane header, the launcher, the attention popover.
Painting the agent's `◆` in `ui.accent` would put, in the same row and seven
pixels to the right of the status dot, the colour that means **waiting for
you**: one hue, two meanings, the left one lit permanently and the right one
rarely and for a reason. The always-on diamond would read as a state marker
because it wore a state colour.

The rule worth more than the fix: **"one word, one meaning" governs colour, not
just wording.** **The accent answers one question in this app - *does something
want me?* - and it can only keep answering it while it is never spent on
anything else.** The practical half decides it anyway: `finished` is a frequent
state, and an accent lit most of the time has stopped being a signal, while
`waiting` is rare and expensive to miss. And the general form: **when two states
need telling apart, spend hue, shape or presence before brightness** -
brightness is the axis this interface already uses for rank and availability,
so every borrowing of it makes the next reading ambiguous. There is no hue gap
*between* the glyphs either, because all of them answer *which kind* and a
difference between them would say one kind mattered more. Both marks stand:
folding state into the type glyph would mean a row cannot say it is a shell
while it is running.

**Projects in the rail are separated by air, not by a rule.** `space::MD` above
every project row except the first (`ProjectHeaderArgs::lead_gap`), on the
margin rather than inside the row - the row's fill marks the active project, and
8px of it above would read as the selection reaching into the gap. A hairline
is refused twice over: the rail already spends background on the active project
and the selected surface, and `divider` in this app means the boundary of a
*region*. The reason that settles it is **collapse** - a folded project is a
single row, and a line above a single row reads as a separator between two rows
rather than between two groups, while air is not a mark and reads the same at
any group size.

The group header is deliberately **not** made louder: `PROJECTS` in the header
is a section label and a project row is an item with a user-chosen lowercase
path name, and the active project already holds `text`, the top of the scale,
so raising the inactive ones would close the gap that says which is active.
With the air the group carries five signals - 28 vs 25px, 12 vs 11.5px,
`MEDIUM` vs regular, the sessions' indent, and the gap.

**Two row heights, on purpose.** The rail's `row::SESSION` is 25px and the pane
launcher's row is 30px, and that pair is not a token leak. The rail is a
**standing** list read peripherally fifteen rows deep, where density is what
keeps the working set unscrolled; the launcher is a **transient** list at the
centre of attention for a second, driven by arrows, where 30px is arrow-target
size and a tight row is how the wrong session gets launched.

## Project groups

**The word.** *Group* means one thing: a named, ordered set of projects in the
rail. A project together with its sessions is a **project block**; the section
above says "group" in that older sense, and new text does not.

**A group is optional and has no setting.** With no project in a group the rail
is the rail without groups, row for row. A group appears when its first project
is put into it and is gone when its last one leaves, so there is never an empty
label to explain. A project is in at most one group and groups do not nest.

**Shape, not hue.** A label is a neutral filled tag (`tag_fill`) with the
caret on the **right**, and its members hang off a 2px rule (`group_rule`).
Every distinguishable hue in this app already names a state, so a coloured
group would be a second meaning for a colour; and the caret on the right is the
third thing, after the fill and the rule, that tells a label from a project,
whose caret is on the left. `tag_fill` is its own role because it has to sit
above hover and the active row's fill - or a hovered project reads as a label -
without being a control's fill: the label is a heading, not a button. The
light theme's rule is the map's own hover border; the thickness, not the
contrast, is what carries it, because a high-contrast line would argue with the
project names beside it.

**Membership lives on the project.** Each project names its group; the session
keeps the groups in rail order. A list of project ids per group would need an
invariant - no id in two lists - that the type can hold instead, and the order
inside a group is simply the project list's order, which is already saved. The
session schema version is **not** raised for this: the loader refuses a version
it does not know, so a bump would make every earlier build treat a session with
groups as corrupt, while additive keys are read past.

**Folding is only visual.** Folding a group hides its rows and nothing else:
Activity and notifications do not read it. If folding also hid names, a working
group would have to be kept open just to keep its names in notifications, and
folding would stop serving its first purpose - getting rows out of the way.
Two purposes, two switches; hiding names is `Keep names private`.

**`Keep names private`.** The names inside a private group are shown only where
the person looked for them: the open group in the rail, and `⌘K` for a query
they typed. Activity and notifications come to the person, so there only the
group's name appears - a notification's summary and body are the only fields
any of the three platforms is handed that vary per notification, and both are
fixed words around the group's name. The label says `private`, because without
it Activity would be hiding names for no visible reason. In Activity the
group's sessions fold into **one row** where its highest session would stand;
the chip and the section headings still count sessions, from the rows before
folding, so the numbers above the list do not change with privacy.

**A folded group says what is under it**, by the folded project's contract:
`failed`, `waiting`, `running`, unread `finished`, one word per session, its
highest; or `N projects` when nothing is happening. It adds `waiting`, which a
folded project leaves to the title bar's chip, because one group answers for
several projects at once. One counter serves both rows (`FoldedTally`).

**At narrow widths the label gives way before the summary.** The label shrinks
from 130 to 48px with an ellipsis and its full name in a tooltip; then
`private` goes; then `running` and `finished`. `failed` and `waiting` never
go. A folded group exists to say what is happening under it, and its name is
one hover away; the summary is not anywhere else. The label's width is
computed, not left to flex layout: a truncating text has no minimum width in
the layout engine, so as a shrinkable flex item it went straight to 48px however
wide the rail was.

**A new group is named in the rail.** `New group…` makes the group at once, at
the end of the groups, with the project already under its rule, and opens the
label as the field - the same field a project's rename uses, so focus and the
macOS Edit menu behave as they already do there. There is no dialog: the rail
is where the group will live. An empty name, `esc`, or leaving the empty field
undoes all of it and puts the project back under the project that was above it
- a neighbour rather than an index, so closing another project meanwhile does
not shift where it returns. Typing an existing group's name (without regard to
case) says `Acme exists · ⏎ adds to it`, because that is almost certainly what
was meant. A rename onto another group's name does nothing: merging two groups
by renaming is not something anyone asked for.

**Dragging.** The project reorder's insertion line stays as it was, in the same
role; a drop also decides the section - between two members is into their
group, among the projects without one is out of any. While the line is inside a
group its rule lightens to `dim`, so the destination is plain even on the
border between two groups; outside groups no rule lightens, and that absence is
the signal. Three things are refused on purpose:

- **Dropping a project on a project never makes a group.** Any "onto" zone on a
  project row takes room from the insertion line, and a slightly missed reorder
  would become a group nobody asked for.
- **A folded group does not open under a drag**, on hover or after the drop.
  Opening it would move everything below the pointer while the pointer is on
  its way somewhere, and the target would slide away. A drop on its label puts
  the project at the end; for an exact place, open the group first.
- **A group lands only between groups** - never inside one, since groups do
  not nest, and never above the projects without a group, which always come
  first.

**Going to a session opens its group.** From `⌘K`, Activity or `⌥⇥`, a session
in a folded group is shown with the group opened, a private one included: the
person asked to go there.

## Focus: which pane has the keyboard, and which pane the user is in

**Two questions look like one: which pane has the keyboard, and which pane the
user is in.** `LayoutTree::focused_pane` answers the first by asking the window,
and the window says `None` whenever focus rests anywhere but a pane - the rail,
the palette, a dialog, an inline rename, the Files tree.
`SplitlaneApp::focused_pane_as_shown` answers the second in three steps, and it
is the answer the app **draws**: the pane border, the slot header's fill and the
rail row's accent bar are all painted from it, so a person can always point at
the pane it returns.

The case that forced it: `split` asked the window, so **`Add pane` did nothing
at all after a click on the rail** while the screen went on naming a focused
pane; the workaround gave the cause away - click another project and come back,
which calls `focus_first`. Zoom and swap mode had the same silence, and several
call sites had each written the three-step fallback out by hand.
`src-app/tests/focused_pane_policy.rs` is the machine form, because the state
that breaks it is one no runtime test sets up - a test that opens a window
focuses a pane and leaves it focused. A site that must ask the window says so
with `// focus-exact: <reason>`, and there are three kinds: the machinery that
maintains the fallback, a job about the keystroke that just happened (the swap
target, after the arrow moved focus), and a **destructive** action - "nothing
happened" is recoverable, "closed the surface you were not looking at" is not.

**Focus is shown in three places at once.** `FocusedSurface`
(`app/workspace_ops/focus.rs`, read once per frame by the rail) names what takes
input - an `Agent` by thread id, the container's `Diff`, or a `Slot` by leaf
index - all three resolved from the focused pane's **active tab**. The three
signals are the pane border (`focus_border`), the slot header's fill
(`slot_header_focus` over `slot_header`) and the rail row's accent bar.
**Exactly one pane is focused at all times**: when focus rests outside the panes
the answer falls back to `focused_pane_now` (the same value `⌃⇥` is answered
from) and then to the first pane, so the bar never goes out while the user is
looking at it. A pane with no session in it shows the **launcher** and is a pane
in every sense: it holds focus through a handle of its own and is the targeting
ladder's second rung.

**Nothing may be left holding the keyboard that is not on screen.** GPUI
dispatches an action along the **focus chain**, and a `FocusHandle` that is not
in the rendered frame's dispatch tree falls back to the window root - which sits
*above* this app's own root `div`, and every `on_action` lives on that `div`. So
a stale focus does not misroute one keystroke: **every action in the app dies at
once**, chord and button alike, and the one people notice is `Add pane`, because
it is the one they press next. Seen in use twice: `⌘B` opened and closed the
Files panel and then nothing worked at all; and "open Files, close it, and Add
pane stops adding until I click a pane".

Focus goes stale in two ways that need different questions. A holder whose
handle lives on the app - the two docked panels, the pickers, the dialogs -
outlives its element, so `window.focused()` still answers `Some` and only a
**list** can say the answer is a surface nobody can see. A holder whose handle
went with it - a rename field, a composer, a find bar, a launcher query, each
owning its input entity - leaves GPUI's focus map with the entity, so
`window.focused()` answers `None` while `window.focus` still points at the dead
id. `SplitlaneApp::render` asks both once a frame and hands the keyboard back to
`return_focus_to_panes`, which is `focused_pane_as_shown` - the pane the border,
the header and the rail are already drawing. The question is **not** "is a pane
focused": a find bar, a composer and an inline rename all legitimately hold
focus while their pane is drawn as focused, and pulling it out from under
somebody mid-word would be a second and worse bug. The list is the price of
answering this in one place rather than at each of the two dozen places a
surface closes, and it is only right while it is complete -
`src-app/tests/focus_holder_policy.rs` is the machine form, since a handle that
forgets to join it fails silently in both directions.

**A pane edge answers focus in one of two ways, and which one is the theme's to
say.** `pane_border_idle` is what an *unfocused* pane draws, and it is a role
rather than a colour because the two themes need opposite answers. On Harbor
Light the pane fill and the desk sit 1.23 : 1 apart, so an unfocused pane has no
edge of its own and one must be drawn; the signal is then the **change** between
two borders in the same place, worth 3.08 : 1. On Harbor Dark the same move puts
a second hairline at **1.25 : 1** of the focused one - a pair the eye cannot
compare - so the idle edge is `transparent`, and the signal is the outline
**appearing**. `a_pane_edge_answers_focus_in_one_of_the_two_ways_that_work` is
the machine form of the rule.

The dark focus border itself is **`#44705f`**: 3.40 against the desk and 3.13
against the fill, so it clears the 3 : 1 floor on both grounds (`#33453f`
measured 1.88 : 1 against the desk). `accent_border #3f6a5c` was the other
candidate and lost twice: 2.87 on the fill, and it already means the hovered
divider and the composer's focused input row. A theme that states no
`focus_border` does not take a fixed blend either - `derived_focus_border` walks
the mix up until it clears, because how far an accent can get depends on the
accent (One Dark's green reaches the floor at a mix of about 0.6 and a saturated blue never
does), and an accent that cannot carry it hands the border to the theme's own
`text`.

## Naming a surface, and the capability tier beside an agent

**Two automatic sources name a surface, and the process outranks the store.**
`Thread::title` is written by the OSC title of the process in the pane
(`TitleSource::Process`) and by the session's own `ai-title`, checked off disk
at every turn end (`TitleSource::StoredSummary`). If both wrote it and the last
to arrive won, the pane header - which reads the live title - would show the
other name: one surface, two names. `may_name_surface` is the rule. The process
wins while it is speaking, because it says what the agent is doing *now* and it
is what the header already shows; `Thread::title_from_process` is **runtime only
and deliberately not persisted**, so a restored row is named by its summary
again until its CLI paints. A manual rename (`title_user_set`) outranks both.

A **capability tier word** sits beside every agent name where an agent is
chosen (the pane launcher, Settings -> Agents): one of **four** words for all
sixteen agents - `full control`, `resume by name`, `history only`,
`launch only`. In Settings it is a chip; in the launcher it is the row's
trailing text, because a row there is 30px and a chip on it would be a second
object on a line already three columns wide. The assignment is
`TerminalAgent::capability_tier`'s, derived from this codebase's own predicates
rather than transcribed. **The top rung takes two predicates**: a session
Splitlane can pin (`supports_forced_session_id`) and a state file Splitlane can
read (`reports_state`), so "full control" means the app knows where that agent
is at any moment. A word the code has not earned is not said at all, and the
chip is the word **alone** - no caveat beside it.

The **ladder table** above that list in Settings -> Agents
(`capability_ladder`) is counted rather than written down: every number is a
predicate over `TerminalAgent::ALL`, so a row cannot claim more than the build
earns. Five rows, ordered from the common to the rare, and the **one** accent
sits on the last, because the rarest thing is the one worth pointing at and a
reader who got that far has read the rest. "Run it in a pane" is the table's
premise rather than a rung and does not take the accent.

## The pane header

**A pane header says what is in the pane, not what can be done to it.** One
order - kind glyph, name, badge, branch, "↻ rerun" for a shell, spacer, context
meter, `⋯`, `×` - and no action cluster. What the surface itself cannot say is
**pushed** by `app/pane_header.rs::sync_surface_facts` (kind, model, status,
branch), keyed by the surface's terminal entity so a fact travels with the
process when the surface moves. The badge is the model for an agent and "not
restorable" for a shell; the model has no source but the session's own
transcript, which a bounded tail probe reads into `Thread::model` every 15s for
the surfaces actually on screen. "↻ rerun" names the command it will run, which
the shell reports through `OSC 133;E` - reading it off the grid never fired,
because the shell integrations do not emit `133;B`.

Which of these survive a narrow pane is `Pane::header_level`, a measured ladder
of **four** steps: below 320px, then 320, 420 and 520px, each letting in one
more item (status, rerun, badge). An unmeasured header shows the least it can:
assuming the widest step before the first layout pushed the trailing `⋯` and `×`
outside the clip for one frame on a narrow pane, and arriving on the second
frame is the cheaper mistake.

The pane border is focus's, not attention's. The waiting signal is the status
dot in the slot header and on the rail row.

## Permission asks are answered in the agent's own terminal

**A permission ask is answered in the agent's own terminal.** A person answers
the agent the way they would with no Splitlane around it, and the app's job is
not losing that agent from view rather than standing between them - see
[agent-state-and-the-rail.md](agent-state-and-the-rail.md). There is nothing to
switch: no key, no bar, no row in Settings.

Concretely, the build has no permission bar, no session-scoped grants, no
blocking connection class in `ipc.rs` (the IPC server is fully asynchronous),
and no intervention settings. The shim does not register `PermissionRequest` as
a hook event for any agent, because `splitlane-ai-hook` has no dispatch arm for
it and registering it would install a hook that reports nothing. With nothing
waiting on a person, every hook the shim registers carries the same five-second
`timeout`; a hook with no `timeout` key means "no deadline" to the CLIs.

**What would re-open it:** a holding-the-ask path would be rebuilt deliberately
from the git history rather than kept as a switched-off option, because a
dormant contour goes stale against CLIs that change monthly.

**The reporting frames are untouched.** `ai.tool_use` and the rest of the
`ai.*` lifecycle still arrive, fire-and-forget, and the detector still reads
them as an accelerator.

## The grid, and why the slot tree has three named forms

**The grid exists because the four-pane ceiling was otherwise unreachable.** On
a 14" laptop - 1512pt, ~240pt rail - three panes side by side are ~50 columns
against a TUI laid out for 80, so the *live* ceiling of a row is **two**. In a
2x2 each cell is as wide as one of two side-by-side panes (~75 columns) and pays
in height (~25 rows). Row-or-column is the choice of which axis to sacrifice
**entirely**: honest at two panes and dishonest at four, because neither axis
survives being quartered. The grid halves both and pays half on each. So it is
not a cosmetic option - it is the shape in which four panes are reachable on a
laptop.

**It is a real nested tree, not a flag** (`layout/grid.rs`): an outer column of
two rows, each row a pair of leaves. Three jobs come out free. `render` already
recurses, so there is no second renderer. `serde` already round-trips a
recursive `LayoutNode`, so nothing is added to the schema. And
`focus_in_direction` is already spatial - it lays the tree into unit rectangles
and picks by centre distance - so `Alt+Arrow` is two-dimensional here without
new navigation code. `min_main_axis_px` already answers the cross-axis case, so
the drag clamps too.

**The two column ratios are one `Rc<Cell<f32>>` shared by both rows**, and that
is the entire mechanism behind "one shared vertical divider": dragging either
half writes the shared cell and both rows move, so the grid cannot be dragged
into a 2-over-1. Drawn, it is a cross - one vertical rule interrupted at the
crossing by the horizontal one. The sharing is an invariant of the constructor
and **deliberately not part of `is_grid`**, which is shape alone: a tree read
off disk arrives with every ratio in a fresh allocation, and a predicate that
asked about `Rc::ptr_eq` would refuse to recognise the very file it has to
repair. `flattened` rebuilds a grid rather than returning it, for exactly that.

**Leaving a grid gives back the cells the grid filled in**
(`cells_worth_keeping`). The launchers a 2x2 fills itself with are the
*form's*, not the person's - they exist because a grid has four cells, the same
reason closing a cell leaves the launcher standing in it. So `Side by side` /
`Stacked` on a grid of two sessions produce a row of **two**, and on three,
three: the count the person asked about. A full grid loses nothing, because four
live sessions are four somebody opened. An **empty pane in a row is not
touched** - that one was made by `Add pane`, which is a person asking for it in
as many words. And never below one pane: a grid of four launchers turned side by
side is one launcher, not an emptied container.

It is the rule **restore already applies**: `prune_empty_panes` drops empty
panes everywhere except inside a grid. The agreement it buys is about the panes
that **hold something**: a lone empty pane is not restored anywhere, so a grid
of four launchers turned side by side is one launcher now and no panes on the
next launch - the same invitation, drawn by the empty area instead of by a pane.
Without this the two would disagree: a grid of two turned side by side would
become a row of four that came back as a row of two on the next launch, which
is the layout rearranging itself across a restart - the exact thing the grid's
own close rule exists to prevent.

**The orientation control has a third segment and nothing converts itself.** A
row of four is not drawable, and the honest handling is a refusal whose sentence
names the form that would fit - one press instead of zero, in exchange for never
having the screen rearranged under a running agent. The third glyph is a 2x2 in
the same 11x9 box, so the control reads as three pictures of shapes rather than
two shapes and a mode. The action is `layout_grid` and it has **no default
chord**: every unclaimed letter left in a terminal-facing context is one a shell
would rather have.

`PresetLayout` has `Grid` (`"grid"`) so "save current layout as a preset" does
not quietly turn a grid into a column. It is exactly 2x2, not an arbitrary grid,
and an old word is not re-pointed to mean it. `workspace.up` takes `grid` and
falls back to side by side with any number of panes but four, because a preset
cannot invent the panes to fill the other cells.

## Panes are not draggable

**Panes are not draggable, and that is a decision.** A pane is a *place*; the
object is the session, and the object already moves - a rail row drags onto any
pane, and the row's menu says `Show in ‹pane›`. "Swap these two panes" is "show
this one there and that one here", and both halves exist.

The counter-argument the grid raises is answered without a gesture: in a 2x2 the
position is **chosen**, so the destination has to be nameable, and `3rd` names
an index rather than a place the eye can find. `pane_slot_label` says
`top left` / `top right` / `bottom left` / `bottom right` in a grid, in the
grid's own reading order, and leaves the flat forms alone (a side at two, an
ordinal past two, because past two in one direction there is no side).

**What would re-open it:** evidence that a gesture is needed. If so it is a
**swap** and nothing else - the only operation that does not change the form,
where insertion in a 2x2 would force a rule about the third and fourth cells
that nobody could predict - and the grab would be the pane header, which would
then stop being only a label. Both are costs, and neither is worth paying before
somebody has tried the two-action path in a grid whose cells have names.

## Menus, the rename field, and the macOS Edit menu

**A divider in the surface's `⋯` menu is a claim that there is something on both
sides of it.** Three of its four sections are conditional - a document has no
agent operations, a container may have no custom commands, and most surfaces
have neither the "no longer here" note nor a queued prompt - so an unguarded
divider drew, for a file surface, `Copy Path`, `Copy Relative Path`, two rules
in a row, and `Close pane`: a mark that means something, pointing at no content.
Only the last divider needs no guard, because `Close pane` is always under it.
The height the position clamp is asked about counts the dividers and the
`Delete session` row, so the menu does not run below the window edge.

The path section carries `Reveal in file manager` wherever the surface has a
path, and `Open with the default app` for a document. The pair are separate
functions rather than one, because on macOS `open <file>` hands the file to its
application and `open <dir>` shows the directory - one spelling, two meanings -
and the symlink guard lives in `workspace_ops::open_in_default_app`, so two
doors onto one file cannot disagree about whether it is safe to follow.

**The Files tree's own menu says the same things.** One object with two menus
giving different answers is the class of defect to avoid.
`reveal_file_in_file_manager` is the third member of the pair above and exists
for the same reason: a file is not a directory, `open -R` is Finder's own
reveal-and-select, and Windows' `/select,` spells the same thing. A
**directory** gets the reveal and no open row, because opening a folder *is*
revealing it. The open row's label is `file_view::path_opens_in_an_editor`, and
`FilesContextMenu` carries **both** that answer and `is_dir` rather than asking
disk: `render_files_context_menu` runs on every frame the menu is up, so a disk
read in it is a disk read per frame on the thread that draws. Asked on the
right-click it is one gesture-sized head read, the same order as the `stat`
`session_file_exists` makes - and it cannot be deferred to the click, because it
decides the row's *wording*. The reader asks `metadata().is_file()` first, which
is not a tidiness check: a FIFO opened read-only **blocks until a writer
appears**, and a right-click is not a place this UI may hang. The menu's height
is arithmetic (`clamped_context_menu_position` needs one before layout) and
counts the rows it actually draws.

**A rename is a text field.** A hand-rolled `on_key_down` arm that pushes
`key_char` into a `String` and drops every keystroke carrying `control` or
`platform` refuses paste by name, and the arrows, selection, `delete` and IME
with it - while typing still works, because text arrives through the input
handler and needs no action dispatch at all. The slot header's rename is the
shared `TextArea`, the same widget the rail's rename uses -
`Pane::begin_rename` / `commit_rename` / `cancel_rename`, with `commit_rename`
**taking the text** rather than reading the entity back, for the reason
`apply_agents_rename` states: the callback fires from inside the field's own
update. The field is told to `set_submit_on_empty`, because clearing a name back
to the derived one is a real answer and `TextArea::submit` swallows an empty one
by default.

**The macOS Edit menu's `Copy` / `Paste` ask who has focus.** Dispatching
straight to `TerminalCopy` / `TerminalPaste` is right for the one surface that
is usually focused and wrong everywhere else. `bootstrap::route_os_copy` /
`route_os_paste` walk a ladder instead and hand the action to the first
candidate the focused element can actually answer: a text area, a text input, a
rendered document, then the terminal. There are **two doors and two
spellings**: the rendered root's own listener has a `Window` in hand and uses
`*_in`, because asking the app which window is active is a second answer to a
question the caller has already settled; the app-global fallback
(`install_macos_menu_action_fallbacks`) has no window and resolves through
`active_window()`, the same way `App::dispatch_action` does, so the pair cannot
disagree. `is_action_available` asks GPUI's dispatch tree along the path to the
focused node - the same question dispatch itself asks - so nothing is dispatched
into the void. The order is a function with a test
(`the_terminal_is_the_last_thing_asked`) because a wrong order still compiles,
still dispatches, and still looks right in a terminal, which is the one surface
that would keep working.

Worth knowing while reading that code: these menu items do **not** steal the
chord. GPUI derives an item's key equivalent from a keymap binding for the
item's own action, and nothing binds `copy` / `paste` - they are two of the
registry's unassigned entries - so the items carry an empty key equivalent and
only a click reaches them.

## Opening a file in an editor

**`editor.rs` - "open this file, at this line, in the editor the person
chose."** `EditorPreference::from_config` reads `external_editor` and
`open_at_location_with` honours it: `system` goes straight to the OS handler, a
named binary is forced ahead of everything and falls through when it is not
installed, `auto` is `$VISUAL` → `$EDITOR` → a probe list → the OS handler.
`open::that_detached` is used everywhere the OS handler is, so nothing waits on
a launcher.

The named arm **tries the spawn rather than predicting it**. Asking
`resolved != Path::new(name)` as a stand-in for "was it found" conflates two
answers, because `resolve_editor_command` returns an *absolute* path unchanged -
so `external_editor = "/usr/local/bin/myed"`, the most literal way to name an
editor, would read as "not installed" and be silently ignored. `try_spawn`
already logs and reports, and the fall-through is the same either way.

There are two doors onto one preference: `⌘`-clicking a `path:42:7` in a
terminal, and the file surface's `⏎` / `Open in editor`.

## The file surface

**`file_view.rs` is one pane kind for every file.** `FileBody` says which of
three a file is: markdown (rendered through `markdown/`), text (its own lines
behind a right-aligned number gutter, mono, syntax-coloured) or not text at all
(a centred card naming it, with `Reveal in file manager` and `Open with the
default app`, because there is no viewer here and inventing one costs a
dependency to do worse than the OS preview). A file is text if it decodes as
UTF-8 with no NUL in the first 8 KiB - git's own rule - and it is cut at 2 000
lines rather than refused, with the badge saying so. The badge is the extension
uppercased plus `read only`; markdown keeps `markdown · read only`.

**Its colours are the diff's own.** `diff::highlight_lines(text, ext, &syntax)`
returns per-line, line-relative, non-overlapping `(Range, Hsla)` runs - exactly
the shape a read-only view wants - so one file gets the same colours in both
surfaces and neither owns a second grammar table. An incremental editor
highlighter that keeps a tree-sitter tree alive across keystrokes under a parse
budget is the wrong model here: right for an editor, pure overhead for a file
parsed once. So the feature is one call, the grammars already in the build, and
**no new dependency**. The theme snapshot is taken on the render thread
(`active_theme()` polls an mtime) and handed to the background read, as
`diff/view/loader.rs` does; a theme change **re-colours the text in hand**
rather than re-reading the file, because a reload would throw away the reader's
selection and scroll for a reason that has nothing to do with the file.

**Text is picked by the character** - click, drag, shift-click to extend,
double-click for a word, triple-click for a line - and `⌘C` copies the picked
bytes without the gutter, falling back to the whole file when nothing is picked.
GPUI exposes no character offset for a point inside a `div`, **but `StyledText`
does**: it keeps a `TextLayout`, which answers `index_for_position` and
`position_for_index`. The lines are `StyledText` for the colouring, and the
hit-testing comes with them - no custom `Element` and no borrowed editor.

The selection is painted as a `background_color` **highlight run merged into the
syntax runs** (`merge_line_styles`) rather than as a quad positioned from
`position_for_index`: the glyphs and the band come out of one shaping pass, so
the band cannot be a pixel or a frame out of step with the characters it is
behind - and a selection over half a keyword keeps the keyword's colour.
Double-click takes a **programmer's** word (alphanumeric plus `_`), deliberately
not `unicode_word_indices`, which keeps a dot between letters inside a word for
abbreviations and so takes `foo.bar` whole. Asking a line's layout is only safe
for a line that painted, which is exactly the line that received the event. The
per-line `on_mouse_move` listener is hung only while a drag is in progress,
because a file draws up to 2 000 rows. A reload clears the selection - line 40
of the old file is not line 40 of the new one, and the watcher fires on every
save an agent makes; `clamp_to_line` is the second guard, because slicing a
`str` off a char boundary panics and `with_highlights` debug-asserts all three
of its bounds. A **rendered** markdown document has no selection at all, having
no lines to point at.

**`read only` is settled, not deferred.** The promise the badge makes is that
*this app has no object you can lose*, which is why closing a pane needs no
confirmation, why `⇧⌘W` is safe and why the empty state offers two buttons and
no warnings - an editable surface would introduce the first losable object and
reopen every one of those decisions. The case an editor serves, where the edit
is faster than the sentence, is served by **`⏎`** (`open_file_in_editor`),
which hands the file to the editor the person already chose; the surface's `⋯`
says the same thing in a row, and says `Open with the default app` instead for a
file that has no editor to be opened in (`opens_in_an_editor`).

**Leaving the app is the one exception, and only while an agent is running.**
Every process the app started ends with it, on purpose (`agents::parent_guard`:
an orphan would keep spending tokens with nobody watching), and the next launch
brings each agent back with `--resume` in its own pane. So the conversation is
never the losable object - the turn in flight is. Every door out of the process
(`Quit`, `CloseWindow`, both close buttons, an update-pill click that restarts
or hands over to the Windows installer) goes through
`SplitlaneApp::request_exit` (`app/exit_guard.rs`), which leaves at once when no
agent is `running` and otherwise names the sessions and asks. `running` only:
`waiting for you` is visible and a person's to answer, `starting` has sent
nothing. A repeat of the same door does **not** confirm - a held `⌘Q`
auto-repeats, and a repeat answering a question nobody has read yet is the
failure the card exists to prevent. What reopens this: sessions that outlive the
app (a PTY host process), which would make quitting lose nothing at all.

The chord's context is `Markdown && !MarkdownSearch`, because the find bar
layers onto the same node and `enter` there belongs to `markdown_find_next`. It
is a *predicate* context rather than a bare word, which is why
`every_default_row_becomes_a_binding` exists: `make_binding` answers an
unparseable predicate with a `warn!` and a `None`, which for a shipped row means
the chord is simply absent with nothing to see.

## A launch with nothing to restore opens no project

**The rule.** When there is no container to restore - a first launch, or a
session in which the person closed every project - the app opens a container
only for a directory a person *named*, and the one way to name it at launch is
to start the app from a terminal standing in it
(`launch_cwd::first_container_cwd`, asked of `stdin` being a terminal). A launch
from Finder, the Dock, a `.desktop` file or the Start menu names no directory,
and gets the no-project state: the rail's "No projects yet" line and the content
area's welcome, whose `Open a folder` dispatches the same `new_workspace` action
as `⇧⌘O` and File → New Project.

**The case that forced it.** Answering "nothing to restore" with a container for
the inherited directory means, from the desktop, the home folder (`/` on macOS,
promoted to `~` by `implicit_launch_cwd`). On a fresh machine the first screen
was a project named after the user's login, reading `no repository`, `not a git
repository` and `not restorable` - three negatives as a first impression - and
the first `claude` run there, made only to log in, raised a queue of permission
prompts naming this app: Desktop, Documents, Downloads, the Photos library,
Music. None of them is the app's own read; macOS attributes what a pane's child
touches to the app that spawned it, and an agent that indexes its working
directory walks all of `~` when that directory *is* `~`. The home folder is not
a project, and making it one handed every agent started there the whole of it.

The comparison is the kind of application this is. A terminal (Terminal, iTerm,
Ghostty) opens a shell in `~` because it has no notion of a project; an
application built around projects (Zed, VS Code, JetBrains) opens to a screen
that asks for one and never invents it. The rail lists projects, an agent runs
in its project's directory and the git facts come from there - this is the
second kind.

**What would re-open it.** Evidence that people launching from the desktop want
a shell before they want a project. The answer then is a shell that is *not* a
project - a surface with no directory claim - rather than bringing back `~` as a
container. On Windows a GUI-subsystem binary has no console on `stdin` even when
started from one, so a terminal launch there also opens no project; that is a
missing convenience, not a wrong project.

## A session works through the sessions it opened

**The rule.** An agent in a pane may open agent sessions (`surface.add_agent`,
`splitlane add`) with nothing switched on, and what it opens is its own: the
record of each carries `opened_by`, the id and name of the session that opened
it. Three limits come with that, all checked on the server:

- a session that was itself opened by a session opens none;
- one session has at most eight it opened running at a time;
- a caller that is in no pane is not covered by any of this, and is let in by
  the variables the app was launched with, as before.

**Who is calling is read, not asked.** Every pane exports its surface id to the
processes it runs, and a request could repeat it. That would be the caller
describing itself, and any process of the same user can set a variable. What a
process cannot choose is whose child it is. The server takes the pid on the
other end of the connection (`ipc::peer_pid`), reads the process table once,
and walks that pid's ancestors; the pane is the one whose PTY child is among
them (`caller::pane_of`). The walk runs on the connection's thread, because it
blocks and the handlers run on the thread that draws the window.

Two cases are refused rather than guessed at:

- **A process that left its tree.** After a double fork its parent is init,
  and nothing above it names a pane. It is "in no pane", not "in the pane it
  started in": the second reading would let anything that was ever started in
  a pane keep that pane's rights after the pane's agent had finished with it.
- **A pid that changed hands.** The peer can exit after sending, and the
  number can be given to another process before the request is looked at. The
  process's start token is read when it connects and compared against the
  table at request time; a mismatch places the caller nowhere.

**Why it can be on by default.** The capability this replaces was one right:
write into any pane. It had to be switched on by a person, and it had nowhere
safe to be remembered - every place a confirmation could be kept is written
with the same rights as the file that asked for it. So it reset on every
launch, and orchestration, which needed it, did not survive a restart.

Narrowing the right removes the need to remember anything. A session that can
only reach what it opened gains nothing it did not already have: an agent can
start a second agent by typing its name into its own shell. Doing it through
the app makes the second one a row in the rail, counts it against a limit, and
records where it came from. The harm a forged request could do is the harm the
agent could do anyway, so no confirmation stands in front of it.

**Why depth is one and the count is eight.** Both bound the same failure: an
agent in a loop that opens sessions. Without a depth limit the bound on eight
is a bound on eight per level. Eight is as many rows as a person keeps track
of under one opener; every Claude Code session opened this way also draws on
the same usage window as the one that opened it.

**A session with no pane to go to is opened without one.** The ordinary ladder
ends on "the focused pane, replaced", and the focused pane is as a rule the
caller's own - an agent opening five sessions would push itself and four of
them off the screen. So opening has a ladder of its own
(`orchestration::place_opened`): an empty pane, then a new pane where one
fits, then the rail. A session in the rail is not hidden: it has a row, a
status, a place in the attention queue, and the state pass reads it like any
other. It starts its process at once, at the default grid size, because
nothing will ever draw it to trigger the start. Neither arm takes the keyboard
or changes which project is on screen.

**The first prompt waits for the agent, not for the pane.** The launch command
is typed into the pane's shell, so for the first moments the shell is what
reads input. The prompt is written once something speaks for the agent - its
own file, read by the state pass, or a hook frame - and the screen has
settled. If nothing speaks for it within half a minute the prompt is dropped
and a warning is logged: an agent that did not start leaves a shell at the
other end, and text written to a shell is a command. The caller is not told,
because its request was answered when the session was created; what it sees
is a wait for the first turn that does not return a turn. When the opener is a session, the text is preceded by one line,
`[Splitlane] Sent by the agent session "<name>", not typed by a person.` It
is plain text in the receiving agent's own transcript, so the agent, a person
watching the pane and anyone reading the conversation later all learn the same
thing from it. It is not markup: a tag shaped like a vendor's own would be an
intervention in a format this app does not own.

**Writing follows the same line.** `surface.send_text` and
`surface.send_keystroke` let a session type into the sessions it opened, with
nothing switched on (`orchestration::may_write`). Text sent that way carries
the line above; a key carries nothing, having nowhere to put it. Into a
session another session opened, or its own opener, a session does not write:
there is no correspondence between sessions, only a task handed down. Into
one a person opened it writes only with that person's leave, which has a
section of its own below. The case that forced this is the plainest
complaint about agents sharing a window: a session answering a question that
was asked in another.

**A session that is waiting for a person is not typed into by any caller in a
pane.** Not by its opener either, and not when the launch variable is set. The
question on its screen was put to a person, and Esc on a permission ask is the
answer "no". A rule that stopped text and let keys through would leave exactly
that door open.

**The launch variable still opens what it opened.** `SPLITLANE_IPC_SCRIPTING=1`
is set by the person who starts the app and cannot be changed from a pane, and
scripts and flows are written against it. A write it lets through is
unchanged, byte for byte: no line is added, because the line needs a paste and
a shell in the middle of a command does not take one. The one thing that
changed under it is the waiting rule above.

`surface.send_text` also answers with `rail_status_at_send`, the rail's word
for the target at the moment the text went in. Measured on Claude Code
2.1.289: text sent while a tool runs is taken into the turn in flight, and
text sent while the model is generating becomes a second turn straight after
the first, whose answer is then the only one a later read returns. A caller
that wrote into `running` has to know it did.

**Stopping a turn is a key, and it is confirmed by the agent's own record.**
`surface.interrupt` writes the key that stops a turn of that agent and leaves
the agent running - Esc for Claude Code, and not Ctrl-C, which in some agents
ends the process on a second press. The key is a property of the agent
(`TerminalAgent::interrupt_key`), filled in only from a measurement; an agent
with none is refused with `no_interrupt`. The grounds are those of any key
(`orchestration::may_interrupt`), with one difference: a session never stops
its own turn, whatever the app was launched with, because the call would be
part of the turn it stopped. A session waiting for a person is refused for
the reason a key is: on a permission question, the key that stops a turn is
the answer "no". With no turn running nothing is written, since the same key
on an idle input line means something else to the agent.

The call answers as soon as the key is written. Whether the turn stopped is
read afterwards from the rail by whoever asked (`splitlane interrupt` does
it): the state pass holds the end of a run for three seconds before it
believes it, which is longer than a handler on the thread that draws the
window may take. The run ends with `last_outcome: "interrupted"`, taken from
the record the agent wrote, and that word past the baseline is the
confirmation. Silence is not one: an agent still generating is silent too.

Measured on Claude Code 2.1.292, through this call: a first turn stopped
during generation and a later turn stopped during a tool call were both
confirmed in five to six seconds, the tool's child was gone, the agent
stayed, and text sent afterwards reached the transcript as itself.

**A stop before the first token leaves the prompt behind, and the app
removes it.** Asked the moment the rail first said `running`, the key landed
before the model had answered. Claude Code wrote nothing to the transcript
and put the prompt back on the input line; the next text sent was appended
to it and submitted as one prompt. A caller that redirects a session - stop,
then a new task - would have handed it the old task and the new one glued
together.

The case is told apart in the agent's own file: a prompt with nothing after
it, under a status that says the turn is over (`read_turn_end` in the state
pass, from `TranscriptProbe::unanswered_prompt`). Such a run ends as
`interrupted` like any other stop, for a person's Esc as much as for a
caller's. The line is cleared only when a caller asked for the stop within
the last half minute (`clear_returned_prompt`): a person who pressed Esc
themselves gets their prompt back to edit, which is what the agent meant by
returning it. The key is the agent's own (`TerminalAgent::clear_input_key`),
measured: for Claude Code, Ctrl-U removes one line of a three-line prompt
and Ctrl-C removes all of it. Ctrl-C on an empty line is the first half of
quitting, which is why it is never pressed on a guess. It is written in the
same pass that counts the run, before the count is published, so the text a
waiting caller sends next finds the line empty. Measured end to end on
2.1.292: the stop was reported `interrupted`, and the next text reached the
transcript as itself.

`not_confirmed` is what is left: the key was written and the run did not end
as stopped - it finished by itself a moment earlier, or the time ran out.

**A session arranges the panes of what it opened, and no others.** With eight
sessions and four panes, opening and closing are not enough: a session has to
be able to free a pane without ending the work in it, and to bring a session
that has none back onto the screen. `surface.park` does what closing a pane by
hand does - the session leaves the layout and goes on running in the rail.
`surface.show` gives a session a pane: an empty one, then a new one where one
fits, and failing both, the pane of another session **of the same opener**,
the one that has gone longest without focus (`orchestration::pane_to_give_up`).
The opener's own pane and a pane showing anything it did not open are never
candidates, and with no candidate the answer is a refusal. A person's layout
is theirs; a session that could take a pane of theirs to show its own work
would be deciding what they look at. Neither call moves the keyboard.

"Longest without focus" needed state that was not there: the app remembered
the focused pane and the one before it, which is an order over two. It now
keeps a count per pane, written where the pair is (`track_pane_focus`).

**Closing is "Delete session", and it is the opener's alone.** The interface
has two actions that could be called closing: closing a pane, which parks the
session, and Delete session, which stops the process and drops the row. For an
opener only the second is worth the name - a session left running without a
pane spends out of sight, which is the thing an opener closes sessions to
prevent. `surface.close` goes through `remove_thread`, the path the menu
takes. The conversation survives in the agent's own history, as it does for
the menu.

**Stopping the session means stopping the agent, and closing the PTY did not
do that.** The teardown signals the process group of the pane's shell. An
interactive shell gives each foreground job a group of its own, so the agent,
whose launch command was typed into that shell, is in a group the signal
never reaches. Measured on macOS: after "Delete session" the shell sat
exiting and the agent and its shim were still running minutes later, with a
turn in flight running to its end unseen. `remove_thread` therefore reads the
groups under the shell while the tree still names them
(`process_tree::JobGroups`), closes the PTY, and signals those groups: SIGTERM,
then SIGKILL half a second later. The read has to come first, because once
the shell is gone its children hang off init and nothing says whose they
were. Closing a project goes the same way for each of its sessions, from the
interface and from `workspace.close` alike; the second used to leave the
sessions in the surface cache, running and listed, with no project. A group
is recognised at signal time by any member it was read with, not by one: the
first signal can end the shim that leads the group while the agent it wrapped
is still there for the second.

What stands in front of it (`orchestration::may_close`), in the order asked:
the caller's own session is never the target; a session in another project is
out of reach; a session the caller did not open is refused - and for one a
person opened there is no flag, variable or later permission that changes
that. Last, a session whose rail word is `running` or `waiting` is closed
only with `stop_turn`: the turn is what would be lost, the same line Quit
draws when agents are working. Before that last one, a session whose row
carries the mark of a finished run nobody has looked at is refused with
`unseen_by_person`, and `stop_turn` does not lift it. The mark is put there
only when the last message was a person's: they typed into a session an
agent opened, the answer came, and the mark is the one thing telling them it
is there. A run the opener started leaves no mark, so the opener closing its
own finished work is never stopped by this. A dialog asking the person to confirm was
considered and refused, because it would make a request wait on a person's
answer, and the server has no call that waits on a person.

**What a session opened sits under it in the rail.** A session and the
sessions it opened are one piece of work, and rows scattered through a
project by age and by pin do not read as one. So they follow their opener,
first opened first (`agents_sidebar::under_their_openers`) - the one place
the rail is not newest-first, because it is a list of what was handed out. A
pin on the opener takes them with it; a pin on one of them does not lift it
out. The pane of each says `opened by ‹name›` in its header, with the
opener's name as it is now. When the opener is closed its last name is
written into their records, since from then on that is the only place it
exists, and the header says `opened by ‹name› · closed`; with nothing to sit
under, those rows keep their own places.

**What it does not claim.** A process of the same user that runs code inside a
pane is, as far as the process table can tell, that pane - and it is: that is
what the agent in the pane does. The rule places a call; it does not vouch for
what the caller was told to do.

**What would re-open it.** A platform where the peer's pid cannot be read
leaves every caller "in no pane", which is the launch-variable path and no
worse than before. The Windows call (`GetNamedPipeClientProcessId`) compiles
and has not been run against a live pipe.

## There is no switch that lets an agent write into any pane

**The rule.** Free access - a switch in Settings -> Agents, backed by the key
`ai_unrestricted` in `splitlane.json` - is gone. Nothing replaces it as a
setting. An agent in a pane writes into the sessions it opened, by default
("A session works through the sessions it opened", above); into a session a
person opened, an agent writes only after the person was asked and said yes
("A session a person opened is driven only with their leave", below). A
person's own script outside a pane is let in by the variables the app is
launched with, as it always was.

**What the switch was, and what it protected.** One right: any caller on the
socket may type into any pane without the launch variable. It could not be
granted by the file alone, because the file is reloaded while the app runs
and an agent allowed to edit files could write `"ai_unrestricted": true` and
then type into every other pane. So the key was a request, the grant was a
switch a person turned on, and the grant lived only in the running process -
every place it could have been saved is written with the same rights as the
file that asked.

**Why it was removed rather than fixed.** That last property is what made it
unusable for the thing it was for. A grant that resets on every launch means
orchestration stops working after a restart until someone remembers a switch,
and remembering it safely needs a store an agent of the same user cannot
write, which Linux and Windows do not have. The way out was not a better
store but a narrower right: once the server can tell which pane a call comes
from, "a session reaches what it opened" needs no grant at all, because it
gives an agent nothing it could not already do by starting a second agent in
its own shell. With that in place the wide right had no user left.

**What a file that still has the key does.** Nothing. The key is not read,
and a file containing it loads as before; unknown keys are ignored.

**What would re-open it.** A need for agents to write into sessions a person
opened. The answer to that is not a switch set ahead of time but a question
put to the person at the moment it matters, for that one session - the next
section.

## A session a person opened is driven only with their leave

**The rule.** When a session writes into an agent session a person opened, in
its own project, nothing is written and the person is asked: may this session
send messages to that one. The answer is for the pair and lasts until the app
is quit. With a yes the session writes into it as it does into one it opened -
text, keys and the key that stops a turn - and the text carries the same line
saying an agent sent it. It never closes it and never moves it: those stay
with what a session opened itself. With a no it is refused
(`person_declined`) and the person is not asked again.

A shell is never asked about, because text written to a shell is a command. A
session that was itself opened by a session asks for nothing: it was given a
task, not a plan. A session waiting for a person is refused as before, leave
or no leave, and nobody is asked about it while it waits.

**The call does not wait for the answer.** A person can take any time to
give one, the server has no call that waits on a person, and the session
that asked has a turn of its own to get on with. The write comes back at once
with its own code - JSON-RPC `-32005`, reason `asked_person`, exit 9 - which
is neither done nor refused. `surface.status` answers `drive` for the caller
and the target (`asked`, `allowed`, `declined`), and `splitlane wait --until
allowed` waits on that word. A write repeated while the question stands asks
nothing new.

**The question is an ordinary wait, on the session that asked.** While it
stands that session's row says `waiting`, with what it wants as the message,
so it is in the rail, the attention queue and `Activity` like any session
that needs a person. It is answered where every wait is answered, in the
waiting session's own pane: a strip between the header and the terminal,
`Allow` and `Don't allow`, one row for each session asked about and no
button that answers them all - the point of asking per pair is that nobody
says yes without looking. It is not in the target's pane, which may not be on
screen and is not the one waiting.

The agent's own state knows nothing of the question; it goes on saying
working or idle. So the state pass takes the reading as a wait for as long
as a question stands (`apply_agent_states`), rather than writing the word
over the status afterwards: a word rewritten after the fact is read back on
the next pass as a run that started and ended, and was announced as one.
For a session only a hook speaks for, the word is put when the question is
and what it replaced is put back with the answer.

**A person can hand a session over without being asked, and take it back.**
The row's menu offers `Let ‹name› drive` for each session that could ask -
the agent sessions of the same project that a person opened - and, for a
session that is driven, `Stop ‹name› driving`. Handing over is the same as
answering yes, and it is the one thing that lifts an earlier no: the person
can change their mind, the agent cannot change it for them by asking again.
Taking back is not a no. The pair is forgotten, the session's turn is not
stopped, and the other session may ask again. A session is driven by one at
a time. There is no dragging a row onto another to do this: a drop already
means "show it in that pane", and a third meaning told apart only by where
the pointer was let go would hand a session to an agent by a slip of the
hand.

A driven session stays where it is in the rail. It says `by ‹name›` where
its agent's name was and `driven by ‹name›` in its pane's header, so a
person looking at it can tell that what is typed there may not be theirs.
`surface.list` and `surface.status` carry the same fact as `driven_by`.

**Nothing is printed into the asking session.** The designs show a line in
its terminal saying what the person answered. With no call in flight that
would be the app typing into an agent's session on its own, so it is not
done: the session learns the answer from the wait, or from its next write.

**Why it is kept in memory only.** The same reason the switch was removed:
every place an answer could be saved is written with the rights of the agent
that would benefit. A pair goes when either session is closed - a closed
session drives nothing - and all of them go when the app is quit.

**With the launch variable set, nobody is asked.** `SPLITLANE_IPC_SCRIPTING=1`
is the person's own choice at launch and opens what it always opened, so a
write it lets through neither asks nor is stopped by an earlier no.

**What it does not claim.** The word a refusal is made from is the rail's, and
the rail is sampled: a session that began waiting for a person within the
last two seconds can still be written into. That is true of every write this
app lets through and is not new here.
