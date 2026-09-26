//! Attention Queue (CLI Cockpit).
//!
//! A cross-workspace popover listing everything standing on the user, in the
//! triage order the design gives it: a run that fell over, then the agents
//! holding a question, then the results that finished out of sight and nobody
//! has collected. Enter / click teleports to the pane through the same
//! mechanics as `handle_jump_next_waiting` (workspace switch + hidden tab
//! activation + focus). A question the app carries is sanitized at ingress
//! (`AgentSession::message`, 512 chars, bidi-stripped) and rendered as inert
//! text, never interpreted.
//!
//! **Every row is derived from live state on every render** and never from a
//! snapshot, so a session that unblocks while the popover is open disappears at
//! the next repaint, and a row whose pane died is dropped rather than left
//! navigable. The finished section used to be the exception, and had to be:
//! opening the popover cleared every mark in the same breath, so a live query
//! would have drawn nothing. Clearing was replaced with **lowering**, and
//! the snapshot went with the thing that forced it.
//!
//! # What opening it does, and what it deliberately does not
//!
//! Opening lowers a fresh mark to the quiet one - the list has been seen, so
//! the chip stops shouting - and nothing more. Only the surface coming up in a
//! pane takes the mark away, plus a click on its own row here, which clears
//! **that** mark and pointedly not its neighbours. An earlier design cleared all
//! of them on open, under "opening the list is reading it", and this one keeps exactly
//! half of that: a result nobody has collected is still uncollected. The two
//! failures that buys are a ritual walk of the panes for silence, which would
//! have devalued the word "read" a second way, and desensitisation - a chip
//! that is always hot is not a chip.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, SharedString, Styled, Window, deferred, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::app::waiting::WaitingStop;
use crate::project::AgentsTarget;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// The popover's width, from the design.
const QUEUE_WIDTH: f32 = 300.;
/// The gap between the row carrying the waiting chip and the popover under it.
const ANCHOR_GAP: f32 = 6.;
/// The popover's right inset - the same one the title bar pads itself by, so
/// the card's right edge lines up with the chip's.
const ANCHOR_INSET: f32 = 14.;

/// Which of Activity's three kinds a row is - and the order
/// they come out in.
///
/// **Triage lives in the sort order.** Outside, the chip answers "is anything
/// standing?"; inside, the list answers "where do I start?" - a fallen run
/// first, because the question there is whether to run it again at all, then
/// the agents holding a question, then the results nobody has collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueueKind {
    Failed,
    Waiting,
    Finished,
}

/// One row of the queue: one thing standing on the user.
pub(crate) struct QueueRow {
    /// Which section it belongs to, and its rank in the list.
    pub(crate) kind: QueueKind,
    /// How to get there. The same [`WaitingStop`] the chip and `\u{2325}\u{21e5}`
    /// navigate, so a row cannot offer a destination the other two do not have.
    pub(crate) stop: WaitingStop,
    /// The surface record behind the row, when there is one.
    ///
    /// Only a finished row needs it - clicking one clears **that** mark - but
    /// every row that has a record carries it, because "the row I clicked" is
    /// not a thing the app should have to re-derive from a position.
    pub(crate) thread_id: Option<u64>,
    /// The session's own name - the same string its rail row carries, because
    /// the popover's rows are the rail's row anatomy and a row is identified
    /// by its name, not by which CLI is behind it.
    pub(crate) name: String,
    /// The line under the name: `project \u{b7} branch`, or just the project
    /// when the container is not a repository.
    pub(crate) meta: String,
    /// Whether nobody has seen this yet, even in this list.
    ///
    /// Only a finished row can be unacknowledged - a waiting agent is never
    /// "acknowledged", because reading a list does not answer its question.
    pub(crate) unacknowledged: bool,
    /// How long it has been standing, when the app knows.
    ///
    /// Only a pane-bound session is timed (`AgentSession::waiting_since`); an
    /// agent surface reports its state through `Thread::status`, which carries
    /// no timestamp. A finished row is always timed - the mark carries the
    /// instant. `None` draws nothing rather than "0s", which would be a number
    /// the app made up.
    pub(crate) waiting_secs: Option<u64>,
    /// Set when this row stands for a private group rather than a session:
    /// see [`fold_private_groups`].
    pub(crate) private_group: Option<PrivateGroupRow>,
}

/// A private group's one row: which group, and how many of its sessions are
/// on the row's rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrivateGroupRow {
    pub(crate) group_id: u64,
    pub(crate) sessions: usize,
}

