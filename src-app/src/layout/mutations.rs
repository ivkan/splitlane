//! Tree-growing mutations: split, swap.
//!
//! Every split lands here, and every split obeys the two rules the module doc
//! states: at most [`MAX_PANES`] leaves, and one container whose children are
//! all leaves.
//!
//! The tree used to nest a fresh container whenever the requested direction
//! differed from the parent's, which is how an N-ary tree grows and is exactly
//! what this app's panes do not have. So a split into a container that
//! already exists **appends into it and keeps its orientation**: the direction
//! argument decides the arrangement only when there is no arrangement yet
//! (going from one pane to two). Changing the arrangement afterwards is the
//! Orientation control's job, which is a different question and has its own
//! button.

use gpui::{App, Entity, Window};

use crate::pane::Pane;

use super::MAX_PANES;
use super::tree::{LayoutTree, SplitDirection, insert_sibling};

impl LayoutTree {
    /// Insert `new_pane` beside `target` (or beside the first leaf when
    /// `target` is `None`), keeping the tree flat and inside the cap.
    ///
    /// Returns `false` when the cap is already reached or `target` is not a
    /// leaf of this tree - the two cases a caller has to be able to tell from
    /// "done", because one of them means "say so to the user".
    fn insert_flat(
        &mut self,
        target: Option<&Entity<Pane>>,
        direction: SplitDirection,
        new_pane: Entity<Pane>,
    ) -> bool {
        // The cap is enforced HERE and not only at the call sites. Seven
        // places grow this tree (two split chords, the pane buttons, a tab
        // drag, the launch pad, the IPC `surface.split`, moving an agent
        // surface in); a bound that lives in all seven is a bound that one of
        // them will forget.
        if self.leaf_count() >= MAX_PANES {
            return false;
        }
        match self {
            LayoutTree::Leaf(pane) => {
                if target.is_some_and(|wanted| pane != wanted) {
                    return false;
                }
                // The one place a container is born, and the one place the
                // direction argument is heard.
                let old = std::mem::replace(self, LayoutTree::Leaf(new_pane.clone()));
                *self = LayoutTree::new_split(direction, old, LayoutTree::Leaf(new_pane));
                true
            }
            LayoutTree::Container { children, .. } => {
                if children.is_empty() {
                    return false;
                }
                let idx = match target {
                    Some(wanted) => children.iter().position(
                        |child| matches!(&child.node, LayoutTree::Leaf(pane) if pane == wanted),
                    ),
                    None => Some(0),
                };
                let Some(idx) = idx else {
                    return false;
                };
                // Never a grid: a grid is four cells, which is the cap, and its
                // children are rows rather than leaves. So resetting every
                // share here cannot reach the grid's own shared ratios.
                insert_sibling(children, idx, new_pane);
                true
            }
        }
    }

    /// Split the focused pane. See [`LayoutTree::insert_flat`] for the shape
    /// rules; `false` means nothing was inserted (no focused pane here, or the
    /// container is already full).
    pub fn split_at_focused(
        &mut self,
        direction: SplitDirection,
        new_pane: Entity<Pane>,
        window: &Window,
        cx: &App,
    ) -> bool {
        let Some(focused) = self.focused_pane(window, cx) else {
            return false;
        };
        self.insert_flat(Some(&focused), direction, new_pane)
    }

    /// Split beside the first (leftmost/topmost) leaf. Used by the IPC handler
    /// and the launch pad, where there is no focus to read.
    pub fn split_first_leaf(&mut self, direction: SplitDirection, new_pane: Entity<Pane>) -> bool {
        self.insert_flat(None, direction, new_pane)
    }

    /// Split at a specific pane entity (identified by `Entity` identity, not
    /// focus). Used when the split request comes from a button on the pane
    /// itself, or from a drop onto it.
    pub fn split_at_pane(
        &mut self,
        target: &Entity<Pane>,
        direction: SplitDirection,
        new_pane: Entity<Pane>,
    ) -> bool {
        self.insert_flat(Some(target), direction, new_pane)
    }

    /// Swap two pane entities in the tree. Ratios and tree shape are preserved.
    ///
    /// Returns `false` if either pane is absent so stale handles cannot replace
    /// a live leaf with a closed pane.
    pub fn swap_panes(&mut self, a: &Entity<Pane>, b: &Entity<Pane>) -> bool {
        if a == b || !self.contains_leaf(a) || !self.contains_leaf(b) {
            return false;
        }
        self.swap_panes_unchecked(a, b);
        true
    }

