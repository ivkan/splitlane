//! The third named form: **four panes in two rows of two**.
//!
//! ## Why it exists
//!
//! Not as a fourth cosmetic option. `MAX_PANES` had been four for some time
//! and the screen never gave it: on a 14" laptop (1512pt, ~240pt rail) three
//! panes side by side are ~50 columns against a TUI laid out for 80, so the
//! **live** ceiling has been two panes and nobody wrote that down. In a 2x2
//! each cell is as wide as one of two side-by-side panes (~75 columns) and pays
//! in height (~25 rows). Row-or-column is the choice of which axis to sacrifice
//! entirely - honest at two panes, dishonest at four, because neither axis
//! survives being quartered. The grid halves both and pays half on each.
//!
//! ## How it is represented, and why that shape
//!
//! A real nested tree (an outer column of two rows, each row a pair of leaves)
//! and **not** a flag on a flat container. Three jobs come out free that way.
//! [`LayoutTree::render`] already recurses, so there is no second renderer.
//! `serde` already round-trips a recursive `LayoutNode`, so nothing is added to
//! the schema. And `focus_in_direction` is already spatial (it lays the tree
//! out into unit rectangles and picks by centre distance), so `Alt+Arrow` is
//! two-dimensional here without a line of new navigation code - there was
//! already a test proving it on exactly this shape.
//!
//! **The two column ratios are one `Rc<Cell<f32>>`, shared by both rows.** That
//! is what makes the vertical divider *one* divider rather than two that happen
//! to start level: dragging either half writes the shared cell and both rows
//! move. Drawn, it is a cross - one vertical rule interrupted at the crossing by
//! the horizontal one, which is what a 2x2's dividers look like. The design
//! asks for exactly this ("one shared horizontal divider and one shared
//! vertical, so the grid cannot be dragged into a 2-over-1"), and sharing an
//! `Rc` is the whole of the mechanism.
//!
//! The sharing is an **invariant established by the constructor**, not part of
//! [`LayoutTree::is_grid`]: the predicate is about shape alone, so a tree read
//! off disk - where every ratio arrives in a fresh `Rc` - is recognised as a
//! grid and then rebuilt with the sharing restored ([`LayoutTree::flattened`]).
//! A predicate that asked about `Rc::ptr_eq` would refuse to recognise the very
//! file it needs to repair.
//!
//! ## Four cells, always
//!
//! A grid has exactly four children, launchers included: choosing `Grid` with
//! fewer panes open lays the open ones into cells and fills the rest with the
//! launcher, which is what `Add pane` does and for the same reason - an empty
//! pane is an invitation. It follows that `Add pane` inside a grid is at the
//! ceiling and refuses, and that **closing a cell leaves the launcher in it**
//! rather than removing it: a hole is not a shape this app draws, and dropping
//! to a 2-over-1 would be the layout rearranging itself unasked, which is the
//! one thing the grid's design forbids outright.

use std::cell::Cell;
use std::rc::Rc;

use gpui::Entity;

use crate::pane::Pane;

use super::tree::{LayoutChild, LayoutTree, SplitDirection};

/// How a container's panes are arranged, as the toolbar's segmented control
/// states it.
///
/// Distinct from [`SplitDirection`], which names **a divider**. A grid has two
/// dividers, so no direction names it, and a third `SplitDirection` variant
/// would have been a word meaning something else in the same type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneForm {
    /// Panes side by side. `SplitDirection::Vertical`.
    Row,
    /// Panes stacked. `SplitDirection::Horizontal`.
    Column,
    /// Two rows of two.
    Grid,
}

/// The number of cells a grid has, which is also [`super::MAX_PANES`]. Named
/// separately because the two are the same number for different reasons: four
/// is the ceiling because there are four kinds a pane can be of, and a grid has
/// four cells because it is 2x2.
pub(crate) const GRID_CELLS: usize = 4;

