//! A session a person opened, driven by another session with the person's
//! leave.
//!
//! A session works through the sessions it opened, and into one a person
//! opened it does not write. When it tries, the person is asked, once for the
//! pair, in the asking session's own pane; this is the book of what was asked
//! and what was answered. The reasoning is in
//! `docs/internals/design-decisions.md`, "A session a person opened is driven
//! only with their leave".
//!
//! It is kept in memory and nowhere else. Every place an answer could be
//! saved is written with the same rights as the agent that would benefit from
//! it, so an answer lasts as long as the process that heard it.

use std::collections::HashMap;

/// Where one pair stands: `(the session that asked, the session asked for)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    /// The person has been asked and has not answered.
    Asked,
    Allowed,
    /// The person said no. Not asked again until the app restarts, or until
    /// the person hands the session over themselves.
    Declined,
}

/// What the write rule is told about a caller and a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drive {
    /// Not a pair a person can be asked about: the target is not an agent
    /// session a person opened in the caller's project, or the caller was
    /// itself opened by a session.
    NotOffered,
    /// It could be asked, and has not been.
    NotAsked,
    Standing(Standing),
}

impl Drive {
    /// The word a caller reads under `drive` in `surface.status`. Published.
    ///
    /// "Not asked yet" has no word and "never will be" has one, because a
    /// caller waiting for an answer has to tell them apart: the first can
    /// still become a question, the second cannot.
    pub(crate) fn word(self) -> Option<&'static str> {
        match self {
            Drive::NotAsked => None,
            Drive::NotOffered => Some("not_offered"),
            Drive::Standing(Standing::Asked) => Some("asked"),
            Drive::Standing(Standing::Allowed) => Some("allowed"),
            Drive::Standing(Standing::Declined) => Some("declined"),
        }
    }
}

/// Every pair that has been asked about, keyed by surface record ids.
#[derive(Debug, Default)]
pub(crate) struct DriveBook {
    pairs: HashMap<(u64, u64), Standing>,
}

impl DriveBook {
    pub(crate) fn standing(&self, asker: u64, target: u64) -> Option<Standing> {
        self.pairs.get(&(asker, target)).copied()
    }

    /// Put the question, unless the pair already has a standing. `true` when
    /// this call is what asked.
    ///
    /// A pair that was declined stays declined: the agent cannot change a
    /// person's mind for them by asking again.
    pub(crate) fn ask(&mut self, asker: u64, target: u64) -> bool {
        if self.pairs.contains_key(&(asker, target)) {
            return false;
        }
        self.pairs.insert((asker, target), Standing::Asked);
        true
    }

    /// The person answered a standing question. `false` when there was none.
    ///
    /// A yes takes the session from whoever drove it before, like handing it
    /// over does: the question can have stood while the person gave the
    /// session to somebody else, and two sessions allowed at once would leave
    /// "stop driving" taking it back from only one of them.
    pub(crate) fn answer(&mut self, asker: u64, target: u64, allow: bool) -> bool {
        if self.standing(asker, target) != Some(Standing::Asked) {
            return false;
        }
        if allow {
            self.give(asker, target);
        } else {
            self.pairs.insert((asker, target), Standing::Declined);
        }
        true
    }

    /// The person handed `target` to `asker` without being asked. It lifts an
    /// earlier no, and takes the session from whoever drove it before: one
    /// session is driven by one.
    pub(crate) fn give(&mut self, asker: u64, target: u64) {
        self.pairs
            .retain(|(_, of), standing| *of != target || *standing != Standing::Allowed);
        self.pairs.insert((asker, target), Standing::Allowed);
    }

    /// The person took `target` back. Not a no: the pair is forgotten, and
    /// the session that drove it may ask again.
    pub(crate) fn take_back(&mut self, target: u64) -> Option<u64> {
        let driver = self.driver_of(target)?;
        self.pairs.remove(&(driver, target));
        Some(driver)
    }

    /// The session allowed to drive `target`, if one is.
    pub(crate) fn driver_of(&self, target: u64) -> Option<u64> {
        self.pairs
            .iter()
            .find(|((_, of), standing)| *of == target && **standing == Standing::Allowed)
            .map(|((asker, _), _)| *asker)
    }

