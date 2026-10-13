//! What the program in a pane reports about itself through OSC 7501.
//!
//! The protocol is read in `splitlane_host::program_status`; this is where a
//! pane keeps what was reported. Nothing here decides anything: the report is
//! kept beside the pane so the agent-state pass can write it into its trace
//! next to what its own sources concluded, which is how the two get compared
//! before a report is ever allowed to move the rail.
//!
//! A report is a claim by whatever printed it, and anything that can print to
//! the pane can print one.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use splitlane_host::program_status::{self as protocol, Change, Event, Scanner};

#[cfg(test)]
use splitlane_host::program_status::Kind;
pub(crate) use splitlane_host::program_status::{Record, State};

/// Changes kept for the trace between two passes. A program can report as
/// fast as it can print, and a pass that nobody traces never empties this.
const JOURNAL_MAX: usize = 32;

/// One thing a pane's program said, or that was done to its record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Said {
    Root(Record),
    /// The root record was removed, by the program.
    Cleared,
    /// A new shell prompt ended a record that ends with its program.
    EndedByPrompt,
    /// A report for a record beneath the root. Only noted: the pane keeps
    /// the root record alone.
    Child {
        id: String,
        state: Option<State>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) at: Instant,
    pub(crate) said: Said,
}

/// What a pane's program has reported, as of now.
#[derive(Debug, Default)]
pub(crate) struct ProgramStatus {
    root: Option<(Record, Instant)>,
    /// A report was applied at least once. Tells a program that never
    /// speaks the protocol from one that cleared its record.
    heard: bool,
    journal: VecDeque<Entry>,
    journal_lost: usize,
}

/// What the trace is given: the root record and everything said since the
/// last time it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reported {
    pub(crate) root: Option<(Record, Instant)>,
    pub(crate) since_last: Vec<Entry>,
    /// Changes that did not fit and were dropped, oldest first.
    pub(crate) lost: usize,
}

impl ProgramStatus {
    /// `None` for a pane whose program never reported anything.
    pub(crate) fn take_for_trace(&mut self) -> Option<Reported> {
        self.heard.then(|| Reported {
            root: self.root.clone(),
            since_last: self.journal.drain(..).collect(),
            lost: std::mem::take(&mut self.journal_lost),
        })
    }

    fn note(&mut self, said: Said) {
        self.heard = true;
        if self.journal.len() == JOURNAL_MAX {
            self.journal.pop_front();
            self.journal_lost += 1;
        }
        self.journal.push_back(Entry {
            at: Instant::now(),
            said,
        });
    }

    fn apply(&mut self, report: protocol::Report) {
        match (report.id, report.change) {
            (None, Change::Set(record)) => {
                self.root = Some((record.clone(), Instant::now()));
                self.note(Said::Root(record));
            }
            (None, Change::Clear) => {
                self.root = None;
                self.note(Said::Cleared);
            }
            (Some(id), change) => self.note(Said::Child {
                id,
                state: match change {
                    Change::Set(record) => Some(record.state),
                    Change::Clear => None,
                },
            }),
        }
    }

    fn prompt_started(&mut self) {
        if self
            .root
            .as_ref()
            .is_some_and(|(record, _)| record.state.ends_with_the_program())
        {
            self.root = None;
            self.note(Said::EndedByPrompt);
        }
    }
}

pub(crate) type SharedProgramStatus = Arc<Mutex<ProgramStatus>>;

pub(crate) fn shared() -> SharedProgramStatus {
    Arc::new(Mutex::new(ProgramStatus::default()))
}

/// Reads a pane's output for reports, on the thread that reads the PTY.
pub(crate) struct StatusTap {
    scanner: Scanner,
    shared: SharedProgramStatus,
}

impl StatusTap {
    pub(crate) fn new(shared: SharedProgramStatus) -> Self {
        Self {
            scanner: Scanner::new(),
            shared,
        }
    }

