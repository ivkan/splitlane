//! The orchestration journal's view of the rail: turns that start and end,
//! waits for a person, exits, and turns that stall.
//!
//! Read from the same snapshots `surface.rail` events are published from, so
//! the journal and a script watching the rail cannot tell two stories. The
//! state pass and the hook frames both change what a snapshot says; reading
//! the result instead of each writer keeps one place that decides what is
//! an event.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Entity};

use crate::SplitlaneApp;
use crate::project::ThreadStatus;
use crate::rail_state::RailSnapshot;
use crate::terminal::view::TerminalView;

/// How long a turn may say it is working while its transcript does not
/// grow before the journal calls it stalled. Long enough for a slow tool
/// call or a long think, which write nothing until they end; short enough
/// that a hung session is on record well before somebody gives up on it.
pub(crate) const STALL_AFTER: Duration = Duration::from_secs(5 * 60);

/// What the journal last saw of one session.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Seen {
    status: ThreadStatus,
    runs_ended: u64,
    pane_exit: Option<i32>,
    transcript_len: Option<u64>,
    /// Since when the transcript has stood at `transcript_len` during a turn.
    still_since: Instant,
    stall_reported: bool,
}

/// What one session looks like on this pass.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Facts {
    pub status: ThreadStatus,
    pub runs_ended: u64,
    pub last_outcome: Option<&'static str>,
    /// The exit code of the process under the pane, once it has ended.
    pub pane_exit: Option<i32>,
    /// The length of the agent's transcript, where the state pass reads one.
    pub transcript_len: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    TurnStart,
    TurnEnd {
        outcome: Option<&'static str>,
        runs_ended: u64,
    },
    Waiting,
    PaneExit(i32),
    Stall {
        still_for: Duration,
        transcript_len: u64,
    },
}

/// The events between what was seen last and `now`.
///
/// A session seen for the first time is taken as it stands: after a restart
/// every restored session would otherwise begin with a line about a turn it
/// was not seen to start. One already working or waiting is the exception,
/// because that is a turn somebody may be waiting on.
pub(crate) fn step(previous: Option<Seen>, now: Facts, at: Instant) -> (Seen, Vec<Event>) {
    let mut events = Vec::new();
    let became = |status: ThreadStatus| {
        now.status == status && previous.is_none_or(|seen| seen.status != status)
    };
    if became(ThreadStatus::Thinking) {
        events.push(Event::TurnStart);
    }
    if let Some(seen) = previous
        && now.runs_ended > seen.runs_ended
    {
        events.push(Event::TurnEnd {
            outcome: now.last_outcome,
            runs_ended: now.runs_ended,
        });
    }
    if became(ThreadStatus::WaitingForInput) {
        events.push(Event::Waiting);
    }
    if let Some(code) = now.pane_exit
        && previous.is_some_and(|seen| seen.pane_exit.is_none())
    {
        events.push(Event::PaneExit(code));
    }

    // The clock runs only through a turn, and starts again whenever the
    // transcript moves or the turn begins.
    let same_turn = previous.filter(|seen| {
        seen.status == ThreadStatus::Thinking
            && now.status == ThreadStatus::Thinking
            && seen.transcript_len == now.transcript_len
    });
    let still_since = same_turn.map_or(at, |seen| seen.still_since);
    let mut stall_reported = same_turn.is_some_and(|seen| seen.stall_reported);
    if let Some(len) = now.transcript_len
        && same_turn.is_some()
        && !stall_reported
        && at.saturating_duration_since(still_since) >= STALL_AFTER
    {
        events.push(Event::Stall {
            still_for: at.saturating_duration_since(still_since),
            transcript_len: len,
        });
        stall_reported = true;
    }
    (
        Seen {
            status: now.status,
            runs_ended: now.runs_ended,
            pane_exit: now.pane_exit,
            transcript_len: now.transcript_len,
            still_since,
            stall_reported,
        },
        events,
    )
}

