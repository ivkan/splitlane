//! Core layout tree types, constants, and ratio-manipulation helpers.
//!
//! Part of the `split.rs` decomposition. The `LayoutTree` enum + its
//! `LayoutChild`/`DragState` supporting types live here; rendering, mutation,
//! navigation, preset, and serialization logic is split into sibling modules.

use std::cell::Cell;
use std::rc::Rc;

use gpui::Entity;

use crate::pane::Pane;

// ---------------------------------------------------------------------------
// Split direction
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    /// Horizontal divider - panes stacked top/bottom (flex_col)
    Horizontal,
    /// Vertical divider - panes side by side (flex_row)
    Vertical,
}

// ---------------------------------------------------------------------------
// Drag state - tracks an in-progress divider drag
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub(crate) struct DragState {
    pub(crate) divider_idx: usize,
    pub(crate) start_pos: f32,
    pub(crate) start_ratio_before: f32,
    pub(crate) start_ratio_after: f32,
}

// ---------------------------------------------------------------------------
// Layout tree - N-ary tree
// ---------------------------------------------------------------------------

pub struct LayoutChild {
    pub node: LayoutTree,
    pub ratio: Rc<Cell<f32>>,
}

pub enum LayoutTree {
    Leaf(Entity<Pane>),
    Container {
        direction: SplitDirection,
        children: Vec<LayoutChild>,
        drag: Rc<Cell<Option<DragState>>>,
        /// Actual main-axis pixel size of this container, captured each frame
        /// via canvas() prepaint. Used for pixel-accurate drag-to-resize.
        container_size: Rc<Cell<f32>>,
    },
}

/// The gap between two slots, and the width of the handle that resizes them.
///
/// The design's number: "Between panes a 10px drag handle". It was 4px of
/// divider with a 7px hit area, which read as a seam rather than as the gap a
/// row of cards leaves between them - and the slot's own radius has nothing to
/// show against a seam.
pub(crate) const DIVIDER_PX: f32 = 10.0;
pub(super) const DIVIDER_HIT_PX: f32 = 10.0;
/// Minimum pane size in pixels. No pane may be dragged below this.
///
/// The design's number ("A pane can never be dragged below 300px"). It was
/// 80, which is narrower than a terminal line of any use and which only made
/// sense next to a cap of 32 panes. Where three panes at 300 will not fit,
/// `resize_adjacent_ratios` keeps the ratios it has rather than fighting the
/// window - a minimum that cannot be honoured is not an excuse to jump.
pub(crate) const MIN_PANE_SIZE: f32 = 300.0;

/// Re-normalize ratios so they sum to 1.0 (proportional scaling).
pub(super) fn normalize_ratios(children: &[LayoutChild]) {
    let sum: f32 = children.iter().map(|c| c.ratio.get()).sum();
    if sum > 0.0 && (sum - 1.0).abs() > f32::EPSILON {
        for child in children {
            child.ratio.set(child.ratio.get() / sum);
        }
    }
}

/// Redistribute a removed child's ratio equally among remaining children.
/// Each sibling gets `removed_ratio / num_remaining` added to its current ratio.
pub(super) fn redistribute_equal(children: &[LayoutChild], removed_ratio: f32) {
    if children.is_empty() {
        return;
    }
    let share = removed_ratio / children.len() as f32;
    for child in children {
        child.ratio.set(child.ratio.get() + share);
    }
}

pub(super) fn resize_adjacent_ratios(
    start_ratio_before: f32,
    start_ratio_after: f32,
    delta_px: f32,
    container_px: f32,
    min_before_px: f32,
    min_after_px: f32,
) -> Option<(f32, f32)> {
    if !start_ratio_before.is_finite()
        || !start_ratio_after.is_finite()
        || !delta_px.is_finite()
        || !container_px.is_finite()
        || !min_before_px.is_finite()
        || !min_after_px.is_finite()
        || container_px <= 0.0
    {
        return None;
    }

    let pair_sum = start_ratio_before + start_ratio_after;
    if !pair_sum.is_finite() || pair_sum <= 0.0 {
        return None;
    }

    let min_before_ratio = (min_before_px.max(0.0) / container_px).min(pair_sum);
    let min_after_ratio = (min_after_px.max(0.0) / container_px).min(pair_sum);
    let lower = min_before_ratio;
    let upper = pair_sum - min_after_ratio;
    if lower > upper {
        return Some((start_ratio_before, start_ratio_after));
    }

    let new_before = (start_ratio_before + delta_px / container_px).clamp(lower, upper);
    Some((new_before, pair_sum - new_before))
}

