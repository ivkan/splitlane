//! Claude Code session discovery - reads the on-disk session store at
//! `~/.claude/projects/<slug>/<uuid>.jsonl` and produces unified
//! [`SessionMeta`](crate::agent_sessions::SessionMeta) entries for the
//! sessions popover.
//!
//! There is no public Claude Code API for listing sessions (issue #34318);
//! the `.jsonl` files are the source of truth. Each line is one event;
//! the *first* line that carries `cwd` is the session envelope. The
//! LLM-generated `type:"ai-title"` record (when present) provides the
//! human-readable label that the `claude --resume` picker shows.
//!
//! All filesystem work happens off the GPUI main thread - call
//! [`read_sessions_for_cwd`] from inside `smol::unblock`.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::agent_sessions::{AssistantUsage, SessionAgent, SessionMeta, clean_session_label};

/// Maximum number of leading lines to scan for envelope + title. The
/// first lines of a Claude Code session file are typically
/// `permission-mode` and `file-history-snapshot` records with no `cwd`;
/// the actual user/assistant events start on line 3+. The `ai-title`
/// record (when present) is usually around line 9 but can appear later.
/// 256 covers >95% of files in the wild without the scan being visible
/// to the user.
const TITLE_SCAN_LIMIT: usize = 256;

/// Deeper line cap for the attribution scan, which walks PAST
/// the title break to aggregate `message.usage` across assistant turns. A
/// session's turns are spread through the file, so this is much larger than
/// [`TITLE_SCAN_LIMIT`] - but still bounded, and it runs ONLY on the attribution
/// path (the diff column load), never on the popover title scan. 20k lines
/// covers very long sessions while keeping a pathological file bounded.
const MODEL_USAGE_SCAN_LIMIT: usize = 20_000;

/// The model id used by synthetic CLI replies. They carry a zero `usage`, and
/// a line under this name is the CLI speaking rather than the agent answering.
///
/// Its home was `SYNTHETIC_MODEL`, and this file was
/// the only thing outside that crate reading it. The crate went with the
/// rendered face; the constant is a fact about what Claude Code writes into the
/// transcript, so it stays here, beside the code that reads that transcript.
const SYNTHETIC_MODEL: &str = "<synthetic>";

// Per-line JSONL read cap, centralized (see `crate::limits`).
use crate::agent_state::{OpenCall, ToolExecution, TranscriptProbe};
use crate::limits::MAX_LINE_BYTES;

/// The tag the CLI wraps a finished background agent's report in.
///
/// Used as a cheap gate on the raw line before anything is looked for inside
/// it, because most lines of a transcript are not this.
const TASK_NOTIFICATION_TAG: &str = "<task-notification>";

/// What the CLI writes into the transcript when a person presses Esc.
///
/// Two forms in the local corpus - the bare one and the one that names a tool
/// call it cut short - and both arrive as an ordinary `user` record carrying a
/// text block. Without this they read as a **prompt**, which would leave the
/// turn open forever on a session the person deliberately stopped: the one way
/// [`probe_state_from_tail`]'s turn reading could put a spinner on a session
/// that is plainly idle. Both forms are written out, because they are matched by
/// **equality** against one content block rather than searched for in the line -
/// see [`is_interrupt_block`] for the prompt that made a substring match unsafe.
const INTERRUPT_MARK: &str = "[Request interrupted by user]";
const INTERRUPT_TOOL_MARK: &str = "[Request interrupted by user for tool use]";

/// The tags the CLI writes its **own** user records under, running a slash
/// command.
///
/// Three of them, and all three are one gesture: the caveat that introduces a
/// local command, the command itself, and whatever it printed. `/clear`,
/// `/cost` and `/help` start no model turn at all - the session sits waiting
/// for the person - and `/clear` is the record that *begins* every new
/// transcript, so reading these as prompts pinned a spinner on every cleared
/// session for the whole trust horizon.
///
/// A cheap gate on the raw line, like [`TASK_NOTIFICATION_TAG`]: most user
/// records are not any of these.
const LOCAL_COMMAND_TAGS: [&str; 3] = [
    "<command-name>",
    "<local-command-caveat>",
    "<local-command-stdout>",
];

/// The key a background agent is held open under.
///
/// Prefixed so that an agent id can never collide with a `tool_use` id in the
/// same set - two id spaces, one map, and nothing says they cannot meet.
fn async_agent_key(agent_id: &str) -> String {
    format!("agent:{agent_id}")
}

/// Every `<task-id>` named by a task notification on this line.
///
/// A line can carry more than one - a notification is enqueued per agent and
/// the queue is written as one record - so this yields all of them rather than
/// the first. Scanning the text rather than a field is deliberate: the same
/// notification appears under three record shapes and only one of them puts it
/// where a field lookup would find it.
fn task_notification_ids(line: &str) -> Vec<&str> {
    const OPEN: &str = "<task-id>";
    const CLOSE: &str = "</task-id>";
    let mut ids = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find(OPEN) {
        rest = &rest[start + OPEN.len()..];
        let Some(end) = rest.find(CLOSE) else { break };
        let id = &rest[..end];
        if !id.is_empty() {
            ids.push(id);
        }
        rest = &rest[end + CLOSE.len()..];
    }
    ids
}

/// Cap rendered first-user-message labels at this character count to keep
/// the popover row from overflowing horizontally.
const LABEL_MAX_CHARS: usize = 80;

/// First-line envelope. Tolerant: any missing field falls back via
/// `serde(default)` so the parser never bails on a forward-compatible schema
/// change.
#[derive(Debug, Deserialize)]
struct FirstLineEnvelope {
    #[serde(default, rename = "sessionId")]
    session_id: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    cwd: String,
    #[serde(default, rename = "gitBranch")]
    git_branch: String,
}

/// How much of a transcript's tail the model probe reads. Assistant turns are
/// tens of kilobytes apart at worst, and this is the only part of the file the
/// probe needs: it wants the NEWEST model, not every one the session used.
/// Bounded so a multi-megabyte transcript costs the same as a short one.
const MODEL_TAIL_BYTES: u64 = 512 * 1024;

/// How much of a transcript's tail the answer probe reads.
///
/// Four times [`MODEL_TAIL_BYTES`], and for a different reason than that
/// constant's: the model probe runs every 15 seconds for every visible surface
/// and wants to be cheap, while this one runs when a user asks for their last
/// answer and wants to *find* it. A turn that ended in a long run of tool calls
/// can push the text that preceded it a long way back, and a window that misses
/// it reports "no answer" about a session that plainly has one.
const ANSWER_TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// The last answer the agent gave in this transcript, as the markdown it was
/// written in.
///
/// The transcript is the source, never the rendered view and never the
/// terminal's scrollback: the `.jsonl` already stores an assistant message as
/// markdown, so this is a copy rather than a re-serialization, and it is
/// unaffected by what is or is not drawn on screen. Scrollback would carry
/// wrapped lines and TUI chrome; a rendered AST would have to be turned back
/// into markdown and would lose whatever it did not model.
///
/// **Sidechain turns are skipped.** A `Task` subagent writes its own assistant
/// turns into the same file with `isSidechain: true`, and they are frequently
/// the newest ones - a subagent usually finishes after the lead's last word.
/// Copying one would hand the user a report they did not ask for, attributed to
/// a session that did not write it.
///
/// **A turn is its text blocks, in order.** One assistant turn interleaves text
/// with `tool_use`; the answer is the text, concatenated. A turn carrying no
/// text at all is not an answer - it is the agent still working - so the search
/// continues past it. That is also why the newest turn is often not the one
/// returned: an agent that ends on a tool call has not finished answering.
///
/// Blocking I/O - call from inside `smol::unblock`.
pub fn read_last_answer_from_tail(path: &Path) -> Option<String> {
    // `None` means two different things to the caller - "no answer yet" and
    // "could not read" - and it shows the user the first. That is the right
    // message (they can act on neither), but the second must not vanish
    // entirely, or a permissions problem looks like an empty session forever.
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) => {
            log::warn!("last answer: cannot open {}: {e}", path.display());
            return None;
        }
    };
    let len = match file.metadata() {
        Ok(meta) => meta.len(),
        Err(e) => {
            log::warn!("last answer: cannot stat {}: {e}", path.display());
            return None;
        }
    };
    let start = len.saturating_sub(ANSWER_TAIL_BYTES);
    let mut reader = BufReader::new(file);
    if start > 0 {
        use std::io::Seek;
        reader.seek(std::io::SeekFrom::Start(start)).ok()?;
        // The window opened mid-line; that fragment is not JSON. Dropped here
        // rather than skipped below, and capped for the reason every read in
        // this file is capped: an agent writes these files.
        let mut partial = String::new();
        reader
            .by_ref()
            .take(MAX_LINE_BYTES)
            .read_line(&mut partial)
            .ok()?;
    }
    let mut newest: Option<String> = None;
    let mut buf = String::new();
    loop {
        buf.clear();
        let read = reader
            .by_ref()
            .take(MAX_LINE_BYTES)
            .read_line(&mut buf)
            .ok()?;
        if read == 0 {
            break;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(buf.trim()) else {
            continue;
        };
        if let Some(answer) = answer_text_of(&value) {
            newest = Some(answer);
        }
    }
    newest
}

/// How much of a transcript's tail the state probe reads.
///
/// The same size as the model probe and for the same reason: it runs for every
/// visible surface on a timer, so it must cost the same on a 40 MB transcript as
/// on a fresh one. A call that has been left open is by definition among the
/// newest records, so a window this size cannot miss one.
#[allow(
    dead_code,
    reason = "read by the rail's state pass, which lands next; exercised by \
    this module's own tests in the meantime"
)]
const STATE_TAIL_BYTES: u64 = 512 * 1024;