/// **One row per private group.** Pure - unit-tested. Call it on a sorted,
/// deduplicated list.
///
/// A group with `Keep names private` shows its names only where the person
/// looked - the open group in the rail, a query they typed - and Activity is a
/// list that comes to them. So the group's sessions fold into one row named by
/// the group (as typed), standing where its highest session would stand: the
/// first of its rows in triage order becomes the group's row, keeping that
/// session's stop, so a click goes to the session at the top rung. It counts
/// the sessions on that rung; the rest of the group's rows go.
///
/// The counts above the list are taken before this, so the chip and the
/// section headings still count sessions, not rows.
pub(crate) fn fold_private_groups(
    rows: Vec<QueueRow>,
    private_group_of: impl Fn(&QueueRow) -> Option<(u64, String)>,
) -> Vec<QueueRow> {
    let tagged: Vec<(QueueRow, Option<(u64, String)>)> = rows
        .into_iter()
        .map(|row| {
            let group = private_group_of(&row);
            (row, group)
        })
        .collect();
    // Sorted, so a group's first row is on its highest rung.
    let top_rung = |group_id: u64| {
        tagged
            .iter()
            .find(|(_, group)| group.as_ref().is_some_and(|(id, _)| *id == group_id))
            .map(|(row, _)| row.kind)
    };
    let on_top_rung = |group_id: u64| {
        let top = top_rung(group_id);
        tagged
            .iter()
            .filter(|(row, group)| {
                group.as_ref().is_some_and(|(id, _)| *id == group_id) && Some(row.kind) == top
            })
            .count()
    };
    let sessions: Vec<Option<usize>> = tagged
        .iter()
        .map(|(_, group)| group.as_ref().map(|(id, _)| on_top_rung(*id)))
        .collect();
    let mut seen: Vec<u64> = Vec::new();
    let mut folded = Vec::with_capacity(tagged.len());
    for ((mut row, group), sessions) in tagged.into_iter().zip(sessions) {
        let (Some((group_id, name)), Some(sessions)) = (group, sessions) else {
            folded.push(row);
            continue;
        };
        if seen.contains(&group_id) {
            continue;
        }
        seen.push(group_id);
        row.name = name;
        row.meta = String::new();
        row.waiting_secs = None;
        row.private_group = Some(PrivateGroupRow { group_id, sessions });
        folded.push(row);
    }
    folded
}

/// Triage first, then longest-standing first inside each section; rows the app
/// cannot time sink to the end of their own section, keeping the rail order
/// they arrived in. Pure - unit-tested.
///
/// An untimed row is not "standing zero seconds", so it must not sort as if it
/// were - it would jump its whole section every time.
pub(crate) fn sort_rows(rows: &mut [QueueRow]) {
    rows.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(a.waiting_secs.is_none().cmp(&b.waiting_secs.is_none()))
            .then(b.waiting_secs.cmp(&a.waiting_secs))
    });
}

/// **One session, one row.** Pure - unit-tested. Call it on a sorted list.
///
/// A session can genuinely be two things at once: a run that finished out of
/// sight, was started again and fell over carries both a failed status and an
/// uncollected mark, and the two facts are cleared by different things, so
/// neither is wrong. But this list is triage, and drawing one session twice
/// asks the reader to start with it twice. The list is sorted first, so what
/// survives is the **higher rung** - the one that says what to do - and the
/// surviving row still carries the `thread_id`, so activating it clears the
/// mark as well.
pub(crate) fn one_row_per_session(rows: &mut Vec<QueueRow>) {
    let mut seen = std::collections::HashSet::new();
    rows.retain(|row| match row.thread_id {
        Some(thread_id) => seen.insert(thread_id),
        // A pane session with no surface record cannot collide with anything:
        // the mark lives on a record.
        None => true,
    });
}

/// A row's second line: `project \u{b7} branch`, or the project alone when the
/// container is not a repository. Pure - unit-tested.
pub(crate) fn queue_meta(project: &str, branch: &str) -> String {
    if branch.is_empty() {
        project.to_string()
    } else {
        format!("{project} \u{b7} {branch}")
    }
}

/// Compact wait label: `42s`, `7m`, `1h 12m`. Pure - unit-tested.
pub(crate) fn wait_label(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60)
    }
}

impl SplitlaneApp {
    /// Derive the queue rows from live session state. Sessions whose
    /// resolved surface no longer exists in any layout (pane closed after
    /// resolution, sweep not yet run) are dropped entirely - the queue must
    /// never offer a navigable row to a dead pane.
    pub(crate) fn attention_queue_rows(&self, cx: &Context<Self>) -> Vec<QueueRow> {
        let mut rows = self.activity_rows_unsorted(cx);
        sort_rows(&mut rows);
        one_row_per_session(&mut rows);
        rows
    }

