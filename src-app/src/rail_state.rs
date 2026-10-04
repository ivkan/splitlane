//! What the rail knows about one agent surface, in the shape a script reads.
//!
//! `surface.status` used to answer from the hook's session table alone, which
//! is the weaker of the two sources the rail draws from: during a permission
//! ask the last hook frame is a tool call, so the hook says the agent is
//! working while the rail says it is waiting for a person. A script that waits
//! on the hook's word repeats the mistake the rail was built to stop making.
//! This module is the rail's own word on the wire, with who decided it and how
//! far that source can be trusted.
//!
//! Everything here is plain data and pure functions, so the server, the CLI
//! (the same binary) and the tests share one vocabulary and none of it needs a
//! window.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::project::ThreadStatus;

/// The event type a change of [`RailSnapshot::status`] or
/// [`RailSnapshot::runs_ended`] is published under.
pub const RAIL_EVENT_TYPE: &str = "surface.rail";

/// The five words the rail shows, as they travel.
///
/// Not the hook's vocabulary (`thinking`, `waiting_for_input`, `finished`,
/// ...): those name frames a hook sent, these name what the row says.
pub fn status_word(status: ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Starting => "starting",
        ThreadStatus::Thinking => "running",
        ThreadStatus::WaitingForInput => "waiting",
        ThreadStatus::Idle => "idle",
        ThreadStatus::Failed => "failed",
    }
}

/// Who last decided the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailSource {
    /// The agent's own file: its status file or its transcript.
    Detector,
    /// An `ai.*` hook frame.
    Hook,
    /// Bytes arriving from the pane, which can only ever say `running`.
    PtyFlow,
    /// Nothing has spoken for this surface yet.
    None,
}

impl RailSource {
    pub fn wire_str(self) -> &'static str {
        match self {
            RailSource::Detector => "detector",
            RailSource::Hook => "hook",
            RailSource::PtyFlow => "pty_flow",
            RailSource::None => "none",
        }
    }

    /// How much a waiting script may build on this source.
    ///
    /// `T1` reads what the agent wrote for itself. `T2` is the vendor's hook,
    /// which reports a turn ending but depends on a contract the vendor keeps
    /// changing. `T3` has no turn boundary at all: output arriving proves
    /// work, and output stopping proves nothing - an agent thinking, an agent
    /// waiting for a person and an agent that has finished are all silent.
    pub fn tier(self) -> &'static str {
        match self {
            RailSource::Detector => "T1",
            RailSource::Hook => "T2",
            RailSource::PtyFlow | RailSource::None => "T3",
        }
    }
}

/// How the last run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Finished,
    Failed,
}

impl RunOutcome {
    pub fn wire_str(self) -> &'static str {
        match self {
            RunOutcome::Finished => "finished",
            RunOutcome::Failed => "failed",
        }
    }
}

/// The end of a turn, as the agent's own file records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnd {
    /// An id that differs between turns: the closing record's `uuid` for
    /// Claude Code, the `turn_id` for Codex.
    pub marker: String,
    /// The file says this turn ended in an error.
    pub failed: bool,
}

/// The part of the rail's answer that is kept on the surface's record.
///
/// Runtime only: a count of runs that ended while this process watched is not
/// a fact about the next launch.
#[derive(Debug, Clone, Default)]
pub struct RailRecord {
    /// Runs that have ended on this surface since it was created. Only ever
    /// grows, so a script can take its value before sending a prompt and wait
    /// for a larger one.
    pub runs_ended: u64,
    pub last_outcome: Option<RunOutcome>,
    /// The newest turn end the agent's own file records, when this build
    /// reads that file.
    pub turn_marker: Option<String>,
    /// The question a hook reported with the current wait, when it gave one.
    pub waiting_message: Option<String>,
    /// The shim reported that the agent binary exited and nothing has started
    /// in the pane since.
    pub agent_exited: bool,
}

impl RailRecord {
    pub fn record_run_end(&mut self, outcome: RunOutcome) {
        self.runs_ended = self.runs_ended.saturating_add(1);
        self.last_outcome = Some(outcome);
    }
}

/// One surface's answer, ready to serialise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailSnapshot {
    pub status: ThreadStatus,
    pub source: RailSource,
    pub runs_ended: u64,
    pub last_outcome: Option<RunOutcome>,
    pub turn_marker: Option<String>,
    pub exited: bool,
    pub message: Option<String>,
}

