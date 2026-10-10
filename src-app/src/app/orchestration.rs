//! Sessions opened by other sessions.
//!
//! An agent in a pane can open agent sessions of its own and work through
//! them. What it may do is decided here, on the server, from two facts the
//! caller cannot choose: which pane its process is running under
//! ([`crate::caller`]) and which session opened which
//! ([`crate::project::Thread::opened_by`]). The reasoning behind each rule is
//! in `docs/internals/design-decisions.md`, "A session works through the
//! sessions it opened".

use std::collections::HashMap;
use std::time::Duration;

use gpui::{App, Context, Entity};

use crate::SplitlaneApp;
use crate::app::drive::{Drive, Standing};
use crate::app::ipc_handler::{JSONRPC_ERROR_KEY, JsonRpcError};
use crate::app::targeting::PaneRefusal;
use crate::caller::CallerLineage;
use crate::pane::TabContent;
use crate::terminal::view::TerminalView;

/// How many sessions one session may have open at a time.
///
/// Counted in sessions and not in panes: a session with no pane to go to is
/// opened without one, so room on screen is never what stops the ninth.
pub(crate) const MAX_OPENED_SESSIONS: usize = 8;

/// The text put in front of whatever one session writes into another.
///
/// It lands in the receiving agent's own transcript, so the agent, a person
/// reading the pane and anybody opening the conversation later all see that
/// no person typed what follows.
pub(crate) fn provenance_line(sender_title: &str) -> String {
    format!(
        "[Splitlane] Sent by the agent session {}, not typed by a person.",
        crate::app::send_answer::quoted_title(sender_title)
    )
}

/// Whether a session's first prompt goes into its launch command instead of
/// being written into the agent's input line afterwards.
///
/// Only a prompt that is to be submitted: an argument cannot be left on the
/// input line for a person to read first. And only where the text survives
/// the trip. On Windows an agent installed from npm is started through a
/// `.cmd` wrapper, which does not carry a line break in an argument, so
/// there the prompt is still written into the input line.
pub(crate) fn prompt_goes_at_launch(
    agent: crate::agent_launcher::TerminalAgent,
    submit: bool,
    text: &str,
) -> bool {
    submit
        && agent.takes_prompt_at_launch()
        && !cfg!(windows)
        // The agent would read it as one of its own options.
        && !text.starts_with('-')
        && !text.contains('\0')
}

/// Whether text may be typed into a session rather than pasted
/// ([`crate::agent_launcher::TerminalAgent::takes_typed_text`]).
///
/// Typed text is only safe in front of the agent it was measured on. At a
/// shell every line feed runs the line before it, and a shell is what is left
/// in the pane when the agent has gone. So the agent's own process has to be
/// under the pane and its interface up; anything less is pasted, which a
/// shell holds as one piece of text.
pub(crate) fn typed_text_is_safe(look: OpeningLook) -> bool {
    look.agent_found && look.entry != crate::agent_state::TextEntry::NoInterface
}

/// Leave `text` where the agent's shim collects it by `key`, readable by the
/// user alone. `None` when there is nowhere to leave it.
fn stash_opening_prompt(key: &str, text: &str) -> Option<std::path::PathBuf> {
    use std::io::Write;

    let dir = crate::runtime_paths::opening_prompt_dir()?;
    // A prompt nobody collected is removed by the wait that follows it. One
    // left by an app that quit inside that wait is somebody's text with no
    // further use, and this is the next time anything looks in here.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age > OPENING_PROMPT_AGENT_MAX * 4);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    // Written beside and moved into place, so the shim finds the whole text
    // or nothing: it may look while this is still being written.
    let (path, beside) = (dir.join(key), dir.join(format!("{key}.tmp")));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&beside)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| std::fs::rename(&beside, &path));
    if let Err(e) = written {
        log::warn!("opening prompt: could not be left for the agent's shim: {e}");
        let _ = std::fs::remove_file(&beside);
        return None;
    }
    Some(path)
}

/// How long an opening prompt waits for the new session's agent to be there
/// to read it ([`opening_prompt_step`]) before it is given up on.
const OPENING_PROMPT_AGENT_MAX: Duration = Duration::from_secs(30);
const OPENING_PROMPT_POLL: Duration = Duration::from_millis(200);
/// And then for its screen to stop changing.
const OPENING_PROMPT_SETTLE_FLOOR: Duration = Duration::from_millis(700);
const OPENING_PROMPT_SETTLE_MAX: Duration = Duration::from_secs(8);

/// What an opening prompt's wait sees of its session on one look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpeningLook {
    /// The agent's own file or a hook frame has spoken for it.
    pub spoken_for: bool,
    /// The agent's own process was under the pane at the last state pass.
    pub agent_found: bool,
    /// This agent's text cursor is known to say where typed text would go.
    pub cursor_marks_text_entry: bool,
    pub entry: crate::agent_state::TextEntry,
    /// The rail says the session is waiting for a person.
    pub waiting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpeningStep {
    Write,
    Wait,
    GiveUp(crate::rail_state::NotWritten),
}

/// Whether an opening prompt may be written now, `waited` after its session
/// was opened.
///
/// It goes once something says an agent is there to read it:
///
/// - the agent's own file or a hook frame has spoken, which is how Claude
///   Code is known to be at its input line;
/// - or the agent's process is under the pane and its interface shows a text
///   cursor. This is for an agent nothing speaks for before its first prompt.
///   Codex is one: it writes no file and sends no frame until a prompt is
///   submitted, so the prompt waited for a sign that could only follow it,
///   and was never written.
///
/// A session that is waiting for a person is given up on at once and not
/// waited through. Whatever it asks - whether to trust the folder is the
/// usual one - the answer is the person's, and an Enter written there would
/// give it for them. The prompt is not kept for later either: text that
/// appears in a pane minutes after it was sent, when somebody finally
/// answers, is a surprise to whoever is typing there by then.
pub(crate) fn opening_prompt_step(look: OpeningLook, waited: Duration) -> OpeningStep {
    use crate::agent_state::TextEntry;
    use crate::rail_state::NotWritten;
    if look.waiting {
        return OpeningStep::GiveUp(NotWritten::Waiting);
    }
    let takes_text =
        look.cursor_marks_text_entry && look.agent_found && look.entry == TextEntry::Open;
    if look.spoken_for || takes_text {
        OpeningStep::Write
    } else if waited >= OPENING_PROMPT_AGENT_MAX {
        OpeningStep::GiveUp(NotWritten::NoAgent)
    } else {
        OpeningStep::Wait
    }
}

/// How long a request to stop a turn stands. Past this, a run that ends was
/// not ended by that request, and what is on the session's input line is not
/// ours to remove.
pub(crate) const INTERRUPT_ASK_STANDS: Duration = Duration::from_secs(30);

/// Why a request was refused. Nothing was done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The caller is in no pane of this app, and the app was not launched
    /// with the variable that lets a script outside one do this.
    NotFromAPane,
    /// The caller already has [`MAX_OPENED_SESSIONS`] sessions running.
    WorkerCeiling,
    /// The caller was itself opened by a session.
    OpenedSessionCannotOpen,
    /// A pane was required and there is none to give.
    NoRoom,
    /// The target is not a session the caller opened.
    NotYours,
    /// The target is the caller's own session.
    Itself,
    /// The target is waiting for a person.
    Waiting,
    /// The target is in another project than the caller.
    OtherProject,
    /// The target is in the middle of a turn, and closing it would lose it.
    TurnInFlight,
    /// No key is known that stops a turn of the target's agent.
    NoInterrupt,
    /// The target finished a turn a person started, and the person has not
    /// looked at it yet.
    UnseenByPerson,
    /// A person was asked whether the caller may write into the target, and
    /// said no.
    PersonDeclined,
}

impl Refusal {
    /// The JSON-RPC code every refusal travels under.
    pub(crate) const CODE: i32 = -32004;

    /// The word a program branches on. Published: do not reword.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Refusal::NotFromAPane => "not_from_a_pane",
            Refusal::WorkerCeiling => "worker_ceiling",
            Refusal::OpenedSessionCannotOpen => "opened_session_cannot_open",
            Refusal::NoRoom => "no_room",
            Refusal::NotYours => "not_yours",
            Refusal::Itself => "self",
            Refusal::Waiting => "waiting",
            Refusal::OtherProject => "other_project",
            Refusal::TurnInFlight => "turn_in_flight",
            Refusal::NoInterrupt => "no_interrupt",
            Refusal::UnseenByPerson => "unseen_by_person",
            Refusal::PersonDeclined => "person_declined",
        }
    }

    fn sentence(self) -> String {
        match self {
            Refusal::NotFromAPane => "the caller is not running in a pane of this app; a script \
                 outside one needs SPLITLANE_IPC_ORCHESTRATION=1 (SPLITLANE_IPC_SCRIPTING=1 to \
                 submit) set where Splitlane was launched"
                .to_string(),
            Refusal::WorkerCeiling => format!(
                "this session already has {MAX_OPENED_SESSIONS} sessions it opened still \
                 running; close one first"
            ),
            Refusal::OpenedSessionCannotOpen => "a session opened by another session cannot \
                 open sessions; do the task in this one"
                .to_string(),
            Refusal::NoRoom => "no pane is empty and another would not fit; ask without \
                 requiring a pane to open the session in the rail"
                .to_string(),
            Refusal::NotYours => "the target is not a session the calling session opened; a \
                 session works only through the sessions it opened"
                .to_string(),
            Refusal::Itself => "the target is the calling session itself".to_string(),
            Refusal::Waiting => "the target is waiting for a person, and only a person \
                 answers that"
                .to_string(),
            Refusal::OtherProject => {
                "the target is in another project than the calling session".to_string()
            }
            Refusal::TurnInFlight => "the target is in the middle of a turn or is waiting for \
                 a person, and closing it loses that turn; wait for it to end, or ask to stop \
                 the turn as well"
                .to_string(),
            Refusal::NoInterrupt => "no key is known that stops a turn of the target's agent \
                 and leaves the agent running"
                .to_string(),
            Refusal::UnseenByPerson => "the target finished a turn a person started, and the \
                 person has not looked at it yet; closing it would take the result away \
                 from them"
                .to_string(),
            Refusal::PersonDeclined => "a person was asked whether the calling session may \
                 send messages to the target, and said no; they are not asked again until \
                 Splitlane restarts"
                .to_string(),
        }
    }

    pub(crate) fn into_value(self, method: &str) -> serde_json::Value {
        serde_json::json!({
            JSONRPC_ERROR_KEY: {
                "code": Self::CODE,
                "message": format!("{method} refused ({}): {}", self.word(), self.sentence()),
                "data": { "reason": self.word() },
            }
        })
    }
}

/// The pane a call came from, with what the rules need to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneCaller {
    pub surface_id: u64,
    pub thread_id: u64,
    pub ws_idx: usize,
    pub title: String,
    /// Whether this session was itself opened by a session.
    pub opened: bool,
}

/// Who is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A process in no pane of this app: a person's script, or anything the
    /// process table could not place.
    Outside,
    Pane(PaneCaller),
}

/// The variables the app was launched with. They are what a caller outside a
/// pane is allowed by, and a pane cannot change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaunchGates {
    pub orchestration: bool,
    pub scripting: bool,
}

impl LaunchGates {
    pub(crate) fn read() -> Self {
        Self {
            orchestration: crate::app::ipc_handler::ipc_orchestration_enabled(),
            scripting: crate::app::ipc_handler::ipc_scripting_enabled(),
        }
    }
}

/// Whether `caller` may open a session, having `already_open` running.
///
/// A caller in a pane needs nothing switched on. It is bounded instead: by a
/// count, and by depth - a session that was opened by one opens none.
pub(crate) fn may_open(
    caller: &Caller,
    already_open: usize,
    gates: LaunchGates,
    submit: bool,
) -> Result<(), Refusal> {
    match caller {
        Caller::Pane(pane) => {
            if pane.opened {
                return Err(Refusal::OpenedSessionCannotOpen);
            }
            if already_open >= MAX_OPENED_SESSIONS {
                return Err(Refusal::WorkerCeiling);
            }
            Ok(())
        }
        Caller::Outside => {
            let open = if submit {
                gates.scripting
            } else {
                gates.orchestration
            };
            if open {
                Ok(())
            } else {
                Err(Refusal::NotFromAPane)
            }
        }
    }
}

/// The session a write is aimed at, as the rules see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WriteTarget {
    /// Its surface record, when it has one.
    pub thread_id: Option<u64>,
    /// The record id of the session that opened it, if a session did.
    pub opened_by: Option<u64>,
    /// The rail says it is waiting for a person.
    pub waiting: bool,
    /// Where a person's leave for this caller to drive it stands.
    pub drive: Drive,
}