/// What the session's own file says it is doing, as of `now_secs`.
///
/// Returns the calls started and not finished (oldest first) and whether the
/// newest thing in the file is a stated failure. It does **not** decide what
/// that means - [`crate::agent_state::classify`] does, because the answer needs
/// a second input this file cannot supply.
///
/// A `tool_result` whose `tool_use` fell outside the window is ignored rather
/// than treated as an orphan: the window is a view of the end of the file, and
/// a call whose start is not in it was not left open recently enough to matter.
///
/// Blocking I/O - call from inside `smol::unblock`.
#[allow(
    dead_code,
    reason = "read by the rail's state pass, which lands next; exercised by \
    this module's own tests in the meantime"
)]
pub fn probe_state_from_tail(path: &Path, now_secs: i64) -> Option<TranscriptProbe> {
    let file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        // A session whose file exists and says nothing yet. "Nothing is open,
        // so the turn is over" would be a confident claim built on no evidence
        // at all, which is the one thing this probe must never make.
        return None;
    }
    let start = len.saturating_sub(STATE_TAIL_BYTES);
    let mut reader = BufReader::new(file);
    if start > 0 {
        use std::io::Seek;
        reader.seek(std::io::SeekFrom::Start(start)).ok()?;
        // The window opens mid-line, and on a transcript holding any non-ASCII
        // text it routinely opens mid-**codepoint** - so this read can fail as
        // `InvalidData`. The bytes are consumed either way and the fragment was
        // going to be discarded, so the error is the expected outcome here, not
        // a reason to abandon the probe. Failing it out would have made the
        // file source flaky on exactly the files that carry prose.
        let mut partial = Vec::new();
        std::io::BufRead::read_until(
            &mut reader.by_ref().take(MAX_LINE_BYTES),
            b'\n',
            &mut partial,
        )
        .ok()?;
    }

    // Insertion order is file order, which is chronological, so the first
    // surviving entry is the oldest open call.
    //
    // The kind travels with the entry rather than being derived from the name
    // at the end, because one of the two things this reader opens is not a tool
    // call at all - see the background agent below - and `execution_of` answers
    // about names Claude Code writes for tools.
    let mut open: Vec<(String, String, Option<i64>, ToolExecution)> = Vec::new();
    // Whether the newest record that had an opinion said the turn was still
    // running, and when that record was written. Both move together and only
    // on a record that had an opinion - see `turn_signal`.
    let mut turn_open = false;
    let mut turn_at: Option<i64> = None;
    let mut incomplete = false;
    let mut last_error: Option<usize> = None;
    let mut last_message: Option<usize> = None;
    let mut index = 0usize;
    // What the tail cost. One entry per API **message**, not per line: Claude
    // Code writes a message as one line per content block and repeats the full
    // usage on each, so counting lines inflates the bill (1.63x over this
    // machine's corpus). Lines
    // sharing an id are always adjacent - measured over 26 519 assistant lines
    // with no exception - so remembering the last one counted is the whole of
    // the dedup, and it survives a message split across two tail reads.
    let mut spend: Vec<crate::agent_state::SpendSample> = Vec::new();
    let mut last_costed_id: Option<String> = None;
    let mut raw: Vec<u8> = Vec::new();
    loop {
        raw.clear();
        let read = std::io::BufRead::read_until(
            &mut reader.by_ref().take(MAX_LINE_BYTES),
            b'\n',
            &mut raw,
        )
        .ok()?;
        if read == 0 {
            break;
        }
        // Oversize is decided on the **bytes**, before anything is parsed.
        //
        // A record too long for the per-line cap comes back as a fragment that
        // fills the cap and carries no terminator. Deciding this by "did serde
        // fail" was not enough: the tail of a truncated record is agent-written
        // text, so a fragment can balance its own braces and parse as a whole
        // record - forging a `tool_use` or an `api_error` that never happened.
        // A fragment is never handed to the parser now.
        //
        // A **short** unterminated line is the opposite and is nothing: the
        // agent is writing that record right now. Every probe of a working
        // session can land mid-write, so treating it as a gap would mark almost
        // every live session untrustworthy and quietly reduce the detector to
        // its process half.
        let terminated = raw.last() == Some(&b'\n');
        if !terminated {
            if read as u64 == MAX_LINE_BYTES {
                incomplete = true;
            }
            continue;
        }
        let Ok(text) = std::str::from_utf8(&raw) else {
            // Bytes that are not text where text was promised: something was
            // written that this reader cannot account for.
            incomplete = true;
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            // A terminated line that is not JSON is a record we cannot account
            // for, and the thing it might have closed is the thing that decides
            // whether a person is being kept waiting.
            incomplete = true;
            continue;
        };
        index += 1;
        // What this record cost, if anything. Read here because the line is
        // already parsed and the read is already paid for; nothing below looks
        // at it.
        if let Some(sample) = cost_of_record(&value, &mut last_costed_id) {
            spend.push(sample);
        }
        // Any record the session wrote counts as the session still speaking, so
        // a stated failure stops being current the moment anything follows it.
        // Keyed off "a record parsed" rather than "a message with array content
        // parsed", which left an `api_error` looking current across a turn that
        // visibly carried on.
        let is_error_record = value.get("type").and_then(|v| v.as_str()) == Some("system")
            && matches!(
                value.get("subtype").and_then(|v| v.as_str()),
                Some("api_error" | "model_refusal_fallback")
            );
        if !is_error_record {
            last_message = Some(index);
        }

        // The transcript states its failures rather than leaving them to be
        // inferred from silence, which is the one thing silence cannot tell us
        // apart from a question.
        if is_error_record {
            last_error = Some(index);
            continue;
        }

        // A record with no readable timestamp yields an **unknown** age, never
        // zero. Zero would date the call to 1970 and make it instantly older
        // than any grace, so one schema change moving this field would have had
        // every session claiming a person was being kept waiting, from the
        // first poll, permanently.
        let secs = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(crate::agent_sessions::last_activity_secs_from_iso)
            .filter(|secs| *secs > 0);

        // A **background agent** is the one thing a session can be doing that
        // the pairing below cannot see, and it is the longest thing it ever
        // does. `Agent` hands back `status: "async_launched"` almost at once -
        // 0.05 s over the 51 launches in the local corpus, against a subagent
        // measured still working fifteen minutes later - so the call it opened
        // is closed while the work has not started, and every arm downstream
        // reads the session as finished. That is the rail saying `idle` beside
        // a pane whose own footer names a running agent.
        //
        // The file states both ends of the real span, so this reads them as one
        // more open call. Two records apart, and both are here: the launch
        // carries `toolUseResult.agentId`, and when the agent stops the CLI
        // enqueues a `<task-notification>` naming the same id. Four end states
        // appear in the corpus - completed, killed, failed, stopped - and the
        // notification is written for all of them, so the close does not depend
        // on the agent having succeeded.
        //
        // Its kind is [`ToolExecution::Opaque`], which is what a subagent has
        // always been in that vocabulary: it runs inside the agent, it spawns
        // nothing, and it may legitimately take minutes. So this can move the
        // rail from `idle` to `running` and can never, on its own, claim that a
        // person is being kept waiting.
        //
        // **Not bounded by `TRUST_HORIZON`.** It was, and a cross-vendor pass
        // named the mistake: the horizon guards against a stale record turning
        // into a permanent claim about a *person*, which an `Opaque` call
        // cannot make - so expiring one bought nothing and cost the single
        // answer it had, on the ordinary case of a subagent that runs longer
        // than half an hour. `agent_state::classify` keeps it while the
        // agent's own process is still under the pane, which is the bound that
        // actually fits: what the horizon was really guarding against is a
        // frozen file, and the process answers that directly.
        if let Some(result) = value.get("toolUseResult")
            && result.get("isAsync").and_then(|v| v.as_bool()) == Some(true)
            && result.get("status").and_then(|v| v.as_str()) == Some("async_launched")
            && let Some(agent_id) = result.get("agentId").and_then(|v| v.as_str())
        {
            open.push((
                async_agent_key(agent_id),
                "background agent".to_string(),
                secs,
                ToolExecution::Opaque,
            ));
        }
        // The notification lands in three record shapes - a `queue-operation`
        // with a bare `content` string, the `user` turn it becomes, and an
        // `attachment` - and only one of them would survive the guard below. So
        // it is read off the line's own text, which is the one thing all three
        // share, rather than off a field that moves between them.
        if text.contains(TASK_NOTIFICATION_TAG) {
            for id in task_notification_ids(text) {
                let key = async_agent_key(id);
                open.retain(|(open_id, _, _, _)| *open_id != key);
            }
        }

        // Read before the content guard below, which drops every record whose
        // content is not an array - and a person's prompt is routinely written
        // as a bare string, so the guard skips the single most important record
        // there is for this question.
        if let Some(open_now) = turn_signal(&value, text) {
            turn_open = open_now;
            turn_at = open_now.then_some(secs).flatten();
        }

        let Some(content) = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for block in content {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (
                        block.get("id").and_then(|v| v.as_str()),
                        block.get("name").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    let execution = crate::agent_state::execution_of(name);
                    open.push((id.to_string(), name.to_string(), secs, execution));
                }
                Some("tool_result") => {
                    if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                        open.retain(|(open_id, _, _, _)| open_id != id);
                    }
                }
                _ => {}
            }
        }
    }

    Some(TranscriptProbe {
        spend,
        open_calls: open
            .into_iter()
            .map(|(_, name, secs, execution)| OpenCall {
                // For a tool call Claude Code's kind is a function of the name
                // alone, and that map lives in `agent_state` because it is also
                // what the rule's own tests reason about. It was asked here
                // until a background agent needed an entry with no tool name to
                // ask about, so the kind now travels with the entry.
                execution,
                name,
                // A record stamped in the future - a clock that moved, a file
                // copied from another machine - is age zero, never a negative
                // that would wrap into "waiting forever". A record with no
                // usable stamp has no age at all, which is a different thing
                // from being new and is treated as such by the rule.
                age: secs.map(|secs| {
                    std::time::Duration::from_secs(now_secs.saturating_sub(secs).max(0) as u64)
                }),
            })
            .collect(),
        // An open turn this reader could not date is dropped, not trusted. It
        // is the opposite choice from an open *call*, which keeps its vote with
        // no age at all - and the asymmetry is deliberate: a call with no age
        // is refused the waiting answer by the rule and can only cost a dot,
        // while a turn with no age has nothing bounding it but the process and
        // would spin on a file whose stamps a schema change moved.
        // A record stamped in the **future** of our own clock is undatable, for
        // the same reason a record with no stamp is. An open call clamps such a
        // stamp to age zero and is safe there - the waiting predicate needs an
        // age past `GRACE`, so age zero never claims a person. A turn has no
        // such lower bound: age zero sits inside the horizon for ever, and would
        // spin on a frozen file for as long as its process lived.
        open_turn: turn_open
            .then_some(turn_at)
            .flatten()
            .filter(|secs| *secs <= now_secs)
            .map(|secs| Duration::from_secs((now_secs - secs) as u64)),
        errored: match (last_error, last_message) {
            (Some(err), Some(msg)) => err > msg,
            (Some(_), None) => true,
            _ => false,
        },
        incomplete,
    })
}

/// What one record says about whether the turn is still running.
///
/// `Some(true)` - the turn continues past this record. `Some(false)` - it ended
/// here. `None` - this record says nothing either way, which is most of them: a
/// transcript is mostly `attachment`, `system`, `mode`, `last-prompt` and the
/// rest of the CLI's own bookkeeping.
///
/// # Where the answer comes from
///
/// `message.stop_reason` on an `assistant` record, which is the API's own field
/// and is written for every one of them: `tool_use` while the turn continues,
/// `end_turn` when it is over. Nothing is inferred from silence here - the file
/// states it.
///
/// A `user` record continues the turn whether it is a person's prompt or a tool
/// result: in the first case the agent has just been given something to do, in
/// the second it has just been given something to carry on with. The one
/// exception is an interrupt, which is written as a `user` record and *ends*
/// the turn - see [`INTERRUPT_MARK`].
///
/// # Two kinds of record are skipped, and both would say the wrong thing
///
/// A **sidechain** record belongs to a subagent, whose turns start and end
/// inside the parent's; its `end_turn` would close a turn that is still going.
/// A **synthetic-model** record is the CLI speaking rather than the agent
/// answering, which is the same reason the transcript's own answer reader skips
/// it.
fn turn_signal(value: &serde_json::Value, raw_line: &str) -> Option<bool> {
    if value.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let message = value.get("message");
    match value.get("type").and_then(|v| v.as_str())? {
        "assistant" => {
            let message = message?;
            if message.get("model").and_then(|v| v.as_str()) == Some(SYNTHETIC_MODEL) {
                return None;
            }
            // **Only a measured word may end a turn.** `stop_reason` is not a
            // two-value field: the API also has `max_tokens`, which the CLI
            // continues from, and `pause_turn`, which means "mid-turn" by
            // definition. Reading everything-but-`tool_use` as the end - which
            // this did until a cross-vendor pass named it - turns either of
            // those into `idle` on a session that is working, which is the one
            // claim this module may not make.
            //
            // So the **closing** set is the allowlist and everything else
            // leaves the standing answer alone. Same shape as `execution_of`'s
            // prior next door, and it errs the same way: a turn held open too
            // long is a late dot, bounded by the process and by the horizon; a
            // turn closed too early is a lie.
            match message.get("stop_reason").and_then(|v| v.as_str())? {
                "end_turn" | "stop_sequence" => Some(false),
                "tool_use" => Some(true),
                _ => None,
            }
        }
        "user" => {
            // **A local command is not a turn.** `/clear`, `/cost`, `/help` and
            // every custom `/sf:audit` are written as ordinary `user` records
            // wrapped in `<command-name>`, and the built-in ones start no model
            // turn at all - the session sits waiting for the person. Read as
            // prompts they pinned a spinner on a freshly cleared session for the
            // whole trust horizon, and `/clear` is the record that *begins*
            // every new transcript, so that was every cleared session.
            //
            // `None` rather than "turn over", because a custom command **does**
            // start a turn: the standing answer is left alone and the assistant
            // record a second later opens it properly. The cost is a dot one
            // record late.
            //
            // All three tags, because the CLI writes a `<local-command-caveat>`
            // record beside the command and it carries a plain string - so
            // gating on the command tag alone left the caveat opening the turn
            // the command had just declined to.
            if LOCAL_COMMAND_TAGS.iter().any(|tag| raw_line.contains(tag)) {
                return None;
            }
            let content = message?.get("content")?;
            if let Some(text) = content.as_str() {
                return (!text.trim().is_empty()).then_some(true);
            }
            let blocks = content.as_array()?;
            // Read before the general case: an interrupt is a text block like
            // any other, and it is the one that means the opposite.
            if blocks.iter().any(is_interrupt_block) {
                return Some(false);
            }
            blocks
                .iter()
                .any(|block| {
                    matches!(
                        block.get("type").and_then(|v| v.as_str()),
                        Some("text" | "tool_result")
                    )
                })
                .then_some(true)
        }
        _ => None,
    }
}

/// Whether one content block is the CLI's own interrupt record.
///
/// **Equality, not `contains`.** The mark used to be matched against the whole
/// raw line, and a person's genuine prompt can carry that text verbatim -
/// pasting a transcript excerpt and asking why it stopped there is exactly the
/// shape of question people put to an agent. Their prompt would have read as an
/// interrupt, and the rail would have said `idle` about the turn answering it.
/// Across the local corpus every interrupt record's text block is the mark and
/// nothing else, in both of its two forms.
fn is_interrupt_block(block: &serde_json::Value) -> bool {
    if block.get("type").and_then(|v| v.as_str()) != Some("text") {
        return false;
    }
    block
        .get("text")
        .and_then(|v| v.as_str())
        .is_some_and(|text| text.trim() == INTERRUPT_MARK || text.trim() == INTERRUPT_TOOL_MARK)
}