    /// Every pair in which one session drives another, `(driver, driven)`,
    /// in the order of the driven session's id.
    pub(crate) fn driven(&self) -> Vec<(u64, u64)> {
        let mut pairs: Vec<(u64, u64)> = self
            .pairs
            .iter()
            .filter(|(_, standing)| **standing == Standing::Allowed)
            .map(|(pair, _)| *pair)
            .collect();
        pairs.sort_unstable_by_key(|(_, driven)| *driven);
        pairs
    }

    /// The sessions `asker` is allowed to drive, in the order of their ids.
    pub(crate) fn driven_by(&self, asker: u64) -> Vec<u64> {
        let mut targets: Vec<u64> = self
            .pairs
            .iter()
            .filter(|((by, _), standing)| *by == asker && **standing == Standing::Allowed)
            .map(|((_, target), _)| *target)
            .collect();
        targets.sort_unstable();
        targets
    }

    /// The sessions `asker` has an unanswered question about, in the order of
    /// their ids so the rows of the question do not move between frames.
    pub(crate) fn asked_by(&self, asker: u64) -> Vec<u64> {
        let mut targets: Vec<u64> = self
            .pairs
            .iter()
            .filter(|((by, _), standing)| *by == asker && **standing == Standing::Asked)
            .map(|((_, target), _)| *target)
            .collect();
        targets.sort_unstable();
        targets
    }

    pub(crate) fn has_questions(&self, asker: u64) -> bool {
        self.pairs
            .iter()
            .any(|((by, _), standing)| *by == asker && *standing == Standing::Asked)
    }

    /// Drop every pair a session that no longer exists is part of. A closed
    /// asker has nobody left to drive anything, and a closed target has
    /// nothing left to drive.
    pub(crate) fn keep_only(&mut self, exists: impl Fn(u64) -> bool) {
        self.pairs
            .retain(|(asker, target), _| exists(*asker) && exists(*target));
    }
}

/// What the asking session's row says while its question stands: the text
/// under `rail.message`, and the body of the question in its pane.
pub(crate) fn question_summary(targets: &[String]) -> String {
    match targets {
        [one] => format!("wants to send messages to {one}"),
        many => format!("wants to send messages to {} sessions", many.len()),
    }
}

/// The command that takes a session back, with both names: it is listed
/// where the row it is about is not in sight.
pub(crate) fn stop_driving_label(driver: &str, driven: &str) -> String {
    format!("Stop {driver} driving {driven}")
}

