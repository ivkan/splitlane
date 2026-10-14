# Changelog

Notable changes to Splitlane are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- On macOS and Linux, agent sessions outlive the window: every pane's terminal
  session runs in the session host, a process of Splitlane's apart from the
  window. Quitting with the sessions kept closes the window and leaves them
  working; an update and a crash of the window leave them running too. The
  next start shows each session in its pane again, with what it printed
  meanwhile, and a card offers to open or end any session no pane shows. On
  Linux, keeping is now the default when quitting, as on macOS: the window
  closes instead of being minimised. `"session_host": false` in
  `splitlane.json` runs the panes in the window as before. Windows is
  unchanged.

### Changed

- The fleet skill tells a lead agent that `splitlane wait` prints the
  session's state as it returns, and not to check it again with `status` or
  loop until such a check passes.
- The fleet skill no longer takes every failure of its first command for
  Splitlane not running. Only a message that says so means that; after any
  other the lead tries `splitlane ls` and reports what went wrong.

### Fixed

- Quitting with only shells open ends them, as it was meant to. A shell row
  counted as an open agent session whenever its terminal was alive, so
  closing such a window kept it running; now a shell counts only while an
  agent runs in it.
- A lead agent is told that a session it opened has finished when that
  session answered within seconds, while the lead was still in the turn that
  opened it. A lead whose wait sat inside a script that kept running was
  never told in that case.
- `splitlane --help` lists every scripting command with a line on what it
  does. It named five of twenty-one, and the error for a mistyped command
  sent the reader there for the list.
- The context meter in a pane's header no longer shows a ceiling far below
  the real one (`219k / 217k` for a session a quarter full). Claude Code
  also compacts a session that has sat idle for most of an hour, at whatever
  size it is, and that was taken for the context window filling up. Only a
  compaction that interrupts work measures the ceiling now; until one does,
  the meter shows a count.

## [0.2.7] - 2026-10-13

### Added

- Quitting no longer has to stop your agents. With agent sessions open, Quit
  and the window's close button put the window away and leave every session
  working - hidden on macOS, minimised on Linux and Windows - and starting
  Splitlane again brings it back. On macOS this is the default; on Linux and
  Windows the app asks first. **Settings > General > When quitting**
  (`on_quit`: `keep`, `stop` or `ask`) chooses. **Quit and Stop Sessions**
  (`⌥⌘Q` on macOS, `Ctrl+Alt+Q` elsewhere, `quit_and_stop`) ends the app and everything it started.
  The kept sessions live in the app's process: an update, a crash or a logout
  still ends them.
- Starting Splitlane while it is already running brings its window forward
  instead of refusing with an error.
- A session a script opened is reachable with its key after Splitlane
  restarts, while it is still a row with no terminal: the call that presents
  the key selects it. `send` to such a session starts its agent and writes
  once the agent takes text.

### Fixed

- A lead agent whose own waiting went wrong is still told that a session it
  opened has finished. The line Splitlane writes into the lead was cancelled
  whenever something had been waiting on that session or the lead had asked
  for its status, even when the lead never saw the result - a wait wrapped in
  a script that kept running left the lead idle beside a finished session.
  The line is now only held back: dropped once the lead is at work, written
  if it is still idle a minute later.
- On macOS, `splitlane ps` and every other command whose answer is longer
  than 8 KiB failed with `invalid JSON-RPC response ... EOF while parsing`:
  the app sent the first 8192 bytes and closed the connection. It showed up
  once enough agent sessions were open for the list to grow past that size.
- `send` to an agent session that has not been started since Splitlane was
  no longer writes at once, into the shell the agent is being launched from,
  where the text was lost or ran as a command. It is kept until the agent is
  ready, and the answer says `queued`.

## [0.2.6] - 2026-10-11

### Added