/// On what ground a write is let through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteLeave {
    /// The caller opened the target. The text is marked as sent by it.
    Opener,
    /// A person opened the target and let the caller drive it. The text is
    /// marked the same way.
    Driver,
    /// The app was launched with the variable that opens writes, which is
    /// what let a write through before sessions could own sessions. Nothing
    /// about such a write changes.
    LaunchVariable,
}

/// Why a write is not let through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteDenied {
    Refused(Refusal),
    /// A caller outside a pane, and the launch variable is not set: the
    /// answer such a caller has always had.
    GateClosed,
    /// The target is a session a person opened, and the person has not said
    /// whether the caller may write into it. They are asked; nothing is
    /// written.
    AskPerson,
}

/// The JSON-RPC code a write travels under when it put a question to a person
/// instead of being done. Not [`Refusal::CODE`]: a refusal is final, and this
/// is not an answer yet.
pub(crate) const ASKED_PERSON_CODE: i32 = -32005;

/// The response to a write that asked a person. The call does not wait for
/// the answer: a person can take any time to give one, and the caller has a
/// turn of its own to get on with.
pub(crate) fn asked_person_value(method: &str) -> serde_json::Value {
    serde_json::json!({
        JSONRPC_ERROR_KEY: {
            "code": ASKED_PERSON_CODE,
            "message": format!(
                "{method} asked a person (asked_person): the target is a session a person \
                 opened, and they are being asked whether the calling session may send it \
                 messages; nothing was written. Wait for their answer and send again"
            ),
            "data": { "reason": "asked_person" },
        }
    })
}

/// Whether `caller` may type into `target`.
///
/// Typing covers text and keys alike. `gate_open` is whether the app was
/// launched with the variable that opens writes.
///
/// A session that is waiting for a person is refused to every caller in a
/// pane, whatever else holds: the question on its screen was put to a person,
/// and a key from another agent would be an answer to it.
pub(crate) fn may_write(
    caller: &Caller,
    target: WriteTarget,
    gate_open: bool,
) -> Result<WriteLeave, WriteDenied> {
    let Caller::Pane(pane) = caller else {
        return if gate_open {
            Ok(WriteLeave::LaunchVariable)
        } else {
            Err(WriteDenied::GateClosed)
        };
    };
    if target.thread_id == Some(pane.thread_id) {
        return if gate_open {
            Ok(WriteLeave::LaunchVariable)
        } else {
            Err(WriteDenied::Refused(Refusal::Itself))
        };
    }
    if target.waiting {
        return Err(WriteDenied::Refused(Refusal::Waiting));
    }
    if target.opened_by == Some(pane.thread_id) {
        return Ok(WriteLeave::Opener);
    }
    if target.drive == Drive::Standing(Standing::Allowed) {
        return Ok(WriteLeave::Driver);
    }
    if gate_open {
        return Ok(WriteLeave::LaunchVariable);
    }
    match target.drive {
        Drive::Standing(Standing::Declined) => Err(WriteDenied::Refused(Refusal::PersonDeclined)),
        Drive::Standing(Standing::Asked) | Drive::NotAsked => Err(WriteDenied::AskPerson),
        Drive::NotOffered | Drive::Standing(Standing::Allowed) => {
            Err(WriteDenied::Refused(Refusal::NotYours))
        }
    }
}

/// Whether `caller` may stop the turn `target` is in.
///
/// Stopping a turn is a key written into the session, so the grounds are
/// those of [`may_write`], with one difference: a session never stops its own
/// turn, whatever the app was launched with. The call that asked would be
/// part of the turn it stopped.
///
/// A session that is waiting for a person is refused for the reason it is
/// refused a key: the key that stops a turn is, on a permission question, the
/// answer "no".
pub(crate) fn may_interrupt(
    caller: &Caller,
    target: WriteTarget,
    gate_open: bool,
) -> Result<WriteLeave, WriteDenied> {
    if let Caller::Pane(pane) = caller
        && target.thread_id == Some(pane.thread_id)
    {
        return Err(WriteDenied::Refused(Refusal::Itself));
    }
    may_write(caller, target, gate_open)
}

/// Whether `caller` may take `target` out of its pane or put it in one.
///
/// Neither types into the session, so whether it is waiting for a person does
/// not come into it. A caller outside a pane rearranges on the strength of the
/// launch variable, like the other calls that change the layout for a script.
pub(crate) fn may_arrange(
    caller: &Caller,
    target: WriteTarget,
    gates: LaunchGates,
) -> Result<(), Refusal> {
    let Caller::Pane(pane) = caller else {
        return if gates.orchestration {
            Ok(())
        } else {
            Err(Refusal::NotFromAPane)
        };
    };
    if target.thread_id == Some(pane.thread_id) {
        return Err(Refusal::Itself);
    }
    if target.opened_by == Some(pane.thread_id) {
        Ok(())
    } else {
        Err(Refusal::NotYours)
    }
}

/// The session a call to move one names.
struct ArrangedTarget {
    terminal: Entity<TerminalView>,
    target: WriteTarget,
    /// The project and the position in it of the session's record.
    position: Option<(usize, usize)>,
}

/// What decides whether the end of a run is told to a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunFacts {
    /// The state pass is what speaks for this session: it read the agent's
    /// own file this pass.
    pub detector_speaks: bool,
    /// The agent's own file says a person stopped the turn.
    pub interrupted: bool,
    /// The last message the session was given came from the session that
    /// opened it, and that session is still open.
    pub last_message_from_opener: bool,
}

/// Whether a run the state pass watched end is news for a person: a mark on
/// the row, and a notification if it ran long enough.
///
/// Counting the run for a waiting script is a different question and is not
/// asked here; a run that is not news still ended.
pub(crate) fn run_is_news(facts: RunFacts) -> bool {
    facts.detector_speaks && !facts.interrupted && !facts.last_message_from_opener
}

/// The session a close is aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CloseTarget {
    pub thread_id: u64,
    pub ws_idx: usize,
    pub opened_by: Option<u64>,
    /// The rail says `running` or `waiting`: a turn is in flight, or a
    /// question to a person is on screen.
    pub turn_in_flight: bool,
    /// Its row carries the mark of a finished run nobody has looked at. The
    /// mark is only ever put there for a person: a run its opener started is
    /// the opener's to read and leaves none.
    pub unseen_by_person: bool,
}

/// Whether closing a session whose rail says this would lose a turn.
///
/// `running` is a turn, and so is a question asked in the middle of one. A
/// question the agent asks before it has taken any prompt is not: it is read
/// off the pane's terminal, nothing has been asked of the agent yet, and a
/// session that opened it must be able to close it without a key that stops
/// a turn - which for most agents is not known.
pub(crate) fn closing_loses_a_turn(rail: &crate::rail_state::RailSnapshot) -> bool {
    use crate::project::ThreadStatus;
    match rail.status {
        ThreadStatus::Thinking => true,
        ThreadStatus::WaitingForInput => rail.source != crate::rail_state::RailSource::Terminal,
        ThreadStatus::Starting | ThreadStatus::Idle | ThreadStatus::Failed => false,
    }
}

/// Whether `caller` may close `target`: stop its process and drop its row.
///
/// The conversation is not what is lost - the agent keeps its own record and
/// the session can be opened again from history. What is lost is the turn, so
/// a session in the middle of one is closed only when the caller says it
/// means to stop it. The same line the app draws when it is quit with agents
/// working.
///
/// A session a person opened is never closed by a session, and nothing here
/// lets one be: that is not a matter of asking.
pub(crate) fn may_close(
    caller: &Caller,
    target: CloseTarget,
    gates: LaunchGates,
    stop_turn: bool,
) -> Result<(), Refusal> {
    match caller {
        Caller::Pane(pane) => {
            if target.thread_id == pane.thread_id {
                return Err(Refusal::Itself);
            }
            if target.ws_idx != pane.ws_idx {
                return Err(Refusal::OtherProject);
            }
            if target.opened_by != Some(pane.thread_id) {
                return Err(Refusal::NotYours);
            }
            // A person typed into a session this one opened, the answer
            // came, and they have not seen it. The row's mark is the only
            // thing telling them it is there, and it goes with the row.
            // Saying the turn may be stopped does not lift this: no turn is
            // what would be lost.
            if target.unseen_by_person {
                return Err(Refusal::UnseenByPerson);
            }
        }
        Caller::Outside => {
            if !gates.orchestration {
                return Err(Refusal::NotFromAPane);
            }
        }
    }
    if target.turn_in_flight && !stop_turn {
        return Err(Refusal::TurnInFlight);
    }
    Ok(())
}

/// A pane, as the choice of which one to give up sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneFacts {
    /// The surface record showing in it, if it shows one.
    pub thread_id: Option<u64>,
    /// Who opened that session.
    pub opened_by: Option<u64>,
    /// When the pane last took focus; 0 for never.
    pub focused: u64,
}

/// Which pane a session gives up to show `showing`, when none is free and
/// none can be added: the pane of another session it opened, the one that has
/// gone longest without focus. The index into `panes`, or `None`.
///
/// Its own pane and a pane showing anything it did not open are never
/// candidates. A person's layout is theirs, and a session that could take
/// their pane to show its own work would be deciding what they look at.
pub(crate) fn pane_to_give_up(panes: &[PaneFacts], opener: u64, showing: u64) -> Option<usize> {
    panes
        .iter()
        .enumerate()
        .filter(|(_, pane)| {
            pane.opened_by == Some(opener)
                && pane.thread_id.is_some()
                && pane.thread_id != Some(showing)
                && pane.thread_id != Some(opener)
        })
        .min_by_key(|(_, pane)| pane.focused)
        .map(|(idx, _)| idx)
}

/// What the caller asked for about a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlacementAsk {
    /// A pane if one is free or fits, the rail otherwise.
    Auto,
    /// The rail, whatever the screen looks like.
    Parked,
    /// A pane, or a refusal.
    Pane,
}

impl PlacementAsk {
    fn parse(value: Option<&serde_json::Value>) -> Result<Self, JsonRpcError> {
        match value.map(|v| v.as_str()) {
            None | Some(Some("auto")) => Ok(Self::Auto),
            Some(Some("parked")) => Ok(Self::Parked),
            Some(Some("pane")) => Ok(Self::Pane),
            Some(_) => Err(JsonRpcError::invalid_params(
                "placement must be \"auto\", \"parked\" or \"pane\"",
            )),
        }
    }
}

/// Why a session was opened without a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParkedBecause {
    Requested,
    Ceiling,
    TooNarrow,
}

impl ParkedBecause {
    fn word(self) -> &'static str {
        match self {
            ParkedBecause::Requested => "requested",
            ParkedBecause::Ceiling => "ceiling",
            ParkedBecause::TooNarrow => "too_narrow",
        }
    }
}

/// Where a newly opened session goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A pane that is showing nothing.
    EmptyPane,
    /// A pane added for it.
    NewPane,
    /// No pane: a row in the rail and a running process.
    Parked(ParkedBecause),
}

/// Choose where a newly opened session goes.
///
/// There is no rung that replaces what a pane is showing. The ordinary ladder
/// ends on "the focused pane, replaced", and the focused pane is as a rule the
/// caller's own - an agent opening five sessions would push itself and four
/// of them off the screen. A session with nowhere to go is opened in the rail
/// instead, where it has a row, a status and a place in the attention queue.
///
/// `another_pane` is the answer to "can a pane be added", `None` meaning yes.
pub(crate) fn place_opened(
    ask: PlacementAsk,
    has_empty_pane: bool,
    another_pane: Option<PaneRefusal>,
) -> Result<Placement, Refusal> {
    if ask == PlacementAsk::Parked {
        return Ok(Placement::Parked(ParkedBecause::Requested));
    }
    if has_empty_pane {
        return Ok(Placement::EmptyPane);
    }
    let because = match another_pane {
        // A project showing no pane at all takes the session as its first.
        None | Some(PaneRefusal::NoPanes) => return Ok(Placement::NewPane),
        Some(PaneRefusal::EmptyPaneAlready) => return Ok(Placement::EmptyPane),
        Some(PaneRefusal::Ceiling) => ParkedBecause::Ceiling,
        Some(PaneRefusal::TooNarrow { .. } | PaneRefusal::TooNarrowGridFits { .. }) => {
            ParkedBecause::TooNarrow
        }
    };
    match ask {
        PlacementAsk::Pane => Err(Refusal::NoRoom),
        PlacementAsk::Auto | PlacementAsk::Parked => Ok(Placement::Parked(because)),
    }
}

