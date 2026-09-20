//! Tab drag-and-drop primitives (reorder and cross-pane move).
//!
//! Holds the drag payloads carried by GPUI's managed drag API and the
//! [`TabDragPreview`] ghost entity rendered under the cursor. Wiring
//! (`on_drag` / `drag_over` / `on_drop`) lives in `Pane::render_tab_bar`.
//! Extracted into its own module to keep `pane.rs` near the 250-line budget.
//!
//! Mirrors the in-repo `WorkspaceDrag` / `WorkspaceDragPreview` precedent
//! (`app/drag.rs`, `sidebar/mod.rs`) - same GPUI commit, identical API shape.

use gpui::{
    Context, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px,
    svg,
};

use crate::agent_sessions::SessionAgent;
use crate::ui_tokens as tok;

/// Drag payload for an agent-session row dragged out of the docked sessions
/// sidebar (bridges the sessions sidebar and pane drag-and-drop). Unlike
/// [`RailSurfaceDrag`], which moves a surface that is already running,
/// dropping a `SessionDrag` on a
/// pane spawns a *fresh* terminal at the session's `cwd` running the agent's
/// `--resume` command. Cloned cheaply (owned ids); `title`/`icon` snapshot the
/// row so the shared [`TabDragPreview`] ghost renders without the sidebar.
#[derive(Clone)]
pub struct SessionDrag {
    pub agent: SessionAgent,
    pub session_id: String,
    pub cwd: String,
    pub title: SharedString,
    pub icon: SharedString,
}

/// Drag payload for a **parked** agent surface dragged out of the rail.
///
/// The design's third pane: "dragging a sidebar session over the panes area
/// reveals a dashed drop strip \u{2026} Dropping there appends a pane".
/// Unlike [`SessionDrag`], which spawns a fresh terminal running a `--resume`
/// command, this moves a surface that is already running: the same
/// `TerminalView` entity, with its `SessionBinding` and its PTY, goes into a
/// slot. Nothing relaunches.
///
/// Only parked surfaces are draggable. One that is already in a slot has no
/// third place to be, and the design says so too - the strip appears only for
/// "a session that is not already open".
#[derive(Clone)]
pub struct RailSurfaceDrag {
    pub ws_idx: usize,
    /// The surface itself, by id and not by rail position. The rail can be
    /// reordered while the pointer is down (a pin, a delete, a new session),
    /// so a drop resolves the position from this rather than carrying one that
    /// may have moved under it.
    pub thread_id: u64,
    pub title: SharedString,
    pub icon: SharedString,
}

/// Drag payload for a markdown file dragged out of the docked Files sidebar.
/// Dropping it on a pane opens the file via `FileView::open` - into a new
/// split (edge) or appended as a tab (center) - without a process. Only
/// markdown rows are draggable; every other file is inert. Cloned cheaply (an owned `PathBuf` + snapshotted
/// `title`/`icon`) so the shared [`TabDragPreview`] ghost renders without the
/// sidebar.
#[derive(Clone)]
pub struct MarkdownFileDrag {
    pub path: std::path::PathBuf,
    pub title: SharedString,
    pub icon: SharedString,
}

/// Floating ghost rendered under the cursor during a tab drag - a compact
/// version of the tab chip (leading icon + label).
pub struct TabDragPreview {
    pub title: SharedString,
    pub icon: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::SMALL)
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .shadow(crate::ui_primitives::menu_shadow(ui))
            .text_size(tok::text::ROW)
            .font_weight(FontWeight::MEDIUM)
            .text_color(ui.text)
            .child(
                svg()
                    .size(px(12.))
                    .flex_none()
                    .path(self.icon.clone())
                    .text_color(ui.muted),
            )
            .child(self.title.clone())
    }
}

/// The four pane edges a drop-to-split can target. This is a 4-way
/// edge, distinct from the codebase's 2-way [`crate::layout::SplitDirection`]
/// (`Horizontal`=stacked / `Vertical`=side-by-side): an edge encodes both the
/// split axis *and* which side the new pane lands on, which the 2-way enum
/// can't express. The commit maps each edge to a `SplitDirection` plus an
/// optional `swap_panes` for the "before" edges (Up/Left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEdge {
    Up,
    Down,
    Left,
    Right,
}

impl DropEdge {
    /// Map a drop edge to the 2-way [`crate::layout::SplitDirection`] plus
    /// whether the new pane swaps to the "before" position. `split_at_pane`
    /// always inserts *after* the target, so the leading edges (Up/Left) swap
    /// the moved/duplicated pane onto the correct side. Single source for the
    /// three drop-to-split handlers (DropSplit / dropped tab / dropped session).
    pub fn to_split(self) -> (crate::layout::SplitDirection, bool) {
        match self {
            DropEdge::Up => (crate::layout::SplitDirection::Horizontal, true),
            DropEdge::Down => (crate::layout::SplitDirection::Horizontal, false),
            DropEdge::Left => (crate::layout::SplitDirection::Vertical, true),
            DropEdge::Right => (crate::layout::SplitDirection::Vertical, false),
        }
    }
}

