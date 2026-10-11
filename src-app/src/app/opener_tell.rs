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
}

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
    /// Not when something is already waiting on this session by name: that
    /// is the opener's own `splitlane wait`, which ends on the same event,
    /// and a second message about one turn would start a second turn.
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
        if watched {
            return;
        }
        let tells = self.opener_tells.entry(opener).or_default();
        tells.retain(|tell| tell.thread_id != thread_id);
        tells.push(Tell { thread_id });
    }

    /// Note that `opener` has asked the app about `thread_id` itself.
    ///
    /// `splitlane wait` and `splitlane answer` both begin by reading the
    /// session's status. An opener that does either after the turn ended has
    /// the news already: a wait started a moment late returns at once, and
    /// the line would then arrive as a turn about something it has read.
    pub(crate) fn opener_looked_at(&mut self, opener: u64, thread_id: u64) {
        if let Some(tells) = self.opener_tells.get_mut(&opener) {
            tells.retain(|tell| tell.thread_id != thread_id);
            if tells.is_empty() {
                self.opener_tells.remove(&opener);
            }
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
                continue;
            }
            let hold = |app: &mut Self, sessions: Vec<(Tell, String, u64)>| {
                app.opener_tells.insert(
                    opener,
                    sessions.into_iter().map(|(tell, ..)| tell).collect(),
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
                hold(self, sessions);
                continue;
            };
            let Some(look) = self.opening_look(opener, &terminal, cx) else {
                hold(self, sessions);
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
                    hold(self, sessions);
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
                hold(self, sessions);
                continue;
            }
            let floor = std::time::Duration::from_millis(
                self.cached_config.resolved_submit_paste_delay_ms(),
            );
            Self::schedule_deferred_submit(&terminal, floor, cx);
            self.opener_told_at
                .insert(opener, std::time::Instant::now());
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