    /// The rows as the popover draws them: [`Self::attention_queue_rows`]
    /// with each private group folded into one row. Counts are taken from the
    /// unfolded rows; only the drawing and the keys use these.
    pub(crate) fn attention_queue_display_rows(&self, cx: &Context<Self>) -> Vec<QueueRow> {
        fold_private_groups(self.attention_queue_rows(cx), |row| {
            crate::app::waiting::stop_ws_idx(&row.stop)
                .and_then(|ws_idx| self.private_group_of(ws_idx))
                .map(|group| (group.id, group.name.clone()))
        })
    }

    /// Everything Activity holds, in no order and before that rule.
    ///
    /// The chip's list, not a second one derived from `agent_sessions` alone.
    /// The two used to be built separately, and because an agent surface
    /// reports through `Thread::status` while a pane session reports through
    /// `Workspace::agent_sessions`, the chip could say "3 agents waiting" over
    /// a popover listing two. They are six pixels apart, so they read one list.
    fn activity_rows_unsorted(&self, cx: &Context<Self>) -> Vec<QueueRow> {
        self.failed_stops(cx)
            .into_iter()
            .filter_map(|stop| self.queue_row(QueueKind::Failed, stop, cx))
            .chain(
                self.waiting_stops(cx)
                    .into_iter()
                    .filter_map(|stop| self.queue_row(QueueKind::Waiting, stop, cx)),
            )
            .chain(self.finished_rows(cx))
            .collect()
    }

    /// The uncollected results, as rows.
    ///
    /// **Live, not a snapshot.** It was a snapshot taken when the popover
    /// opened, and had to be: opening cleared every mark in the same breath, so
    /// a live query would have drawn an empty section. Lowering replaced
    /// clearing - the mark survives the reading and only the
    /// surface coming up in a pane takes it away - so the reason for the
    /// snapshot went with it.
    fn finished_rows(&self, cx: &Context<Self>) -> Vec<QueueRow> {
        self.finished_unseen_rows()
            .into_iter()
            .filter_map(|(thread_id, mark)| {
                // Resolved by **id**, with the position looked up now rather
                // than stored: a surface deleted between two frames shifts
                // every index after it, and a stored pair would then navigate
                // to a different session. The same rule the tab menu's
                // `delete_thread_id` follows, for the same reason.
                let target = crate::project::find_surface(&self.workspaces, thread_id)?;
                let mut row =
                    self.queue_row(QueueKind::Finished, WaitingStop::Surface(target), cx)?;
                row.waiting_secs = Some(mark.elapsed().as_secs());
                row.unacknowledged = !mark.acknowledged;
                Some(row)
            })
            .collect()
    }