impl LayoutTree {
    /// True for the grid shape: an outer column of exactly two rows, each a
    /// pair of leaves.
    ///
    /// Shape only - see the module note on why the ratio sharing is deliberately
    /// not part of this question.
    pub fn is_grid(&self) -> bool {
        let LayoutTree::Container {
            direction: SplitDirection::Horizontal,
            children: rows,
            ..
        } = self
        else {
            return false;
        };
        rows.len() == 2 && rows.iter().all(|row| is_grid_row(&row.node))
    }

    /// Which of the three named forms this tree is in, or `None` for a single
    /// pane - which has no arrangement to state, a different answer from any of
    /// the three.
    pub fn root_form(&self) -> Option<PaneForm> {
        if self.is_grid() {
            return Some(PaneForm::Grid);
        }
        match self {
            LayoutTree::Leaf(_) => None,
            LayoutTree::Container {
                direction: SplitDirection::Vertical,
                ..
            } => Some(PaneForm::Row),
            LayoutTree::Container {
                direction: SplitDirection::Horizontal,
                ..
            } => Some(PaneForm::Column),
        }
    }

    /// The two numbers a grid has: the left column's share of the width and the
    /// top row's share of the height. `None` when this is not a grid.
    ///
    /// Read off the first row, because the second row's column ratio is the
    /// same cell - that is the invariant this form is built on.
    pub(crate) fn grid_ratios(&self) -> Option<(f32, f32)> {
        if !self.is_grid() {
            return None;
        }
        let LayoutTree::Container { children: rows, .. } = self else {
            return None;
        };
        let top = rows.first()?;
        let LayoutTree::Container {
            children: cells, ..
        } = &top.node
        else {
            return None;
        };
        Some((cells.first()?.ratio.get(), top.ratio.get()))
    }

    /// Build the grid from up to [`GRID_CELLS`] panes at 50/50.
    ///
    /// The caller pads: a grid always has four cells, and the panes that fill
    /// the spare ones are the caller's to create because they are launcher
    /// panes and this module cannot reach a `Context`.
    pub fn grid_of_four(panes: Vec<Entity<Pane>>) -> Option<Self> {
        Self::grid_of_four_with(panes, 0.5, 0.5)
    }

    /// The same, keeping ratios a person already dragged.
    ///
    /// Used by [`LayoutTree::flattened`] to repair a grid read off disk, where
    /// the shape survives serialization and the `Rc` sharing does not.
    pub(crate) fn grid_of_four_with(
        panes: Vec<Entity<Pane>>,
        col_ratio: f32,
        row_ratio: f32,
    ) -> Option<Self> {
        if panes.len() != GRID_CELLS {
            return None;
        }
        let col_ratio = sane_ratio(col_ratio);
        let row_ratio = sane_ratio(row_ratio);
        // The one cell both rows read. Everything this form promises about its
        // vertical divider is this line.
        let left = Rc::new(Cell::new(col_ratio));
        let right = Rc::new(Cell::new(1.0 - col_ratio));

        let mut panes = panes.into_iter();
        let mut row = |ratio: f32| -> Option<LayoutChild> {
            let a = panes.next()?;
            let b = panes.next()?;
            Some(LayoutChild {
                node: LayoutTree::Container {
                    direction: SplitDirection::Vertical,
                    children: vec![
                        LayoutChild {
                            node: LayoutTree::Leaf(a),
                            ratio: left.clone(),
                        },
                        LayoutChild {
                            node: LayoutTree::Leaf(b),
                            ratio: right.clone(),
                        },
                    ],
                    drag: Rc::new(Cell::new(None)),
                    container_size: Rc::new(Cell::new(0.0)),
                },
                ratio: Rc::new(Cell::new(ratio)),
            })
        };
        let top = row(row_ratio)?;
        let bottom = row(1.0 - row_ratio)?;
        Some(LayoutTree::Container {
            direction: SplitDirection::Horizontal,
            children: vec![top, bottom],
            drag: Rc::new(Cell::new(None)),
            container_size: Rc::new(Cell::new(0.0)),
        })
    }