- A script or a scheduled job can start an agent on a task with nothing set
  where Splitlane was launched: `splitlane add` from a directory of a project
  that is open. The session opens in that project, its row says `by` and the
  name given with `--as`, and `add` returns a key (`session_key`) that lets
  the program `send` to it, stop its turn and `close` it, as `--key` or in
  `SPLITLANE_SESSION_KEY`. Eight such sessions run at a time. On by default;
  `"script_sessions": false` or Settings > Agents turns it off.

### Changed

- `splitlane add` from outside a pane opens the session in the project its
  current directory is in, not in the project on screen. With
  `SPLITLANE_IPC_ORCHESTRATION=1` a call from no project still gets the one
  on screen.

### Fixed

- Codex sessions report their state again. Codex now runs a session in a
  background server, where its hooks no longer reach Splitlane, so a Codex
  session showed neither `running` nor the end of a turn, `splitlane wait`
  could not wait for it and `splitlane answer` had nothing to read.
  Splitlane now finds the session's file by the directory and the time the
  pane was launched. Two Codex sessions started in the same directory within
  the same few seconds may still stay without a state.
- `splitlane` run by a Codex session is recognised as coming from that
  session's pane, so Codex can open sessions and drive the ones it opened
  like any other agent. Codex runs its commands outside the pane's process
  tree and they were refused as calls from outside. On macOS this still needs
  Settings > Agents > Let Codex drive sessions, which lets Codex's sandbox
  reach Splitlane.
- A finished run is no longer reported as about three seconds longer than
  it took, so a turn just under ten seconds no longer sends a notification
  meant for longer ones.

## [0.2.5] - 2026-10-11

### Added

- A session is told when a session it opened ends its turn. If the opener is
  not already waiting on it with `splitlane wait`, Splitlane writes one line
  into the opener and submits it, once the opener is idle and its input line
  is empty, so the work carries on without you relaying the news. On by
  default; `"tell_opener": false` or Settings > Agents turns it off.
- A Codex session can open and read other sessions on macOS. Codex's sandbox
  blocks Splitlane, and Codex can allow it only together with network access
  for its commands, so this is off until you turn it on: Settings > Agents >
  Let Codex drive sessions shows what it costs and asks first, then adds a
  permissions profile to Codex's config. Turning it off removes the profile.
- `splitlane` run by Codex inside its sandbox says that the sandbox blocked
  it and where to allow it, instead of asking whether Splitlane is running.
- Notifications about an agent waiting for you, a crashed or stalled session
  and a limit running out arrive with the system's notification sound. A
  finished run stays silent. Settings > Notifications > Play a sound chooses
  between never, these, and every notification
  (`agent_panel.notify_sound`). The sound follows the system volume and Do
  Not Disturb.

### Changed

- Status colours are swapped: a working agent is green and an agent waiting
  for you is amber, everywhere a state is shown. Amber now means only "look
  here" - a waiting agent, or a usage limit about to run out - and no longer
  marks a linked worktree, the `+ worktree` button, search marks on the
  scrollbar or the "Enter queues" note.
- Focus is shown without colour: the focused pane's border, the focused row in
  the sidebar and the row selected by keyboard in the launcher and the command
  palette are neutral instead of green.
- The Activity chip is tinted only for a failed run. Waiting shows as an amber
  dot and an unread result as a green ring on the plain chip.
- A working session whose agent has not been heard from for as long as the
  "may be stuck" notification waits says `no activity 14m` in the sidebar and
  the pane header, in place of the agent's name. Its dot does not change.
- The Stop button in a pane header is a plain hollow square instead of red.
- In the launcher's list of open sessions a failed session is marked in red;
  it used to look like an idle one.
- The count of background commands beside an idle session (`idle · 1 shell`)
  is drawn in the colour of a running session, in the sidebar and in the pane
  header. It was in the idle grey and easy to miss. The session still reads
  as idle: the word and the dot keep their tone.

### Fixed

- The context meter in a pane header no longer reads full after a session
  compacts its own context. It showed the last turn before the compaction
  until the agent answered again, which could be minutes or hours later.

## [0.2.4] - 2026-10-11

### Fixed

