//! Agent surfaces inside the container's slot tree.
//!
//! An agent surface used to be shown one at a time, full-area, parked beside
//! the layout rather than in it. That is why an agent and a terminal could not
//! be on screen together: the container had a split tree, and the agent was
//! not in it. The bottom dock was the old answer to that, and it could compose
//! exactly one arrangement.
//!
//! This is the general one. An agent surface goes into a slot like any other
//! surface, so it splits like any other surface - agent beside terminal, agent
//! beside the diff, agent beside markdown, two agents side by side - and none
//! of those is a feature of its own.
//!
//! What must survive the move is the surface's identity: its `SessionBinding`
//! and its live PTY. Both hang off the [`TerminalView`] entity, and it is that
//! same entity that moves into the slot - nothing is respawned, relaunched, or
//! copied. The entity carries `agent_thread_id`, so wherever it ends up (a
//! different slot, a different pane after a drag) it is still the same agent
//! surface, and the save path writes it as one.

use gpui::{App, Context, Entity, Focusable, Window};

use crate::SplitlaneApp;
use crate::layout::LayoutTree;
use crate::pane::{Pane, TabContent};
use crate::terminal::view::TerminalView;
use crate::workspace::Workspace;

/// The agent surface `thread_id`'s PTY view, if it is showing in `pane`.
pub(crate) fn agent_view_in_pane(
    pane: &Entity<Pane>,
    thread_id: u64,
    cx: &App,
) -> Option<Entity<TerminalView>> {
    pane.read(cx).tabs.iter().find_map(|tab| match tab {
        TabContent::Terminal(view) if view.read(cx).agent_thread_id == Some(thread_id) => {
            Some(view.clone())
        }
        _ => None,
    })
}

/// The pane in `root` holding the agent surface `thread_id`, if any.
pub(crate) fn agent_pane(root: &LayoutTree, thread_id: u64, cx: &App) -> Option<Entity<Pane>> {
    root.collect_leaves()
        .into_iter()
        .find(|pane| agent_view_in_pane(pane, thread_id, cx).is_some())
}

/// Where the agent surface `thread_id` is showing in `container`'s slot tree:
/// the index of the leaf that holds it, counted left-to-right, or `None` when
/// it is parked.
///
/// Derived on every read rather than stored on the thread. A stored placement
/// would have to be updated by closing a tab, closing a pane, dragging a tab
/// to another pane, and every layout preset - and the cost of missing one is a
/// session file claiming a slot that no longer exists.
pub(crate) fn agent_leaf_index(container: &Workspace, thread_id: u64, cx: &App) -> Option<usize> {
    container
        .root
        .as_ref()?
        .collect_leaves()
        .iter()
        .position(|pane| agent_view_in_pane(pane, thread_id, cx).is_some())
}