    /// Put `replacement` where `target` sits, leaving the tree's shape alone.
    ///
    /// This is how a cell is closed in a grid: the form survives and the cell
    /// becomes a launcher. Returns whether the target was found.
    pub fn replace_leaf(&mut self, target: &Entity<Pane>, replacement: Entity<Pane>) -> bool {
        match self {
            LayoutTree::Leaf(pane) => {
                if pane == target {
                    *pane = replacement;
                    true
                } else {
                    false
                }
            }
            LayoutTree::Container { children, .. } => {
                for child in children.iter_mut() {
                    // `replacement` is moved on the first hit, so the loop has
                    // to stop there rather than clone it into every branch.
                    if child.node.contains_leaf(target) {
                        return child.node.replace_leaf(target, replacement);
                    }
                }
                false
            }
        }
    }
}

fn is_grid_row(node: &LayoutTree) -> bool {
    matches!(
        node,
        LayoutTree::Container {
            direction: SplitDirection::Vertical,
            children,
            ..
        } if children.len() == 2
            && children
                .iter()
                .all(|cell| matches!(cell.node, LayoutTree::Leaf(_)))
    )
}

/// The cells that survive **leaving** a grid.
///
/// A grid has four cells always, and choosing it with fewer panes open fills
/// the rest with the launcher. Those launchers are the *form's*, not the
/// person's: they exist because a 2x2 has four cells, in the same way that
/// closing a cell leaves the launcher standing in it rather than collapsing to
/// a 2-over-1. So leaving the form gives them back - a person who put two panes
/// into a grid, looked at it and pressed `Side by side` means the two panes
/// they opened, not four, and carrying the two invitations across would hand
/// them a row nobody asked for and no way back to three.
///
/// It is deliberately asked of a grid **only**. An empty pane in a row was made
/// by `Add pane`, which is a person asking for it in as many words; dropping
/// that one on a re-orientation would be destroying something requested.
///
/// The whole rule is one the app already applies at the other end: restore
/// prunes empty panes everywhere except inside a grid (`prune_empty_panes`).
/// Without this, a grid of two sessions turned side by side became a row of
/// four that came back as a row of two on the next launch - the layout
/// rearranging itself across a restart, which is the exact thing the grid's
/// design forbids.
///
/// Never fewer than one: a grid of four launchers is a legitimate state, and
/// answering it with no panes at all would be a re-orientation that emptied the
/// container. One launcher survives, the orientation control goes away with the
/// second pane, and that is the honest end of it.
///
/// That corner is where the parity with restore stops, and it stops on a rule
/// older than this one: **a lone empty pane is not restored** anywhere, grid or
/// not, because an empty pane is an invitation rather than part of the
/// arrangement. So a grid of four launchers turned side by side is one launcher
/// now and a container with no panes on the next launch - which is the same
/// invitation, drawn by the empty area instead of by a pane. What this function
/// makes agree is the panes that hold something.
pub(crate) fn cells_worth_keeping<T>(cells: Vec<T>, is_empty: impl Fn(&T) -> bool) -> Vec<T> {
    let mut kept = Vec::with_capacity(cells.len());
    let mut first_empty = None;
    for cell in cells {
        if is_empty(&cell) {
            if first_empty.is_none() {
                first_empty = Some(cell);
            }
        } else {
            kept.push(cell);
        }
    }
    if kept.is_empty() {
        kept.extend(first_empty);
    }
    kept
}