/// Whether a palette query is asking for that command.
///
/// Not "the label contains the query", which is how a session is found:
/// the label holds both sessions' names, so somebody typing `api` to go to
/// `api` would be offered "Stop plan driving api" - as the first row, when
/// `api` is not in a pane - and Enter would take the session back. The
/// query has to begin the way the command does, with `stop` or `driving`.
/// `needle` is lower case, as the palette hands it over.
pub(crate) fn stop_driving_matches(label: &str, needle: &str) -> bool {
    let Some(first) = needle.split_whitespace().next() else {
        return false;
    };
    (["stop", "driving"]
        .iter()
        .any(|word| word.starts_with(first)))
        && label.to_lowercase().contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: u64 = 3;
    const API: u64 = 10;
    const WEB: u64 = 11;

    #[test]
    fn a_pair_is_asked_about_once() {
        let mut book = DriveBook::default();
        assert!(book.ask(PLAN, API));
        assert!(!book.ask(PLAN, API), "the question is already standing");
        assert_eq!(book.standing(PLAN, API), Some(Standing::Asked));
        assert_eq!(book.asked_by(PLAN), [API]);
        assert!(book.has_questions(PLAN));
    }

    #[test]
    fn an_answer_ends_the_question_either_way() {
        let mut book = DriveBook::default();
        book.ask(PLAN, API);
        book.ask(PLAN, WEB);
        assert!(book.answer(PLAN, API, true));
        assert!(book.answer(PLAN, WEB, false));
        assert_eq!(book.standing(PLAN, API), Some(Standing::Allowed));
        assert_eq!(book.standing(PLAN, WEB), Some(Standing::Declined));
        assert!(!book.has_questions(PLAN));
        assert_eq!(book.driver_of(API), Some(PLAN));
        assert_eq!(book.driver_of(WEB), None);
        assert_eq!(book.driven_by(PLAN), [API]);
        assert_eq!(book.driven(), [(PLAN, API)]);
        // Nothing is standing, so there is nothing to answer.
        assert!(!book.answer(PLAN, API, false));
        assert_eq!(book.standing(PLAN, API), Some(Standing::Allowed));
    }

    /// The agent cannot change a person's mind for them by asking again.
    #[test]
    fn a_no_is_not_asked_again() {
        let mut book = DriveBook::default();
        book.ask(PLAN, API);
        book.answer(PLAN, API, false);
        assert!(!book.ask(PLAN, API));
        assert_eq!(book.standing(PLAN, API), Some(Standing::Declined));
    }

    /// Taking a session back is not a no.
    #[test]
    fn a_session_taken_back_can_be_asked_for_again() {
        let mut book = DriveBook::default();
        book.give(PLAN, API);
        assert_eq!(book.take_back(API), Some(PLAN));
        assert_eq!(book.standing(PLAN, API), None);
        assert_eq!(book.take_back(API), None);
        assert!(book.ask(PLAN, API));
    }

    #[test]
    fn a_session_is_driven_by_one() {
        let mut book = DriveBook::default();
        book.give(PLAN, API);
        book.give(WEB, API);
        assert_eq!(book.driver_of(API), Some(WEB));
        assert_eq!(book.standing(PLAN, API), None);
    }

    /// A question stood while the person handed the session to another, and
    /// was then answered yes. The answer moves the session; it does not add a
    /// second driver that taking back would leave behind.
    #[test]
    fn a_yes_to_a_standing_question_takes_the_session_from_its_driver() {
        let mut book = DriveBook::default();
        book.ask(PLAN, API);
        book.give(WEB, API);
        assert!(book.answer(PLAN, API, true));
        assert_eq!(book.driver_of(API), Some(PLAN));
        assert_eq!(book.standing(WEB, API), None);
        assert_eq!(book.take_back(API), Some(PLAN));
        assert_eq!(book.driver_of(API), None);
    }

    /// The person can change their mind, and handing a session over is how.
    #[test]
    fn a_no_is_lifted_by_the_person_handing_the_session_over() {
        let mut book = DriveBook::default();
        book.ask(PLAN, API);
        book.answer(PLAN, API, false);
        book.give(PLAN, API);
        assert_eq!(book.standing(PLAN, API), Some(Standing::Allowed));
        // And a question still standing is answered by it.
        book.ask(PLAN, WEB);
        book.give(PLAN, WEB);
        assert!(!book.has_questions(PLAN));
    }

    #[test]
    fn a_closed_session_takes_its_pairs_with_it() {
        let mut book = DriveBook::default();
        book.ask(PLAN, API);
        book.answer(PLAN, API, true);
        book.ask(PLAN, WEB);
        book.keep_only(|id| id != PLAN);
        assert_eq!(book.driver_of(API), None);
        assert!(!book.has_questions(PLAN));

        book.ask(PLAN, API);
        book.answer(PLAN, API, true);
        book.keep_only(|id| id != API);
        assert_eq!(book.standing(PLAN, API), None);
    }

    #[test]
    fn a_pair_nobody_was_asked_about_yet_has_no_word() {
        assert_eq!(Drive::NotAsked.word(), None);
        assert_eq!(Drive::NotOffered.word(), Some("not_offered"));
        assert_eq!(Drive::Standing(Standing::Asked).word(), Some("asked"));
        assert_eq!(Drive::Standing(Standing::Allowed).word(), Some("allowed"));
        assert_eq!(Drive::Standing(Standing::Declined).word(), Some("declined"));
    }

    #[test]
    fn taking_a_session_back_names_both_sessions() {
        assert_eq!(stop_driving_label("plan", "api"), "Stop plan driving api");
    }

    /// The command is found by its own words and never by a session's name
    /// alone: a query for the session must not put this row under Enter.
    #[test]
    fn the_command_is_found_by_its_verb_not_by_a_sessions_name() {
        let label = stop_driving_label("plan", "api");
        for asked in [
            "st",
            "stop",
            "stop plan",
            "stop plan driving api",
            "dri",
            "driving api",
        ] {
            assert!(stop_driving_matches(&label, asked), "{asked}");
        }
        for not_asked in [
            "",
            "api",
            "plan",
            "plan driving",
            "stop web",
            "stopwatch",
            "a",
        ] {
            assert!(!stop_driving_matches(&label, not_asked), "{not_asked}");
        }
    }

    #[test]
    fn the_question_names_one_session_and_counts_several() {
        assert_eq!(
            question_summary(&["api".to_string()]),
            "wants to send messages to api"
        );
        assert_eq!(
            question_summary(&["api".to_string(), "web".to_string(), "docs".to_string()]),
            "wants to send messages to 3 sessions"
        );
    }
}