impl RailSnapshot {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": status_word(self.status),
            "source": self.source.wire_str(),
            "tier": self.source.tier(),
            "runs_ended": self.runs_ended,
            "last_outcome": self.last_outcome.map(RunOutcome::wire_str),
            "turn_marker": self.turn_marker,
            "exited": self.exited,
            // A question belongs to a wait. Kept off every other word so a
            // stale one cannot be read as current.
            "message": self
                .message
                .as_deref()
                .filter(|_| self.status == ThreadStatus::WaitingForInput),
        })
    }
}

/// The events owed for this set of surfaces, given what was last published.
///
/// One function decides, and it compares against what it last said rather than
/// being told what changed: the word is written from many places, and a
/// publisher that trusts each of them to report its own change is a publisher
/// one of them will forget. A surface seen for the first time is published,
/// so a subscriber that joins late still learns where things stand from the
/// next change of anything.
///
/// It must not be driven from rendering. A minimised or covered window draws no
/// frames while the state pass and the hooks keep running on their own timers,
/// so an event tied to a frame would go quiet exactly when nobody is looking.
pub fn rail_changes(
    published: &mut HashMap<u64, (ThreadStatus, u64)>,
    current: &[(u64, Option<u64>, RailSnapshot)],
    ts: u64,
) -> Vec<(u64, serde_json::Value)> {
    let mut events = Vec::new();
    for (surface_id, thread_id, snapshot) in current {
        let now = (snapshot.status, snapshot.runs_ended);
        if published.insert(*surface_id, now) == Some(now) {
            continue;
        }
        events.push((
            *surface_id,
            serde_json::json!({
                "type": RAIL_EVENT_TYPE,
                "surface_id": surface_id,
                "thread_id": thread_id,
                "status": status_word(snapshot.status),
                "source": snapshot.source.wire_str(),
                "runs_ended": snapshot.runs_ended,
                "last_outcome": snapshot.last_outcome.map(RunOutcome::wire_str),
                "ts": ts,
            }),
        ));
    }
    // A closed surface must not leave an entry behind, and must be published
    // afresh if its id is ever reused.
    published.retain(|surface_id, _| current.iter().any(|(id, _, _)| id == surface_id));
    events
}

/// What the state pass remembers about one surface's turn-end marker.
///
/// It exists for the run the pass never saw. The pass samples every two
/// seconds and reports a finished run only for a surface it watched working, so
/// a turn that starts and ends between two samples leaves no trace in the
/// status and a script waiting for it would wait until its timeout. The agent's
/// own file still records that the turn ended, and this is the bookkeeping that
/// turns that record into a counted run - without touching the row's unread
/// mark or the notification, which stay with runs somebody could have watched.
#[derive(Debug, Clone, Default)]
pub struct TurnEndWatch {
    /// The file's length when the marker was last read, so an idle surface's
    /// unchanged file is not parsed again every pass.
    pub file_len: Option<u64>,
    /// The newest turn end the file records.
    pub current: Option<TurnEnd>,
    /// The marker already accounted for in `runs_ended`.
    counted: Option<String>,
    /// Whether `counted` has been taken at all. A surface seen for the first
    /// time adopts whatever its file already holds: a turn that ended before
    /// this process was watching is not a run that ended now.
    primed: bool,
    /// When a watched run last ended. A marker that moves just after belongs
    /// to that run - the status can settle a moment before the record lands.
    settled_at: Option<Instant>,
    /// A marker that moved while the surface was idle, and when that was first
    /// seen. Counted once it has stood for the confirmation span.
    pending: Option<(String, Instant)>,
}

impl TurnEndWatch {
    /// Take in what this pass read. `None` leaves the standing marker alone:
    /// it means "not read" or "not in the window", never "there is none".
    pub fn observe(&mut self, file_len: u64, end: Option<TurnEnd>) {
        self.file_len = Some(file_len);
        if end.is_some() {
            self.current = end;
        }
    }

    /// The pass watched a run end. Whatever the file says now is that run's.
    pub fn settle(&mut self, at: Instant) {
        self.counted = self.current.as_ref().map(|end| end.marker.clone());
        self.primed = true;
        self.settled_at = Some(at);
        self.pending = None;
    }