/// The markdown of one transcript line, when that line is an answer the session
/// itself gave. `None` for everything else, which is most lines.
fn answer_text_of(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    if value.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let message = value.get("message")?;
    // The CLI writes its own interjections under a synthetic model name.
    // Skipped here for the same reason the badge skips it: it is the CLI
    // speaking, not the agent answering.
    if message.get("model").and_then(|v| v.as_str()) == Some(SYNTHETIC_MODEL) {
        return None;
    }
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return non_empty(text.trim());
    }
    let blocks = content.as_array()?;
    let mut out = String::new();
    for block in blocks {
        if block.get("type").and_then(|v| v.as_str()) != Some("text") {
            continue;
        }
        let Some(text) = block.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(text.trim_end());
    }
    non_empty(out.trim())
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// The cost of one already-parsed assistant record, when this build can price
/// it, and `None` for everything else.
///
/// **A message is costed once, however many lines it spans.** Claude Code
/// writes one API message as one line per content block - `thinking`, `text`,
/// `tool_use` - and repeats the whole `message.usage` on each, so costing per
/// line inflates the bill by its block count (1.63x over this machine's
/// corpus). Lines sharing
/// an id are always adjacent - checked over 26 519 assistant lines with no
/// exception - so remembering the last one costed is the whole of the dedup,
/// and it holds across a message split between two reads.
///
/// `last_costed_id` is the caller's, because the two readers that use this walk
/// different windows of the same file and each needs its own memory of where it
/// is.
fn cost_of_record(
    value: &serde_json::Value,
    last_costed_id: &mut Option<String>,
) -> Option<crate::agent_state::SpendSample> {
    if value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    let message = value.get("message")?;
    let id = message.get("id").and_then(|v| v.as_str())?;
    if last_costed_id.as_deref() == Some(id) {
        return None;
    }
    *last_costed_id = Some(id.to_string());
    let model = message.get("model").and_then(|v| v.as_str())?;
    let u = message.get("usage")?;
    let usage = crate::agent_sessions::AssistantUsage {
        input: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        output: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        cache_read: u
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_creation: u
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    };
    if usage.is_empty() {
        return None;
    }
    let dollars = crate::pricing::estimate_cost(model, &usage)?;
    let at = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(crate::agent_sessions::last_activity_secs_from_iso)
        .filter(|at| *at > 0)?;
    Some(crate::agent_state::SpendSample { at, dollars })
}

/// What one read of the tail can say about a live surface: which model it is
/// talking to, and what it has been spending.
///
/// Two facts, one read. They are paced alike - a model changes when a person
/// changes it, and a rate over ten minutes does not need a two-second refresh -
/// so one slow probe answers both, over the same bytes. Keeping them apart
/// would have meant reading the same 512 KB twice a minute per surface to learn
/// the cheaper of the two.
pub struct TailFacts {
    /// How many tokens the newest turn carried into the model - the session's
    /// **actual** context size, not an estimate.
    ///
    /// `input + cache_read + cache_creation` of the last assistant message.
    /// Traced against this machine's own transcripts: it climbs monotonically
    /// between compactions (683 044 → 683 828 → 684 758 → 688 035 over four
    /// minutes) and drops to the compaction's `postTokens` when one fires.
    ///
    /// **A count, not a share.** What fraction of the window this is cannot be
    /// answered from anything on disk - see the note above the field's only
    /// writer - so this deliberately carries no denominator and no percentage.
    pub context_tokens: Option<u64>,
    /// The newest model an assistant turn named, for the slot header's badge
    /// (the design: "badge = model for agents").
    ///
    /// The header asks what the session is talking to *now*, so a session that
    /// switched models reports the one it switched to. The CLI writes its own
    /// interjections under a synthetic model name; naming that on a badge would
    /// report a model nobody can select, so it is skipped - the same rule the
    /// transcript's own reader applies. It is skipped for the **badge only**:
    /// the spend below still counts those turns, because the bill did.
    pub model: Option<String>,
    /// One entry per API message in the tail, oldest first.
    pub spend: Vec<crate::agent_state::SpendSample>,
    /// The context size an **automatic** compaction fired at, when one passed
    /// through this window of the file.
    ///
    /// This is the session's own ceiling, measured rather than assumed, and it
    /// is the only honest denominator there is: nothing on disk states the
    /// context window, and it belongs to the plan rather than to the model.
    ///
    /// **Only an automatic compaction says anything.** A manual one happens
    /// wherever the person asked for it: measured over this machine's corpus,
    /// 28 manual compactions fired between 116 712 and 801 793 tokens while
    /// all 8 automatic ones fired between 919 686 and 1 002 336. Taking a
    /// manual one as the ceiling would peg the meter to whatever size somebody
    /// happened to type `/compact` at, and every later turn would read past
    /// 100%.
    ///
    /// It has to be **caught as it goes past**: once the new segment grows, the
    /// record sits far behind the tail this reads. A session the app joined
    /// after its last compaction has no ceiling until the next one, which is
    /// the meter's phase one.
    pub auto_compacted_at: Option<u64>,
    /// The newest compaction this window of the file carried, and whether a
    /// turn has added to the context since.
    ///
    /// The ring beside the arc means one thing - **that drop was a compaction
    /// and not a fall in spending** - so it lives exactly as long as the two
    /// can be confused: until the next turn puts something back. Not a timer,
    /// because a minute is a made-up length, and not "until somebody looks",
    /// because a look is not something this app can see. A session that
    /// compacted and then stopped keeps the ring, which is right: nothing has
    /// happened since.
    pub compaction: Option<CompactionSeen>,
}

/// One compaction, as the transcript states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactionSeen {
    /// Unix seconds it was written.
    pub at: i64,
    pub pre_tokens: u64,
    pub post_tokens: u64,
    /// Automatic ones measure the ceiling; manual ones happen wherever the
    /// person asked and measure nothing.
    pub automatic: bool,
    /// Whether a turn has added to the context since. `false` is what puts the
    /// ring on the arc.
    pub turn_since: bool,
}

/// See [`TailFacts`].
///
/// Reads the last [`MODEL_TAIL_BYTES`] rather than walking the file. The first
/// line of the window is almost always a partial line and is dropped for that
/// reason.
///
/// Blocking I/O - call from inside `smol::unblock`.
pub fn read_tail_facts(path: &Path) -> Option<TailFacts> {
    let file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(MODEL_TAIL_BYTES);
    let mut reader = BufReader::new(file);
    if start > 0 {
        use std::io::Seek;
        reader.seek(std::io::SeekFrom::Start(start)).ok()?;
        // The window almost certainly opened mid-line; that fragment is not
        // JSON and would only ever be skipped below, but dropping it here
        // keeps the loop's meaning simple.
        //
        // Capped like every other line read in this file, and for the same
        // reason: an agent can write into `~/.claude/projects/`, and an
        // uncapped `read_line` on a transcript written as one enormous line
        // would allocate the rest of the file on a background thread. The
        // window is 512 KB; a line inside it is not bounded by that.
        let mut partial = String::new();
        reader
            .by_ref()
            .take(MAX_LINE_BYTES)
            .read_line(&mut partial)
            .ok()?;
    }
    let mut newest: Option<String> = None;
    let mut spend: Vec<crate::agent_state::SpendSample> = Vec::new();
    let mut last_costed_id: Option<String> = None;
    // The newest turn's context size. Overwritten rather than accumulated: this
    // is a level, not a total, and only the last one is true now.
    let mut context_tokens: Option<u64> = None;
    let mut auto_compacted_at: Option<u64> = None;
    let mut compaction: Option<CompactionSeen> = None;
    let mut buf = String::new();
    loop {
        buf.clear();
        // The same per-line cap the scans above use. An agent can
        // write into `~/.claude/projects/`, so an unbounded read here would
        // allocate whatever a malicious single-line file claims.
        let read = reader
            .by_ref()
            .take(MAX_LINE_BYTES)
            .read_line(&mut buf)
            .ok()?;
        if read == 0 {
            break;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(buf.trim()) else {
            continue;
        };
        // Asked before the model guards below, because a record the badge skips
        // is still a record the bill counted: the CLI's own interjections carry
        // a synthetic model name that must never reach a badge, and an empty
        // model is nothing to show - neither is a reason to pretend the turn
        // was free. `cost_of_record` prices only what the table knows, so an
        // unpriceable model drops out there rather than here.
        if let Some(sample) = cost_of_record(&value, &mut last_costed_id) {
            spend.push(sample);
        }
        // The ceiling, when an automatic compaction passes by. See the field.
        if let Some(meta) = value.get("compactMetadata")
            && let Some(pre) = meta.get("preTokens").and_then(|v| v.as_u64())
            && pre > 0
        {
            let automatic = meta.get("trigger").and_then(|v| v.as_str()) == Some("auto");
            if automatic {
                auto_compacted_at = Some(pre);
            }
            compaction = Some(CompactionSeen {
                at: value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(crate::agent_sessions::last_activity_secs_from_iso)
                    .unwrap_or(0),
                pre_tokens: pre,
                post_tokens: meta.get("postTokens").and_then(|v| v.as_u64()).unwrap_or(0),
                automatic,
                turn_since: false,
            });
        }
        // The context the newest turn carried. Every line of a multi-block
        // message repeats it, so overwriting is right and the dedup above is
        // not needed here.
        if value.get("type").and_then(|v| v.as_str()) == Some("assistant")
            && let Some(u) = value.get("message").and_then(|m| m.get("usage"))
        {
            let field = |key: &str| u.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            let carried = field("input_tokens")
                .saturating_add(field("cache_read_input_tokens"))
                .saturating_add(field("cache_creation_input_tokens"));
            if carried > 0 {
                context_tokens = Some(carried);
                // Something was put back, so the drop can no longer be mistaken
                // for a fall in spending and the ring comes off.
                if let Some(seen) = compaction.as_mut() {
                    seen.turn_since = true;
                }
            }
        }
        if value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(model) = value
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        if model.is_empty() || model == SYNTHETIC_MODEL {
            continue;
        }
        newest = Some(model.to_string());
    }
    Some(TailFacts {
        model: newest,
        spend,
        context_tokens,
        auto_compacted_at,
        compaction,
    })
}

/// Convert an absolute path into the slug Claude Code uses as the directory
/// name under `~/.claude/projects/`. Algorithm (matches Claude Code's own
/// encoder): every character that is **not** ASCII alphanumeric becomes `-`.
/// That covers `/`, `\`, the Windows drive `:` (so `C:\dev\splitlane` →
/// `C--dev-splitlane`, NOT `C:-dev-splitlane`), spaces (`C:\Program Files\..`
/// → `C--Program-Files-..`), and `.` (so `/home/u/.claude` → `-home-u--claude`,
/// the dir Claude Code actually writes). Runs of separators are NOT collapsed -
/// `C:\` produces the literal `C--`. No percent-encoding or hashing.
///
/// The previous encoder only replaced `/` and `\`, leaving the drive `:`
/// intact: on Windows it produced `C:-dev-splitlane` while the on-disk dir is
/// `C--dev-splitlane`, so `read_dir` opened a path that never existed and the
/// sessions sidebar came up empty.
pub fn slug_for_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Compute the absolute path of `<claude config dir>/projects/<slug>/`.
/// Returns `None` when the root cannot be resolved (no `$HOME` /
/// `%USERPROFILE%` and no `CLAUDE_CONFIG_DIR`).
///
/// The root is [`crate::claude_pid_state::claude_config_dir`] rather than a
/// second hardcoded `~/.claude`, because the CLI honours `CLAUDE_CONFIG_DIR`
/// and a user who sets it has no `~/.claude` at all - every lookup rooted
/// there finds nothing while looking exactly like "this project has no
/// sessions". Two roots for one directory is the "one word, one meaning"
/// failure in path form.
///
/// A trailing separator is normalized away first. Claude derives its own slug
/// from the agent process's cwd, which never carries one, so `/a/b/` has to
/// resolve to the same directory as `/a/b` - otherwise the lookup misses a
/// directory that exists, and [`session_file_exists`] reports "no session"
/// for a session that is very much there, sending `--session-id` for an id
/// the CLI already knows.
pub fn project_dir_for_cwd(cwd: &str) -> Option<PathBuf> {
    let slug = slug_for_cwd(normalize_cwd_for_slug(cwd));
    Some(
        crate::claude_pid_state::claude_config_dir()?
            .join("projects")
            .join(slug),
    )
}

/// Where a thread's agent transcript lives on disk, when it can have one.
///
/// `None` for a bare shell, for an agent with no forced session id, and for any
/// agent other than Claude Code - generalising over "any agent" from one example
/// was declined in P2.1 and this does not reverse it.
///
/// Its home used to be `transcript::transcript_path`, beside the rendered face.
/// The face is gone and this is not: it is the gate "whose file can this build
/// read", asked by the state detector, the model probe and the rail's context
/// menu, and it belongs next to [`project_dir_for_cwd`], which it calls.
pub fn transcript_path(thread: &crate::project::Thread) -> Option<PathBuf> {
    // `conversation_is_readable`, not `reports_state`: this function composes a
    // **Claude-shaped** path, and its callers parse a Claude-shaped file. Since
    // Codex earned `reports_state` those are two different facts, and following
    // the wrong one would hand the model badge and "copy the last answer" a
    // rollout to read as a transcript.
    if !thread
        .terminal_agent
        .is_some_and(crate::agent_launcher::TerminalAgent::conversation_is_readable)
    {
        return None;
    }
    let session_id = thread.session_id.as_deref()?;
    if !crate::agent_sessions::is_valid_session_id(session_id) {
        return None;
    }
    let dir = project_dir_for_cwd(&thread.cwd)?;
    Some(dir.join(format!("{session_id}.jsonl")))
}

/// Strip trailing path separators, unless that would reduce `cwd` to a bare
/// root. `/` is all separator and `C:\` is a drive root: trimming those
/// changes the slug (`-` → ``, `C--` → `C-`) instead of normalizing it, and
/// Claude keeps them intact.
fn normalize_cwd_for_slug(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.ends_with(':') {
        cwd
    } else {
        trimmed
    }
}

/// Whether the CLI already holds a session file for `session_id` under
/// `cwd`'s project dir.
///
/// Drives the mint-vs-resume choice in
/// [`crate::agent_launcher::SessionBinding::resolve`]: `--session-id` is only
/// legal for an id the CLI has never seen, so a thread may only pass it on
/// the launch that creates the session.
///
/// Unlike the readers below this is a single `stat` - it never walks or
/// parses the project dir - so it is cheap enough for the PTY-mount path on
/// the GPUI main thread. Ids are filtered through
/// [`crate::agent_sessions::is_valid_session_id`] first, whose allow-list
/// admits only ASCII alphanumerics plus `-`/`_` (and never a leading `-`):
/// no separator, no `.`, no space, no shell metacharacter. A tampered
/// `session.json` can therefore neither escape the project dir here nor
/// smuggle an extra argument into the launch command.
pub fn session_file_exists(cwd: &str, session_id: &str) -> bool {
    if !crate::agent_sessions::is_valid_session_id(session_id) {
        return false;
    }
    project_dir_for_cwd(cwd)
        .map(|dir| dir.join(format!("{session_id}.jsonl")).is_file())
        .unwrap_or(false)
}

fn project_snapshot_mtime(project_dir: &Path) -> Option<SystemTime> {
    let mut latest = fs::metadata(project_dir)
        .ok()
        .and_then(|m| m.modified().ok());
    let entries = fs::read_dir(project_dir).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !is_jsonl_file(&path) {
            continue;
        }
        let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
        latest = max_mtime(latest, modified);
    }

    latest
}

