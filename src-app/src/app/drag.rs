//! Drag-and-drop payloads for sidebar workspace reordering.
//!
//! Extracted from `main.rs`. `WorkspaceDrag` is the payload
//! used as the drag value; `WorkspaceDragPreview` is a small floating
//! GPUI entity rendered under the cursor during the drag.

use crate::ui_tokens as tok;
use gpui::{
    Context, FontWeight, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px,
    svg,
};

/// Drag payload used when reordering workspace cards in the sidebar.
#[derive(Clone)]
pub(crate) struct WorkspaceDrag {
    pub(crate) id: u64,
    pub(crate) source_idx: usize,
    pub(crate) title: SharedString,
    pub(crate) branch: Option<SharedString>,
}

/// Floating preview entity rendered under the cursor during a workspace drag.
pub(crate) struct WorkspaceDragPreview {
    pub(crate) title: SharedString,
    pub(crate) branch: Option<SharedString>,
}

impl Render for WorkspaceDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let mut preview = div()
            .w(px(crate::app::constants::SIDEBAR_WIDTH - 16.))
            .min_h(px(44.))
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::PANEL)
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.text.opacity(0.12))
            .shadow(crate::ui_primitives::menu_shadow(ui))
            .flex()
            .flex_col()
            .gap(tok::space::XS)
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(ui.text)
            .child(self.title.clone());

        if let Some(branch) = self.branch.clone() {
            preview = preview.child(
                div()
                    .h(px(14.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::XS)
                    .text_xs()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(ui.muted)
                    .child(
                        svg()
                            .size(px(12.))
                            .flex_none()
                            .path("icons/git-branch-sidebar.svg")
                            .text_color(ui.muted),
                    )
                    .child(branch),
            );
        }

        preview
    }
}

/// What the rail is dragging right now, recorded when the drag starts.
///
/// GPUI keeps the active drag's value private, and the rail has to know the
/// kind while it draws: the drop band for projects without a group exists only
/// while a *project* is in flight. Read together with `has_active_drag`, so a
/// value left over from an earlier drag says nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RailDragKind {
    Project,
    Group,
}

/// Drag payload for moving a whole project group by its label.
#[derive(Clone)]
pub(crate) struct GroupDrag {
    pub(crate) id: u64,
    /// Its place in the group list when the drag began, for the drop edge.
    pub(crate) source_position: usize,
    pub(crate) label: SharedString,
    pub(crate) summary: SharedString,
}

/// The label and its summary under the cursor while a group is dragged.
pub(crate) struct GroupDragPreview {
    pub(crate) label: SharedString,
    pub(crate) summary: SharedString,
}

impl Render for GroupDragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::SM)
            .px(tok::space::SM)
            .py(tok::space::XS)
            .rounded(tok::radius::PANEL)
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.text.opacity(0.12))
            .shadow(crate::ui_primitives::menu_shadow(ui))
            .font_family(tok::font::MONO)
            .child(
                div()
                    .px(tok::group::LABEL_PAD_X)
                    .py(tok::group::LABEL_PAD_Y)
                    .rounded(tok::radius::BADGE)
                    .bg(ui.tag_fill)
                    .text_color(ui.text)
                    .text_size(tok::mono::LABEL)
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.label.clone()),
            )
            .child(
                div()
                    .text_size(tok::mono::HINT)
                    .text_color(ui.faint)
                    .child(self.summary.clone()),
            )
    }
}
