//! The rail's `Active only` filter.
//!
//! The Activity list answers "where do I go now"; this answers "how do I work
//! in the rail all day" once the projects no longer fit in it. With the filter
//! on, the rail keeps the sessions something is happening in, the projects
//! that hold them, and the project in focus. The reasons behind each rule are
//! in `docs/internals/agent-state-and-the-rail.md`, under the filter.

use std::collections::HashSet;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, Role,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*,
};

use crate::SplitlaneApp;
use crate::project::{Thread, ThreadStatus};
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// What the filter looks at for one session row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowFacts {
    pub(crate) status: ThreadStatus,
    /// A run ended out of sight and nobody has looked.
    pub(crate) unread: bool,
    /// Background commands the agent left alive beside an idle turn.
    pub(crate) shells_beside_idle: u32,
    /// A shell in which somebody started an agent by hand.
    pub(crate) shell_runs_agent: bool,
    /// The session holds the keyboard.
    pub(crate) focused: bool,
}

impl RowFacts {
    pub(crate) fn of(thread: &Thread, focused: bool) -> Self {
        Self {
            status: thread.status,
            unread: thread.finished_unseen.is_some(),
            shells_beside_idle: thread.rail.shells_beside_idle(thread.status),
            shell_runs_agent: thread.shell_runs_agent,
            focused,
        }
    }
}

/// Whether the filter keeps a session row. Pure - unit-tested.
///
/// Kept: running (and starting), waiting, failed, finished and unread, and an
/// idle session still holding a shell - a background command it left alive,
/// or an agent somebody started in a shell by hand.
///
/// **And the session in focus, whatever it is doing.** A session that has
/// just gone idle leaves the rail only once focus has left it: otherwise its
/// row would vanish from under the pointer of the person who is reading it.
pub(crate) fn keeps(row: RowFacts) -> bool {
    // Exhaustive on purpose: a status word added later has to be placed on
    // one side of the filter here, or the build stops - it can never be
    // hidden by default.
    let at_work = match row.status {
        ThreadStatus::Starting
        | ThreadStatus::Thinking
        | ThreadStatus::WaitingForInput
        | ThreadStatus::Failed => true,
        ThreadStatus::Idle => false,
    };
    at_work || row.unread || row.shells_beside_idle > 0 || row.shell_runs_agent || row.focused
}

/// What the rail draws with the filter on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActiveOnlyView {
    /// Session rows kept, by surface record id.
    pub(crate) threads: HashSet<u64>,
    /// Projects kept, by index.
    pub(crate) projects: HashSet<usize>,
    /// Session rows hidden.
    pub(crate) hidden_sessions: usize,
    /// Projects something was hidden from: a session, or the project itself.
    pub(crate) hidden_projects: usize,
}

/// Decide the filter for every project at once. Pure - unit-tested.
///
/// A project stays when a session in it stays, and the project in focus stays
/// even when nothing is happening in it - it is where the person is working.
pub(crate) fn active_only_view(
    projects: &[(usize, Vec<(u64, RowFacts)>)],
    focused_project: Option<usize>,
) -> ActiveOnlyView {
    let mut view = ActiveOnlyView::default();
    for (ws_idx, rows) in projects {
        let mut hidden_here = 0;
        for (thread_id, facts) in rows {
            if keeps(*facts) {
                view.threads.insert(*thread_id);
            } else {
                hidden_here += 1;
            }
        }
        let kept = rows.len() > hidden_here || focused_project == Some(*ws_idx);
        if kept {
            view.projects.insert(*ws_idx);
        }
        view.hidden_sessions += hidden_here;
        if hidden_here > 0 || !kept {
            view.hidden_projects += 1;
        }
    }
    view
}