    /// Whether a turn the pass never saw running has ended, as of `at`.
    ///
    /// `true` exactly once per such turn, and only after the marker has stood
    /// beside an idle word for `confirm`: the same span a watched run is held
    /// for, and for the same reason - a working session reads idle for about a
    /// second each time a background task ends.
    pub fn unseen_turn_ended(
        &mut self,
        status: ThreadStatus,
        at: Instant,
        confirm: Duration,
    ) -> bool {
        let Some(marker) = self.current.as_ref().map(|end| end.marker.clone()) else {
            return false;
        };
        if !self.primed {
            self.primed = true;
            self.counted = Some(marker);
            return false;
        }
        if self.counted.as_deref() == Some(marker.as_str()) {
            self.pending = None;
            return false;
        }
        if status != ThreadStatus::Idle {
            // A run in flight will be counted when the pass sees it end; the
            // marker is adopted there.
            self.pending = None;
            return false;
        }
        if self
            .settled_at
            .is_some_and(|settled| at.saturating_duration_since(settled) < confirm)
        {
            self.counted = Some(marker);
            self.pending = None;
            return false;
        }
        let first = match &self.pending {
            Some((pending, first)) if *pending == marker => *first,
            _ => {
                self.pending = Some((marker.clone(), at));
                at
            }
        };
        if at.saturating_duration_since(first) < confirm {
            return false;
        }
        self.counted = Some(marker);
        self.pending = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIRM: Duration = Duration::from_secs(3);

    fn end(marker: &str) -> Option<TurnEnd> {
        Some(TurnEnd {
            marker: marker.to_string(),
            failed: false,
        })
    }

    fn snapshot(status: ThreadStatus, runs_ended: u64) -> RailSnapshot {
        RailSnapshot {
            status,
            source: RailSource::Detector,
            runs_ended,
            last_outcome: None,
            turn_marker: None,
            exited: false,
            message: None,
        }
    }

    /// The words on the wire are the rail's five, not the hook's.
    #[test]
    fn the_wire_words_are_the_rails() {
        let words: Vec<&str> = [
            ThreadStatus::Starting,
            ThreadStatus::Thinking,
            ThreadStatus::WaitingForInput,
            ThreadStatus::Idle,
            ThreadStatus::Failed,
        ]
        .into_iter()
        .map(status_word)
        .collect();
        assert_eq!(words, ["starting", "running", "waiting", "idle", "failed"]);
    }

    #[test]
    fn the_tier_follows_the_source() {
        assert_eq!(RailSource::Detector.tier(), "T1");
        assert_eq!(RailSource::Hook.tier(), "T2");
        assert_eq!(RailSource::PtyFlow.tier(), "T3");
        assert_eq!(RailSource::None.tier(), "T3");
    }

    /// A question is only reported while it is being asked.
    #[test]
    fn a_message_is_only_shown_with_a_wait() {
        let mut snap = snapshot(ThreadStatus::WaitingForInput, 0);
        snap.message = Some("Allow Bash?".to_string());
        assert_eq!(snap.to_json()["message"], "Allow Bash?");
        snap.status = ThreadStatus::Thinking;
        assert!(snap.to_json()["message"].is_null());
    }

    /// One change, one event; no change, none. And it needs no window: the
    /// whole decision is this function over plain data.
    #[test]
    fn a_change_is_published_exactly_once() {
        let mut published = HashMap::new();
        let idle = [(7, Some(70), snapshot(ThreadStatus::Idle, 0))];
        assert_eq!(
            rail_changes(&mut published, &idle, 1).len(),
            1,
            "first sight"
        );
        assert!(rail_changes(&mut published, &idle, 2).is_empty(), "repeat");

        let running = [(7, Some(70), snapshot(ThreadStatus::Thinking, 0))];
        let events = rail_changes(&mut published, &running, 3);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, 7);
        assert_eq!(events[0].1["type"], "surface.rail");
        assert_eq!(events[0].1["status"], "running");
        assert_eq!(events[0].1["thread_id"], 70);
        assert!(rail_changes(&mut published, &running, 4).is_empty());
    }

    /// A run that ended and left the word where it was is still a change: the
    /// counter is what a waiting script is watching.
    #[test]
    fn a_counted_run_is_a_change_even_when_the_word_stands() {
        let mut published = HashMap::new();
        rail_changes(
            &mut published,
            &[(7, None, snapshot(ThreadStatus::Idle, 1))],
            1,
        );
        let events = rail_changes(
            &mut published,
            &[(7, None, snapshot(ThreadStatus::Idle, 2))],
            2,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1["runs_ended"], 2);
    }

