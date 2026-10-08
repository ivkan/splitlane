# What every agent is doing, and how the rail says so

This page covers the status words, the detector that sets them, who owns
`Thread::status`, when a desktop notification fires, and the `Activity` chip and
popover above the panes. Each block gives a rule, the case that forced it, and
what would re-open it. The interface side of the same reasoning is in
[design-decisions.md](design-decisions.md).

## `starting` is the fifth status word

**`starting` is the fifth status word, and the only one that is not a detector
state.** There are five: `starting` · `running` · `waiting for you` · `idle` ·
`failed`. `starting` is true by construction, not by detection. It is set when
Splitlane issues the launch command and taken down by the **PTY-output counter**,
which is the one signal all sixteen agents give without cooperating. It exists
because otherwise the header shows the wrong word for two seconds: opening a
session showed `idle` while `claude --resume <uuid>` sat on screen waiting for
the CLI to paint.

It is **muted text, not a state colour**, and its dot is a **hollow ring**, not
a fifth fill. `running` is amber and `waiting` is the accent because both are
claims on the user's attention, and `starting` claims nothing. The four settled
states are filled dots, and the one unsettled state is the one left unfilled.

**It comes down ahead of the ranking in `deposit_pty_flow`**, and that is the
one subtle thing about it. `starting` is nobody's claim to keep - not the
detector's, not the hook's. It belongs to the launch, and the PTY-output counter
is the only source guaranteed to run for every agent. Behind the rank check it
would outlive its truth on a surface whose hook spoke once and went quiet. It
also goes whether the counter moved or not: an agent that printed nothing is not
still starting, and `idle` claims less.