/// The line at the foot of the filtered rail, before its `Show all`.
/// Pure - unit-tested.
///
/// It is there so a short rail does not read as a broken one; with nothing
/// hidden there is nothing to explain, and no line.
pub(crate) fn hidden_line(sessions: usize, projects: usize) -> Option<String> {
    let projects_word = |count: usize| {
        if count == 1 {
            "1 project".to_string()
        } else {
            format!("{count} projects")
        }
    };
    match (sessions, projects) {
        (0, 0) => None,
        (0, projects) => Some(format!("{} hidden", projects_word(projects))),
        (sessions, projects) => Some(format!("{sessions} hidden in {}", projects_word(projects))),
    }
}

impl SplitlaneApp {
    pub(crate) fn handle_toggle_active_only(
        &mut self,
        _: &crate::ToggleActiveOnly,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_rail_active_only(!self.rail_active_only, cx);
    }

    /// Turn the filter on or off, and remember it for the next launch.
    pub(crate) fn set_rail_active_only(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.rail_active_only == on {
            return;
        }
        self.rail_active_only = on;
        self.save_session(cx);
        cx.notify();
    }

    /// The filter's decision for this frame, or `None` while it is off.
    ///
    /// `focus` is the surface holding the keyboard, as the rail already reads
    /// it for its own rows.
    pub(super) fn active_only_view(
        &self,
        focus: Option<crate::app::workspace_ops::FocusedSurface>,
    ) -> Option<ActiveOnlyView> {
        if !self.rail_active_only {
            return None;
        }
        let focused_thread = match focus {
            Some(crate::app::workspace_ops::FocusedSurface::Agent { thread_id, .. }) => {
                Some(thread_id)
            }
            _ => None,
        };
        let projects: Vec<(usize, Vec<(u64, RowFacts)>)> = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(ws_idx, ws)| {
                let rows = ws
                    .threads
                    .iter()
                    .map(|thread| {
                        (
                            thread.id,
                            RowFacts::of(thread, focused_thread == Some(thread.id)),
                        )
                    })
                    .collect();
                (ws_idx, rows)
            })
            .collect();
        let focused_project = (!self.workspaces.is_empty()).then_some(self.active_idx);
        Some(active_only_view(&projects, focused_project))
    }

    /// The filter's switch, in the `PROJECTS` row beside its `+`.
    pub(super) fn render_active_only_toggle(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = self.rail_active_only;
        let chord = self.shortcut_for_action("toggle_active_only");
        let tooltip = match chord {
            // The chord is scoped away from terminals and text fields, and the
            // tooltip says so: otherwise a press in a pane looks like a bug.
            Some(chord) => format!("Show only the sessions at work ({chord} outside a terminal)"),
            None => "Show only the sessions at work".to_string(),
        };
        // On: the design's tag - a filled step and the accent dot, so the
        // state reads at a glance. Off: plain text a step down, which is what
        // the rest of this row is.
        let resting_fill = if on {
            ui.subtle
        } else {
            ui.subtle.opacity(0.0)
        };
        let resting_text = if on { ui.text } else { ui.dim };
        div()
            .id("rail-active-only")
            .role(Role::Button)
            .aria_label(if on {
                "Show all sessions"
            } else {
                "Show only active sessions"
            })
            .flex_none()
            .h(tok::space::SECTION)
            .px(tok::space::SM)
            .mr(tok::space::XS)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .rounded(tok::radius::TAG)
            .bg(resting_fill)
            .text_size(tok::mono::PATH)
            .text_color(resting_text)
            .when(on, |toggle| {
                toggle.child(
                    div()
                        .flex_none()
                        .size(tok::space::XS)
                        .rounded_full()
                        .bg(ui.accent),
                )
            })
            .child("Active only")
            .cursor_pointer()
            .animated_hover(move |style, delta| {
                style
                    .bg(lerp_color(resting_fill, ui.subtle, delta))
                    .text_color(lerp_color(resting_text, ui.text, delta));
            })
            .tooltip(crate::ui_primitives::text_tooltip(tooltip))
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.set_rail_active_only(!this.rail_active_only, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    }

    /// `14 hidden in 6 projects · Show all`, under the filtered list.
    pub(super) fn render_hidden_line(
        &self,
        view: &ActiveOnlyView,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let line = hidden_line(view.hidden_sessions, view.hidden_projects)?;
        let resting = ui.text_secondary;
        Some(
            div()
                .flex_none()
                .mx(tok::space::MD)
                .px(tok::space::LG)
                .py(tok::space::MD)
                .border_t_1()
                .border_color(ui.divider)
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::XS)
                .text_size(tok::mono::PATH)
                .text_color(ui.faint)
                .child(div().min_w_0().truncate().child(SharedString::from(line)))
                .child(div().flex_none().child("\u{b7}"))
                .child(
                    div()
                        .id("rail-show-all")
                        .role(Role::Button)
                        .flex_none()
                        .underline()
                        .text_color(resting)
                        .cursor_pointer()
                        .animated_hover(move |style, delta| {
                            style.text_color(lerp_color(resting, ui.text, delta));
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.set_rail_active_only(false, cx);
                            cx.stop_propagation();
                        }))
                        .child("Show all"),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(status: ThreadStatus) -> RowFacts {
        RowFacts {
            status,
            unread: false,
            shells_beside_idle: 0,
            shell_runs_agent: false,
            focused: false,
        }
    }

    #[test]
    fn the_filter_keeps_what_is_happening_and_hides_what_is_not() {
        for status in [
            ThreadStatus::Starting,
            ThreadStatus::Thinking,
            ThreadStatus::WaitingForInput,
            ThreadStatus::Failed,
        ] {
            assert!(keeps(facts(status)), "{status:?} is kept");
        }
        assert!(!keeps(facts(ThreadStatus::Idle)), "idle is hidden");
        assert!(keeps(RowFacts {
            unread: true,
            ..facts(ThreadStatus::Idle)
        }));
        assert!(keeps(RowFacts {
            shells_beside_idle: 1,
            ..facts(ThreadStatus::Idle)
        }));
        assert!(keeps(RowFacts {
            shell_runs_agent: true,
            ..facts(ThreadStatus::Idle)
        }));
    }

    /// The row a person is reading does not vanish when its agent goes idle;
    /// it goes once focus has left it.
    #[test]
    fn a_session_that_went_idle_leaves_only_after_focus_does() {
        let idle_in_focus = RowFacts {
            focused: true,
            ..facts(ThreadStatus::Idle)
        };
        assert!(keeps(idle_in_focus));
        let focus_left = RowFacts {
            focused: false,
            ..idle_in_focus
        };
        assert!(!keeps(focus_left));
    }

    #[test]
    fn projects_stay_with_their_kept_sessions_and_the_one_in_focus_always_does() {
        let projects = vec![
            // A project at work: one session kept, one hidden.
            (
                0,
                vec![
                    (10, facts(ThreadStatus::Thinking)),
                    (11, facts(ThreadStatus::Idle)),
                ],
            ),
            // Nothing happening: hidden whole.
            (
                1,
                vec![
                    (20, facts(ThreadStatus::Idle)),
                    (21, facts(ThreadStatus::Idle)),
                ],
            ),
            // Nothing happening, but in focus.
            (2, vec![(30, facts(ThreadStatus::Idle))]),
            // Empty and not in focus.
            (3, vec![]),
        ];
        let view = active_only_view(&projects, Some(2));
        assert_eq!(view.threads, HashSet::from([10]));
        assert_eq!(view.projects, HashSet::from([0, 2]));
        assert_eq!(view.hidden_sessions, 4);
        assert_eq!(view.hidden_projects, 4);
        assert_eq!(
            hidden_line(view.hidden_sessions, view.hidden_projects).as_deref(),
            Some("4 hidden in 4 projects")
        );
    }

    #[test]
    fn the_hidden_line_counts_and_says_nothing_when_nothing_is_hidden() {
        assert_eq!(hidden_line(0, 0), None);
        assert_eq!(
            hidden_line(14, 6).as_deref(),
            Some("14 hidden in 6 projects")
        );
        assert_eq!(hidden_line(1, 1).as_deref(), Some("1 hidden in 1 project"));
        assert_eq!(hidden_line(0, 2).as_deref(), Some("2 projects hidden"));
    }
}
