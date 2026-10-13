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
    opener_status: ThreadStatus,
    opener_ran_since: bool,
    held_for: std::time::Duration,
    watched: bool,
) -> HeldStep {
    if opener_status == ThreadStatus::Thinking || opener_ran_since {
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

/// The line itself, for sessions given as title and surface id. One line
/// whatever the count: every line break typed into an agent is a chance for
/// it to submit half a message.
pub(crate) fn tell_text(sessions: &[(String, u64)]) -> String {
    let named = |(title, surface_id): &(String, u64)| {
        let short: String = title.chars().take(TITLE_MAX).collect();
        format!(
            "{} (surface {surface_id})",
            crate::app::send_answer::quoted_title(&short)
        )
    };
    match sessions {
        [one] => format!(
            "[Splitlane] {}, a session this one opened, has ended its turn. \
             Read its answer with: splitlane answer {}. \
             Sent by the app, not typed by a person.",
            named(one),
            one.1
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
        let held = watched.then(|| self.held_now(opener));
        let tells = self.opener_tells.entry(opener).or_default();
        tells.retain(|tell| tell.thread_id != thread_id);
        tells.push(Tell { thread_id, held });
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
    pub(crate) fn opener_looked_at(&mut self, opener: u64, thread_id: u64) {
        let now = self.held_now(opener);
        if let Some(tells) = self.opener_tells.get_mut(&opener) {
            for tell in tells.iter_mut().filter(|tell| tell.thread_id == thread_id) {
                tell.held.get_or_insert(now);
            }
        }
    }

    fn held_now(&self, opener: u64) -> Held {
        Held {
            since: std::time::Instant::now(),
            opener_runs: self
                .thread_by_id(opener)
                .map_or(0, |thread| thread.rail.runs_ended),
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
            let opener_now = self
                .thread_by_id(opener)
                .map(|thread| (thread.status, thread.rail.runs_ended));
            let mut waiting: Vec<Tell> = Vec::new();
            let tells: Vec<Tell> = tells
                .into_iter()
                .filter_map(|mut tell| {
                    let (Some(held), Some((status, runs))) = (tell.held, opener_now) else {
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
                    let ran_since = runs > held.opener_runs;
                    match held_step(status, ran_since, held.since.elapsed(), watched) {
                        HeldStep::Due => Some(tell),
                        HeldStep::Keep => {
                            // The minute is counted from the end of the
                            // wait, not of the turn: a wait that returns
                            // long after the turn must still be given time
                            // to wake its opener.
                            if watched {
                                tell.held = Some(Held {
                                    since: std::time::Instant::now(),
                                    opener_runs: runs,
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
            let sessions: Vec<(Tell, String, u64)> = tells
                .into_iter()
                .filter_map(|tell| {
                    let thread = self.thread_by_id(tell.thread_id)?;
                    if thread.status == ThreadStatus::Thinking {
                        return None;
                    }
                    let title = crate::project::clean_sidebar_title(&thread.title)
                        .unwrap_or_else(|| thread.title.clone());
                    let surface_id = self.terminal_of(tell.thread_id, cx)?.entity_id().as_u64();
                    Some((tell, title, surface_id))
                })
                .collect();
            if sessions.is_empty() {
                keep_waiting(self, waiting);
                continue;
            }
            let hold = |app: &mut Self, sessions: Vec<(Tell, String, u64)>, waiting: Vec<Tell>| {
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
            let named: Vec<(String, u64)> = sessions
                .iter()
                .map(|(_, title, surface_id)| (title.clone(), *surface_id))
                .collect();
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

    #[test]
    fn an_opener_at_work_heard_and_is_not_told_again() {
        for held_for in [std::time::Duration::ZERO, HELD_FOR * 10] {
            assert_eq!(
                held_step(ThreadStatus::Thinking, false, held_for, false),
                HeldStep::Drop
            );
        }
        // A turn too short to be seen at work is seen in the count of turns.
        assert_eq!(
            held_step(ThreadStatus::Idle, true, HELD_FOR * 10, false),
            HeldStep::Drop
        );
    }

    /// The case that forced it: a lead's wait returned on time inside a
    /// script of the lead's own that never ended, and the lead sat idle for
    /// an hour beside a session that had finished.
    #[test]
    fn an_opener_still_idle_after_its_wait_ended_is_told() {
        let just_short = HELD_FOR - std::time::Duration::from_secs(1);
        for status in [ThreadStatus::Idle, ThreadStatus::WaitingForInput] {
            assert_eq!(held_step(status, false, just_short, false), HeldStep::Keep);
            assert_eq!(held_step(status, false, HELD_FOR, false), HeldStep::Due);
        }
    }

    #[test]
    fn a_wait_still_running_keeps_the_line_back() {
        assert_eq!(
            held_step(ThreadStatus::Idle, false, HELD_FOR * 10, true),
            HeldStep::Keep
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

    #[test]
    fn the_line_names_each_session_and_stays_one_line() {
        let one = tell_text(&[("api".to_string(), 101)]);
        assert!(one.contains("\"api\" (surface 101)"), "{one}");
        assert!(one.contains("splitlane answer 101"), "{one}");
        let two = tell_text(&[("api".to_string(), 101), ("web".to_string(), 102)]);
        assert!(
            two.contains("surface 101") && two.contains("surface 102"),
            "{two}"
        );
        let long = tell_text(&[("x".repeat(500) + "\nrm -rf", 7)]);
        assert!(long.len() < 300, "{long}");
        for text in [one, two, long] {
            assert!(!text.contains('\n'));
            assert!(text.ends_with("not typed by a person."));
        }
    }
}
