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
        self.stops_in(
            ThreadStatus::from_agent_state(state.clone()),
            std::slice::from_ref(&state),
            cx,
        )
    }

    /// Every agent at work, by the same two doors. A pane session whose agent
    /// has gone quiet mid-turn is still in its turn, which is how a surface's
    /// status already reads it.
    pub(crate) fn running_stops(&self, cx: &Context<Self>) -> Vec<WaitingStop> {
        self.stops_in(
            ThreadStatus::Thinking,
            &[AgentState::Thinking, AgentState::Stalled],
            cx,
        )
    }

    fn stops_in(
        &self,
        thread_status: ThreadStatus,
        session_states: &[AgentState],
        cx: &Context<Self>,
    ) -> Vec<WaitingStop> {
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
                .filter(|session| session_states.contains(&session.state))
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
        let mut counts = ActivityCounts::from_rows(&self.attention_queue_rows(cx));
        counts.agents_open = counts.running > 0
            || !counts.is_empty()
            || self
                .workspaces
                .iter()
                .flat_map(|ws| ws.threads.iter())
                .any(crate::app::agents_sidebar::is_agent_session);
        counts
    }
}

/// What Activity holds, and how far the reader has got with the finished
/// part. [`chip_state`] turns this into a rung and a summary.
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
    /// Agents at work. Counted, never announced: see [`Self::is_empty`].
    pub(crate) running: usize,
    /// Whether any agent session is open at all - the chip's whole condition
    /// for being on screen. A session where only a shell runs does not count.
    pub(crate) agents_open: bool,
}

impl ActivityCounts {
    /// Count one list of rows.
    ///
    /// **The one derivation.** The chip, the popover's subtitle and its section
    /// headings are then the same arithmetic over the same rows rather than
    /// three walks that can disagree - which is what once let the chip say
    /// "3 agents waiting" over a popover listing two.
    ///
    /// `agents_open` is not a fact about rows; the caller that can see the
    /// rail sets it.
    pub(crate) fn from_rows(rows: &[crate::app::attention_queue::QueueRow]) -> Self {
        use crate::app::attention_queue::QueueKind;
        let count = |kind: QueueKind| rows.iter().filter(|row| row.kind == kind).count();
        Self {
            failed: count(QueueKind::Failed),
            waiting: count(QueueKind::Waiting),
            finished: count(QueueKind::Finished),
            finished_unacknowledged: rows.iter().filter(|row| row.unacknowledged).count(),
            running: count(QueueKind::Running),
            agents_open: false,
        }
    }

    /// Is there anything standing on the reader? This is the queue's own
    /// emptiness, and so the thing the announcement's edge is an edge of.
    ///
    /// `running` is left out on purpose: an agent starting work needs nobody,
    /// and an announcement for it would be the chip calling about the one
    /// thing on screen that moves by itself.
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
        // A session in a folded group is shown with its group opened - a
        // private one too: the person asked to go there. Activity and the
        // chord both come through here.
        if let Some(ws_idx) = stop_ws_idx(&stop) {
            self.reveal_project_group(ws_idx, cx);
        }
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

/// Which rung of the chip's scale is speaking: what the pill itself says.
///
/// The chip used to speak for its highest rung only - `1 failed`, else
/// `2 agents waiting`, else `3 finished` - and was on screen only when one of
/// them was. It is now on screen whenever an agent session is open and says a
/// summary of up to three parts (see [`chip_state`]); the rung is what is left
/// of the old scale, and it decides the pill alone. The reasoning is in
/// `docs/internals/agent-state-and-the-rail.md`, under the Activity chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChipRung {
    /// A run fell over. The one rung that tints the pill: nothing on the
    /// reading axis clears it, and it goes when the session goes somewhere
    /// else.
    Failed,
    /// Somebody is waiting for an answer. The pill stays neutral and the dot
    /// carries the attention hue; this rung differs from [`Self::Neutral`] only
    /// in having no hover of its own.
    Waiting,
    /// Agents are open and none of them wants anything. The neighbouring
    /// buttons' step, so a chip that is always there does not shout.
    Neutral,
}

/// One part of the chip's summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChipPart {
    Failed(usize),
    Waiting(usize),
    Running(usize),
}

impl ChipPart {
    /// The part as words (`2 waiting`) or, once width has run out, as its
    /// number alone - the dot beside it still says which number it is.
    pub(crate) fn text(self, short: bool) -> String {
        let (count, word) = match self {
            ChipPart::Failed(count) => (count, "failed"),
            ChipPart::Waiting(count) => (count, "waiting"),
            ChipPart::Running(count) => (count, "running"),
        };
        if short {
            count.to_string()
        } else {
            format!("{count} {word}")
        }
    }

