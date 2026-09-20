//! The container's slot tree: **one of three named forms**.
//!
//! The types are still N-ary and still recursive, because the persisted schema
//! and the renderer are, but the shape a live tree may take is bounded here
//! and nowhere else:
//!
//! - at most [`MAX_PANES`] leaves, enforced by the insert itself rather than
//!   by each of the seven call sites that used to re-check it;
//! - one of **a row of N**, **a column of N**, or **a grid of four**
//!   ([`PaneForm`]). The first two are flat - exactly one container, whose
//!   children are all leaves - and a split in the other direction re-orients
//!   that container instead of nesting a second one, so "which way are the
//!   panes arranged" always has one answer. The grid is the only form that
//!   nests, and its nesting is fixed at two rows of two;
//! - no pane under [`tree::MIN_PANE_SIZE`], which the divider drag clamps to.
//!
//! **No free nesting**, and the reason is worth keeping: the bounded shape is
//! what keeps seven jobs finite - one ceiling check, one normalisation, one
//! spatial `Alt+Arrow`, one divider drag. Free nesting turns every one of them
//! from a finite job into an open one in exchange for shapes nobody asked for.
//! `main_vertical` and `tiled` are still gone with their actions: `tiled` was
//! tmux's arbitrary grid, which is exactly what the named 2x2 is not.
//!
//! Module layout:
//! - [`tree`] - core types, constants, ratio helpers, `new_split`
//! - [`mutations`] - `split_at_*` and `swap_panes`
//! - [`close`] - `close_focused` and `remove_pane` (kept separate from
//!   `mutations` for the 280 LOC cap)
//! - [`queries`] - read-only traversal + `equalize_ratios`
//! - [`render`] - GPUI flex rendering with drag-to-resize
//! - [`presets`] - `from_panes_equal`, and the flattening normalizer
//! - [`navigation`] - `FocusDirection`, `FocusNav`, focus movement
//! - [`serde`] - `serialize` / `from_layout_node`

mod close;
mod grid;
mod mutations;
mod navigation;
mod presets;
mod queries;
mod render;
mod serde;
mod tree;

pub use grid::PaneForm;
pub(crate) use grid::{GRID_CELLS, cells_worth_keeping};
pub use navigation::{FocusDirection, FocusNav};
pub(crate) use tree::DIVIDER_PX;
/// Only the drag clamp uses this number in anger; it is re-exported for the
/// test that keeps it distinct from the targeting ladder's 320px threshold,
/// which is a different rule with a different job.
#[cfg(test)]
pub(crate) use tree::MIN_PANE_SIZE;
pub use tree::{LayoutTree, SplitDirection};

/// Ceiling on leaf panes in a single container's layout tree.
///
/// **Four, and it is a ceiling rather than a limit.** There are four
/// kinds a pane can be of - agent, shell, diff, markdown - so a fifth pane has
/// nothing to be, and that is the whole of this number's meaning.
///
/// **What actually limits panes is width**, not this: `MIN_PANE_FOR_SPLIT`
/// (320px on the long axis) refuses a split that would leave any pane too
/// narrow to read its own header, and on a 1332pt window it already refuses the
/// third. So "a pane per kind" is not a promise, eviction is the ordinary case,
/// and this constant is only the point past which counting stops meaning
/// anything.
///
/// Historical note, because older commits say otherwise: it was **three**, and
/// before that 32 - a self-DoS bound rather than a design one. Three was the
/// design's own number until the width rule replaced it and the count was
/// called "its rounding".
///
/// This is the single source for a bound that every split/insert site
/// used to re-declare locally, and it stays that: the split path enforces it
/// too, so the ceiling holds even for a caller that forgets to ask.
pub(crate) const MAX_PANES: usize = 4;
