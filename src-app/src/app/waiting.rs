//! What is waiting for the user, counted once for the whole window.
//!
//! Two things in this app can stop on a question, and they come from the same
//! `ai.*` hook stream through two different doors: an **agent surface** parked
//! on its container ([`crate::project::Thread::status`]) and an **agent session
//! bound to a pane** (`Workspace::agent_sessions`). A frame reaches one or the
//! other and never both - containers and surfaces draw their ids from one
//! counter, so the handler's two lookups are mutually exclusive - which is
//! exactly why a count that names only one of them is wrong.
//!
//! The design puts that count in one place: the title bar's waiting chip, or
//! the right end of the toolbar under a system frame. Before it existed, the
//! rail's *collapsed* container row carried the aggregate, because otherwise a
//! waiting agent behind a closed caret was invisible. It no longer does: the
//! chip is on screen whatever the rail is doing.

use gpui::{Context, Hsla, Window};

use crate::SplitlaneApp;
use crate::ai_types::AgentState;
use crate::app::workspace_ops::WorkspaceFocusTarget;
use crate::project::{AgentsTarget, ThreadStatus};

/// One thing that has stopped on the user, and how to get to it.
#[derive(Clone)]
pub(crate) enum WaitingStop {
    /// An agent surface of a container - selecting it is what shows it.
    Surface(AgentsTarget),
    /// An agent running in a pane, which may be a background tab.
    Pane {
        ws_idx: usize,
        pane: gpui::Entity<crate::pane::Pane>,
        tab_idx: usize,
    },
}

/// The container a stop is in.
pub(crate) fn stop_ws_idx(stop: &WaitingStop) -> Option<usize> {
    match stop {
        WaitingStop::Surface(AgentsTarget::Thread { ws_idx, .. }) => Some(*ws_idx),
        WaitingStop::Surface(_) => None,
        WaitingStop::Pane { ws_idx, .. } => Some(*ws_idx),
    }
}

impl SplitlaneApp {
    /// Everything waiting for the user, in the rail's own order: containers top
    /// to bottom, and inside a container its surfaces before its panes.
    ///
    /// A pane session whose surface no longer exists in any layout is dropped
    /// rather than counted - the chip must never promise a destination that is
    /// not there, which is the same rule the attention queue follows.
    ///
    /// One door, not two. An unanswered permission ask used to be the second:
    /// the hook held the call open and said nothing about the turn, so
    /// `Thread::status` never moved and the chip would have read "no agent is
    /// waiting" over a pane with its bar up. Nothing holds a call open any
    /// more, and the detector - which reads the agent's own transcript - sees
    /// a permission wait for what it is.
    pub(crate) fn waiting_stops(&self, cx: &Context<Self>) -> Vec<WaitingStop> {
        self.stops_matching(AgentState::WaitingForInput, cx)
    }

