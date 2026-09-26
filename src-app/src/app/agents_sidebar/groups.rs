//! Project groups in the rail: what the label does, and how it is drawn.
//!
//! The model and the rules that need no window are in
//! `app::project_groups`; this is the half that touches the app and paints.
//! The reasons behind the rules are in `docs/internals/design-decisions.md`,
//! under "Project groups".

use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, FontWeight, InteractiveElement,
    IntoElement, MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Transformation, Window, deferred, div, percentage, prelude::*, px, svg,
};

use super::state::{AgentsContextMenu, AgentsRenameTarget};
use super::{FoldedTally, RAIL_CARET_WIDTH, RAIL_ROW_PADDING_X, RowSharedState, rail_overlay_open};
use crate::SplitlaneApp;
use crate::app::project_groups::{
    self as groups, NamingKind, PendingNewGroup, ProjectGroup, RailSections, TallyWord,
};
use crate::settings::components::{menu_divider_color, select_menu};
use crate::ui_tokens as tok;

/// Between the parts of a label row: the label, `private`, each word, the
/// caret.
const GROUP_ROW_GAP: gpui::Pixels = tok::space::SM;

/// One character of the summary's mono face at its size. JetBrains Mono
/// advances 0.6em, so the width rule can be arithmetic rather than a text
/// layout per frame.
fn summary_char_width() -> f32 {
    f32::from(tok::mono::HINT) * 0.6
}

impl SplitlaneApp {
    // ------------------------------------------------------------------
    // Operations
    // ------------------------------------------------------------------

    fn group_position(&self, group_id: u64) -> Option<usize> {
        self.project_groups
            .iter()
            .position(|group| group.id == group_id)
    }

    pub(crate) fn project_group(&self, group_id: u64) -> Option<&ProjectGroup> {
        self.project_groups
            .iter()
            .find(|group| group.id == group_id)
    }

    /// The group `ws_idx` is in, if it is in one that exists.
    pub(crate) fn group_of_project(&self, ws_idx: usize) -> Option<&ProjectGroup> {
        let id = self.workspaces.get(ws_idx)?.group?;
        self.project_group(id)
    }

    /// Groups with at least one project, in rail order - the ones the rail
    /// draws and the submenu lists.
    pub(crate) fn live_project_groups(&self) -> Vec<&ProjectGroup> {
        self.project_groups
            .iter()
            .filter(|group| {
                !group.name.is_empty()
                    && self.workspaces.iter().any(|ws| ws.group == Some(group.id))
            })
            .collect()
    }

    /// After any change of membership: a group with nobody in it is gone.
    pub(crate) fn reconcile_project_groups(&mut self) {
        groups::reconcile_groups(
            &mut self.project_groups,
            self.workspaces.iter_mut().map(|ws| &mut ws.group),
        );
        // A field naming a group that just went - its project closed while
        // it was being named - has nothing left to name.
        if let Some(AgentsRenameTarget::Group { group_id }) = self.agents_view.agents_renaming
            && self.project_group(group_id).is_none()
        {
            self.agents_view.agents_renaming = None;
            self.agents_view.agents_rename_input = None;
            self.group_field_blur = None;
        }
        if self
            .pending_new_group
            .is_some_and(|pending| self.project_group(pending.group_id).is_none())
        {
            self.pending_new_group = None;
        }
    }

    /// Move project `ws_idx` to the end of the project list, keeping the
    /// active project active. The rail draws each section in list order, so
    /// the end of the list is the end of whichever section it is in.
    fn move_project_to_end(&mut self, ws_idx: usize) -> usize {
        self.move_project_to(ws_idx, self.workspaces.len().saturating_sub(1))
    }

    fn move_project_to(&mut self, ws_idx: usize, to: usize) -> usize {
        if ws_idx >= self.workspaces.len() {
            return ws_idx;
        }
        let active_id = self.workspaces.get(self.active_idx).map(|ws| ws.id);
        let ws = self.workspaces.remove(ws_idx);
        let to = to.min(self.workspaces.len());
        self.workspaces.insert(to, ws);
        if let Some(id) = active_id
            && let Some(active) = self.workspaces.iter().position(|ws| ws.id == id)
        {
            self.active_idx = active;
        }
        to
    }

