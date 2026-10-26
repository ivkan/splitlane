//! What happened to the sessions one agent opens and drives for another,
//! one JSON line each, in `orchestration.jsonl` beside the app's log.
//!
//! A failed hand-off is reported after the fact - "the session I opened never
//! got its task" - and by then the rail says only how things stand now: the
//! prompt that was lost, the turn that never started, the worktree that could
//! not be made are gone from it. The log has some of this as prose and only
//! at its own level; a line here per event, with the session's ids on each,
//! is what lets one hand-off be followed from its open to its close.
//!
//! Never the text of a prompt, only its length: text sent to an agent often
//! carries secrets, and a journal of it would be one more place they sit.

use std::sync::OnceLock;

use crate::log_file::Appender;

/// About ten thousand events per file, two files kept.
const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;

static JOURNAL: OnceLock<Appender> = OnceLock::new();

/// Begin writing the journal. Until this is called, and in every process
/// but the app's own window, events go nowhere: a CLI call has nothing to
/// add to it, and the writer is a thread that must not exist before the
/// process environment has been settled.
pub(crate) fn start() {
    let Some(path) = crate::runtime_paths::orchestration_journal_path() else {
        return;
    };
    match Appender::start(path, MAX_JOURNAL_BYTES) {
        Ok(appender) => {
            let _ = JOURNAL.set(appender);
        }
        Err(e) => log::warn!("orchestration journal not started: {e}"),
    }
}

/// One event about a session. `thread_id` is the session's record, `None`
/// for a refusal that left no record behind; `fields` is an object whose
/// keys are added to the line.
pub(crate) fn record(event: &str, session: Session<'_>, fields: serde_json::Value) {
    let Some(journal) = JOURNAL.get() else {
        return;
    };
    journal.append(line(now_unix(), event, session, fields).into_bytes());
}

/// Who an event is about: the ids a line carries whatever the event.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Session<'a> {
    pub thread_id: Option<u64>,
    /// The record id of the session that opened this one, or drives it.
    pub opener: Option<u64>,
    pub agent: Option<&'static str>,
    /// The agent's own session id, which names its transcript.
    pub session_id: Option<&'a str>,
}

fn line(at_unix: u64, event: &str, session: Session<'_>, fields: serde_json::Value) -> String {
    let mut line = serde_json::json!({
        "at_unix": at_unix,
        "event": event,
        "thread_id": session.thread_id,
        "opener": session.opener,
        "agent": session.agent,
        "session_id": session.session_id,
    });
    if let (Some(line), serde_json::Value::Object(fields)) = (line.as_object_mut(), fields) {
        line.extend(fields);
    }
    format!("{line}\n")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_carries_the_sessions_ids_and_the_events_own_fields() {
        let text = line(
            1_700_000_000,
            "open",
            Session {
                thread_id: Some(7),
                opener: Some(3),
                agent: Some("claude"),
                session_id: None,
            },
            serde_json::json!({ "cwd": "/work", "prompt_bytes": 12 }),
        );
        assert!(text.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(text.trim_end()).expect("json");
        assert_eq!(parsed["at_unix"], 1_700_000_000);
        assert_eq!(parsed["event"], "open");
        assert_eq!(parsed["thread_id"], 7);
        assert_eq!(parsed["opener"], 3);
        assert_eq!(parsed["agent"], "claude");
        assert!(parsed["session_id"].is_null());
        assert_eq!(parsed["cwd"], "/work");
        assert_eq!(parsed["prompt_bytes"], 12);
    }

    #[test]
    fn nothing_is_written_before_the_journal_is_started() {
        // Tests never start it, so this must return without a writer.
        record("open", Session::default(), serde_json::json!({}));
        assert!(JOURNAL.get().is_none());
    }
}
