use crate::SplitlaneApp;
use crate::theme::UiColors;
use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Role, StatefulInteractiveElement, Styled, div, px, svg,
};

impl SplitlaneApp {
    pub(crate) fn render_settings_nav_header(
        &self,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover_background = crate::app::constants::sidebar_tab_active_background();

        div()
            .id("settings-nav-header")
            .flex_none()
            .h(tok::row::HEADER)
            .px(tok::space::MD)
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .pl(tok::space::MD)
                    .text_size(tok::text::HEADING)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui.text)
                    .child("Settings"),
            )
            .child(
                div()
                    .id("settings-back")
                    .role(Role::Button)
                    .aria_label("Close settings")
                    .flex_none()
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(tok::radius::PANEL)
                    .animated_hover_bg(hover_background.opacity(0.0), hover_background)
                    .tooltip(crate::ui_primitives::text_tooltip("Close settings"))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.close_settings(cx);
                        cx.notify();
                    }))
                    .child(
                        svg()
                            .size(px(16.))
                            .flex_none()
                            .path("icons/close.svg")
                            .text_color(ui.muted),
                    ),
            )
            .into_any_element()
    }
}
