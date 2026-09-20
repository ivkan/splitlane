//! Layout presets, JSON layout application, zoom, and split-equalize.
//!
//! Part of the workspace_ops decomposition.

use std::collections::VecDeque;
use std::path::PathBuf;

use gpui::{Context, Entity, Focusable, Window};
use splitlane_config::schema::LayoutNode;

use crate::layout::{LayoutTree, MAX_PANES, SplitDirection};
use crate::pane::Pane;
use crate::{LayoutEvenHorizontal, LayoutEvenVertical, SplitEqualize, SplitlaneApp, ToggleZoom};

/// What a layout control says when the panes are not on screen at all.
///
/// One home, because two controls meet the same wall: with Settings open the
/// content area is the application layer and the container's slots are behind
/// it, so a re-orientation would mutate a tree the person cannot see.
const LAYOUT_OFF_SCREEN: &str = "This surface fills the area - it has no slots to arrange";

impl SplitlaneApp {
    pub(crate) fn handle_toggle_zoom(
        &mut self,
        _: &ToggleZoom,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.active_workspace() else {
            return;
        };

        if ws.is_zoomed() {
            let Some(ws) = self.active_workspace_mut() else {
                return;
            };
            if let Some(pane) = ws.exit_zoom(cx) {
                pane.read(cx).focus_handle(cx).focus(window, cx);
            }
        } else {
            // Zoom: save the full tree, replace root with the focused pane
            if ws.root.as_ref().is_none_or(|root| root.leaf_count() <= 1) {
                return;
            }

            // The same question `split` asks, and for the same reason: zoom
            // with focus on the rail used to do nothing at all, without even a
            // sentence. Resolved before the container is borrowed mutably,
            // because the answer is the app's rather than the tree's.
            let Some(focused) = self.focused_pane_as_shown(window, cx) else {
                return;
            };

            focused.update(cx, |p, _| p.zoomed = true);
            let Some(ws) = self.active_workspace_mut() else {
                return;
            };
            let Some(full_tree) = ws.root.take() else {
                return;
            };
            ws.saved_layout = Some(full_tree);
            ws.root = Some(LayoutTree::Leaf(focused.clone()));
            focused.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Apply a layout preset: collect all panes, rebuild tree with the given
    /// factory.
    ///
    /// **Leaving a grid drops the cells the grid filled in.** See
    /// [`crate::layout::cells_worth_keeping`] for why that belongs to the form
    /// rather than to this control: the short version is that a grid of two
    /// sessions turned side by side used to become a row of four, and the next
    /// launch brought it back as a row of two, because restore has applied this
    /// exact rule all along (`prune_empty_panes`).
    fn apply_layout_preset(
        &mut self,
        build: impl FnOnce(Vec<Entity<Pane>>) -> Option<LayoutTree>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The same guard `handle_layout_grid` carries, in the one place both
        // arrive at: with Settings open there are no slots on screen, and the
        // action is reachable from the Shortcuts screen itself. Rearranging a
        // tree nobody can see is the one outcome no press should have.
        if !self.panes_surface_visible() {
            self.show_toast(LAYOUT_OFF_SCREEN, cx);
            return;
        }
        if let Some(ws) = self.active_workspace_mut() {
            ws.exit_zoom(cx);
        }

        // Read before taking `&mut`: which panes survive is a question about
        // the panes themselves, and answering it needs the `App` the workspace
        // borrow would hold.
        let Some((leaves, leaving_a_grid)) = self
            .active_workspace()
            .and_then(|ws| ws.root.as_ref())
            .map(|root| (root.collect_leaves(), root.is_grid()))
        else {
            return;
        };

        // No-op for single pane
        if leaves.len() <= 1 {
            return;
        }

        let panes = if leaving_a_grid {
            crate::layout::cells_worth_keeping(leaves, |pane| pane.read(cx).tabs.is_empty())
        } else {
            leaves
        };

        let Some(ws) = self.active_workspace_mut() else {
            return;
        };
        // Rebuilt from the leaves, so assigning drops the old tree and the
        // panes it named live on in `panes`.
        //
        // Only a tree that exists is assigned. `build` is a closure, and the
        // one input it cannot make a tree out of is an empty list - which the
        // two guards above rule out (a grid keeps at least one cell, and
        // anything else arrived with two or more). Emptying the container is
        // the one outcome a re-orientation may not have, so it is refused
        // here rather than proved at each caller.
        let Some(tree) = build(panes) else {
            return;
        };
        ws.root = Some(tree);
        if let Some(ref r) = ws.root {
            r.focus_first(window, cx);
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Apply a layout from a `LayoutNode` (deserialized JSON) to the active workspace.
    ///
    /// Handles pane count mismatch: spawns new panes when the layout has more
    /// leaves than available, drops extras when fewer. Exits zoom first.
    pub(crate) fn apply_layout_from_json(
        &mut self,
        layout: &mut LayoutNode,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // Validate the layout (clamps ratios, pads children, etc.)
        splitlane_config::loader::validate_layout(layout);

        let needed = layout.leaf_count();
        if needed == 0 {
            return Err("Layout has no panes".into());
        }
        if needed > MAX_PANES {
            return Err(format!("Layout exceeds maximum pane count ({MAX_PANES})"));
        }

        if let Some(ws) = self.active_workspace_mut() {
            ws.exit_zoom(cx);
        }

        let Some(ws) = self.active_workspace_mut() else {
            return Err("No active workspace".into());
        };

        // Collect existing panes and drop the old tree
        let existing: Vec<Entity<Pane>> = ws
            .root
            .take()
            .map(|r| r.collect_leaves())
            .unwrap_or_default();

        // Keep only the panes we need; extras are dropped with the old tree
        let mut pane_deque: VecDeque<Entity<Pane>> = existing.into_iter().take(needed).collect();

        let ws_id = ws.id;
        let fallback_cwd = PathBuf::from(&ws.cwd);
        // No agent surfaces to place: an applied layout describes shells and
        // markdown, and any agent already in the tree keeps the pane it is in
        // (the panes above are reused, not rebuilt).
        let agent_views = std::collections::HashMap::new();
        // Normalized: `workspace.restore_layout` takes whatever its caller
        // sent, and a container holds one row or one column.
        let tree = LayoutTree::from_layout_node_normalized(layout, &mut pane_deque, &mut |node| {
            let surfaces = match node {
                LayoutNode::Pane { surfaces } => surfaces.as_slice(),
                _ => &[],
            };
            SplitlaneApp::spawn_pane_from_surfaces(ws_id, surfaces, &fallback_cwd, &agent_views, cx)
        });
        let Some(tree) = tree else {
            return Err("Layout has no panes".into());
        };

        let Some(ws) = self.active_workspace_mut() else {
            return Err("No active workspace".into());
        };
        ws.root = Some(tree);
        // A diff pane this layout spawned needs its app-side host, exactly as
        // one restored at launch does (`bootstrap.rs`): without it the pane's
        // file column reads "No Git repository" beside the diff it describes.
        if self.active_container_shows_a_diff(cx) {
            self.rebuild_diff_view(cx);
        }
        self.save_session(cx);
        cx.notify();

        Ok(())
    }

    pub(crate) fn handle_layout_even_h(
        &mut self,
        _: &LayoutEvenHorizontal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_layout_preset(
            |panes| LayoutTree::from_panes_equal(SplitDirection::Vertical, panes),
            window,
            cx,
        );
    }

    pub(crate) fn handle_layout_even_v(
        &mut self,
        _: &LayoutEvenVertical,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_layout_preset(
            |panes| LayoutTree::from_panes_equal(SplitDirection::Horizontal, panes),
            window,
            cx,
        );
    }

    /// The third form: two rows of two.
    ///
    /// It does not go through [`Self::apply_layout_preset`], and the reason is
    /// the one thing that makes this form different from the other two. A row
    /// and a column are arrangements *of the panes there are*; a grid is a
    /// shape with four cells, and choosing it with fewer panes open lays the
    /// open ones into cells and **fills the rest with the launcher** - exactly
    /// what `Add pane` does, for the same reason: an empty pane is an
    /// invitation, not a hole. Making those panes needs a `Context`, which the
    /// preset builder is a closure precisely in order not to have.
    ///
    /// Refused where a 2x2 does not fit, in the same words the dimmed segment
    /// carries - one question, one sentence, however it is asked.
    pub(crate) fn handle_layout_grid(
        &mut self,
        _: &crate::LayoutGrid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.panes_surface_visible() {
            self.show_toast(LAYOUT_OFF_SCREEN, cx);
            return;
        }
        if !self.grid_fits_now() {
            self.show_toast(crate::app::targeting::GRID_REFUSAL, cx);
            return;
        }
        if let Some(ws) = self.active_workspace_mut() {
            ws.exit_zoom(cx);
        }
        let Some(mut panes) = self
            .active_workspace()
            .and_then(|ws| ws.root.as_ref())
            .map(|root| root.collect_leaves())
        else {
            return;
        };
        // Already four in two rows of two: pressing the lit segment is not a
        // rebuild. Rebuilding would re-share the ratio cells and snap the
        // dividers the person had dragged back to 50/50.
        if self
            .active_workspace()
            .and_then(|ws| ws.root.as_ref())
            .is_some_and(|root| root.is_grid())
        {
            return;
        }
        // Panes past the fourth are not dropped - they are what the person is
        // working in. There cannot be more than four (`MAX_PANES`), and if a
        // file written by some other build ever produced more, taking the
        // first four is what every other door here does.
        panes.truncate(crate::layout::GRID_CELLS);
        while panes.len() < crate::layout::GRID_CELLS {
            panes.push(self.create_pane_with_existing_tabs(Vec::new(), 0, cx));
        }
        let Some(grid) = LayoutTree::grid_of_four(panes) else {
            return;
        };
        let Some(ws) = self.active_workspace_mut() else {
            return;
        };
        ws.root = Some(grid);
        if let Some(root) = ws.root.as_ref() {
            root.focus_first(window, cx);
        }
        self.save_session(cx);
        cx.notify();
    }

    /// The design's `\u{2325}\`: one more pane, up to the limit.
    ///
    /// **It used to toggle** - a split from one pane, a collapse to the focused
    /// one from several - and that was taken apart. Two reasons, and the
    /// second is the one that made it urgent. One name is one action: a control
    /// whose meaning depends on the number of panes is a mode, and this app has
    /// none. And the meaning it took on from three panes was closing two live
    /// sessions, which is the only destructive act in this interface that had no
    /// name of its own - a person learns what a gesture does by making it, and
    /// this one taught by destroying.
    ///
    /// The generalisation was ours rather than the design's. What the design
    /// wrote was about the **empty** pane a split had just made ("`Esc` or a
    /// second `\u{2325}\` closes it again, and because nothing was opened there is
    /// nothing to undo"), and the safety was entirely in the word *empty*.
    /// Undoing a pane you just added is still `Esc` in it, or `\u{2318}\u{21e7}T`;
    /// collapsing on purpose is [`Self::collapse_to_pane`], from the pane's own
    /// `\u{22ef}` menu, where the target is named by where the menu was opened.
    ///
    /// **The refusal is not spoken here**, because `split` already speaks it:
    /// a dimmed button carries its reason in a tooltip, and a chord, having
    /// nowhere to put one, toasts. Both sentences come from
    /// [`crate::app::targeting::PaneRefusal::sentence`], so the keyboard and
    /// the mouse cannot come to disagree about what the limit is.
    pub(crate) fn handle_add_pane(
        &mut self,
        _: &crate::AddPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The diff, the launcher and the Skills page take the whole area and
        // have no slot of their own to split from. Say so rather than mutate a
        // tree off screen.
        if !self.panes_surface_visible() {
            self.show_toast("This surface fills the area - it has no slot to split", cx);
            return;
        }
        let Some(ws) = self.active_workspace() else {
            return;
        };
        let Some(root) = &ws.root else { return };
        // The container keeps the way it is already arranged; side by side is
        // the design's own default for a second pane.
        let direction = root.root_direction().unwrap_or(SplitDirection::Vertical);
        // Focus stays where `split` put it: on the new half. The rule this
        // replaces sent it to the FIRST pane, because the restored pair was "a
        // shape coming back, not a surface the user asked to see" - true while
        // the second half arrived holding a shell. Now it arrives
        // holding the launcher, so the new half is a question addressed to the
        // user, and answering it on the keyboard means being in it.
        self.split(direction, window, cx);
    }

    /// Close every pane of `keep`'s container except `keep`.
    ///
    /// **The target is named, not inferred**, and that is the whole reason this
    /// is a row in a pane's own `\u{22ef}` menu rather than a toolbar button or a
    /// chord. A menu opened from a pane header has already said which pane it
    /// means by being where it is; a button would have to mean "the focused
    /// one", which is exactly the implicit target that made the old `\u{2325}\`
    /// surprising. It is rare and it is destructive, so it gets no chord either
    /// - one slip away from the gesture that adds is the wrong place for it.
    ///
    /// A page of reasoning about *which* pane to keep went with the inference,
    /// and so did the two cross-vendor findings that had corrected it. The one
    /// rule that survives is a fact about the caller rather than a choice made
    /// here: an empty pane draws no `\u{22ef}` at all ("no status word, no context
    /// and no `\u{22ef}` menu either"), so the launcher cannot be `keep`.
    ///
    /// Closing really closes - the same path the slot header's `\u{d7}` takes, so
    /// an agent surface is parked rather than lost and every closed pane lands
    /// on the undo stack (`\u{2318}\u{21e7}T`). What it is NOT is zoom: `toggle_zoom`
    /// hides the tree and keeps it.
    pub(crate) fn collapse_to_pane(
        &mut self,
        keep: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace_idx) = self.workspaces.iter().position(|ws| ws.contains_pane(keep))
        else {
            return;
        };
        if let Some(ws) = self.workspaces.get_mut(workspace_idx) {
            ws.exit_zoom(cx);
        }
        let Some(ws) = self.workspaces.get(workspace_idx) else {
            return;
        };
        let Some(root) = &ws.root else { return };
        let doomed: Vec<Entity<Pane>> = root
            .collect_leaves()
            .into_iter()
            .filter(|pane| pane != keep)
            .collect();
        if doomed.is_empty() {
            return;
        }
        for pane in &doomed {
            if let Some(record) = super::capture_closed_pane_record(pane, workspace_idx, cx) {
                super::push_closed_pane_record(&mut self.closed_panes, record);
            }
        }
        let Some(ws) = self.workspaces.get_mut(workspace_idx) else {
            return;
        };
        for pane in &doomed {
            let Some(root) = ws.root.take() else { break };
            let (new_root, _removed) = root.remove_pane(pane);
            ws.root = new_root;
        }
        keep.read(cx).focus_handle(cx).focus(window, cx);
        self.save_session(cx);
        cx.notify();
    }

    pub(crate) fn handle_split_equalize(
        &mut self,
        _: &SplitEqualize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ws) = self.active_workspace_mut()
            && let Some(ref root) = ws.root
        {
            root.equalize_ratios();
            self.save_session(cx);
            cx.notify();
        }
    }
}