    /// [`Self::waiting_stops`] for any one agent state - the same order, the
    /// same two doors.
    ///
    /// `⌥⇥` used to walk only the second door (`agent_sessions`, resolved to a
    /// pane), which meant it could not reach an agent surface at all: those
    /// report through `Thread::status`. The chip and the chord name the same
    /// destination in `FOCUS.md`, so they read the same list.
    pub(crate) fn stops_matching(&self, state: AgentState, cx: &Context<Self>) -> Vec<WaitingStop> {
        let thread_status = ThreadStatus::from_agent_state(state.clone());
        let mut stops = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for (thread_idx, thread) in ws.threads.iter().enumerate() {
                if thread.status == thread_status {
                    stops.push(WaitingStop::Surface(AgentsTarget::Thread {
                        ws_idx,
                        thread_idx,
                    }));
                }
            }
            let waiting: std::collections::HashSet<u64> = ws
                .agent_sessions
                .values()
                .filter(|session| session.state == state)
                .filter_map(|session| session.surface_id)
                .collect();
            if waiting.is_empty() {
                continue;
            }
            let Some(root) = &ws.root else { continue };
            for pane in root.collect_leaves() {
                for (tab_idx, tab) in pane.read(cx).tabs.iter().enumerate() {
                    if let Some(terminal) = tab.as_terminal()
                        && waiting.contains(&terminal.entity_id().as_u64())
                    {
                        stops.push(WaitingStop::Pane {
                            ws_idx,
                            pane: pane.clone(),
                            tab_idx,
                        });
                    }
                }
            }
        }
        stops
    }

    /// Everything whose run fell over, by the same two doors.
    ///
    /// The scale gives a failed run its own rung above waiting, and this is
    /// where the rung's count comes from. `AgentState::Errored` is the shim's
    /// `ai.exit` for a pane session and `ThreadStatus::Failed` for a surface -
    /// `stops_matching` already knows both, so lifting the rung cost a call and
    /// not a second walk.
    pub(crate) fn failed_stops(&self, cx: &Context<Self>) -> Vec<WaitingStop> {
        self.stops_matching(AgentState::Errored, cx)
    }

    /// What the chip is looking at, counted once for the whole window.
    ///
    /// **Counted off the popover's own rows**, and that is the point rather
    /// than an implementation detail: the chip, the popover's subtitle, its
    /// sections and the announcement's edge are then one derivation, and a
    /// session that is two things at once - a run that fell over after having
    /// finished out of sight - is one row and counted once. Deriving them
    /// separately is exactly how the chip once said "3 agents waiting" over a
    /// popover listing two.
    pub(crate) fn activity_counts(&self, cx: &Context<Self>) -> ActivityCounts {
        ActivityCounts::from_rows(&self.attention_queue_rows(cx))
    }
}

/// The three things Activity holds, and how far the reader has got with the
/// third. [`chip_state`] turns this into a rung, a shape and a sentence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ActivityCounts {
    /// Runs that fell over.
    pub(crate) failed: usize,
    /// Agents standing on an answer.
    pub(crate) waiting: usize,
    /// Runs that ended out of sight and have not been collected.
    pub(crate) finished: usize,
    /// How many of those nobody has seen even in the list.
    pub(crate) finished_unacknowledged: usize,
}

impl ActivityCounts {
    /// Count one list of rows.
    ///
    /// **The one derivation.** The chip, the popover's subtitle and its section
    /// headings are then the same arithmetic over the same rows rather than
    /// three walks that can disagree - which is what once let the chip say
    /// "3 agents waiting" over a popover listing two.
    pub(crate) fn from_rows(rows: &[crate::app::attention_queue::QueueRow]) -> Self {
        use crate::app::attention_queue::QueueKind;
        let count = |kind: QueueKind| rows.iter().filter(|row| row.kind == kind).count();
        Self {
            failed: count(QueueKind::Failed),
            waiting: count(QueueKind::Waiting),
            finished: count(QueueKind::Finished),
            finished_unacknowledged: rows.iter().filter(|row| row.unacknowledged).count(),
        }
    }

    /// Is there anything at all? This is the queue's own emptiness, and so the
    /// thing the announcement's edge is an edge of.
    pub(crate) fn is_empty(&self) -> bool {
        self.failed == 0 && self.waiting == 0 && self.finished == 0
    }
}