impl SplitlaneApp {
    /// Every terminal this app holds, on screen or not, with the id of its
    /// surface record when it has one.
    fn held_terminals(&self, cx: &App) -> Vec<(Entity<TerminalView>, Option<u64>)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for container in &self.workspaces {
            for pane in container.collect_panes() {
                for terminal in pane.read(cx).terminals() {
                    if seen.insert(terminal.entity_id()) {
                        out.push((terminal.clone(), terminal.read(cx).agent_thread_id));
                    }
                }
            }
        }
        for (thread_id, terminal) in &self.agents_view.agents_terminal_view_cache {
            if seen.insert(terminal.entity_id()) {
                out.push((terminal.clone(), Some(*thread_id)));
            }
        }
        out
    }

    /// Place a call in the pane it came from.
    pub(crate) fn caller_of(&self, lineage: &CallerLineage, cx: &App) -> Caller {
        let mut pty_children: HashMap<u32, (u64, Option<u64>)> = HashMap::new();
        for (terminal, thread_id) in self.held_terminals(cx) {
            let view = terminal.read(cx);
            // A pane whose shell has exited no longer vouches for anything:
            // its pid is free to be handed to an unrelated process.
            if view.terminal.child_pid == 0 || view.terminal.exited.is_some() {
                continue;
            }
            pty_children.insert(
                view.terminal.child_pid,
                (terminal.entity_id().as_u64(), thread_id),
            );
        }
        let Some((surface_id, Some(thread_id))) = crate::caller::pane_of(lineage, &pty_children)
        else {
            return Caller::Outside;
        };
        let found = self
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, container)| {
                let thread = container.threads.iter().find(|t| t.id == thread_id)?;
                Some(PaneCaller {
                    surface_id,
                    thread_id,
                    ws_idx,
                    title: thread.title.clone(),
                    opened: thread.opened_by.is_some(),
                })
            });
        found.map_or(Caller::Outside, Caller::Pane)
    }

    /// What the rules need to know about the surface a write is aimed at.
    pub(crate) fn write_target(&self, terminal: &Entity<TerminalView>, cx: &App) -> WriteTarget {
        let thread_id = terminal.read(cx).agent_thread_id.or_else(|| {
            self.agents_view
                .agents_terminal_view_cache
                .iter()
                .find(|(_, held)| *held == terminal)
                .map(|(thread_id, _)| *thread_id)
        });
        WriteTarget {
            thread_id,
            opened_by: thread_id
                .and_then(|id| self.thread_by_id(id))
                .and_then(|thread| thread.opened_by.as_ref())
                .map(|by| by.id),
            waiting: self
                .rail_snapshot_for(terminal, thread_id, cx)
                .is_some_and(|rail| rail.status == crate::project::ThreadStatus::WaitingForInput),
            drive: Drive::NotOffered,
        }
    }

    /// [`Self::write_target`] for a call from `caller`: the same facts, and
    /// where a person's leave for this caller stands.
    pub(crate) fn write_target_for(
        &self,
        caller: &Caller,
        terminal: &Entity<TerminalView>,
        cx: &App,
    ) -> WriteTarget {
        let target = self.write_target(terminal, cx);
        WriteTarget {
            drive: self.drive_between(caller, target),
            ..target
        }
    }

    /// Whether a person can be asked about this caller and this target, and
    /// what they said if they were.
    ///
    /// Only an agent session a person opened, in the caller's own project,
    /// is something to ask about. A shell is not: text written to a shell is
    /// a command. And a session that was itself opened by one asks for
    /// nothing - it has a task, not a plan of its own.
    fn drive_between(&self, caller: &Caller, target: WriteTarget) -> Drive {
        let (Caller::Pane(pane), Some(thread_id)) = (caller, target.thread_id) else {
            return Drive::NotOffered;
        };
        if pane.opened || target.opened_by.is_some() || thread_id == pane.thread_id {
            return Drive::NotOffered;
        }
        let in_callers_project = self.workspaces.get(pane.ws_idx).is_some_and(|container| {
            container.threads.iter().any(|thread| {
                thread.id == thread_id
                    && thread.terminal_agent.is_some()
                    && !crate::app::agents_sidebar::is_shell_surface(thread)
            })
        });
        if !in_callers_project {
            return Drive::NotOffered;
        }
        match self.drive.standing(pane.thread_id, thread_id) {
            Some(standing) => Drive::Standing(standing),
            None => Drive::NotAsked,
        }
    }

    /// The response to a write the rules did not let through, having done
    /// what the denial calls for: a question to a person is put here.
    pub(crate) fn deny_write(
        &mut self,
        denied: WriteDenied,
        caller: &Caller,
        target: WriteTarget,
        method: &str,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        match denied {
            WriteDenied::Refused(refusal) => refusal.into_value(method),
            WriteDenied::GateClosed => crate::app::ipc_handler::write_gate_refusal(method),
            WriteDenied::AskPerson => {
                if let (Caller::Pane(pane), Some(thread_id)) = (caller, target.thread_id)
                    && self.drive.ask(pane.thread_id, thread_id)
                {
                    // Told once, when the wait begins: a second question put
                    // while the first stands adds a row, not a second
                    // notification.
                    let first = self.drive.asked_by(pane.thread_id).len() == 1;
                    self.show_drive_questions(pane.thread_id, cx);
                    if first {
                        self.notify_drive_question(pane.thread_id, cx);
                    }
                }
                asked_person_value(method)
            }
        }
    }

    /// Tell the person `asker` is waiting for them, and for what: the title
    /// every wait has, and a body that says the answer is theirs to give and
    /// not the agent's.
    fn notify_drive_question(&mut self, asker: u64, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_by_id(asker) else {
            return;
        };
        let Some(agent) = thread.terminal_agent else {
            return;
        };
        let title = crate::project::clean_sidebar_title(&thread.title)
            .unwrap_or_else(|| thread.title.clone());
        // From the questions themselves, not from the row's message: that
        // may be holding a question the agent was already asking.
        let names: Vec<String> = self
            .drive_questions_of(asker)
            .into_iter()
            .map(|question| question.target_name.to_string())
            .collect();
        if names.is_empty() {
            return;
        }
        let message = crate::app::drive::question_summary(&names);
        let Some(ws_id) = self
            .workspaces
            .iter()
            .find(|container| container.threads.iter().any(|t| t.id == asker))
            .map(|container| container.id)
        else {
            return;
        };
        let seen = self.thread_is_seen(asker, cx);
        crate::app::ipc_handler::fire_attention_notification(
            agent,
            self.notification_subject(ws_id, title, cx),
            Some(&format!("It {message}.")),
            &self.cached_config,
            seen,
            cx.background_executor().clone(),
        );
    }

    /// The questions `asker` has standing, as its pane draws them.
    pub(crate) fn drive_questions_of(&self, asker: u64) -> Vec<crate::pane::DriveQuestion> {
        let name = |thread: &crate::project::Thread| {
            gpui::SharedString::from(
                crate::project::clean_sidebar_title(&thread.title)
                    .unwrap_or_else(|| thread.title.clone()),
            )
        };
        let Some(asker_name) = self.thread_by_id(asker).map(name) else {
            return Vec::new();
        };
        self.drive
            .asked_by(asker)
            .into_iter()
            .filter_map(|target| {
                Some(crate::pane::DriveQuestion {
                    asker,
                    target,
                    asker_name: asker_name.clone(),
                    target_name: name(self.thread_by_id(target)?),
                })
            })
            .collect()
    }

    /// The person handed `target` to `asker` from the menu, without being
    /// asked: the same as answering yes, and it lifts an earlier no.
    pub(crate) fn give_drive(&mut self, asker: u64, target: u64, cx: &mut Context<Self>) {
        self.drive.give(asker, target);
        // A question about this pair that was standing is answered by it.
        self.show_drive_questions(asker, cx);
    }

    /// The person took `target` back. Its turn is not stopped; the session
    /// that drove it can no longer write into it, and may ask again.
    pub(crate) fn stop_drive(&mut self, target: u64, cx: &mut Context<Self>) {
        if self.drive.take_back(target).is_some() {
            cx.notify();
        }
    }

    /// The name of the session that drives `target`, when one does.
    pub(crate) fn driver_name(&self, target: u64) -> Option<String> {
        let driver = self.thread_by_id(self.drive.driver_of(target)?)?;
        Some(
            crate::project::clean_sidebar_title(&driver.title)
                .unwrap_or_else(|| driver.title.clone()),
        )
    }

    /// The sessions a person could hand `target` to: the agent sessions of
    /// its project that a person opened, which are the only ones that can
    /// ask. Empty when `target` is not something that can be handed over - a
    /// shell, or a session another session opened, which its opener drives.
    pub(crate) fn drive_candidates(&self, target: u64) -> Vec<(u64, String)> {
        let is_agent = |thread: &crate::project::Thread| {
            thread.terminal_agent.is_some()
                && !crate::app::agents_sidebar::is_shell_surface(thread)
                && thread.opened_by.is_none()
        };
        let Some(container) = self
            .workspaces
            .iter()
            .find(|container| container.threads.iter().any(|t| t.id == target))
        else {
            return Vec::new();
        };
        if !container
            .threads
            .iter()
            .any(|thread| thread.id == target && is_agent(thread))
        {
            return Vec::new();
        }
        container
            .threads
            .iter()
            .filter(|thread| thread.id != target && is_agent(thread))
            .map(|thread| {
                (
                    thread.id,
                    crate::project::clean_sidebar_title(&thread.title)
                        .unwrap_or_else(|| thread.title.clone()),
                )
            })
            .collect()
    }

    /// The person answered the question about `asker` and `target`.
    pub(crate) fn answer_drive(
        &mut self,
        asker: u64,
        target: u64,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        if self.drive.answer(asker, target, allow) {
            self.show_drive_questions(asker, cx);
        }
    }

    /// Make `asker`'s row say what its questions say: waiting for a person
    /// while one stands, and what it said before once none does.
    ///
    /// The question is the app's and not the agent's, so the agent's own
    /// state knows nothing of it. For a session the state pass reads, the
    /// pass keeps the word up on every reading; for one only a hook speaks
    /// for, the word is put here and what it replaced is remembered.
    pub(crate) fn show_drive_questions(&mut self, asker: u64, cx: &mut Context<Self>) {
        use crate::project::ThreadStatus;
        const OURS: &str = "wants to send messages to ";
        let names: Vec<String> = self
            .drive
            .asked_by(asker)
            .into_iter()
            .filter_map(|id| self.thread_by_id(id))
            .map(|thread| {
                crate::project::clean_sidebar_title(&thread.title)
                    .unwrap_or_else(|| thread.title.clone())
            })
            .collect();
        let before = self.drive_status_before.get(&asker).copied();
        let Some(thread) = self.thread_by_id_mut(asker) else {
            return;
        };
        let mut remember = None;
        let mut forget = false;
        if names.is_empty() {
            if thread.status == ThreadStatus::WaitingForInput
                && let Some(before) = before
            {
                thread.status = before;
            }
            // Only our own text: a question the agent itself was asking when
            // ours was put beside it is still being asked.
            if thread
                .rail
                .waiting_message
                .as_deref()
                .is_some_and(|message| message.starts_with(OURS))
            {
                thread.rail.waiting_message = None;
            }
            forget = true;
        } else {
            if thread.status != ThreadStatus::WaitingForInput {
                remember = Some(thread.status);
                thread.status = ThreadStatus::WaitingForInput;
            }
            if thread
                .rail
                .waiting_message
                .as_deref()
                .is_none_or(|message| message.starts_with(OURS))
            {
                thread.rail.waiting_message = Some(crate::app::drive::question_summary(&names));
            }
        }
        if forget {
            self.drive_status_before.remove(&asker);
        } else if let Some(status) = remember {
            self.drive_status_before.entry(asker).or_insert(status);
        }
        self.sync_attention(cx);
        self.publish_rail_changes(cx);
        cx.notify();
    }

    /// The target of a call that names one, with the project and position of
    /// its record. `Err` is the response to send.
    fn arranged_target(
        &self,
        params: &serde_json::Value,
        cx: &App,
    ) -> Result<ArrangedTarget, serde_json::Value> {
        if params.get("surface_id").is_none_or(|v| v.is_null())
            && params
                .get("name")
                .and_then(|n| n.as_str())
                .is_none_or(str::is_empty)
        {
            return Err(JsonRpcError::invalid_params(
                "A target is required: 'surface_id' or 'name'",
            )
            .into_value());
        }
        let terminal = self
            .resolve_surface(params, cx)
            .map_err(JsonRpcError::into_value)?;
        let target = self.write_target(&terminal, cx);
        let position = target.thread_id.and_then(|thread_id| {
            self.workspaces
                .iter()
                .enumerate()
                .find_map(|(ws_idx, container)| {
                    let thread_idx = container.threads.iter().position(|t| t.id == thread_id)?;
                    Some((ws_idx, thread_idx))
                })
        });
        Ok(ArrangedTarget {
            terminal,
            target,
            position,
        })
    }

    /// `surface.park`: take a session out of its pane and leave it running in
    /// the rail - what closing its pane by hand does.
    pub(crate) fn handle_park(
        &mut self,
        params: &serde_json::Value,
        lineage: &CallerLineage,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        let ArrangedTarget {
            terminal,
            target,
            position,
        } = match self.arranged_target(params, cx) {
            Ok(found) => found,
            Err(response) => return response,
        };
        let caller = self.caller_of(lineage, cx);
        if let Err(refusal) = may_arrange(&caller, target, LaunchGates::read()) {
            return refusal.into_value("surface.park");
        }
        let surface_id = terminal.entity_id().as_u64();
        let (Some(thread_id), Some((ws_idx, _))) = (target.thread_id, position) else {
            return JsonRpcError::invalid_params(
                "The target has no row in the rail to stay in; it cannot be parked",
            )
            .into_value();
        };
        // Already out of a pane is the state asked for.
        if self.agent_surface_in_slot(ws_idx, thread_id, cx) {
            self.remove_agent_from_slots(ws_idx, thread_id, cx);
            self.save_session(cx);
            cx.notify();
        }
        serde_json::json!({ "parked": true, "surface_id": surface_id })
    }

    /// `surface.close`: stop a session's process and drop its row - "Delete
    /// session" from the rail, and not "close the pane", which only parks.
    ///
    /// For an opener there is no other meaning of closing worth having: a
    /// session left running without a pane goes on spending, out of sight.
    pub(crate) fn handle_close(
        &mut self,
        params: &serde_json::Value,
        lineage: &CallerLineage,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        const METHOD: &str = "surface.close";
        let stop_turn = params
            .get("stop_turn")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        let ArrangedTarget {
            terminal,
            target,
            position,
        } = match self.arranged_target(params, cx) {
            Ok(found) => found,
            Err(response) => return response,
        };
        let (Some(thread_id), Some((ws_idx, thread_idx))) = (target.thread_id, position) else {
            return JsonRpcError::invalid_params("The target is not a session with a row to close")
                .into_value();
        };
        let turn_in_flight = self
            .rail_snapshot_for(&terminal, Some(thread_id), cx)
            .is_some_and(|rail| closing_loses_a_turn(&rail));
        let caller = self.caller_of(lineage, cx);
        let close_target = CloseTarget {
            thread_id,
            ws_idx,
            opened_by: target.opened_by,
            turn_in_flight,
            unseen_by_person: self
                .thread_by_id(thread_id)
                .is_some_and(|thread| thread.finished_unseen.is_some()),
        };
        if let Err(refusal) = may_close(&caller, close_target, LaunchGates::read(), stop_turn) {
            return refusal.into_value(METHOD);
        }
        let surface_id = terminal.entity_id().as_u64();
        // The one path that removes a session: it takes the surface out of
        // its pane, drops the view that owns the PTY, and with it the process.
        if self.remove_thread(ws_idx, thread_idx, cx).is_err() {
            return JsonRpcError::invalid_params("The session was already gone").into_value();
        }
        self.save_session(cx);
        cx.notify();
        serde_json::json!({ "closed": true, "surface_id": surface_id, "thread_id": thread_id })
    }

    /// `surface.interrupt`: stop the turn a session is in and leave the
    /// session running.
    ///
    /// Answers as soon as the key is written. Whether the turn stopped is
    /// read afterwards from the rail, by whoever asked: the agent's own file
    /// records the stop, and the state pass holds the end of a run for a few
    /// seconds before it believes it, which is longer than a handler on the
    /// thread that draws the window may take.
    pub(crate) fn handle_interrupt(
        &mut self,
        params: &serde_json::Value,
        lineage: &CallerLineage,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        const METHOD: &str = "surface.interrupt";
        let ArrangedTarget {
            terminal, target, ..
        } = match self.arranged_target(params, cx) {
            Ok(found) => found,
            Err(response) => return response,
        };
        let caller = self.caller_of(lineage, cx);
        let target = WriteTarget {
            drive: self.drive_between(&caller, target),
            ..target
        };
        if let Err(denied) = may_interrupt(&caller, target, LaunchGates::read().scripting) {
            return self.deny_write(denied, &caller, target, METHOD, cx);
        }
        let rail = self.rail_snapshot_for(&terminal, target.thread_id, cx);
        // The agent of the session's record. A terminal with no record has
        // only what a hook said about it, and a hook does not say which key
        // stops the agent it reports for.
        let key = target
            .thread_id
            .and_then(|id| self.thread_by_id(id))
            .and_then(|thread| thread.terminal_agent)
            .and_then(crate::agent_launcher::TerminalAgent::interrupt_key);
        let Some(key) = key else {
            return Refusal::NoInterrupt.into_value(METHOD);
        };
        let surface_id = terminal.entity_id().as_u64();
        let status = rail.as_ref().map(|rail| rail.status);
        let answer = |sent: bool| {
            serde_json::json!({
                "sent": sent,
                "surface_id": surface_id,
                "rail_status_at_send": status.map(crate::rail_state::status_word),
                // The line the stop is confirmed against: a run past this
                // one has ended.
                "runs_ended": rail.as_ref().map_or(0, |rail| rail.runs_ended),
                "last_outcome": rail
                    .as_ref()
                    .and_then(|rail| rail.last_outcome)
                    .map(crate::rail_state::RunOutcome::wire_str),
            })
        };
        // Nothing to stop, so nothing is written: the same key on an idle
        // input line is a keypress the agent reads as something else.
        if status != Some(crate::project::ThreadStatus::Thinking) {
            return answer(false);
        }
        if let Err(e) = crate::app::ipc_handler::pane_takes_input(&terminal, cx) {
            return e.into_value();
        }
        let refused_before = terminal.read(cx).terminal.refused_input_count();
        match terminal.read(cx).send_keystroke(key) {
            Ok(()) if terminal.read(cx).terminal.refused_input_count() != refused_before => {
                JsonRpcError::input_not_taken(
                    "The pane's input queue is full; the key was not sent",
                )
                .into_value()
            }
            Ok(()) => {
                if let Some(thread_id) = target.thread_id {
                    self.interrupts_asked
                        .insert(thread_id, std::time::Instant::now());
                }
                answer(true)
            }
            Err(e) => JsonRpcError::invalid_params(e).into_value(),
        }
    }

    /// Empty the input line of a session whose agent put its prompt back
    /// there when our key stopped the turn before the first token.
    ///
    /// Left alone, the next text the caller sends is appended to that prompt
    /// and the two are submitted as one. Called by the state pass at the end
    /// of the run, and only for a run a caller asked to stop: a person who
    /// pressed the key themselves gets their prompt back to edit, which is
    /// what the agent meant by returning it.
    pub(crate) fn clear_returned_prompt(&mut self, thread_id: u64, cx: &App) {
        let key = self
            .thread_by_id(thread_id)
            .and_then(|thread| thread.terminal_agent)
            .and_then(crate::agent_launcher::TerminalAgent::clear_input_key);
        let (Some(key), Some(terminal)) = (key, self.terminal_of(thread_id, cx)) else {
            return;
        };
        match terminal.read(cx).send_keystroke(key) {
            Ok(()) => log::info!(
                "surface record {thread_id}: its turn was stopped before the first token; \
                 the prompt its agent put back on the input line was cleared"
            ),
            Err(e) => {
                log::warn!("surface record {thread_id}: could not clear the returned prompt: {e}")
            }
        }
    }

    /// `surface.show`: put a session that is in no pane into one.
    pub(crate) fn handle_show(
        &mut self,
        params: &serde_json::Value,
        lineage: &CallerLineage,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        const METHOD: &str = "surface.show";
        let direction = match params.get("direction").map(|d| d.as_str()) {
            None | Some(Some("vertical")) => crate::layout::SplitDirection::Vertical,
            Some(Some("horizontal")) => crate::layout::SplitDirection::Horizontal,
            Some(_) => {
                return JsonRpcError::invalid_params(
                    "direction must be \"horizontal\" or \"vertical\"",
                )
                .into_value();
            }
        };
        let ArrangedTarget {
            terminal,
            target,
            position,
        } = match self.arranged_target(params, cx) {
            Ok(found) => found,
            Err(response) => return response,
        };
        let caller = self.caller_of(lineage, cx);
        if let Err(refusal) = may_arrange(&caller, target, LaunchGates::read()) {
            return refusal.into_value(METHOD);
        }
        let surface_id = terminal.entity_id().as_u64();
        let shown = |displaced: Option<u64>| {
            serde_json::json!({
                "shown": true,
                "surface_id": surface_id,
                "displaced_surface_id": displaced,
            })
        };
        // A terminal with no record lives in a pane and nowhere else.
        let (Some(thread_id), Some((ws_idx, thread_idx))) = (target.thread_id, position) else {
            return shown(None);
        };
        if self.agent_surface_in_slot(ws_idx, thread_id, cx) {
            return shown(None);
        }
        let leaves = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
            .map(|root| root.collect_leaves())
            .unwrap_or_default();
        let beside = match crate::app::ipc_handler::optional_id_param(params, "beside_surface_id") {
            Ok(None) => None,
            Ok(Some(beside)) => {
                let Some(pane) = leaves.iter().find(|pane| {
                    pane.read(cx)
                        .terminals()
                        .any(|held| held.entity_id().as_u64() == beside)
                }) else {
                    return JsonRpcError::invalid_params(
                        "beside_surface_id is not in a pane of the target's project",
                    )
                    .into_value();
                };
                Some(pane.clone())
            }
            Err(e) => return e.into_value(),
        };
        let room = self.refuse_another_pane_in(ws_idx, cx);
        let empty_pane = leaves
            .iter()
            .find(|pane| pane.read(cx).tabs.is_empty())
            .cloned();
        // Where it goes, decided before the view is touched. A named
        // neighbour is honoured when a pane can be added next to it; when
        // none can, the request is still for a pane and the ladder answers.
        enum Spot {
            Beside(Entity<crate::pane::Pane>),
            Empty(Entity<crate::pane::Pane>),
            New,
            InPlaceOf(Entity<crate::pane::Pane>, u64),
        }
        let can_add = matches!(room, None | Some(PaneRefusal::NoPanes));
        let spot = match (beside, empty_pane) {
            (Some(pane), _) if can_add => Spot::Beside(pane),
            (_, Some(pane)) => Spot::Empty(pane),
            _ if can_add => Spot::New,
            _ => {
                let opener = match &caller {
                    Caller::Pane(pane) => pane.thread_id,
                    // A script outside a pane opened nothing it could give up.
                    Caller::Outside => return Refusal::NoRoom.into_value(METHOD),
                };
                let facts: Vec<PaneFacts> = leaves
                    .iter()
                    .map(|pane| {
                        let shown_thread = pane
                            .read(cx)
                            .surface()
                            .and_then(|tab| tab.as_terminal().cloned())
                            .and_then(|view| view.read(cx).agent_thread_id);
                        PaneFacts {
                            thread_id: shown_thread,
                            opened_by: shown_thread
                                .and_then(|id| self.thread_by_id(id))
                                .and_then(|thread| thread.opened_by.as_ref())
                                .map(|by| by.id),
                            focused: self
                                .pane_focus_order
                                .get(&pane.entity_id())
                                .copied()
                                .unwrap_or(0),
                        }
                    })
                    .collect();
                let Some(idx) = pane_to_give_up(&facts, opener, thread_id) else {
                    return Refusal::NoRoom.into_value(METHOD);
                };
                let pane = leaves[idx].clone();
                let displaced = pane
                    .read(cx)
                    .surface()
                    .and_then(|tab| tab.as_terminal().map(|view| view.entity_id().as_u64()))
                    .unwrap_or(0);
                Spot::InPlaceOf(pane, displaced)
            }
        };
        let agents_target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
        let Some(view) = self.mount_agents_terminal_for_target(agents_target, cx) else {
            return JsonRpcError::invalid_params("The session could not be shown").into_value();
        };
        // As with opening: the keyboard stays where the person left it.
        let displaced = match spot {
            Spot::Empty(pane) => {
                pane.update(cx, |pane, cx| pane.show_terminal(view.clone(), cx));
                None
            }
            Spot::InPlaceOf(pane, displaced) => {
                // The session it replaces is an agent's, owned by the surface
                // cache: it goes on running in the rail.
                pane.update(cx, |pane, cx| pane.show_terminal(view.clone(), cx));
                Some(displaced)
            }
            Spot::New => {
                if self
                    .append_pane_with(ws_idx, TabContent::Terminal(view.clone()), cx)
                    .is_none()
                {
                    return Refusal::NoRoom.into_value(METHOD);
                }
                None
            }
            Spot::Beside(neighbour) => {
                let pane =
                    self.create_pane_with_existing_tab(TabContent::Terminal(view.clone()), cx);
                let inserted = self
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|container| container.root.as_mut())
                    .is_some_and(|root| root.split_at_pane(&neighbour, direction, pane));
                if !inserted {
                    return Refusal::NoRoom.into_value(METHOD);
                }
                None
            }
        };
        view.update(cx, |view, cx| view.ensure_backend_started(cx));
        self.save_session(cx);
        cx.notify();
        shown(displaced)
    }

    /// The terminal holding the session `thread_id`, on screen or not.
    pub(crate) fn terminal_of(&self, thread_id: u64, cx: &App) -> Option<Entity<TerminalView>> {
        self.held_terminals(cx)
            .into_iter()
            .find(|(_, held)| *held == Some(thread_id))
            .map(|(terminal, _)| terminal)
    }

    /// Note that the opener of `thread_id` has just written text into it.
    pub(crate) fn note_opener_wrote(&mut self, thread_id: u64, cx: &App) {
        let submits = self
            .terminal_of(thread_id, cx)
            .map(|terminal| terminal.read(cx).keyboard_submits);
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.opener_wrote_at = submits;
        }
    }

    /// Whether the last message `thread_id` was given came from the session
    /// that opened it.
    ///
    /// A person's message is Enter pressed on the keyboard in that session's
    /// terminal, counted by the terminal itself. Any key would be too wide a
    /// test: an arrow pressed by accident in the pane would turn the opener's
    /// run into the person's news. An opener that has been closed has nobody
    /// left to read the result, so the news goes to the person.
    pub(crate) fn last_message_was_openers(&self, thread_id: u64, cx: &App) -> bool {
        let Some(thread) = self.thread_by_id(thread_id) else {
            return false;
        };
        // The session that hands this one its tasks: the one that opened
        // it, or the one a person let drive it.
        let Some(opener) = thread
            .opened_by
            .as_ref()
            .map(|by| by.id)
            .or_else(|| self.drive.driver_of(thread_id))
        else {
            return false;
        };
        if self.thread_by_id(opener).is_none() {
            return false;
        }
        let Some(wrote_at) = thread.opener_wrote_at else {
            return false;
        };
        self.terminal_of(thread_id, cx)
            .is_some_and(|terminal| terminal.read(cx).keyboard_submits == wrote_at)
    }

    /// How many of the sessions `opener` opened still have a process.
    ///
    /// A session whose agent has exited, and one restored from a file that
    /// nobody has started yet, hold no place: the limit is on what is running.
    fn running_sessions_opened_by(&self, opener: u64, cx: &App) -> usize {
        let running: std::collections::HashSet<u64> = self
            .held_terminals(cx)
            .into_iter()
            .filter(|(terminal, _)| terminal.read(cx).terminal.exited.is_none())
            .filter_map(|(_, thread_id)| thread_id)
            .collect();
        self.workspaces
            .iter()
            .flat_map(|container| container.threads.iter())
            .filter(|thread| {
                thread.opened_by.as_ref().is_some_and(|by| by.id == opener)
                    && !thread.rail.agent_exited
                    && running.contains(&thread.id)
            })
            .count()
    }

    /// `surface.add_agent`: open an agent session the way the launcher does,
    /// for a caller that is another session or a person's script.
    pub(crate) fn handle_add_agent(
        &mut self,
        params: &serde_json::Value,
        lineage: &CallerLineage,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        const METHOD: &str = "surface.add_agent";
        let agent_name = params.get("agent").and_then(|a| a.as_str()).unwrap_or("");
        let Some(agent) = crate::agent_launcher::TerminalAgent::from_tag(agent_name)
            .or_else(|| crate::agent_launcher::TerminalAgent::from_binary(agent_name))
        else {
            let known: Vec<&str> = crate::agent_launcher::TerminalAgent::ALL
                .iter()
                .map(|a| a.tag())
                .collect();
            return JsonRpcError::invalid_params(format!(
                "Missing or unknown 'agent'; one of: {}",
                known.join(", ")
            ))
            .into_value();
        };
        let ask = match PlacementAsk::parse(params.get("placement")) {
            Ok(ask) => ask,
            Err(e) => return e.into_value(),
        };
        let name = match params.get("name") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => match value
                .as_str()
                .and_then(crate::app::ipc_handler::sanitize_pane_name)
            {
                Some(name) => Some(name),
                None => {
                    return JsonRpcError::invalid_params("'name' is empty or not a usable name")
                        .into_value();
                }
            },
        };
        let prompt = params
            .get("prompt")
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty());
        const MAX_PROMPT_LEN: usize = 64 * 1024;
        if prompt.is_some_and(|p| p.len() > MAX_PROMPT_LEN) {
            return JsonRpcError::invalid_params("Prompt exceeds 64 KiB limit").into_value();
        }
        let submit = params
            .get("submit")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        if submit && prompt.is_none() {
            return JsonRpcError::invalid_params("'submit' needs a 'prompt' to submit")
                .into_value();
        }

        let caller = self.caller_of(lineage, cx);
        let already_open = match &caller {
            Caller::Pane(pane) => self.running_sessions_opened_by(pane.thread_id, cx),
            Caller::Outside => 0,
        };
        if let Err(refusal) = may_open(&caller, already_open, LaunchGates::read(), submit) {
            return refusal.into_value(METHOD);
        }
        if !agent.is_installed() {
            return JsonRpcError::invalid_params(format!(
                "{} is not installed",
                agent.display_name()
            ))
            .into_value();
        }
        // The caller's own project. A script outside a pane has none, and
        // gets the one on screen, like every other call without a target.
        let ws_idx = match &caller {
            Caller::Pane(pane) => pane.ws_idx,
            Caller::Outside => self.active_idx,
        };
        let Some(container) = self.workspaces.get(ws_idx) else {
            return JsonRpcError::invalid_params("No project to open the session in").into_value();
        };
        let empty_pane = container.root.as_ref().and_then(|root| {
            root.collect_leaves()
                .into_iter()
                .find(|pane| pane.read(cx).tabs.is_empty())
        });
        // Decided before anything is created, so a refusal leaves nothing.
        let placement = match place_opened(
            ask,
            empty_pane.is_some(),
            self.refuse_another_pane_in(ws_idx, cx),
        ) {
            Ok(placement) => placement,
            Err(refusal) => return refusal.into_value(METHOD),
        };

        let title = name
            .clone()
            .unwrap_or_else(|| agent.display_name().to_string());
        let thread_id = match self.add_terminal_thread(ws_idx, title, Some(agent), cx) {
            Ok(id) => id,
            Err(err) => {
                return JsonRpcError::invalid_params(format!("Could not open a session: {err:?}"))
                    .into_value();
            }
        };
        let opened_by = match &caller {
            Caller::Pane(pane) => Some(splitlane_config::schema::OpenedBy {
                id: pane.thread_id,
                title: pane.title.clone(),
            }),
            Caller::Outside => None,
        };
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.opened_by = opened_by;
            // A name the opener chose is a name somebody chose: the agent's
            // own title must not replace it, or the opener could no longer
            // find the session by the name it gave.
            thread.title_user_set = name.is_some();
        }
        let Some(thread_idx) = self.thread_index_by_id(ws_idx, thread_id) else {
            return JsonRpcError::invalid_params("The session vanished while opening").into_value();
        };
        let from_opener = matches!(caller, Caller::Pane(_));
        let opening_text = prompt.map(|prompt| match &caller {
            Caller::Pane(pane) => format!("{}\n\n{prompt}", provenance_line(&pane.title)),
            Caller::Outside => prompt.to_string(),
        });
        // Left for the agent's shim before the launch command is typed, so
        // the file is there by the time the shell has started and run it.
        let at_launch = opening_text
            .as_deref()
            .filter(|text| prompt_goes_at_launch(agent, submit, text))
            .map(|text| {
                let key = uuid::Uuid::new_v4().simple().to_string();
                let (file, text) = (key.clone(), text.to_string());
                let stashed = cx
                    .background_executor()
                    .spawn(async move { stash_opening_prompt(&file, &text) });
                (key, stashed)
            });
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.opening_prompt_key = at_launch.as_ref().map(|(key, _)| key.clone());
        }
        let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
        let mounted = self.mount_agents_terminal_for_target(target, cx);
        // The key was for that one launch: a later reopen resumes the
        // conversation and must not ask for a prompt that is gone.
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.opening_prompt_key = None;
        }
        let Some(view) = mounted else {
            if let Some((_, stashed)) = at_launch {
                cx.background_executor()
                    .spawn(async move {
                        if let Some(path) = stashed.await {
                            let _ = std::fs::remove_file(path);
                        }
                    })
                    .detach();
            }
            // An error must not leave a row behind that the caller was told
            // does not exist.
            let _ = self.remove_thread(ws_idx, thread_idx, cx);
            return JsonRpcError::invalid_params("The session could not be started").into_value();
        };
        // Neither arm moves the keyboard or changes which project is on
        // screen: the person is typing somewhere, and a session opening is
        // not a reason to take that away.
        let placement = match placement {
            Placement::EmptyPane => match empty_pane {
                Some(pane) => {
                    pane.update(cx, |pane, cx| pane.show_terminal(view.clone(), cx));
                    placement
                }
                None => Placement::Parked(ParkedBecause::Ceiling),
            },
            Placement::NewPane => {
                match self.append_pane_with(ws_idx, TabContent::Terminal(view.clone()), cx) {
                    Some(_) => placement,
                    // The tree did not take another leaf after all.
                    None => Placement::Parked(ParkedBecause::Ceiling),
                }
            }
            Placement::Parked(_) => placement,
        };
        // A pane starts its process when it is first drawn. A session in the
        // rail is never drawn, and one in a project that is not on screen is
        // not drawn yet, so it is started here.
        view.update(cx, |view, cx| view.ensure_backend_started(cx));

        if let Some(text) = opening_text {
            match at_launch {
                Some((_, stashed)) => {
                    self.await_prompt_collected(thread_id, &view, stashed, text, from_opener, cx);
                }
                None => {
                    self.schedule_opening_prompt(thread_id, &view, text, submit, from_opener, cx);
                }
            }
        }
        self.save_session(cx);
        cx.notify();

        let rail = self.rail_snapshot_for(&view, Some(thread_id), cx);
        let (placement_word, placement_reason) = match placement {
            Placement::EmptyPane | Placement::NewPane => ("pane", None),
            Placement::Parked(because) => ("parked", Some(because.word())),
        };
        serde_json::json!({
            "surface_id": view.entity_id().as_u64(),
            "thread_id": thread_id,
            "agent": agent.binary(),
            "tier": rail.as_ref().map(|rail| rail.source.tier()),
            "session_id": self.thread_by_id(thread_id).and_then(|t| t.session_id.clone()),
            // The line a wait is measured from: a run past this one has ended.
            "runs_ended": rail.as_ref().map_or(0, |rail| rail.runs_ended),
            "placement": placement_word,
            "placement_reason": placement_reason,
            "opened_by": match &caller {
                Caller::Pane(pane) => Some(pane.surface_id),
                Caller::Outside => None,
            },
            // `pending` when a prompt was given: it is written once the agent
            // is there to read it, and `surface.status` says what became of it.
            "opening_prompt": rail
                .as_ref()
                .and_then(|rail| rail.opening_prompt)
                .map(crate::rail_state::OpeningPrompt::wire_str),
        })
    }

    /// Follow a first prompt that went into the launch command
    /// ([`prompt_goes_at_launch`]) until the agent's shim has collected it.
    ///
    /// The shim removes the file when it has read it, which is the one thing
    /// here that says the agent was started with the text. A file that could
    /// not be left at all falls back to writing the prompt into the input
    /// line: the shim drops a key it finds nothing behind, so the agent is
    /// starting without a prompt either way.
    fn await_prompt_collected(
        &mut self,
        thread_id: u64,
        terminal: &Entity<TerminalView>,
        stashed: gpui::Task<Option<std::path::PathBuf>>,
        text: String,
        from_opener: bool,
        cx: &mut Context<Self>,
    ) {
        use crate::rail_state::{NotWritten, OpeningPrompt};

        self.note_opening_prompt(thread_id, OpeningPrompt::Pending);
        let weak = terminal.downgrade();
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let Some(path) = stashed.await else {
                    log::warn!(
                        "opening prompt: no place to leave it for surface record \
                         {thread_id}; writing it into the input line instead"
                    );
                    let _ = cx.update(|cx| {
                        let terminal = weak.upgrade()?;
                        this.update(cx, |app, cx| {
                            app.schedule_opening_prompt(
                                thread_id,
                                &terminal,
                                text,
                                true,
                                from_opener,
                                cx,
                            );
                        })
                        .ok()
                    });
                    return;
                };
                let started = std::time::Instant::now();
                let mut collected = false;
                let became = loop {
                    if !collected {
                        let file = path.clone();
                        collected = !cx
                            .background_executor()
                            .spawn(async move { file.exists() })
                            .await;
                    }
                    // `None` once the session is gone: closed, or no longer
                    // drawn anywhere.
                    let seen = cx.update(|cx| {
                        let view = weak.upgrade()?;
                        this.update(cx, |app, cx| app.opening_look(thread_id, &view, cx))
                            .ok()
                            .flatten()
                    });
                    // The shim removes the file before it starts the agent,
                    // and the agent may not start: a session id in use, a
                    // binary that fails. So the receipt counts once the
                    // agent is seen as well.
                    if collected && seen.is_none_or(|look| look.agent_found || look.spoken_for) {
                        break OpeningPrompt::Submitted;
                    }
                    if seen.is_none() || started.elapsed() >= OPENING_PROMPT_AGENT_MAX {
                        let file = path.clone();
                        let _ = cx
                            .background_executor()
                            .spawn(async move { std::fs::remove_file(file) })
                            .await;
                        break OpeningPrompt::NotWritten(NotWritten::NoAgent);
                    }
                    smol::Timer::after(OPENING_PROMPT_POLL).await;
                };
                if became != OpeningPrompt::Submitted {
                    log::warn!(
                        "opening prompt: nothing collected it for surface record \
                         {thread_id} within {:?}",
                        started.elapsed()
                    );
                }
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        if from_opener && became == OpeningPrompt::Submitted {
                            app.note_opener_wrote(thread_id, cx);
                        }
                        app.note_opening_prompt(thread_id, became);
                    })
                });
            },
        )
        .detach();
    }

    /// Write a new session's first prompt once its agent is there to read it.
    ///
    /// The launch command is typed into the pane's shell, so for the first
    /// moments the thing reading input is the shell and not the agent, and
    /// after that the agent may be asking something of its own before it
    /// takes a prompt. [`opening_prompt_step`] says when the prompt may go,
    /// and then the screen is given time to settle.
    ///
    /// What became of the prompt is kept on the session's record
    /// ([`crate::rail_state::OpeningPrompt`]): the call that opened the
    /// session has answered long before this ends, and a prompt that was not
    /// written used to leave nothing but a line in the log.
    /// It goes through the same write as `surface.send_text`: a paste, and
    /// when it is to be submitted, a carriage return of its own afterwards.
    fn schedule_opening_prompt(
        &mut self,
        thread_id: u64,
        terminal: &Entity<TerminalView>,
        text: String,
        submit: bool,
        from_opener: bool,
        cx: &mut Context<Self>,
    ) {
        use crate::rail_state::{NotWritten, OpeningPrompt};

        self.note_opening_prompt(thread_id, OpeningPrompt::Pending);
        let weak = terminal.downgrade();
        let submit_floor =
            Duration::from_millis(self.cached_config.resolved_submit_paste_delay_ms());
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let started = std::time::Instant::now();
                let look = |cx: &mut gpui::AsyncApp| -> Option<OpeningLook> {
                    cx.update(|cx| {
                        let view = weak.upgrade()?;
                        this.update(cx, |app, cx| app.opening_look(thread_id, &view, cx))
                            .ok()
                            .flatten()
                    })
                };
                let give_up = |because: NotWritten, cx: &mut gpui::AsyncApp| {
                    log::warn!(
                        "opening prompt: not written to surface record {thread_id} \
                         ({because:?}) after {:?}",
                        started.elapsed()
                    );
                    let _ = cx.update(|cx| {
                        this.update(cx, |app, _| {
                            app.note_opening_prompt(thread_id, OpeningPrompt::NotWritten(because));
                        })
                    });
                };
                loop {
                    // The session was closed, or the app is going away.
                    let seen = look(cx)?;
                    match opening_prompt_step(seen, started.elapsed()) {
                        OpeningStep::Write => {}
                        OpeningStep::Wait => {
                            smol::Timer::after(OPENING_PROMPT_POLL).await;
                            continue;
                        }
                        OpeningStep::GiveUp(because) => {
                            give_up(because, cx);
                            return None;
                        }
                    }
                    Self::wait_for_terminal_settle(
                        &weak,
                        OPENING_PROMPT_SETTLE_FLOOR,
                        OPENING_PROMPT_SETTLE_MAX,
                        OPENING_PROMPT_POLL,
                        cx,
                    )
                    .await?;
                    // What the agent shows may have changed while it settled:
                    // a question can come up after the first screen.
                    match opening_prompt_step(look(cx)?, started.elapsed()) {
                        OpeningStep::Write => break,
                        OpeningStep::Wait => {}
                        OpeningStep::GiveUp(because) => {
                            give_up(because, cx);
                            return None;
                        }
                    }
                }
                let written = cx.update(|cx| {
                    let Some(terminal) = weak.upgrade() else {
                        return false;
                    };
                    let view = terminal.read(cx);
                    // Outside a paste every newline is an Enter, and the text
                    // would be submitted in pieces. Not writing it is the
                    // smaller harm: the session is there and can be sent to.
                    if (text.contains('\n') || text.contains('\r'))
                        && !view.bracketed_paste_enabled()
                    {
                        return false;
                    }
                    view.inject_text(&text);
                    true
                });
                if !written {
                    give_up(NotWritten::NoPaste, cx);
                    return None;
                }
                let _ = cx.update(|cx| {
                    this.update(cx, |app, cx| {
                        if from_opener {
                            app.note_opener_wrote(thread_id, cx);
                        }
                        let mut became = OpeningPrompt::Written;
                        if submit && let Some(terminal) = weak.upgrade() {
                            Self::schedule_deferred_submit(&terminal, submit_floor, cx);
                            became = OpeningPrompt::Submitted;
                        }
                        app.note_opening_prompt(thread_id, became);
                    })
                });
                Some(())
            },
        )
        .detach();
    }

    fn note_opening_prompt(&mut self, thread_id: u64, became: crate::rail_state::OpeningPrompt) {
        if let Some(thread) = self.thread_by_id_mut(thread_id) {
            thread.rail.opening_prompt = Some(became);
        }
    }

    /// Whether text for `thread_id` is typed and not pasted, as things stand
    /// in its pane now ([`typed_text_is_safe`]).
    pub(crate) fn types_into(&self, thread_id: u64, view: &Entity<TerminalView>, cx: &App) -> bool {
        self.thread_by_id(thread_id).is_some_and(|thread| {
            !thread.rail.agent_exited
                && thread
                    .terminal_agent
                    .is_some_and(crate::agent_launcher::TerminalAgent::takes_typed_text)
        }) && self
            .opening_look(thread_id, view, cx)
            .is_some_and(typed_text_is_safe)
    }

    /// One look at a session an opening prompt is waiting to be written to.
    /// `None` when the session is gone.
    pub(crate) fn opening_look(
        &self,
        thread_id: u64,
        view: &Entity<TerminalView>,
        cx: &App,
    ) -> Option<OpeningLook> {
        let thread = self.thread_by_id(thread_id)?;
        Some(OpeningLook {
            spoken_for: thread.detector_read_at.is_some() || thread.hook_has_spoken,
            agent_found: self
                .pty_flow
                .get(&thread_id)
                .is_some_and(|flow| flow.agent_found),
            cursor_marks_text_entry: thread
                .terminal_agent
                .is_some_and(crate::agent_launcher::TerminalAgent::cursor_marks_text_entry),
            entry: crate::agent_state::TextEntry::from_modes(
                view.read(cx).terminal.session_backend().modes(),
            ),
            waiting: thread.status == crate::project::ThreadStatus::WaitingForInput,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(opened: bool) -> Caller {
        Caller::Pane(PaneCaller {
            surface_id: 7,
            thread_id: 3,
            ws_idx: 0,
            title: "lead".to_string(),
            opened,
        })
    }

    const NO_GATES: LaunchGates = LaunchGates {
        orchestration: false,
        scripting: false,
    };

    /// The point of the whole arrangement: nothing has to be switched on.
    #[test]
    fn a_session_in_a_pane_opens_sessions_with_nothing_switched_on() {
        assert_eq!(may_open(&pane(false), 0, NO_GATES, false), Ok(()));
        assert_eq!(may_open(&pane(false), 0, NO_GATES, true), Ok(()));
        assert_eq!(
            may_open(&pane(false), MAX_OPENED_SESSIONS - 1, NO_GATES, true),
            Ok(())
        );
    }

    #[test]
    fn the_ninth_session_is_refused() {
        assert_eq!(
            may_open(&pane(false), MAX_OPENED_SESSIONS, NO_GATES, false),
            Err(Refusal::WorkerCeiling)
        );
    }

    /// Depth one. The variables do not lift it: they are for a caller outside
    /// a pane, and this one is in one.
    #[test]
    fn a_session_that_was_opened_opens_none() {
        let all = LaunchGates {
            orchestration: true,
            scripting: true,
        };
        assert_eq!(
            may_open(&pane(true), 0, NO_GATES, false),
            Err(Refusal::OpenedSessionCannotOpen)
        );
        assert_eq!(
            may_open(&pane(true), 0, all, false),
            Err(Refusal::OpenedSessionCannotOpen)
        );
    }

    fn look() -> OpeningLook {
        OpeningLook {
            spoken_for: false,
            agent_found: false,
            cursor_marks_text_entry: true,
            entry: crate::agent_state::TextEntry::NoInterface,
            waiting: false,
        }
    }

    /// Codex before its first prompt: no file, no hook frame, and an input
    /// line. The prompt used to wait for one of the first two and was never
    /// written.
    #[test]
    fn an_opening_prompt_goes_to_an_agent_showing_an_input_line() {
        let at_its_input_line = OpeningLook {
            agent_found: true,
            entry: crate::agent_state::TextEntry::Open,
            ..look()
        };
        assert_eq!(
            opening_prompt_step(at_its_input_line, Duration::from_secs(2)),
            OpeningStep::Write
        );
    }

    /// Claude Code at its input line, as before: its own file speaks for it,
    /// and nothing is asked of the cursor.
    #[test]
    fn an_opening_prompt_goes_to_an_agent_its_own_file_speaks_for() {
        let spoken_for = OpeningLook {
            spoken_for: true,
            ..look()
        };
        assert_eq!(
            opening_prompt_step(spoken_for, Duration::from_secs(2)),
            OpeningStep::Write
        );
    }

    /// The question is the person's. Nothing is written, at once, and even
    /// where the agent's own file speaks.
    #[test]
    fn an_opening_prompt_is_not_written_into_a_question() {
        use crate::rail_state::NotWritten;
        let asking = OpeningLook {
            spoken_for: true,
            agent_found: true,
            entry: crate::agent_state::TextEntry::Closed,
            waiting: true,
            ..look()
        };
        assert_eq!(
            opening_prompt_step(asking, Duration::ZERO),
            OpeningStep::GiveUp(NotWritten::Waiting)
        );
    }

    /// A list to choose from that has not stood long enough to be called a
    /// question is waited on, not written into.
    #[test]
    fn an_opening_prompt_waits_while_there_is_no_place_to_type() {
        let no_place = OpeningLook {
            agent_found: true,
            entry: crate::agent_state::TextEntry::Closed,
            ..look()
        };
        assert_eq!(
            opening_prompt_step(no_place, Duration::from_secs(2)),
            OpeningStep::Wait
        );
    }

    /// What is left in a pane when its agent has gone is a shell, and at a
    /// shell a typed line feed runs the line.
    #[test]
    fn text_is_typed_only_in_front_of_a_live_agent() {
        use crate::agent_state::TextEntry;

        let at_agent = OpeningLook {
            spoken_for: true,
            agent_found: true,
            cursor_marks_text_entry: true,
            entry: TextEntry::Open,
            waiting: false,
        };
        assert!(typed_text_is_safe(at_agent));
        // Mid-turn the cursor is hidden and typed text is queued.
        assert!(typed_text_is_safe(OpeningLook {
            entry: TextEntry::Closed,
            ..at_agent
        }));
        // The agent exited: a shell, which takes pastes and shows a cursor.
        assert!(!typed_text_is_safe(OpeningLook {
            agent_found: false,
            ..at_agent
        }));
        assert!(!typed_text_is_safe(OpeningLook {
            entry: TextEntry::NoInterface,
            ..at_agent
        }));
    }

    #[test]
    fn a_first_prompt_goes_at_launch_only_where_that_was_measured() {
        use crate::agent_launcher::TerminalAgent;

        let claude = TerminalAgent::ClaudeCode;
        // An argument cannot be left unsubmitted on the input line.
        assert!(!prompt_goes_at_launch(claude, false, "fix it"));
        // The agent would take it for an option of its own.
        assert!(!prompt_goes_at_launch(claude, true, "--help me"));
        assert!(!prompt_goes_at_launch(claude, true, "a\0b"));
        assert_eq!(
            prompt_goes_at_launch(claude, true, "fix it\n\nand test it"),
            !cfg!(windows)
        );
        for agent in TerminalAgent::ALL {
            if !agent.takes_prompt_at_launch() {
                assert!(!prompt_goes_at_launch(agent, true, "fix it"));
            }
        }
    }

    /// A shell prompt takes pastes and shows a cursor too. Without the
    /// agent's process under the pane the text would be run as a command.
    #[test]
    fn an_opening_prompt_is_not_written_to_a_shell() {
        use crate::rail_state::NotWritten;
        let shell = OpeningLook {
            entry: crate::agent_state::TextEntry::Open,
            ..look()
        };
        assert_eq!(
            opening_prompt_step(shell, Duration::from_secs(2)),
            OpeningStep::Wait
        );
        assert_eq!(
            opening_prompt_step(shell, OPENING_PROMPT_AGENT_MAX),
            OpeningStep::GiveUp(NotWritten::NoAgent)
        );
    }

    /// An agent whose cursor nobody measured is not written to on the
    /// cursor's word: it may draw one of its own and hide the terminal's.
    #[test]
    fn an_unmeasured_agent_still_needs_something_to_speak_for_it() {
        let unmeasured = OpeningLook {
            agent_found: true,
            cursor_marks_text_entry: false,
            entry: crate::agent_state::TextEntry::Open,
            ..look()
        };
        assert_eq!(
            opening_prompt_step(unmeasured, Duration::from_secs(2)),
            OpeningStep::Wait
        );
    }

    #[test]
    fn a_caller_outside_a_pane_is_let_in_by_the_launch_variables_only() {
        let orchestration = LaunchGates {
            orchestration: true,
            scripting: false,
        };
        let scripting = LaunchGates {
            orchestration: true,
            scripting: true,
        };
        assert_eq!(
            may_open(&Caller::Outside, 0, NO_GATES, false),
            Err(Refusal::NotFromAPane)
        );
        assert_eq!(may_open(&Caller::Outside, 0, orchestration, false), Ok(()));
        // Submitting is typing Enter into a pane, which the narrower variable
        // has never opened.
        assert_eq!(
            may_open(&Caller::Outside, 0, orchestration, true),
            Err(Refusal::NotFromAPane)
        );
        assert_eq!(may_open(&Caller::Outside, 0, scripting, true), Ok(()));
    }

    const MINE: WriteTarget = WriteTarget {
        thread_id: Some(9),
        opened_by: Some(3),
        waiting: false,
        drive: Drive::NotOffered,
    };
    const A_PERSONS: WriteTarget = WriteTarget {
        thread_id: Some(10),
        opened_by: None,
        waiting: false,
        drive: Drive::NotOffered,
    };

    #[test]
    fn a_session_writes_into_what_it_opened_with_nothing_switched_on() {
        assert_eq!(may_write(&pane(false), MINE, false), Ok(WriteLeave::Opener));
        // The variable does not change the ground: the text is still marked.
        assert_eq!(may_write(&pane(false), MINE, true), Ok(WriteLeave::Opener));
    }

    #[test]
    fn a_session_does_not_write_into_one_it_did_not_open() {
        assert_eq!(
            may_write(&pane(false), A_PERSONS, false),
            Err(WriteDenied::Refused(Refusal::NotYours))
        );
        // Opened by some other session.
        let anothers = WriteTarget {
            opened_by: Some(77),
            ..MINE
        };
        assert_eq!(
            may_write(&pane(false), anothers, false),
            Err(WriteDenied::Refused(Refusal::NotYours))
        );
        // A session that was opened does not write upwards to its opener.
        let opener = WriteTarget {
            thread_id: Some(1),
            opened_by: None,
            waiting: false,
            drive: Drive::NotOffered,
        };
        assert_eq!(
            may_write(&pane(true), opener, false),
            Err(WriteDenied::Refused(Refusal::NotYours))
        );
    }

    /// The question on a waiting session's screen was put to a person.
    #[test]
    fn nobody_in_a_pane_types_into_a_session_that_waits_for_a_person() {
        let mine_waiting = WriteTarget {
            waiting: true,
            ..MINE
        };
        let persons_waiting = WriteTarget {
            waiting: true,
            ..A_PERSONS
        };
        for gate_open in [false, true] {
            assert_eq!(
                may_write(&pane(false), mine_waiting, gate_open),
                Err(WriteDenied::Refused(Refusal::Waiting))
            );
            assert_eq!(
                may_write(&pane(false), persons_waiting, gate_open),
                Err(WriteDenied::Refused(Refusal::Waiting))
            );
        }
    }

    /// What the launch variable opened before, it opens still.
    #[test]
    fn the_launch_variable_opens_what_it_always_opened() {
        assert_eq!(
            may_write(&Caller::Outside, A_PERSONS, true),
            Ok(WriteLeave::LaunchVariable)
        );
        assert_eq!(
            may_write(&Caller::Outside, A_PERSONS, false),
            Err(WriteDenied::GateClosed)
        );
        // A person's script outside a pane is not held to the waiting rule:
        // it is the person's own hand, by the person's own choice at launch.
        let waiting = WriteTarget {
            waiting: true,
            ..A_PERSONS
        };
        assert_eq!(
            may_write(&Caller::Outside, waiting, true),
            Ok(WriteLeave::LaunchVariable)
        );
        assert_eq!(
            may_write(&pane(false), A_PERSONS, true),
            Ok(WriteLeave::LaunchVariable)
        );
    }

    #[test]
    fn a_session_typing_into_itself_is_named_as_that() {
        let itself = WriteTarget {
            thread_id: Some(3),
            opened_by: None,
            waiting: false,
            drive: Drive::NotOffered,
        };
        assert_eq!(
            may_write(&pane(false), itself, false),
            Err(WriteDenied::Refused(Refusal::Itself))
        );
        assert_eq!(
            may_write(&pane(false), itself, true),
            Ok(WriteLeave::LaunchVariable)
        );
    }

    fn persons(drive: Drive) -> WriteTarget {
        WriteTarget { drive, ..A_PERSONS }
    }

    /// Into a session a person opened, a session writes once the person has
    /// said it may; until they have, the write asks and does nothing.
    #[test]
    fn a_persons_session_is_written_into_only_with_their_leave() {
        for not_answered in [Drive::NotAsked, Drive::Standing(Standing::Asked)] {
            assert_eq!(
                may_write(&pane(false), persons(not_answered), false),
                Err(WriteDenied::AskPerson)
            );
        }
        assert_eq!(
            may_write(
                &pane(false),
                persons(Drive::Standing(Standing::Allowed)),
                false
            ),
            Ok(WriteLeave::Driver)
        );
        assert_eq!(
            may_write(
                &pane(false),
                persons(Drive::Standing(Standing::Declined)),
                false
            ),
            Err(WriteDenied::Refused(Refusal::PersonDeclined))
        );
        // A pair nobody can be asked about is what it always was.
        assert_eq!(
            may_write(&pane(false), persons(Drive::NotOffered), false),
            Err(WriteDenied::Refused(Refusal::NotYours))
        );
    }

    /// A leave does not reach past the question on the session's own screen,
    /// and a person is not asked about a session that is asking them
    /// something itself.
    #[test]
    fn a_leave_does_not_answer_the_sessions_own_question() {
        for drive in [Drive::NotAsked, Drive::Standing(Standing::Allowed)] {
            let waiting = WriteTarget {
                waiting: true,
                ..persons(drive)
            };
            assert_eq!(
                may_write(&pane(false), waiting, false),
                Err(WriteDenied::Refused(Refusal::Waiting))
            );
            assert_eq!(
                may_interrupt(&pane(false), waiting, false),
                Err(WriteDenied::Refused(Refusal::Waiting))
            );
        }
    }

    /// The launch variable is the person's own choice, made before any
    /// question could be put: with it set nobody is asked, and a no given
    /// earlier in the run does not stand against it. A leave still marks the
    /// text as sent by a session.
    #[test]
    fn the_launch_variable_asks_nobody() {
        for drive in [
            Drive::NotAsked,
            Drive::Standing(Standing::Asked),
            Drive::Standing(Standing::Declined),
        ] {
            assert_eq!(
                may_write(&pane(false), persons(drive), true),
                Ok(WriteLeave::LaunchVariable)
            );
        }
        assert_eq!(
            may_write(
                &pane(false),
                persons(Drive::Standing(Standing::Allowed)),
                true
            ),
            Ok(WriteLeave::Driver)
        );
    }

    /// Stopping a turn is a key, so a leave covers it and its absence asks.
    #[test]
    fn a_driven_sessions_turn_is_stopped_on_the_same_leave() {
        assert_eq!(
            may_interrupt(
                &pane(false),
                persons(Drive::Standing(Standing::Allowed)),
                false
            ),
            Ok(WriteLeave::Driver)
        );
        assert_eq!(
            may_interrupt(&pane(false), persons(Drive::NotAsked), false),
            Err(WriteDenied::AskPerson)
        );
    }

    #[test]
    fn asking_a_person_is_not_a_refusal_on_the_wire() {
        let value = asked_person_value("surface.send_text");
        let error = &value[JSONRPC_ERROR_KEY];
        assert_eq!(error["code"], -32005);
        assert_ne!(error["code"], Refusal::CODE);
        assert_eq!(error["data"]["reason"], "asked_person");
    }

    /// The grounds are those of a key, because it is one.
    #[test]
    fn a_session_stops_the_turn_of_what_it_opened_and_of_nothing_else() {
        assert_eq!(
            may_interrupt(&pane(false), MINE, false),
            Ok(WriteLeave::Opener)
        );
        assert_eq!(
            may_interrupt(&pane(false), A_PERSONS, false),
            Err(WriteDenied::Refused(Refusal::NotYours))
        );
        assert_eq!(
            may_interrupt(&Caller::Outside, A_PERSONS, false),
            Err(WriteDenied::GateClosed)
        );
        assert_eq!(
            may_interrupt(&Caller::Outside, A_PERSONS, true),
            Ok(WriteLeave::LaunchVariable)
        );
    }

    /// The key that stops a turn is, on a permission question, the answer
    /// "no". Not for its opener and not with the variable set.
    #[test]
    fn a_session_that_waits_for_a_person_is_not_interrupted() {
        let mine_waiting = WriteTarget {
            waiting: true,
            ..MINE
        };
        for gate_open in [false, true] {
            assert_eq!(
                may_interrupt(&pane(false), mine_waiting, gate_open),
                Err(WriteDenied::Refused(Refusal::Waiting))
            );
        }
    }

    /// Where a key into its own pane is let through by the launch variable,
    /// stopping its own turn is not: the call would be part of that turn.
    #[test]
    fn a_session_never_interrupts_itself() {
        let itself = WriteTarget {
            thread_id: Some(3),
            opened_by: None,
            waiting: false,
            drive: Drive::NotOffered,
        };
        for gate_open in [false, true] {
            assert_eq!(
                may_interrupt(&pane(false), itself, gate_open),
                Err(WriteDenied::Refused(Refusal::Itself))
            );
        }
    }

    const ALL_GATES: LaunchGates = LaunchGates {
        orchestration: true,
        scripting: true,
    };

    #[test]
    fn a_session_moves_only_what_it_opened() {
        assert_eq!(may_arrange(&pane(false), MINE, NO_GATES), Ok(()));
        assert_eq!(
            may_arrange(&pane(false), A_PERSONS, NO_GATES),
            Err(Refusal::NotYours)
        );
        // The variable is for a caller outside a pane; in one it adds nothing.
        assert_eq!(
            may_arrange(&pane(false), A_PERSONS, ALL_GATES),
            Err(Refusal::NotYours)
        );
        let itself = WriteTarget {
            thread_id: Some(3),
            opened_by: None,
            waiting: false,
            drive: Drive::NotOffered,
        };
        assert_eq!(
            may_arrange(&pane(false), itself, NO_GATES),
            Err(Refusal::Itself)
        );
        // Moving a session types nothing into it, so a question on its screen
        // does not stand in the way.
        let waiting = WriteTarget {
            waiting: true,
            ..MINE
        };
        assert_eq!(may_arrange(&pane(false), waiting, NO_GATES), Ok(()));
    }

    #[test]
    fn a_script_outside_a_pane_rearranges_by_the_launch_variable() {
        assert_eq!(
            may_arrange(&Caller::Outside, A_PERSONS, NO_GATES),
            Err(Refusal::NotFromAPane)
        );
        assert_eq!(may_arrange(&Caller::Outside, A_PERSONS, ALL_GATES), Ok(()));
    }

    /// Record 9 in project 0, opened by record 3 (the caller in `pane`).
    const MINE_IDLE: CloseTarget = CloseTarget {
        thread_id: 9,
        ws_idx: 0,
        opened_by: Some(3),
        turn_in_flight: false,
        unseen_by_person: false,
    };

    fn rail_showing(
        status: crate::project::ThreadStatus,
        source: crate::rail_state::RailSource,
    ) -> crate::rail_state::RailSnapshot {
        crate::rail_state::RailSnapshot {
            status,
            source,
            runs_ended: 0,
            last_outcome: None,
            turn_marker: None,
            background_shells: 0,
            exited: false,
            message: None,
            opening_prompt: None,
            agent: Some("codex"),
            session_id: None,
            cwd: None,
        }
    }

    /// A session standing on "Trust this folder?" has been asked nothing.
    /// Its opener closes it like an idle one.
    #[test]
    fn a_question_before_the_first_prompt_is_no_turn_to_lose() {
        use crate::project::ThreadStatus;
        use crate::rail_state::RailSource;
        assert!(!closing_loses_a_turn(&rail_showing(
            ThreadStatus::WaitingForInput,
            RailSource::Terminal
        )));
        // A permission ask in the middle of a turn still is one.
        for source in [RailSource::Detector, RailSource::Hook] {
            assert!(closing_loses_a_turn(&rail_showing(
                ThreadStatus::WaitingForInput,
                source
            )));
        }
        assert!(closing_loses_a_turn(&rail_showing(
            ThreadStatus::Thinking,
            RailSource::PtyFlow
        )));
        assert!(!closing_loses_a_turn(&rail_showing(
            ThreadStatus::Idle,
            RailSource::Detector
        )));
    }

    const NEWS: RunFacts = RunFacts {
        detector_speaks: true,
        interrupted: false,
        last_message_from_opener: false,
    };

    #[test]
    fn a_run_the_pass_read_to_its_end_is_news() {
        assert!(run_is_news(NEWS));
    }

    /// A session the hook speaks for is announced by the hook's own handler,
    /// and one only the byte counter speaks for by nobody: output stopping is
    /// not a turn ending. The pass announcing either was a second
    /// notification for the first and a false "finished" for the second.
    #[test]
    fn a_run_the_pass_does_not_speak_for_is_not_its_news() {
        assert!(!run_is_news(RunFacts {
            detector_speaks: false,
            ..NEWS
        }));
    }

    /// Pressing Esc is not the agent finishing.
    #[test]
    fn a_turn_a_person_stopped_is_not_news() {
        assert!(!run_is_news(RunFacts {
            interrupted: true,
            ..NEWS
        }));
    }

    /// The result of a task an opener handed down is the opener's to read.
    #[test]
    fn a_run_started_by_the_opener_is_not_news_for_the_person() {
        assert!(!run_is_news(RunFacts {
            last_message_from_opener: true,
            ..NEWS
        }));
    }

    #[test]
    fn a_session_closes_what_it_opened_once_its_turn_is_over() {
        assert_eq!(may_close(&pane(false), MINE_IDLE, NO_GATES, false), Ok(()));
    }

    #[test]
    fn a_session_never_closes_itself() {
        let itself = CloseTarget {
            thread_id: 3,
            opened_by: None,
            ..MINE_IDLE
        };
        for stop_turn in [false, true] {
            assert_eq!(
                may_close(&pane(false), itself, ALL_GATES, stop_turn),
                Err(Refusal::Itself)
            );
        }
    }

    #[test]
    fn a_session_in_another_project_is_out_of_reach() {
        let elsewhere = CloseTarget {
            ws_idx: 1,
            ..MINE_IDLE
        };
        assert_eq!(
            may_close(&pane(false), elsewhere, ALL_GATES, true),
            Err(Refusal::OtherProject)
        );
    }

    /// Not with the variables set and not with the turn stopped: there is no
    /// way for a session to close one a person opened.
    #[test]
    fn a_session_a_person_opened_is_never_closed_by_a_session() {
        let persons = CloseTarget {
            opened_by: None,
            ..MINE_IDLE
        };
        let anothers = CloseTarget {
            opened_by: Some(77),
            ..MINE_IDLE
        };
        for target in [persons, anothers] {
            for gates in [NO_GATES, ALL_GATES] {
                for stop_turn in [false, true] {
                    assert_eq!(
                        may_close(&pane(false), target, gates, stop_turn),
                        Err(Refusal::NotYours)
                    );
                }
            }
        }
    }

    /// A turn in flight is what closing would lose, so it takes saying so.
    /// Saying so lifts this refusal and no other.
    #[test]
    fn a_turn_in_flight_is_stopped_only_on_purpose() {
        let busy = CloseTarget {
            turn_in_flight: true,
            ..MINE_IDLE
        };
        assert_eq!(
            may_close(&pane(false), busy, NO_GATES, false),
            Err(Refusal::TurnInFlight)
        );
        assert_eq!(may_close(&pane(false), busy, NO_GATES, true), Ok(()));
    }

    /// The mark on the row is a person's unread result. Stopping the turn is
    /// not what the caller would have to mean, so saying so changes nothing.
    #[test]
    fn a_result_a_person_has_not_seen_is_not_closed_away() {
        let unseen = CloseTarget {
            unseen_by_person: true,
            ..MINE_IDLE
        };
        for stop_turn in [false, true] {
            assert_eq!(
                may_close(&pane(false), unseen, NO_GATES, stop_turn),
                Err(Refusal::UnseenByPerson)
            );
        }
        // A person's own script, let in at launch, is the person's hand.
        assert_eq!(
            may_close(&Caller::Outside, unseen, ALL_GATES, false),
            Ok(())
        );
        // Not the caller's to close comes first: the word must not tell a
        // session anything about one it has no business with.
        let anothers = CloseTarget {
            opened_by: Some(77),
            ..unseen
        };
        assert_eq!(
            may_close(&pane(false), anothers, NO_GATES, false),
            Err(Refusal::NotYours)
        );
    }

    #[test]
    fn a_script_outside_a_pane_closes_by_the_launch_variable() {
        let persons = CloseTarget {
            opened_by: None,
            ..MINE_IDLE
        };
        assert_eq!(
            may_close(&Caller::Outside, persons, NO_GATES, false),
            Err(Refusal::NotFromAPane)
        );
        assert_eq!(
            may_close(&Caller::Outside, persons, ALL_GATES, false),
            Ok(())
        );
        let busy = CloseTarget {
            turn_in_flight: true,
            ..persons
        };
        assert_eq!(
            may_close(&Caller::Outside, busy, ALL_GATES, false),
            Err(Refusal::TurnInFlight)
        );
    }

    fn facts(thread_id: u64, opened_by: Option<u64>, focused: u64) -> PaneFacts {
        PaneFacts {
            thread_id: Some(thread_id),
            opened_by,
            focused,
        }
    }

    /// The opener is record 3. It gives up the pane of one of its own that
    /// has gone longest without focus.
    #[test]
    fn the_pane_given_up_is_the_openers_own_least_recently_focused() {
        let panes = [
            facts(3, None, 9),     // the opener itself
            facts(10, None, 1),    // a person's session, long unfocused
            facts(11, Some(3), 7), // opened by it
            facts(12, Some(3), 4), // opened by it, older focus
        ];
        assert_eq!(pane_to_give_up(&panes, 3, 20), Some(3));
        // A pane that never had focus goes first.
        let panes = [facts(11, Some(3), 7), facts(12, Some(3), 0)];
        assert_eq!(pane_to_give_up(&panes, 3, 20), Some(1));
    }

    #[test]
    fn a_persons_pane_and_the_openers_own_are_never_given_up() {
        let empty = PaneFacts {
            thread_id: None,
            opened_by: None,
            focused: 0,
        };
        let panes = [
            facts(3, None, 0),
            facts(10, None, 0),
            facts(13, Some(77), 0), // opened by some other session
            empty,
        ];
        assert_eq!(pane_to_give_up(&panes, 3, 20), None);
        // Nor the pane of the session being shown, were it somehow in one.
        let panes = [facts(20, Some(3), 0)];
        assert_eq!(pane_to_give_up(&panes, 3, 20), None);
    }

    #[test]
    fn an_empty_pane_is_taken_before_one_is_added() {
        assert_eq!(
            place_opened(PlacementAsk::Auto, true, None),
            Ok(Placement::EmptyPane)
        );
        assert_eq!(
            place_opened(PlacementAsk::Auto, true, Some(PaneRefusal::Ceiling)),
            Ok(Placement::EmptyPane)
        );
        assert_eq!(
            place_opened(PlacementAsk::Pane, true, Some(PaneRefusal::Ceiling)),
            Ok(Placement::EmptyPane)
        );
    }

    #[test]
    fn a_pane_is_added_where_one_fits() {
        assert_eq!(
            place_opened(PlacementAsk::Auto, false, None),
            Ok(Placement::NewPane)
        );
        assert_eq!(
            place_opened(PlacementAsk::Auto, false, Some(PaneRefusal::NoPanes)),
            Ok(Placement::NewPane)
        );
    }

    /// No room is not a refusal, and it is never a replacement: there is no
    /// answer here that puts the session where another one is showing.
    #[test]
    fn with_no_room_the_session_opens_in_the_rail_and_says_why() {
        assert_eq!(
            place_opened(PlacementAsk::Auto, false, Some(PaneRefusal::Ceiling)),
            Ok(Placement::Parked(ParkedBecause::Ceiling))
        );
        assert_eq!(
            place_opened(
                PlacementAsk::Auto,
                false,
                Some(PaneRefusal::TooNarrow { would_be: 3 })
            ),
            Ok(Placement::Parked(ParkedBecause::TooNarrow))
        );
        assert_eq!(
            place_opened(
                PlacementAsk::Auto,
                false,
                Some(PaneRefusal::TooNarrowGridFits {
                    would_be: 3,
                    stacked: false
                })
            ),
            Ok(Placement::Parked(ParkedBecause::TooNarrow))
        );
    }

    #[test]
    fn a_required_pane_that_cannot_be_given_is_refused() {
        assert_eq!(
            place_opened(PlacementAsk::Pane, false, Some(PaneRefusal::Ceiling)),
            Err(Refusal::NoRoom)
        );
        assert_eq!(
            place_opened(
                PlacementAsk::Pane,
                false,
                Some(PaneRefusal::TooNarrow { would_be: 2 })
            ),
            Err(Refusal::NoRoom)
        );
    }

    #[test]
    fn asking_for_the_rail_leaves_the_layout_alone() {
        assert_eq!(
            place_opened(PlacementAsk::Parked, true, None),
            Ok(Placement::Parked(ParkedBecause::Requested))
        );
    }

    #[test]
    fn placement_is_one_of_three_words() {
        use serde_json::json;
        assert_eq!(PlacementAsk::parse(None), Ok(PlacementAsk::Auto));
        assert_eq!(
            PlacementAsk::parse(Some(&json!("parked"))),
            Ok(PlacementAsk::Parked)
        );
        assert_eq!(
            PlacementAsk::parse(Some(&json!("pane"))),
            Ok(PlacementAsk::Pane)
        );
        assert!(PlacementAsk::parse(Some(&json!("left"))).is_err());
        assert!(PlacementAsk::parse(Some(&json!(1))).is_err());
    }

    #[test]
    fn a_refusal_carries_its_word_where_a_program_reads_it() {
        let value = Refusal::WorkerCeiling.into_value("surface.add_agent");
        let error = &value[JSONRPC_ERROR_KEY];
        assert_eq!(error["code"], Refusal::CODE);
        assert_eq!(error["data"]["reason"], "worker_ceiling");
        let promoted = crate::app::ipc_handler::promote_response(value, serde_json::json!(1));
        assert_eq!(promoted["error"]["code"], -32004);
        assert_eq!(promoted["error"]["data"]["reason"], "worker_ceiling");
    }

    /// The sender's name is somebody else's text on an agent's input line.
    #[test]
    fn the_provenance_line_cannot_be_broken_by_a_session_name() {
        assert_eq!(
            provenance_line("lead"),
            "[Splitlane] Sent by the agent session \"lead\", not typed by a person."
        );
        let hostile = provenance_line("a\"b\n\x1b[201~c");
        assert!(!hostile.contains('\n'));
        assert!(!hostile.contains('\x1b'));
        assert_eq!(hostile.matches('"').count(), 2);
    }
}