**A second withdrawal exists because the first is not enough**
(`withdraw_starting_if_unwatched`, run in the pass's own bookkeeping). Every
other status is somebody's claim, and whoever made it takes it back. This one
has no source of its own, so the pass that finds it unattended has to take it
back. `agent_state_targets` skips a PTY that has not spawned. A restored
container that is **not the active one is never rendered**: its views never
promote, and their launch commands sit in `pending_input`. Without this second
withdrawal nothing could end the word there. The case that forced it was a
restored second project still showing the ring minutes after launch.

**The counter keeps the `running` it raised for as long as output keeps
coming.** Past `starting`, the same counter is the third source for an agent
with no status reader and no hook frame yet, and it takes back only the
`running` it put up itself. Output on the next pass renews that claim rather
than reading as somebody else's. It used to read as somebody else's, and the
claim was then nobody's: Codex paints its screen across two passes at launch,
then waits for the first prompt with no rollout file and no hook frame, and the
header showed `running` with a Stop button until the first turn.

**An agent with a state reader of its own is never the counter's to claim, not
even before that reader has a file.** The rule is `pty_flow_outranked_by`, and
the agents are the ones `TerminalAgent::reports_state` names: Claude Code and
Codex. The case that forced it is Codex 0.158. It plays an animation on its
idle prompt for about fifteen seconds after launch (measured: 15-18 KB of
output every two seconds, then nothing) and repaints again on every focus-in,
which Codex asks the terminal to report. The renewal rule above then held
`running` and a Stop button over an agent nobody had asked anything, for the
whole animation, and brought it back each time the pane was focused. A byte
counter cannot tell that animation from a turn, and nothing else speaks at that
moment: Codex writes no rollout file before its first turn, and its
`SessionStart` hook does not fire at launch either (measured with a trusted
hook on 0.158, 25 seconds idle, no call). So the session id the Codex reader
needs is not bound yet.

Such an agent is idle by definition until its first turn, and the first turn is
exactly what gives it a better source. With the hook installed and trusted, the
`UserPromptSubmit` frame sets `running` the moment the prompt is submitted, the
`SessionStart` frame binds the session id, and the pass after the rollout
appears hands the surface to the detector. Claude Code loses nothing: its status
file exists from launch, so the detector already owned it. `starting` still
comes down for every agent, reader or not, because that part of the counter's
job is about the launch and not about a turn.

The price is Codex with no hook frames: the shim is not on `PATH`, Splitlane is
not reachable from the pane, or the person answered Codex's "Hooks need review"
prompt with "Continue without trusting". Codex has no forced-session-id flag, so
without the hook no session id is ever bound, the reader never gets a file, and
that surface never shows `running`, even during real turns. The other choice
was to keep the counter for Codex and show `running` over an idle prompt, which
is a false answer on the one agent where a better source is usually present.
A missing word is the cheaper error. What would re-open it is a way to find a
Codex rollout without the hook - for example matching the rollout's own
`session_meta` (working directory and start time) to the surface - which would
make the no-hook case read turns properly instead of falling back to bytes.

**A loader over the pane body was considered and refused.** The pane is the
CLI's own terminal, so a curtain would hide the only two seconds that explain a
failure, such as a session id already in use or a CLI not on PATH. There is also
no honest "loaded" predicate to end it on.


## The detector: two sources, in order

The rail owes three answers about each surface: working, stopped, or waiting for
you. They come from signals no vendor has to keep providing. The pass is
`app/agent_state_pass.rs`. Every two seconds it runs over **every** mounted agent
surface in every container, not only the visible ones, and asks two sources in
order.

**"Every" includes a surface that is in no pane.** A session pushed out of its
pane by another keeps its PTY, its agent and its row in the rail; the only
thing it loses is the pane. The target list used to be built from the layout
tree alone, so the pass stopped visiting such a session, and nothing took its
place: `Thread::detector_read_at` is written only by a pass that visits the
surface, so it stayed set, the hook kept standing down for a reader that was
not coming, and the out-of-turn re-read found no target either. Measured on a
live Claude Code session that was thirty seconds into a turn when another
session was opened in its pane: the turn ended, the `Stop` frame arrived, and
the row said `running` for the next seventy-five seconds, with no unread mark,
no notification and no count for a script waiting on it. The run was announced
when the session was shown again, with a duration that included the time it
had been out of sight.

So `agent_state_targets` walks the panes and then the views the surface cache
holds, merged by surface id. A view whose PTY has not spawned is skipped in
both, which is still what keeps a restored session nobody has opened out of
the pass. The alternative was to hand such a surface back to the hook when the
pass stops visiting it. It was refused because the hook cannot say the one
thing a session out of sight most needs said: during a permission ask its last
frame is a tool call, so it says `running` while a person is being waited on.

**The status file is asked first.** Claude Code writes
`~/.claude/sessions/<pid>.json`, and `claude_pid_state::state_for` reads it. The
read is keyed by the surface's forced session uuid, so it cannot answer about
somebody else's session. The file is used because of one measured gap: **an
interactive prompt writes nothing to the transcript while it is on screen**. The
transcript rule below therefore cannot see the most common way an agent waits
for you, and the status file says `waiting` the whole time. Silence here is
never an answer. These cases all fall through to the next source:

- a missing file;
- a file with no `status` (an IDE-entrypoint session; the field is written for
  the terminal entrypoint only);
- a word this build does not know;
- a file naming another session.

**Then the rule**, which needs no cooperation at all. It reads the transcript
the CLI writes for itself, plus whether the agent's process has spawned a
worker. `agent_state.rs::classify` is the rule,
`claude_sessions::probe_state_from_tail` is the reader, and `process_tree.rs` is
the process probe.

**Why both, and in that order.** The status file is a vendor's courtesy and can
disappear in a release. The rule is ours and reads what the CLI writes for its
own sake. So the file answers first where it is right, and the pass keeps the
rule's worker baseline warm underneath (`state == Finished` still takes one). A
CLI release that drops the field must degrade to the old behaviour, not to a
stale one.

**The rule takes three inputs, not two, and the third is easy to forget.**
Beside the transcript and the worker sits `AgentProcess`: whether the agent's
own process is still under the pane. It used to be folded into
`Worker::Unknown`, which only protects the `Spawns` arm (`Bash` demands
`Worker::Absent`). `Instant` (`Read`/`Write`/`Edit`/`NotebookEdit`) accepts
`Unknown`. So a **killed** agent whose transcript froze with a call open was
reported as waiting for a person, and the claim held for as long as
`TRUST_HORIZON` allowed. This was observed on a live session.
`AgentProcess::NotSeen` now silences every arm. A test states the price: a live
agent whose launch chain this build cannot read loses the answer entirely. That
is a missing dot rather than a false one.

**"An open call means a person is waiting" is true of `Write` and false of
`Bash`.** On Claude Code 2.1.246, a `Bash` permission ask writes **nothing** to
disk while it hangs. The `tool_use` record lands 3.5 s *after* the human
answers, with an honest timestamp of when the call was made. So the `Spawns` arm
cannot reach the case it was written for. The only way to see an open `Bash`
with no worker is an agent that died mid-command, which is exactly what the
rail's no-false-claims invariant forbids claiming. This is version-specific:
re-measure it on every CLI bump, with both shapes of the question.

**A background agent is the longest thing a session does, and call pairing
cannot see it.** `Agent` returns `status: "async_launched"` in about 0.05 s (51
launches measured in a local corpus), and the subagent runs for minutes
afterwards. Pairing alone closed the call before the work had started, and the
rail said `idle` beside a pane whose own footer named a running agent. The
transcript records both ends of the real span, so `probe_state_from_tail` reads
them as one more open call:

- `toolUseResult.agentId` opens it;
- the `<task-notification>` carrying the same id closes it.

The close is recognised for all four end states in the corpus (`completed`,
`killed`, `failed`, `stopped`), so it does not depend on the agent succeeding.
Its kind is `Opaque`, which is what a subagent has always been in that
vocabulary. It can move a surface from `idle` to `running`, and it can never, on
its own, claim a person is waiting. It is **not** bounded by `TRUST_HORIZON`,
and that exemption follows the general rule rather than being a special case.
The horizon exists to stop a stale record from becoming a permanent claim about
a *person*, and only the `Instant` and `Spawns` arms can make such a claim.
Ageing out an `Opaque` call buys nothing and costs the one answer it had.
`classify` keeps any `Opaque` call while `AgentProcess::Found` holds, which is a
better bound than a clock: the horizon really guards against a **frozen file**,
and the process check answers that directly. The notification arrives under
three record shapes, and only one of them has a `message`, so it is read off the
line's own text, the one thing all three share.

**No call open is not the same as no turn open.** Reading them as one thing was
the most common cause of a missing dot. Everything above is about a tool *call*,
and an agent is not always in one. Between the prompt and the first call,
between a result and the next call, and for the whole final answer, it is
**generating**, and generation appends nothing. That silence read as an empty
call set and answered `Finished`, so a session visibly working said `idle`.
Measured on real Claude Code transcripts, the gaps ran to 41 seconds in one
session and 18 in another. `TranscriptProbe::open_turn` closes this, and it is a
**reading, not an inference**. Claude Code stamps every `assistant` record with
the API's own `stop_reason`: `tool_use` while the turn continues, `end_turn`
when it is over. The file states the difference.

`open_turn` is consulted only where the call set is empty, so it cannot mask a
waiting answer. It votes `Thinking` and never `WaitingForInput`: a turn in
flight with no call open is either the model generating **or** a permission ask
the file cannot see, and "working" is the cheaper mistake between those two. It
is bounded by `AgentProcess::Found` first and `TRUST_HORIZON` second. Unlike an
`Opaque` call, a turn has no legitimate reason to outlive the horizon, because
anything genuinely long is a call and never reaches that branch. A stamp in the
**future** of our own clock cannot be dated, and the turn is dropped. An open
call clamps such a stamp to age zero and is safe there, because the waiting
predicate needs an age past `GRACE`. A turn has no lower bound, and age zero
would sit inside the horizon for ever.

**The closing set is the allowlist, not the opening one.** This is the same
shape as `execution_of`'s prior. `end_turn` and `stop_sequence` end a turn,
`tool_use` continues it, and every other word - `max_tokens`, `pause_turn`,
whatever the next API release adds - leaves the standing answer alone. Treating
everything except `tool_use` as the end turned either of the first two into
`idle` on a session that was working.

Four record shapes are excluded, and each would have said the wrong thing:

- **An interrupt** arrives as a `user` record. It is matched by **equality
  against one content block**, never as a substring of the line, because a
  person's own prompt can quote that text. Asking an agent why a pasted
  transcript stopped is an ordinary question, and a substring match silenced the
  turn answering it.
- **A local command** (`<command-name>`, and the `<local-command-caveat>` /
  `<local-command-stdout>` records written beside it) starts no model turn.
  `/clear` is the record that *begins* every new transcript, so without this
  exclusion every cleared session showed a spinner for half an hour.
- **A sidechain** record belongs to a subagent, whose `end_turn` would close a
  turn that is still running.
- **A synthetic-model** record is the CLI speaking, not the agent answering.

Of 373 local transcripts, **18 end mid-turn**, and every one of them is a dead
session. That is the exposure both bounds cover.

**Codex states its turn outright, and reading its source is how we know.** It
has three events - `task_started`, `task_complete`, `turn_aborted` - and all
three are persisted in every history mode. That comes from
`codex-rs/rollout/src/policy.rs::should_persist_event_msg` at tag
`rust-v0.149.1`. A local corpus agrees exactly: 19 started, 17 complete, 1
aborted across 18 rollout files, with the one unaccounted turn belonging to a
session closed mid-turn. This is a **better** signal than Claude Code's, not a
copy of it. Claude has no turn event at all, and its end has to be inferred from
`stop_reason`, a field whose range the file never states - which is exactly what
made the first version of that reader close a turn on `max_tokens`. Codex names
all three states, so nothing is inferred and there is no unknown word to guard
against. The turn is dated from `task_started`, so its age is the whole turn's
rather than the last record's. That is the honest reading when nothing in
between says anything about it.

**Codex is readable and Claude Code is not, and that asymmetry is structural.**
`openai/codex` is the CLI's actual Rust source, under Apache-2.0.
`anthropics/claude-code` holds an issue tracker, docs, a devcontainer, plugins
and examples, but no CLI source, and the shipped `claude` is a compiled binary.
So for Codex, a question about the file format has an authoritative answer, and
for Claude Code it never will. That side stays inference from measurement, and
has to be re-measured on every CLI bump. The rule that follows: **when the
source is available, read it rather than the corpus.** A corpus shows what
happened to occur, and the writer shows what can.

**`/clear` mints a new session id and a new transcript, and the surface follows
it.** Splitlane names a session on the command line, and it cannot assume the
session keeps that name. In five of five local transcripts containing a
`/clear`, it is record index 4 of a file that *begins* there, and the old file
is frozen where it stood. Everything pointed at the old id then breaks at once:

- the detector reads a file that will never grow again and says `idle` for
  ever;
- `claude_pid_state` refuses the status file because the ids no longer match;
- "copy the last answer" returns an answer from before the clear.

`claude_pid_state::session_for` reads the session the process says it is
running, and the pass adopts it onto `Thread::session_id`. Three things must
agree before an adoption is even proposed:

1. the pid was resolved under this pane's own PTY child;
2. the status file says that process is running in *this* surface's directory;
3. the named session has a transcript in that directory's project.

This guard is deliberately narrower than `state_for`'s exact session-id
comparison, not a weakening of it. `state_for` checks against an id Splitlane
chose, and adoption exists precisely because that id has gone stale.

**Then two consecutive passes have to agree.** This is the guard that makes
reading a pid-numbered file safe at all. `<pid>.json` is named by pid alone and
nothing ever removes it, so a hard-killed agent leaves one behind. If that pid
is recycled onto this pane's agent, the stale file names a dead session in the
same directory, and every guard above passes - the transcript check least of
all, since a dead session's transcript is on disk too. What closes the hole is
that the window is **transient**. The live agent writes its own file about two
seconds into its life, so a stale reading cannot survive the next pass, while a
real `/clear` reports the same new id for as long as the session lasts. A clock
comparison was the other candidate. It is refused for the reason
`process_tree::ProcId` already states: the start token is deliberately opaque,
because comparing a process's start against our own timestamp needs a shared
clock frame the three platforms do not give, and agreement needs no frame at
all. The cost is one more tick on a surface that has been quietly wrong since it
was cleared. Adoption also writes `session.json`, because a session id is what a
**restart** resumes, and an unsaved one would put the surface back on the
orphaned session.

**The detector traces itself under `RUST_LOG=splitlane::agent_state=debug`.** It
uses its own target rather than a module path, so one variable turns on this and
nothing else. It exists because this is the one part of the app whose defects
are intermittent and invisible: a missing dot looks exactly like a session that
really is idle, and by the time anyone thinks to look, the moment is gone. Every
two seconds, for every surface, it logs:

- what the status file said;
- what the transcript held (calls with ages, the open turn, `errored`,
  `incomplete`);
- the resolved agent pid;
- the worker;
- which source decided;
- what the PTY-output counter moved by, the status before and after it, and
  whether it claimed `running` or stood down, and for whom.

The one-shot probe next door (`claude_sessions::the_detector_on_a_real_session`,
run with `--ignored`) answers about one surface at one instant. That is the
wrong shape for a fault nobody can reproduce on demand.

Three things are easy to get backwards:

- **A hanging call reads by how its tool executes, not by its name.**
  `Read`/`Write`/`Edit` are in-process and fast, so one hanging past five
  seconds is a question. `Bash` spawns, so its worker decides and its clock says
  nothing. Everything else - `mcp__*`, a subagent, an unknown name from the next
  CLI release - is `Opaque`, and **nothing is claimed about it**. That is why
  the prior is an allowlist.
- **`Worker::Unknown` is not an error.** It means "not asked", and the rule
  reads it as working. Every path that reports waiting is backed by something
  positive.
- **The pid to ask about is not `Thread::agent_pid`.** That is the *shim's* pid,
  and the shim always has one child, so it would answer `Present` for ever.
  `process_tree::agent_under_pty` starts at the pane's PTY child (the user's
  **shell**; the launch command is typed into it) and steps over the shim. It
  recognises the shim by **executable path**: the shim is staged under each
  agent's own binary name, so there is no `splitlane-shim` process and the name
  separates nothing.