    /// Read a chunk of output. `answer` is handed what to write back to the
    /// program when it asked whether its reports are read.
    ///
    /// Whoever holds the PTY must write that before anything its emulator
    /// answers to the same chunk: the program asks what the terminal is
    /// right after, and takes whichever answer arrives first. A pane whose
    /// PTY the session host holds passes an `answer` that does nothing,
    /// because the host has answered already.
    pub(crate) fn advance(&mut self, bytes: &[u8], answer: &mut impl FnMut(&'static [u8])) {
        let Self { scanner, shared } = self;
        scanner.feed(bytes, &mut |event| match event {
            Event::Query { bel } => answer(protocol::reply(bel)),
            Event::Report(body) => {
                if let Some(report) = protocol::parse(body) {
                    shared
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .apply(report);
                }
            }
        });
    }

    /// The shell printed a new prompt, so the program that was working or
    /// blocked is gone, whether or not it said so on its way out.
    pub(crate) fn prompt_started(&self) {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .prompt_started();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap() -> (StatusTap, SharedProgramStatus) {
        let shared = shared();
        (StatusTap::new(shared.clone()), shared)
    }

    fn feed(tap: &mut StatusTap, bytes: &[u8]) -> Vec<u8> {
        let mut answers = Vec::new();
        tap.advance(bytes, &mut |answer| answers.extend_from_slice(answer));
        answers
    }

    fn root(shared: &SharedProgramStatus) -> Option<State> {
        let status = shared.lock().expect("status");
        status.root.as_ref().map(|(record, _)| record.state)
    }

    #[test]
    fn the_question_is_answered_and_a_report_is_kept() {
        let (mut tap, shared) = tap();
        assert_eq!(shared.lock().expect("status").take_for_trace(), None);

        let answers = feed(&mut tap, b"\x1b]7501;?\x1b\\\x1b[c");
        assert_eq!(answers, protocol::reply(false));
        // Asking is not reporting.
        assert_eq!(shared.lock().expect("status").take_for_trace(), None);

        assert_eq!(feed(&mut tap, b"\x1b]7501;state=working:app=x\x07"), b"");
        assert_eq!(root(&shared), Some(State::Working));
    }

    #[test]
    fn the_trace_gets_every_change_once() {
        let (mut tap, shared) = tap();
        feed(
            &mut tap,
            b"\x1b]7501;state=working\x07\x1b]7501;state=blocked:kind=question\x07",
        );
        let reported = shared
            .lock()
            .expect("status")
            .take_for_trace()
            .expect("heard");
        let states: Vec<_> = reported
            .since_last
            .iter()
            .map(|entry| match &entry.said {
                Said::Root(record) => (record.state, record.kind),
                other => unreachable!("{other:?}"),
            })
            .collect();
        assert_eq!(
            states,
            [
                (State::Working, None),
                (State::Blocked, Some(Kind::Question))
            ]
        );
        let again = shared
            .lock()
            .expect("status")
            .take_for_trace()
            .expect("heard");
        assert!(again.since_last.is_empty());
        assert_eq!(
            again.root.map(|(record, _)| record.state),
            Some(State::Blocked)
        );
    }

    #[test]
    fn a_child_record_never_lands_on_the_root() {
        let (mut tap, shared) = tap();
        feed(&mut tap, b"\x1b]7501;state=working\x07");
        feed(&mut tap, b"\x1b]7501;state=blocked:id=worker/1\x07");
        feed(&mut tap, b"\x1b]7501;state=clear:id=worker\x07");
        assert_eq!(root(&shared), Some(State::Working));
        feed(&mut tap, b"\x1b]7501;state=clear\x07");
        assert_eq!(root(&shared), None);
    }

    #[test]
    fn a_new_prompt_ends_work_and_leaves_a_result() {
        let (mut tap, shared) = tap();
        for (report, left) in [
            (&b"\x1b]7501;state=working\x07"[..], None),
            (b"\x1b]7501;state=blocked\x07", None),
            (b"\x1b]7501;state=done\x07", Some(State::Done)),
            (b"\x1b]7501;state=error\x07", Some(State::Error)),
            (b"\x1b]7501;state=idle\x07", Some(State::Idle)),
        ] {
            feed(&mut tap, report);
            tap.prompt_started();
            assert_eq!(root(&shared), left, "{report:?}");
        }
    }

    #[test]
    fn a_flood_of_reports_keeps_the_newest() {
        let (mut tap, shared) = tap();
        for _ in 0..JOURNAL_MAX + 5 {
            feed(&mut tap, b"\x1b]7501;state=working\x07");
        }
        feed(&mut tap, b"\x1b]7501;state=done\x07");
        let reported = shared
            .lock()
            .expect("status")
            .take_for_trace()
            .expect("heard");
        assert_eq!(reported.since_last.len(), JOURNAL_MAX);
        assert_eq!(reported.lost, 6);
        assert!(matches!(
            reported.since_last.last().map(|entry| &entry.said),
            Some(Said::Root(record)) if record.state == State::Done
        ));
    }
}