- A pane could be left with its terminal one column wide while the pane
  itself was full width, so an agent working in it drew one character per
  line. Seen in one session, each time a prompt was submitted and until the
  turn ended; the cause is not established. A size too small for one cell is no
  longer sent to the terminal, on macOS the size the terminal holds is
  compared with the size sent and sent again when they differ, and every
  size sent is recorded in `terminal-resize.log` in the app's data directory.

## [0.2.3] - 2026-10-10

### Added

- A question Claude Code or Codex asks before it takes its first prompt -
  whether to trust the folder, how to sign in, which settings to restart
  with - now shows as `waiting for you` in the rail, counts in Activity and
  sends the "needs input" notification. The row used to say `idle` over an
  agent that would take no prompt. Scripts see `rail.status: "waiting"` with
  the new `rail.source: "terminal"`, and `splitlane wait --until turn-end`
  exits `5` for it.
- `splitlane add` with a prompt now returns once the prompt has been written
  or given up on, and says which: `opening_prompt` in its output and in
  `rail`, exit `5` when the agent was asking a person something first, `7`
  when no agent was there to take it. It used to exit `0` in every case.
- Updates can be checked for at once: **Check now** in About Splitlane,
  **Check for Updates** in the application menu on macOS, and `Check for
  updates now` in the command palette. About says what the last check found
  and how long ago. Until now the app asked at startup and every four hours
  and said nothing about it, so a window left open across a release could
  not be told to look.

### Fixed

- `splitlane send` from one session to another is refused when the other
  session's agent has exited. The text went to the shell left in that pane,
  and with `--submit` the lines of a task were run there as commands.
- A Claude Code session opened by another session with `splitlane add
  --submit` no longer declines its task as "pasted text from an agent
  session". The task is now given to Claude Code as it starts, and text sent
  to it afterwards with `splitlane send` is typed instead of pasted. Claude
  Code marks a paste as text that may not be the person's, and with the line
  Splitlane puts in front of a task from another session, about half of the
  sessions measured did nothing at all. A prompt given this way also
  survives a question the agent asks first, such as whether to trust the
  folder. Not on Windows, where the task is pasted as before.
- A prompt given to `splitlane add --agent codex` is written. Codex reports
  nothing before its first prompt, and the prompt waited for a report that
  could only follow it, so it was dropped after 30 seconds with the command
  long since answered `0`.
- The context meter in a pane header no longer shows a count above its own
  ceiling, such as `334k / 228k`. The ceiling is the size the session last
  compacted at, and a session that has grown well past it without compacting
  has a larger window than that; the meter goes back to a plain count until
  the next compaction measures it.

## [0.2.2] - 2026-10-10

### Changed

- About Splitlane states that the program comes with no warranty and may
  be redistributed under the GNU General Public License, and links to the
  license text.
- The fleet skill now tells a lead agent to start `splitlane wait` in the
  same turn as it hands out a task, in the background where its harness
  reports a background command ending, and not to end its turn without one.
  A lead that opened a session and stopped used to hear nothing when that
  session finished. The skill also leads with sessions the agent opens
  itself, says to judge a session by `rail` and not by `hooked`, and says
  that a finished turn can be a question. Installed copies are replaced at
  the next start.

### Fixed

- `splitlane ps` lists agent sessions - those opened from the launcher or
  with `splitlane add` - and `splitlane status` reports their state. `ps`
  used to answer `(no agents)` and `status` `idle` while such a session was
  in the middle of a turn, because both read hook sessions only. Each `ps`
  row now carries the same `rail` object as `status`.
- `splitlane read` returns the screen as well as the history above it. It
  used to return the history alone, so a pane that had not yet scrolled - a
  fresh shell, an agent stopped at a start-up question - read as empty, and
  `splitlane wait --pattern` could not see a line until it had scrolled off
  the screen. The MCP `read_pane` tool reads the same way.
- A pane keeps the name scripts address it by across a restart. Names given
  by `splitlane up`, `split --name` or `rename` came back as
  `terminal@<project>`, so a script written against `left` and `right` found
  neither. The status bar's "restored" line also counted every pane twice.