**Who owns `Thread::status`: the detector, with the hook as an accelerator.** A
hook frame for a surface the detector reads does not write the status. It asks
for an immediate re-read instead (`accelerate_agent_state`, one in flight per
surface). If both wrote it, the dot would blink: during a permission wait the
last hook frame is a `tool_use`, so the hook says `Thinking` while the
transcript says `WaitingForInput`. The split is per surface
(`Thread::detector_read_at`, set and cleared every pass) rather than wholesale,
because the detector does not read every agent. Which agents it reads is
`TerminalAgent::reports_state`, which `claude_sessions::transcript_path` also
consults so the two cannot drift. It names **two**: Claude Code, and Codex
through `codex_state::probe_state_from_tail` over its `rollout-*.jsonl`. The
remaining fourteen are still driven by the hook.

Codex reads state and still sits on the `resume by name` capability tier. That
is the tier working as intended, not a gap: the top rung asks for a session
Splitlane can *pin* as well as a file it can read, and Codex has no
forced-session-id flag.

Two rules in `agent_state_pass::deposit` exist because a claim about a person is
not a fact you write once:

- **A pass that cannot read a file withdraws what it last said.** It goes to
  `Idle`, the state that draws nothing, rather than leaving a `WaitingForInput`
  standing with nothing behind it and no hook frame coming (the agent that would
  have sent one is what went away). It withdraws only what the detector itself
  put there.
- **An older reading never lands on a newer one.** The field records the moment
  the reading was *taken*, because the periodic pass and the hook's re-read run
  concurrently and can finish out of order.

**A notification asks whether you can see the session, not whether you can see
the app.** Gating on `window_active()` encodes "if you are in Splitlane you will
see it", which is true of one pane and false of exactly what this app is for.
Four sessions across three projects are invisible while the window is right in
front of you: a folded project draws no surface rows, a container that is not
the active one draws no panes at all, and an off-screen run finishing showed
nothing but a dot going out. `should_fire_desktop_notification` therefore asks
`SplitlaneApp::surface_is_seen` (by terminal entity id) or `thread_is_seen` (by
surface record id). They are two doors onto one answer: the window is active
**and** the surface is the active tab of a pane of the **active** container. An
unresolved surface counts as unseen, deliberately. That is the case the app
knows least about, and a completion nobody hears about is worse than a toast for
something that happened to be visible.

**A completion carries a duration floor; a claim on your attention does not.**
`MIN_TURN_FOR_NOTIFICATION` is ten seconds - the number from `undistract-me`,
the tool that established the pattern - and it bounds `turn_finished` only.
"Needs input", "crashed" and "stalled" are claims a short run does not make less
true, and an agent that dies in two seconds is the one most worth hearing about.
It is a **constant, not a setting**. A second knob would contradict the "One
switch" promise in Settings -> Notifications, for a number nobody has an opinion
about until the wrong one has annoyed them. A run of unknown length is announced
(`turn_was_long_enough(None) == true`). The unknown cases are a frame with no
start stamp and a session restored mid-run, and silence there is the worse
error. The run is clocked from entering `Thinking` from outside a run, so the
`WaitingForInput` bounce a permission ask makes does not restart it. That is
`ai_types::next_turn_started` for the hook path and `agent_state_pass::run_ended`
for the detector's.

**The body names the surface, and the name and the message are joined, not
chosen between.** The summary line is the agent (`Claude Code needs input`), so
with two sessions of one agent it is identical by construction, and the body is
the only place the answer to *which one* can live. Choosing the message over the
name dropped the name in exactly the case where the notification was worth
reading - an agent with something specific to say - so two Claude Code sessions
in one project sent two notifications naming neither. At that point a person
checks every pane by hand. The name leads, and that order is also the
**truncation rule**: every notification service cuts the tail, so an over-long
message loses itself rather than the identity. That is why the two parts keep
their own caps instead of sharing one. `agent_exit_notification_body` and
`stalled_notification_body` have the same shape.

This was reported from live use in the same terms: a notification that says
something finished but not *which* thing, so six identical notifications are six
reasons to go and look. The attention popover already answered *which one*; the
desktop notification was the surface answering it worse.

**`SplitlaneApp::surface_name` is the sibling of `surface_is_seen`, and it asks a
wider question on purpose.** The two `ai.*` arms know different amounts about
the same thing. A frame whose PTY belongs to a `Thread` arrives with the
surface's own title. A frame belonging to a `Workspace` - an agent somebody
started by hand in a shell pane, which no launcher made a record for - arrives
with the *project's* title. The surface id is already in hand at those sites,
because it was resolved for the seen-check. `surface_name` resolves it to the
string the rail row carries: the thread's title where there is a record, the
tab's label where there is not. That is the order `attention_queue` uses, so the
popover and the notification cannot call one surface two things.
`surface_name` searches **every** container, while `surface_is_seen` searches
only the active one. That difference is the point: being outside the active
container is what makes a surface unseen, and naming is the opposite question -
a notification fires precisely for a surface nobody is looking at. `None` keeps
the project as the fallback, because a name the app cannot verify is worse than
the wider one it can.

**On macOS, the `notify-rust` path delivered nothing, and it was a backend fault
rather than a gating one.** `notify-rust` reaches `mac-notification-sys`, which
is built on `NSUserNotificationCenter` - deprecated in 10.14 and silently ignored
since. `show_desktop_notification` returned `Ok(())` while the process never
connected to `com.apple.usernoted` at all; the notification daemon's log never
named the app. Bundling did not help (tested, with a correctly pinned
identifier), and the crates were already at their latest published versions.
The gate, the floor and the body above were all correct and ran on Linux and
Windows the whole time.

One diagnostic trap from that investigation: **absence from `com.apple.ncprefs`
means nothing.** An app the system was demonstrably delivering to was absent
from it too. What does answer is the application list in System Settings ->
Notifications.

**macOS has its own backend, and it does not trust its own send.**
`agents/mac_notifications.rs` goes through `UNUserNotificationCenter` (the
`mac-usernotifications` crate, by the author of `notify-rust`). The `notify-rust`
path is target-gated to the other two platforms, so its dead macOS arm cannot
come back by accident. Two measured properties of that API reach into the
interface:

- It needs an `.app` bundle. An ad-hoc signature is enough; Developer ID is not
  required. So **a `cargo run` build will never show a notification**, which is
  the platform's rule and not a bug to hunt.