    pub(crate) fn set_group_collapsed(
        &mut self,
        group_id: u64,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(group) = self
            .project_groups
            .iter_mut()
            .find(|group| group.id == group_id)
            && group.collapsed != collapsed
        {
            group.collapsed = collapsed;
            self.save_session(cx);
            cx.notify();
        }
    }

    pub(crate) fn toggle_group_collapsed(&mut self, group_id: u64, cx: &mut Context<Self>) {
        if let Some(collapsed) = self.project_group(group_id).map(|group| group.collapsed) {
            self.set_group_collapsed(group_id, !collapsed, cx);
        }
    }

    pub(crate) fn toggle_group_private(&mut self, group_id: u64, cx: &mut Context<Self>) {
        if let Some(group) = self
            .project_groups
            .iter_mut()
            .find(|group| group.id == group_id)
        {
            group.private = !group.private;
            self.save_session(cx);
            cx.notify();
        }
    }

    /// `Add to group ▸` → a group: the project goes to the end of it. Picking
    /// the group it is already in does nothing.
    pub(crate) fn add_project_to_group(
        &mut self,
        ws_idx: usize,
        group_id: u64,
        cx: &mut Context<Self>,
    ) {
        if self.group_position(group_id).is_none() {
            return;
        }
        let Some(ws) = self.workspaces.get_mut(ws_idx) else {
            return;
        };
        if ws.group == Some(group_id) {
            return;
        }
        ws.group = Some(group_id);
        self.move_project_to_end(ws_idx);
        self.reconcile_project_groups();
        self.save_session(cx);
        cx.notify();
    }