/// Fraction of a pane's *smaller* dimension that counts as an edge band for
/// drop-to-split (Zed's `drop_target_size` default). Cursor inside any edge
/// band → split toward the nearest edge; the center 60% → move-into-pane.
pub const SPLIT_EDGE_BAND: f32 = 0.20;

/// Resolve a cursor position (relative to a pane's content bounds) to a split
/// edge, using `band` as the fraction of the smaller dimension for each edge
/// strip. The nearest edge wins by min-distance; a cursor in the center
/// (outside every band) returns `None` = move-into-pane. Ported from Zed's
/// `handle_drag_move` (`crates/workspace/src/pane.rs`), adapted to compute in
/// `f32` (GPUI `Pixels` isn't `Ord`) and to Splitlane's [`DropEdge`].
///
/// `width`/`height` are the content size; `x`/`y` the cursor offset from the
/// content's top-left. Correct for non-square panes (uses both dimensions).
pub fn compute_drop_edge(width: f32, height: f32, x: f32, y: f32, band: f32) -> Option<DropEdge> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let size = width.min(height) * band;
    let in_band = x < size || x > width - size || y < size || y > height - size;
    if !in_band {
        return None;
    }
    // Distance from the cursor to each edge; the closest edge wins.
    let candidates = [
        (DropEdge::Up, y),
        (DropEdge::Right, width - x),
        (DropEdge::Down, height - y),
        (DropEdge::Left, x),
    ];
    candidates
        .into_iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(edge, _)| edge)
}

/// The blue preview overlay's target rectangle `(x, y, w, h)` (content-local
/// pixels) for a given drop direction over a pane of size `width`×`height`.
/// `None` (center / move-into-pane) fills the whole pane; each edge fills the
/// corresponding half. Used to drive the overlay's glide animation:
/// lerping between two of these rects as the cursor crosses band boundaries is
/// what makes the preview slide instead of snapping.
pub fn split_rect(dir: Option<DropEdge>, width: f32, height: f32) -> (f32, f32, f32, f32) {
    let (hw, hh) = (width * 0.5, height * 0.5);
    match dir {
        None => (0.0, 0.0, width, height),
        Some(DropEdge::Up) => (0.0, 0.0, width, hh),
        Some(DropEdge::Down) => (0.0, hh, width, hh),
        Some(DropEdge::Left) => (0.0, 0.0, hw, height),
        Some(DropEdge::Right) => (hw, 0.0, hw, height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_edge_center_is_none() {
        // 1000x800, 20% band → 160px strips; center is move-into-pane.
        assert_eq!(compute_drop_edge(1000., 800., 500., 400., 0.20), None);
    }

    #[test]
    fn drop_edge_picks_nearest_edge() {
        assert_eq!(
            compute_drop_edge(1000., 800., 40., 400., 0.20),
            Some(DropEdge::Left)
        );
        assert_eq!(
            compute_drop_edge(1000., 800., 960., 400., 0.20),
            Some(DropEdge::Right)
        );
        assert_eq!(
            compute_drop_edge(1000., 800., 500., 30., 0.20),
            Some(DropEdge::Up)
        );
        assert_eq!(
            compute_drop_edge(1000., 800., 500., 770., 0.20),
            Some(DropEdge::Down)
        );
    }

    #[test]
    fn drop_edge_non_square_uses_smaller_dimension() {
        // Tall pane 200x1000 → band = 200*0.2 = 40px. A cursor near the right
        // edge resolves to Right even though the pane is far taller than wide.
        assert_eq!(
            compute_drop_edge(200., 1000., 180., 500., 0.20),
            Some(DropEdge::Right)
        );
    }

    #[test]
    fn drop_edge_degenerate_bounds_is_none() {
        assert_eq!(compute_drop_edge(0., 0., 0., 0., 0.20), None);
    }

    #[test]
    fn split_rect_center_fills_pane() {
        assert_eq!(split_rect(None, 800., 600.), (0., 0., 800., 600.));
    }

    #[test]
    fn split_rect_edges_cover_correct_half() {
        assert_eq!(
            split_rect(Some(DropEdge::Up), 800., 600.),
            (0., 0., 800., 300.)
        );
        assert_eq!(
            split_rect(Some(DropEdge::Down), 800., 600.),
            (0., 300., 800., 300.)
        );
        assert_eq!(
            split_rect(Some(DropEdge::Left), 800., 600.),
            (0., 0., 400., 600.)
        );
        assert_eq!(
            split_rect(Some(DropEdge::Right), 800., 600.),
            (400., 0., 400., 600.)
        );
    }
}