/// A ratio read off disk is whatever was written there. Anything that is not a
/// usable fraction falls back to an even split rather than producing a cell of
/// zero or negative width - the same defensive reading `resolve_ratios` does
/// for the flat forms.
///
/// The bound is the **honest domain of a ratio**, `(0, 1)` exclusive, and not a
/// narrower band of what looks reasonable. It was `(0.05, 0.95)`, which is
/// tighter than what the divider drag can legitimately produce: `MIN_PANE_SIZE`
/// is 300px, so on a window past 6000px a person can drag past 0.95 on purpose,
/// and would then find the divider snapped back to the middle on the next
/// launch with nothing to explain it. A floor in pixels belongs to the drag,
/// which already has one; this only has to refuse what is not a fraction.
fn sane_ratio(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 && value < 1.0 {
        value
    } else {
        0.5
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, Entity, TestAppContext};

    use crate::pane::Pane;
    use crate::terminal::TerminalView;

    use super::*;

    // `cells_worth_keeping` is about the shape a re-orientation leaves behind
    // and nothing about panes, so it is exercised on plain values: `None` is a
    // cell the grid filled in, `Some(n)` a pane the person opened.
    fn kept(cells: &[Option<u8>]) -> Vec<Option<u8>> {
        cells_worth_keeping(cells.to_vec(), |cell| cell.is_none())
    }

    #[test]
    fn leaving_a_grid_gives_back_the_cells_the_grid_filled_in() {
        assert_eq!(
            kept(&[Some(1), None, Some(2), None]),
            vec![Some(1), Some(2)],
            "two sessions in a grid turn side by side as two panes, not four"
        );
        assert_eq!(
            kept(&[Some(1), Some(2), Some(3), None]),
            vec![Some(1), Some(2), Some(3)],
            "and three as three - the count the person asked about"
        );
    }

    #[test]
    fn leaving_a_full_grid_keeps_every_pane() {
        let full = [Some(1), Some(2), Some(3), Some(4)];
        assert_eq!(
            kept(&full),
            full.to_vec(),
            "four live sessions are four the person opened; none is the form's"
        );
    }

    #[test]
    fn leaving_a_grid_of_launchers_leaves_one() {
        assert_eq!(
            kept(&[None, None, None, None]),
            vec![None],
            "a re-orientation may not empty the container"
        );
    }

    #[test]
    fn the_order_of_the_cells_that_stay_is_the_grid_s_own() {
        assert_eq!(
            kept(&[None, Some(9), None, Some(3)]),
            vec![Some(9), Some(3)],
            "reading order, so a pane does not move across the screen unasked"
        );
    }

    fn test_pane(cx: &mut impl AppContext, workspace_id: u64) -> Entity<Pane> {
        let terminal = cx.new(|cx| TerminalView::display_only_for_test(workspace_id, cx));
        cx.new(|cx| Pane::new(terminal, cx))
    }

    fn four(cx: &mut impl AppContext) -> Vec<Entity<Pane>> {
        (0..4).map(|_| test_pane(cx, 1)).collect()
    }

    fn launcher_pane(cx: &mut impl AppContext) -> Entity<Pane> {
        cx.new(|cx| Pane::new_with_tabs(Vec::new(), 0, cx))
    }

    /// The whole of what `Side by side` does to a grid, in the order the
    /// handler does it: read the leaves, give back the cells the form filled
    /// in, rebuild flat.
    #[gpui::test]
    fn a_grid_of_two_sessions_turns_side_by_side_as_two_panes(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let (a, b) = (test_pane(cx, 1), test_pane(cx, 1));
        let tree = LayoutTree::grid_of_four(vec![
            a.clone(),
            launcher_pane(cx),
            b.clone(),
            launcher_pane(cx),
        ])
        .expect("grid");

        let panes = cx.update(|_, cx| {
            cells_worth_keeping(tree.collect_leaves(), |pane| pane.read(cx).tabs.is_empty())
        });
        assert_eq!(
            panes,
            vec![a, b],
            "the two the person opened, in reading order"
        );

        let row = LayoutTree::from_panes_equal(SplitDirection::Vertical, panes).expect("a row");
        assert!(row.is_flat(), "and a row, not a nest");
        assert!(!row.is_grid());
        assert_eq!(row.leaf_count(), 2);
    }

    #[gpui::test]
    fn a_grid_of_three_sessions_turns_into_the_three(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let live: Vec<Entity<Pane>> = (0..3).map(|_| test_pane(cx, 1)).collect();
        let mut cells = live.clone();
        cells.push(launcher_pane(cx));
        let tree = LayoutTree::grid_of_four(cells).expect("grid");

        let panes = cx.update(|_, cx| {
            cells_worth_keeping(tree.collect_leaves(), |pane| pane.read(cx).tabs.is_empty())
        });
        assert_eq!(
            panes, live,
            "three panes is a count this control can now reach"
        );
    }

    #[gpui::test]
    fn a_full_grid_turns_into_four(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let panes = four(cx);
        let tree = LayoutTree::grid_of_four(panes.clone()).expect("grid");
        let kept = cx.update(|_, cx| {
            cells_worth_keeping(tree.collect_leaves(), |pane| pane.read(cx).tabs.is_empty())
        });
        assert_eq!(kept, panes, "nothing here is the form's to take back");
    }

    #[gpui::test]
    fn a_grid_is_four_leaves_in_two_rows(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let tree = LayoutTree::grid_of_four(four(cx)).expect("four panes build a grid");
        assert!(tree.is_grid());
        assert_eq!(tree.leaf_count(), GRID_CELLS);
        assert_eq!(tree.root_form(), Some(PaneForm::Grid));
    }

    #[gpui::test]
    fn the_two_rows_share_one_column_ratio(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let tree = LayoutTree::grid_of_four(four(cx)).expect("grid");
        let LayoutTree::Container { children: rows, .. } = &tree else {
            panic!("a grid is a container");
        };
        let cell_ratio = |row: &LayoutChild, idx: usize| {
            let LayoutTree::Container { children, .. } = &row.node else {
                panic!("a grid row is a container");
            };
            children[idx].ratio.clone()
        };
        // The claim this form rests on: one cell, two rows. Not "equal" - the
        // same allocation, so a drag cannot move one row without the other.
        assert!(Rc::ptr_eq(
            &cell_ratio(&rows[0], 0),
            &cell_ratio(&rows[1], 0)
        ));
        assert!(Rc::ptr_eq(
            &cell_ratio(&rows[0], 1),
            &cell_ratio(&rows[1], 1)
        ));

        cell_ratio(&rows[0], 0).set(0.7);
        assert_eq!(cell_ratio(&rows[1], 0).get(), 0.7);
    }

    #[gpui::test]
    fn fewer_or_more_than_four_is_not_a_grid(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let three: Vec<_> = (0..3).map(|_| test_pane(cx, 1)).collect();
        assert!(LayoutTree::grid_of_four(three).is_none());
        let five: Vec<_> = (0..5).map(|_| test_pane(cx, 1)).collect();
        assert!(LayoutTree::grid_of_four(five).is_none());
    }

    #[gpui::test]
    fn a_row_of_four_is_not_a_grid(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let tree = LayoutTree::from_panes_equal(SplitDirection::Vertical, four(cx)).expect("row");
        assert!(!tree.is_grid());
        assert_eq!(tree.root_form(), Some(PaneForm::Row));
    }

    #[gpui::test]
    fn dragged_ratios_survive_a_rebuild(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let tree = LayoutTree::grid_of_four_with(four(cx), 0.62, 0.38).expect("grid");
        assert_eq!(tree.grid_ratios(), Some((0.62, 0.38)));
    }

    #[gpui::test]
    fn a_ratio_the_file_could_not_have_meant_falls_back_to_even(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let tree = LayoutTree::grid_of_four_with(four(cx), f32::NAN, 0.0).expect("grid");
        assert_eq!(tree.grid_ratios(), Some((0.5, 0.5)));
        // But a ratio a person could actually have dragged to is kept. The
        // bound used to be (0.05, 0.95), which is narrower than what the
        // divider drag can produce on a very wide window - so a deliberate drag
        // came back snapped to the middle on the next launch.
        let tree = LayoutTree::grid_of_four_with(four(cx), 0.97, 0.03).expect("grid");
        assert_eq!(tree.grid_ratios(), Some((0.97, 0.03)));
    }

    /// The sharing has to survive the way a grid actually comes back: through
    /// the schema, where every ratio arrives in an allocation of its own.
    ///
    /// This is the invariant, not `is_grid`: a restored grid that *looks* right
    /// and has four independent ratios has two vertical dividers that drift
    /// apart on the first drag - the 2-over-1 the form exists to prevent. The
    /// door is `flattened`, which every stored or received layout comes
    /// through.
    #[gpui::test]
    fn the_shared_column_ratio_is_restored_by_the_normalizer(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let panes = four(cx);
        // A grid built the way deserialization builds one: right shape, four
        // separate `Rc`s, and a column split the person had dragged off centre.
        let unshared = LayoutTree::Container {
            direction: SplitDirection::Horizontal,
            children: vec![
                grid_row_unshared(panes[0].clone(), panes[1].clone(), 0.4, 0.62),
                grid_row_unshared(panes[2].clone(), panes[3].clone(), 0.6, 0.62),
            ],
            drag: Rc::new(Cell::new(None)),
            container_size: Rc::new(Cell::new(0.0)),
        };
        assert!(unshared.is_grid(), "the shape is right before normalising");

        let tree = unshared.flattened().expect("a grid normalises to a grid");
        assert!(tree.is_grid(), "and stays one");
        assert_eq!(
            tree.grid_ratios(),
            Some((0.62, 0.4)),
            "the dragged ratios are the file's and are kept"
        );

        let LayoutTree::Container { children: rows, .. } = &tree else {
            panic!("a grid is a container");
        };
        let cell_ratio = |row: &LayoutChild, idx: usize| {
            let LayoutTree::Container { children, .. } = &row.node else {
                panic!("a grid row is a container");
            };
            children[idx].ratio.clone()
        };
        assert!(
            Rc::ptr_eq(&cell_ratio(&rows[0], 0), &cell_ratio(&rows[1], 0)),
            "the normalizer re-shares the column ratio the schema could not carry"
        );
    }

    fn grid_row_unshared(
        a: Entity<Pane>,
        b: Entity<Pane>,
        row_ratio: f32,
        col_ratio: f32,
    ) -> LayoutChild {
        LayoutChild {
            node: LayoutTree::Container {
                direction: SplitDirection::Vertical,
                children: vec![
                    LayoutChild {
                        node: LayoutTree::Leaf(a),
                        ratio: Rc::new(Cell::new(col_ratio)),
                    },
                    LayoutChild {
                        node: LayoutTree::Leaf(b),
                        ratio: Rc::new(Cell::new(1.0 - col_ratio)),
                    },
                ],
                drag: Rc::new(Cell::new(None)),
                container_size: Rc::new(Cell::new(0.0)),
            },
            ratio: Rc::new(Cell::new(row_ratio)),
        }
    }

    #[gpui::test]
    fn replacing_a_cell_keeps_the_grid_a_grid(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let panes = four(cx);
        let closed = panes[2].clone();
        let mut tree = LayoutTree::grid_of_four(panes).expect("grid");
        let launcher = test_pane(cx, 1);

        assert!(tree.replace_leaf(&closed, launcher.clone()));
        assert!(tree.is_grid());
        assert_eq!(tree.leaf_count(), GRID_CELLS);
        assert!(!tree.contains_leaf(&closed));
        assert!(tree.contains_leaf(&launcher));
    }

    #[gpui::test]
    fn replacing_a_pane_that_is_not_here_changes_nothing(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let mut tree = LayoutTree::grid_of_four(four(cx)).expect("grid");
        let stranger = test_pane(cx, 1);
        let launcher = test_pane(cx, 1);
        assert!(!tree.replace_leaf(&stranger, launcher.clone()));
        assert!(!tree.contains_leaf(&launcher));
    }

    #[gpui::test]
    fn reading_order_is_top_left_across_then_down(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let panes = four(cx);
        let expected = panes.clone();
        let tree = LayoutTree::grid_of_four(panes).expect("grid");
        assert_eq!(tree.collect_leaves(), expected);
    }
}