- `send` answers `ok` even under a denial, so the same "`Ok` means delivered"
  trap reappears one level up. The backend therefore asks
  `getNotificationSettings` before every send and refuses rather than reporting a
  delivery it cannot vouch for. Settings -> Notifications **states the operating
  system's answer** beside the rungs, with the class rows reading `blocked`
  rather than `covered` wherever macOS will stop them.

**macOS is asked for permission when the first agent session is created**,
launched or restored, in `build_agent_terminal_view`, the one function both
paths go through. The alternatives were weighed. At app launch, the prompt lands
before the person has done the thing notifications are about. At the first
notification, it lands when by construction nobody is looking, which collects a
refusal by absence. `NotDetermined` is the only "ask once" bookkeeping there is:
macOS keeps it, so this app does not.

**The detector notifies for the surfaces it reads**, and the `ai.stop` handler
stands down for exactly those surfaces (`Thread::detector_read_at` is set). This
is the same split, for the same reason, as `Thread::status` itself. When the
detector owned the dot and the hook owned the toast, a surface whose shim had
gone quiet showed a correct status and announced nothing. `running_since` lives
on `SplitlaneApp` rather than on `Thread` because many places write
`Thread::status`, and a stamp maintained at each is a stamp one of them will
forget. `run_ended` is the single place that both fills and empties it, so an
entry cannot outlive the run it measures.

**Whether a run's end is news is asked once, with three conditions**
(`orchestration::run_is_news`, called from `announce_finished_run`). The run
is still counted for a waiting script whichever way the answer goes: the mark
and the notification are for a person, the count is for whoever is waiting.

- **The pass announces only for a surface it speaks for.** `run_ended` looks
  at `Thread::status` and its own clock, and neither says who wrote the
  status. So the pass announced runs it had not read. Measured on a live
  instance: an agent the hook speaks for was announced by `ai.stop` and again
  by the pass a second later, a second notification for one run; a surface
  only the byte counter speaks for was announced as finished when its output
  stopped, including once on its own launch, with no turn anywhere. The count
  of ended runs already asked `detector_read_at`; the announcement now asks it
  too. A hooked agent is announced by its hook, and a surface nothing reads
  is announced by nobody.
- **A turn a person stopped is not announced.** Esc ends the run, and the pass
  reported it as a finished one: a mark on the row and, past ten seconds, a
  notification, about a turn the person had just ended by hand. The agent's
  own file says which it was. Claude Code writes the interrupt as a `user`
  record, Codex as `turn_aborted`, and the reader hands it over beside the
  turn-end marker (`TurnEnd::interrupted`). It is read from the file rather
  than inferred from a marker that did not move, because the closing record of
  a finished turn can land a moment after the status settles, and inferring
  from its absence would silence a real finish. The interrupt is written the
  instant it happens, three seconds before the held end is confirmed, so it is
  there to read by then. For Claude Code the marker itself stays on the last
  turn that finished; only the flag is about the newest one. A session whose
  very first turn is stopped has no finished turn to carry the flag, so the
  interrupt record is the end it reports: that is the ordinary case for a
  session opened with a task and then redirected, and it used to be announced
  as finished. The flag is dropped as soon as another turn opens, because it
  is about the newest turn and the next one can end with no record at all -
  Esc before the first token writes nothing and puts the prompt back on the
  input line. That turn is recognised by what the file does show: a prompt
  nothing followed, read at a moment the status says the turn is over. It is
  given an end of its own, marked stopped, so it is not announced as an
  answer either. The same reading gives the run its `last_outcome`,
  `interrupted`, which is what a caller that asked for the stop waits for.
- **News of a finished run belongs to whoever sent the last message.** A
  session opened by another session ends a run its opener started, and the
  opener reads the result. Telling the person as well would put eight marks
  and eight notifications on the work of one agent they are already watching.
  If the person typed the last message, or the opener has been closed, it is
  the person's news as usual. The same rule is asked in the `ai.stop` handler
  for the agents the hook speaks for. `waiting for you` and `failed` are not
  completions and are not subject to it: they go to the person in full.

  "The last message" needs a definition of a person's message, and any key in
  the pane is too wide: an arrow pressed by accident would turn the opener's
  run into the person's news. It is Enter on the keyboard in that terminal,
  counted by the terminal (`TerminalView::keyboard_submits`); the session
  remembers the count as it stood when its opener last wrote text
  (`Thread::opener_wrote_at`), and the opener's message is the last one for as
  long as the two are equal. The count is taken before the terminal's modes
  decide what the key does, so it can be one too high, which errs towards
  telling the person.

**A run ends on a span, not on a sample** (`agent_state_pass::confirm_run_end`).
A working Claude Code session says `idle` for about a second in the middle of
its work every time a background task ends. This was measured on CLI 2.1.270 by
logging a live session's own `sessions/<pid>.json`. While a background agent
runs, the file stays on `busy`, even after the main turn has ended. When the
background agent finishes, the file reads `idle` until the main agent, woken by
the task's notification, writes `busy` again - 1.23 s, 1.24 s and 1.36 s over
three runs. The session never stopped and its TUI shows it working throughout,
but a two-second pass lands in that window about every other time. It read a
finished run, marked the row `finished` and, past the ten-second floor, sent a
toast about a session that was still going. It was reported from live use as a
session out of focus that starts a background agent or calls a skill, lands in
`finished`, sends a notification, and is still working when you switch to it.

So the first reading that would end a run is held. The surface stays `running`,
and the run ends only if a later reading still says so `RUN_END_CONFIRM` (3 s)
after the first. Any other reading clears the hold. It is a span rather than
"two passes in a row" because a hook frame triggers an out-of-turn re-read, and
two readings a few hundred milliseconds apart can both fall inside one window.
The price has two parts:

- a finish is reported late: four seconds after the first idle reading, up to
  six after the agent stopped;
- a turn followed by another within the span counts as one run, so the first is
  never announced. From the status file, that sequence cannot be told apart from
  the blip. The cases that produce it are a Stop hook continuing the agent and a
  message queued in that session, and neither is a finish somebody is waiting to
  hear about.

Only `Thinking` to `Idle` is held. `waiting for you` goes up at once, because
holding a claim that somebody is waiting past its truth is the one false answer
this detector may not give.

**`shell` is idle with something alive beside it, and it is not announced as
finished at once.** The CLI writes `shell` when the turn is over but a
background command it started is still running. It used to resolve to `idle`
and be forgotten, on the argument that a dev server would otherwise hold a
session on `running` for its whole life. That argument still stands, and the
word still resolves to `idle` (`claude_pid_state::state_from_status`). What it
missed is the other thing the same word means: an agent that started its tests
in the background, said it would wait for them, and carries on when they
finish. For that session the row read `finished`, a notification went out, and
a minute later it was working again, sometimes several times in one piece of
work. It was reported from live use, with the pane's own footer saying
`1 shell still running` beside a row that said the session was done.

Measured on CLI 2.1.294 by logging a session's own `sessions/<pid>.json` every
50 ms across turns that left background commands running:

- **The file says `shell` and nothing more.** With two commands alive and with
  one, it is the same file apart from its two timestamps: no count, no list,
  no task id.
- **The transcript has both ends.** The result of the `Bash` call that started
  the command carries `toolUseResult.backgroundTaskId`, and when the command
  stops the CLI enqueues a `<task-notification>` naming that id: `completed`
  for one that ran out, `failed` for one killed from outside. That is the shape
  a background agent already has, one field over, and the same reader counts
  it (`TranscriptProbe::background_shells`).
- **There is no `idle` in between.** The file goes from `shell` straight to
  `busy`, about 10 ms after the notification is written, when a command ends
  with others alive and when the last one ends. Every command ending wakes the
  agent.
- **Nothing tells a test run from a dev server.** Both are a `Bash` call with
  `run_in_background`, and the records are the same. This one is read off the
  shape of the launch record; a command that never ends was not left running
  to watch.

So there are three rules, and each follows from one of those.

**The status file decides whether, the transcript says how many**
(`rail_state::background_shell_count`). A count from the transcript alone is
not reported: it keeps counting a command whose end it never recorded. And
with the file's word the count is at least one, because the transcript reader
sees a window of the file and a dev server started before it is not in it.
The count lives on the session's record, is rewritten by every pass from that
pass's own reading, and is zero for any reading that cannot say. A command
that ended and an agent process that is gone both take it down on the next
pass, the second because the status file of a process that is not running is
never opened. It is a property of an idle session: under any other word it is
reported as zero.