    /// A project dropped on another project's row: the existing reorder,
    /// and the dragged project joins the target's section - the target's
    /// group, or none. The drop edge is the list's, so the insertion line the
    /// rail drew is where it lands.
    pub(crate) fn drop_project_beside(
        &mut self,
        dragged_id: u64,
        target_idx: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(target_group) = self.workspaces.get(target_idx).map(|ws| ws.group) else {
            return;
        };
        if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == dragged_id) {
            ws.group = target_group;
        }
        self.reorder_workspace(dragged_id, target_idx, cx);
        self.reconcile_project_groups();
        self.save_session(cx);
        cx.notify();
    }

    /// A project dropped on a group's label: the end of that group. A folded
    /// group stays folded and its summary takes the project in.
    pub(crate) fn drop_project_on_group(
        &mut self,
        dragged_id: u64,
        group_id: u64,
        cx: &mut Context<Self>,
    ) {
        if let Some(ws_idx) = self.workspaces.iter().position(|ws| ws.id == dragged_id) {
            self.add_project_to_group(ws_idx, group_id, cx);
        }
    }

    /// A project dropped on the band that stands in for an empty `PROJECTS`
    /// section: out of its group.
    pub(crate) fn drop_project_out_of_groups(&mut self, dragged_id: u64, cx: &mut Context<Self>) {
        if let Some(ws_idx) = self.workspaces.iter().position(|ws| ws.id == dragged_id) {
            self.remove_project_from_group(ws_idx, cx);
        }
    }

    /// A group dropped on another group's label: before or after it, by
    /// which way it travelled - the same rule a project's drop edge follows.
    pub(crate) fn reorder_group(
        &mut self,
        dragged_id: u64,
        target_id: u64,
        cx: &mut Context<Self>,
    ) {
        let (Some(from), Some(to)) = (
            self.group_position(dragged_id),
            self.group_position(target_id),
        ) else {
            return;
        };
        if from == to {
            return;
        }
        let group = self.project_groups.remove(from);
        self.project_groups.insert(to, group);
        self.save_session(cx);
        cx.notify();
    }

    /// `Remove from group`: the project goes to the end of the projects
    /// without a group.
    pub(crate) fn remove_project_from_group(&mut self, ws_idx: usize, cx: &mut Context<Self>) {
        let Some(ws) = self.workspaces.get_mut(ws_idx) else {
            return;
        };
        if ws.group.take().is_none() {
            return;
        }
        self.move_project_to_end(ws_idx);
        self.reconcile_project_groups();
        self.save_session(cx);
        cx.notify();
    }

    /// `Ungroup`: every member goes to the end of the projects without a
    /// group, keeping the order it had inside the group. There is no earlier
    /// place to return to - a project may never have been outside the group.
    pub(crate) fn ungroup(&mut self, group_id: u64, cx: &mut Context<Self>) {
        let members: Vec<u64> = self
            .workspaces
            .iter()
            .filter(|ws| ws.group == Some(group_id))
            .map(|ws| ws.id)
            .collect();
        for id in members {
            if let Some(ws_idx) = self.workspaces.iter().position(|ws| ws.id == id) {
                self.workspaces[ws_idx].group = None;
                self.move_project_to_end(ws_idx);
            }
        }
        self.reconcile_project_groups();
        self.save_session(cx);
        cx.notify();
    }

    /// `New group…`: the group is made at once, at the end of the groups, with
    /// the project already under its rule, and its label opens for the name.
    /// Nothing asks first - the rail is where the group will live, so that is
    /// where it is named.
    pub(crate) fn start_new_group_with(
        &mut self,
        ws_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_agents_rename(cx);
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return;
        };
        let project_id = ws.id;
        let previous_group = ws.group;
        let previous_neighbour = ws_idx
            .checked_sub(1)
            .and_then(|above| self.workspaces.get(above))
            .map(|ws| ws.id);
        let group_id = groups::next_group_id(
            &self.project_groups,
            self.pending_new_group.map_or(1, |p| p.group_id + 1),
        );
        self.project_groups.push(ProjectGroup {
            id: group_id,
            name: String::new(),
            collapsed: false,
            private: false,
        });
        self.workspaces[ws_idx].group = Some(group_id);
        let moved_to = self.move_project_to_end(ws_idx);
        debug_assert_eq!(self.workspaces[moved_to].id, project_id);
        self.pending_new_group = Some(PendingNewGroup {
            group_id,
            project_id,
            previous_neighbour,
            previous_index: ws_idx,
            previous_group,
        });
        // Not reconciled yet: if this was the last project of its old group,
        // that group has to survive the naming, or abandoning it could not
        // put the project back. An empty group is neither drawn nor listed,
        // and settling the name reconciles.
        self.begin_agents_rename(AgentsRenameTarget::Group { group_id }, window, cx);
    }

    /// Put back what `New group…` did: the group goes, and the project
    /// returns to its old group and its old place. A no-op unless `group_id`
    /// is the group still waiting for its name.
    ///
    /// The old group is still there even if the move emptied it, because
    /// `start_new_group_with` does not reconcile.
    pub(crate) fn abandon_pending_group(&mut self, group_id: u64, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_new_group.filter(|p| p.group_id == group_id) else {
            return;
        };
        self.pending_new_group = None;
        if let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|ws| ws.id == pending.project_id)
        {
            self.workspaces[ws_idx].group = pending.previous_group;
            let to = match pending.previous_neighbour {
                None => 0,
                Some(neighbour) => self
                    .workspaces
                    .iter()
                    .filter(|ws| ws.id != pending.project_id)
                    .position(|ws| ws.id == neighbour)
                    .map_or(pending.previous_index, |above| above + 1),
            };
            self.move_project_to(ws_idx, to);
        }
        self.project_groups.retain(|group| group.id != group_id);
        self.reconcile_project_groups();
        self.save_session(cx);
        cx.notify();
    }

    /// Settle a name typed into a group's label. Returns whether the field
    /// is done: `false` only for a rename onto a name another group has,
    /// which does nothing and leaves the field open.
    pub(crate) fn apply_group_name(
        &mut self,
        group_id: u64,
        typed: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let name = groups::normalize_group_name(typed);
        let existing =
            groups::group_named(&self.project_groups, &name, Some(group_id)).map(|group| group.id);
        if let Some(pending) = self.pending_new_group.filter(|p| p.group_id == group_id) {
            if name.is_empty() {
                self.abandon_pending_group(group_id, cx);
                return true;
            }
            self.pending_new_group = None;
            match existing {
                // The name of a group that exists: the project joins it. It
                // is already last in the list, so it is last in that group.
                Some(existing) => {
                    if let Some(ws) = self
                        .workspaces
                        .iter_mut()
                        .find(|ws| ws.id == pending.project_id)
                    {
                        ws.group = Some(existing);
                    }
                    self.project_groups.retain(|group| group.id != group_id);
                }
                None => {
                    if let Some(group) = self
                        .project_groups
                        .iter_mut()
                        .find(|group| group.id == group_id)
                    {
                        group.name = name;
                    }
                }
            }
            self.reconcile_project_groups();
            self.save_session(cx);
            cx.notify();
            return true;
        }
        if name.is_empty() {
            return true;
        }
        if existing.is_some() {
            return false;
        }
        if let Some(group) = self
            .project_groups
            .iter_mut()
            .find(|group| group.id == group_id)
        {
            group.name = name;
            self.save_session(cx);
            cx.notify();
        }
        true
    }

    /// Focus left a group's field. An empty or taken name cannot be kept, so
    /// the field is abandoned - which, for a new group, puts the project back;
    /// any other name is kept, as a click elsewhere keeps a project's.
    ///
    /// Deferred, because this runs inside the focus-out subscription that the
    /// settling drops.
    pub(crate) fn settle_group_field_on_blur(&mut self, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = weak.update(cx, |this, cx| {
                let Some(AgentsRenameTarget::Group { group_id }) = this.agents_view.agents_renaming
                else {
                    return;
                };
                let typed = this
                    .agents_view
                    .agents_rename_input
                    .as_ref()
                    .map(|input| input.read(cx).value())
                    .unwrap_or_default();
                let name = groups::normalize_group_name(&typed);
                let is_new = this
                    .pending_new_group
                    .is_some_and(|pending| pending.group_id == group_id);
                let taken = !is_new
                    && groups::group_named(&this.project_groups, &name, Some(group_id)).is_some();
                if name.is_empty() || taken {
                    this.cancel_agents_rename(cx);
                } else {
                    this.apply_agents_rename(typed, cx);
                }
            });
        });
    }

    pub(crate) fn begin_group_rename(
        &mut self,
        group_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_group(group_id).is_some() {
            self.close_agents_menu(cx);
            self.begin_agents_rename(AgentsRenameTarget::Group { group_id }, window, cx);
        }
    }

    pub(crate) fn open_group_menu(
        &mut self,
        group_id: u64,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.commit_agents_rename(cx);
        self.dismiss_transient_surfaces();
        self.workspace_menu_open = None;
        self.agents_view.agents_menu_open = Some(AgentsContextMenu::Group { group_id, position });
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Drawing
    // ------------------------------------------------------------------

    /// Every group, after the projects without one: its label, and while it
    /// is open, its members under the rule.
    pub(super) fn render_group_sections(
        &mut self,
        mut list: gpui::Div,
        sections: &RailSections,
        shared: &RowSharedState,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let ui = shared.ui;
        for (position, members) in &sections.groups {
            let Some(group) = self.project_groups.get(*position).cloned() else {
                continue;
            };
            let naming = matches!(
                shared.renaming,
                Some(AgentsRenameTarget::Group { group_id }) if group_id == group.id
            );
            if naming && let Some(input) = shared.rename_input.clone() {
                list = list.child(self.group_naming_rows(&group, input, ui, cx));
            } else {
                let tally = FoldedTally::of(
                    members
                        .iter()
                        .filter_map(|&ws_idx| self.workspaces.get(ws_idx))
                        .flat_map(|ws| ws.threads.iter()),
                    true,
                );
                list = list.child(self.group_label_row(
                    &group,
                    *position,
                    members.len(),
                    tally,
                    ui,
                    cx,
                ));
            }
            // Folding is visual only, and a folded group has no rule: the
            // rule is what says "these belong to the label above", and there
            // is nothing under a folded label for it to say that about.
            if group.collapsed {
                continue;
            }
            let mut body = div()
                .flex_none()
                .flex()
                .flex_col()
                .ml(tok::group::RULE_INSET)
                .pl(tok::group::RULE_GAP)
                .pb(tok::group::RULE_GAP)
                .border_color(ui.group_rule)
                // While a project is over this group - its insertion line
                // between two of these members - the rule lightens, so the
                // group it will land in is plain even with the line on the
                // border between two groups. No rule lightens outside a group,
                // and that is the signal for "no group".
                .drag_over::<crate::app::drag::WorkspaceDrag>(move |style, _, _, _| {
                    style.border_color(ui.dim)
                });
            body.style().border_widths.left = Some(tok::group::RULE.into());
            for (index, &ws_idx) in members.iter().enumerate() {
                // The first member sits right under the label; the label's
                // own air is what separates it from the group above.
                body = self.project_block(body, ws_idx, index > 0, shared, cx);
            }
            list = list.child(body);
        }
        list
    }

    /// A group's label row: the name, `private` for a private group, the
    /// summary while folded, and the caret on the right.
    fn group_label_row(
        &self,
        group: &ProjectGroup,
        position: usize,
        project_count: usize,
        tally: FoldedTally,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group_id = group.id;
        let words = if group.collapsed {
            groups::folded_group_words(tally, project_count)
        } else {
            Vec::new()
        };
        // The name as drawn: capitals in mono at the label's size, plus the
        // label's own padding.
        let label_natural = group.name.to_uppercase().chars().count() as f32 * f32::from(tok::mono::LABEL) * 0.6
                + 2.0 * f32::from(tok::group::LABEL_PAD_X)
            // A pixel of slack, so rounding never turns a name that fits into
            // one that ends in an ellipsis.
            + 1.0;
        let fit = groups::fit_label_row(
            &words,
            group.private,
            label_natural,
            self.group_label_room(),
            summary_char_width(),
            f32::from(GROUP_ROW_GAP),
        );
        let full_name: SharedString = group.name.clone().into();
        let tooltips_ok = !rail_overlay_open(self);
        let hit_group = SharedString::from(format!("rail-group-hit-{group_id}"));
        let drag = crate::app::drag::GroupDrag {
            id: group_id,
            source_position: position,
            label: group.name.to_uppercase().into(),
            summary: groups::TallyWord::Projects(project_count).text().into(),
        };
        let drag_kind = self.rail_drag_kind.clone();

        let label = div()
            .id(SharedString::from(format!("rail-group-label-{group_id}")))
            .flex_none()
            .w(px(fit.label_width))
            // The drop target's 1px frame is always there, transparent, and
            // comes out of the padding, so lighting it moves nothing.
            .px(tok::group::LABEL_PAD_X - px(1.))
            .py(tok::group::LABEL_PAD_Y - px(1.))
            .border_1()
            .border_color(ui.tag_fill)
            .group_drag_over::<crate::app::drag::WorkspaceDrag>(hit_group.clone(), move |style| {
                style.border_color(ui.dim)
            })
            .rounded(tok::radius::BADGE)
            .bg(ui.tag_fill)
            .text_color(ui.text)
            .text_size(tok::mono::LABEL)
            .font_weight(FontWeight::MEDIUM)
            .truncate()
            // Capitals are the label's style, as they are `PROJECTS`'s; the
            // name is kept, and shown everywhere else, as typed.
            .child(group.name.to_uppercase())
            .when(tooltips_ok, |label| {
                label.tooltip(move |_w, cx| {
                    cx.new(|_| crate::app::sidebar::SidebarTooltip {
                        label: full_name.clone(),
                    })
                    .into()
                })
            });

        let mut row = div()
            .id(SharedString::from(format!("rail-group-{group_id}")))
            .flex_none()
            .h(tok::row::GROUP)
            .mt(tok::space::XL)
            .mb(tok::space::XS)
            .pl(tok::space::SM)
            .pr(RAIL_ROW_PADDING_X)
            .flex()
            .flex_row()
            .items_center()
            .gap(GROUP_ROW_GAP)
            .font_family(tok::font::MONO)
            .cursor_pointer()
            .group(hit_group)
            // A project dropped on the label goes to the end of the group.
            // A folded group is not opened for it, on hover or after: opening
            // would move everything below the pointer while it is on its way
            // somewhere else. For an exact place, open the group first.
            .on_drop(cx.listener(
                move |this, drag: &crate::app::drag::WorkspaceDrag, _window, cx| {
                    this.drop_project_on_group(drag.id, group_id, cx);
                },
            ))
            // The whole group travels by its label, and lands only between
            // groups: never inside one (no nesting), never above the projects
            // without a group (they always come first).
            .on_drag(drag, move |drag, _offset, _window, cx| {
                drag_kind.set(Some(crate::app::drag::RailDragKind::Group));
                cx.new(|_| crate::app::drag::GroupDragPreview {
                    label: drag.label.clone(),
                    summary: drag.summary.clone(),
                })
            })
            .drag_over::<crate::app::drag::GroupDrag>(move |style, drag, _, _| {
                let indicator = ui.text.opacity(0.4);
                if drag.id == group_id {
                    style
                } else if drag.source_position < position {
                    style.border_b_1().border_color(indicator)
                } else {
                    style.border_t_1().border_color(indicator)
                }
            })
            .on_drop(cx.listener(
                move |this, drag: &crate::app::drag::GroupDrag, _window, cx| {
                    this.reorder_group(drag.id, group_id, cx);
                },
            ))
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                this.close_agents_menu(cx);
                let is_double = matches!(e, ClickEvent::Mouse(m) if m.down.click_count == 2);
                if is_double {
                    // The first click of the pair already folded or opened
                    // it; the pair means rename, and leaves the fold as it
                    // was.
                    this.toggle_group_collapsed(group_id, cx);
                    this.begin_group_rename(group_id, window, cx);
                } else {
                    this.commit_agents_rename(cx);
                    this.toggle_group_collapsed(group_id, cx);
                }
            }))
            .on_aux_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                if e.is_right_click()
                    && let Some(position) = e.mouse_position()
                {
                    this.open_group_menu(group_id, position, cx);
                    cx.stop_propagation();
                }
            }))
            .child(label);
        if fit.private_word {
            row = row.child(
                div()
                    .flex_none()
                    .text_size(tok::mono::HINT)
                    .text_color(ui.faint)
                    .child("private"),
            );
        }
        row = row.child(div().flex_1().min_w_0());
        for word in fit.words {
            let tone = match word {
                TallyWord::Failed(_) => ui.agent_error,
                TallyWord::Waiting(_) => ui.accent,
                _ => ui.faint,
            };
            row = row.child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(tok::mono::HINT)
                    .text_color(tone)
                    .child(word.text()),
            );
        }
        row.child(group_caret(group_id, !group.collapsed, ui))
            .into_any_element()
    }

    /// Pixels a label row has for the label's minimum, `private` and the
    /// summary: the rail less the list's inset, the row's padding and the
    /// caret.
    fn group_label_room(&self) -> f32 {
        self.rail_width
            - 2.0 * f32::from(tok::space::MD)
            - f32::from(tok::space::SM)
            - f32::from(RAIL_ROW_PADDING_X)
            - f32::from(RAIL_CARET_WIDTH)
            - f32::from(GROUP_ROW_GAP)
    }

    /// The label as a field, while a group is being named: the whole row, no
    /// caret and no summary, and a line under it once there is something to
    /// say.
    fn group_naming_rows(
        &self,
        group: &ProjectGroup,
        input: gpui::Entity<crate::widgets::text_area::TextArea>,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = if self
            .pending_new_group
            .is_some_and(|pending| pending.group_id == group.id)
        {
            NamingKind::New
        } else {
            NamingKind::Rename
        };
        let typed = input.read(cx).value();
        let hint = groups::naming_hint(kind, &typed, &self.project_groups, group.id);
        // A name that would join another group is a question, not a warning:
        // one step brighter than the plain instructions, and no hue.
        let hint_tone = match kind {
            NamingKind::New if typed.trim().is_empty() => ui.faint,
            NamingKind::New
                if groups::group_named(&self.project_groups, &typed, Some(group.id)).is_none() =>
            {
                ui.faint
            }
            _ => ui.dim,
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(tok::row::GROUP)
                    .mt(tok::space::XL)
                    .pl(tok::space::SM)
                    .pr(RAIL_ROW_PADDING_X)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(tok::group::FIELD)
                            // A column, so the field is stretched to the row's
                            // width. In a row with centred items it got no width
                            // of its own and wrapped after every letter.
                            .flex()
                            .flex_col()
                            .justify_center()
                            .overflow_hidden()
                            .px(tok::space::SM)
                            .rounded(tok::radius::BADGE)
                            .bg(ui.tag_fill)
                            .border_1()
                            .border_color(ui.focus_border)
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::LABEL)
                            .text_color(ui.text)
                            .child(input),
                    ),
            )
            .children(hint.map(|hint| {
                div()
                    .flex_none()
                    .pt(tok::space::XS)
                    .pl(tok::space::SM)
                    .pr(RAIL_ROW_PADDING_X)
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::HINT)
                    .text_color(hint_tone)
                    .child(hint)
            }))
            .child(div().flex_none().h(tok::space::XS))
            .into_any_element()
    }

    /// The label's menu: `Rename group ⏎` · `New project in group` ·
    /// `Keep names private` · — · `Ungroup`.
    pub(crate) fn render_group_menu(
        &self,
        group_id: u64,
        position: gpui::Point<gpui::Pixels>,
        ui: crate::theme::UiColors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let private = self
            .project_group(group_id)
            .is_some_and(|group| group.private);
        let width = px(220.);
        // Four rows, one divider, the menu's padding.
        let height = px(8. + 4. * 29. + 10.);
        let menu_pos = crate::app::sidebar::context_menu::clamped_context_menu_position(
            position, width, height, window,
        );
        let menu = select_menu("rail-group-menu", ui)
            .occlude()
            .absolute()
            .left(menu_pos.x)
            .top(menu_pos.y)
            .w(width)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_agents_menu(cx)))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(self.render_select_menu_item(
                "rail-group-menu-rename".into(),
                "Rename group",
                Some("\u{23ce}".into()),
                ui,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.begin_group_rename(group_id, window, cx);
                    cx.stop_propagation();
                }),
            ))
            .child(self.render_select_menu_item(
                "rail-group-menu-new-project".into(),
                "New project in group",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.close_agents_menu(cx);
                    this.create_agents_project_with_picker_into(Some(group_id), cx);
                    cx.stop_propagation();
                }),
            ))
            .child(self.render_select_menu_item(
                "rail-group-menu-private".into(),
                "Keep names private",
                private.then(|| SharedString::from("\u{2713}")),
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.close_agents_menu(cx);
                    this.toggle_group_private(group_id, cx);
                    cx.stop_propagation();
                }),
            ))
            .child(
                div()
                    .mx(tok::space::XS)
                    .my(tok::space::XS)
                    .flex_none()
                    .h(px(1.))
                    .bg(menu_divider_color(ui)),
            )
            .child(self.render_select_menu_item(
                "rail-group-menu-ungroup".into(),
                "Ungroup",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.close_agents_menu(cx);
                    this.ungroup(group_id, cx);
                    cx.stop_propagation();
                }),
            ));
        deferred(menu).priority(3).into_any_element()
    }
}

/// The label's caret, on the right: pointing right while folded, down while
/// open. A project's caret is on the left, which is the third thing telling a
/// label from a project after the fill and the rule.
///
/// Not a button of its own, unlike a project's: the whole label row folds, so
/// the caret only shows which way it is.
fn group_caret(group_id: u64, is_open: bool, ui: crate::theme::UiColors) -> AnyElement {
    let (from, to) = if is_open { (0.0, 0.25) } else { (0.25, 0.0) };
    div()
        .flex_none()
        .w(RAIL_CARET_WIDTH)
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .size(tok::mono::LABEL)
                .flex_none()
                .path("icons/chevron-right.svg")
                .text_color(ui.faint)
                .with_animation(
                    SharedString::from(format!("rail-group-caret-{group_id}-{is_open}")),
                    Animation::new(tok::motion::CARET),
                    move |svg, delta| {
                        svg.with_transformation(Transformation::rotate(percentage(
                            from + (to - from) * delta,
                        )))
                    },
                ),
        )
        .into_any_element()
}