fn max_mtime(current: Option<SystemTime>, candidate: Option<SystemTime>) -> Option<SystemTime> {
    match (current, candidate) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Read all Claude Code session metadata for the given working directory.
/// Sessions are sorted by timestamp descending (most recent first) and only
/// those whose first-line `cwd` matches `cwd` (via
/// [`cwd_matches`](crate::agent_sessions::cwd_matches): exact on Unix,
/// case/separator-insensitive on Windows) are kept - dedupes the rare slug
/// collision where two distinct paths produce the same directory name
/// (`/a/b-c` and `/a/b/c` both slug to `-a-b-c`).
///
/// **Blocking I/O** - call from inside `smol::unblock` or
/// `cx.background_executor`. Never invoke on the GPUI main thread.
pub fn read_sessions_for_cwd(cwd: &str) -> Vec<SessionMeta> {
    read_sessions_for_cwd_with_omitted(cwd).0
}

/// Like [`read_sessions_for_cwd`], but also reports how many older matching
/// sessions were omitted by the sidebar retention cap.
pub fn read_sessions_for_cwd_with_omitted(cwd: &str) -> (Vec<SessionMeta>, usize) {
    let Some(project_dir) = project_dir_for_cwd(cwd) else {
        return (Vec::new(), 0);
    };
    // Existing Claude sessions are append-only JSONL files. Appending to a
    // file does not reliably change the parent directory mtime, so include
    // leaf-file mtimes in the cache fingerprint.
    let snapshot_mtime = project_snapshot_mtime(&project_dir);
    if let Some(snapshot_mtime) = snapshot_mtime
        && let Some(cached) = crate::agent_sessions::cache::lookup_with_mtime(
            SessionAgent::Claude,
            cwd,
            snapshot_mtime,
        )
    {
        return cached;
    }
    let Ok(entries) = fs::read_dir(&project_dir) else {
        return (Vec::new(), 0);
    };

    let sessions = entries.flatten().filter_map(|entry| {
        let path = entry.path();
        if !is_jsonl_file(&path) {
            return None;
        }
        read_session_meta(&path).filter(|meta| crate::agent_sessions::cwd_matches(&meta.cwd, cwd))
    });

    let (sessions, omitted) = crate::agent_sessions::collect_recent_sessions(
        sessions,
        crate::agent_sessions::SIDEBAR_SESSION_RETAINED_PER_SOURCE,
    );
    if let Some(snapshot_mtime) = project_snapshot_mtime(&project_dir) {
        crate::agent_sessions::cache::store_result_with_mtime(
            SessionAgent::Claude,
            cwd,
            snapshot_mtime,
            &sessions,
            omitted,
        );
    }
    (sessions, omitted)
}

/// Like [`read_sessions_for_cwd`] but the retained
/// attribution candidates are scanned deeper to populate `model` + aggregated
/// `usage`. Deliberately bypasses the title-scan mtime cache - that cache
/// stores usage-less rows for the popover, and the attribution result is
/// instead cached on the diff `Column` keyed to its diff fingerprint
/// (re-fetched only on re-diff). **Blocking I/O** - call from inside
/// `smol::unblock`.
pub fn read_sessions_with_usage_for_attribution(cwd: &str, branch: &str) -> Vec<SessionMeta> {
    let Some(project_dir) = project_dir_for_cwd(cwd) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&project_dir) else {
        return Vec::new();
    };

    let mut candidates: Vec<(SessionMeta, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_jsonl_file(&path) {
            continue;
        }
        if let Some(meta) = read_session_meta(&path)
            && crate::agent_sessions::cwd_matches(&meta.cwd, cwd)
        {
            crate::agent_sessions::push_ranked_attribution(
                &mut candidates,
                meta,
                path,
                branch,
                crate::agent_sessions::DIFF_ATTRIBUTION_MATCH_CAP,
            );
        }
    }

    let enriched: Vec<SessionMeta> = candidates
        .into_iter()
        .filter_map(
            |(fallback, path)| match read_session_meta_inner(&path, true) {
                Some(meta) if crate::agent_sessions::cwd_matches(&meta.cwd, cwd) => Some(meta),
                Some(_) => None,
                None => Some(fallback),
            },
        )
        .collect();
    crate::agent_sessions::match_sessions_to_column(enriched, cwd, branch)
}

fn is_jsonl_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
}

/// Read the head of a `.jsonl` and collect everything we need for a UI
/// row in a single pass: the first envelope carrying `cwd`, the
/// LLM-generated `ai-title` (when present), and the cleaned first
/// `type:"user"` message (used as a fallback title when `ai-title` is
/// absent - typical of sessions older than that feature's introduction).
///
/// Title priority:
/// 1. `type:"ai-title"` → `aiTitle` field. Matches what the
///    `claude --resume` picker shows for newer sessions.
/// 2. First `type:"user"` message, with `<command-*>` boilerplate
///    collapsed into `/<name> <args>` when present.
fn read_session_meta(path: &Path) -> Option<SessionMeta> {
    read_session_meta_inner(path, false)
}

