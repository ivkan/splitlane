//! Project groups in the rail: a named, ordered set of projects.
//!
//! A group is optional. With no project in one the rail is what it always was,
//! and there is no setting: a group appears with the first project put into it
//! and is gone with the last one taken out. A project is in at most one group
//! and groups do not nest.
//!
//! Membership lives on the project (`Workspace::group`), not as a list on the
//! group, so "at most one group" is a property of the type rather than an
//! invariant two lists have to keep. The order inside a group is the order of
//! `SplitlaneApp::workspaces`, which is also the order projects without a
//! group keep, so moving a project to the end of its section is moving it to
//! the end of that list.
//!
//! The word *group* means this and nothing else. A project together with its
//! sessions is a *project block*.

use splitlane_config::schema::ProjectGroupSession;

/// The longest name a group takes, in characters. Input past it is refused
/// rather than cut on commit, so the field never shows a name that will not be
/// kept. On screen the width of the rail cuts a name long before this does.
pub(crate) const GROUP_NAME_MAX_CHARS: usize = 40;

/// One group, as the running app holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectGroup {
    pub(crate) id: u64,
    /// As typed. The label draws it in capitals; that is a style.
    pub(crate) name: String,
    /// Folded in the rail. Only visual: Activity and notifications do not
    /// read it, so folding a group away never costs a name in a notification.
    pub(crate) collapsed: bool,
    /// `Keep names private`.
    pub(crate) private: bool,
}

impl ProjectGroup {
    pub(crate) fn from_session(record: &ProjectGroupSession) -> Self {
        Self {
            id: record.id,
            name: record.name.clone(),
            collapsed: record.collapsed,
            private: record.private,
        }
    }

    pub(crate) fn to_session(&self) -> ProjectGroupSession {
        ProjectGroupSession {
            id: self.id,
            name: self.name.clone(),
            collapsed: self.collapsed,
            private: self.private,
        }
    }
}

/// Groups read from a session file, cleaned: blank names and repeated ids are
/// dropped, since neither can be drawn or addressed.
pub(crate) fn groups_from_session(records: &[ProjectGroupSession]) -> Vec<ProjectGroup> {
    let mut groups: Vec<ProjectGroup> = Vec::new();
    for record in records {
        let name = normalize_group_name(&record.name);
        if name.is_empty() || groups.iter().any(|group| group.id == record.id) {
            continue;
        }
        groups.push(ProjectGroup {
            name,
            ..ProjectGroup::from_session(record)
        });
    }
    groups
}

/// Bring groups and memberships back into agreement: a project pointing at a
/// group that does not exist is in no group, and a group nobody is in is gone
/// - a group exists exactly while it holds a project.
pub(crate) fn reconcile_groups<'a>(
    groups: &mut Vec<ProjectGroup>,
    memberships: impl IntoIterator<Item = &'a mut Option<u64>>,
) {
    let mut memberships: Vec<&'a mut Option<u64>> = memberships.into_iter().collect();
    for membership in memberships.iter_mut() {
        if let Some(id) = **membership
            && !groups.iter().any(|group| group.id == id)
        {
            **membership = None;
        }
    }
    groups.retain(|group| memberships.iter().any(|m| **m == Some(group.id)));
}

/// A name as it will be kept: edges trimmed, line breaks folded to spaces, and
/// no longer than [`GROUP_NAME_MAX_CHARS`].
pub(crate) fn normalize_group_name(raw: &str) -> String {
    let single_line: String = raw
        .chars()
        .map(|ch| if ch == '\n' || ch == '\r' { ' ' } else { ch })
        .collect();
    single_line
        .trim()
        .chars()
        .take(GROUP_NAME_MAX_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The group already carrying `name`, compared without regard to case, other
/// than `except` - the group being named, which may keep its own name in
/// different capitals.
pub(crate) fn group_named<'a>(
    groups: &'a [ProjectGroup],
    name: &str,
    except: Option<u64>,
) -> Option<&'a ProjectGroup> {
    let wanted = normalize_group_name(name).to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    groups
        .iter()
        .filter(|group| Some(group.id) != except)
        .find(|group| group.name.to_lowercase() == wanted)
}

/// The id a new group takes: past every id in use, so an id is never reused
/// within a run and a stale reference can never land on a newer group.
pub(crate) fn next_group_id(groups: &[ProjectGroup], floor: u64) -> u64 {
    groups
        .iter()
        .map(|group| group.id + 1)
        .max()
        .unwrap_or(1)
        .max(floor)
}

