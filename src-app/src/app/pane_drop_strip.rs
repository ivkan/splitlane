//! The dashed strip at the far edge of the panes area: where a third pane
//! comes from.
//!
//! The design: "dragging a sidebar session over the panes area reveals a
//! dashed drop strip (76px wide, vertical label 'drop to open a third pane').
//! Dropping there appends a pane and re-splits evenly (max 3). Dropping onto
//! an existing pane swaps that pane's session. The strip only appears while
//! dragging a session that is not already open and the pane count is under 3."
//!
//! Every one of those conditions is checked here, and the strip is simply not
//! emitted when any fails - a strip that appears and then refuses the drop
//! would teach the cap one refusal at a time.
//!
//! It runs along the axis the panes do NOT: beside a row of panes it is a
//! column at the right edge, under a column of panes it is a bar along the
//! bottom. That is the only place a fourth position can be, and it keeps the
//! strip pointing at where the pane will land.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div,
    px,
};

use crate::SplitlaneApp;
use crate::layout::SplitDirection;
use crate::pane_drag::RailSurfaceDrag;
use crate::ui_tokens as tok;

/// The design's 76, kept as a floor the arithmetic below cannot actually
/// reach.
///
/// The strip used to be exactly this wide - a drop target the size of a
/// scrollbar, and hitting it with a dragged row was fiddly, reported from real
/// use. It draws the pane it will open, so it is now sized like one: the long
/// axis divided by the pane count it is about to produce, which is exactly
/// what that pane will get.
///
/// The floor is dead by construction and left in as a guard rather than a
/// promise: the strip is only drawn at all when `room_for_another_pane_now`
/// says every resulting pane clears `MIN_PANE_FOR_SPLIT` (320px), computed
/// from the same long axis over the same divisor - so the quotient below is
/// never under 320, and an unmeasured area does not get here at all.
const STRIP_THICKNESS: f32 = 76.;

impl SplitlaneApp {
    /// The strip, or nothing at all.
    pub(crate) fn render_pane_drop_strip(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let drag = self.dragging_rail_surface.clone()?;
        // The drag has to be over this container's panes: a surface belongs to
        // a directory, and dropping one into another project's layout would
        // mean running it somewhere it does not live.
        if drag.ws_idx != self.active_idx {
            return None;
        }
        let container = self.workspaces.get(self.active_idx)?;
        let root = container.root.as_ref()?;
        // The ladder's own question, not a second opinion. This used to ask
        // `leaf_count() >= MAX_PANES` - the count - while the ladder measured
        // width, so the strip invited a drop that would have produced a pane
        // too narrow to read its own header. Now: one question, three
        // askers (here, the ladder, the Split button).
        if !self.room_for_another_pane_now(cx) {
            return None;
        }
        let next_pane = root.leaf_count() + 1;
        // Already open: nothing to append.
        if crate::app::agent_slots::agent_leaf_index(container, drag.thread_id, cx).is_some() {
            return None;
        }

        let stacked = matches!(root.root_direction(), Some(SplitDirection::Horizontal));
        let mut strip = div()
            .id("pane-drop-strip")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(tok::radius::PANEL)
            .border_dashed()
            .border_1()
            .border_color(ui.accent_border)
            .bg(ui.accent_surface)
            .px(tok::space::SM)
            .child(
                // In its own box, so it wraps inside the column instead of
                // running past it: a flex child does not wrap against the
                // parent's width on its own. The design sets this label
                // vertically in the narrow case; GPUI has no text rotation, so
                // it stays horizontal and wraps over three short lines.
                div()
                    .w_full()
                    .text_center()
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.accent)
                    // Counts, because the strip can now open a third **or** a
                    // fourth: four kinds, four panes at the ceiling.
                    .child(SharedString::from(format!(
                        "drop to open a {} pane",
                        ordinal(next_pane)
                    ))),
            );

        // The panes area measures itself every frame for the ladder's third
        // rung; the same number sizes this. It is the area's own box and does
        // not change while the strip is up, so there is no feedback loop
        // between what the strip is and what it is measured against.
        let (area_w, area_h) = self.panes_area.get();
        let long_axis = if stacked { area_h } else { area_w };
        let gaps = crate::layout::DIVIDER_PX * (next_pane - 1) as f32;
        let thickness = ((long_axis - gaps) / next_pane as f32).max(STRIP_THICKNESS);

        strip = if stacked {
            // Panes stacked top to bottom: the next position is below them.
            strip.w_full().h(px(thickness))
        } else {
            // Panes side by side: the next position is to their right.
            strip.h_full().w(px(thickness))
        };

        Some(
            strip
                .on_drop(cx.listener(|this, drag: &RailSurfaceDrag, window, cx| {
                    // Re-resolved from the payload rather than trusted: the
                    // rail could have been reordered under the pointer, and
                    // `thread_idx` is a position while `thread_id` is the
                    // surface.
                    let Some(thread_idx) = this.thread_index_by_id(drag.ws_idx, drag.thread_id)
                    else {
                        return;
                    };
                    this.dragging_rail_surface = None;
                    this.append_agent_to_slots(drag.ws_idx, thread_idx, window, cx);
                }))
                .into_any_element(),
        )
    }
}

/// `3` -> `"third"`. Only ever asked about a pane index, which the ceiling
/// bounds at four, so the table is the whole domain rather than a prefix of it.
fn ordinal(n: usize) -> &'static str {
    match n {
        1 => "first",
        2 => "second",
        3 => "third",
        4 => "fourth",
        _ => "further",
    }
}
