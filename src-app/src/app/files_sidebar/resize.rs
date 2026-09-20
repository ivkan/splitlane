//! The Files panel's left edge, and the width the user drags it to.
//!
//! The rail's own edge is `app/rail_resize.rs`, and this is deliberately the
//! same thing mirrored: a 5px strip with no fill, the design's accent border on
//! hover, a double-click back to the default, one save at the end of the
//! gesture rather than one per pixel. The only difference is the sign - this
//! panel is on the right, so dragging its edge LEFT makes it wider.

use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Styled, div, prelude::*, px,
};

use super::{FILES_RESIZE_HIT, FILES_SIDEBAR_WIDTH, FILES_WIDTH_MAX, FILES_WIDTH_MIN};
use crate::SplitlaneApp;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};

impl SplitlaneApp {
    /// The strip on the panel's left edge. Absolutely positioned so it takes
    /// no column out of the row - the tree keeps every pixel of the width the
    /// user chose.
    pub(super) fn render_files_resize_handle(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = crate::theme::ui_colors();
        let resting = gpui::transparent_black();
        let hovered = ui.accent_border;
        let width = self.files_width;
        div()
            .id("files-resize-handle")
            .absolute()
            .top_0()
            .bottom_0()
            // Flush inside the panel rather than centred on its edge. The rail
            // can centre its handle because that one is a child of the body
            // row; this panel clips its own children, so the outer half of a
            // centred strip would be cut away and the target halved.
            .left_0()
            .w(px(FILES_RESIZE_HIT))
            .cursor_col_resize()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                    this.files_resize_drag = Some((e.position.x.into(), width));
                    cx.stop_propagation();
                }),
            )
            .on_click(cx.listener(|this, e: &ClickEvent, _w, cx| {
                if matches!(e, ClickEvent::Mouse(m) if m.down.click_count == 2) {
                    this.set_files_width(FILES_SIDEBAR_WIDTH, cx);
                }
            }))
            .animated_hover(move |style, delta| {
                style.bg(lerp_color(resting, hovered, delta));
            })
            .into_any_element()
    }

    pub(crate) fn handle_files_resize_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start_x, start_width)) = self.files_resize_drag else {
            return;
        };
        // A drag whose button was released outside the window never sends the
        // mouse-up; treating "button no longer down" as the end is what keeps
        // the panel from following the pointer afterwards.
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.end_files_resize(cx);
            return;
        }
        // The panel grows leftward, so the delta is negated.
        let delta = f32::from(event.position.x) - start_x;
        self.set_files_width(start_width - delta, cx);
    }

    pub(crate) fn handle_files_resize_end(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.end_files_resize(cx);
    }

    fn end_files_resize(&mut self, cx: &mut Context<Self>) {
        if self.files_resize_drag.take().is_none() {
            return;
        }
        // Saved once, at the end of the gesture - not on every pixel of it.
        self.save_session(cx);
        cx.notify();
    }

    /// Set the panel's width, clamped to the range the design states.
    pub(crate) fn set_files_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let clamped = clamp_files_width(width);
        if (self.files_width - clamped).abs() < f32::EPSILON {
            return;
        }
        self.files_width = clamped;
        cx.notify();
    }
}

/// The design's 210-420. A restored value outside it - hand-edited, or written
/// by a build with a different range - is clamped rather than refused: the
/// width is a preference, not a contract.
pub(crate) fn clamp_files_width(width: f32) -> f32 {
    if !width.is_finite() {
        return FILES_SIDEBAR_WIDTH;
    }
    width.clamp(FILES_WIDTH_MIN, FILES_WIDTH_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_clamps_to_the_stated_range() {
        assert_eq!(clamp_files_width(300.), 300.);
        assert_eq!(clamp_files_width(10.), FILES_WIDTH_MIN);
        assert_eq!(clamp_files_width(10_000.), FILES_WIDTH_MAX);
    }

    #[test]
    fn a_nonsense_width_falls_back_to_the_default() {
        assert_eq!(clamp_files_width(f32::NAN), FILES_SIDEBAR_WIDTH);
        assert_eq!(clamp_files_width(f32::INFINITY), FILES_SIDEBAR_WIDTH);
    }
}