/// Whether the naming field is naming a new group or renaming one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamingKind {
    New,
    Rename,
}

/// The one line under the naming field. `None` when there is nothing to say.
pub(crate) fn naming_hint(
    kind: NamingKind,
    typed: &str,
    groups: &[ProjectGroup],
    naming: u64,
) -> Option<String> {
    match (kind, group_named(groups, typed, Some(naming))) {
        // Not an error: somebody typing a group's name almost certainly
        // wanted that group, and Enter does exactly that.
        (NamingKind::New, Some(existing)) => Some(format!(
            "{} exists \u{b7} \u{23ce} adds to it",
            existing.name
        )),
        (NamingKind::New, None) => Some("\u{23ce} create \u{b7} esc cancel".to_string()),
        // Renaming onto another group's name would merge two groups, which
        // nobody asked for; Enter leaves both as they are.
        (NamingKind::Rename, Some(_)) => {
            Some("name in use \u{b7} \u{23ce} does nothing".to_string())
        }
        (NamingKind::Rename, None) => None,
    }
}

/// The rail's list, sectioned: projects without a group first, under
/// `PROJECTS`, then each group with its members, in group order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RailSections {
    pub(crate) ungrouped: Vec<usize>,
    /// `(index into the groups list, member project indices)`. A group with
    /// no members is not listed: it has nothing to draw and is about to go.
    pub(crate) groups: Vec<(usize, Vec<usize>)>,
}

/// Split `display_order` - every project index, in the order the rail draws
/// them without groups - into sections. Filtering one order rather than
/// sorting again keeps whatever adjacency that order already holds (sibling
/// worktrees of one repository) inside each section.
pub(crate) fn rail_sections(
    display_order: &[usize],
    group_of: impl Fn(usize) -> Option<u64>,
    groups: &[ProjectGroup],
) -> RailSections {
    let known = |id: u64| groups.iter().any(|group| group.id == id);
    let mut sections = RailSections::default();
    for &index in display_order {
        match group_of(index) {
            Some(id) if known(id) => {}
            _ => sections.ungrouped.push(index),
        }
    }
    for (position, group) in groups.iter().enumerate() {
        let members: Vec<usize> = display_order
            .iter()
            .copied()
            .filter(|&index| group_of(index) == Some(group.id))
            .collect();
        if !members.is_empty() {
            sections.groups.push((position, members));
        }
    }
    sections
}

/// What a folded group says for the rows it is not drawing: the folded
/// project's words, plus `waiting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TallyWord {
    Failed(usize),
    Waiting(usize),
    Running(usize),
    Finished(usize),
    /// Nothing is happening: how many projects the group holds.
    Projects(usize),
}

impl TallyWord {
    pub(crate) fn text(self) -> String {
        match self {
            TallyWord::Failed(n) => format!("{n} failed"),
            TallyWord::Waiting(n) => format!("{n} waiting"),
            TallyWord::Running(n) => format!("{n} running"),
            TallyWord::Finished(n) => format!("{n} finished"),
            TallyWord::Projects(1) => "1 project".to_string(),
            TallyWord::Projects(n) => format!("{n} projects"),
        }
    }

    /// `failed` and `waiting` are what a folded group answers for, so width
    /// never takes them.
    fn stays(self) -> bool {
        matches!(self, TallyWord::Failed(_) | TallyWord::Waiting(_))
    }
}

