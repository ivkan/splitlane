//! "Shortcuts" settings tab - agents-styled list of every rebindable
//! action with click-to-record key capture.
//!
//! Layout: a "Bindings" eyebrow with an inline "Reset to defaults" button on
//! the right, a filter field, then one `setting_card` per context group (A6),
//! each headed by the group's label and holding one row per shortcut,
//! separated by 1px hairlines. Click capture is driven by
//! `SplitlaneApp::handle_shortcut_recording` (in `app::settings`).
//!
//! The rows come from `keybindings::group_shortcuts`, which the command
//! palette also consumes: the two surfaces show one selection twice, so the
//! selection and the filter live in `keybindings/display.rs`, not here.

use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    SharedString, Styled, Window, div, prelude::*,
};

use crate::settings::components::{
    SETTINGS_CONTROL_CORNER_RADIUS, hairline, secondary_button, section_header_with_action,
    setting_card,
};
use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;
use crate::{SplitlaneApp, config_writer, keybindings};

impl SplitlaneApp {
    pub(crate) fn render_shortcuts_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let recording_idx = self.recording_shortcut_idx;

        let reset_btn = secondary_button(
            "reset-shortcuts",
            "Reset to defaults",
            ui,
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                config_writer::reset_shortcuts();
                let config = splitlane_config::loader::load_config();
                keybindings::apply_keybindings(cx, &config.shortcuts);
                this.effective_shortcuts = keybindings::effective_shortcuts(&config.shortcuts);
                this.recording_shortcut_idx = None;
                cx.notify();
            }),
        );

        let header = section_header_with_action(ui, "Bindings", reset_btn);
        let filter = self.render_shortcuts_filter(ui, cx);

        let query = self.shortcuts_filter_input.read(cx).value().to_lowercase();
        let groups = keybindings::group_shortcuts(&self.effective_shortcuts, &query);

        let mut body = div().flex().flex_col().gap(tok::space::SECTION);

        if groups.is_empty() {
            body = body.child(
                setting_card(ui).child(
                    div()
                        .px(tok::space::XL)
                        .py(tok::space::XL)
                        .text_size(tok::text::ROW)
                        .text_color(ui.muted)
                        .child("No shortcut matches this filter."),
                ),
            );
        }

        for (group_label, rows) in groups {
            let mut list = setting_card(ui);
            let total = rows.len();
            for (position, (idx, entry)) in rows.into_iter().enumerate() {
                // `idx` indexes `self.effective_shortcuts`, NOT the displayed
                // order: the editor rebinds by that index, and grouping +
                // filtering both reorder and drop rows.
                let is_recording = recording_idx == Some(idx);
                let is_last = position + 1 == total;

                let key_badge = if is_recording {
                    div()
                        .px(tok::space::MD)
                        .py(tok::space::XS)
                        .rounded(SETTINGS_CONTROL_CORNER_RADIUS)
                        .bg(ui.accent)
                        .text_size(tok::text::CAPTION)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(ui.text)
                        .child("Press a key…")
                } else {
                    div()
                        .px(tok::space::MD)
                        .py(tok::space::XS)
                        .rounded(SETTINGS_CONTROL_CORNER_RADIUS)
                        .bg(ui.subtle)
                        .text_size(tok::text::CAPTION)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(ui.text)
                        .child(entry.key.clone())
                };

                let row = div()
                    .id(("shortcut", idx))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(tok::space::XL)
                    .px(tok::space::XL)
                    .py(tok::space::MD)
                    .animated_hover_bg(ui.subtle.opacity(0.0), ui.subtle)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.recording_shortcut_idx = Some(idx);
                        this.settings_focus.focus(window, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(tok::text::ROW)
                            .text_color(ui.text)
                            .truncate()
                            .child(entry.description.clone()),
                    )
                    .child(key_badge);

                list = list.child(row);
                if !is_last {
                    list = list.child(hairline(ui));
                }
            }

            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .child(group_header(ui, group_label))
                    .child(list),
            );
        }

        let hint = div()
            .pt(tok::space::MD)
            .text_size(tok::text::CAPTION)
            .text_color(ui.muted)
            .child("Click a row to record a new shortcut. Escape to cancel.");

        div()
            .flex()
            .flex_col()
            .child(header)
            .child(div().pb(tok::space::XL).child(filter))
            .child(body)
            .child(hint)
    }

    /// The shortcuts filter - the shared [`crate::ui_primitives::filter_pill`]
    /// recipe, same as the Agents rail, the sessions rail, Review and the
    /// settings nav. Escape clears it (the settings-wide Escape still closes
    /// the page when it is already empty, in `handle_settings_key_down`).
    fn render_shortcuts_filter(
        &self,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let show_clear = !self.shortcuts_filter_input.read(cx).value().is_empty();
        crate::ui_primitives::filter_pill(
            "shortcuts-filter",
            "shortcuts-filter-clear",
            ui,
            self.shortcuts_filter_input.clone(),
            show_clear,
            cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.shortcuts_filter_input.update(cx, |input, cx| {
                    input.clear(cx);
                });
            }),
        )
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
            if ev.keystroke.key == "escape"
                && !this.shortcuts_filter_input.read(cx).value().is_empty()
            {
                this.shortcuts_filter_input.update(cx, |input, cx| {
                    input.clear(cx);
                });
                cx.notify();
                cx.stop_propagation();
            }
        }))
        .on_mouse_down_out(cx.listener(|this, _, window: &mut Window, cx| {
            if this
                .shortcuts_filter_input
                .read(cx)
                .focus_handle
                .is_focused(window)
            {
                window.blur();
                cx.notify();
            }
        }))
        .into_any_element()
    }
}

/// Per-group eyebrow. `section_header` takes a `&'static str`; group labels
/// are runtime-derived from the registry, so this mirrors its typography for
/// a borrowed label.
fn group_header(ui: crate::theme::UiColors, label: &str) -> impl IntoElement {
    div().pb(tok::space::MD).child(
        div()
            .text_size(crate::ui_primitives::LABEL)
            .font_weight(gpui::FontWeight::NORMAL)
            .text_color(ui.muted)
            .child(SharedString::from(label.to_string())),
    )
}
