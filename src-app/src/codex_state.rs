//! What Codex is doing, from the rollout file it writes for itself.
//!
//! The second reader, and the second agent of sixteen that can honestly say
//! "waiting for you". It answers the same question as
//! [`crate::claude_sessions::probe_state_from_tail`] and feeds the same
//! [`crate::agent_state::classify`]; everything between the two files is
//! different, and that difference is the whole of this module.
//!
//! # The file
//!
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO timestamp>-<uuid>.jsonl`, one
//! JSON object per line, appended as the session runs (growth between snapshots
//! measured live on 0.149.1). Every record carries a top-level `timestamp` and a
//! `payload`; the first is `session_meta`, whose payload names the session `id`
//! and its `cwd`.
//!
//! A call is `payload.type == "function_call"` with a `call_id`, a `name`, and
//! an `arguments` field that is itself a **JSON string**. It is answered by a
//! later `function_call_output` carrying the same `call_id`. A session closed
//! over an unanswered call leaves it open for ever - one of the eighteen local
//! rollouts ends exactly that way, which is the same observable shape the Claude
//! reader is built on.
//!
//! # Measured, 27 August 2026, corpus of 18 rollouts (0.133.0 and 0.149.1)
//!
//! **One tool name.** `exec_command`, 7 calls of 7. Older notes in this
//! repository expect `shell` and `apply_patch`; neither appears.
//!
//! **The kind is not a function of the name.** `arguments` carries
//! `"sandbox_permissions":"require_escalated"` on some calls and nothing on
//! others, and the two behave differently enough that one word cannot cover
//! both:
//!
//! | `sandbox_permissions` | n | gap between the call and its answer |
//! |---|---|---|
//! | absent | 3 | 0.79 s · 1.74 s · 2.19 s |
//! | `require_escalated` | 3 | 2.13 s · 122.81 s · 133.50 s |
//!
//! A call that needs no escalation is answered in about two seconds and **never
//! stops for a person** - so an old one is a long command, not a question, and
//! nothing is claimed about it: [`ToolExecution::Opaque`].
//!
//! An escalated call *may* stop for a person, and three of the four here did:
//! two minutes against a command that then ran for 0.74 s. But **one did not**,
//! coming back in 2.13 s. So the marker alone is not the answer - a correction
//! to this repository's own earlier note, which called it sufficient. It is
//! `Spawns`: a command that has spawned nothing has not started, and a command
//! that has not started is one waiting for an answer. The worker decides, the
//! same way it decides for Claude Code's `Bash`.
//!
//! **On the grace period.** The shared [`crate::agent_state::GRACE`] of five
//! seconds sits above every non-waiting gap measured here (largest: 2.19 s
//! unescalated, 2.13 s escalated) and far below every waiting one (smallest:
//! 122.81 s). It is not re-derived from this corpus, which has four escalated
//! calls in it - too few for a percentile, and said so rather than dressed up.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agent_state::{OpenCall, ToolExecution, TranscriptProbe};

/// How much of the file's tail is read. The same bound the Claude reader uses,
/// for the same reason: the answer is about the newest records, and a session
/// that has run all day must not cost a full parse every two seconds.
const TAIL_BYTES: u64 = 512 * 1024;

/// A line longer than this is not parsed. Codex writes a command's whole
/// captured output into `function_call_output`, so a big one is ordinary rather
/// than corrupt - and a tail that begins mid-record leaves a fragment that must
/// not be parsed either. Both raise `incomplete`.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// The rollout file for `session_id`, or `None` when it cannot be found.
///
/// Unlike Claude Code's, this path cannot be composed: the file name carries the
/// session's **start timestamp** before its uuid, and the directories are the
/// date it started. So the uuid is searched for rather than joined.
///
/// The walk is bounded by the store's own shape - `sessions/YYYY/MM/DD` - and
/// never recurses freely, so a symlink planted under it cannot lead the reader
/// out of the store or into a cycle.
pub fn rollout_path_for(session_id: &str) -> Option<PathBuf> {
    if !crate::agent_sessions::is_valid_session_id(session_id) {
        return None;
    }
    let root = crate::codex_sessions::sessions_root()?;
    let suffix = format!("-{session_id}.jsonl");
    // Newest first: a session started today is overwhelmingly the common case,
    // and this is asked once every two seconds per surface.
    let mut days: Vec<PathBuf> = Vec::new();
    for year in read_dir_sorted(&root) {
        for month in read_dir_sorted(&year) {
            days.extend(read_dir_sorted(&month));
        }
    }
    for day in days.into_iter().rev() {
        let Ok(entries) = std::fs::read_dir(&day) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with("rollout-") && name.ends_with(&suffix) {
                return Some(path);
            }
        }
    }
    None
}

/// Immediate subdirectories of `dir`, sorted by name - which for `YYYY`, `MM`
/// and `DD` is chronological order.
fn read_dir_sorted(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// Read the tail of a rollout and report what is open, as of `now` (Unix
/// seconds).
///
/// `None` means the file could not be read at all, which the pass carries to
/// "the detector has nothing to say" - never to a state.
pub fn probe_state_from_tail(path: &Path, now: i64) -> Option<TranscriptProbe> {
    let (text, truncated_head) = read_tail(path, TAIL_BYTES)?;
    Some(probe_from_text(&text, truncated_head, now))
}

/// Read at most `bytes` from the end of the file. The bool is `true` when the
/// read began mid-file, so the first line is a fragment.
fn read_tail(path: &Path, bytes: u64) -> Option<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let from = len.saturating_sub(bytes);
    if from > 0 {
        file.seek(SeekFrom::Start(from)).ok()?;
    }
    let mut buf = Vec::with_capacity(bytes.min(len) as usize);
    file.take(bytes).read_to_end(&mut buf).ok()?;
    Some((String::from_utf8_lossy(&buf).into_owned(), from > 0))
}

/// The parse, over text rather than a file, so the whole contract is testable
/// without a filesystem.
fn probe_from_text(text: &str, truncated_head: bool, now: i64) -> TranscriptProbe {
    // `call_id` -> (name, execution, started at). A `function_call_output`
    // removes its entry; whatever is left was never answered.
    let mut open: Vec<(String, OpenCall)> = Vec::new();
    let mut incomplete = truncated_head;
    let mut errored = false;
    // Whether the newest turn-lifecycle event said a turn was running, and when
    // it was written. See the deposit at the end of this function.
    let mut turn_open = false;
    let mut turn_at: Option<Duration> = None;
    let mut last_turn_end: Option<crate::rail_state::TurnEnd> = None;

    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // The first line of a mid-file read is a fragment by construction. It is
        // skipped rather than parsed, and the flag is already set above.
        if index == 0 && truncated_head {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            incomplete = true;
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            // A record this build cannot parse may have been the one that
            // closed a call. Saying so is the whole point of the flag.
            incomplete = true;
            continue;
        };
        let Some(payload) = record.get("payload").and_then(|p| p.as_object()) else {
            continue;
        };
        match payload.get("type").and_then(|t| t.as_str()) {
            Some("function_call") => {
                let Some(call_id) = payload.get("call_id").and_then(|c| c.as_str()) else {
                    // A call with no id can never be matched to its answer, so
                    // it would stay open for ever. Refuse it and say the window
                    // is untrustworthy.
                    incomplete = true;
                    continue;
                };
                let name = payload
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let age = record
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .and_then(|t| age_from_timestamp(t, now));
                let execution = execution_of_call(&name, payload.get("arguments"));
                open.retain(|(id, _)| id != call_id);
                open.push((
                    call_id.to_string(),
                    OpenCall {
                        name,
                        age,
                        execution,
                    },
                ));
            }
            Some("function_call_output") => {
                if let Some(call_id) = payload.get("call_id").and_then(|c| c.as_str()) {
                    open.retain(|(id, _)| id != call_id);
                }
            }
            _ => {}
        }
        // **The turn's own lifecycle, which Codex states outright.** Three
        // events, all three persisted in every history mode
        // (`codex-rs/rollout/src/policy.rs::should_persist_event_msg`, read at
        // tag `rust-v0.149.1` - the version installed here): `task_started`
        // opens a turn, `task_complete` closes it, `turn_aborted` closes it
        // because a person pressed Esc.
        //
        // This is a better signal than the one next door rather than a copy of
        // it. Claude Code has no turn event at all and its end has to be read
        // off `stop_reason`, a field whose full range is not documented in the
        // file - which is what made the reader's first version close a turn on
        // `max_tokens`. Codex names all three states, so nothing is inferred
        // and there is no unknown word to be careful about.
        //
        // Confirmed in the local corpus, and the two sources agree exactly:
        // 19 `task_started`, 17 `task_complete`, 1 `turn_aborted` across 18
        // rollout files - one session left mid-turn, which is what a session
        // closed while working looks like.
        if record.get("type").and_then(|t| t.as_str()) == Some("event_msg") {
            match payload.get("type").and_then(|t| t.as_str()) {
                Some("task_started") => {
                    turn_open = true;
                    turn_at = record
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(|t| age_from_timestamp(t, now));
                }
                // `turn_aborted` is the human refusing. It ends the turn, and
                // any call still standing was the one they refused - a record,
                // not a question.
                Some(ending @ ("task_complete" | "turn_aborted")) => {
                    turn_open = false;
                    turn_at = None;
                    // `turn_id` is optional on `turn_aborted` in the writer's
                    // own type (`TurnAbortedEvent`, read at `rust-v0.158.0`),
                    // so the record's stamp stands in: all the marker has to
                    // do is differ between turns.
                    let marker = payload
                        .get("turn_id")
                        .and_then(|id| id.as_str())
                        .or_else(|| record.get("timestamp").and_then(|t| t.as_str()));
                    if let Some(marker) = marker {
                        last_turn_end = Some(crate::rail_state::TurnEnd {
                            marker: marker.to_string(),
                            // A turn that could not finish still writes
                            // `task_complete`, with what went wrong in `error`.
                            failed: ending == "task_complete"
                                && payload.get("error").is_some_and(|e| !e.is_null()),
                        });
                    }
                }
                _ => {}
            }
            if payload.get("type").and_then(|t| t.as_str()) == Some("turn_aborted") {
                open.clear();
            }
        }
        if payload.get("type").and_then(|t| t.as_str()) == Some("error") {
            errored = true;
        } else if payload.get("type").is_some() {
            errored = false;
        }
    }

    TranscriptProbe {
        // Codex reports a running total rather than a per-message cost, so a
        // rate from it is a subtraction between two passes and not a sum over
        // a tail. Deliberately not built here: it is a different mechanism,
        // and inventing it beside this one would give two answers to the same
        // question.
        spend: Vec::new(),
        open_calls: open.into_iter().map(|(_, call)| call).collect(),
        // Dated from `task_started`, so the age is the whole turn's rather than
        // the last record's - which is the honest reading here: Codex's own
        // event says when the turn began, and nothing between says anything
        // about it. An undatable turn is dropped, as it is for Claude Code:
        // nothing but the process would bound it.
        open_turn: turn_open.then_some(turn_at).flatten(),
        errored,
        incomplete,
        last_turn_end,
    }
}

/// How much of a rollout's tail is read for the last answer. An answer is
/// asked for once, on demand, so the bound is about not loading an all-day
/// session whole rather than about a two-second cadence.
const ANSWER_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// What a look for a Codex session's last answer found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAnswer {
    /// The newest ended turn's answer.
    Found(String),
    /// The newest ended turn ended in an error, and this is what it said.
    /// Reported as itself: an empty answer would read as "the agent had
    /// nothing to say", and the answer before it belongs to another turn.
    TurnFailed(String),
    /// The newest ended turn was stopped by a person before it answered.
    Aborted,
    /// The rollout is there and no turn in it has ended.
    NotYet,
    /// The file could not be read.
    Unreadable,
}

/// The answer of the newest turn that has **ended** in this rollout.
///
/// A turn's answer is `task_complete.last_agent_message`, which Codex writes
/// for exactly this purpose. When that is `null`, the last assistant message
/// of the same turn stands in - Codex writes those as `response_item` records
/// (`message`, role `assistant`, `output_text` blocks) and, in older releases,
/// also as `agent_message` events. A message from an earlier turn never does:
/// each `task_started` clears what was collected.
///
/// Blocking I/O.
pub fn read_last_answer(path: &Path) -> CodexAnswer {
    match read_tail(path, ANSWER_TAIL_BYTES) {
        Some((text, truncated_head)) => last_answer_from_text(&text, truncated_head),
        None => CodexAnswer::Unreadable,
    }
}

fn last_answer_from_text(text: &str, truncated_head: bool) -> CodexAnswer {
    let mut newest = CodexAnswer::NotYet;
    // The last thing the assistant said in the turn being read.
    let mut said: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || (index == 0 && truncated_head) {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(payload) = record.get("payload") else {
            continue;
        };
        let kind = payload.get("type").and_then(|t| t.as_str());
        match (record.get("type").and_then(|t| t.as_str()), kind) {
            (Some("event_msg"), Some("task_started")) => said = None,
            (Some("event_msg"), Some("agent_message")) => {
                if let Some(message) = non_blank(payload.get("message")) {
                    said = Some(message);
                }
            }
            (Some("response_item"), Some("message"))
                if payload.get("role").and_then(|r| r.as_str()) == Some("assistant") =>
            {
                let blocks = payload.get("content").and_then(|c| c.as_array());
                let joined = blocks
                    .into_iter()
                    .flatten()
                    .filter(|block| {
                        block.get("type").and_then(|t| t.as_str()) == Some("output_text")
                    })
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !joined.trim().is_empty() {
                    said = Some(joined.trim_end().to_string());
                }
            }
            (Some("event_msg"), Some("task_complete")) => {
                let error = payload.get("error").filter(|error| !error.is_null());
                newest = if let Some(error) = error {
                    CodexAnswer::TurnFailed(
                        non_blank(error.get("message")).unwrap_or_else(|| error.to_string()),
                    )
                } else if let Some(answer) =
                    non_blank(payload.get("last_agent_message")).or_else(|| said.take())
                {
                    CodexAnswer::Found(answer)
                } else {
                    // A turn that ended with nothing said. The answer before
                    // it is another turn's and is not offered in its place.
                    CodexAnswer::NotYet
                };
                said = None;
            }
            (Some("event_msg"), Some("turn_aborted")) => {
                newest = CodexAnswer::Aborted;
                said = None;
            }
            _ => {}
        }
    }
    newest
}

fn non_blank(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim_end().to_string())
}

/// How a Codex call executes, from its name **and its arguments**.
///
/// The escalation flag is what separates the two, and it is read out of
/// `arguments`, which Codex writes as a JSON *string* rather than an object.
fn execution_of_call(name: &str, arguments: Option<&serde_json::Value>) -> ToolExecution {
    if name != "exec_command" {
        // A name this build has not measured. The allow-list is the point:
        // nothing is claimed about a tool nobody has watched.
        return ToolExecution::Opaque;
    }
    let escalated = arguments
        .and_then(|a| a.as_str())
        .and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok())
        .and_then(|a| {
            a.get("sandbox_permissions")
                .and_then(|p| p.as_str())
                .map(|p| p == "require_escalated")
        })
        .unwrap_or(false);
    if escalated {
        ToolExecution::Spawns
    } else {
        // Measured: an unescalated call is answered in about two seconds and
        // never stops for a person. An old one is a long command.
        ToolExecution::Opaque
    }
}

/// How long ago `timestamp` was, as of `now` (Unix seconds).
///
/// `None` for a stamp this build cannot read **and** for one in the future.
/// A clock that disagrees with the file must not produce a negative age that
/// underflows into an enormous one - that is a person reported as waiting since
/// the epoch.
fn age_from_timestamp(timestamp: &str, now: i64) -> Option<Duration> {
    // The same parser the Claude reader uses, so the two agree about what a
    // stamp means; `0` is its "could not read it".
    let secs = crate::agent_sessions::last_activity_secs_from_iso(timestamp);
    if secs <= 0 {
        return None;
    }
    let age = now.checked_sub(secs)?;
    (age >= 0).then(|| Duration::from_secs(age as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_787_600_000;

    fn at(offset_secs: i64) -> String {
        chrono_like(NOW - offset_secs)
    }

    /// Format a Unix second as the RFC-3339 stamp Codex writes, without pulling
    /// in a date crate for a test fixture.
    fn chrono_like(unix: i64) -> String {
        let days = unix / 86_400;
        let rem = unix % 86_400;
        // 1970-01-01 + days, the long way. Fine for a fixture: the reader only
        // has to agree with `last_activity_secs_from_iso`, which the round-trip asserts.
        let (mut y, mut d) = (1970i64, days);
        loop {
            let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
            let len = if leap { 366 } else { 365 };
            if d < len {
                break;
            }
            d -= len;
            y += 1;
        }
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let months = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut m = 0;
        while d >= months[m] {
            d -= months[m];
            m += 1;
        }
        format!(
            "{y:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
            m + 1,
            d + 1,
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    fn call_line(call_id: &str, offset: i64, escalated: bool) -> String {
        let args = if escalated {
            r#"{\"cmd\":\"printf hi\",\"sandbox_permissions\":\"require_escalated\"}"#
        } else {
            r#"{\"cmd\":\"printf hi\"}"#
        };
        format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call","name":"exec_command","call_id":"{call_id}","arguments":"{args}"}}}}"#,
            at(offset)
        )
    }

    fn output_line(call_id: &str, offset: i64) -> String {
        format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call_output","call_id":"{call_id}","output":"ok"}}}}"#,
            at(offset)
        )
    }

    fn probe(lines: &[String]) -> TranscriptProbe {
        probe_from_text(&lines.join("\n"), false, NOW)
    }

    fn event(kind: &str, offset: i64) -> String {
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"{kind}"}}}}"#,
            at(offset)
        )
    }

    /// Codex states its turn outright, which Claude Code does not: three
    /// events, all persisted in every history mode. Read at tag
    /// `rust-v0.149.1`, and the local corpus agrees - 19 `task_started`,
    /// 17 `task_complete`, 1 `turn_aborted`.
    #[test]
    fn a_started_turn_with_no_call_open_is_work() {
        let probe = probe(&[event("task_started", 130)]);
        assert!(probe.open_calls.is_empty());
        assert_eq!(probe.open_turn.expect("turn open").as_secs(), 130);
        assert_eq!(
            crate::agent_state::classify(
                &probe,
                crate::agent_state::Worker::Absent,
                crate::agent_state::AgentProcess::Found
            ),
            crate::ai_types::AgentState::Thinking
        );
    }

    /// And both ways out of a turn end it. `turn_aborted` is a person pressing
    /// Esc, and it must not leave a spinner behind on the session they stopped.
    #[test]
    fn both_ends_of_a_turn_close_it() {
        for ending in ["task_complete", "turn_aborted"] {
            let probe = probe(&[event("task_started", 130), event(ending, 10)]);
            assert_eq!(probe.open_turn, None, "{ending} must end the turn");
        }
    }

    /// Records cut from rollouts Codex 0.149.1 wrote on a developer machine:
    /// the turn-lifecycle events and the assistant messages, with the prose
    /// left as written. The same three events and the same fields are in the
    /// writer's types at `rust-v0.158.0` (`TurnCompleteEvent`,
    /// `TurnAbortedEvent`), where `turn_id` is optional on an aborted turn.
    const TWO_TURNS: &str = include_str!("../tests/fixtures/codex/rollout-0.149.1-two-turns.jsonl");
    const FAILED_TURN: &str =
        include_str!("../tests/fixtures/codex/rollout-0.149.1-failed-turn.jsonl");
    const ABORTED_TURN: &str =
        include_str!("../tests/fixtures/codex/rollout-0.149.1-aborted-turn.jsonl");

    /// A completed turn's marker is its `turn_id`, and the newest turn wins.
    #[test]
    fn the_turn_end_marker_is_the_turn_id_of_the_newest_ended_turn() {
        let end = probe_from_text(TWO_TURNS, false, NOW)
            .last_turn_end
            .expect("two turns ended");
        assert_eq!(end.marker, "01a03489-8303-79c3-a15a-2685e8ef6126");
        assert!(!end.failed);
        // The first turn alone.
        let first: String = TWO_TURNS.lines().take(3).collect::<Vec<_>>().join("\n");
        let end = probe_from_text(&first, false, NOW)
            .last_turn_end
            .expect("one turn ended");
        assert_eq!(end.marker, "01a03488-84ec-73d3-913f-4bc25252f512");
    }

    /// A turn that could not finish still ends, and says why: `task_complete`
    /// with no message and an `error`.
    #[test]
    fn a_turn_that_ended_in_an_error_is_marked_failed() {
        let end = probe_from_text(FAILED_TURN, false, NOW)
            .last_turn_end
            .expect("the turn ended");
        assert_eq!(end.marker, "01a0e7b6-f5d5-7cf1-a279-21bdf7ae0e79");
        assert!(end.failed);
    }

    /// A person stopping the turn ends it too, and is not a failure.
    #[test]
    fn an_aborted_turn_is_a_turn_end() {
        let end = probe_from_text(ABORTED_TURN, false, NOW)
            .last_turn_end
            .expect("the turn ended");
        assert_eq!(end.marker, "01a034c1-8854-7721-965a-36b2d39e2996");
        assert!(!end.failed);
    }

    /// The writer's type makes `turn_id` optional on an aborted turn. The
    /// record's own stamp then stands in, so two such turns still differ.
    #[test]
    fn an_aborted_turn_without_an_id_is_marked_by_its_stamp() {
        let line = format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"turn_aborted","reason":"interrupted"}}}}"#,
            at(10)
        );
        let end = probe_from_text(&line, false, NOW)
            .last_turn_end
            .expect("the turn ended");
        assert_eq!(end.marker, at(10));
    }

    /// The answer is what Codex itself recorded as the turn's last message,
    /// and it is the newest ended turn's.
    #[test]
    fn the_last_answer_is_the_newest_ended_turns() {
        let CodexAnswer::Found(answer) = last_answer_from_text(TWO_TURNS, false) else {
            panic!("the rollout holds two answered turns");
        };
        assert!(
            answer.starts_with("I can’t proceed without explicit approval"),
            "{answer}"
        );
        let first: String = TWO_TURNS.lines().take(3).collect::<Vec<_>>().join("\n");
        let CodexAnswer::Found(answer) = last_answer_from_text(&first, false) else {
            panic!("the first turn was answered");
        };
        assert!(
            answer.starts_with("I can’t write to /private/tmp"),
            "{answer}"
        );
    }

    /// A turn that failed says so. It is not "no answer", and the answer of
    /// the turn before it is not offered instead.
    #[test]
    fn a_failed_turn_reports_its_error_not_an_older_answer() {
        let both = format!("{TWO_TURNS}{FAILED_TURN}");
        let CodexAnswer::TurnFailed(error) = last_answer_from_text(&both, false) else {
            panic!("the newest turn failed");
        };
        assert!(error.contains("no credits remaining"), "{error}");
    }

    /// A turn a person stopped has no answer, whatever it had said by then.
    #[test]
    fn an_aborted_turn_has_no_answer() {
        assert_eq!(
            last_answer_from_text(ABORTED_TURN, false),
            CodexAnswer::Aborted
        );
        let both = format!("{TWO_TURNS}{ABORTED_TURN}");
        assert_eq!(last_answer_from_text(&both, false), CodexAnswer::Aborted);
    }

    /// When `last_agent_message` is `null` and the turn did say something, the
    /// last assistant message of **that** turn is the answer.
    #[test]
    fn a_null_last_message_falls_back_to_what_the_turn_said() {
        let turn: Vec<&str> = ABORTED_TURN.lines().take(2).collect();
        let complete = r#"{"timestamp":"2026-08-24T17:13:53.307Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"01a034c1-8854-7721-965a-36b2d39e2996","last_agent_message":null}}"#;
        let text = format!("{}\n{complete}", turn.join("\n"));
        let CodexAnswer::Found(answer) = last_answer_from_text(&text, false) else {
            panic!("the turn said something");
        };
        assert!(answer.starts_with("I'll request permission"), "{answer}");

        // Said in an earlier turn: not this turn's answer.
        let started = r#"{"timestamp":"2026-08-24T17:14:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"next"}}"#;
        let silent = format!("{}\n{started}\n{complete}", turn.join("\n"));
        assert_eq!(last_answer_from_text(&silent, false), CodexAnswer::NotYet);
    }

    #[test]
    fn a_rollout_with_no_ended_turn_has_no_answer_yet() {
        let started: String = TWO_TURNS.lines().take(2).collect::<Vec<_>>().join("\n");
        assert_eq!(last_answer_from_text(&started, false), CodexAnswer::NotYet);
        assert_eq!(last_answer_from_text("", false), CodexAnswer::NotYet);
    }

    #[test]
    fn a_missing_rollout_is_unreadable() {
        assert_eq!(
            read_last_answer(Path::new("/nonexistent/rollout.jsonl")),
            CodexAnswer::Unreadable
        );
    }

    /// A turn still running has not ended.
    #[test]
    fn a_turn_in_flight_leaves_no_marker() {
        assert_eq!(probe(&[event("task_started", 30)]).last_turn_end, None);
    }

    /// The fixture and the reader must agree about time, or every age assertion
    /// below is measuring the fixture.
    #[test]
    fn the_fixture_round_trips_through_the_real_parser() {
        for offset in [0, 5, 130, 86_400] {
            let age = age_from_timestamp(&at(offset), NOW).expect("parsed");
            assert_eq!(age.as_secs() as i64, offset, "offset {offset}");
        }
    }

    /// An answered call is not open, however old it is.
    #[test]
    fn an_answered_call_is_closed() {
        let p = probe(&[call_line("c1", 300, true), output_line("c1", 290)]);
        assert!(p.open_calls.is_empty());
        assert!(!p.incomplete);
    }

    /// The measured waiting shape: an escalated call, minutes old, never
    /// answered. `Spawns`, so the worker gets the vote - which is what makes it
    /// the same rule Claude Code goes through.
    #[test]
    fn an_open_escalated_call_spawns_and_carries_its_age() {
        let p = probe(&[call_line("c1", 130, true)]);
        assert_eq!(p.open_calls.len(), 1);
        assert_eq!(p.open_calls[0].name, "exec_command");
        assert_eq!(p.open_calls[0].execution, ToolExecution::Spawns);
        assert_eq!(p.open_calls[0].age, Some(Duration::from_secs(130)));
    }

    /// And the measured *non*-waiting shape. An unescalated call is answered in
    /// about two seconds and never stops for a person, so an old one is a long
    /// command and nothing is claimed about it.
    #[test]
    fn an_unescalated_call_claims_nothing() {
        let p = probe(&[call_line("c1", 600, false)]);
        assert_eq!(p.open_calls.len(), 1);
        assert_eq!(p.open_calls[0].execution, ToolExecution::Opaque);
    }

    /// A tool name nobody has watched gets the allow-list's answer, not a guess.
    #[test]
    fn an_unmeasured_tool_name_is_opaque() {
        assert_eq!(
            execution_of_call("apply_patch", None),
            ToolExecution::Opaque
        );
        assert_eq!(execution_of_call("shell", None), ToolExecution::Opaque);
    }

    /// The human refused: the turn ended, and the call still standing was the
    /// one they refused. Leaving it open would report them as still being asked.
    #[test]
    fn turn_aborted_closes_what_was_standing() {
        let aborted = format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"turn_aborted"}}}}"#,
            at(100)
        );
        let p = probe(&[call_line("c1", 200, true), aborted]);
        assert!(p.open_calls.is_empty());
    }

    /// A record that cannot be parsed may have been the one that closed a call,
    /// so the window says so instead of inventing a state from what is left.
    #[test]
    fn an_unparseable_record_makes_the_window_untrustworthy() {
        let p = probe(&[call_line("c1", 130, true), "{not json".to_string()]);
        assert!(p.incomplete);
    }

    /// A call with no id can never be matched to its answer, so admitting the
    /// gap is the only honest move: kept open it would wait for ever.
    #[test]
    fn a_call_without_an_id_is_refused_and_flagged() {
        let anon = format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call","name":"exec_command"}}}}"#,
            at(130)
        );
        let p = probe(&[anon]);
        assert!(p.open_calls.is_empty());
        assert!(p.incomplete);
    }

    /// A read that began mid-file starts on a fragment. It is skipped rather
    /// than parsed, and the window is untrustworthy either way.
    #[test]
    fn a_mid_file_read_is_untrustworthy_and_skips_its_fragment() {
        let text = format!("ction_call\"}}}}\n{}", call_line("c1", 130, true));
        let p = probe_from_text(&text, true, NOW);
        assert!(p.incomplete);
        assert_eq!(p.open_calls.len(), 1, "the fragment produced nothing");
    }

    /// A stamp ahead of our clock is a disagreement, not an age. Underflowing it
    /// would date the call to the epoch and report a person waiting since 1970.
    #[test]
    fn a_future_timestamp_is_not_an_age() {
        assert_eq!(age_from_timestamp(&at(-60), NOW), None);
    }

    /// The same call id appearing twice is one call, not two - the second
    /// record replaces the first rather than doubling it.
    #[test]
    fn a_repeated_call_id_is_one_call() {
        let p = probe(&[call_line("c1", 300, true), call_line("c1", 130, true)]);
        assert_eq!(p.open_calls.len(), 1);
        assert_eq!(p.open_calls[0].age, Some(Duration::from_secs(130)));
    }

    /// Read the machine's real rollout store and print what this reader makes
    /// of it, so the parse can be checked against the corpus it was derived
    /// from rather than only against fixtures written from the same reading.
    ///
    /// ```text
    /// cargo test -p splitlane-app --bin splitlane -- --ignored the_reader_on_the_real_corpus --nocapture
    /// ```
    #[test]
    #[ignore = "reads the machine's own ~/.codex/sessions"]
    fn the_reader_on_the_real_corpus() {
        let Some(root) = crate::codex_sessions::sessions_root() else {
            panic!("no codex sessions root");
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        let mut files = 0usize;
        let mut open = 0usize;
        let mut incomplete = 0usize;
        let mut spawns = 0usize;
        let mut opaque = 0usize;
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
                    continue;
                }
                files += 1;
                let Some(probe) = probe_state_from_tail(&path, now) else {
                    println!("UNREADABLE {}", path.display());
                    continue;
                };
                if probe.incomplete {
                    incomplete += 1;
                }
                for call in &probe.open_calls {
                    open += 1;
                    match call.execution {
                        ToolExecution::Spawns => spawns += 1,
                        ToolExecution::Opaque => opaque += 1,
                        ToolExecution::Instant => {
                            panic!("Codex has no Instant tools; {} said otherwise", call.name)
                        }
                    }
                    println!(
                        "OPEN {:>10} {:<14} {:?} age={:?}",
                        name.get(8..24).unwrap_or(name),
                        call.name,
                        call.execution,
                        call.age.map(|a| a.as_secs())
                    );
                }
            }
        }
        println!(
            "\nfiles={files} open_calls={open} (spawns={spawns} opaque={opaque}) incomplete_windows={incomplete}"
        );
        assert!(files > 0, "no rollouts on this machine to check against");

        // And the other half: every session this store knows about must be
        // findable by its id alone. The path cannot be composed - the file is
        // named after the moment the session started, under the date it started
        // - so this is the only thing that proves the search agrees with the
        // naming, and it is what stands between a live Codex surface and its
        // own file.
        let mut resolved = 0usize;
        let mut stack = vec![crate::codex_sessions::sessions_root().expect("root")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
                    continue;
                }
                // The id as the file itself spells it, taken from the name
                // rather than from the record, so a disagreement between the two
                // would show up here rather than cancel out.
                let id = name
                    .trim_end_matches(".jsonl")
                    .rsplit('-')
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("-");
                match rollout_path_for(&id) {
                    Some(found) => {
                        assert_eq!(found, path, "resolved the wrong file for {id}");
                        resolved += 1;
                    }
                    None => panic!("could not resolve {id} back to {}", path.display()),
                }
            }
        }
        println!("resolved {resolved}/{files} session ids back to their own file");
        assert_eq!(resolved, files);
    }

    /// The path cannot be composed, so a bad id must not become a walk.
    #[test]
    fn an_invalid_session_id_finds_nothing() {
        assert_eq!(rollout_path_for("../../etc"), None);
        assert_eq!(rollout_path_for(""), None);
    }
}
