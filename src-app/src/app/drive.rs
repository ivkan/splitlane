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
    pub(crate) fn word(self) -> Option<&'static str> {
        match self {
            Drive::NotOffered | Drive::NotAsked => None,
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
    pub(crate) fn answer(&mut self, asker: u64, target: u64, allow: bool) -> bool {
        match self.pairs.get_mut(&(asker, target)) {
            Some(standing @ Standing::Asked) => {
                *standing = if allow {
                    Standing::Allowed
                } else {
                    Standing::Declined
                };
                true
            }
            _ => false,
        }
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
    fn only_a_standing_has_a_word() {
        assert_eq!(Drive::NotOffered.word(), None);
        assert_eq!(Drive::NotAsked.word(), None);
        assert_eq!(Drive::Standing(Standing::Asked).word(), Some("asked"));
        assert_eq!(Drive::Standing(Standing::Allowed).word(), Some("allowed"));
        assert_eq!(Drive::Standing(Standing::Declined).word(), Some("declined"));
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