- A resumed Claude Code session is no longer shown as working, then marked
  as having finished a run, when the app starts. It happened when the end of
  its transcript held a record too large to read.
- On Linux under X11, "Found no xinput mouse pointers" is logged as a
  warning with its cause, not as an error on every start under a virtual
  display.
- A Codex session whose `CODEX_HOME` points somewhere other than `~/.codex`
  is followed like any other. Its record was looked for in `~/.codex` only,
  so the rail never learned that a turn had ended and `splitlane wait
  --until turn-end` exited `7`. The variable is the one Splitlane itself was
  started with.
- The rail's Settings button closes its menu as well as opening it, and so
  does Escape. A second click used to leave the menu open, and Escape went
  to the agent in the terminal instead.

## [0.2.1] - 2026-10-09

### Fixed

- On macOS the app could not update itself: it downloaded the new version
  and then said "Update failed". The updater checked the download against
  the wrong signing team and so refused every release. Versions up to 0.2.0
  cannot get past this by themselves - install this one from the `.dmg` or
  with `brew upgrade --cask splitlane`; from here on the in-app update works.
- The menu behind the rail's Settings button no longer has a line drawn
  across one of its items.

## [0.2.0] - 2026-10-09

### Added

- An agent in a pane can open agent sessions and work through them, with
  nothing switched on: `splitlane add` opens one in the caller's project,
  `send` and `key` write into it, `park` and `show` take it out of a pane and
  put it back, `close` stops it. Each of these works only on sessions the
  caller opened. One session may have eight it opened running, and a session
  that was opened by another cannot open any. Splitlane works out which pane
  a call comes from by itself, from the calling process.
- Text one session sends into another starts with a line naming the sender:
  `[Splitlane] Sent by the agent session "<name>", not typed by a person.`
- `send` reports `rail_status_at_send`, the target's status when the text
  went in.
- `rail.background_shells`: how many background commands a Claude Code
  session started are still running while it is idle. The pane header and
  the rail row say `idle · 1 shell` for such a session.
- `wait --until turn-end --settled` also waits out those commands and the
  turn that follows them, for a session that said it would carry on after
  its tests. Without the flag the wait returns at the end of the turn, as
  before.
- Exit code `8` (JSON-RPC `-32004`): a rule refused the request and nothing
  was done. The reason is in the message and under `error.data.reason`.
- `splitlane interrupt <target>` stops the turn a session is in and leaves
  the session open for a new task. It presses the agent's own interrupt key,
  and reports `interrupted` only when the agent's own record shows the turn
  was stopped; `not_running` when there was nothing to stop, `not_confirmed`
  otherwise. Claude Code only for now: other agents are refused with
  `no_interrupt` (exit `7`). `rail.last_outcome` has a third word,
  `interrupted`.
- `wait --until turn-end` reports a turn that was stopped before it finished
  as `interrupted`, with exit code `10`, whether a person pressed Esc or
  `interrupt` did. It used to say `finished` and exit `0`, which sent a
  script on to read an answer the stopped turn never gave.
- An agent can work through a session **you** opened, once you say it may.
  When it sends to one, nothing is written and its own pane shows a strip
  under the header - `plan wants to send messages to api`, with `Allow` and
  `Don't allow`. Your answer holds for that pair of sessions until you quit
  Splitlane. The agent's call returns at once with exit code `9` (JSON-RPC
  `-32005`, reason `asked_person`), and `splitlane wait --until allowed`
  waits for your answer. After a no it is refused with `person_declined`.
- A session's row menu has `Let <name> drive` to hand it to another session
  without being asked, and `Stop <name> driving` to take it back. A driven
  session says `by <name>` in the rail and `driven by <name>` in its pane's
  header; `surface.list` and `surface.status` carry `driven_by`, and
  `surface.status` carries `drive` for the caller.
- `drive` says `not_offered` for a session nobody would be asked about on
  the caller's behalf, and `splitlane wait --until allowed` ends on it at
  once with exit code `1` instead of waiting out its timeout.
