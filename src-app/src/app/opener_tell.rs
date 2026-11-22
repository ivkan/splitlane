//! Telling a session that a session it opened has ended its turn.
//!
//! The end of a run its opener started is the opener's to read and is not
//! announced to a person ([`crate::app::orchestration::run_is_news`]). An
//! opener blocked in `splitlane wait` hears it there. One that ended its own
//! turn without waiting heard nothing, and the work stopped until a person
//! said so. Here the app says so: one line written into the opener and
//! submitted. The reasoning is in `docs/internals/design-decisions.md`, "An
//! opener is told when its session's turn ends".

use gpui::Context;

use crate::SplitlaneApp;
use crate::project::ThreadStatus;

/// One ended turn an opener has not been told about yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tell {
    /// The session whose turn ended.
    pub thread_id: u64,
    /// Set when the opener has had another way to hear of it: something
    /// was waiting on the session when its turn ended, or the opener asked
    /// about it afterwards. Such a line is not written while the opener may
    /// still be acting on what it heard ([`held_step`]).
    pub held: Option<Held>,
    /// Every end this line is about was a turn the session took up by
    /// itself: none was the end of work the opener gave it.
    pub by_itself: bool,
    /// How many of the ends this line is about were taken up by itself.
    pub self_turns: u32,
    /// When the newest of those ends was noted.
    pub noted_at: std::time::Instant,
}

/// Where an opener stood when it last had another way to hear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Held {
    /// When: the first question about the session, or the last moment
    /// something was seen waiting on it.
    pub since: std::time::Instant,
    /// How many of the opener's own turns had ended by then. The rail is
    /// sampled, and a turn shorter than the gap between two samples is seen
    /// only in this count.
    pub opener_runs: u64,
    /// The opener was in a turn of its own then. That turn says nothing
    /// about whether it heard: it was under way before there was anything
    /// to hear.
    pub opener_at_work: bool,
    /// The opener has read the session's answer since the line was held.
    pub read_answer: bool,
}

/// Where an opener stands now, for a line held back from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenerNow {
    pub status: ThreadStatus,
    /// How many of its turns have ended since the line was held.
    pub turns_since: u64,
    /// Commands it started in the background that are still running.
    pub background_shells: u32,
}

/// How long an opener that had another way to hear is given to act on it.
///
/// A harness that calls its agent back when a background `splitlane wait`
/// ends does so within seconds, and the opener is then at work. An opener
/// still idle after this long did not hear: the wait was wrapped in a script
/// that went on running, or its result was read by nobody.
const HELD_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// How long after one line the next may be written into the same opener.
///
/// The rail is sampled, so for a moment after a line is submitted the opener
/// still reads `idle`. A second line written then would land in a turn that
/// is already starting.
const TELL_GAP: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a session that keeps taking turns up by itself must go without
/// ending another before its opener is told, while a command of its own is
/// still running.
///
/// A session waiting on its own long command ends a turn each time the
/// command wakes it, and each end used to be one line, one turn of the
/// opener: 95 lines for one task in one case. The line waits until the
/// session has nothing of its own left running, or has gone this long
/// without ending another turn, and then names them all at once.
const SELF_TURN_QUIET: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Whether the line about turns a session took up by itself waits for the
/// session to settle.
///
/// `session_busy` is the session at work again, or idle with a command of its
/// own still running: either way another end is coming, and this line would
/// be followed by another. A line that is about the end of work the opener
/// gave is never kept back here.
pub(crate) fn self_turn_waits(
    by_itself: bool,
    session_busy: bool,
    quiet_for: std::time::Duration,
) -> bool {
    by_itself && session_busy && quiet_for < SELF_TURN_QUIET
}

/// The most of a session's title that is put on another agent's input line.
/// A title is whatever the session's agent set it to.
const TITLE_MAX: usize = 60;

/// What is known about an opener at the moment a line could be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenerFacts {
    pub status: ThreadStatus,
    /// The agent's shim reported its exit.
    pub agent_exited: bool,
    /// The agent's own process was under the pane at the last state pass.
    pub agent_found: bool,
    /// A person typed into the opener and has not pressed Enter since.
    pub keyboard_draft: bool,
    /// The opener's interface shows a place to type into.
    pub entry_open: bool,
    /// A line was written into this opener less than [`TELL_GAP`] ago.
    pub just_told: bool,
}