    /// `failed` and `waiting` are what the chip answers for, so width never
    /// takes them - the rule a folded group's summary already follows.
    fn stays(self) -> bool {
        !matches!(self, ChipPart::Running(_))
    }
}

/// What the chip says, how loudly - or nothing, when no agent session is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChipState {
    /// In reading order: `failed`, `waiting`, `running`. Empty when agents
    /// are open and none of the three applies.
    pub(crate) parts: Vec<ChipPart>,
    /// Whether the parts are drawn as numbers alone.
    pub(crate) short: bool,
    pub(crate) rung: ChipRung,
}

impl ChipState {
    /// The summary as one line of text - what a test or a screen reader reads.
    pub(crate) fn label(&self) -> String {
        if self.parts.is_empty() {
            return EMPTY_CHIP_LABEL.to_string();
        }
        self.parts
            .iter()
            .map(|part| part.text(self.short))
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    }
}

/// What the chip says when agents are open and none of them is failed,
/// waiting or running: the name of the surface it opens.
pub(crate) const EMPTY_CHIP_LABEL: &str = "Activity";

/// The chip, in one function, given how many pixels its text may take.
///
/// **On screen whenever an agent session is open**, and absent otherwise. It
/// was on screen only while the list had something to say, which answered
/// "is anything standing on me?" and left "where is work happening?" to a
/// rail that scrolls once there are many projects.
///
/// At most three parts, `failed \u{b7} waiting \u{b7} running`: what needs
/// sorting out first stands on the left. `finished` is not one of them - an
/// unread result already has the ring on its rail row and its own section in
/// the list.
///
/// When `room` is short the words give way to bare numbers first, and then
/// `running` goes. `failed` and `waiting` always stay.
pub(crate) fn chip_state(counts: ActivityCounts, room: f32) -> Option<ChipState> {
    if !counts.agents_open {
        return None;
    }
    let parts: Vec<ChipPart> = [
        (counts.failed > 0).then_some(ChipPart::Failed(counts.failed)),
        (counts.waiting > 0).then_some(ChipPart::Waiting(counts.waiting)),
        (counts.running > 0).then_some(ChipPart::Running(counts.running)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let rung = if counts.failed > 0 {
        ChipRung::Failed
    } else if counts.waiting > 0 {
        ChipRung::Waiting
    } else {
        ChipRung::Neutral
    };
    let (parts, short) = fit_chip_parts(&parts, room, chip_char_width(), CHIP_PART_OVERHEAD);
    Some(ChipState { parts, short, rung })
}

/// One character of the chip's text at its size. The face is proportional,
/// so this is a deliberate over-estimate: the chip giving way one step early
/// costs nothing, a chip pushing the path out of the bar does.
fn chip_char_width() -> f32 {
    f32::from(crate::ui_tokens::text::CAPTION) * 0.6
}

/// What one part costs besides its text: its dot, the gap after the dot, and
/// the gap before the next part.
const CHIP_PART_OVERHEAD: f32 = 6. + 5. + 8.;

/// Which parts fit in `available` pixels, and whether as numbers alone.
/// Pure - unit-tested.
pub(crate) fn fit_chip_parts(
    parts: &[ChipPart],
    available: f32,
    char_width: f32,
    overhead: f32,
) -> (Vec<ChipPart>, bool) {
    let width = |parts: &[ChipPart], short: bool| -> f32 {
        parts
            .iter()
            .map(|part| part.text(short).chars().count() as f32 * char_width + overhead)
            .sum()
    };
    if width(parts, false) <= available {
        return (parts.to_vec(), false);
    }
    if width(parts, true) <= available {
        return (parts.to_vec(), true);
    }
    (
        parts.iter().copied().filter(|part| part.stays()).collect(),
        true,
    )
}

/// How many pixels the chip's summary may take in a bar `width` wide, of which
/// the rail takes `rail`.
///
/// A third of what is right of the rail. The chip shares the bar with the
/// centred project path, which is the thing the bar is for, and with the
/// palette hint and the window controls; a third keeps all three parts in
/// words at an ordinary window size and gives way well before the path does.
pub(crate) fn chip_room(width: f32, rail: f32) -> f32 {
    ((width - rail) / 3.).max(0.)
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
    // Last, as its section is: work in progress is listed for finding, not
    // for sorting out.
    if counts.running > 0 {
        parts.push(format!("{} running", counts.running));
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
pub(crate) const EMPTY_ACTIVITY_RULE: &str =
    "Failed, waiting, unread and running sessions appear here.";

/// The failed section's own heading, above the waiting one.
pub(crate) fn failed_section_label(count: usize) -> String {
    if count == 1 {
        "1 run failed".to_string()
    } else {
        format!("{count} runs failed")
    }
}

/// The pill's fill and border, keyed by rung.
///
/// **Every value here is a role**, which is the rule worth keeping
/// past this control: *a hex literal in a spec is a bug report about a missing
/// role, never a value to ship* - in this codebase most of all, where a
/// hard-coded colour is one no theme can reach. The failed rung takes
/// `agent_error` through the same two blends `accent_surface` and
/// `accent_border` are - so the top rung cannot drift from the one below it.
///
/// **Only a failure tints the pill.** Waiting and running both sit on the
/// neutral pill and say which they are with their dots.
fn chip_tones(rung: ChipRung, ui: crate::theme::UiColors) -> (Hsla, Hsla) {
    match rung {
        ChipRung::Failed => (
            ui.tinted_surface(ui.agent_error),
            ui.tinted_border(ui.agent_error),
        ),
        ChipRung::Waiting | ChipRung::Neutral => (ui.subtle, ui.border_strong),
    }
}

/// A part's dot and number tones. The number of what needs the reader is in
/// the name tone; `running` is a step down, in the rail's own row tone - it
/// is there to be found, not to call.
fn part_tones(part: ChipPart, ui: crate::theme::UiColors) -> (Hsla, Hsla) {
    match part {
        ChipPart::Failed(_) => (ui.agent_error, ui.text),
        ChipPart::Waiting(_) => (ui.attention, ui.text),
        ChipPart::Running(_) => (ui.accent, ui.text_secondary),
    }
}

/// The chip itself: one pill, a dot and a number per part - **and one
/// drawing.**
///
/// It used to be two, one per window frame, under "one decision, two
/// drawings", which held while the only shared thing was *whether* the chip
/// existed. Rungs, parts and the announcement are three axes, and three axes
/// duplicated across two files are three ways for the frames to disagree. So
/// the tones, the parts and the announcement live here; the two frames keep
/// their own geometry, their own id and their own tooltip, which is what
/// genuinely differs between a 22px title bar and a 24px toolbar.
///
/// The pill against the buttons' rounded rectangle stays the only difference in
/// silhouette, which is the row's one shape rule: status versus command.
///
/// The resting border is `border_strong`, because it is the only single role
/// that keeps the chip's edge as visible on its own fill as a button's is on
/// its - the chip sits on `subtle` and a button on `overlay`, so it measures
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
    use gpui::{InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, div, px};

    let (fill, border) = chip_tones(chip.rung, ui);
    let mut body = div()
        .id(id)
        .role(gpui::Role::Button)
        // The parts are dots and numbers on screen; read aloud they are words.
        .aria_label(
            ChipState {
                short: false,
                ..chip.clone()
            }
            .label(),
        )
        .flex_none()
        .h(px(height))
        .px(crate::ui_tokens::space::LG)
        .flex()
        .flex_row()
        .items_center()
        .gap(crate::ui_tokens::space::LG)
        .rounded_full()
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_size(crate::ui_tokens::text::CAPTION)
        .cursor_pointer();
    // With nothing to sum up, the surface's name and a neutral dot - the dot
    // so an announcement still has somewhere to play.
    let parts: Vec<(Hsla, Hsla, String)> = if chip.parts.is_empty() {
        vec![(ui.dim, ui.text_tertiary, EMPTY_CHIP_LABEL.to_string())]
    } else {
        chip.parts
            .iter()
            .map(|part| {
                let (dot, text) = part_tones(*part, ui);
                (dot, text, part.text(chip.short))
            })
            .collect()
    };
    for (index, (dot, text, label)) in parts.into_iter().enumerate() {
        // Only the first dot carries the announcement: the edge is one event
        // whatever the number of parts. The halo is the attention hue in
        // every rung - the dot is the state and carries the rank, the halo is
        // the event and carries none.
        let dot = if index == 0 {
            crate::app::announce::announceable_dot(
                gpui::SharedString::from(id),
                dot_size,
                crate::app::announce::DotFill::Solid(dot),
                ui.attention,
                announcing,
            )
        } else {
            gpui::IntoElement::into_any_element(
                div().flex_none().size(px(dot_size)).rounded_full().bg(dot),
            )
        };
        body = body.child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(crate::ui_tokens::space::SM)
                .child(dot)
                .child(div().text_color(text).child(label)),
        );
    }
    body
}

/// What the chip's tooltip promises, keyed on the part that leads.
///
/// Worded in the popover's own section headings rather than in new
/// vocabulary, and never a promise to travel: the chip opens Activity.
pub(crate) fn chip_tooltip(chip: &ChipState) -> &'static str {
    match chip.parts.first() {
        Some(ChipPart::Failed(_)) => "See which runs failed",
        Some(ChipPart::Waiting(_)) => "See which sessions are waiting for you",
        Some(ChipPart::Running(_)) => "See which sessions are running",
        None => "Open Activity",
    }
}

/// Whether this rung's chip has a hover treatment of its own.
///
/// The failed rung does not: in the design its hover border **is** its
/// resting one. Waiting does not either, for the reason it never did - the
/// rung is a signal, not a resting control. The neutral rung takes the shared
/// `border_hover` and deliberately not a softer one: hover answers "is this
/// clickable?", which the chip is.
pub(crate) fn chip_hovers(rung: ChipRung) -> bool {
    rung == ChipRung::Neutral
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
        ActivityCounts, ChipPart, ChipRung, activity_summary, chip_state, cycle_hint,
        failed_section_label, fit_chip_parts, waiting_section_label,
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
            running: 0,
            agents_open: true,
        }
    }

    fn with_running(mut counts: ActivityCounts, running: usize) -> ActivityCounts {
        counts.running = running;
        counts
    }

    /// Wide enough for every part in words.
    const WIDE: f32 = 10_000.;

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

    /// No agent session open, no chip - whatever else is counted.
    #[test]
    fn the_chip_is_there_exactly_while_an_agent_session_is_open() {
        let mut none = counts(0, 0, 0, 0);
        none.agents_open = false;
        assert_eq!(chip_state(none, WIDE), None);

        // Open, and nothing failed, waiting or running: the surface's name on
        // a neutral pill.
        let idle = chip_state(counts(0, 0, 0, 0), WIDE).expect("agents are open");
        assert!(idle.parts.is_empty());
        assert_eq!(idle.label(), "Activity");
        assert_eq!(idle.rung, ChipRung::Neutral);
    }

    /// At most three parts, in the order `failed \u{b7} waiting \u{b7} running`,
    /// and `finished` never one of them.
    #[test]
    fn the_summary_is_failed_then_waiting_then_running() {
        let all = chip_state(with_running(counts(1, 2, 5, 5), 3), WIDE).expect("open");
        assert_eq!(
            all.parts,
            [
                ChipPart::Failed(1),
                ChipPart::Waiting(2),
                ChipPart::Running(3)
            ]
        );
        assert_eq!(all.label(), "1 failed \u{b7} 2 waiting \u{b7} 3 running");
        assert_eq!(all.rung, ChipRung::Failed);

        let waiting = chip_state(with_running(counts(0, 2, 0, 0), 3), WIDE).expect("open");
        assert_eq!(waiting.label(), "2 waiting \u{b7} 3 running");
        assert_eq!(waiting.rung, ChipRung::Waiting);

        let running = chip_state(with_running(counts(0, 0, 4, 4), 3), WIDE).expect("open");
        assert_eq!(running.label(), "3 running");
        // Neutral: running alone is not a claim on anybody.
        assert_eq!(running.rung, ChipRung::Neutral);
    }

    /// Words give way to numbers first, then `running` goes; `failed` and
    /// `waiting` never do.
    #[test]
    fn a_short_chip_drops_words_then_running() {
        let parts = [
            ChipPart::Failed(1),
            ChipPart::Waiting(2),
            ChipPart::Running(3),
        ];
        // `1 failed` 8, `2 waiting` 9, `3 running` 9 characters; one pixel a
        // character and ten for each part's dot and gaps.
        assert_eq!(
            fit_chip_parts(&parts, 56., 1., 10.),
            (parts.to_vec(), false)
        );
        assert_eq!(fit_chip_parts(&parts, 55., 1., 10.), (parts.to_vec(), true));
        assert_eq!(fit_chip_parts(&parts, 33., 1., 10.), (parts.to_vec(), true));
        assert_eq!(
            fit_chip_parts(&parts, 32., 1., 10.),
            (vec![ChipPart::Failed(1), ChipPart::Waiting(2)], true)
        );
        // However short: the two that need the reader stay.
        assert_eq!(
            fit_chip_parts(&parts, 0., 1., 10.),
            (vec![ChipPart::Failed(1), ChipPart::Waiting(2)], true)
        );
        let short = chip_state(with_running(counts(1, 2, 0, 0), 3), 0.).expect("open");
        assert_eq!(short.label(), "1 \u{b7} 2");
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
        assert_eq!(
            activity_summary(with_running(counts(0, 2, 0, 0), 4)).as_deref(),
            Some("2 waiting \u{b7} 4 running")
        );
        assert_eq!(waiting_section_label(1), "1 waiting for you");
        assert_eq!(failed_section_label(1), "1 run failed");
        assert_eq!(failed_section_label(2), "2 runs failed");
    }
}