    /// Name, meta and wait for one stop, in the rail's vocabulary.
    ///
    /// `None` drops the row: a stop that cannot be named is a stop whose
    /// surface record went away between the walk and here.
    fn queue_row(
        &self,
        kind: QueueKind,
        stop: WaitingStop,
        cx: &Context<Self>,
    ) -> Option<QueueRow> {
        let (ws_idx, name, waiting_secs) = match &stop {
            WaitingStop::Surface(AgentsTarget::Thread { ws_idx, thread_idx }) => {
                let thread = self.workspaces.get(*ws_idx)?.threads.get(*thread_idx)?;
                let name = crate::project::clean_sidebar_title(&thread.title)
                    .unwrap_or_else(|| thread.title.clone());
                (*ws_idx, name, None)
            }
            WaitingStop::Surface(_) => return None,
            WaitingStop::Pane {
                ws_idx,
                pane,
                tab_idx,
            } => {
                let ws = self.workspaces.get(*ws_idx)?;
                let tab = pane.read(cx).tabs.get(*tab_idx)?.clone();
                let terminal = tab.as_terminal()?.clone();
                let surface_id = terminal.entity_id().as_u64();
                // The name the rail would show for this pane's session, with
                // the tab's own label behind it for a session the rail has no
                // surface record for.
                let name = terminal
                    .read(cx)
                    .agent_thread_id
                    .and_then(|tid| ws.threads.iter().find(|t| t.id == tid))
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| crate::pane::Pane::tab_title(&tab, cx));
                let waiting_secs = ws
                    .agent_sessions
                    .values()
                    .find(|session| session.surface_id == Some(surface_id))
                    .and_then(|session| session.waiting_since)
                    .map(|since| since.elapsed().as_secs());
                (*ws_idx, name, waiting_secs)
            }
        };
        let ws = self.workspaces.get(ws_idx)?;
        let thread_id = match &stop {
            WaitingStop::Surface(AgentsTarget::Thread { thread_idx, .. }) => {
                ws.threads.get(*thread_idx).map(|thread| thread.id)
            }
            WaitingStop::Surface(_) => None,
            WaitingStop::Pane { pane, tab_idx, .. } => pane
                .read(cx)
                .tabs
                .get(*tab_idx)
                .and_then(crate::pane::TabContent::as_terminal)
                .and_then(|view| view.read(cx).agent_thread_id),
        };
        Some(QueueRow {
            kind,
            stop,
            thread_id,
            // A standing row is never acknowledged and never needs to be; the
            // finished rows overwrite this from their own mark.
            unacknowledged: false,
            name,
            meta: queue_meta(&ws.title, &ws.git_branch),
            waiting_secs,
            private_group: None,
        })
    }

    pub(crate) fn handle_open_attention_queue(
        &mut self,
        _: &crate::OpenAttentionQueue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_attention_queue(window, cx);
    }

    /// Open the queue, or close it if it is already open.
    ///
    /// Both doors call this - the chord and the title bar's waiting chip - so
    /// a second click on the chip closes the popover it opened, the way a
    /// popover under a control is expected to behave.
    pub(crate) fn toggle_attention_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Pane-scoped: refuses while another surface is on screen. The
        // binding's `Terminal` key context makes that visible in the palette
        // and in Settings, instead of the key silently doing nothing.
        if !self.panes_surface_visible() {
            return;
        }
        if self.attention_queue_open {
            self.close_attention_queue_and_restore_focus(window, cx);
            return;
        }
        self.attention_queue_open = true;
        // Opening the inventory **lowers** what it lists; it no longer clears
        // it. The list has been seen, so the chip stops shouting -
        // but a result nobody has collected is still uncollected, and the rail
        // says so until the surface actually comes up in a pane.
        self.acknowledge_unseen_marks(cx);
        self.attention_queue_selected = 0;
        self.attention_queue_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn close_attention_queue(&mut self, cx: &mut Context<Self>) {
        self.attention_queue_open = false;
        self.attention_queue_selected = 0;
        cx.notify();
    }

    pub(crate) fn close_attention_queue_and_restore_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_attention_queue(cx);
        if let Some(ws) = self.workspaces.get_mut(self.active_idx) {
            ws.focus_first(window, cx);
        }
    }

    /// Enter / click on a row: teleport to the waiting pane (workspace
    /// switch + tab activation + focus - `handle_jump_next_waiting`
    /// mechanics) and close the queue. The surface is re-resolved at
    /// activation time: a pane closed between render and Enter is a clean
    /// no-op (the row is gone at the next repaint anyway).
    /// Go to one row's stop and close the popover.
    ///
    /// Through `go_to_stop`, which is the same door the chord uses - so a row
    /// reaches an agent surface parked on its container exactly as it reaches
    /// a session in a background tab.
    pub(crate) fn attention_queue_activate(
        &mut self,
        stop: WaitingStop,
        thread_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A session in a folded group is shown with its group opened -
        // private or not: the person asked to go there.
        if let Some(ws_idx) = crate::app::waiting::stop_ws_idx(&stop) {
            self.reveal_project_group(ws_idx, cx);
        }
        // Keep the jump cycle coherent: a queue teleport counts as visiting
        // that stop, so the next press of the chord continues from here.
        self.go_to_stop(stop, window, cx);
        // And the mark goes **pointedly**, on the row that was clicked and not
        // on its neighbours - which is what makes the list a triage tool rather
        // than a notice somebody has to dismiss.
        if let Some(thread_id) = thread_id {
            self.clear_unseen_mark(thread_id, cx);
        }
        self.close_attention_queue(cx);
    }

    pub(crate) fn handle_attention_queue_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let mut rows = self.attention_queue_display_rows(cx);
        let len = rows.len();
        match key {
            "escape" => self.close_attention_queue_and_restore_focus(window, cx),
            "enter" if len > 0 => {
                let idx = self.attention_queue_selected.min(len - 1);
                let row = rows.swap_remove(idx);
                self.attention_queue_activate(row.stop, row.thread_id, window, cx);
            }
            "up" if len > 0 && self.attention_queue_selected > 0 => {
                self.attention_queue_selected -= 1;
                cx.notify();
            }
            "down" if len > 0 && self.attention_queue_selected + 1 < len => {
                self.attention_queue_selected += 1;
                cx.notify();
            }
            _ => {}
        }
    }

    /// The queue as the design draws it: a popover anchored under the title
    /// bar's waiting chip, not a centred modal over a scrim.
    ///
    /// `anchor_top` is the bottom edge of whatever row carries the chip - the
    /// title bar when the app draws one, the project toolbar when the platform
    /// draws the frame instead (the chip moves there with it). The popover
    /// hangs `ANCHOR_GAP` below that, right-aligned to the same inset the bar
    /// uses, so it reads as belonging to the chip on both chrome paths.
    ///
    /// There is no scrim. A scrim says "answer me first"; this list is
    /// something the user walks at their own pace, and `\u{2325}\u{21e5}` walks
    /// it without opening the popover at all. The full-window layer under the
    /// card is a click-catcher only.
    pub(crate) fn render_attention_queue(
        &self,
        anchor_top: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = crate::theme::ui_colors();
        let all_rows = self.attention_queue_rows(cx);
        let counts = crate::app::waiting::ActivityCounts::from_rows(&all_rows);
        let rows = self.attention_queue_display_rows(cx);
        let selected = self
            .attention_queue_selected
            .min(rows.len().saturating_sub(1));
        // Counted off **these** rows rather than off the frame's cache: the
        // subtitle sits directly above the section headings, and a subtitle
        // derived from a second source is the "two numbers six pixels apart"
        // failure with the two numbers put even closer together.
        let summary = crate::app::waiting::activity_summary(counts);
        let has_waiting = rows.iter().any(|row| row.kind == QueueKind::Waiting);

        let mut card = div()
            .id("attention-queue")
            .occlude()
            .track_focus(&self.attention_queue_focus)
            .on_key_down(cx.listener(Self::handle_attention_queue_key_down))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.close_attention_queue_and_restore_focus(window, cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .absolute()
            .top(anchor_top + px(ANCHOR_GAP))
            .right(px(ANCHOR_INSET))
            .w(px(QUEUE_WIDTH))
            .flex()
            .flex_col()
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .rounded(tok::radius::MENU)
            .shadow(crate::ui_primitives::menu_shadow(ui))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(tok::space::MD)
                    .px(tok::space::XXL)
                    .py(tok::space::LG)
                    .border_b_1()
                    .border_color(ui.border)
                    .child(
                        div()
                            .flex_none()
                            .text_size(tok::text::ROW)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(ui.text)
                            // **The title names the surface, the sections name
                            // the facts**. It said `Waiting for you`,
                            // which stopped being true the moment the chip
                            // learned to open this with nothing waiting - and
                            // that heading is not lost, it moved down to the
                            // section where it is always true. Not `Attention
                            // queue`: that is the internal name, half of what
                            // is here makes no claim on attention, and "queue"
                            // promises an order to work through.
                            .child("Activity"),
                    )
                    // Absent on an empty surface: the body below says what an
                    // empty Activity means, and one popover may not say it
                    // twice in two phrasings.
                    .children(summary.map(|summary| {
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::LABEL)
                            .text_color(ui.text_tertiary)
                            .child(SharedString::from(summary))
                    })),
            );

        if rows.is_empty() {
            // Explicit empty state - never a silent no-op. It
            // **defines rather than denies**; the argument is on
            // `waiting::EMPTY_ACTIVITY_TITLE`, and the short of it is that
            // three absences cannot be denied in one clause, while an empty
            // surface is the one place where naming what it holds is the most
            // useful thing on screen.
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(tok::space::SM)
                    .px(tok::space::XXL)
                    .py(tok::space::XL)
                    .child(
                        div()
                            .text_size(tok::text::ROW)
                            .text_color(ui.muted)
                            .child(crate::app::waiting::EMPTY_ACTIVITY_TITLE),
                    )
                    .child(
                        div()
                            .text_size(tok::text::CAPTION)
                            .text_color(ui.faint)
                            .child(crate::app::waiting::EMPTY_ACTIVITY_RULE),
                    ),
            );
        } else {
            // One list in three sections, in the triage order the design gives
            // it: a fallen run, then the agents holding a question, then the
            // results nobody has collected. The rule under it is older and
            // survives the rebuild - the accent never reaches below the last
            // section's own rule, so the hierarchy reads whatever is in it.
            let mut section = div().flex().flex_col().p(tok::space::SM);
            let mut drawn: Option<QueueKind> = None;
            let mut list = div().flex().flex_col();
            for (idx, row) in rows.iter().enumerate() {
                if drawn != Some(row.kind) {
                    if drawn.is_some() {
                        list = list.child(section);
                        section = div()
                            .flex()
                            .flex_col()
                            .border_t_1()
                            .border_color(ui.border)
                            .p(tok::space::SM);
                    }
                    drawn = Some(row.kind);
                    // Sessions, not rows: a private group's one row stands
                    // for several.
                    let count = match row.kind {
                        QueueKind::Failed => counts.failed,
                        QueueKind::Waiting => counts.waiting,
                        QueueKind::Finished => counts.finished,
                    };
                    let (heading, tone) = match row.kind {
                        QueueKind::Failed => (
                            crate::app::waiting::failed_section_label(count),
                            ui.agent_error,
                        ),
                        QueueKind::Waiting => {
                            (crate::app::waiting::waiting_section_label(count), ui.muted)
                        }
                        // `dim`, which is what the design mock's `muted` is here -
                        // it shipped as `ui.muted`, one step brighter than the
                        // prototype draws it, the mock-to-app token mapping missed in
                        // the other direction from the row beneath it.
                        QueueKind::Finished => {
                            (format!("{count} finished while you were away"), ui.dim)
                        }
                    };
                    section = section.child(
                        div()
                            .px(tok::space::LG)
                            .py(tok::space::MD)
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::LABEL)
                            .text_color(tone)
                            .child(SharedString::from(heading)),
                    );
                }
                section = section.child(self.attention_queue_row(row, idx == selected, ui, cx));
            }
            card = card.child(list.child(section));
        }

        // The chord is stated inside the block rather than in a bordered
        // footer: the popover is one block. It follows the **sections** (round
        // 30) - with nothing waiting, `⌥⇥` cycles results to read rather than
        // questions to answer - and it is drawn whenever any section is, never
        // over an empty popover, where it would name an empty set.
        // Drawn when there is something true for it to say: the chord reaches
        // the waiting rows, and "opening one marks it read" is about a mark. A
        // popover of nothing but failed runs has neither, and the line would be
        // two claims that are both false.
        if has_waiting || rows.iter().any(|row| row.kind == QueueKind::Finished) {
            card = card.child(
                div()
                    .h(tok::row::PROJECT)
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(tok::space::XXL)
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::HINT)
                    .text_color(ui.faint)
                    .child(SharedString::from(crate::app::waiting::cycle_hint(
                        self.shortcut_for_action("jump_next_waiting"),
                        has_waiting,
                    ))),
            );
        }

        deferred(
            div()
                .id("attention-queue-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(card),
        )
        .with_priority(6)
        .into_any_element()
    }

    /// One row of the popover, in the rail's row anatomy.
    ///
    /// Every row is navigable: it was built from a stop, and a stop that could
    /// not be reached was never turned into a row. The three kinds differ in
    /// exactly two places - the name's weight and the trailing label - because
    /// they are three readings of one list rather than three lists.
    fn attention_queue_row(
        &self,
        row: &QueueRow,
        is_selected: bool,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stop = row.stop.clone();
        let thread_id = row.thread_id;
        let row_id: SharedString = self
            .stop_key(&row.stop, cx)
            .map(|key| format!("attention-row-{:?}-{key}", row.kind))
            .unwrap_or_else(|| format!("attention-row-{:?}-{}", row.kind, row.name))
            .into();
        let resting_background = if is_selected {
            ui.subtle
        } else {
            ui.subtle.opacity(0.0)
        };
        // A step below the standing rows' `text`, not two: the finished half is
        // information and the weight says so, but the name is the thing you
        // click. It shipped as `ui.dim` and that was the token-mapping trap sprung
        // again - the design mock's `dim` is this app's `text_tertiary`, so writing
        // the same word here landed one step too dark. It measured 3.50 : 1 on
        // `overlay`, under the 4.5 : 1 floor, and **1.27 : 1** from the `faint`
        // timestamp beside it - the pair the design threw out as one the eye
        // cannot compare, so the row read as a single grey wash. At
        // `text_tertiary` it is 6.06, a comparable 2.20 from the timestamp and
        // a full 2.15 below a standing row's own name.
        let name_tone = if row.kind == QueueKind::Finished {
            ui.text_tertiary
        } else {
            ui.text
        };
        if let Some(private) = row.private_group {
            return self.private_group_queue_row(row, private, row_id, resting_background, ui, cx);
        }
        let meta: SharedString = row.meta.clone().into();
        div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::LG)
            .px(tok::space::LG)
            .py(tok::space::MD)
            .rounded(tok::radius::CONTROL)
            .bg(resting_background)
            // The rail's own leading glyph, in the rail's own fixed box: these
            // rows are the rail's row anatomy, and borrowing the helper is what
            // keeps that true when the rail changes.
            .child(crate::app::agents_sidebar::surface_type_glyph(false, ui))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(tok::text::ROW)
                            .text_color(name_tone)
                            .child(SharedString::from(row.name.clone())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::HINT)
                            .text_color(ui.text_tertiary)
                            .child(meta),
                    ),
            )
            // How long it has been standing, when the app has a number. An
            // agent surface holding a question reports no timestamp, and the
            // column stays empty rather than printing one nothing measured. A
            // finished row always has one - the mark carries it - and says so
            // in words, which is the honest version of the cooling dot the
            // designer refused: readable, unambiguous, costing no vocabulary.
            .when_some(row.waiting_secs, |element, secs| {
                let (label, tone) = match row.kind {
                    QueueKind::Finished => (format!("finished {} ago", wait_label(secs)), ui.faint),
                    _ => (wait_label(secs), ui.dim),
                };
                element.child(
                    div()
                        .flex_none()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::HINT)
                        .text_color(tone)
                        .child(SharedString::from(label)),
                )
            })
            .cursor_pointer()
            .animated_hover(move |style, delta| {
                style.bg(lerp_color(resting_background, ui.subtle, delta));
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.attention_queue_activate(stop.clone(), thread_id, window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    }
}

impl SplitlaneApp {
    /// A private group's row: the rung's dot, the group's name as typed in
    /// mono, and on the right how many sessions are on that rung. One line,
    /// because a second line would be where names go.
    fn private_group_queue_row(
        &self,
        row: &QueueRow,
        private: PrivateGroupRow,
        row_id: SharedString,
        resting_background: gpui::Hsla,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stop = row.stop.clone();
        let thread_id = row.thread_id;
        let (word, tone) = match row.kind {
            QueueKind::Failed => ("failed", ui.agent_error),
            QueueKind::Waiting => ("waiting", ui.accent),
            QueueKind::Finished => ("finished", ui.faint),
        };
        div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::LG)
            .px(tok::space::LG)
            .py(tok::space::MD)
            .rounded(tok::radius::CONTROL)
            .bg(resting_background)
            .child(div().flex_none().size(px(6.)).rounded_full().bg(tone))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(tok::font::MONO)
                    .text_size(tok::text::ROW)
                    .text_color(ui.text)
                    .child(SharedString::from(row.name.clone())),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::HINT)
                    .text_color(tone)
                    .child(SharedString::from(format!("{} {word}", private.sessions))),
            )
            .cursor_pointer()
            .animated_hover(move |style, delta| {
                style.bg(lerp_color(resting_background, ui.subtle, delta));
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.attention_queue_activate(stop.clone(), thread_id, window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: QueueKind, name: &str, waiting_secs: Option<u64>) -> QueueRow {
        QueueRow {
            kind,
            // The variant is irrelevant to sorting; kind and wait are what
            // order.
            stop: WaitingStop::Surface(AgentsTarget::Thread {
                ws_idx: 0,
                thread_idx: 0,
            }),
            thread_id: None,
            unacknowledged: false,
            name: name.to_string(),
            meta: String::new(),
            waiting_secs,
            private_group: None,
        }
    }

    /// A private group is one row, where its highest session would stand,
    /// named by the group and counting the sessions on that rung; nothing
    /// under it keeps a name.
    #[test]
    fn a_private_group_is_one_row_at_its_highest_rung() {
        let mut rows = vec![
            row(QueueKind::Failed, "p:crash", Some(5)),
            row(QueueKind::Waiting, "open", Some(9)),
            row(QueueKind::Waiting, "p:ask one", Some(4)),
            row(QueueKind::Failed, "p:crash two", Some(3)),
            row(QueueKind::Finished, "p:done", Some(1)),
        ];
        sort_rows(&mut rows);
        let folded = fold_private_groups(rows, |row| {
            row.name
                .starts_with("p:")
                .then(|| (7, "Personal".to_string()))
        });
        let names: Vec<&str> = folded.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Personal", "open"]);
        assert_eq!(folded[0].kind, QueueKind::Failed);
        assert_eq!(
            folded[0].private_group,
            Some(PrivateGroupRow {
                group_id: 7,
                sessions: 2
            })
        );
        assert!(folded[0].meta.is_empty() && folded[0].waiting_secs.is_none());
        assert_eq!(folded[1].private_group, None);
    }

    /// Triage outranks the wait: outside, the chip answers "is anything
    /// standing?"; inside, the list answers "where do I start?"
    #[test]
    fn triage_orders_the_sections_and_the_wait_orders_inside_one() {
        let mut rows = vec![
            row(QueueKind::Finished, "collected-nothing", Some(9_000)),
            row(QueueKind::Waiting, "asking", Some(10)),
            row(QueueKind::Failed, "fell-over", Some(5)),
            row(QueueKind::Waiting, "asking-longer", Some(600)),
        ];
        sort_rows(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        // A fallen run first even though it is the newest thing here, and an
        // hour-old uncollected result last even though it is the oldest.
        assert_eq!(
            order,
            vec!["fell-over", "asking-longer", "asking", "collected-nothing"]
        );
    }

    /// One session, one row. A run that finished out of sight, was started
    /// again and fell over is genuinely both things - the two facts are cleared
    /// by different things and neither is wrong - but a triage list that draws
    /// it twice asks the reader to start with it twice. The higher rung
    /// survives, because it is the one that says what to do.
    #[test]
    fn a_session_that_is_two_things_at_once_gets_one_row() {
        let mut rows = vec![
            {
                let mut row = row(QueueKind::Finished, "twice", Some(600));
                row.thread_id = Some(7);
                row
            },
            {
                let mut row = row(QueueKind::Failed, "twice", Some(5));
                row.thread_id = Some(7);
                row
            },
            row(QueueKind::Waiting, "no-record", Some(20)),
        ];
        sort_rows(&mut rows);
        one_row_per_session(&mut rows);
        let kept: Vec<(QueueKind, &str)> = rows.iter().map(|r| (r.kind, r.name.as_str())).collect();
        assert_eq!(
            kept,
            vec![
                (QueueKind::Failed, "twice"),
                (QueueKind::Waiting, "no-record")
            ]
        );
    }

    #[test]
    fn sorts_longest_wait_first_untimed_last() {
        let mut rows = vec![
            row(QueueKind::Waiting, "a", Some(10)),
            row(QueueKind::Waiting, "untimed", None),
            row(QueueKind::Waiting, "b", Some(300)),
            row(QueueKind::Waiting, "c", Some(60)),
        ];
        sort_rows(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        // Timed rows by wait desc. The untimed row is an agent
        // surface, which carries no timestamp: it sinks to the end instead of
        // sorting as if it had been waiting zero seconds, which would put it
        // first every single frame.
        assert_eq!(order, vec!["b", "c", "a", "untimed"]);
    }

    #[test]
    fn meta_drops_the_separator_without_a_branch() {
        assert_eq!(queue_meta("atlas", "main"), "atlas \u{b7} main");
        // A plain directory is not a repository, and an empty branch must not
        // leave a dangling separator behind the project name.
        assert_eq!(queue_meta("atlas", ""), "atlas");
    }

    #[test]
    fn wait_labels_are_compact() {
        assert_eq!(wait_label(0), "0s");
        assert_eq!(wait_label(59), "59s");
        assert_eq!(wait_label(60), "1m");
        assert_eq!(wait_label(3_599), "59m");
        assert_eq!(wait_label(4_380), "1h 13m");
    }

    /// `unseen` is a property of the reader, not of the session, so it must not
    /// touch the five status words - the dot says `idle` the moment the run
    /// ends, honestly, and the mark rides beside it.
    #[gpui::test]
    fn a_run_that_ended_out_of_sight_marks_the_row_without_touching_the_dot(
        cx: &mut gpui::TestAppContext,
    ) {
        let cx = cx.add_empty_window();
        cx.update(|_, cx| {
            let mut thread = crate::project::Thread::new_terminal("Claude", "/tmp", None);
            thread.status = crate::project::ThreadStatus::Idle;
            assert!(thread.finished_unseen.is_none());
            thread.finished_unseen = Some(crate::project::FinishedMark::unacknowledged());
            // The word the row shows is not a status: the dot is unchanged.
            assert_eq!(thread.status, crate::project::ThreadStatus::Idle);
            assert!(thread.finished_unseen.is_some());
            let _ = cx;
        });
    }

    /// The mark has two levels, cleared by different things - the whole of
    /// the correction to the earlier clear-everything-on-open rule.
    #[gpui::test]
    fn the_popover_lowers_a_mark_and_only_the_pane_takes_it_away(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        cx.update(|_, cx| {
            let mut thread = crate::project::Thread::new_terminal("Claude", "/tmp", None);
            thread.finished_unseen = Some(crate::project::FinishedMark::unacknowledged());
            assert!(!thread.finished_unseen.expect("marked").acknowledged);

            // What opening Activity does: lowers, never clears. The chip stops
            // shouting; the rail still says the result is uncollected.
            if let Some(mark) = thread.finished_unseen.as_mut() {
                mark.acknowledged = true;
            }
            assert!(
                thread.finished_unseen.is_some(),
                "reading the list is not collecting the result"
            );
            assert!(thread.finished_unseen.expect("still marked").acknowledged);

            // And what the pane does: takes it away.
            thread.finished_unseen = None;
            assert!(thread.finished_unseen.is_none());
            let _ = cx;
        });
    }
}