    fn swap_panes_unchecked(&mut self, a: &Entity<Pane>, b: &Entity<Pane>) {
        match self {
            LayoutTree::Leaf(pane) => {
                if pane == a {
                    *pane = b.clone();
                } else if pane == b {
                    *pane = a.clone();
                }
            }
            LayoutTree::Container { children, .. } => {
                for child in children {
                    child.node.swap_panes_unchecked(a, b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, Entity, TestAppContext};

    use crate::pane::Pane;
    use crate::terminal::TerminalView;

    use super::*;

    fn test_pane(cx: &mut impl AppContext, workspace_id: u64) -> Entity<Pane> {
        let terminal = cx.new(|cx| TerminalView::display_only_for_test(workspace_id, cx));
        cx.new(|cx| Pane::new(terminal, cx))
    }

    fn leaf_ids(tree: &LayoutTree) -> Vec<gpui::EntityId> {
        tree.collect_leaves()
            .into_iter()
            .map(|pane| pane.entity_id())
            .collect()
    }

    fn child_ratios(tree: &LayoutTree) -> Vec<f32> {
        match tree {
            LayoutTree::Container { children, .. } => {
                children.iter().map(|child| child.ratio.get()).collect()
            }
            LayoutTree::Leaf(_) => Vec::new(),
        }
    }

    #[gpui::test]
    fn split_at_pane_inserts_sibling_for_matching_direction(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 1);
        let b = test_pane(cx, 1);
        let c = test_pane(cx, 1);
        let mut tree = LayoutTree::new_split(
            SplitDirection::Vertical,
            LayoutTree::Leaf(a.clone()),
            LayoutTree::Leaf(b.clone()),
        );

        assert!(tree.split_at_pane(&a, SplitDirection::Vertical, c.clone()));

        assert_eq!(tree.leaf_count(), 3);
        assert_eq!(
            leaf_ids(&tree),
            vec![a.entity_id(), c.entity_id(), b.entity_id()]
        );
        let ratios = child_ratios(&tree);
        assert_eq!(ratios.len(), 3);
        for ratio in ratios {
            assert!((ratio - 1.0 / 3.0).abs() < f32::EPSILON);
        }
    }

    /// Adding a pane is a new arrangement, so a divider the person dragged
    /// before it does not survive: 70/30 plus one is thirds, not 70/15/15.
    #[gpui::test]
    fn adding_a_pane_evens_out_a_dragged_divider(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 6);
        let b = test_pane(cx, 6);
        let c = test_pane(cx, 6);
        let mut tree = LayoutTree::new_split(
            SplitDirection::Vertical,
            LayoutTree::Leaf(a.clone()),
            LayoutTree::Leaf(b.clone()),
        );
        if let LayoutTree::Container { children, .. } = &tree {
            children[0].ratio.set(0.7);
            children[1].ratio.set(0.3);
        }

        assert!(tree.split_at_pane(&b, SplitDirection::Vertical, c));

        let ratios = child_ratios(&tree);
        assert_eq!(ratios.len(), 3);
        for ratio in ratios {
            assert!((ratio - 1.0 / 3.0).abs() < f32::EPSILON);
        }
    }

    #[gpui::test]
    fn a_cross_direction_split_appends_instead_of_nesting(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 2);
        let b = test_pane(cx, 2);
        let c = test_pane(cx, 2);
        let mut tree = LayoutTree::new_split(
            SplitDirection::Vertical,
            LayoutTree::Leaf(a.clone()),
            LayoutTree::Leaf(b.clone()),
        );

        // The other direction. This used to wrap `a` in a second container;
        // now it lands beside `a` in the one container there is, and the
        // arrangement stays what it was.
        assert!(tree.split_at_pane(&a, SplitDirection::Horizontal, c.clone()));

        assert!(tree.is_flat());
        assert_eq!(tree.root_direction(), Some(SplitDirection::Vertical));
        assert_eq!(
            leaf_ids(&tree),
            vec![a.entity_id(), c.entity_id(), b.entity_id()]
        );
    }

    #[gpui::test]
    fn a_split_past_the_cap_is_refused(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let panes: Vec<_> = (0..super::MAX_PANES).map(|_| test_pane(cx, 5)).collect();
        let mut tree = LayoutTree::from_panes_equal(SplitDirection::Vertical, panes.clone())
            .expect("a full container");
        let extra = test_pane(cx, 5);

        assert!(!tree.split_at_pane(&panes[0], SplitDirection::Vertical, extra.clone()));
        assert!(!tree.split_first_leaf(SplitDirection::Vertical, extra));
        assert_eq!(tree.leaf_count(), super::MAX_PANES);
    }

    #[gpui::test]
    fn swap_panes_refuses_absent_source(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 3);
        let b = test_pane(cx, 3);
        let stale = test_pane(cx, 3);
        let mut tree = LayoutTree::new_split(
            SplitDirection::Vertical,
            LayoutTree::Leaf(a.clone()),
            LayoutTree::Leaf(b.clone()),
        );

        assert!(!tree.swap_panes(&stale, &b));
        assert_eq!(leaf_ids(&tree), vec![a.entity_id(), b.entity_id()]);
    }

    #[gpui::test]
    fn swap_panes_swaps_when_both_exist(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 4);
        let b = test_pane(cx, 4);
        let mut tree = LayoutTree::new_split(
            SplitDirection::Vertical,
            LayoutTree::Leaf(a.clone()),
            LayoutTree::Leaf(b.clone()),
        );

        assert!(tree.swap_panes(&a, &b));
        assert_eq!(leaf_ids(&tree), vec![b.entity_id(), a.entity_id()]);
    }
}