impl SplitlaneApp {
    /// Journal what changed in the rail of every session since the last
    /// pass. `current` and `terminals` are what `publish_rail_changes` just
    /// gathered.
    pub(crate) fn journal_rail_changes(
        &mut self,
        current: &[(u64, Option<u64>, RailSnapshot)],
        terminals: &HashMap<u64, Entity<TerminalView>>,
        cx: &App,
    ) {
        let at = Instant::now();
        let mut journal_seen = std::mem::take(&mut self.journal_seen);
        for (sid, thread_id, snapshot) in current {
            let Some(thread_id) = *thread_id else {
                continue;
            };
            let facts = Facts {
                status: snapshot.status,
                runs_ended: snapshot.runs_ended,
                last_outcome: snapshot
                    .last_outcome
                    .map(crate::rail_state::RunOutcome::wire_str),
                pane_exit: terminals
                    .get(sid)
                    .and_then(|terminal| terminal.read(cx).terminal.exited),
                transcript_len: self
                    .turn_ends
                    .get(&thread_id)
                    .and_then(|watch| watch.file_len),
            };
            let (seen, events) = step(journal_seen.get(&thread_id).copied(), facts, at);
            journal_seen.insert(thread_id, seen);
            for event in events {
                let (name, fields) = match event {
                    Event::TurnStart => ("turn_start", serde_json::json!({})),
                    Event::TurnEnd {
                        outcome,
                        runs_ended,
                    } => (
                        "turn_end",
                        serde_json::json!({ "outcome": outcome, "runs_ended": runs_ended }),
                    ),
                    // The question itself is the agent's words, and may quote
                    // what it was asked: only its length.
                    Event::Waiting => (
                        "waiting",
                        serde_json::json!({
                            "source": snapshot.source.wire_str(),
                            "message_bytes": snapshot.message.as_ref().map(String::len),
                        }),
                    ),
                    Event::PaneExit(code) => ("pane_exit", serde_json::json!({ "code": code })),
                    Event::Stall {
                        still_for,
                        transcript_len,
                    } => (
                        "stall",
                        serde_json::json!({
                            "still_for_s": still_for.as_secs(),
                            "transcript_bytes": transcript_len,
                        }),
                    ),
                };
                self.journal(name, thread_id, fields);
            }
        }
        // A closed session takes its line of sight with it. One that is only
        // out of sight for a while - no terminal until it is woken - keeps
        // it, or its turn would be journaled as starting again.
        journal_seen.retain(|thread_id, _| {
            crate::project::find_surface(&self.workspaces, *thread_id).is_some()
        });
        self.journal_seen = journal_seen;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(status: ThreadStatus, runs_ended: u64, transcript_len: Option<u64>) -> Facts {
        Facts {
            status,
            runs_ended,
            last_outcome: Some("finished"),
            pane_exit: None,
            transcript_len,
        }
    }

    #[test]
    fn a_session_seen_idle_for_the_first_time_says_nothing() {
        let (_, events) = step(None, facts(ThreadStatus::Idle, 3, None), Instant::now());
        assert!(events.is_empty());
    }

    #[test]
    fn a_turn_is_journaled_where_it_starts_and_where_it_ends() {
        let t0 = Instant::now();
        let (seen, _) = step(None, facts(ThreadStatus::Idle, 0, None), t0);
        let (seen, events) = step(Some(seen), facts(ThreadStatus::Thinking, 0, None), t0);
        assert_eq!(events, vec![Event::TurnStart]);
        let (seen, events) = step(Some(seen), facts(ThreadStatus::Thinking, 0, None), t0);
        assert!(events.is_empty(), "a turn starts once");
        let (_, events) = step(Some(seen), facts(ThreadStatus::Idle, 1, None), t0);
        assert_eq!(
            events,
            vec![Event::TurnEnd {
                outcome: Some("finished"),
                runs_ended: 1
            }]
        );
    }

    #[test]
    fn a_wait_is_journaled_once() {
        let t0 = Instant::now();
        let (seen, _) = step(None, facts(ThreadStatus::Thinking, 0, None), t0);
        let (seen, events) = step(
            Some(seen),
            facts(ThreadStatus::WaitingForInput, 0, None),
            t0,
        );
        assert_eq!(events, vec![Event::Waiting]);
        let (_, events) = step(
            Some(seen),
            facts(ThreadStatus::WaitingForInput, 0, None),
            t0,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn a_pane_exit_is_journaled_with_its_code() {
        let t0 = Instant::now();
        let (seen, _) = step(None, facts(ThreadStatus::Idle, 0, None), t0);
        let exited = Facts {
            pane_exit: Some(1),
            ..facts(ThreadStatus::Idle, 0, None)
        };
        let (seen, events) = step(Some(seen), exited, t0);
        assert_eq!(events, vec![Event::PaneExit(1)]);
        let (_, events) = step(Some(seen), exited, t0);
        assert!(events.is_empty());
    }

    #[test]
    fn a_turn_whose_transcript_stands_still_is_a_stall_once() {
        let t0 = Instant::now();
        let (seen, _) = step(None, facts(ThreadStatus::Thinking, 0, Some(100)), t0);
        let (seen, events) = step(
            Some(seen),
            facts(ThreadStatus::Thinking, 0, Some(100)),
            t0 + STALL_AFTER - Duration::from_secs(1),
        );
        assert!(events.is_empty());
        let (seen, events) = step(
            Some(seen),
            facts(ThreadStatus::Thinking, 0, Some(100)),
            t0 + STALL_AFTER,
        );
        assert_eq!(
            events,
            vec![Event::Stall {
                still_for: STALL_AFTER,
                transcript_len: 100
            }]
        );
        let (_, events) = step(
            Some(seen),
            facts(ThreadStatus::Thinking, 0, Some(100)),
            t0 + STALL_AFTER * 2,
        );
        assert!(events.is_empty(), "a stall is told once");
    }

    #[test]
    fn a_transcript_that_grows_restarts_the_clock() {
        let t0 = Instant::now();
        let (seen, _) = step(None, facts(ThreadStatus::Thinking, 0, Some(100)), t0);
        let (seen, _) = step(
            Some(seen),
            facts(ThreadStatus::Thinking, 0, Some(200)),
            t0 + STALL_AFTER - Duration::from_secs(1),
        );
        let (_, events) = step(
            Some(seen),
            facts(ThreadStatus::Thinking, 0, Some(200)),
            t0 + STALL_AFTER + Duration::from_secs(1),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn no_transcript_no_stall() {
        let t0 = Instant::now();
        let (seen, _) = step(None, facts(ThreadStatus::Thinking, 0, None), t0);
        let (_, events) = step(
            Some(seen),
            facts(ThreadStatus::Thinking, 0, None),
            t0 + STALL_AFTER * 2,
        );
        assert!(events.is_empty());
    }
}