impl SplitlaneApp {
    /// Show one stop and hand it the keyboard.
    ///
    /// A surface goes through `select_thread`, which is the targeting ladder:
    /// a waiting agent already in a pane is simply focused there, and one that
    /// is not gets a pane by the same rule a rail click would give it. This is
    /// the one automatic focus move in the app, and it is not automatic - the
    /// user pressed the chip or the chord.
    pub(crate) fn go_to_stop(
        &mut self,
        stop: WaitingStop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match stop {
            WaitingStop::Surface(target) => {
                let AgentsTarget::Thread { ws_idx, .. } = target else {
                    return;
                };
                self.active_idx = ws_idx;
                self.select_agents_target(target, cx);
            }
            WaitingStop::Pane {
                ws_idx,
                pane,
                tab_idx,
            } => {
                self.activate_workspace_at(
                    ws_idx,
                    WorkspaceFocusTarget::PaneTab { pane, tab_idx },
                    window,
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// A stop's stable identity, so `⌥⇥` can cycle without re-deriving
    /// positions that shift under it.
    pub(crate) fn stop_key(&self, stop: &WaitingStop, cx: &Context<Self>) -> Option<u64> {
        match stop {
            WaitingStop::Surface(AgentsTarget::Thread { ws_idx, thread_idx }) => {
                Some(self.workspaces.get(*ws_idx)?.threads.get(*thread_idx)?.id)
            }
            WaitingStop::Surface(_) => None,
            WaitingStop::Pane { pane, tab_idx, .. } => pane
                .read(cx)
                .tabs
                .get(*tab_idx)
                .map(|tab| tab.entity_id().as_u64()),
        }
    }
}

/// The chip's label. Pure, because "1 agent waiting" and "2 agents waiting" is
/// the whole of it and the plural is the only thing that can be wrong.
pub(crate) fn waiting_label(count: usize) -> String {
    if count == 1 {
        "1 agent waiting".to_string()
    } else {
        format!("{count} agents waiting")
    }
}

/// Which rung of the chip's scale is speaking. **Tone carries the rung**, and
/// nothing else does.
///
/// The current scale is a rebuild rather than an addition. It
/// used to run `error > waiting > finished > running > idle` with an unread
/// result at the bottom of what the chip could show - which drew this app's
/// main documented failure mode, a pile of results nobody has collected,
/// quieter than everything else. Two arguments moved it:
///
/// > A waiting agent is standing because it needs an answer; a finished one is
/// > standing because its result has been accepted by nobody. "Just done" would
/// > assume an agent's output can be trusted unread; an uncollected finish is
/// > unchecked work, not a closed task. One thing holds both, and one action
/// > releases both.
///
/// So the real axis is **read / unread**, `running` is not on the scale at all
/// (a running session is the one thing on screen that moves by itself, and it
/// needs nobody), and the two kinds are told apart by the dot's *shape* rather
/// than by brightness - which is the app's own rule about spending hue, shape
/// or presence before the axis that carries rank and legibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChipRung {
    /// A run fell over: the question is whether to start it again. Its own rung
    /// above waiting, and **nothing on the reading axis clears it** - which is
    /// the answer to the objection that kept `failed` out of the chip until
    /// now ("an error that vanishes when glanced at is wrong"). It goes when
    /// the session goes somewhere else.
    Failed,
    /// Standing on the reader, and not yet acknowledged: they have not seen
    /// even the list. The accent rung.
    Unacknowledged,
    /// The quiet mark - known, not collected. Opening Activity lowers a mark to
    /// here; only the surface coming up in a pane takes it away.
    Quiet,
}

/// The chip's leading dot, whose **shape carries the kind** - never the rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChipDot {
    /// Filled: the session is standing by itself and wants something from you.
    /// Waiting and failed are both this - a failed run has nothing to accept,
    /// and the action is to decide rather than to read.
    Filled,
    /// Hollow: there is something here to accept.
    Hollow,
}

/// What the chip says, in which tone, with which dot - or nothing, when there
/// is nothing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChipState {
    pub(crate) label: String,
    pub(crate) rung: ChipRung,
    pub(crate) dot: ChipDot,
}

/// The scale, in one function.
///
/// **The chip is on screen when the popover has something to say**, and the
/// scale decides *how loudly*. Order: a fallen run, then anything
/// standing on the reader, then the quiet mark. Waiting and an uncollected
/// finish share the accent rung and are told apart by the dot; within the rung
/// waiting still speaks, because it is the half whose count the reader can act
/// on immediately and the other number is one click away.
///
/// **Waiting is never "acknowledged".** Reading a list does not answer an
/// agent's question - what takes a waiting agent off this rung is the agent
/// moving on. Only the finished mark has a reader-side level, which is why it
/// is the only one that can go quiet.
pub(crate) fn chip_state(counts: ActivityCounts) -> Option<ChipState> {
    let ActivityCounts {
        failed,
        waiting,
        finished,
        finished_unacknowledged,
    } = counts;
    if failed > 0 {
        return Some(ChipState {
            label: failed_label(failed),
            rung: ChipRung::Failed,
            dot: ChipDot::Filled,
        });
    }
    if waiting > 0 {
        return Some(ChipState {
            label: waiting_label(waiting),
            rung: ChipRung::Unacknowledged,
            dot: ChipDot::Filled,
        });
    }
    if finished > 0 {
        return Some(ChipState {
            // The whole pile, not just its unread part: the popover lists all
            // of them, and a chip that counted one set over a list of another
            // is the exact defect that made the chip and the popover read one
            // list in the first place.
            label: finished_label(finished),
            rung: if finished_unacknowledged > 0 {
                ChipRung::Unacknowledged
            } else {
                ChipRung::Quiet
            },
            dot: ChipDot::Hollow,
        });
    }
    None
}

/// The muted chip's label. No noun: `finished` already says what finished, and
/// the row it came from names which one.
pub(crate) fn finished_label(count: usize) -> String {
    format!("{count} finished")
}

/// The top rung's label, in the app's own status word.
pub(crate) fn failed_label(count: usize) -> String {
    format!("{count} failed")
}

/// The popover's subtitle: a summary of the halves that exist rather than a
/// count of one.
///
/// It replaced `3 of 5 agent sessions`, which answered a question the popover
/// had stopped asking - the surface is named `Activity` now and holds three
/// kinds of thing, so its subtitle has to say how much of each.
pub(crate) fn activity_summary(counts: ActivityCounts) -> Option<String> {
    let mut parts = Vec::new();
    if counts.failed > 0 {
        parts.push(format!("{} failed", counts.failed));
    }
    if counts.waiting > 0 {
        parts.push(format!("{} waiting", counts.waiting));
    }
    if counts.finished > 0 {
        parts.push(format!("{} finished", counts.finished));
    }
    // **Nothing at all when there is nothing to count**. It read
    // `nothing waiting`; the empty body below now says what an empty Activity
    // means, and one popover may not say it twice in two phrasings.
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// What an empty Activity says: the surface's name, then its membership rule.
///
/// **Stop denying and start defining** - and the argument is worth
/// keeping because it is not the obvious one. Three absences cannot be denied
/// in one clause, and denying them in three ("nothing failed, nobody waiting,
/// everything collected") puts the reader in the position of auditing a list at
/// the exact moment there is nothing to audit. An empty surface is the one
/// place where naming what the surface *holds* is not redundant: anywhere else
/// it would be instruction-manual noise, here it is the most useful thing on
/// screen, and it is true of all three memberships by construction - because
/// that list **is** the membership rule.
///
/// Three properties it was checked against. **No new term:** `failed`,
/// `waiting` and `unread` are words this app already uses about sessions, and
/// `Nothing in \u{2026}` is the grammar of the empty states already shipped, so the
/// line joins a pattern instead of starting one. **True cold and true live:**
/// the state is reached both by watching the last row clear and by opening the
/// surface with nothing in it, and a moment-flavoured line ("that was the last
/// one") is right in the first case and false in the second. And it is **the
/// rarest line on screen**, read by somebody who is by definition not in a
/// hurry - the one moment a surface can afford a sentence about itself.
///
/// It replaced `No agent is waiting for you`, which did not lie - no agent was
/// waiting - but answered about one of three memberships, in a surface the
/// reader opens to check all three, in the app's most authoritative voice.
pub(crate) const EMPTY_ACTIVITY_TITLE: &str = "Nothing in Activity";
/// The rule line under [`EMPTY_ACTIVITY_TITLE`].
pub(crate) const EMPTY_ACTIVITY_RULE: &str = "Failed, waiting and unread runs appear here.";

/// The failed section's own heading, above the waiting one.
pub(crate) fn failed_section_label(count: usize) -> String {
    if count == 1 {
        "1 run failed".to_string()
    } else {
        format!("{count} runs failed")
    }
}

/// The chip's tones, in one place, keyed by rung.
///
/// **Every value here is a role**, which is the rule worth keeping
/// past this control: *a hex literal in a spec is a bug report about a missing
/// role, never a value to ship* - in this codebase most of all, where a
/// hard-coded colour is one no theme can reach. The failed rung takes
/// `agent_error`, the role the rail's own failed dot has always used, and its
/// pill is that hue through the same two blends `accent_surface` and
/// `accent_border` are - so the top rung cannot drift from the one below it.
fn chip_tones(rung: ChipRung, ui: crate::theme::UiColors) -> (Hsla, Hsla, Hsla, Hsla) {
    match rung {
        // Hue, not brightness, is what puts this above the accent - and the
        // accent deliberately never sat at the top of the range, precisely so
        // an error would have somewhere to go.
        ChipRung::Failed => (
            ui.agent_error,
            ui.tinted_surface(ui.agent_error),
            ui.tinted_border(ui.agent_error),
            ui.agent_error,
        ),
        ChipRung::Unacknowledged => (ui.accent, ui.accent_surface, ui.accent_border, ui.accent),
        ChipRung::Quiet => (ui.text_tertiary, ui.subtle, ui.border_strong, ui.dim),
    }
}

/// The chip itself: one pill, one dot, one sentence - **and one drawing.**
///
/// It used to be two, one per window frame, under "one decision, two
/// drawings", which held while the only shared thing was *whether* the chip
/// existed. The scale then gave it three rungs and two dot shapes and the
/// announcement gave it a movement, and three axes duplicated across two files
/// is three ways for the frames to disagree. So the tone, the shape and the
/// announcement live here; the two frames keep their own geometry, their own
/// id and their own tooltip, which is what genuinely differs between a 22px
/// title bar and a 24px toolbar.
///
/// **Three levels, not two**. The neutral
/// state is deliberately *not* level with `Add pane` and `Files`: that would
/// erase the line between a state of the system and a button, and the failure
/// mode in this app is not a missed question - an idle agent reasserts itself
/// by standing still - but a pile of uncollected results, which grows
/// invisibly when it is drawn like navigation.
///
/// ```text
/// failed           error text · error dot   · error fill  · error border
/// unacknowledged   accent text · accent dot · accent fill · accent border
/// quiet            dim text   · dim dot     · tinted fill · neutral border
/// buttons          text tone  · no dot      · surf fill   · button border
/// ```
///
/// The pill against the buttons' rounded rectangle stays the only difference in
/// silhouette, which is the row's one shape rule: status versus command.
///
/// The one place the two prototypes name different roles is the resting border
/// (`border_strong` dark, `border` light) and `UiColors` writes one name, so it
/// cannot be both. `border_strong` is the answer, because it is the only single
/// role that keeps the chip's edge as visible on its own fill as a button's is
/// on its - the chip sits on `subtle` and a button on `overlay`, so it measures
/// 1.180 : 1 against the button's 1.173 in dark and 1.408 against 1.505 in
/// light, where plain `border` would give 1.246.
pub(crate) fn chip_body(
    id: &'static str,
    height: f32,
    dot_size: f32,
    chip: &ChipState,
    announcing: Option<crate::app::announce::AnnouncePhase>,
    ui: crate::theme::UiColors,
) -> gpui::Stateful<gpui::Div> {
    use gpui::{InteractiveElement, ParentElement, Styled, div, px};

    let (text, fill, border, dot) = chip_tones(chip.rung, ui);
    div()
        .id(id)
        .flex_none()
        .h(px(height))
        .px(crate::ui_tokens::space::LG)
        .flex()
        .flex_row()
        .items_center()
        .gap(crate::ui_tokens::space::SM)
        .rounded_full()
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_size(crate::ui_tokens::text::CAPTION)
        .text_color(text)
        .cursor_pointer()
        .child(crate::app::announce::announceable_dot(
            gpui::SharedString::from(id),
            dot_size,
            match chip.dot {
                ChipDot::Filled => crate::app::announce::DotFill::Solid(dot),
                ChipDot::Hollow => crate::app::announce::DotFill::Ring(dot),
            },
            // The halo is the accent in every rung: the dot is the state and
            // carries the rank, the halo is the event and carries none.
            ui.accent,
            announcing,
        ))
        .child(chip.label.clone())
}

/// What the chip's tooltip promises.
///
/// **Keyed on what the chip is counting, not on its rung**, and worded in the
/// popover's own section headings rather than in new vocabulary: a chip reading
/// `3 finished` on the accent rung is still about finishes, and promising
/// "waiting" there would name the wrong half.
///
/// The title bar used to say `Go to the first agent waiting for you` while the
/// toolbar said `See which sessions are waiting for you` - two sentences for
/// one control in two frames, and the first was simply untrue: the chip opens
/// Activity, it does not travel anywhere.
pub(crate) fn chip_tooltip(chip: &ChipState) -> &'static str {
    match (chip.rung, chip.dot) {
        (ChipRung::Failed, _) => "See which runs failed",
        (_, ChipDot::Filled) => "See which sessions are waiting for you",
        (_, ChipDot::Hollow) => "See what finished while you were away",
    }
}

/// Whether this rung's chip has a hover treatment of its own.
///
/// The two tinted rungs do not: in the design their hover border **is** their
/// resting one, so there is nothing to animate. The quiet rung takes the shared
/// `border_hover` and deliberately not a softer one - a softer hover would be a
/// fourth carrier on top of hue, dot and fill, spending brightness - the axis
/// this interface reserves for rank and availability - and hover answers "is
/// this clickable?", which the chip is.
pub(crate) fn chip_hovers(rung: ChipRung) -> bool {
    rung == ChipRung::Quiet
}

/// The waiting section's own heading, which is always true where it stands.
pub(crate) fn waiting_section_label(count: usize) -> String {
    format!("{count} waiting for you")
}

/// The line under both sections, naming what the cycling chord does - and it
/// **follows the sections**, which is the chip's rule (on screen only when
/// the popover has something to say) stated once more. `each is answered in
/// its own terminal` promises questions to answer; with nothing waiting there
/// are none, and what `⌥⇥` cycles is results to read. Same reason
/// `Waiting for you` came off the title.
///
/// `chord` is the chord that is really bound, because a hint naming one the
/// user has rebound is worse than no hint at all.
pub(crate) fn cycle_hint(chord: Option<&str>, has_waiting: bool) -> String {
    // **The chord walks only the waiting ones** - `handle_jump_next_waiting` is
    // `waiting_stops` and nothing else. With nothing waiting it reaches
    // nothing, so naming it here would be the footer promising a key that goes
    // nowhere. That was already false for a finished-only popover before the
    // scale's rebuild - ⌥⇥ has never visited a finished row - and the rebuild's
    // third section would have made it false twice over. The sentence keeps the half that is
    // true.
    if !has_waiting {
        return "opening one marks it read".to_string();
    }
    match chord {
        Some(chord) => format!("{chord} cycles them · each is answered in its own terminal"),
        // With no chord bound there is nothing to cycle them *with*, so the
        // sentence states the half that is still true rather than naming a key
        // that does not exist.
        None => "cycling them is unassigned · each is answered in its own terminal".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActivityCounts, ChipDot, ChipRung, activity_summary, chip_state, cycle_hint, failed_label,
        failed_section_label, finished_label, waiting_label, waiting_section_label,
    };

    fn counts(
        failed: usize,
        waiting: usize,
        finished: usize,
        unacknowledged: usize,
    ) -> ActivityCounts {
        ActivityCounts {
            failed,
            waiting,
            finished,
            finished_unacknowledged: unacknowledged,
        }
    }

    /// The footer follows the sections, for the same reason `Waiting for you`
    /// came off the title: with nothing waiting, promising things to answer is
    /// a lie, and what the chord cycles is results to read.
    #[test]
    fn the_footer_names_the_chord_only_where_the_chord_goes() {
        assert_eq!(
            cycle_hint(Some("⌥⇥"), true),
            "⌥⇥ cycles them · each is answered in its own terminal"
        );
        // **The chord walks only the waiting ones**, so with nothing waiting it
        // reaches nothing and the footer may not say it cycles anything. It
        // said exactly that until now, over a popover of finished rows the
        // chord has never visited.
        assert_eq!(cycle_hint(Some("⌥⇥"), false), "opening one marks it read");
        assert!(!cycle_hint(Some("⌥⇥"), false).contains("cycles"));
        // Unbound: the half that is still true survives, and no key is named.
        assert!(!cycle_hint(None, true).contains("⇥"));
        assert!(cycle_hint(None, true).starts_with("cycling them is unassigned"));
    }

    #[test]
    fn the_chip_counts_in_words_and_gets_the_plural_right() {
        assert_eq!(waiting_label(1), "1 agent waiting");
        assert_eq!(waiting_label(2), "2 agents waiting");
        assert_eq!(waiting_label(11), "11 agents waiting");
    }

    /// The scale in one test: on screen when the popover has something to say,
    /// a fallen run above everything, and waiting speaking for its own rung.
    #[test]
    fn the_chip_speaks_for_the_highest_rung_that_has_anything_on_it() {
        assert_eq!(chip_state(counts(0, 0, 0, 0)), None);

        let waiting = chip_state(counts(0, 2, 0, 0)).expect("two agents waiting");
        assert_eq!(waiting.label, "2 agents waiting");
        assert_eq!(waiting.rung, ChipRung::Unacknowledged);
        assert_eq!(waiting.dot, ChipDot::Filled);

        // The scale's whole point: an uncollected result stands on the *same*
        // rung as a waiting agent, and the dot's shape is what tells them
        // apart.
        let unread = chip_state(counts(0, 0, 3, 3)).expect("three uncollected");
        assert_eq!(unread.label, "3 finished");
        assert_eq!(unread.rung, ChipRung::Unacknowledged);
        assert_eq!(unread.dot, ChipDot::Hollow);

        // Seen in the list and still uncollected: same shape, quieter tone.
        let quiet = chip_state(counts(0, 0, 3, 0)).expect("three, all seen");
        assert_eq!(quiet.rung, ChipRung::Quiet);
        assert_eq!(quiet.dot, ChipDot::Hollow);

        // A fallen run outranks both, and takes the filled dot: there is
        // nothing to accept, the action is to decide.
        let failed = chip_state(counts(1, 2, 3, 3)).expect("one failed run");
        assert_eq!(failed.label, "1 failed");
        assert_eq!(failed.rung, ChipRung::Failed);
        assert_eq!(failed.dot, ChipDot::Filled);

        // Within the accent rung, waiting still speaks.
        assert_eq!(
            chip_state(counts(0, 2, 3, 3)).map(|chip| chip.label),
            Some("2 agents waiting".to_string())
        );
        assert_eq!(finished_label(1), "1 finished");
        assert_eq!(failed_label(1), "1 failed");
    }

    /// The chip counts the whole pile, not its unread part - the popover lists
    /// all of them, and two numbers six pixels apart may not disagree.
    #[test]
    fn the_finished_count_is_the_pile_and_the_rung_is_the_unread_part() {
        let mixed = chip_state(counts(0, 0, 5, 1)).expect("five, one unseen");
        assert_eq!(mixed.label, "5 finished");
        assert_eq!(mixed.rung, ChipRung::Unacknowledged);
    }

    #[test]
    fn the_summary_names_only_the_halves_that_exist() {
        // **Nothing, not "nothing waiting"**: the empty body says
        // what an empty Activity means, and one popover may not say it twice in
        // two phrasings.
        assert_eq!(activity_summary(counts(0, 0, 0, 0)), None);
        assert_eq!(
            activity_summary(counts(0, 2, 0, 0)).as_deref(),
            Some("2 waiting")
        );
        assert_eq!(
            activity_summary(counts(0, 0, 3, 0)).as_deref(),
            Some("3 finished")
        );
        assert_eq!(
            activity_summary(counts(0, 2, 3, 0)).as_deref(),
            Some("2 waiting \u{b7} 3 finished")
        );
        assert_eq!(
            activity_summary(counts(1, 2, 3, 0)).as_deref(),
            Some("1 failed \u{b7} 2 waiting \u{b7} 3 finished")
        );
        assert_eq!(waiting_section_label(1), "1 waiting for you");
        assert_eq!(failed_section_label(1), "1 run failed");
        assert_eq!(failed_section_label(2), "2 runs failed");
    }
}
