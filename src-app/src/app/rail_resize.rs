//! The rail's right edge, and the width the user drags it to.
//!
//! The width used to be a constant, which made it the one piece of the
//! chrome's geometry nobody could argue with: a rail wide enough for a long
//! session name is too wide on a laptop, and the other way round. It is now
//! the user's, inside the range the design states, and it is saved with the
//! session so it is still theirs after a restart.
//!
//! The handle is a 5px strip with no fill of its own; it takes the design's
//! accent border on hover so the edge announces itself only when the pointer
//! is on it.

use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Styled, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::app::agents_view_actions::{RAIL_RESIZE_HIT, RAIL_WIDTH_MAX, RAIL_WIDTH_MIN};
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};

impl SplitlaneApp {
    /// The strip on the rail's right edge. Absolutely positioned so it does
    /// not take a column of its own out of the row - the rail keeps every
    /// pixel of the width the user chose.
    pub(crate) fn render_rail_resize_handle(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = crate::theme::ui_colors();
        let resting = gpui::transparent_black();
        let hovered = ui.accent_border;
        let width = self.rail_width;
        div()
            .id("rail-resize-handle")
            .absolute()
            .top_0()
            .bottom_0()
            // Centred on the edge: half the hit area each side, so the target
            // is symmetric around the line the eye sees.
            .left(px(width - RAIL_RESIZE_HIT / 2.))
            .w(px(RAIL_RESIZE_HIT))
            .cursor_col_resize()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                    this.rail_resize_drag = Some((e.position.x.into(), width));
                    cx.stop_propagation();
                }),
            )
            // A double-click on a resize edge conventionally restores the
            // default, and it costs one line to honour that here.
            .on_click(cx.listener(|this, e: &ClickEvent, _w, cx| {
                if matches!(e, ClickEvent::Mouse(m) if m.down.click_count == 2) {
                    this.set_rail_width(crate::app::agents_view_actions::RAIL_WIDTH, cx);
                }
            }))
            .animated_hover(move |style, delta| {
                style.bg(lerp_color(resting, hovered, delta));
            })
            .into_any_element()
    }

    pub(crate) fn handle_rail_resize_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start_x, start_width)) = self.rail_resize_drag else {
            return;
        };
        // A drag whose button was released outside the window never sends the
        // mouse-up; treating "button no longer down" as the end is what keeps
        // the rail from following the pointer afterwards.
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.end_rail_resize(cx);
            return;
        }
        let delta = f32::from(event.position.x) - start_x;
        self.set_rail_width(start_width + delta, cx);
    }

    pub(crate) fn handle_rail_resize_end(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.end_rail_resize(cx);
    }

    fn end_rail_resize(&mut self, cx: &mut Context<Self>) {
        if self.rail_resize_drag.take().is_none() {
            return;
        }
        // Saved once, at the end of the gesture - not on every pixel of it.
        self.save_session(cx);
        cx.notify();
    }

    /// Set the rail's width, clamped to the range the design states.
    pub(crate) fn set_rail_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let clamped = clamp_rail_width(width);
        if (self.rail_width - clamped).abs() < f32::EPSILON {
            return;
        }
        self.rail_width = clamped;
        cx.notify();
    }
}

/// The design's 224-400. A restored value outside it - a file hand-edited, or
/// written by a build with a different range - is clamped rather than
/// refused: the width is a preference, not a contract.
pub(crate) fn clamp_rail_width(width: f32) -> f32 {
    if !width.is_finite() {
        return crate::app::agents_view_actions::RAIL_WIDTH;
    }
    width.clamp(RAIL_WIDTH_MIN, RAIL_WIDTH_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_clamps_to_the_stated_range() {
        assert_eq!(clamp_rail_width(300.), 300.);
        assert_eq!(clamp_rail_width(10.), RAIL_WIDTH_MIN);
        assert_eq!(clamp_rail_width(10_000.), RAIL_WIDTH_MAX);
    }

    #[test]
    fn a_nonsense_width_falls_back_to_the_default() {
        assert_eq!(
            clamp_rail_width(f32::NAN),
            crate::app::agents_view_actions::RAIL_WIDTH
        );
        assert_eq!(
            clamp_rail_width(f32::INFINITY),
            crate::app::agents_view_actions::RAIL_WIDTH
        );
    }
}
