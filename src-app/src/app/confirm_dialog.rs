//! The app's one confirmation card: a title, a sentence saying what happens,
//! `Cancel`, and a danger button naming the act.
//!
//! There are two askers - deleting a rail row and leaving the app while an
//! agent is mid-turn - and one drawing, so the two cannot drift a pixel apart.
//! The sentence is the caller's and is held to the rule the delete copy set:
//! name what goes, then say what actually happens, never "this cannot be
//! undone" when part of it can.

use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Window, deferred, div, px,
};

use crate::SplitlaneApp;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

/// What one of the two buttons does.
pub(crate) type ConfirmHandler =
    Rc<dyn Fn(&mut SplitlaneApp, &mut Window, &mut Context<SplitlaneApp>)>;

pub(crate) struct ConfirmDialog {
    /// Prefix for the element ids, so two askers never share one.
    pub id: &'static str,
    pub title: String,
    pub body: String,
    pub confirm_label: String,
    /// When set, the card takes the keyboard: `escape` cancels and `enter`
    /// confirms, each on a fresh press only. The handle has to be one the render pass lists as a focus
    /// holder, or its keys are pulled back to the panes a frame later.
    pub focus: Option<FocusHandle>,
    pub on_cancel: ConfirmHandler,
    pub on_confirm: ConfirmHandler,
}

pub(crate) fn render_confirm_dialog(
    dialog: ConfirmDialog,
    ui: crate::theme::UiColors,
    cx: &mut Context<SplitlaneApp>,
) -> AnyElement {
    let ConfirmDialog {
        id,
        title,
        body,
        confirm_label,
        focus,
        on_cancel,
        on_confirm,
    } = dialog;
    let element_id = |part: &str| SharedString::from(format!("{id}-{part}"));
    let cancel_resting_background = ui.subtle;
    let cancel_hover_background = ui.surface;

    let mut card = div()
        .id(element_id("dialog"))
        .occlude()
        .w(px(360.))
        .bg(ui.overlay)
        .border_1()
        .border_color(ui.border)
        .rounded(tok::radius::WINDOW)
        .shadow(crate::ui_primitives::menu_shadow(ui))
        .p(tok::space::XXL)
        .flex()
        .flex_col()
        .gap(tok::space::MD)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if let Some(focus) = focus.as_ref() {
        let (on_cancel, on_confirm) = (on_cancel.clone(), on_confirm.clone());
        card = card.track_focus(focus).on_key_down(cx.listener(
            move |this, event: &KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
                // A held key auto-repeats; an answer has to be a fresh press.
                _ if event.is_held => {}
                "escape" => {
                    cx.stop_propagation();
                    on_cancel(this, window, cx);
                }
                "enter" => {
                    cx.stop_propagation();
                    on_confirm(this, window, cx);
                }
                _ => {}
            },
        ));
    }

    let backdrop_cancel = on_cancel.clone();
    let backdrop = div()
        .id(element_id("backdrop"))
        .occlude()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .size_full()
        .bg(gpui::black().opacity(0.45))
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| backdrop_cancel(this, window, cx)),
        )
        .child(
            card.child(
                div()
                    .text_size(tok::text::TITLE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui.text)
                    .child(title),
            )
            .child(
                div()
                    .text_size(tok::text::ROW)
                    .text_color(ui.muted)
                    .child(body),
            )
            .child(
                div()
                    .mt(tok::space::XS)
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(tok::space::MD)
                    .child(
                        div()
                            .id(element_id("cancel"))
                            .px(tok::space::XL)
                            .py(tok::space::MD)
                            .rounded(tok::radius::SMALL)
                            .bg(cancel_resting_background)
                            .text_size(tok::text::ROW)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ui.text)
                            .animated_hover(move |style, delta| {
                                style.bg(lerp_color(
                                    cancel_resting_background,
                                    cancel_hover_background,
                                    delta,
                                ));
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                on_cancel(this, window, cx);
                            }))
                            .child("Cancel"),
                    )
                    .child(
                        div()
                            .id(element_id("confirm"))
                            .px(tok::space::XL)
                            .py(tok::space::MD)
                            .rounded(tok::radius::SMALL)
                            .bg(ui.vc_deleted)
                            .text_size(tok::text::ROW)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui.base)
                            .opacity(1.0)
                            .animated_hover(|style, delta| {
                                style.opacity(1.0 - 0.12 * delta);
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                on_confirm(this, window, cx);
                            }))
                            .child(confirm_label),
                    ),
            ),
        );

    deferred(backdrop).priority(4).into_any_element()
}