**It is not `running`, and it is not a sixth status word.** The row and the
header say `idle · 1 shell`, in the idle tone with the idle dot, where the
header had `idle` and the rail row had the agent's name. `running` would bring
back the dev server that holds a session amber for ever, which is what another
product shipped when it read this word as work. `waiting` is taken: it means a
person is being waited on. The word the CLI itself uses is `shell`, so the row
uses it. A folded project's tally does not count such a session: it is idle,
and the tally has no word for idle.

**The end of the turn is held, not announced and not dropped**
(`agent_state_pass::ShellWait`). When a run ends beside a command started
during that run, no mark is set and no notification is sent. Then one of three
things happens:

- the agent wakes: nothing was announced, and the run clock goes back to where
  the held turn began. It is one run. Without that, the answer after a
  ten-minute test run would be a four-second run under the notification floor
  and would never be announced at all;
- the session is read idle with no command left for `RUN_END_CONFIRM`: the end
  is announced then. This is the agent that did not wake, which the
  measurement says should not happen and a killed process can still produce;
- the command is still alive after two minutes (`SHELL_HOLD`): the end is
  announced once, and the notification says `1 shell still running`. This is
  the dev server, and it is why the hold has an end. Somebody who asked for a
  server and walked away has to hear that the agent finished.

Two minutes is chosen, not measured, and it is the price of having no sign to
tell the two cases apart: a test run longer than that is announced early, with
the text saying a command is still running, and announced again when the agent
finishes. A command that was already alive the last time the session sat idle
is not waited on at all, so one dev server does not delay every finish after
it.

The count of ended runs is not held. A turn that ended is counted when it
ends, for the reason in the next section.

Three things are not covered. Codex 0.158 can leave a command running past its
turn, and whether its rollout records the two ends has not been measured, so
for Codex and for every other agent nothing here applies and a turn's end is
announced as before. `shell` has not been observed on a CLI newer than
2.1.294, or on Windows or Linux. And an agent that is alive but has stopped
writing its file keeps whatever word it wrote last, which is the exposure
every word in that file has.

What would re-open the hold: a field in the status file or the launch record
that says the agent is waiting on a command. With that, the waiting case reads
as `running`, the dev server as plain `idle`, and the two-minute guess goes.


## What a script is told

**`surface.status` carries the rail's own word, beside the hook's.** It used to
answer from the hook's session table alone, and that is the weaker of the two
sources by the argument above: during a permission ask the hook says `thinking`
and the detector says `waiting`. A script that waited on the hook's word made
the mistake the rail was built to stop making. So the response has a `rail`
object - the five status words, who decided (`detector`, `hook`, `pty_flow`,
`none`), and a tier derived from that. The older fields are unchanged and still
say what the hook said, because scripts are written against them.

**The tier is the source, not the agent.** `T1` is the agent's own file, `T2`
is a hook, `T3` is the byte counter or nothing. It is read off
`Thread::detector_read_at` and `Thread::hook_has_spoken` at the moment of the
call, so a surface that loses its better source reports the lower tier the next
time it is asked. A fresh Codex session is `T3` until its first prompt: nothing
speaks for it before then, and saying otherwise would be a claim about a
reader that has no file yet.

**`runs_ended` is counted where the run clock is emptied**, in the state pass
for the surfaces the detector reads and in the `ai.stop` handler for the rest.
That is the same decision that leads to the row's unread mark, and it is one
decision on purpose: a script waiting on the count and a person reading the
rail must not be able to disagree about whether a run ended. The count is
wider than the mark in two ways, and both follow from what each is for. The
mark is news for a reader, so a run that ended on screen leaves none; the run
still ended, and it is counted. An interrupted turn is not news either, and it
is counted for the same reason: a script waiting for that turn would otherwise
wait for its timeout.

**A turn that ended beside a background command is counted, and the script
is told about the command.** The person's notification is held in that case
and the count is not, on purpose. A count that waited for a dev server would
never move, and `wait --until turn-end` would run to its timeout for every
caller that had started one. So the default wait still returns at the end of
the turn, and `rail.background_shells` says how many commands the session
left running. A caller that knows it handed over work with tests in it asks
for `--settled`, which returns only when a run has ended and the session is
idle with that number at zero. The cost is on the caller that asks: with a
command that never ends, `--settled` ends on its timeout. The `surface.rail`
event is sent when the number changes, because a caller waiting to settle is
watching for exactly that and the status word does not move.

**A surface only the byte counter speaks for has no count.** Its `running` is
taken down by output stopping, and output stopping says nothing about a turn:
an agent thinking, an agent waiting for a person and an agent that has finished
are all silent. Counting that as an ended run would hand a script the negative
signal the rail itself refuses to read.

**A turn too short to be seen running is counted from the agent's own record.**
`run_ended` reports only a run the pass watched begin, which is right for a
mark and a notification, and leaves a turn that starts and ends between two
samples with no trace at all. For a script that is a wait that never returns.
The file still records the end: Claude Code's closing `assistant` record has a
`uuid`, and Codex's `task_complete` and `turn_aborted` carry a `turn_id`. The
probe reports the newest one, and when it moves while the status stays `idle`
for `RUN_END_CONFIRM`, the run is counted. The same span as a watched finish,
for the same reason: a working session reads `idle` for about a second when a
background task ends, and its previous turn may have just written a closing
record. Nothing else follows from this count - no mark and no notification.
Those stay with runs somebody could have been waiting on.

Two details keep the two counters from counting one run twice. A marker
already on disk when the surface is first seen is adopted, not counted: it is a
turn that ended before anybody was watching. And a marker that moves within the
span after a watched run ended belongs to that run, because the status can
settle a moment before the record lands.

**Claude Code's marker costs a second read, taken only when it can matter.**
The status file answers first, and when it does the transcript is not read. So
the marker is fetched separately, only while the status says the turn is over,
and only when the file's length has changed since the last read. An idle
session's transcript does not grow, so an idle surface costs one `stat` per
pass.

**The `surface.rail` event is published from one function, and never from
rendering.** `publish_rail_changes` compares each surface's status word and
count against what it last published, and it runs after the state pass and
after every `ai.*` frame. It compares rather than being told, because the
status is written from many places and a publisher that trusts each of them to
report its change is one that some future writer forgets. It is kept out of the
frame loop for the reason the attention edge is kept out of it above: a
minimised or covered window draws nothing while the pass and the hooks keep
running, so an event tied to a frame would go quiet exactly when a script is
the only one watching.

**A terminal with no agent record still has a rail when a hook has spoken for
it.** An agent typed into a shell pane, or started by `workspace.up`, reports
through hook frames routed to the project rather than to a session record. Its
status is the hook session's, its count is kept per surface (the session entry
is dropped a few seconds after a turn ends), and its tier is `T2`. A shell no
hook has reported from has `rail: null`.

**`wait --until turn-end` refuses a surface on the lowest tier instead of
waiting on it.** Nothing that speaks for such a surface can say a turn ended,
so the only thing a wait could return on is output stopping - the same reading
the rail refuses for itself. It gives the surface ten seconds first, because a
session's first hook frame arrives with its first prompt and a wait issued
right after a prompt can be looking at a surface that has not been spoken for
yet. A surface that had a better source earlier in the same wait and lost it
is refused at once. And waiting for a person is returned to the caller with
its own exit code and never waited through by default: it is the middle of a
run, and the one thing a script must not do with it is carry on as if the
agent had answered. A turn that was stopped is returned the same way and for
the same reason, as `interrupted` with exit 10: it ended, the agent is idle,
and there is no answer. It was `finished` and exit 0 at first, with the word
only under `rail.last_outcome`; a caller waiting on a session whose turn a
person stopped by hand then went on to read an answer that was not there, or
the one from the turn before.

**The last answer is read by the CLI, from the agent's own file, never from
scrollback.** `surface.status` says where the conversation is (`rail.agent`,
`rail.session_id`, `rail.cwd`) and `splitlane answer` opens it with the same
reader "Copy the last answer" uses. The server does not read it: a transcript
can be tens of megabytes, the server answers on the thread that draws the
window, and its dispatcher gives a request five seconds. Scrollback is not a
fallback, because a full-screen agent keeps none - an answer guessed from the
current screen is a statement about another session with nothing behind it.
An agent this build has no reader for gets a refusal that says so.

