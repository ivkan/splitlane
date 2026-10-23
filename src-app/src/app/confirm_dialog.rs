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
    /// A name to set in the body as code - a branch, a path - and the text
    /// that follows it: `body`, then the code, then the rest, as one
    /// paragraph.
    pub body_code: Option<(String, String)>,
    /// A quieter second paragraph under the body: what each answer does with
    /// what is at stake.
    pub note: Option<String>,
    pub confirm_label: String,
    /// An answer that ends nothing, drawn between `Cancel` and the danger
    /// button. When there is one, `enter` gives it: a key pressed without
    /// reading must not be the answer that stops things.
    pub safe: Option<(String, ConfirmHandler)>,
    /// When set, the card takes the keyboard: `escape` cancels and `enter`
    /// confirms, each on a fresh press only. The handle has to be one the render pass lists as a focus
    /// holder, or its keys are pulled back to the panes a frame later.
    pub focus: Option<FocusHandle>,
    pub on_cancel: ConfirmHandler,
    pub on_confirm: ConfirmHandler,
    /// A button at the left edge that answers neither way - the quit card's
    /// `Review…`.
    pub aside: Option<(String, ConfirmHandler)>,
    /// How the answers are weighted; see [`ConfirmTone`].
    pub tone: ConfirmTone,
}

/// Which answer the card puts forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmTone {
    /// `Cancel`, then the safe answer if there is one, then the confirming
    /// one in red: what it does cannot be taken back.
    Danger,
    /// No `Cancel` button (`escape` and the backdrop still cancel); the safe
    /// answer is the filled one and the confirming one is plain. For a
    /// question where neither answer destroys anything - the quit card,
    /// whose `Stop and quit` ends processes but keeps their output - so that
    /// red does not become the colour of the usual way out.
    SafeForward,
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
        body_code,
        note,
        confirm_label,
        safe,
        focus,
        on_cancel,
        on_confirm,
        aside,
        tone,
    } = dialog;
    let on_enter = safe
        .as_ref()
        .map_or_else(|| on_confirm.clone(), |(_, handler)| handler.clone());
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
        let on_cancel = on_cancel.clone();
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
                    on_enter(this, window, cx);
                }
                _ => {}
            },
        ));
    }

    let backdrop_cancel = on_cancel.clone();
    let backdrop =
        div()
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
                .child(body_paragraph(body, body_code, ui))
                .children(note.map(|note| {
                    div()
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.dim)
                        .child(note)
                }))
                .child({
                    let plain = |part: &str, label: String, weight: FontWeight| {
                        div()
                            .id(element_id(part))
                            .px(tok::space::XL)
                            .py(tok::space::MD)
                            .rounded(tok::radius::SMALL)
                            .bg(cancel_resting_background)
                            .text_size(tok::text::ROW)
                            .font_weight(weight)
                            .text_color(ui.text)
                            .animated_hover(move |style, delta| {
                                style.bg(lerp_color(
                                    cancel_resting_background,
                                    cancel_hover_background,
                                    delta,
                                ));
                            })
                            .cursor_pointer()
                            .child(label)
                    };
                    let filled = |part: &str, label: String, fill: gpui::Hsla, text: gpui::Hsla| {
                        div()
                            .id(element_id(part))
                            .px(tok::space::XL)
                            .py(tok::space::MD)
                            .rounded(tok::radius::SMALL)
                            .bg(fill)
                            .text_size(tok::text::ROW)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(text)
                            .opacity(1.0)
                            .animated_hover(|style, delta| {
                                style.opacity(1.0 - 0.12 * delta);
                            })
                            .cursor_pointer()
                            .child(label)
                    };
                    let mut row = div()
                        .mt(tok::space::XS)
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(tok::space::MD);
                    if let Some((label, handler)) = aside {
                        row =
                            row.child(plain("aside", label, FontWeight::MEDIUM).on_click(
                                cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    handler(this, window, cx);
                                }),
                            ))
                            .child(div().flex_1());
                    }
                    if tone == ConfirmTone::Danger {
                        row = row.child(
                            plain("cancel", "Cancel".to_string(), FontWeight::MEDIUM).on_click(
                                cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    on_cancel(this, window, cx);
                                }),
                            ),
                        );
                    }
                    let safe_button = safe.map(|(label, handler)| {
                        let click = cx.listener(move |this, _: &ClickEvent, window, cx| {
                            handler(this, window, cx);
                        });
                        match tone {
                            ConfirmTone::Danger => {
                                plain("safe", label, FontWeight::SEMIBOLD).on_click(click)
                            }
                            ConfirmTone::SafeForward => {
                                filled("safe", label, ui.accent, ui.on_accent).on_click(click)
                            }
                        }
                    });
                    let confirm_click = cx.listener(move |this, _: &ClickEvent, window, cx| {
                        on_confirm(this, window, cx);
                    });
                    match tone {
                        ConfirmTone::Danger => row.children(safe_button).child(
                            filled("confirm", confirm_label, ui.vc_deleted, ui.base)
                                .on_click(confirm_click),
                        ),
                        // The forward answer last, where the eye ends.
                        ConfirmTone::SafeForward => row
                            .child(
                                plain("confirm", confirm_label, FontWeight::MEDIUM)
                                    .on_click(confirm_click),
                            )
                            .children(safe_button),
                    }
                }),
            );

    deferred(backdrop).priority(4).into_any_element()
}

/// The body as one paragraph that can wrap, with the code set in the mono
/// face. Each word is its own element so the line breaks between words.
fn body_paragraph(
    body: String,
    code: Option<(String, String)>,
    ui: crate::theme::UiColors,
) -> AnyElement {
    let Some((code, rest)) = code else {
        return div()
            .text_size(tok::text::ROW)
            .text_color(ui.muted)
            .child(body)
            .into_any_element();
    };
    let word = |text: &str| div().child(text.to_string());
    div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_x(tok::space::XS)
        .text_size(tok::text::ROW)
        .text_color(ui.muted)
        .children(body.split_whitespace().map(word))
        .child(
            div()
                .font_family(tok::font::MONO)
                .text_size(tok::mono::ROW)
                .text_color(ui.text)
                .child(code),
        )
        .children(rest.split_whitespace().map(word))
        .into_any_element()
}