    #[test]
    fn a_closed_surface_is_forgotten() {
        let mut published = HashMap::new();
        rail_changes(
            &mut published,
            &[(7, None, snapshot(ThreadStatus::Idle, 0))],
            1,
        );
        rail_changes(&mut published, &[], 2);
        assert!(published.is_empty());
    }

    /// What the file already held when the surface was first seen is history,
    /// not a run that just ended.
    #[test]
    fn the_first_marker_seen_is_adopted_not_counted() {
        let mut watch = TurnEndWatch::default();
        let t0 = Instant::now();
        watch.observe(10, end("a"));
        assert!(!watch.unseen_turn_ended(ThreadStatus::Idle, t0, CONFIRM));
        assert!(!watch.unseen_turn_ended(ThreadStatus::Idle, t0 + CONFIRM * 2, CONFIRM));
    }

    /// The case this exists for: a turn too short for the pass to catch
    /// running. The marker moved, the word never left idle, and the run is
    /// counted once the marker has stood for the span.
    #[test]
    fn a_turn_the_pass_never_saw_running_is_counted_after_the_span() {
        let mut watch = TurnEndWatch::default();
        let t0 = Instant::now();
        watch.observe(10, end("a"));
        watch.unseen_turn_ended(ThreadStatus::Idle, t0, CONFIRM);

        watch.observe(20, end("b"));
        let seen = t0 + Duration::from_secs(10);
        assert!(!watch.unseen_turn_ended(ThreadStatus::Idle, seen, CONFIRM));
        assert!(!watch.unseen_turn_ended(
            ThreadStatus::Idle,
            seen + Duration::from_secs(2),
            CONFIRM
        ));
        assert!(watch.unseen_turn_ended(ThreadStatus::Idle, seen + CONFIRM, CONFIRM));
        assert!(
            !watch.unseen_turn_ended(ThreadStatus::Idle, seen + CONFIRM * 2, CONFIRM),
            "counted once"
        );
    }

    /// A working session reads idle for about a second when a background task
    /// ends, and the turn before it may have just written its closing record.
    /// A blip shorter than the span counts nothing.
    #[test]
    fn an_idle_blip_shorter_than_the_span_counts_nothing() {
        let mut watch = TurnEndWatch::default();
        let t0 = Instant::now();
        watch.observe(10, end("a"));
        watch.unseen_turn_ended(ThreadStatus::Idle, t0, CONFIRM);

        watch.observe(20, end("b"));
        let blip = t0 + Duration::from_secs(10);
        assert!(!watch.unseen_turn_ended(ThreadStatus::Idle, blip, CONFIRM));
        // Working again inside the span: the hold is dropped, not carried.
        assert!(!watch.unseen_turn_ended(
            ThreadStatus::Thinking,
            blip + Duration::from_secs(1),
            CONFIRM
        ));
        assert!(!watch.unseen_turn_ended(
            ThreadStatus::Idle,
            blip + Duration::from_secs(4),
            CONFIRM
        ));
    }

    /// A run the pass watched is counted by the pass. Its marker must not be
    /// counted a second time, whether it was on disk when the run ended or
    /// landed a moment later.
    #[test]
    fn a_watched_run_is_not_counted_twice() {
        let t0 = Instant::now();

        let mut on_disk = TurnEndWatch::default();
        on_disk.observe(10, end("a"));
        on_disk.unseen_turn_ended(ThreadStatus::Idle, t0, CONFIRM);
        on_disk.observe(20, end("b"));
        on_disk.settle(t0);
        assert!(!on_disk.unseen_turn_ended(ThreadStatus::Idle, t0 + CONFIRM * 2, CONFIRM));

        let mut late = TurnEndWatch::default();
        late.observe(10, end("a"));
        late.unseen_turn_ended(ThreadStatus::Idle, t0, CONFIRM);
        late.settle(t0);
        late.observe(20, end("b"));
        let after = t0 + Duration::from_secs(1);
        assert!(!late.unseen_turn_ended(ThreadStatus::Idle, after, CONFIRM));
        assert!(!late.unseen_turn_ended(ThreadStatus::Idle, after + CONFIRM * 2, CONFIRM));
    }

    /// A read that found no marker says nothing about the one already known.
    #[test]
    fn a_read_without_a_marker_keeps_the_standing_one() {
        let mut watch = TurnEndWatch::default();
        watch.observe(10, end("a"));
        watch.observe(20, None);
        assert_eq!(watch.current, end("a"));
        assert_eq!(watch.file_len, Some(20));
    }
}