**`answer` reads the answer to the newest prompt, not the last answer in the
file**, and the difference was found on a live session. "Copy the last answer"
wants the last answer the session ever gave. A script that sent a prompt and
waited for the turn wants the answer to that prompt, and the two differ exactly
when the turn produced none: a person pressed Esc while the agent was asking a
question, the count of ended runs moved, and the read handed back the previous
turn's answer as this one's. So the script's read
(`claude_sessions::read_newest_turn_answer`) drops what it has collected at
every prompt a person wrote and at every interrupt, and stops widening its
window once it has seen the newest prompt. A tool result, a local command, a
background task's notification and a subagent's record are `user` records too,
and none of them is a person asking something new.

Codex gets a reader of its own (`codex_state::read_last_answer`). A turn's
answer is `task_complete.last_agent_message`; when that is `null`, the last
assistant message of the **same** turn stands in, and a message from an
earlier turn never does. A turn that ended in an error reports the error, and
an aborted turn reports that it was aborted: in both cases the answer before
it belongs to another turn and offering it would be answering a different
question. While the rail says a turn is running or waiting, no answer is
offered at all, for the same reason.


## `Thread::finished_unseen`, the Activity chip, and the popover

**`Thread::finished_unseen` is the read/unread axis, and it is not a sixth
status word.** The five status words all answer *what is this session doing*.
Whether **you** have looked since a run ended is a different property: it is
orthogonal to all five, clears itself by being read, and so can be added without
touching the vocabulary or spending the accent. The dot goes to `idle`
immediately and honestly. The row additionally reads as unseen: the **name keeps
full brightness** (`ui.text`, the way an unread row does), and the meta slot
carries the news in plain mono (`surface_row_news`), replacing what the session
*is* until it has been read.

**The slot names how the run ended, not that it ended.** `finished` was never a
claim that a run reached its end; it names an outcome the reader has not seen
yet. When that outcome is a failure, its name is `failed`, drawn in
`agent_error` rather than the news tone, and the chip and the row then agree by
construction rather than by coincidence. Any terminal word the scale grows later
lands in this slot with no further decision. Showing **no word** for a failure
was the other candidate. It was rejected because the red dot would then carry
the whole message alone, and every coloured signal in this interface has a word
beside it.

**The word clears on being read, while the dot does not.** Unread-ness is about
the reader and ends when they look; failure is about the session and survives
being looked at. The mark is set only where a run ended **out of sight** - a run
that finished under the reader's eyes is not news - and by both sources: the
detector for the two agents it reads, and the `ai.stop` handler for the other
fourteen. **The mark is not bounded by the duration floor, and the notification
is.** A word on a row interrupts nobody, so a four-second run that ended out of
sight is still news the rail can hold.

**The mark has two levels, and different things clear them**
(`FinishedMark::acknowledged`). A fresh mark is *not acknowledged*: the reader
has not seen even the list, and that is what puts the chip on its accent rung.
Opening the attention popover **lowers** every mark it lists to the quiet level
(`acknowledge_unseen_marks`): known, not collected. Two things take a mark away:

- the surface actually coming up in a pane (`clear_unseen_marks_on_screen`, run
  once a frame from `sync_surface_facts` over the panes of the active container);
- a click on the mark's own popover row, which clears **that** mark and
  deliberately not its neighbours (`clear_unseen_mark`).

Clearing on the rail row's click would cover one of the ways a surface comes on
screen and miss the palette, `⌥⇥` and a drag onto a pane. There are several
doors and one rule, and the rule belongs where the seeing happens.

**The sticky thing is the quiet mark, not the shout.** An earlier design cleared
every mark when the popover opened. Keeping the quiet mark avoids two failures at
once. The first is a ritual walk through the panes to buy silence, which would
devalue the word "read". The second is desensitisation: a chip that is always
hot is not a chip. Because opening no longer clears anything, the popover's
finished section is a live query over the marks rather than a snapshot taken on
open.

**The news tone is `text_tertiary`, and the reason is a measurement.** Against
the rail, `dim` is 4.01 : 1 and `faint` (what the news replaces) is 3.17 - a
**1.27 : 1** step, which the eye cannot compare. With `dim`, the word's
"one step brighter" had no step in it, and the whole mark rested on the name's
brightness and on the word simply being different. `text_tertiary` puts the step
at 2.19 : 1, the same size as the name's own step (`text` over `text_tertiary`,
2.15): two carriers of one magnitude, neither borrowing the accent. The
objection is real and does not decide it: `text_tertiary` is an ordinary row's
*name* colour, but the word sits in a different column, in mono at `HINT`, and
clears itself, so the column reads loud only for as long as there is news in it.
If one of the two carriers ever has to go, **keep the word**: a different word is
legible at any brightness, which is what the measurement showed.

**The same mistake happened in the popover.** The finished row's name shipped as
`ui.dim`, which is **3.50 : 1** on `overlay` - under the 4.5 : 1 floor for the one
thing in that row you click - and **1.27 : 1** from the `faint` timestamp beside
it. That is the incomparable pair again, so the row read as a single grey wash
rather than a name followed by a time. It was reported from live use as "rather
washed out" and measured afterwards. `text_tertiary` is 6.06, a comparable 2.20
from the timestamp and a full 2.15 below the waiting section's own `text`, so the
rank between the two sections survives and the row is legible.

**Port colours by token name, not by nearest colour.** The design prototypes'
grey ladder and the app's diverge in the middle, and the names do not line up:
prototype `dim #9aa0a8` is app `text_tertiary`, prototype `muted #71767f` is app
`dim`, and prototype `faint #61666e` is app `faint`. Reading a token name off a
prototype and writing the same name here is therefore off by one step, silently,
and this codebase has shipped that off-by-one in both directions. **`muted
#8b9098` is a service step: never port a value into it.** The app's `muted` has
no counterpart in the prototype ladder at all, so a value that lands there during
a translation landed there by accident - which is how the chip once shipped one
step brighter than designed. A prototype value with no named counterpart here is
a question, not a rounding. The step stays for cases this app needs, but no
translation may choose it.

**A folded project draws no session rows**, so its group row carries a tally in
plain text - `1 failed · 1 running · 2 finished` - before the count badge, and
only while collapsed (`folded_project_tally_words`). A window-wide counter can
never provide this: the useful fact is not "three things happened", it is
*which project*. A collapsed group answers for rows it is not drawing, and it
states what is true *now*, disappearing when it stops being true, which is what
separates it from a mark that fades on a timer.

**The row's three words are two kinds.** `finished` is a **news** tally: it
counts sessions the reader has not looked at and disappears when they do.
`running` and `failed` are **state** tallies, so a folded row keeps saying
`1 failed` after it stops saying `1 finished`. That is the row behaving like the
rail rows it stands for: the word clears, the state stays. The order and tones
are deliberate:

- **`failed` leads**, in `agent_error`. The row reads left to right in triage
  order, and somebody scanning a column of folded projects for trouble should
  find it in a fixed position rather than after a variable-width `running`
  tally.
- **`running` is `faint`, one step under `finished`'s `text_tertiary`.** Work in
  progress is a state and a finished run is unread news, and news outranks a
  state.
- **`Starting` is not counted.** It claims nothing by design, and a tally is a
  claim.
- **The tallies may not overlap.** A failed unread session's news word *is*
  `failed`, so `finished` counts only unread sessions that did not fail. One
  session may never occupy two of the three words, or the row would overstate
  the project. `FoldedTally` holds this in general: each session lands on its
  highest word - `failed`, then `running`, then unread `finished` - so an
  unread session that is running again is `running` only. A folded project
  group counts with the same function and adds `waiting` above `running`.

**The popover has a quieter lower section under a rule**: `N finished while you
were away` in `dim` mono, rows in `text_tertiary`, each with `finished 4m ago`
as **text** in `faint`. That text is the honest version of a cooling dot, which
was refused: a mark that fades on a timer is an event again, lost on anyone who
looked away for longer than the fuse, and 40 seconds of cooling cannot be told
apart from four minutes. Unseen-until-read has no fuse.