/// What becomes of a line whose opener had another way to hear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldStep {
    /// Not yet: the opener may still be about to act, or something is still
    /// waiting on the session.
    Keep,
    /// The opener is at work: it heard, or was given something else to do
    /// by somebody who knows.
    Drop,
    /// The opener has sat idle since: it is owed the line after all.
    Due,
}

/// Whether a held line is still held.
///
/// `watched` is asked now and not when the turn ended: a wait that outlasts
/// the turn (one waiting out the session's background command) is still the
/// opener's way to hear, and a line written under it would be the second
/// message about one turn.
pub(crate) fn held_step(
    held: Held,
    opener: OpenerNow,
    held_for: std::time::Duration,
    watched: bool,
) -> HeldStep {
    let at_work = opener.status == ThreadStatus::Thinking;
    if held.opener_at_work {
        // The turn it was in is not evidence, so one more is needed: a turn
        // begun after that one ended.
        return match opener.turns_since {
            0 => HeldStep::Keep,
            1 if at_work => HeldStep::Drop,
            1 if watched => HeldStep::Keep,
            // Nothing of its own is left running: what waited on the session
            // ended inside that turn, where the agent read it. So it did if
            // it went on to read the session's answer.
            1 if opener.background_shells == 0 || held.read_answer => HeldStep::Drop,
            1 if held_for < HELD_FOR => HeldStep::Keep,
            1 => HeldStep::Due,
            _ => HeldStep::Drop,
        };
    }
    if at_work || opener.turns_since > 0 {
        HeldStep::Drop
    } else if watched || held_for < HELD_FOR {
        HeldStep::Keep
    } else {
        HeldStep::Due
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TellStep {
    Write,
    Hold,
    /// There is no agent left to tell.
    Drop,
}

/// Whether the line may go into the opener now.
///
/// A submitted line starts a turn, so it goes in only where a person
/// pressing Enter would start one cleanly: the agent is there, idle, showing
/// its input line, with nothing of a person's on it. A shell is never
/// written to: it would run the line as a command.
pub(crate) fn tell_step(facts: OpenerFacts) -> TellStep {
    // Both halves, as everywhere this is asked: the exit outlives an agent a
    // person started again by hand.
    if crate::app::orchestration::agent_has_left(facts.agent_exited, facts.agent_found) {
        return TellStep::Drop;
    }
    let ready = facts.status == ThreadStatus::Idle
        && facts.agent_found
        && facts.entry_open
        && !facts.keyboard_draft
        && !facts.just_told;
    if ready {
        TellStep::Write
    } else {
        TellStep::Hold
    }
}

/// One session named in the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Told {
    pub title: String,
    pub surface_id: u64,
    /// The session took the turn up by itself, woken by a command of its
    /// own: the opener did not start it and may not be expecting it.
    pub by_itself: bool,
    /// How many turns it took up by itself since the opener was last told.
    pub self_turns: u32,
    /// A command the session started is still running, so this answer may
    /// not be its last word: it can take another turn up by itself.
    pub command_running: bool,
}

/// The line itself. One line whatever the count: every line break typed into
/// an agent is a chance for it to submit half a message.
pub(crate) fn tell_text(sessions: &[Told]) -> String {
    let named = |told: &Told| {
        let short: String = told.title.chars().take(TITLE_MAX).collect();
        let turns = |count: u32| match count {
            1 => "1 turn".to_string(),
            count => format!("{count} turns"),
        };
        let mut notes = match (told.by_itself, told.self_turns) {
            (_, 0) => String::new(),
            (true, 1) => ", taken up by itself".to_string(),
            (true, count) => format!(", {} taken up by itself", turns(count)),
            (false, count) => format!(", then {} taken up by itself", turns(count)),
        };
        if told.command_running {
            notes.push_str(", a command of its own still running");
        }
        format!(
            "{} (surface {}{notes})",
            crate::app::send_answer::quoted_title(&short),
            told.surface_id
        )
    };
    match sessions {
        [one] => format!(
            "[Splitlane] {}, a session this one opened, has ended its turn. \
             Read its answer with: splitlane answer {}. \
             Sent by the app, not typed by a person.",
            named(one),
            one.surface_id
        ),
        many => format!(
            "[Splitlane] Sessions this one opened have ended their turns: {}. \
             Read each answer with: splitlane answer <surface>. \
             Sent by the app, not typed by a person.",
            many.iter().map(named).collect::<Vec<_>>().join(", ")
        ),
    }
}

impl SplitlaneApp {
    /// Note that the turn `thread_id` just ended was its opener's to read,
    /// for the opener to be told.
    ///
    /// When something is already waiting on this session by id the line is
    /// held, not written: that is the opener's own `splitlane wait`, which
    /// ends on the same event, and a second message about one turn would
    /// start a second turn. It is written later only to an opener that the
    /// wait did not wake ([`held_step`]).
    pub(crate) fn tell_opener_of(&mut self, thread_id: u64, cx: &mut Context<Self>) {
        if !self.cached_config.tell_opener_enabled() {
            return;
        }
        let Some(thread) = self.thread_by_id(thread_id) else {
            return;
        };
        let by_itself = thread.turn_began_unprompted;
        let Some(opener) = thread
            .opened_by
            .as_ref()
            .map(|by| by.id)
            .or_else(|| self.drive.driver_of(thread_id))
        else {
            return;
        };
        let watched = self.terminal_of(thread_id, cx).is_some_and(|terminal| {
            self.event_bus.is_watched(
                crate::rail_state::RAIL_EVENT_TYPE,
                terminal.entity_id().as_u64(),
            )
        });
        log::debug!(
            target: crate::app::agent_state_pass::TRACE,
            "#{thread_id} turn ended for its opener #{opener}: watched={watched}",
        );
        // A turn the session took up by itself, ended while a wait is on it,
        // is the wait's to report: such a wait is one waiting out the
        // session's own command (`--settled`), or one started after the turn
        // began, and it returns on this end or a later one. A line as well
        // would be a second turn of the opener's about one thing.
        if watched && by_itself {
            log::debug!(
                target: crate::app::agent_state_pass::TRACE,
                "#{thread_id} ended a turn it took up by itself under a wait: not told",
            );
            return;
        }
        let held = watched.then(|| self.held_now(opener));
        let tells = self.opener_tells.entry(opener).or_default();
        // Ends not yet told are one line: what the opener has not heard of
        // yet, it hears of together.
        let earlier = tells
            .iter()
            .position(|tell| tell.thread_id == thread_id)
            .map(|at| tells.remove(at));
        let (by_itself_all, self_turns) = match earlier {
            Some(earlier) => (
                earlier.by_itself && by_itself,
                earlier.self_turns + u32::from(by_itself),
            ),
            None => (by_itself, u32::from(by_itself)),
        };
        tells.push(Tell {
            thread_id,
            held,
            by_itself: by_itself_all,
            self_turns,
            noted_at: std::time::Instant::now(),
        });
    }

    /// Note that `opener` has asked the app about `thread_id` itself.
    ///
    /// `splitlane wait` and `splitlane answer` both begin by reading the
    /// session's status. An opener that does either in its own turn after
    /// the session's turn ended has the news already: a wait started a moment
    /// late returns at once, and the line would then arrive as a turn about
    /// something it has read. So the line is held from here on. It is not
    /// struck out, because the question may have come from a background
    /// command whose answer the agent never saw. The first look starts the
    /// clock; a script asking every minute must not keep the line held for
    /// ever.
    ///
    /// `for_answer` is the opener reading the session's answer, which is
    /// more than a question: a wait asks the status over and over, in a
    /// script as readily as in a turn, and only the agent itself goes on to
    /// read what was said.
    pub(crate) fn opener_looked_at(&mut self, opener: u64, thread_id: u64, for_answer: bool) {
        let now = self.held_now(opener);
        if let Some(tells) = self.opener_tells.get_mut(&opener) {
            for tell in tells.iter_mut().filter(|tell| tell.thread_id == thread_id) {
                match tell.held.as_mut() {
                    Some(held) => held.read_answer |= for_answer,
                    None => {
                        tell.held = Some(Held {
                            read_answer: for_answer,
                            ..now
                        });
                    }
                }
            }
        }
    }

    fn held_now(&self, opener: u64) -> Held {
        let thread = self.thread_by_id(opener);
        Held {
            since: std::time::Instant::now(),
            opener_runs: thread.map_or(0, |thread| thread.rail.runs_ended),
            opener_at_work: thread.is_some_and(|thread| thread.status == ThreadStatus::Thinking),
            read_answer: false,
        }
    }

    /// Write what each opener is owed, where it can take it now.
    pub(crate) fn flush_opener_tells(&mut self, cx: &mut Context<Self>) {
        if self.opener_tells.is_empty() {
            return;
        }
        // Switched off with lines still held: they are not owed any more.
        if !self.cached_config.tell_opener_enabled() {
            self.opener_tells.clear();
            return;
        }
        let openers: Vec<u64> = self.opener_tells.keys().copied().collect();
        for opener in openers {
            let Some(tells) = self.opener_tells.remove(&opener) else {
                continue;
            };
            // Lines the opener had another way to hear of: gone once it is
            // at work, kept back while it may still act, due after that.
            let opener_now = self.thread_by_id(opener).map(|thread| {
                (
                    thread.status,
                    thread.rail.runs_ended,
                    thread.rail.background_shells,
                )
            });
            let mut waiting: Vec<Tell> = Vec::new();
            let tells: Vec<Tell> = tells
                .into_iter()
                .filter_map(|mut tell| {
                    // Turns the session took up by itself are told once it
                    // has settled, not one at a time.
                    let session_busy = self.thread_by_id(tell.thread_id).is_some_and(|thread| {
                        thread.status == ThreadStatus::Thinking || thread.rail.background_shells > 0
                    });
                    if self_turn_waits(tell.by_itself, session_busy, tell.noted_at.elapsed()) {
                        waiting.push(tell);
                        return None;
                    }
                    let (Some(held), Some((status, runs, background_shells))) =
                        (tell.held, opener_now)
                    else {
                        return Some(tell);
                    };
                    let watched = self
                        .terminal_of(tell.thread_id, cx)
                        .is_some_and(|terminal| {
                            self.event_bus.is_watched(
                                crate::rail_state::RAIL_EVENT_TYPE,
                                terminal.entity_id().as_u64(),
                            )
                        });
                    let now = OpenerNow {
                        status,
                        turns_since: runs.saturating_sub(held.opener_runs),
                        background_shells,
                    };
                    // An opener still in the turn it was in when the line
                    // was held has not had its minute yet either.
                    let in_that_turn = held.opener_at_work && now.turns_since == 0;
                    match held_step(held, now, held.since.elapsed(), watched) {
                        HeldStep::Due => Some(tell),
                        HeldStep::Keep => {
                            // The minute is counted from the end of the
                            // wait, not of the turn: a wait that returns
                            // long after the turn must still be given time
                            // to wake its opener.
                            if watched || in_that_turn {
                                tell.held = Some(Held {
                                    since: std::time::Instant::now(),
                                    ..held
                                });
                            }
                            waiting.push(tell);
                            None
                        }
                        HeldStep::Drop => {
                            log::debug!(
                                target: crate::app::agent_state_pass::TRACE,
                                "#{opener} is at work: held line about #{} dropped",
                                tell.thread_id,
                            );
                            None
                        }
                    }
                })
                .collect();
            let keep_waiting = |app: &mut Self, waiting: Vec<Tell>| {
                if !waiting.is_empty() {
                    app.opener_tells.entry(opener).or_default().extend(waiting);
                }
            };
            // A session that is gone has no answer to read, and one at work
            // again was given its next task by somebody who knows. The
            // surface is looked up now and not when the turn ended: a line
            // can be held for a long time, and it has to name what is there.
            let sessions: Vec<(Tell, Told)> = tells
                .into_iter()
                .filter_map(|tell| {
                    let thread = self.thread_by_id(tell.thread_id)?;
                    if thread.status == ThreadStatus::Thinking {
                        return None;
                    }
                    let title = crate::project::clean_sidebar_title(&thread.title)
                        .unwrap_or_else(|| thread.title.clone());
                    let surface_id = self.terminal_of(tell.thread_id, cx)?.entity_id().as_u64();
                    let told = Told {
                        title,
                        surface_id,
                        by_itself: tell.by_itself,
                        self_turns: tell.self_turns,
                        command_running: thread.rail.background_shells > 0,
                    };
                    Some((tell, told))
                })
                .collect();
            if sessions.is_empty() {
                keep_waiting(self, waiting);
                continue;
            }
            let hold = |app: &mut Self, sessions: Vec<(Tell, Told)>, waiting: Vec<Tell>| {
                app.opener_tells.insert(
                    opener,
                    sessions
                        .into_iter()
                        .map(|(tell, ..)| tell)
                        .chain(waiting)
                        .collect(),
                );
            };
            let Some(thread) = self.thread_by_id(opener) else {
                log::debug!(
                    target: crate::app::agent_state_pass::TRACE,
                    "#{opener} is gone: {} ended turn(s) not told",
                    sessions.len(),
                );
                continue;
            };
            let (status, agent_exited) = (thread.status, thread.rail.agent_exited);
            // No terminal this pass is not the opener being gone.
            let Some(terminal) = self.terminal_of(opener, cx) else {
                hold(self, sessions, waiting);
                continue;
            };
            let Some(look) = self.opening_look(opener, &terminal, cx) else {
                hold(self, sessions, waiting);
                continue;
            };
            let facts = OpenerFacts {
                status,
                agent_exited,
                agent_found: look.agent_found,
                keyboard_draft: terminal.read(cx).keyboard_draft,
                entry_open: look.entry == crate::agent_state::TextEntry::Open,
                just_told: self
                    .opener_told_at
                    .get(&opener)
                    .is_some_and(|at| at.elapsed() < TELL_GAP),
            };
            match tell_step(facts) {
                TellStep::Drop => {
                    log::debug!(
                        target: crate::app::agent_state_pass::TRACE,
                        "#{opener} has no agent left: {} ended turn(s) not told",
                        sessions.len(),
                    );
                    continue;
                }
                TellStep::Hold => {
                    hold(self, sessions, waiting);
                    continue;
                }
                TellStep::Write => {}
            }
            let named: Vec<Told> = sessions.iter().map(|(_, told)| told.clone()).collect();
            let text = tell_text(&named);
            let refused_before = terminal.read(cx).terminal.refused_input_count();
            // Typed where the agent takes that, as one session's text into
            // another is: a paste is marked by the agent as text that may
            // not be the person's.
            if !cfg!(windows) && self.types_into(opener, &terminal, cx) {
                terminal.read(cx).type_text(&text);
            } else {
                terminal.read(cx).inject_text(&text);
            }
            if terminal.read(cx).terminal.refused_input_count() != refused_before {
                hold(self, sessions, waiting);
                continue;
            }
            let floor = std::time::Duration::from_millis(
                self.cached_config.resolved_submit_paste_delay_ms(),
            );
            Self::schedule_deferred_submit(&terminal, floor, cx);
            self.opener_told_at
                .insert(opener, std::time::Instant::now());
            keep_waiting(self, waiting);
            log::debug!(
                target: crate::app::agent_state_pass::TRACE,
                "#{opener} told of {} ended turn(s)",
                named.len(),
            );
        }
        // An opener that is gone is not told again.
        let live: Vec<u64> = self.opener_told_at.keys().copied().collect();
        for opener in live {
            if self.thread_by_id(opener).is_none() {
                self.opener_told_at.remove(&opener);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY: OpenerFacts = OpenerFacts {
        status: ThreadStatus::Idle,
        agent_exited: false,
        agent_found: true,
        keyboard_draft: false,
        entry_open: true,
        just_told: false,
    };

    #[test]
    fn an_idle_agent_with_an_empty_line_is_told() {
        assert_eq!(tell_step(READY), TellStep::Write);
    }

    #[test]
    fn a_line_is_held_until_enter_would_start_a_clean_turn() {
        for status in [
            ThreadStatus::Thinking,
            ThreadStatus::WaitingForInput,
            ThreadStatus::Starting,
            ThreadStatus::Failed,
        ] {
            assert_eq!(
                tell_step(OpenerFacts { status, ..READY }),
                TellStep::Hold,
                "{status:?}"
            );
        }
        assert_eq!(
            tell_step(OpenerFacts {
                keyboard_draft: true,
                ..READY
            }),
            TellStep::Hold
        );
        assert_eq!(
            tell_step(OpenerFacts {
                entry_open: false,
                ..READY
            }),
            TellStep::Hold
        );
        assert_eq!(
            tell_step(OpenerFacts {
                agent_found: false,
                ..READY
            }),
            TellStep::Hold
        );
    }

    fn held_idle() -> Held {
        Held {
            since: std::time::Instant::now(),
            opener_runs: 0,
            opener_at_work: false,
            read_answer: false,
        }
    }

    fn opener(status: ThreadStatus, turns_since: u64, background_shells: u32) -> OpenerNow {
        OpenerNow {
            status,
            turns_since,
            background_shells,
        }
    }

    #[test]
    fn an_opener_at_work_heard_and_is_not_told_again() {
        let held = held_idle();
        for held_for in [std::time::Duration::ZERO, HELD_FOR * 10] {
            assert_eq!(
                held_step(held, opener(ThreadStatus::Thinking, 0, 0), held_for, false),
                HeldStep::Drop
            );
        }
        // A turn too short to be seen at work is seen in the count of turns.
        assert_eq!(
            held_step(held, opener(ThreadStatus::Idle, 1, 0), HELD_FOR * 10, false),
            HeldStep::Drop
        );
    }

    /// The case that forced it: a lead's wait returned on time inside a
    /// script of the lead's own that never ended, and the lead sat idle for
    /// an hour beside a session that had finished.
    #[test]
    fn an_opener_still_idle_after_its_wait_ended_is_told() {
        let held = held_idle();
        let just_short = HELD_FOR - std::time::Duration::from_secs(1);
        for status in [ThreadStatus::Idle, ThreadStatus::WaitingForInput] {
            assert_eq!(
                held_step(held, opener(status, 0, 1), just_short, false),
                HeldStep::Keep
            );
            assert_eq!(
                held_step(held, opener(status, 0, 1), HELD_FOR, false),
                HeldStep::Due
            );
        }
    }

    #[test]
    fn a_wait_still_running_keeps_the_line_back() {
        assert_eq!(
            held_step(
                held_idle(),
                opener(ThreadStatus::Idle, 0, 0),
                HELD_FOR * 10,
                true
            ),
            HeldStep::Keep
        );
    }

    /// A session that answers in a few seconds ends its turn while the lead
    /// is still in the turn that opened it. The lead's being at work then is
    /// not the lead having heard.
    #[test]
    fn an_opener_that_was_mid_turn_is_told_once_it_sits_idle_with_its_script_running() {
        let held = Held {
            opener_at_work: true,
            ..held_idle()
        };
        // Still in that turn, however long it takes.
        assert_eq!(
            held_step(
                held,
                opener(ThreadStatus::Thinking, 0, 1),
                HELD_FOR * 10,
                false
            ),
            HeldStep::Keep
        );
        // That turn over, its background command still running: a minute.
        assert_eq!(
            held_step(held, opener(ThreadStatus::Idle, 1, 1), HELD_FOR / 2, false),
            HeldStep::Keep
        );
        assert_eq!(
            held_step(held, opener(ThreadStatus::Idle, 1, 1), HELD_FOR, false),
            HeldStep::Due
        );
    }

    #[test]
    fn an_opener_that_was_mid_turn_and_heard_there_is_not_told() {
        let held = Held {
            opener_at_work: true,
            ..held_idle()
        };
        // Nothing of its own left running: the wait ended inside the turn.
        assert_eq!(
            held_step(held, opener(ThreadStatus::Idle, 1, 0), HELD_FOR * 10, false),
            HeldStep::Drop
        );
        // Its own commands still running, and it read the session's answer:
        // a lead with several sessions out, called back about this one.
        let asked = Held {
            read_answer: true,
            ..held
        };
        assert_eq!(
            held_step(
                asked,
                opener(ThreadStatus::Idle, 1, 2),
                HELD_FOR * 10,
                false
            ),
            HeldStep::Drop
        );
        // A turn begun afterwards.
        assert_eq!(
            held_step(
                held,
                opener(ThreadStatus::Thinking, 1, 1),
                HELD_FOR * 10,
                false
            ),
            HeldStep::Drop
        );
        assert_eq!(
            held_step(held, opener(ThreadStatus::Idle, 2, 1), HELD_FOR * 10, false),
            HeldStep::Drop
        );
    }

    #[test]
    fn one_line_is_not_written_on_the_heels_of_another() {
        assert_eq!(
            tell_step(OpenerFacts {
                just_told: true,
                ..READY
            }),
            TellStep::Hold
        );
    }

    #[test]
    fn a_shell_left_behind_is_never_written_to() {
        assert_eq!(
            tell_step(OpenerFacts {
                agent_exited: true,
                agent_found: false,
                ..READY
            }),
            TellStep::Drop
        );
        // The exit outlives an agent a person started again by hand.
        assert_eq!(
            tell_step(OpenerFacts {
                agent_exited: true,
                ..READY
            }),
            TellStep::Write
        );
    }

    fn told(title: &str, surface_id: u64) -> Told {
        Told {
            title: title.to_string(),
            surface_id,
            by_itself: false,
            self_turns: 0,
            command_running: false,
        }
    }

    #[test]
    fn the_line_names_each_session_and_stays_one_line() {
        let one = tell_text(&[told("api", 101)]);
        assert!(one.contains("\"api\" (surface 101)"), "{one}");
        assert!(one.contains("splitlane answer 101"), "{one}");
        let two = tell_text(&[told("api", 101), told("web", 102)]);
        assert!(
            two.contains("surface 101") && two.contains("surface 102"),
            "{two}"
        );
        let long = tell_text(&[Told {
            by_itself: true,
            self_turns: 1,
            command_running: true,
            ..told(&("x".repeat(500) + "\nrm -rf"), 7)
        }]);
        assert!(long.len() < 360, "{long}");
        for text in [one, two, long] {
            assert!(!text.contains('\n'));
            assert!(text.ends_with("not typed by a person."));
        }
    }

    /// A lead that missed a stop asked what it would have needed to know:
    /// that it did not start this turn, and whether more may follow.
    #[test]
    fn the_line_says_who_began_the_turn_and_whether_more_may_follow() {
        let line = tell_text(&[Told {
            by_itself: true,
            self_turns: 1,
            command_running: true,
            ..told("run", 7)
        }]);
        assert!(
            line.contains(
                "\"run\" (surface 7, taken up by itself, a command of its own still running)"
            ),
            "{line}"
        );
        let plain = tell_text(&[told("run", 7)]);
        assert!(plain.contains("\"run\" (surface 7)"), "{plain}");
    }

    /// Several turns taken up by itself are named in one line, with their
    /// count, and so are such turns after the end of the opener's own task.
    #[test]
    fn turns_taken_up_by_itself_are_counted_in_one_line() {
        let many = tell_text(&[Told {
            by_itself: true,
            self_turns: 4,
            ..told("run", 7)
        }]);
        assert!(
            many.contains("\"run\" (surface 7, 4 turns taken up by itself)"),
            "{many}"
        );
        let after_task = tell_text(&[Told {
            by_itself: false,
            self_turns: 1,
            ..told("run", 7)
        }]);
        assert!(
            after_task.contains("\"run\" (surface 7, then 1 turn taken up by itself)"),
            "{after_task}"
        );
    }

    /// The case behind the rule: a session woken again and again by its own
    /// command wrote a line into its opener at every turn end. Those lines
    /// wait until the session settles or goes quiet; the end of the task the
    /// opener sent is never kept back.
    #[test]
    fn turns_taken_up_by_itself_wait_for_the_session_to_settle() {
        let soon = std::time::Duration::from_secs(30);
        let late = SELF_TURN_QUIET;
        assert!(self_turn_waits(true, true, soon));
        assert!(!self_turn_waits(true, false, soon), "settled: told now");
        assert!(!self_turn_waits(true, true, late), "quiet long enough");
        assert!(
            !self_turn_waits(false, true, soon),
            "the opener's own task is told at its first end"
        );
    }
}