/// The words, in reading order - trouble first, then what wants the reader,
/// then work and news - or `N projects` when all four are zero.
pub(crate) fn folded_group_words(
    tally: crate::app::agents_sidebar::FoldedTally,
    projects: usize,
) -> Vec<TallyWord> {
    let words: Vec<TallyWord> = [
        (tally.failed > 0).then_some(TallyWord::Failed(tally.failed)),
        (tally.waiting > 0).then_some(TallyWord::Waiting(tally.waiting)),
        (tally.running > 0).then_some(TallyWord::Running(tally.running)),
        (tally.finished > 0).then_some(TallyWord::Finished(tally.finished)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if words.is_empty() {
        vec![TallyWord::Projects(projects)]
    } else {
        words
    }
}

/// What a label row draws at a given width.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LabelFit {
    /// The `private` word after the label.
    pub(crate) private_word: bool,
    pub(crate) words: Vec<TallyWord>,
    /// The label's own width: its name's, capped at
    /// [`crate::ui_tokens::group::LABEL_MAX`] and at what the rest of the row
    /// leaves, and never under [`crate::ui_tokens::group::LABEL_MIN`].
    ///
    /// Computed rather than left to flex layout. A text that truncates has no
    /// minimum width in the layout engine, so as a shrinkable flex item the
    /// label went straight to its floor however much room the row had -
    /// `PERS…` in a rail wide enough for the whole name.
    pub(crate) label_width: f32,
}

/// Decide what a label row keeps in `available` pixels, `char_width` being
/// one mono character of the summary's size and `gap` the space between parts.
///
/// The order things give way in is the design's: the label shrinks first, to
/// [`crate::ui_tokens::group::LABEL_MIN`] - that part is flex layout and costs
/// nothing here; then `private` goes; then `running` and `finished`, whole
/// words or nothing, keeping `running` if it fits on its own. `failed` and
/// `waiting` never go. The label giving way first is the point: a folded
/// group exists to say what is going on under it, and its name is still one
/// click away in the tooltip.
pub(crate) fn fit_label_row(
    words: &[TallyWord],
    private: bool,
    label_natural: f32,
    available: f32,
    char_width: f32,
    gap: f32,
) -> LabelFit {
    let mut fit = fit_label_row_parts(words, private, available, char_width, gap);
    let width = |text: &str| text.chars().count() as f32 * char_width + gap;
    let others: f32 = fit
        .words
        .iter()
        .map(|word| width(&word.text()))
        .sum::<f32>()
        + if fit.private_word {
            width("private")
        } else {
            0.0
        };
    let min = f32::from(crate::ui_tokens::group::LABEL_MIN);
    let max = f32::from(crate::ui_tokens::group::LABEL_MAX);
    let room = available - gap - others;
    // The floor limits how far a long name is squeezed; it never widens a
    // short one, or `ACME` sits in a pill with air on its right.
    fit.label_width = label_natural.min(max).min(room).max(min.min(label_natural));
    fit
}

fn fit_label_row_parts(
    words: &[TallyWord],
    private: bool,
    available: f32,
    char_width: f32,
    gap: f32,
) -> LabelFit {
    let width = |text: &str| text.chars().count() as f32 * char_width + gap;
    let label = f32::from(crate::ui_tokens::group::LABEL_MIN) + gap;
    let staying: f32 = words
        .iter()
        .filter(|word| word.stays())
        .map(|word| width(&word.text()))
        .sum();
    let rest: Vec<TallyWord> = words.iter().copied().filter(|w| !w.stays()).collect();
    let rest_width: f32 = rest.iter().map(|word| width(&word.text())).sum();
    let private_width = if private { width("private") } else { 0.0 };

    let staying_words = || words.iter().copied().filter(|w| w.stays());
    if label + private_width + staying + rest_width <= available {
        return LabelFit {
            private_word: private,
            words: words.to_vec(),
            label_width: 0.0,
        };
    }
    // `private` leaves before any of the summary does.
    if label + staying + rest_width <= available {
        return LabelFit {
            private_word: false,
            words: words.to_vec(),
            label_width: 0.0,
        };
    }
    let running = rest
        .iter()
        .copied()
        .find(|word| matches!(word, TallyWord::Running(_)));
    if rest.len() > 1
        && let Some(running) = running
        && label + staying + width(&running.text()) <= available
    {
        return LabelFit {
            private_word: false,
            words: words
                .iter()
                .copied()
                .filter(|w| w.stays() || *w == running)
                .collect(),
            label_width: 0.0,
        };
    }
    LabelFit {
        private_word: false,
        words: staying_words().collect(),
        label_width: 0.0,
    }
}

/// Where a project's group operations put things back when a new group is
/// abandoned: the project returns to its old group and its old place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingNewGroup {
    pub(crate) group_id: u64,
    pub(crate) project_id: u64,
    /// The project just above it in the list before it moved, by id - a
    /// neighbour rather than an index, so a project closed meanwhile does not
    /// shift where it goes back to. `None` when it was first.
    pub(crate) previous_neighbour: Option<u64>,
    /// Its index then, for when that neighbour has gone too.
    pub(crate) previous_index: usize,
    pub(crate) previous_group: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::agents_sidebar::FoldedTally;

    fn group(id: u64, name: &str) -> ProjectGroup {
        ProjectGroup {
            id,
            name: name.to_string(),
            collapsed: false,
            private: false,
        }
    }

    #[test]
    fn without_groups_the_rail_is_one_section_in_its_old_order() {
        let sections = rail_sections(&[2, 0, 1], |_| None, &[]);
        assert_eq!(sections.ungrouped, vec![2, 0, 1]);
        assert!(sections.groups.is_empty());
    }

    #[test]
    fn ungrouped_projects_come_first_then_groups_in_their_own_order() {
        // Projects 0..5; 1 and 4 in group 9, 3 in group 7; groups listed 9, 7.
        let membership = [None, Some(9), None, Some(7), Some(9)];
        let groups = [group(9, "Acme"), group(7, "Personal")];
        let sections = rail_sections(&[0, 1, 2, 3, 4], |i| membership[i], &groups);
        assert_eq!(sections.ungrouped, vec![0, 2]);
        assert_eq!(sections.groups, vec![(0, vec![1, 4]), (1, vec![3])]);
    }

    #[test]
    fn a_group_with_no_members_is_not_drawn_and_a_dangling_reference_is_no_group() {
        let membership = [Some(3), Some(42)];
        let groups = [group(3, "Acme"), group(5, "Empty")];
        let sections = rail_sections(&[0, 1], |i| membership[i], &groups);
        assert_eq!(sections.ungrouped, vec![1]);
        assert_eq!(sections.groups, vec![(0, vec![0])]);
    }

    #[test]
    fn a_name_is_trimmed_single_line_and_at_most_forty_characters() {
        assert_eq!(normalize_group_name("  Acme  "), "Acme");
        assert_eq!(normalize_group_name("a\nb"), "a b");
        let long = "x".repeat(60);
        assert_eq!(
            normalize_group_name(&long).chars().count(),
            GROUP_NAME_MAX_CHARS
        );
        assert_eq!(normalize_group_name("   "), "");
    }

    #[test]
    fn names_collide_without_regard_to_case_but_not_with_themselves() {
        let groups = [group(1, "Acme"), group(2, "Personal")];
        assert_eq!(group_named(&groups, " acme ", None).map(|g| g.id), Some(1));
        assert_eq!(group_named(&groups, "ACME", Some(1)), None);
        assert_eq!(group_named(&groups, "", None), None);
    }

    #[test]
    fn the_naming_hint_says_what_enter_will_do() {
        let groups = [group(1, "Acme"), group(2, "")];
        assert_eq!(
            naming_hint(NamingKind::New, "", &groups, 2).as_deref(),
            Some("\u{23ce} create \u{b7} esc cancel")
        );
        assert_eq!(
            naming_hint(NamingKind::New, "acme", &groups, 2).as_deref(),
            Some("Acme exists \u{b7} \u{23ce} adds to it")
        );
        assert_eq!(naming_hint(NamingKind::Rename, "Other", &groups, 2), None);
        assert_eq!(
            naming_hint(NamingKind::Rename, "ACME", &groups, 2).as_deref(),
            Some("name in use \u{b7} \u{23ce} does nothing")
        );
        // Its own name in other capitals is not "in use".
        assert_eq!(naming_hint(NamingKind::Rename, "ACME", &groups, 1), None);
    }

    #[test]
    fn a_quiet_folded_group_counts_its_projects() {
        assert_eq!(
            folded_group_words(FoldedTally::default(), 1),
            vec![TallyWord::Projects(1)]
        );
        assert_eq!(TallyWord::Projects(1).text(), "1 project");
        assert_eq!(TallyWord::Projects(3).text(), "3 projects");
    }

    #[test]
    fn a_folded_group_names_trouble_then_waiting_then_work_then_news() {
        let tally = FoldedTally {
            failed: 1,
            waiting: 2,
            running: 3,
            finished: 4,
        };
        let words: Vec<String> = folded_group_words(tally, 5)
            .into_iter()
            .map(TallyWord::text)
            .collect();
        assert_eq!(words, ["1 failed", "2 waiting", "3 running", "4 finished"]);
    }

    const CHAR: f32 = 5.7;
    const GAP: f32 = 6.0;

    #[test]
    fn at_full_width_everything_is_drawn() {
        let words = [
            TallyWord::Failed(1),
            TallyWord::Waiting(1),
            TallyWord::Running(2),
        ];
        let fit = fit_label_row(&words, true, 60.0, 360.0, CHAR, GAP);
        assert!(fit.private_word);
        assert_eq!(fit.words, words);
    }

    #[test]
    fn private_goes_before_the_summary() {
        let words = [TallyWord::Failed(1), TallyWord::Running(2)];
        // Room for label, failed and running, but not also `private`.
        let needed = 48.0 + GAP + 2.0 * GAP + ("1 failed".len() + "2 running".len()) as f32 * CHAR;
        let fit = fit_label_row(&words, true, 60.0, needed + 1.0, CHAR, GAP);
        assert!(!fit.private_word);
        assert_eq!(fit.words, words);
    }

    #[test]
    fn running_and_finished_leave_but_failed_and_waiting_never_do() {
        // The 48e row at 224: failed, waiting and running do not fit together.
        let words = [
            TallyWord::Failed(1),
            TallyWord::Waiting(1),
            TallyWord::Running(2),
            TallyWord::Finished(1),
        ];
        let fit = fit_label_row(&words, false, 60.0, 150.0, CHAR, GAP);
        assert_eq!(fit.words, [TallyWord::Failed(1), TallyWord::Waiting(1)]);
        // Even with no room at all, the two that stay stay.
        let fit = fit_label_row(&words, true, 60.0, 10.0, CHAR, GAP);
        assert_eq!(fit.words, [TallyWord::Failed(1), TallyWord::Waiting(1)]);
        assert!(!fit.private_word);
    }

    #[test]
    fn the_label_keeps_its_whole_name_while_the_row_has_room() {
        // `PERSONAL` at mono 10 with 7px padding either side: 76px. At 389 it
        // is drawn whole beside `private`; it used to be cut to its floor.
        let natural = 8.0 * 6.0 + 14.0;
        let fit = fit_label_row(&[], true, natural, 389.0 - 44.0, CHAR, GAP);
        assert!(fit.private_word);
        assert_eq!(fit.label_width, natural);
        // A long name stops at the ceiling.
        let fit = fit_label_row(&[], false, 400.0, 389.0 - 44.0, CHAR, GAP);
        assert_eq!(fit.label_width, 130.0);
        // Squeezed by the summary, it gives way down to the floor and no
        // further.
        let words = [TallyWord::Failed(1), TallyWord::Waiting(1)];
        let fit = fit_label_row(&words, false, 400.0, 150.0, CHAR, GAP);
        assert!(fit.label_width < 130.0 && fit.label_width >= 48.0);
        let fit = fit_label_row(&words, false, 400.0, 10.0, CHAR, GAP);
        assert_eq!(fit.label_width, 48.0);
        // A short name keeps its own width: the floor is for squeezing.
        let fit = fit_label_row(&[], false, 39.0, 345.0, CHAR, GAP);
        assert_eq!(fit.label_width, 39.0);
    }

    #[test]
    fn running_is_kept_when_it_fits_without_finished() {
        let words = [TallyWord::Running(2), TallyWord::Finished(3)];
        let only_running = 48.0 + GAP + GAP + "2 running".len() as f32 * CHAR;
        let fit = fit_label_row(&words, false, 60.0, only_running + 1.0, CHAR, GAP);
        assert_eq!(fit.words, [TallyWord::Running(2)]);
    }

    #[test]
    fn blank_and_repeated_groups_are_dropped_on_restore() {
        let records = [
            ProjectGroupSession {
                id: 1,
                name: " Acme ".to_string(),
                collapsed: true,
                private: true,
            },
            ProjectGroupSession {
                id: 1,
                name: "Again".to_string(),
                collapsed: false,
                private: false,
            },
            ProjectGroupSession {
                id: 2,
                name: "  ".to_string(),
                collapsed: false,
                private: false,
            },
        ];
        let groups = groups_from_session(&records);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Acme");
        assert!(groups[0].collapsed && groups[0].private);
    }

    #[test]
    fn a_group_lives_exactly_while_it_holds_a_project() {
        let mut groups = vec![group(1, "Acme"), group(2, "Empty")];
        let mut memberships = [Some(1), Some(9), None];
        reconcile_groups(&mut groups, memberships.iter_mut());
        assert_eq!(groups, vec![group(1, "Acme")]);
        assert_eq!(memberships, [Some(1), None, None]);
    }

    #[test]
    fn a_new_id_is_past_every_id_in_use() {
        assert_eq!(next_group_id(&[], 1), 1);
        assert_eq!(next_group_id(&[group(4, "a"), group(2, "b")], 1), 5);
        assert_eq!(next_group_id(&[group(4, "a")], 9), 9);
    }
}