- The sessions a session opened sit under it in the rail, in the order they
  were opened, and each one's pane says `opened by <name>`. A bracket in the
  rows' margin joins them to their opener. The opener's row shows how many it
  opened; clicking that count folds them away, and the folded row then says
  what they are doing (`1 waiting · 6 running`). When the opener is closed
  and some of its sessions are left, a line `opened by <name> · closed`
  stands in its place.
- A notification that a session another session opened is waiting or has
  failed says `Opened by <name> in <project>.`; in `Activity` its row says
  `<project> · opened by <name>`, and the row of a session asking to drive
  one of yours says `<project> · asks for <name>`.
- The name in `opened by <name>` and `driven by <name>` in a pane's header
  can be pressed: it shows the opener, or offers `Show <name>` and
  `Stop <name> driving`. The command palette finds `Stop <name> driving
  <session>`.
- Every pane exports `SPLITLANE_CLI`, the absolute path of the `splitlane`
  executable it belongs to, so a script or an agent can reach the instance
  it runs in where `splitlane` is not on `PATH`.
- `splitlane skill install | status | uninstall` writes the fleet skill into
  the skills directory of Claude Code, of Codex and the shared `~/.agents`,
  each only where that directory exists. A copy you edited is reported as
  `modified` and left alone unless you pass `--force`.
- Splitlane keeps that skill installed: every start writes it for the agents
  found on the machine, never over a copy you edited, so an agent asked to
  open sessions in Splitlane knows how without a command run first.
  Settings -> Agents -> Fleet skill turns it off and removes it
  (`"fleet_skill": false` in `splitlane.json`).
- The skill in `skills/splitlane-fleet` teaches an agent to open its own
  sessions, wait on them, read their answers, redirect and close them.

### Changed

- A session that is waiting for a person is not typed into by a caller in a
  pane, with `send` or with `key`, also when `SPLITLANE_IPC_SCRIPTING=1` is
  set. A script outside a pane is not affected.
- The injection fence has its row in Settings -> Agents at all times. It was
  shown only while Free access was on, though it acted regardless.
- `splitlane close` refuses a session that finished a turn you started and
  that you have not looked at yet (`unseen_by_person`), with or without
  `--stop-turn`: closing it would take the result away with the row.

### Removed

- **Free access**: the switch in Settings -> Agents, the notice about it at
  launch, and the `ai_unrestricted` key in `splitlane.json`. It let any
  caller type into any pane and had to be turned on again after every
  restart. A file that still has the key loads as before; the key does
  nothing. If you relied on it for a lead agent, have the agent open its
  sessions with `splitlane add`; for your own scripts, launch Splitlane with
  `SPLITLANE_IPC_SCRIPTING=1`.

### Fixed

- A Claude Code session waiting for an answer sent no notification when its
  hook was not reporting, which is how a session opened by another session
  could stand on a permission prompt unnoticed. The notification now goes out
  as soon as Splitlane reads the wait, with or without the hook, once per
  question.
- A Claude Code session that started a command in the background and said
  it would wait for it was shown as `finished` and announced the moment its
  turn ended, then went on working when the command was done. The end of the
  turn is now held while such a command runs: nothing is announced if the
  agent carries on, and a command still running after two minutes is
  announced once, as `1 shell still running`.

- Deleting a session or closing its project stops its agent. On macOS the
  agent process kept running after its row and pane were gone, and a turn in
  flight ran to its end unseen. `workspace.close` over the socket did not end
  the project's sessions at all: they stayed in `surface.list` with no
  project to open them from.
- A prompt given when opening a session is not written into a pane whose
  agent did not start.
- A turn stopped with Esc before the agent had answered anything is no
  longer marked and announced as finished. Neither is the first turn of a
  session when it is stopped.
- When `splitlane interrupt` stops a turn that early, Claude Code puts the
  prompt back on its input line, and the next text sent was appended to it
  and submitted as one prompt. Splitlane now clears the line in that case.