impl SplitlaneApp {
    /// Whether this container's agent surface is showing in its slot tree
    /// rather than parked.
    pub(crate) fn agent_surface_in_slot(&self, ws_idx: usize, thread_id: u64, cx: &App) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|container| agent_leaf_index(container, thread_id, cx))
            .is_some()
    }

    /// Show the slot tree with the agent surface `thread_id` active in its
    /// pane. This is what selecting its rail row does once it lives in a slot:
    /// the surface is on screen as part of the layout, so pointing the content
    /// area at the layout and raising its tab is the whole of "select it".
    ///
    /// Returns `false` when the surface is not in this container's tree.
    pub(crate) fn reveal_agent_in_slot(
        &mut self,
        ws_idx: usize,
        thread_id: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pane) = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
            .and_then(|root| agent_pane(root, thread_id, cx))
        else {
            return false;
        };
        let tab_idx = pane
            .read(cx)
            .tabs
            .iter()
            .position(|tab| matches!(tab, TabContent::Terminal(view) if view.read(cx).agent_thread_id == Some(thread_id)));
        if let Some(idx) = tab_idx {
            pane.update(cx, |pane, cx| {
                pane.selected_idx = idx;
                cx.notify();
            });
        }
        self.active_idx = ws_idx;
        // Focus needs a `Window`, which a rail click handler does not have;
        // the same deferred channel `select_container_surface` uses hands it
        // over on the next render.
        self.pending_pane_focus = Some(pane);
        self.reroot_files_tree(cx);
        self.save_session(cx);
        cx.notify();
        true
    }

    /// Append a parked agent surface to the end of the container's slot tree
    /// and re-split evenly - the drop strip's whole behaviour.
    ///
    /// The design: "Dropping there appends a pane and re-splits evenly
    /// (max 3)." Appending rather than splitting from the focused slot is the
    /// difference from [`SplitlaneApp::move_agent_into_slot`]: a strip at the
    /// far edge of the panes area is aimed at a position, not at a neighbour.
    pub(crate) fn append_agent_to_slots(
        &mut self,
        ws_idx: usize,
        thread_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
        let Some(thread_id) = self.thread_for_target(target).map(|thread| thread.id) else {
            return false;
        };
        if self.agent_surface_in_slot(ws_idx, thread_id, cx) {
            return false;
        }
        let Some(container) = self.workspaces.get(ws_idx) else {
            return false;
        };
        let Some(root) = container.root.as_ref() else {
            return false;
        };
        if root.leaf_count() >= crate::layout::MAX_PANES {
            self.show_toast(
                format!("Maximum pane count reached ({})", crate::layout::MAX_PANES),
                cx,
            );
            return false;
        }
        // The last slot, so the new one lands after it - which is where the
        // strip is drawn.
        let Some(anchor) = root.collect_leaves().into_iter().next_back() else {
            return false;
        };
        let direction = root
            .root_direction()
            .unwrap_or(crate::layout::SplitDirection::Vertical);
        let Some(view) = self.mount_agents_terminal_for_target(target, cx) else {
            return false;
        };
        let pane = self.create_pane_with_existing_tab(TabContent::Terminal(view), cx);
        let inserted = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|container| container.root.as_mut())
            .is_some_and(|root| root.split_at_pane(&anchor, direction, pane.clone()));
        if !inserted {
            return false;
        }
        self.active_idx = ws_idx;
        pane.read(cx).focus_handle(cx).focus(window, cx);
        self.save_session(cx);
        cx.notify();
        true
    }

    /// Show a parked agent surface in `pane` - the drop onto an existing slot.
    ///
    /// The design calls this "swaps that pane's session", which in this app is
    /// literal: a pane there shows exactly one session. A `Pane` here holds a
    /// tab strip, so the surface is added to it and raised. Nothing in the slot
    /// is closed, because closing a live shell to make room for a drop is a
    /// destruction nobody asked for - and the slot header's `\u{d7}` is right
    /// there for anyone who did.
    /// Name a pane by its **position**, for a menu that has already named it by
    /// its contents.
    ///
    /// Positions and not numbers, because **panes are not numbered anywhere a
    /// person can see**: a menu that says "pane 2" asks the reader
    /// for a count the interface never showed them - guessable at two, wrong at
    /// four. With two panes the position is a side, which is what the eye
    /// already has; in a flat row or column past two there is no side, so an
    /// ordinal is the least bad answer and the contents carry the
    /// identification.
    ///
    /// **A grid cell has a name and gets it**. This is what
    /// "reorder the grid" comes to, and it is why there is no drag: in a 2x2
    /// the position is *chosen*, so the destination has to be nameable - and
    /// `3rd` names an index rather than a place the eye can find. Two words
    /// each, in the menu that already exists. The order is the grid's own
    /// reading order, top-left across then down, which `collect_leaves` states
    /// and a test pins.
    pub(crate) fn pane_slot_label(
        index: usize,
        count: usize,
        form: Option<crate::layout::PaneForm>,
    ) -> &'static str {
        if form == Some(crate::layout::PaneForm::Grid) {
            return ["top left", "top right", "bottom left", "bottom right"]
                .get(index)
                .copied()
                .unwrap_or("");
        }
        if count == 2 {
            let stacked = form == Some(crate::layout::PaneForm::Column);
            return match (index, stacked) {
                (0, true) => "top",
                (_, true) => "bottom",
                (0, false) => "left",
                _ => "right",
            };
        }
        ["1st", "2nd", "3rd", "4th"]
            .get(index)
            .copied()
            .unwrap_or("")
    }

    /// Append a pane and show `target`'s surface in it - the menu path for a
    /// drag onto the drop strip.
    ///
    /// Only offered when [`Self::room_for_another_pane_now`] says yes, which is
    /// the 320px floor and not a count, so this cannot produce a pane too
    /// narrow to read its own header.
    pub(crate) fn show_surface_in_new_pane(
        &mut self,
        target: crate::project::AgentsTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let crate::project::AgentsTarget::Thread { ws_idx, .. } = target else {
            return false;
        };
        // A surface already in a slot has a pane of its own, and this path does
        // not take it out of that one - so appending would put the session in
        // two panes. The menu does not offer it in that case; refusing here
        // means the invariant does not depend on the menu getting it right.
        if let Some(thread_id) = self.thread_for_target(target).map(|thread| thread.id)
            && self.agent_surface_in_slot(ws_idx, thread_id, cx)
        {
            return false;
        }
        let Some(view) = self.mount_agents_terminal_for_target(target, cx) else {
            return false;
        };
        let Some(pane) = self.append_pane_with(ws_idx, crate::pane::TabContent::Terminal(view), cx)
        else {
            return false;
        };
        self.active_idx = ws_idx;
        pane.read(cx).focus_handle(cx).focus(window, cx);
        self.save_session(cx);
        cx.notify();
        true
    }

    /// Give every terminal living in a pane a record of its own, so the rail
    /// has one kind of surface row instead of two.
    ///
    /// A shell created by a split, restored from a layout, or launched by a
    /// preset had no record: it was drawn by `container_pane_rows`, derived
    /// from the pane it sat in, and that row carried no actions at all - no
    /// delete, no overflow menu - while a shell created from the rail's
    /// `+ shell` carried the full cluster. Same object, same word in the rail,
    /// two behaviours, and the difference was invisible until someone hovered
    /// one and got nothing. Reported from real use, twice.
    ///
    /// Idempotent by construction - a terminal that already has a record is
    /// skipped - which is what lets this run from the render pass beside
    /// `track_pane_focus` and cover every producer instead of chasing
    /// each one. It reuses [`Self::park_displaced_surface`], so the name a
    /// terminal is showing is pinned before the id goes on, exactly as it is
    /// when a surface is displaced.
    pub(crate) fn adopt_orphan_pane_surfaces(&mut self, cx: &mut Context<Self>) {
        let mut orphans: Vec<(usize, crate::pane::TabContent)> = Vec::new();
        for (ws_idx, container) in self.workspaces.iter().enumerate() {
            // A container at its thread limit cannot take another record, and
            // asking it every frame would toast every frame and write the
            // session file every frame - a render-rate loop over a surface
            // that can never be adopted. Found by a cross-vendor pass; the
            // sweep runs from the render pass, so anything it retries forever
            // it retries at frame rate.
            if container.threads.len() >= crate::app::project_ops::MAX_THREADS_PER_PROJECT {
                continue;
            }
            let Some(root) = container.root.as_ref() else {
                continue;
            };
            for pane in root.collect_leaves() {
                let Some(surface) = pane.read(cx).surface() else {
                    continue;
                };
                if surface
                    .as_terminal()
                    .is_some_and(|view| view.read(cx).agent_thread_id.is_none())
                {
                    orphans.push((ws_idx, surface));
                }
            }
        }
        if orphans.is_empty() {
            return;
        }
        // Only what actually took a record is worth a write.
        let mut adopted = false;
        for (ws_idx, surface) in &orphans {
            adopted |= self.park_displaced_surface(*ws_idx, surface, cx);
        }
        if adopted {
            self.save_session(cx);
        }
    }

    /// Give a surface that has just been pushed out of a pane somewhere to
    /// live, so that showing one thing never destroys another.
    ///
    /// The design answers "where did my shell go" with the rail: the row stays,
    /// dimmed, keeping its status dot. That answer only holds if something
    /// still owns the surface. An agent surface is owned by
    /// `agents_terminal_view_cache` and already has a row, so it parks itself.
    /// A terminal a split created owns nothing and is named by nothing - drop
    /// the pane's pointer and the PTY is unreachable, which is how a running
    /// shell used to vanish from the rail with its process, silently, on every
    /// replace.
    ///
    /// A markdown or diff surface is a view of something still on disk and has
    /// no row kind of its own; it is reopened from the file tree or the rail,
    /// which is the same answer restore gives for an over-cap slot.
    /// Returns whether the surface is safe to displace. A markdown or diff
    /// view and an already-owned agent surface are safe with nothing done;
    /// only a terminal that owns nothing can come back `false`, and then the
    /// caller must not go through with the replace.
    pub(crate) fn park_displaced_surface(
        &mut self,
        ws_idx: usize,
        displaced: &crate::pane::TabContent,
        cx: &mut Context<Self>,
    ) -> bool {
        let crate::pane::TabContent::Terminal(view) = displaced else {
            return true;
        };
        if view.read(cx).agent_thread_id.is_some() {
            return true;
        }
        let title = crate::pane::Pane::tab_title(displaced, cx);
        let Ok(thread_id) = self.add_terminal_thread(ws_idx, title.clone(), None, cx) else {
            // The container is at its thread limit. Nothing to park it into,
            // and saying so is better than losing it quietly.
            self.show_toast(
                "That project is full - the displaced shell could not be kept".to_string(),
                cx,
            );
            return false;
        };
        // No custom name is pinned here. It used to be, because
        // `terminal_tab_title` decided "name this by its rail row" from
        // `agent_thread_id.is_some()`, so giving a shell a record renamed it -
        // and pinning froze a label that had been following the shell's cwd.
        // The title asks `surface_is_agent` now, so a shell keeps being named
        // like a shell and keeps tracking its directory.
        // A shell takes a record and is still not an agent, so it keeps
        // being named the way a shell is named. One call sets both.
        view.update(cx, |view, _| view.bind_surface(thread_id, false));
        self.adopt_terminal_as_surface(thread_id, view.clone(), cx);
        true
    }

    /// Put `target`'s surface in `dest`, the menu path for what a drag onto a
    /// pane does.
    ///
    /// **A swap when the surface is already on screen, a replace when it is
    /// not**, and the difference is not a nicety. "Shown" is one state per
    /// session and the rail draws one row for it, so a session in two panes is
    /// a state the rail cannot express - it would have to draw the row twice or
    /// lie about one of them. The drag path held that line by construction
    /// (a surface can only be dragged out of the pane it is in); a menu can
    /// name any pane from anywhere, so it has to hold the line itself.
    pub(crate) fn show_surface_in_pane(
        &mut self,
        ws_idx: usize,
        thread_idx: usize,
        dest: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = crate::project::AgentsTarget::Thread { ws_idx, thread_idx };
        let Some(thread_id) = self.thread_for_target(target).map(|thread| thread.id) else {
            return false;
        };
        // The destination is a handle captured when the menu was built, and a
        // pane can be closed from the keyboard while a menu is open. Mounting
        // into a leaf that has left the tree would hide the session in a pane
        // nothing draws, so the handle is re-checked against the tree now.
        let Some(root) = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
        else {
            return false;
        };
        if !root.collect_leaves().iter().any(|leaf| leaf == dest) {
            return false;
        }
        // Where it is now, if anywhere. Read before anything moves.
        let source = agent_pane(root, thread_id, cx);
        if source.as_ref().is_some_and(|pane| pane == dest) {
            // Already there. Focus it rather than shuffling it in place.
            dest.read(cx).focus_handle(cx).focus(window, cx);
            return true;
        }
        let Some(view) = self.mount_agents_terminal_for_target(target, cx) else {
            return false;
        };
        let displaced = dest.read(cx).surface();
        // Before anything points away from it: a surface nothing else owns
        // gets a rail row, so the replace below cannot destroy it. If it
        // cannot be given one, nothing happens at all - a view action that
        // says "show this here" must not be able to end a process, so a
        // refusal is the only honest answer left.
        if let Some(displaced) = displaced.as_ref()
            && !self.park_displaced_surface(ws_idx, displaced, cx)
        {
            return false;
        }
        // Source first, in two steps, so no moment of this has one surface in
        // two panes. `cx.emit` runs subscribers inline, and a subscriber that
        // walks the tree between the two updates would see the state the rail
        // cannot draw. Emptying the source first makes the only transient
        // state "in no pane", which is always safe to observe.
        if let Some(source) = source.as_ref() {
            source.update(cx, |pane, cx| pane.clear_to_launcher(cx));
        }
        dest.update(cx, |pane, cx| pane.show_terminal(view, cx));
        // And the other half: whatever the destination was showing goes back
        // to where this surface came from. A destination that was showing
        // nothing hands nothing back, and the source stays the launcher.
        if let (Some(source), Some(displaced)) = (source, displaced) {
            source.update(cx, |pane, cx| pane.show_surface(displaced, cx));
        }
        self.active_idx = ws_idx;
        dest.read(cx).focus_handle(cx).focus(window, cx);
        self.save_session(cx);
        cx.notify();
        true
    }

    /// Where the surface `thread_id` sits in `ws_idx`'s rail list right now.
    ///
    /// A drag payload carries both the position and the id; the position is
    /// what every placement API takes, and the id is what is still true after
    /// a rail reorder under the pointer. So a drop resolves one from the
    /// other rather than trusting the position it started with.
    pub(crate) fn thread_index_by_id(&self, ws_idx: usize, thread_id: u64) -> Option<usize> {
        self.workspaces
            .get(ws_idx)?
            .threads
            .iter()
            .position(|thread| thread.id == thread_id)
    }

    /// Take the agent surface `thread_id` out of every slot of `ws_idx`.
    ///
    /// Used when the surface itself goes away. Taking it out of the layout is
    /// not the same as deleting it: closing its tab by hand does exactly this
    /// and the surface is simply parked again, still in the rail, still
    /// running. What must not happen is the reverse - the row gone and the PTY
    /// left in a slot, reachable by nothing.
    pub(crate) fn remove_agent_from_slots(
        &mut self,
        ws_idx: usize,
        thread_id: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .workspaces
            .get(ws_idx)
            .and_then(|container| container.root.as_ref())
        else {
            return;
        };
        for pane in root.collect_leaves() {
            let tab_idx = pane.read(cx).tabs.iter().position(
                |tab| matches!(tab, TabContent::Terminal(view) if view.read(cx).agent_thread_id == Some(thread_id)),
            );
            if let Some(idx) = tab_idx {
                // `close_tab_at` emits `Remove` when it empties the pane, and
                // the app's pane-event handler drops the leaf from the tree -
                // the same path a hand-closed last tab takes.
                pane.update(cx, |pane, cx| pane.close_tab_at(idx, cx));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::SplitlaneApp;
    use crate::layout::PaneForm;

    /// A grid cell has a name, and it is the whole of what "reorder
    /// the grid" comes to. The order is the grid's own reading order, which
    /// `collect_leaves` states and `reading_order_is_top_left_across_then_down`
    /// pins - so the label and the pane it names cannot drift apart.
    #[test]
    fn a_grid_cell_is_named_by_where_it_is() {
        let names: Vec<&str> = (0..4)
            .map(|idx| SplitlaneApp::pane_slot_label(idx, 4, Some(PaneForm::Grid)))
            .collect();
        assert_eq!(
            names,
            ["top left", "top right", "bottom left", "bottom right"]
        );
    }

    /// And nothing else changed: two panes are still a side, and a flat row or
    /// column of three or four is still an ordinal, because past two in one
    /// direction there is no side to name.
    #[test]
    fn the_flat_forms_keep_the_answers_they_had() {
        assert_eq!(
            SplitlaneApp::pane_slot_label(0, 2, Some(PaneForm::Row)),
            "left"
        );
        assert_eq!(
            SplitlaneApp::pane_slot_label(1, 2, Some(PaneForm::Column)),
            "bottom"
        );
        assert_eq!(
            SplitlaneApp::pane_slot_label(2, 3, Some(PaneForm::Row)),
            "3rd"
        );
        // A single pane has no arrangement, and `root_form` answers `None`
        // there; the label has to survive that rather than assume a form.
        assert_eq!(SplitlaneApp::pane_slot_label(0, 1, None), "1st");
    }
}