**The chip is on screen whenever the popover has something to say**, and that
sentence is the whole rule. When the chip appeared only for `waiting > 0`, news
on the rows had **no entrance from the top**: three runs finishing out of sight
left the title bar empty, and the only trace was a word on rail rows that scroll
and that a folded project does not draw at all. It was reported from live use as
"with several projects it is impossible to keep track". `waiting::chip_state` is
the one decision, **and `waiting::chip_body` is the one drawing**, so the title
bar and the system-frame toolbar cannot disagree about when the chip exists,
what it says, or how loudly. Separate drawings held only while the one shared
thing was *whether* the chip existed; with rungs, two dot shapes and a movement,
three axes duplicated across two files are three ways to drift. Each frame keeps
its own geometry (height and dot size), its own element id and its own tooltip,
which is what genuinely differs.

**The chip is a status and the row beside it is controls, so there are levels,
not two categories.** Drawing the chip level with `Add pane` and `Files` - same
tone, same fill, same border - fixed its brightness and erased the line between a
*state of the system* and a *button*. The failure mode this app actually has is
not a missed question (an idle agent reasserts itself by standing still) but a
**pile of unread results**, which grows invisibly while it is drawn like
navigation.

**Same pill, same dot, same position - one step down.** The dot appears on every
chip state: it says these are the same kind of thing, and hue carries the rank,
so the reader gets a scale rather than categories. The pill shape, against the
buttons' rounded rectangle, is the only difference in silhouette, and it is the
row's one shape rule: status versus command. Every state opens the same
destination, so there is one thing to learn to click. Hover uses the **shared**
`border_hover`, deliberately not a softer one: a softer hover would be one more
carrier spending the brightness axis, and hover answers *is this clickable?*,
which the chip is - it opens `Activity`.

**The scale is built on read / unread, not waiting / finished.** A scale of
`error > waiting > finished > running > idle`, with an uncollected result at the
bottom, drew this app's main failure mode - a pile of results nobody has
collected - quieter than everything else. The argument that moved it, and the one
the decision stands or falls on:

> A waiting agent is standing because it needs an answer; a finished one is
> standing because nobody has accepted its result. "Just done" would assume an
> agent's output can be trusted unread; an uncollected finish is unchecked work,
> not a closed task. One thing holds both, and one action releases both.

What the chip says, in `waiting::chip_state`:

```
failed          error text  · error dot (filled)  · error fill  · error border
unacknowledged  accent text · accent dot          · accent fill · accent border
quiet           dim text    · dim dot             · tinted fill · neutral border
buttons         text tone   · no dot              · surf fill   · button border
```

**Tone carries the rung; the dot's shape carries the kind.** Filled means the
session is standing by itself and wants something (waiting, or fallen over).
Hollow means there is something here to accept. That reuses the shape `starting`
already draws instead of spending brightness, which is the axis this interface
uses for rank and legibility. If a ring ever fails to read on some renderer, the
stated fallback is **size rather than fill**: filled 6px against filled 4px,
worse but legible anywhere. The 1.5px border is a floor, because it is the first
thing rounding eats.

**`running` is not on the scale at all.** That follows from the argument rather
than being an omission: a running session is not standing and needs nobody - it
is the one thing on screen that moves by itself. It stays plain text in the
rail.

**`failed` is its own rung, above waiting**, with a filled dot in `agent_error`,
the role the rail's failed dot uses. Its pill takes that hue through the same two
blends `accent_surface` and `accent_border` use (`UiColors::tinted_surface` /
`tinted_border`), so the top rung cannot drift from the one below it. The accent
deliberately does not sit at the top of the brightness range, so that red has
somewhere to go. The objection - "everything else clears by being read, and an
error that vanishes when glanced at is wrong" - is answered, not waived:
**nothing on the reading axis clears a failure.** It goes when the session moves
on. It does displace waiting in the chip, which is the point, and the popover
lists all three sections in the same order.

**Every value in the chip is a role.** *A hex literal in a spec is a bug report
about a missing role, never a value to ship* - in this codebase most of all,
where a hard-coded colour is one no theme can reach. A version of the chip
measured in contrast ratios off a screenshot shipped seven literals. Six sat
1.01-1.08 : 1 from an existing role, below the step the eye cannot resolve, and
are now those roles: `subtle` fill, `text_tertiary` text, `dim` dot,
`border_hover` hover. The seventh is the one place the dark and light prototypes
named **different** roles - `border_strong` in dark, `border` in light - and
`UiColors` writes one name, so it cannot be both. It is `border_strong`, because
that is the only single role that keeps the chip's edge as visible on its own
fill as a button's is on its own. The chip sits on `subtle` and a button on
`overlay`, so the chip measures 1.180 : 1 against the button's 1.173 in dark, and
1.408 against 1.505 in light, where plain `border` would give 1.246.

**The popover is named `Activity`; the title names the surface and the sections
name the facts.** `Waiting for you` as a title stops being true the moment the
chip can open the popover with nothing waiting, so it is a section heading
instead (`N waiting for you`), where it is always true. The subtitle is a summary
(`1 failed · 2 waiting · 3 finished`, or `nothing waiting`). The name is not
`Attention queue`: that is the internal name, half of what is here makes no claim
on attention, and "queue" promises an order to work through.

**The empty state defines rather than denies:**

```
Nothing in Activity
Failed, waiting and unread runs appear here.
```

Three absences cannot be denied in one clause, and denying them in three
("nothing failed, nobody waiting, everything collected") makes the reader audit a
list at the exact moment there is nothing to audit. An empty surface is the one
place where naming what it *holds* is not redundant. The sentence is true of all
three memberships by construction, because that list **is** the membership rule.
It introduces no new term, it is true whether the reader watched the last row
clear or opened an empty popover, and it is read by somebody who is by definition
not in a hurry. It replaced `No agent is waiting for you`, which did not lie but
answered about one of three memberships in the app's most authoritative voice.
Two things go with it: the header count is dropped (the body already says it, and
one popover may not say it twice in two phrasings), and the `⌥⇥` footer is
**hidden** rather than reworded, because with nothing to cycle the honest version
of that line is no line.

**Triage lives in the sort order**: a run that fell over, then agents holding a
question, then results nobody has collected - `1 run failed`, `N waiting for
you`, `N finished while you were away`, each with a rule above it. Outside, the
chip answers *is anything standing?*; inside, the list answers *where do I
start?* It is one list in three sections, not three lists (`QueueKind` orders
`sort_rows`), so the arrows and `enter` walk every row and a click reaches every
destination. Clicking a finished row goes to that session **and clears that one
mark**, which makes the list a triage tool rather than a notice somebody has to
dismiss.

**The footer follows the sections too** (`waiting::cycle_hint`). With something
waiting it reads `⌥⇥ cycles them · each is answered in its own terminal`, naming
the chord that is actually bound. The chord walks only the waiting rows, so with
nothing waiting it reaches nothing: the footer drops the chord and reads only
`opening one marks it read`. The footer is drawn whenever any section is, and
never over an empty popover, where it would describe an empty set.

**The popover has no `running` section, and that is a decision.** There are
three reasons, and the third is the one to keep. `running` normally holds every
agent, so a list of it helps nobody find anything. `running` is not a claim on
attention, and a third kind of section blurs the distinction the rest of the
design is built on. And **the other sections clear by being read or by the
session moving on**, while `running` clears only when the work ends, which would
turn the popover from "what wants me, and what happened while I was out" into a
dashboard held open. The rail is already the list of everything.

## The queue's edge, and the one thing this app announces

**The rule:** the ephemeral channel announces, the persistent one remembers, and
neither may be the only one. The dot, the chip and the rail row are all the
persistent kind: always true and never an event. So a session that goes quiet
while the reader is looking at another pane changes nothing in their field of
view. The case that raised it came from a day's live use: when a mark silently
becomes `finished`, it is very hard to notice unless you are staring at that
spot. The chip's scale does not answer that, and the reason is worth keeping: it
makes the change legible **in the badge the reader is already looking at**, and
this reader's eyes are elsewhere.

**The edge is a property of the queue, not of the person.**

> One signal at the moment the attention queue goes from "nobody is waiting for
> you" to "somebody is". While anybody is already in it - silence.

