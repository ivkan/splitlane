//! Building a flat tree from a list of panes, and flattening one that is not.
//!
//! Two builders used to live here beside `from_panes_equal`: `main_vertical`
//! (a pane on the left, the rest stacked on the right) and `tiled` (tmux's
//! grid). Both are gone with their actions. Neither can be drawn without
//! nesting a second container, and a container's panes are one row or one
//! column now - a preset that quietly produced a grid would make the toolbar's
//! Orientation control, which states one of two answers, wrong.

use std::cell::Cell;
use std::rc::Rc;

use gpui::Entity;

use crate::pane::Pane;

use super::MAX_PANES;
use super::tree::{LayoutChild, LayoutTree, SplitDirection};

impl LayoutTree {
    /// Build a flat container with all panes at equal ratios in the given
    /// direction. `None` for empty, `Leaf` for one pane, `Container` for 2+.
    ///
    /// Panes past [`MAX_PANES`] are dropped rather than laid out: the caller
    /// has already been told (or has already checked) that the cap is three,
    /// and returning a tree that breaks the invariant would push the problem
    /// somewhere it cannot be seen.
    pub fn from_panes_equal(direction: SplitDirection, panes: Vec<Entity<Pane>>) -> Option<Self> {
        let mut panes = panes;
        panes.truncate(MAX_PANES);
        match panes.len() {
            0 => None,
            1 => Some(LayoutTree::Leaf(panes.into_iter().next().unwrap())),
            n => {
                let ratio = 1.0 / n as f32;
                let children = panes
                    .into_iter()
                    .map(|pane| LayoutChild {
                        node: LayoutTree::Leaf(pane),
                        ratio: Rc::new(Cell::new(ratio)),
                    })
                    .collect();
                Some(LayoutTree::Container {
                    direction,
                    children,
                    drag: Rc::new(Cell::new(None)),
                    container_size: Rc::new(Cell::new(0.0)),
                })
            }
        }
    }

    /// Return this tree in the one shape a container may hold: a leaf, or a
    /// single container of leaves, at most [`MAX_PANES`] of them.
    ///
    /// Live trees are flat by construction - every split goes through
    /// `insert_flat`. This exists for the two doors that do not: a
    /// `session.json` written by a build that nested freely, and the IPC
    /// `workspace.restore_layout`, whose payload is whatever the caller sent.
    /// Both arrive as an arbitrary `LayoutNode`, so both are normalized on the
    /// way in rather than being trusted and then rendered.
    ///
    /// The outermost direction is kept, because it is the one the persisted
    /// file (or the caller) actually stated; the ratios are not, because
    /// ratios of a shape that no longer exists mean nothing. Panes past the
    /// cap are dropped.
    pub fn flattened(self) -> Option<Self> {
        // The grid is a legal shape and the only one that nests, so it is
        // recognised here rather than flattened away - and it is **rebuilt**
        // rather than returned as it stands, because the one thing that does
        // not survive a round trip through the schema is the `Rc` the two rows
        // share for their column ratio. Read off disk each ratio arrives in an
        // allocation of its own, so the vertical divider would come back as two
        // that happen to start level and drift apart on the first drag. The
        // shape is the file's; the sharing is this build's to restore.
        if let Some((cols, rows)) = self.grid_ratios() {
            return LayoutTree::grid_of_four_with(self.collect_leaves(), cols, rows);
        }
        let direction = match &self {
            LayoutTree::Leaf(_) => return Some(self),
            LayoutTree::Container { direction, .. } => *direction,
        };
        if self.is_flat() {
            return Some(self);
        }
        LayoutTree::from_panes_equal(direction, self.collect_leaves())
    }

    /// True when this tree is already in the one legal *flat* shape. A grid is
    /// legal and is not flat; [`LayoutTree::flattened`] asks both questions.
    pub fn is_flat(&self) -> bool {
        match self {
            LayoutTree::Leaf(_) => true,
            LayoutTree::Container { children, .. } => {
                children.len() <= MAX_PANES
                    && children
                        .iter()
                        .all(|child| matches!(child.node, LayoutTree::Leaf(_)))
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

    /// The end of the chain `cells_worth_keeping` starts: leaving a grid can
    /// come down to one pane, and one pane has to be a tree. If this ever
    /// answered `None`, a re-orientation would empty the container.
    #[gpui::test]
    fn one_pane_is_a_leaf_and_never_nothing(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let only = test_pane(cx, 1);
        let tree = LayoutTree::from_panes_equal(SplitDirection::Vertical, vec![only.clone()])
            .expect("one pane is a tree");
        assert!(matches!(tree, LayoutTree::Leaf(_)));
        assert_eq!(tree.collect_leaves(), vec![only]);
    }

    #[gpui::test]
    fn from_panes_equal_drops_everything_past_the_cap(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let panes: Vec<_> = (0..5).map(|_| test_pane(cx, 1)).collect();
        let tree = LayoutTree::from_panes_equal(SplitDirection::Vertical, panes)
            .expect("five panes build a tree");
        assert_eq!(tree.leaf_count(), MAX_PANES);
        assert!(tree.is_flat());
    }

    #[gpui::test]
    fn a_nested_tree_flattens_and_keeps_its_outer_direction(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 1);
        let b = test_pane(cx, 1);
        let c = test_pane(cx, 1);
        // The shape an older session.json can hold: a column whose first child
        // is a row.
        let inner = LayoutTree::new_split(
            SplitDirection::Vertical,
            LayoutTree::Leaf(a.clone()),
            LayoutTree::Leaf(b.clone()),
        );
        let nested = LayoutTree::new_split(
            SplitDirection::Horizontal,
            inner,
            LayoutTree::Leaf(c.clone()),
        );
        assert!(!nested.is_flat());

        let flat = nested.flattened().expect("a nested tree flattens");
        assert!(flat.is_flat());
        assert_eq!(flat.leaf_count(), 3);
        assert_eq!(flat.root_direction(), Some(SplitDirection::Horizontal));
        assert_eq!(
            flat.collect_leaves()
                .into_iter()
                .map(|pane| pane.entity_id())
                .collect::<Vec<_>>(),
            vec![a.entity_id(), b.entity_id(), c.entity_id()],
        );
    }

    #[gpui::test]
    fn a_flat_tree_is_returned_untouched(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 1);
        let b = test_pane(cx, 1);
        let flat = LayoutTree::new_split(
            SplitDirection::Vertical,
            LayoutTree::Leaf(a),
            LayoutTree::Leaf(b),
        );
        // A ratio the user dragged to survives, because nothing is rebuilt.
        if let LayoutTree::Container { children, .. } = &flat {
            children[0].ratio.set(0.7);
            children[1].ratio.set(0.3);
        }
        let flat = flat.flattened().expect("already flat");
        match &flat {
            LayoutTree::Container { children, .. } => {
                assert!((children[0].ratio.get() - 0.7).abs() < f32::EPSILON);
            }
            LayoutTree::Leaf(_) => panic!("a two-pane tree is a container"),
        }
    }
}