## [0.1.4] - 2026-10-04

### Changed

- A worktree Splitlane created is now removed when its project closes, as
  the "Remove it when the project closes" choice always said. Until now it
  never was. It is removed only when it holds nothing but committed files:
  an ignored file, an untracked file or an edited `.env` keeps it, because
  git would delete those without asking. The branch is never deleted.
- `workspace.close` and `workspace.select` require `index`. Without one,
  `workspace.close` used to close whichever project was active.
- A parameter that names a target (`surface_id`, `to_surface_id`, `index`)
  must be a JSON number. A string such as `"42"` used to be read as "not
  given", and the write went to the first pane of the active project; it is
  now refused with `-32602`.

### Added

- Splitlane asks the release feed for a newer version every four hours
  while it stays open, not only at startup.
- The macOS `.dmg` is itself signed, notarized and stapled. Before, only the
  app inside it was.
- `surface.send_text` and `surface.send_keystroke` answer `-32003` when the
  pane did not take the input - its process has exited, or its input queue
  is full - instead of `sent: true`.

### Fixed

- The git commands Splitlane runs in the background no longer run programs
  named in a repository's own configuration: a file-system monitor, a text
  converter, an external diff or a content filter. Opening a folder is not
  enough to execute what its `.git/config` says.
- On Linux, starting the app with no display server prints an error and
  exits instead of hanging.
- The error for an unreachable instance no longer repeats the program name,
  and `--help` no longer lists a flag meant for the update test harness.

## [0.1.3] - 2026-09-28

### Fixed

- Codex and Claude Code no longer show as running before their first
  message. Codex 0.158 animates its idle prompt and repaints on focus, and
  that output was read as work; for agents whose state Splitlane reads from
  their own records, terminal output no longer counts as running.

### Added

- Every package now ships `THIRD_PARTY_LICENSES.md`, the license texts of
  all Rust crates compiled into Splitlane. The macOS app bundle also carries
  `LICENSE`.

### Documentation

- The README and `THIRD_PARTY_NOTICES.md` credit Zed Industries for GPUI and
  list the code adapted from Zed's crates, with their licenses.

## [0.1.2] - 2026-09-28

### Security

- `"ai_unrestricted": true` in `splitlane.json` no longer opens `send` and
  `key` by itself. The file is reloaded while Splitlane runs and agents can
  edit it, so an agent could grant itself write access to other panes. The
  key is now a request: Free access opens only when you turn it on in
  Settings -> Agents, once per launch. Setting the key to `false` still
  closes access at once. If you had Free access on, Splitlane tells you after
  the upgrade and one click in Settings turns it back on.

## [0.1.1] - 2026-09-28

### Fixed

- A freshly started Codex, or any agent without hooks, no longer shows as
  running with a Stop button until its first message. It now settles to
  idle once its terminal goes quiet.
- The main window has a title, so Linux taskbars and Alt-Tab show
  "Splitlane" instead of "Unnamed Window".

### Documentation

- The Install section and `keys/README.md` explain how to verify downloads,
  including the minisign public key, and cover the tarball, rpm key import
  for zypper, and the minimum glibc version.

## [0.1.0] - 2026-09-27

First release with builds: a signed and notarized macOS app for Apple
Silicon, and signed `.deb`/`.rpm` packages, an AppImage and a tarball for
Linux on x86_64 and aarch64. Windows is built from source for now. See
[Install](README.md#install).

[Unreleased]: https://github.com/ivkan/splitlane/compare/v0.2.7...HEAD
[0.2.7]: https://github.com/ivkan/splitlane/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/ivkan/splitlane/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/ivkan/splitlane/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/ivkan/splitlane/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/ivkan/splitlane/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/ivkan/splitlane/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/ivkan/splitlane/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/ivkan/splitlane/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/ivkan/splitlane/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/ivkan/splitlane/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/ivkan/splitlane/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ivkan/splitlane/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ivkan/splitlane/releases/tag/v0.1.0