/// Insert a new pane as a sibling after `children[idx]`.
/// The new child steals half of the target child's ratio.
///
/// # Panics
/// Panics in debug builds if `idx >= children.len()`.
pub(super) fn insert_sibling(children: &mut Vec<LayoutChild>, idx: usize, new_pane: Entity<Pane>) {
    debug_assert!(idx < children.len(), "insert_sibling: idx out of bounds");
    // Fail-safe on a stale index - `.get()` instead of `children[idx]`,
    // which would panic in release. Halve the target's ratio inside a scoped
    // borrow so the borrow drops before the `children.insert` below.
    let half = {
        let Some(target) = children.get(idx) else {
            return;
        };
        let old_ratio = target.ratio.get();
        debug_assert!(old_ratio.is_finite(), "insert_sibling: ratio is NaN/inf");
        let half = if old_ratio.is_finite() {
            old_ratio / 2.0
        } else {
            0.5
        };
        target.ratio.set(half);
        half
    };
    children.insert(
        idx + 1,
        LayoutChild {
            node: LayoutTree::Leaf(new_pane),
            ratio: Rc::new(Cell::new(half)),
        },
    );
}

impl LayoutTree {
    /// Create a new 2-child container with 50/50 ratios.
    pub(super) fn new_split(
        direction: SplitDirection,
        first: LayoutTree,
        second: LayoutTree,
    ) -> Self {
        LayoutTree::Container {
            direction,
            children: vec![
                LayoutChild {
                    node: first,
                    ratio: Rc::new(Cell::new(0.5)),
                },
                LayoutChild {
                    node: second,
                    ratio: Rc::new(Cell::new(0.5)),
                },
            ],
            drag: Rc::new(Cell::new(None)),
            container_size: Rc::new(Cell::new(0.0)),
        }
    }

    pub(super) fn min_main_axis_px(&self, axis: SplitDirection) -> f32 {
        match self {
            LayoutTree::Leaf(_) => MIN_PANE_SIZE,
            LayoutTree::Container {
                direction,
                children,
                ..
            } => {
                if children.is_empty() {
                    return MIN_PANE_SIZE;
                }

                if *direction == axis {
                    let children_min: f32 = children
                        .iter()
                        .map(|child| child.node.min_main_axis_px(axis))
                        .sum();
                    children_min + DIVIDER_PX * children.len().saturating_sub(1) as f32
                } else {
                    children
                        .iter()
                        .map(|child| child.node.min_main_axis_px(axis))
                        .fold(MIN_PANE_SIZE, f32::max)
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

    #[test]
    fn resize_adjacent_ratios_preserves_pair_sum() {
        let (before, after) =
            resize_adjacent_ratios(0.3, 0.4, 50.0, 500.0, 80.0, 80.0).expect("valid resize");

        assert!((before - 0.4).abs() < f32::EPSILON);
        assert!((after - 0.3).abs() < f32::EPSILON);
        assert!((before + after - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn resize_adjacent_ratios_clamps_asymmetric_minimums() {
        let (before, after) =
            resize_adjacent_ratios(0.3, 0.4, -500.0, 500.0, 160.0, 80.0).expect("valid resize");

        assert!((before - 0.32).abs() < 0.0001);
        assert!((after - 0.38).abs() < 0.0001);
    }

    #[test]
    fn resize_adjacent_ratios_keeps_ratios_when_minimums_are_impossible() {
        let (before, after) =
            resize_adjacent_ratios(0.3, 0.4, 50.0, 200.0, 160.0, 160.0).expect("valid resize");

        assert!((before - 0.3).abs() < f32::EPSILON);
        assert!((after - 0.4).abs() < f32::EPSILON);
    }

    #[gpui::test]
    fn min_main_axis_sums_matching_direction_subtrees(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 1);
        let b = test_pane(cx, 1);
        let c = test_pane(cx, 1);
        let tree = LayoutTree::Container {
            direction: SplitDirection::Vertical,
            children: vec![
                LayoutChild {
                    node: LayoutTree::Leaf(a),
                    ratio: Rc::new(Cell::new(0.33)),
                },
                LayoutChild {
                    node: LayoutTree::Leaf(b),
                    ratio: Rc::new(Cell::new(0.33)),
                },
                LayoutChild {
                    node: LayoutTree::Leaf(c),
                    ratio: Rc::new(Cell::new(0.34)),
                },
            ],
            drag: Rc::new(Cell::new(None)),
            container_size: Rc::new(Cell::new(0.0)),
        };

        // Three panes at the 300px minimum plus the two dividers between them.
        let expected = MIN_PANE_SIZE * 3.0 + DIVIDER_PX * 2.0;
        assert!((tree.min_main_axis_px(SplitDirection::Vertical) - expected).abs() < f32::EPSILON);
    }

    #[gpui::test]
    fn min_main_axis_uses_max_for_cross_direction_subtrees(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let a = test_pane(cx, 1);
        let b = test_pane(cx, 1);
        let c = test_pane(cx, 1);
        let stacked = LayoutTree::new_split(
            SplitDirection::Horizontal,
            LayoutTree::Leaf(a),
            LayoutTree::Leaf(b),
        );
        let tree = LayoutTree::new_split(SplitDirection::Vertical, stacked, LayoutTree::Leaf(c));

        // Two panes on the main axis; the stacked pair contributes the wider
        // of its own two, not their sum.
        let expected = MIN_PANE_SIZE * 2.0 + DIVIDER_PX;
        assert!((tree.min_main_axis_px(SplitDirection::Vertical) - expected).abs() < f32::EPSILON);
        assert!(
            (tree.min_main_axis_px(SplitDirection::Horizontal) - expected).abs() < f32::EPSILON
        );
    }
}