`app/announce.rs` enforces two limits, because the queue's own emptiness is the
only input it reads. Coming back to the desk is not an edge; nor is the machine
waking, the window taking focus, a repaint, a theme change, a rail reorder or a
filter - none of those change `ActivityCounts`. A **top-up** is not an edge
either: three in the queue, two dealt with, a fourth arrives, and nothing fires.
Only `0 -> >=1`. Emptying the queue is deliberately silent, and the argument is
stronger than "it needs no action": the queue emptied **because you emptied
it**, and a signal about a change you just caused is the definition of noise.

**Movement rather than sound**, because the complaint covers three audiences and
only the third wants a noise:

| who | what serves them |
|---|---|
| in the app, looking at another pane | peripheral vision catching **movement**; the chip is permanently on screen |
| away from the machine entirely | the sticky mark, which already works |
| at the machine, not looking at the display | **only here** does sound help |

The reported case was the first. Sound remains a legitimate future on this same
edge, and it inherits two properties from serving only the third case: off by
default, and **never the only announcement**. A product whose finishes are
inaudible with sound off has to stay visible.

**The movement, exactly:**

```
duration        480 ms total, one iteration, no delay, no repeat
dot, scale      1 -> 1.9      0-140 ms    cubic-bezier(0.16, 1, 0.3, 1)
                1.9 -> 1      140-420 ms  cubic-bezier(0.33, 1, 0.68, 1)
halo, spread    0 -> 10px     0-480 ms    linear
halo, opacity   0.45 -> 0     0-480 ms    linear
halo, colour    the ACCENT, in every rung - never the dot's own tone
origin          centre
```

**Both ends of the duration are argued, and the argument outlives the number.**
Below about 300 ms, a peripheral event is *detected but not localised*: the
reader notices "something" and cannot tell where, which sends them hunting. Above
about 600 ms, it starts to read as a cycle, and the eye waits for a second beat.
480 ms registers, localises and finishes.

**Area is the payload.** Peripheral vision integrates a change of area far better
than it resolves a 6px shape, so the halo does the work, and the dot's growth
exists to make the halo look *caused* rather than decorative. **The digit does
not move** - an animated number reads as instability, and the count is a fact
rather than an event - and **the icon's box does not move**: nothing should
slosh, and displacement is what turns a movement into an interruption.

**The halo takes the accent in every rung.** The motivating case is `finished`,
which is correctly drawn muted, and a halo of 45% muted grey on a dark rail at
10px is approximately nothing. A halo in the dot's own tone would give the
weakest announcement in exactly the case this feature exists for. The resolution
is a distinction the design already uses everywhere:

> The dot is a **state** and carries the rank. The halo is an **event** and
> carries none. The announcement is one act performed identically whether a
> question arrived or a result did; the rank is in what stays on screen
> afterwards, and that does not change.

**The accent is legal there only because the halo ends at zero.** The accent
means "something wants you", and a mark that lives 480 ms and leaves no trace
makes no standing claim. If it settled on any non-zero value, the argument would
collapse, so "the halo ends at zero" is load-bearing rather than tidy, and
`the_pulse_starts_and_ends_at_rest` is the machine form of it. No new colour is
introduced: the accent already exists in both themes. The dot's own fill is
untouched, so `finished` stays muted **as a state**.

**What pulses.** The chip's dot, once, on the edge: eight finishes in one tick
are one edge and one pulse. And the rail dot of **each session that made the
edge**: if two arrive together, both glyphs pulse and the chip still pulses
once, because both arrived and the edge is still one edge. Never on window
focus, window show, wake, a theme change, a rail reorder, a filter change, a pane
focus or a repaint.

**The pulse is app state, not an element lifecycle, and that is the
implementation trap.** The pulse is bound to a **state transition**, never to an
element appearing. Reordering the rail remounts its rows, and an animation keyed
on mounting would replay a whole queue's worth of pulses every time the order
changed. So `refresh_attention_edge` records the edge once a frame from
`sync_surface_facts`, after that frame's own clearing of on-screen marks, so a
run that ended under the reader's eyes never enters the queue and never
announces. The drawing asks whether *this* dot is in *this* announcement, and a
520 ms timer takes the announcement down. Worth knowing before touching it: the
other animations in this build repeat (`.repeat()` - skeleton, splash, spinner),
so "play once, exactly on a state change" is new machinery rather than an
application of existing machinery.

**GPUI's animation is the frame pump, not the clock**, which is the other half
of that trap. `with_animation`'s own delta restarts at zero whenever the element
state is new, and element state is new after a remount. A row scrolled out and
back within the 480 ms would play the pulse a second time from the start, and a
dot laid out late would start a fresh 480 ms out of phase with the chip's.
`AnnouncePhase` carries the **edge's own instant**, so the drawing asks where the
announcement actually is: a remount resumes, a late mount joins, and an
announcement older than its duration draws at rest. The generation still keys
the element id, but only so a second edge replaces the first rather than
continuing it.

**Coming back is not an edge, and the frame loop is what makes that hard.** The
edge is observed only when the window paints. A minimised window, a fully
occluded one or a sleeping machine paints nothing, while the state pass and the
hook handler keep marking finished runs on their own timers. So the first frame
back would compare a latch from *before* the absence against a queue filled
*during* it, and announce - costing somebody who stepped away a noise they cannot
act on any faster. `rebaseline_attention_edge`, called from the
window-activation observer, makes that frame **adopt** what it finds instead of
reading it as a crossing, which is the rule the very first frame already applies.
It says what the app actually knows: the queue is different now, and *when* it
changed is not something an unobserved period can answer. An edge nobody could
have seen is not an edge this app may claim.

**Three motion tokens, and three conditions on adding a fourth.**
`motion::ANNOUNCE` (480 ms), `ANNOUNCE_RISE` and `ANNOUNCE_SETTLE` are named by
the **work**, never by the shape: a curve called `EASE_OUT_EXPO` becomes
universal, and the next animation then picks it by taste instead of by argument.
The two curves are a **pair belonging to one animation**: the next animation
either reuses them, which means it is the same act, or argues for its own pair.
And a token lands **in the same commit that uses it**. A test in `ui_tokens.rs`
asserts the lists are closed, which is not the same as immutable: growing one is
a decision somebody has to write down.

**`prefers-reduced-motion` is specified and deliberately not built.** GPUI
exposes no source for the flag. The entire difference between the two modes
would be *a scale in place, 5 pixels on a six-pixel dot, once, over 420 ms*: no
translation, no parallax, no rotation, no zoom - none of the classes the media
query exists to suppress. So the full pulse ships and **no setting is added**,
because that setting would be the first control in this app whose only job is to
undo a decision the app made. The reduced form is on record in case a source
appears: the same halo with frozen geometry (10px fixed), opacity `0 -> 0.45` by
80 ms and back to zero by 480, and the dot not moving. The reduced variant is the
full one **minus geometry**, not a different idea. On macOS, the cheap route to
the flag is `NSWorkspace`, which exposes it directly and posts on change.

**A per-session announcement flag was refused, and both arguments are
reusable.** It is *asked at the wrong moment*: it would have to be set when a
session starts, exactly when nobody yet knows whether anyone will be waiting on
the result. And it *rots silently*: set on Monday, wrong by Thursday, with
nothing to say so. A global setting has no such rot, because it describes **how
you work** and changes when that does. If the need survives long live use, it
comes back **with its own word, named when it is introduced**, not folded into
an existing affordance, and specifically not into `pinned`.

**The notification ladder is the second, independent half of this design**, in
`settings/tabs/notifications.rs`: three rungs, no zero rung,
`agent_panel.notify_level`, default `Waiting`. That file's header explains why
three positions do not break "One switch" and why the absent bottom rung
replaces a lock.

**An in-app toast per completion was refused**, and that is a decision, not a
gap. There are already three channels - the status dot, the title bar's chip, and
the system notification - and with four agents running, a toast per finish is a
storm that competes with the dot, which is the honest signal. A completion is
information; `waiting for you` is a claim on attention. The interface already
spends two different colours on that distinction, and
`DesktopNotificationUrgency` spends two levels on it; flattening them would lose
both. What the chip says about a finished session is the scale above - never a
toast.