/// Shared session-head scan. `scan_usage = false` is the title-only popover
/// path: bounded by [`TITLE_SCAN_LIMIT`], it stops as soon as the envelope +
/// title are known. `scan_usage = true` is the attribution path: it
/// walks past the title (bounded by [`MODEL_USAGE_SCAN_LIMIT`]) aggregating
/// `message.usage` across assistant turns and capturing `message.model`.
fn read_session_meta_inner(path: &Path, scan_usage: bool) -> Option<SessionMeta> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();

    let mut envelope: Option<FirstLineEnvelope> = None;
    let mut ai_title: Option<String> = None;
    let mut user_fallback: Option<String> = None;
    // Model + aggregated usage (attribution path only).
    let mut model: Option<String> = None;
    let mut usage = AssistantUsage::default();
    let mut saw_usage = false;
    // One API message is written as one JSONL line PER CONTENT BLOCK, and every
    // one of those lines carries the FULL `message.usage` - see the comment on
    // the assistant arm below. Folding per line therefore counts a turn once
    // per block. Bounded by `MODEL_USAGE_SCAN_LIMIT`, so this set holds at most
    // 20k short ids.
    let mut counted_messages: HashSet<String> = HashSet::new();
    // Lines that carried usage but no `message.id` to dedup on. Zero in every
    // transcript measured; a non-zero count means the format moved and the sum
    // is inflated again, so it is said out loud rather than absorbed.
    let mut usage_lines_without_id = 0usize;

    let scan_limit = if scan_usage {
        MODEL_USAGE_SCAN_LIMIT
    } else {
        TITLE_SCAN_LIMIT
    };
    for _ in 0..scan_limit {
        buf.clear();
        // Cap each line read
        // at MAX_LINE_BYTES. An agent can write to
        // `~/.claude/projects/<slug>/` (it's the very directory Claude
        // Code persists sessions to), so a malicious 500 MB
        // single-line JSONL would otherwise allocate fully on a
        // background smol::unblock thread before the
        // TITLE_SCAN_LIMIT count guard fires. Truncation surfaces as
        // a partial line that fails serde_json::from_str and is
        // skipped on `continue` below; the file's session entry is
        // simply omitted, not the entire scan.
        let n = reader
            .by_ref()
            .take(MAX_LINE_BYTES)
            .read_line(&mut buf)
            .ok()?;
        if n == 0 {
            break;
        }
        if n as u64 == MAX_LINE_BYTES && !buf.ends_with('\n') {
            // An exactly-MAX_LINE_BYTES line with no trailing newline is
            // ambiguous - it may be a genuinely TRUNCATED oversized line, or a
            // COMPLETE final record written without a final EOL. Peek one byte
            // to disambiguate: empty = EOF = the line is complete, fall through
            // and parse it (don't drop a valid final session). Non-empty = more
            // bytes follow = the cap truncated it mid-line → genuinely oversized.
            let more_follows = match reader.fill_buf() {
                Ok(b) => !b.is_empty(),
                // I/O error mid-read: abort like the drain loop below (don't
                // silently fall through and parse a possibly-truncated buf).
                Err(_) => return None,
            };
            if more_follows {
                // Newer Claude Code writes oversized records ahead of the
                // envelope -- notably a `type:"queue-operation"` first line
                // whose `content` blob can run to hundreds of KB and which
                // carries no `cwd`. Abandoning the file here dropped the
                // whole session from the sidebar (and logged a WARN per
                // file on every open). Instead, discard the rest of this
                // one overlong line in bounded chunks -- preserving the
                // anti-OOM guard, since we never buffer the tail --
                // and keep scanning: the envelope lands on a later,
                // normal-sized line.
                log::debug!(
                    target: "splitlane_app::claude_sessions",
                    "skipped an oversized (>{} B) line in {}; continuing scan for the envelope",
                    MAX_LINE_BYTES,
                    path.display(),
                );
                loop {
                    let chunk = match reader.fill_buf() {
                        Ok(b) => b,
                        Err(_) => return None,
                    };
                    if chunk.is_empty() {
                        return None; // EOF mid-line: nothing more to find.
                    }
                    if let Some(nl) = chunk.iter().position(|&b| b == b'\n') {
                        reader.consume(nl + 1);
                        break;
                    }
                    let consumed = chunk.len();
                    reader.consume(consumed);
                }
                continue;
            }
            // EOF after exactly MAX_LINE_BYTES: `buf` is a complete final
            // record - fall through to the normal parse below.
        }
        let trimmed = buf.trim_end();
        if !trimmed.starts_with('{') {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if envelope.is_none()
            && value
                .get("cwd")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
            && let Ok(parsed) = serde_json::from_value::<FirstLineEnvelope>(value.clone())
            && !parsed.cwd.is_empty()
        {
            // session_id lands in `claude --resume <id>`, so hold it to the
            // strict `^[A-Za-z0-9_-]+$` allow-list (Claude ids are UUIDs):
            // rejects a `\r`/`\n` that would submit injected text and a
            // `;`/space that would chain a second shell command. cwd lands in
            // display chrome today but a future `cd <cwd>` prefix would inherit
            // the gap; a path legitimately carries `/` + spaces, so keep the
            // control-char guard for it. Guard both at the gate, not the
            // consumer.
            if !crate::agent_sessions::is_valid_session_id(&parsed.session_id)
                || parsed.cwd.chars().any(|c| c.is_control())
            {
                log::warn!(
                    "claude_sessions: dropped {} -- envelope carries an invalid session_id or control chars in cwd",
                    path.display(),
                );
                continue;
            }
            envelope = Some(parsed);
        }

        match value.get("type").and_then(|v| v.as_str()) {
            Some("ai-title") => {
                if let Some(title) = value.get("aiTitle").and_then(|v| v.as_str())
                    && let Some(cleaned) = clean_session_label(title, LABEL_MAX_CHARS)
                {
                    ai_title = Some(cleaned);
                    // Title-only path: stop as soon as we have envelope + title.
                    // Attribution path: keep walking to aggregate usage/model.
                    if envelope.is_some() && !scan_usage {
                        break;
                    }
                }
            }
            Some("user") if user_fallback.is_none() => {
                if let Some(text) = extract_user_content(&value)
                    && let Some(cleaned) = clean_user_message(&text)
                {
                    user_fallback = Some(cleaned);
                }
            }
            // Assistant turns carry `message.model` + `message.usage`.
            // Aggregate usage across turns; keep the most recent non-empty model
            // (overwrite - a session that switched models reports the last one,
            // which is the most representative for a single-figure estimate).
            //
            // **A turn is counted once per `message.id`, not once per line.**
            // Claude Code writes one API message as one line per content block -
            // `thinking`, `text`, `tool_use` each get their own record, each with
            // its own `uuid` - and every one of them repeats the FULL
            // `message.usage` of the whole message. Folding per line therefore
            // multiplies a turn by its block count, which is not a rounding
            // error: measured over this machine's 60 transcripts for one project
            // (26 222 assistant lines, 16 151 distinct ids) the naive sum was
            // **1.63x** the real one, and 1.81x on the largest single file. That
            // number was on screen, in the diff column's cost estimate.
            //
            // Four things that would make this wrong were checked over the same
            // corpus and none of them happened: no line lacked `message.id`; the
            // `usage` of the lines sharing an id was byte-identical, so taking
            // the first is as good as the last; no id appeared in two files, so
            // `--resume` and forking do not re-emit history under the same ids;
            // and no assistant line was a `isSidechain` subagent turn. A line
            // with an API error carries a zeroed usage, which `is_empty` already
            // drops. Measured against Claude Code 2.1.263.
            Some("assistant") if scan_usage => {
                if let Some(message) = value.get("message") {
                    if let Some(m) = message.get("model").and_then(|v| v.as_str())
                        && !m.is_empty()
                    {
                        model = Some(m.to_string());
                    }
                    if let Some(u) = message.get("usage") {
                        let turn = AssistantUsage {
                            input: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                            output: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                            cache_read: u
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            cache_creation: u
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                        };
                        // An id we have already folded is this same message's
                        // next content block. No id at all is a shape we have
                        // never seen: count it, because dropping a turn we
                        // cannot identify would understate a real bill, and say
                        // so after the scan.
                        let first_time = match message.get("id").and_then(|v| v.as_str()) {
                            Some(id) if !id.is_empty() => counted_messages.insert(id.to_string()),
                            _ => {
                                usage_lines_without_id += 1;
                                true
                            }
                        };
                        if first_time && !turn.is_empty() {
                            usage.add(&turn);
                            saw_usage = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if usage_lines_without_id > 0 {
        log::warn!(
            target: "splitlane_app::claude_sessions",
            "{} assistant line(s) in {} carried usage with no message.id; \
             the per-message dedup could not run on them and the cost estimate \
             for this session may be inflated",
            usage_lines_without_id,
            path.display(),
        );
    }

    let envelope = envelope?;
    // The title for display, the opening prompt for search. Both, because an
    // agent writes its title once from the first prompt and never again, so a
    // session whose subject has moved on is findable by neither its current
    // name nor - once the title has replaced it - the words the person typed.
    let summary = ai_title.or_else(|| user_fallback.clone());
    let first_prompt = user_fallback;

    Some(SessionMeta {
        agent: SessionAgent::Claude,
        session_id: envelope.session_id,
        last_activity_secs: crate::agent_sessions::last_activity_secs_for_file(
            path,
            &envelope.timestamp,
        ),
        timestamp: envelope.timestamp,
        cwd: envelope.cwd,
        git_branch: envelope.git_branch,
        summary,
        first_prompt,
        model,
        usage: saw_usage.then_some(usage),
    })
}

fn extract_user_content(line: &serde_json::Value) -> Option<String> {
    let content = line.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text")
                && let Some(text) = block.get("text").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn clean_user_message(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(name) = extract_xml_block(trimmed, "command-name") {
        let args = extract_xml_block(trimmed, "command-args").unwrap_or_default();
        let joined = if args.is_empty() {
            name
        } else {
            format!("{name} {args}")
        };
        return clean_session_label(&joined, LABEL_MAX_CHARS);
    }

    clean_session_label(trimmed, LABEL_MAX_CHARS)
}

fn extract_xml_block(haystack: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = haystack.find(&open)? + open.len();
    let end = haystack[start..].find(&close)? + start;
    Some(haystack[start..end].trim().to_string())
}

#[cfg(test)]
mod answer_tests {
    use super::*;
    use serde_json::json;

    fn assistant(text_blocks: &[&str]) -> serde_json::Value {
        let content: Vec<_> = text_blocks
            .iter()
            .map(|t| json!({"type": "text", "text": t}))
            .collect();
        json!({"type": "assistant", "message": {"model": "claude-opus-5", "content": content}})
    }

    #[test]
    fn a_turns_text_blocks_are_joined_in_order() {
        assert_eq!(
            answer_text_of(&assistant(&["first", "second"])).as_deref(),
            Some("first\n\nsecond")
        );
    }

    /// A subagent writes into the same file and usually finishes last. Copying
    /// its report as "the last answer" would attribute to this session a thing
    /// it did not say.
    #[test]
    fn a_sidechain_turn_is_not_this_sessions_answer() {
        let mut sub = assistant(&["the subagent's report"]);
        sub["isSidechain"] = json!(true);
        assert_eq!(answer_text_of(&sub), None);

        // The flag being absent or false is the normal case and must pass.
        let mut not_sub = assistant(&["the lead's answer"]);
        not_sub["isSidechain"] = json!(false);
        assert_eq!(
            answer_text_of(&not_sub).as_deref(),
            Some("the lead's answer")
        );
    }

    /// An agent that ended on a tool call has not answered yet, so the turn is
    /// skipped and an older one wins. This is why the newest assistant line is
    /// frequently not the answer.
    #[test]
    fn a_turn_with_no_text_is_not_an_answer() {
        let tool_only = json!({
            "type": "assistant",
            "message": {"model": "claude-opus-5", "content": [
                {"type": "tool_use", "id": "t1", "name": "Edit", "input": {}}
            ]}
        });
        assert_eq!(answer_text_of(&tool_only), None);

        // Text blocks that are only whitespace do not rescue it.
        assert_eq!(answer_text_of(&assistant(&["   ", "\n"])), None);
    }

    #[test]
    fn only_assistant_turns_are_answers() {
        let user = json!({"type": "user", "message": {"content": "a question"}});
        assert_eq!(answer_text_of(&user), None);
    }

    #[test]
    fn the_clis_own_interjections_are_not_the_agent_answering() {
        let mut synthetic = assistant(&["[Request interrupted]"]);
        synthetic["message"]["model"] = json!(SYNTHETIC_MODEL);
        assert_eq!(answer_text_of(&synthetic), None);
    }

    /// The tail reader keeps the LAST answer, not the first it finds.
    #[test]
    fn the_tail_reader_returns_the_newest_answer() {
        let dir = std::env::temp_dir().join(format!("splitlane-answer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("t.jsonl");
        let lines = [
            assistant(&["older answer"]),
            json!({"type": "user", "message": {"content": "next question"}}),
            assistant(&["newer answer"]),
            // Still working: must not displace the answer above.
            json!({"type": "assistant", "message": {"model": "claude-opus-5",
                   "content": [{"type": "tool_use", "id": "t", "name": "Read", "input": {}}]}}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, body).expect("write");

        assert_eq!(
            read_last_answer_from_tail(&path).as_deref(),
            Some("newer answer")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_transcript_with_no_answer_yet_reports_none() {
        let dir =
            std::env::temp_dir().join(format!("splitlane-answer-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("t.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .expect("write");
        assert_eq!(read_last_answer_from_tail(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point this at a live agent's transcript and at the **PTY child** of the
    /// pane it is running in, to see the whole detector answer about a real
    /// session. Ignored by default: it needs a machine with an agent running on
    /// it, which CI is not.
    ///
    /// `SPLITLANE_PROBE_PID` is the pane's PTY child - the user's shell - and
    /// not the agent, and not `Thread::agent_pid` either. The agent is resolved
    /// from it by stepping over our own shim; see
    /// [`crate::process_tree::agent_under_pty`] for why neither of the other
    /// two can be asked.
    ///
    /// ```text
    /// SPLITLANE_PROBE_FILE=~/.claude/projects/<slug>/<uuid>.jsonl \
    /// SPLITLANE_PROBE_PID=65597 \
    ///   SPLITLANE_PROBE_SESSION=<forced session uuid> \
    ///   cargo test -p splitlane-app --bin splitlane -- --ignored the_detector_on_a_real_session --nocapture
    /// ```
    #[test]
    #[ignore = "needs a live agent on the machine"]
    fn the_detector_on_a_real_session() {
        let Ok(file) = std::env::var("SPLITLANE_PROBE_FILE") else {
            panic!("set SPLITLANE_PROBE_FILE to a transcript path");
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        let probe = probe_state_from_tail(std::path::Path::new(&file), now).expect("probe");
        let pty_child = std::env::var("SPLITLANE_PROBE_PID")
            .ok()
            .and_then(|p| p.parse::<u32>().ok());
        let snapshot = crate::process_tree::ProcessSnapshot::capture();
        let shim_dir = crate::ai_hooks::extract::ensure_binaries_extracted().ok();
        let agent = match (&snapshot, pty_child, &shim_dir) {
            (Some(snapshot), Some(pty_child), Some(shim_dir)) => {
                crate::process_tree::agent_under_pty(snapshot, pty_child, shim_dir)
            }
            _ => None,
        };
        // The worker question needs a baseline from a finished turn, so a
        // one-shot reading cannot answer it. **Start this while the agent is
        // idle**: the resting subtree becomes the baseline, and anything that
        // appears over the next few seconds is what the pass would call a
        // worker. Reporting a hardcoded `Unknown` here instead would have made
        // the classification below print the same answer whatever the machine
        // was doing, which is worse than useless in a diagnostic.
        let baseline = match (&snapshot, pty_child) {
            (Some(snapshot), Some(pty_child)) => {
                crate::process_tree::WorkerBaseline::take(snapshot, pty_child)
            }
            _ => None,
        };
        println!("baseline (resting subtree): {baseline:?}");
        std::thread::sleep(std::time::Duration::from_secs(5));
        let later = crate::process_tree::ProcessSnapshot::capture();
        let worker = match (&later, pty_child) {
            (Some(later), Some(pty_child)) => {
                crate::process_tree::worker_against(later, baseline.as_ref(), pty_child)
            }
            _ => crate::agent_state::Worker::Unknown,
        };
        println!("open calls: {:?}", probe.open_calls);
        println!("open turn: {:?}", probe.open_turn);
        println!(
            "errored: {} incomplete: {}",
            probe.errored, probe.incomplete
        );
        println!("shim dir: {shim_dir:?}");
        println!("pty child: {pty_child:?} -> agent: {agent:?}");
        println!("worker: {worker:?}");
        let agent_process = if agent.is_some() {
            crate::agent_state::AgentProcess::Found
        } else {
            crate::agent_state::AgentProcess::NotSeen
        };
        println!("agent process: {agent_process:?}");
        println!(
            "=> rule says {:?}",
            crate::agent_state::classify(&probe, worker, agent_process)
        );

        // The first source, printed beside the rule so the two can be compared
        // on the same session at the same instant. This is the whole reason the
        // diagnostic is worth keeping: the interesting cases are the ones where
        // they disagree, and on Claude Code 2.1.246 there is a big one - a
        // `Bash` permission ask reaches the status file and never reaches the
        // transcript, so the rule reports work while the file reports a person
        // being kept waiting.
        //
        // `SPLITLANE_PROBE_SESSION` is the surface's forced session uuid; the
        // file is refused without it, exactly as in the pass.
        let session = std::env::var("SPLITLANE_PROBE_SESSION").ok();
        match agent {
            Some(pid) => {
                println!(
                    "pid file: {:?}",
                    crate::claude_pid_state::pid_file_path(pid)
                );
                println!(
                    "=> status file says {:?}",
                    crate::claude_pid_state::state_for(pid, session.as_deref())
                );
            }
            None => {
                println!("=> status file not asked: no agent pid resolved under this PTY child")
            }
        }
    }

    /// Build a transcript out of raw lines and probe it, which is also what
    /// keeps the probe honest about the file it actually reads.
    fn probe_lines(lines: &[&str], now_secs: i64) -> TranscriptProbe {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").expect("write");
        probe_state_from_tail(&path, now_secs).expect("probe")
    }

    fn at(secs: &str) -> String {
        format!("2026-08-25T10:00:{secs}.000Z")
    }

    /// `stop_reason` is not a two-value field, and reading everything but
    /// `tool_use` as the end of the turn made `max_tokens` and `pause_turn` -
    /// both of which mean the turn is carrying on - into `idle` on a working
    /// session. Only a measured word closes; the rest leaves the answer
    /// standing.
    #[test]
    fn only_a_measured_stop_reason_ends_a_turn() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        for carries_on in ["max_tokens", "pause_turn", "a_word_from_the_next_release"] {
            let probe = probe_lines(
                &[
                    &format!(
                        r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"go"}}}}"#,
                        at("05")
                    ),
                    &format!(
                        r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","stop_reason":"{carries_on}","content":[{{"type":"text","text":"partial"}}]}}}}"#,
                        at("10")
                    ),
                ],
                now,
            );
            assert!(
                probe.open_turn.is_some(),
                "{carries_on} must not end the turn"
            );
        }
    }

    /// A person's own prompt can carry the interrupt text verbatim - pasting a
    /// transcript excerpt and asking why it stopped there is an ordinary thing
    /// to ask an agent. Matched by equality against one block, the prompt is a
    /// prompt; matched as a substring of the line, it silenced the turn
    /// answering it.
    #[test]
    fn a_prompt_quoting_an_interrupt_is_still_a_prompt() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":[{{"type":"text","text":"why did it stop at [Request interrupted by user] here?"}}]}}}}"#,
                at("10")
            )],
            now,
        );
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 40);
    }

    /// `/clear` is the record that begins every new transcript, and it starts
    /// no model turn: the session sits waiting for the person. Read as a prompt
    /// it pinned a spinner on every freshly cleared session for the whole trust
    /// horizon.
    #[test]
    fn a_local_command_starts_no_turn() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"<command-message>clear</command-message>\n<command-name>/clear</command-name>"}}}}"#,
                at("10")
            )],
            now,
        );
        assert_eq!(probe.open_turn, None);
    }

    /// The CLI writes a caveat record beside the command, carrying a plain
    /// string. Gating on the command tag alone left it opening the very turn
    /// the command had just declined to - found on a real transcript truncated
    /// to the moment after a `/clear`.
    #[test]
    fn the_caveat_beside_a_local_command_starts_no_turn_either() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        for tag in ["<local-command-caveat>", "<local-command-stdout>"] {
            let probe = probe_lines(
                &[&format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"{tag}some text"}}}}"#,
                    at("10")
                )],
                now,
            );
            assert_eq!(probe.open_turn, None, "{tag} must start no turn");
        }
    }

    /// And a custom command is still answered by the model, so the assistant
    /// record a second later opens the turn properly. The command record itself
    /// says nothing rather than saying the wrong thing.
    #[test]
    fn a_custom_command_opens_its_turn_at_the_first_answer() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"<command-name>/sf:audit</command-name>"}}}}"#,
                    at("05")
                ),
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","stop_reason":"tool_use","content":[{{"type":"thinking","thinking":"..."}}]}}}}"#,
                    at("10")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 40);
    }

    /// A stamp in the future of our own clock dates the turn to age zero, which
    /// is inside the horizon for ever. An open **call** clamps the same way and
    /// is safe there, because the waiting predicate needs an age past `GRACE`;
    /// a turn has no lower bound, so it is dropped instead.
    #[test]
    fn a_turn_stamped_in_the_future_is_undatable() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("10"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"go"}}}}"#,
                at("40")
            )],
            now,
        );
        assert_eq!(probe.open_turn, None);
    }

    /// A prompt with nothing after it yet. The single commonest false `idle`
    /// there was: the agent has been given something to do, has not called a
    /// tool yet, and the call set is empty.
    #[test]
    fn a_prompt_with_no_answer_yet_leaves_the_turn_open() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":[{{"type":"text","text":"do the thing"}}]}}}}"#,
                at("10")
            )],
            now,
        );
        assert!(probe.open_calls.is_empty());
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 30);
        assert_eq!(
            crate::agent_state::classify(
                &probe,
                crate::agent_state::Worker::Absent,
                crate::agent_state::AgentProcess::Found
            ),
            crate::ai_types::AgentState::Thinking
        );
    }

    /// A prompt written as a bare string rather than a block list. Both shapes
    /// are in the local corpus, and the array-only guard further down this
    /// reader drops this one - which is why the turn is read before it.
    #[test]
    fn a_string_prompt_is_a_prompt_too() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"/review"}}}}"#,
                at("10")
            )],
            now,
        );
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 30);
    }

    /// The gap this whole reading exists for: the model generating with no call
    /// in flight. `stop_reason` says the turn continues, so the file settles it.
    #[test]
    fn an_assistant_record_that_is_not_the_end_of_the_turn_keeps_it_open() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"go"}}}}"#,
                    at("05")
                ),
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","stop_reason":"tool_use","content":[{{"type":"thinking","thinking":"..."}}]}}}}"#,
                    at("10")
                ),
            ],
            now,
        );
        assert!(probe.open_calls.is_empty());
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 40);
    }

    /// And `end_turn` ends it. Without this half the dot would never go out.
    #[test]
    fn end_turn_closes_the_turn() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"go"}}}}"#,
                    at("05")
                ),
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","stop_reason":"end_turn","content":[{{"type":"text","text":"done"}}]}}}}"#,
                    at("10")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_turn, None);
        assert_eq!(
            crate::agent_state::classify(
                &probe,
                crate::agent_state::Worker::Absent,
                crate::agent_state::AgentProcess::Found
            ),
            crate::ai_types::AgentState::Finished
        );
    }

    /// An interrupt arrives as a `user` record and would otherwise read as a
    /// prompt - leaving a spinner on the one session a person has just stopped
    /// by hand, for as long as the process lives.
    #[test]
    fn an_interrupt_closes_the_turn_rather_than_opening_one() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        for mark in [
            "[Request interrupted by user]",
            "[Request interrupted by user for tool use]",
        ] {
            let probe = probe_lines(
                &[
                    &format!(
                        r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","stop_reason":"tool_use","content":[{{"type":"text","text":"working"}}]}}}}"#,
                        at("05")
                    ),
                    &format!(
                        r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":[{{"type":"text","text":"{mark}"}}]}}}}"#,
                        at("10")
                    ),
                ],
                now,
            );
            assert_eq!(probe.open_turn, None, "{mark} must end the turn");
        }
    }

    /// A subagent's records sit in the parent's own transcript, and its turns
    /// begin and end inside the parent's. Reading its `end_turn` would report
    /// the session finished while it is still working.
    #[test]
    fn a_subagents_end_turn_does_not_end_the_parents() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"go"}}}}"#,
                    at("05")
                ),
                &format!(
                    r#"{{"type":"assistant","isSidechain":true,"timestamp":"{}","message":{{"role":"assistant","stop_reason":"end_turn","content":[{{"type":"text","text":"sub done"}}]}}}}"#,
                    at("10")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 45);
    }

    /// The CLI's own interjections are written as assistant records under a
    /// synthetic model name. They are not the agent answering, and the
    /// transcript's answer reader skips them for the same reason.
    #[test]
    fn the_clis_own_interjection_does_not_end_a_turn() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":"go"}}}}"#,
                    at("05")
                ),
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","model":"{SYNTHETIC_MODEL}","stop_reason":"end_turn","content":[{{"type":"text","text":"note"}}]}}}}"#,
                    at("10")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 45);
    }

    /// The state the whole probe exists to see: a call written to disk with no
    /// result after it. Measured to be how a permission wait looks.
    #[test]
    fn a_call_without_a_result_is_open() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Write","input":{{}}}}]}}}}"#,
                at("10")
            )],
            now,
        );
        assert_eq!(probe.open_calls.len(), 1);
        assert_eq!(probe.open_calls[0].name, "Write");
        assert_eq!(probe.open_calls[0].age.expect("age").as_secs(), 30);
        assert!(!probe.errored);
    }

    /// The real shape of a background agent, taken from the local corpus: the
    /// `Agent` call is answered in the same second, and the work runs on.
    ///
    /// Before this the pairing closed with that answer and the session read as
    /// finished for the whole run - the rail saying `idle` beside a pane whose
    /// own footer named a running agent.
    #[test]
    fn a_background_agent_stays_open_after_its_call_is_answered() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Agent","input":{{}}}}]}}}}"#,
                    at("10")
                ),
                &format!(
                    r#"{{"type":"user","timestamp":"{}","toolUseResult":{{"isAsync":true,"status":"async_launched","agentId":"ab862e6dc069d6f9d"}},"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":[{{"type":"text","text":"Async agent launched successfully."}}]}}]}}}}"#,
                    at("10")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_calls.len(), 1);
        assert_eq!(probe.open_calls[0].name, "background agent");
        assert_eq!(probe.open_calls[0].age.expect("age").as_secs(), 40);
        // Opaque is what makes this safe: it can say the session is working and
        // it can never say a person is being kept waiting.
        assert_eq!(probe.open_calls[0].execution, ToolExecution::Opaque);
        assert_eq!(
            crate::agent_state::classify(
                &probe,
                crate::agent_state::Worker::Absent,
                crate::agent_state::AgentProcess::Found
            ),
            crate::ai_types::AgentState::Thinking
        );
    }

    /// The other end of the span. All four end states the corpus contains
    /// (`completed`, `killed`, `failed`, `stopped`) are written the same way,
    /// so the close does not depend on the agent having succeeded.
    #[test]
    fn a_task_notification_closes_its_background_agent() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        for status in ["completed", "killed", "failed", "stopped"] {
            let probe = probe_lines(
                &[
                    &format!(
                        r#"{{"type":"user","timestamp":"{}","toolUseResult":{{"isAsync":true,"status":"async_launched","agentId":"a053d575490c765fc"}},"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"launched"}}]}}}}"#,
                        at("10")
                    ),
                    // The `queue-operation` shape: no `message` at all, which is
                    // why this is read off the line's text.
                    &format!(
                        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"{}","content":"<task-notification>\n<task-id>a053d575490c765fc</task-id>\n<status>{status}</status>\n</task-notification>"}}"#,
                        at("30")
                    ),
                ],
                now,
            );
            assert!(
                probe.open_calls.is_empty(),
                "{status} left the agent open: {:?}",
                probe.open_calls
            );
        }
    }

    /// One notification record can name several agents, and closing the first
    /// must not stop the scan.
    #[test]
    fn a_notification_closes_every_agent_it_names() {
        assert_eq!(
            task_notification_ids(
                "<task-notification><task-id>one</task-id></task-notification>\
                 <task-notification><task-id>two</task-id></task-notification>"
            ),
            vec!["one", "two"]
        );
        // A tag left unclosed by a cap-truncated line yields what it stated and
        // stops, rather than reading the rest of the file as an id.
        assert!(task_notification_ids("<task-id>unterminated").is_empty());
    }

    /// An agent id and a `tool_use` id live in one map and must not meet.
    #[test]
    fn an_agent_id_cannot_close_a_tool_call() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("50"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"shared","name":"Write","input":{{}}}}]}}}}"#,
                    at("10")
                ),
                &format!(
                    r#"{{"type":"queue-operation","timestamp":"{}","content":"<task-notification><task-id>shared</task-id></task-notification>"}}"#,
                    at("20")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_calls.len(), 1);
        assert_eq!(probe.open_calls[0].name, "Write");
    }

    /// A result closes its own call and nothing else.
    #[test]
    fn a_result_closes_only_its_own_call() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{}}}},{{"type":"tool_use","id":"t2","name":"Edit","input":{{}}}}]}}}}"#,
                    at("10")
                ),
                &format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok"}}]}}}}"#,
                    at("12")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_calls.len(), 1);
        assert_eq!(probe.open_calls[0].name, "Edit");
    }

    /// File order is chronological, so the first entry is the one that has been
    /// waiting longest - which is the one the rule reads.
    #[test]
    fn open_calls_come_back_oldest_first() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{}}}}]}}}}"#,
                    at("05")
                ),
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t2","name":"Read","input":{{}}}}]}}}}"#,
                    at("30")
                ),
            ],
            now,
        );
        assert_eq!(probe.open_calls[0].name, "Edit");
        assert_eq!(probe.open_calls[0].age.expect("age").as_secs(), 35);
    }

    /// A stated failure only counts while it is still the newest thing said.
    /// A turn that carried on afterwards is not a failed session.
    #[test]
    fn a_failure_is_only_current_until_the_session_speaks_again() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let err = format!(
            r#"{{"type":"system","subtype":"api_error","timestamp":"{}"}}"#,
            at("10")
        );
        assert!(probe_lines(&[&err], now).errored);

        let after = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"text","text":"recovered"}}]}}}}"#,
            at("20")
        );
        assert!(!probe_lines(&[&err, &after], now).errored);
    }

    /// A result whose call began before the window is not an orphan to worry
    /// about - it simply is not an open call.
    #[test]
    fn a_result_with_no_call_in_the_window_is_ignored() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"gone","content":"ok"}}]}}}}"#,
                at("10")
            )],
            now,
        );
        assert!(probe.open_calls.is_empty());
    }

    /// The defect this flag exists for, in the shape it actually occurs: a
    /// result too long for the per-line cap. The call it closes must not be
    /// left looking open.
    #[test]
    fn an_oversize_result_marks_the_window_incomplete() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let huge = "x".repeat((MAX_LINE_BYTES as usize) + 4096);
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Read","input":{{}}}}]}}}}"#,
                    at("10")
                ),
                &format!(
                    r#"{{"type":"user","timestamp":"{}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"{huge}"}}]}}}}"#,
                    at("12")
                ),
            ],
            now,
        );
        assert!(
            probe.incomplete,
            "a capped read leaves a fragment, and the probe has to admit it"
        );
    }

    /// A probe that lands while the agent is writing its next record must not
    /// call the window incomplete. Live sessions are exactly the ones being
    /// probed, so treating a half-written last line as a gap would take the
    /// file source out of play almost always - and look like it never worked
    /// rather than like a bug.
    #[test]
    fn a_half_written_last_line_is_not_a_gap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        let good = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{}}}}]}}}}"#,
            at("10")
        );
        // No trailing newline: the agent is mid-write, which is what a probe of
        // a working session normally catches.
        std::fs::write(&path, format!("{good}\n{{\"type\":\"assis")).expect("write");
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_state_from_tail(&path, now).expect("probe");
        assert!(
            !probe.incomplete,
            "a mid-write tail is not a missing record"
        );
        assert_eq!(probe.open_calls.len(), 1);
    }

    /// A blank line is not an unreadable record - a file ending in one must not
    /// make every session look untrustworthy.
    #[test]
    fn a_blank_line_is_not_an_unreadable_record() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("40"));
        let probe = probe_lines(
            &[
                &format!(
                    r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{}}}}]}}}}"#,
                    at("10")
                ),
                "",
            ],
            now,
        );
        assert!(!probe.incomplete);
        assert_eq!(probe.open_calls.len(), 1);
    }

    /// A record stamped in the future must not wrap into "waiting forever".
    #[test]
    fn a_clock_that_moved_gives_age_zero() {
        let now = crate::agent_sessions::last_activity_secs_from_iso(&at("00"));
        let probe = probe_lines(
            &[&format!(
                r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{}}}}]}}}}"#,
                at("30")
            )],
            now,
        );
        assert_eq!(probe.open_calls[0].age.expect("age").as_secs(), 0);
    }

    #[test]
    fn slug_unix_path() {
        assert_eq!(slug_for_cwd("/home/alice/myapp"), "-home-alice-myapp");
    }

    #[test]
    fn slug_replaces_spaces() {
        // Spaces are non-alphanumeric, so they become `-` like every other
        // separator (real example: `C:\Program Files\Splitlane` →
        // `C--Program-Files-Splitlane`).
        assert_eq!(
            slug_for_cwd("/home/alice/my project"),
            "-home-alice-my-project"
        );
    }

    #[test]
    fn slug_replaces_dots() {
        // A leading-dot segment is NOT preserved: the `.` becomes `-`, so a
        // dotfile dir produces a double dash (`/home/arthur/.claude` →
        // `-home-arthur--claude`, the dir Claude Code writes on Linux).
        assert_eq!(slug_for_cwd("/home/alice/.config"), "-home-alice--config");
    }

    #[test]
    fn slug_windows_path_replaces_drive_colon() {
        // Regression guard: the drive `:` MUST become `-`. The old encoder left
        // it as `C:-Users-alice-myapp`, which never matched the on-disk
        // `C--Users-alice-myapp` and emptied the sidebar on Windows.
        assert_eq!(
            slug_for_cwd("C:\\Users\\alice\\myapp"),
            "C--Users-alice-myapp"
        );
    }

    #[test]
    fn slug_matches_real_windows_project_dir() {
        // Verified against a real install: Claude Code stores `C:\dev\splitlane`
        // sessions under `~/.claude/projects/C--dev-splitlane/`.
        assert_eq!(slug_for_cwd("C:\\dev\\splitlane"), "C--dev-splitlane");
    }

    #[test]
    fn trailing_separator_resolves_to_the_same_project_dir() {
        // Claude's own cwd never carries a trailing separator, so a stored
        // `thread.cwd` that does must still find the directory that exists.
        // Getting this wrong makes `session_file_exists` report "no session"
        // for a live one, which re-sends `--session-id` and reproduces
        // `Session ID <uuid> is already in use`.
        assert_eq!(
            project_dir_for_cwd("/home/alice/myapp/"),
            project_dir_for_cwd("/home/alice/myapp")
        );
        assert_eq!(
            project_dir_for_cwd("/home/alice/myapp///"),
            project_dir_for_cwd("/home/alice/myapp")
        );
        assert_eq!(
            project_dir_for_cwd("C:\\dev\\splitlane\\"),
            project_dir_for_cwd("C:\\dev\\splitlane")
        );
    }

    #[test]
    fn bare_roots_keep_their_slug() {
        // All-separator paths are not normalized: trimming would change the
        // slug rather than canonicalize it.
        assert_eq!(normalize_cwd_for_slug("/"), "/");
        assert_eq!(normalize_cwd_for_slug("C:\\"), "C:\\");
        assert_eq!(slug_for_cwd(normalize_cwd_for_slug("/")), "-");
        assert_eq!(slug_for_cwd(normalize_cwd_for_slug("C:\\")), "C--");
    }

    #[test]
    fn session_file_exists_rejects_ids_outside_the_allow_list() {
        // The allow-list gate runs before any path join, so no id can escape
        // the project dir or reach the filesystem as a traversal.
        for hostile in ["../../etc/passwd", "a/b", "a\\b", "with space", ".hidden"] {
            assert!(
                !session_file_exists("/home/alice/myapp", hostile),
                "hostile id {hostile:?} must be rejected before the path join"
            );
        }
    }

    #[test]
    fn slug_root() {
        assert_eq!(slug_for_cwd("/"), "-");
    }

    #[test]
    fn read_session_meta_skips_leading_metadata_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .join("aaaaaaaa-1111-2222-3333-444444444444.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"permission-mode","permissionMode":"default","sessionId":"aaaaaaaa-1111-2222-3333-444444444444"}"#,
                "\n",
                r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{"trackedFileBackups":{}},"isSnapshotUpdate":false}"#,
                "\n",
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"hi"},"uuid":"x","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"aaaaaaaa-1111-2222-3333-444444444444","version":"2.1.119","gitBranch":"main"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Implement feature X","sessionId":"aaaaaaaa-1111-2222-3333-444444444444"}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        let meta = read_session_meta(&path).expect("envelope extracted");
        assert_eq!(meta.agent, SessionAgent::Claude);
        assert_eq!(meta.session_id, "aaaaaaaa-1111-2222-3333-444444444444");
        assert_eq!(meta.cwd, "/tmp/proj");
        assert_eq!(meta.timestamp, "2026-04-26T13:38:41.095Z");
        assert_eq!(meta.git_branch, "main");
        assert_eq!(meta.summary.as_deref(), Some("Implement feature X"));
    }

    /// Diagnostic, not a gate: how often does a real transcript end up with a
    /// title and no opening prompt to search by?
    ///
    /// The corpus measurement behind `first_prompt` counted `type:"user"`
    /// records, but what the reader keeps is the first user turn that
    /// *survives cleaning* - a command wrapper or an empty turn does not. A
    /// cross-vendor pass pointed out that the two are not the same question,
    /// and it was right that the first measurement did not answer the second.
    ///
    /// **Measured 29 August: 3 of 373 titled sessions have no prompt**, and
    /// the cause is neither cleaning nor the scan budget. Those three open
    /// with a pasted screenshot, so the first user line is 82 925 bytes
    /// against the 64 KiB [`crate::limits::MAX_LINE_BYTES`] cap - truncated,
    /// unparseable, skipped. That cap is deliberate: it stops a malicious
    /// single-line JSONL from allocating on a background thread. So this is a
    /// known bound rather than a bug to route around here, and raising it is
    /// a maintainer's call.
    ///
    /// Run with:
    ///   cargo test -p splitlane-app first_prompt_coverage -- --ignored --nocapture
    #[test]
    #[ignore = "reads the developer's own ~/.claude corpus"]
    fn first_prompt_coverage_on_the_real_corpus() {
        let Some(root) = dirs::home_dir().map(|h| h.join(".claude/projects")) else {
            return;
        };
        let mut total = 0usize;
        let mut titled = 0usize;
        let mut titled_without_prompt = 0usize;
        let mut without_prompt: Vec<String> = Vec::new();
        let Ok(projects) = std::fs::read_dir(&root) else {
            return;
        };
        for project in projects.flatten() {
            let Ok(files) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(meta) = read_session_meta(&path) else {
                    continue;
                };
                total += 1;
                if meta.summary.is_some() {
                    titled += 1;
                    if meta.first_prompt.is_none() {
                        titled_without_prompt += 1;
                        if without_prompt.len() < 5 {
                            without_prompt.push(format!(
                                "{} -> {:?}",
                                path.file_name().unwrap_or_default().to_string_lossy(),
                                meta.summary
                            ));
                        }
                    }
                }
            }
        }
        println!("sessions read           : {total}");
        println!("with a summary          : {titled}");
        println!("summary but no prompt   : {titled_without_prompt}");
        for row in &without_prompt {
            println!("  {row}");
        }
    }

    /// The title is for display and the opening prompt is for search, so the
    /// prompt survives even when a title exists to replace it. Without this a
    /// session is findable only by a name its agent wrote once, near the
    /// start, and never revised - which is how a live session with a moved-on
    /// subject stopped being findable by anything but its uuid.
    #[test]
    fn the_first_prompt_is_kept_as_a_search_key_under_an_ai_title() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("searchable.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"analyse the soak harness numbers"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"s"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Review research findings","sessionId":"s"}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        let meta = read_session_meta(&path).expect("meta");
        assert_eq!(
            meta.summary.as_deref(),
            Some("Review research findings"),
            "the agent's own title is still what is shown"
        );
        assert_eq!(
            meta.first_prompt.as_deref(),
            Some("analyse the soak harness numbers"),
            "and the words the person typed are still there to search by"
        );
    }

    #[test]
    fn ai_title_wins_over_first_user_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ordering.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"first user message body"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"s"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Fix the thing","sessionId":"s"}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        let meta = read_session_meta(&path).expect("meta");
        assert_eq!(meta.summary.as_deref(), Some("Fix the thing"));
    }

    #[test]
    fn ai_title_uses_same_label_normalization_as_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("title-normalized.jsonl");
        let long_title = format!("Fix\n\t{}{}", "a".repeat(100), "\u{1b}");
        std::fs::write(
            &path,
            format!(
                r#"{{"parentUuid":null,"type":"user","message":{{"role":"user","content":"first user message body"}},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"s"}}
{{"type":"ai-title","aiTitle":{}}}
"#,
                serde_json::to_string(&long_title).expect("json string")
            ),
        )
        .expect("write fixture");

        let meta = read_session_meta(&path).expect("meta");
        let summary = meta.summary.as_deref().expect("summary");
        assert!(!summary.contains('\n'));
        assert!(!summary.contains('\t'));
        assert!(summary.chars().count() <= LABEL_MAX_CHARS + 1);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn falls_back_to_first_user_message_when_no_ai_title() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"Refactor the auth flow"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"s"}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        let meta = read_session_meta(&path).expect("meta");
        assert_eq!(meta.summary.as_deref(), Some("Refactor the auth flow"));
    }

    #[test]
    fn cleans_slash_command_boilerplate_in_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("slash.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"<command-message>implement-story</command-message>\n<command-name>/implement-story</command-name>\n<command-args>@docs/spec-x.md story-1</command-args>"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"s"}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        let meta = read_session_meta(&path).expect("meta");
        assert_eq!(
            meta.summary.as_deref(),
            Some("/implement-story @docs/spec-x.md story-1")
        );
    }

    #[test]
    fn usage_scan_aggregates_across_assistant_turns_and_captures_model() {
        // The deeper scan (scan_usage=true) walks past the title
        // break, sums `message.usage` across assistant turns, and captures the
        // model. The title-only scan (false) leaves both None.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("usage.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"550e8400-e29b-41d4-a716-446655440000","gitBranch":"main"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-opus-4-8-20260101","usage":{"input_tokens":100,"output_tokens":40,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Some title"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-opus-4-8-20260101","usage":{"input_tokens":200,"output_tokens":60,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        // Title-only path: no model/usage.
        let title_only = read_session_meta_inner(&path, false).expect("meta");
        assert!(title_only.model.is_none());
        assert!(title_only.usage.is_none());
        assert_eq!(title_only.summary.as_deref(), Some("Some title"));

        // Attribution path: aggregated usage + model.
        let with_usage = read_session_meta_inner(&path, true).expect("meta");
        assert_eq!(
            with_usage.model.as_deref(),
            Some("claude-opus-4-8-20260101")
        );
        let usage = with_usage.usage.expect("usage aggregated");
        assert_eq!(usage.input, 300);
        assert_eq!(usage.output, 100);
        assert_eq!(usage.cache_read, 10);
        assert_eq!(usage.cache_creation, 5);
    }

    #[test]
    fn usage_counts_a_message_once_however_many_lines_it_spans() {
        // Claude Code writes one API message as one line per content block -
        // thinking, text, tool_use - and every line repeats the full
        // `message.usage`. Counting per line inflated the cost estimate by 1.63x
        // over this machine's corpus. The fixture is that exact shape: one
        // message across three lines, then a second message on one line.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dedup.jsonl");
        let block = |uuid: &str, id: &str, out: u64, read: u64| {
            format!(
                r#"{{"type":"assistant","uuid":"{uuid}","message":{{"id":"{id}","model":"claude-opus-5","usage":{{"input_tokens":2,"output_tokens":{out},"cache_read_input_tokens":{read},"cache_creation_input_tokens":7}}}}}}"#
            )
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n{}\n{}\n",
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"550e8400-e29b-41d4-a716-446655440000","gitBranch":"main"}"#,
                block("a1", "msg_one", 500, 1000),
                block("a2", "msg_one", 500, 1000),
                block("a3", "msg_one", 500, 1000),
                block("b1", "msg_two", 300, 2000),
            ),
        )
        .expect("write fixture");

        let meta = read_session_meta_inner(&path, true).expect("meta");
        let usage = meta.usage.expect("usage aggregated");
        // Two messages, not four lines: 500+300 out, 1000+2000 read, 2+2 in,
        // 7+7 creation. Naive per-line folding would give 1800 / 5000 / 8 / 28.
        assert_eq!(usage.output, 800, "a turn is counted once per message.id");
        assert_eq!(usage.cache_read, 3000);
        assert_eq!(usage.input, 4);
        assert_eq!(usage.cache_creation, 14);
    }

    #[test]
    fn usage_without_a_message_id_is_still_counted() {
        // The dedup key is `message.id`. A line that has usage and no id is a
        // shape no measured transcript contains, and the fallback is deliberate:
        // dropping a turn we cannot identify would understate a real bill, which
        // is the worse of the two errors. (It is also logged - see the scan.)
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("no-id.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"550e8400-e29b-41d4-a716-446655440000","gitBranch":"main"}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-opus-5","usage":{"input_tokens":100,"output_tokens":40,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-opus-5","usage":{"input_tokens":200,"output_tokens":60,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        let usage = read_session_meta_inner(&path, true)
            .expect("meta")
            .usage
            .expect("usage aggregated");
        assert_eq!(usage.input, 300);
        assert_eq!(usage.output, 100);
    }

    #[test]
    fn the_tail_costs_a_message_once_however_many_lines_it_spans() {
        // The same shape the deeper scan guards, on the other reader: one API
        // message written as three content-block lines, each repeating the full
        // usage, then a second message on one line.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tail.jsonl");
        let block = |uuid: &str, id: &str, stamp: &str| {
            format!(
                r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"{stamp}","message":{{"id":"{id}","model":"claude-opus-4-1-20250805","usage":{{"input_tokens":0,"output_tokens":1000000,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
            )
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n{}\n",
                block("a1", "msg_one", "2026-09-06T10:00:00.000Z"),
                block("a2", "msg_one", "2026-09-06T10:00:01.000Z"),
                block("a3", "msg_one", "2026-09-06T10:00:02.000Z"),
                block("b1", "msg_two", "2026-09-06T10:05:00.000Z"),
            ),
        )
        .expect("write fixture");

        let facts = read_tail_facts(&path).expect("facts");
        assert_eq!(
            facts.spend.len(),
            2,
            "two messages, not four lines: {:?}",
            facts.spend
        );
        // A million output tokens at Opus pricing is a round number, so the
        // dedup is visible in the total rather than only in the count.
        let total: f64 = facts.spend.iter().map(|s| s.dollars).sum();
        let one = facts.spend[0].dollars;
        assert!(one > 0.0, "the fixture's model must be priced");
        assert!(
            (total - one * 2.0).abs() < 1e-9,
            "counting lines would have charged four: {total}"
        );
        assert_eq!(facts.model.as_deref(), Some("claude-opus-4-1-20250805"));
    }

    #[test]
    fn a_model_the_table_does_not_know_costs_nothing_rather_than_a_guess() {
        // The badge still names it - that is a fact about the session. The bill
        // does not, because we have no rate for it, and an invented one would
        // be worse than an absent one.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unpriced.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"assistant","uuid":"a1","timestamp":"2026-09-06T10:00:00.000Z","message":{"id":"m1","model":"some-new-model-9","usage":{"input_tokens":100,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
                "\n",
            ),
        )
        .expect("write fixture");

        let facts = read_tail_facts(&path).expect("facts");
        assert!(facts.spend.is_empty(), "an unpriced model is not costed");
        assert_eq!(facts.model.as_deref(), Some("some-new-model-9"));
    }

    #[test]
    fn only_an_automatic_compaction_measures_the_ceiling() {
        // A manual compaction happens wherever the person asked for it.
        // Measured over this machine's corpus: 28 manual ones fired between
        // 116 712 and 801 793 tokens while all 8 automatic ones fired between
        // 919 686 and 1 002 336. Taking a manual one as the ceiling would peg
        // the meter to whatever size somebody typed `/compact` at, and every
        // later turn would read past 100%.
        let dir = tempfile::tempdir().expect("tempdir");
        let manual = dir.path().join("manual.jsonl");
        std::fs::write(
            &manual,
            concat!(
                r#"{"type":"system","compactMetadata":{"trigger":"manual","preTokens":116712,"postTokens":9000}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert_eq!(
            read_tail_facts(&manual).expect("facts").auto_compacted_at,
            None,
            "a manual compaction says nothing about the ceiling"
        );

        let auto = dir.path().join("auto.jsonl");
        std::fs::write(
            &auto,
            concat!(
                r#"{"type":"system","compactMetadata":{"trigger":"manual","preTokens":116712,"postTokens":9000}}"#,
                "\n",
                r#"{"type":"system","compactMetadata":{"trigger":"auto","preTokens":1000132,"postTokens":9399}}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert_eq!(
            read_tail_facts(&auto).expect("facts").auto_compacted_at,
            Some(1_000_132)
        );
    }

    #[test]
    fn the_context_size_is_the_newest_turns_and_not_a_sum() {
        // It is a level, not a total: the newest turn's own carry is the size
        // now, and adding turns together would report a number that never
        // existed. Every line of a multi-block message repeats it, so the last
        // line wins by overwriting rather than by being deduped.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ctx.jsonl");
        let turn = |uuid: &str, id: &str, read: u64| {
            format!(
                r#"{{"type":"assistant","uuid":"{uuid}","timestamp":"2026-09-06T10:00:00.000Z","message":{{"id":"{id}","model":"claude-opus-4-1-20250805","usage":{{"input_tokens":2,"output_tokens":10,"cache_read_input_tokens":{read},"cache_creation_input_tokens":100}}}}}}"#
            )
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                turn("a1", "m1", 500_000),
                turn("b1", "m2", 600_000),
                turn("b2", "m2", 600_000),
            ),
        )
        .expect("write fixture");

        let facts = read_tail_facts(&path).expect("facts");
        assert_eq!(
            facts.context_tokens,
            Some(600_102),
            "the newest turn's carry, not 500k + 600k"
        );
    }

    #[test]
    fn truncate_label_caps_long_text() {
        let long = "a".repeat(120);
        let label = clean_user_message(&long).expect("label");
        assert_eq!(label.chars().count(), LABEL_MAX_CHARS + 1);
        assert!(label.ends_with('…'));
    }

    /// A JSONL file whose
    /// first line exceeds [`MAX_LINE_BYTES`] must NOT be loaded
    /// fully into memory. The truncated line fails the
    /// `serde_json::from_str` parse and the file is skipped --
    /// `read_session_meta` returns `None` without OOMing.
    #[test]
    fn read_session_meta_truncates_oversize_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oversize.jsonl");
        // 1 MB single line, no newline. Well above MAX_LINE_BYTES.
        let big = "x".repeat(1024 * 1024);
        std::fs::write(&path, &big).expect("write fixture");
        // Sanity: input file is 1 MB.
        let meta = std::fs::metadata(&path).expect("metadata");
        assert_eq!(meta.len(), 1024 * 1024);
        // The reader caps at MAX_LINE_BYTES and surfaces None.
        assert!(read_session_meta(&path).is_none());
    }

    #[test]
    fn read_session_meta_returns_none_when_no_cwd_envelope_in_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("no-cwd.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"permission-mode","permissionMode":"default","sessionId":"x"}
{"type":"file-history-snapshot","snapshot":{}}
"#,
        )
        .expect("write fixture");
        assert!(read_session_meta(&path).is_none());
    }

    #[test]
    fn session_id_control_char_guard() {
        // sessionId carries CR+LF + an injected shell command. Without
        // the guard, this id would flow into `claude --resume <id>` and
        // submit `rm -rf ~` as a separate PTY command.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("malicious.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"abc\r\nrm -rf ~","version":"2.1.119","gitBranch":"main"}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert!(
            read_session_meta(&path).is_none(),
            "session with control chars in sessionId must be dropped"
        );
    }

    #[test]
    fn session_id_legitimate_uuid_passes_guard() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ok.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj","sessionId":"550e8400-e29b-41d4-a716-446655440000","version":"2.1.119","gitBranch":"main"}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        let meta = read_session_meta(&path).expect("legitimate UUID must pass the guard");
        assert_eq!(meta.session_id, "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn cwd_control_char_guard() {
        // cwd is display-only today (prettify_cwd in sessions_sidebar) but
        // the same JSONL field could leak into a future `cd <cwd>`
        // prefix. Guard at the gate, not at each future consumer.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("malicious-cwd.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"hi"},"uuid":"u","timestamp":"2026-04-26T13:38:41.095Z","cwd":"/tmp/proj\r\nrm -rf ~","sessionId":"550e8400-e29b-41d4-a716-446655440000","version":"2.1.119","gitBranch":"main"}"#,
                "\n",
            ),
        )
        .expect("write fixture");
        assert!(
            read_session_meta(&path).is_none(),
            "session with control chars in cwd must be dropped"
        );
    }

    /// A second read_session_meta call against
    /// the same path returns the same content, and the
    /// cache::lookup/store cycle round-trips a fixture vector. This
    /// test exercises the cache primitives directly (the
    /// `read_sessions_for_cwd` path hits real `~/.claude/projects/`
    /// which we cannot reliably set up in unit tests).
    #[test]
    fn session_cache_round_trips_and_invalidates_on_mtime_change() {
        use crate::agent_sessions::cache;
        cache::clear();

        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = "/some/cwd";
        let project_dir = dir.path();

        // Empty cache: lookup returns None.
        assert!(
            cache::lookup(SessionAgent::Claude, cwd, project_dir).is_none(),
            "freshly-cleared cache must miss"
        );

        let fixture = vec![SessionMeta {
            agent: SessionAgent::Claude,
            session_id: "abc".into(),
            last_activity_secs: 0,
            timestamp: "2026-04-26T13:00:00Z".into(),
            cwd: cwd.into(),
            git_branch: String::new(),
            summary: None,
            first_prompt: None,
            model: None,
            usage: None,
        }];
        cache::store_result(SessionAgent::Claude, cwd, project_dir, &fixture, 7);

        let (hit, omitted) = cache::lookup(SessionAgent::Claude, cwd, project_dir)
            .expect("post-store lookup must hit");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].session_id, "abc");
        assert_eq!(omitted, 7);

        // Touch the directory to bump its mtime; sleep long enough to
        // cross every mtime-granularity floor we care about: ext4/
        // APFS/NTFS report sub-millisecond mtimes, but FAT32/exFAT
        // round to 2 s and NFS without `actimeo` typically rounds to
        // 1 s. 2500 ms is the safe floor across the matrix (FAT32
        // 2 s + a healthy guard band) -- a faster sleep would silently
        // flake on Windows CI runners that fall back to FAT32-style
        // semantics or on NFS-mounted CI volumes.
        std::thread::sleep(std::time::Duration::from_millis(2500));
        std::fs::write(project_dir.join("touch.tmp"), b"x").expect("touch");

        assert!(
            cache::lookup(SessionAgent::Claude, cwd, project_dir).is_none(),
            "mtime bump must invalidate the cached entry"
        );
    }

    #[test]
    fn exactly_max_final_line_is_parsed_not_dropped() {
        // A complete envelope exactly MAX_LINE_BYTES long with no
        // trailing newline (a final record written without a final EOL) must be
        // parsed, not misclassified as oversized and dropped.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("exact.jsonl");
        let prefix =
            r#"{"sessionId":"550e8400-e29b-41d4-a716-446655440000","cwd":"/tmp/proj","p":""#;
        let suffix = r#""}"#;
        let pad = MAX_LINE_BYTES as usize - prefix.len() - suffix.len();
        let line = format!("{prefix}{}{suffix}", "x".repeat(pad));
        assert_eq!(
            line.len() as u64,
            MAX_LINE_BYTES,
            "fixture must be exactly the cap"
        );
        std::fs::write(&path, &line).expect("write"); // no trailing newline
        let meta = read_session_meta(&path).expect("exactly-MAX complete record must parse");
        assert_eq!(meta.cwd, "/tmp/proj");
    }

    #[test]
    fn genuinely_oversized_line_is_skipped() {
        // A line longer than MAX_LINE_BYTES (the cap truncates it
        // mid-line, more bytes follow) is still classified oversized and
        // dropped - the peek sees a non-empty buffer, not EOF.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oversized.jsonl");
        let line = format!(
            r#"{{"cwd":"/tmp/proj","p":"{}"#,
            "x".repeat(MAX_LINE_BYTES as usize + 2000)
        );
        std::fs::write(&path, &line).expect("write"); // truncated, no close/newline
        assert!(
            read_session_meta(&path).is_none(),
            "an oversized line must be skipped, not parsed"
        );
    }
}
