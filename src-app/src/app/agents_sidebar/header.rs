use crate::SplitlaneApp;
use crate::theme::UiColors;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Role, SharedString, StatefulInteractiveElement, Styled, div, svg,
};

impl SplitlaneApp {
    /// The rail's head: one button that opens the palette.
    ///
    /// It reads as a field and is not one. That is the design's call and it is
    /// load-bearing: an inline filter can only narrow what the rail already
    /// lists, while the thing the user is usually after - a session closed
    /// last week - is not in the rail at all. Pointing the affordance at the
    /// palette makes one gesture cover both, which is why the rail's own
    /// `TextInput` went with it rather than sitting beside it.
    ///
    /// The trailing hint names the chord that really opens the palette - the
    /// design's `⌘K` on macOS, and whatever Linux and Windows could safely
    /// take there (a bare `ctrl-k` is readline's kill-line).
    pub(super) fn render_rail_search(&self, ui: UiColors, cx: &mut Context<Self>) -> AnyElement {
        let chord: SharedString = self
            .shortcut_for_action("open_command_palette")
            .unwrap_or("Unassigned")
            .to_string()
            .into();
        let border_resting = ui.border;
        let border_hover = ui.border_hover;
        let label_resting = ui.muted;
        let label_hover = ui.text_secondary;

        div()
            .flex_none()
            .px(tok::space::LG)
            .pt(tok::space::LG)
            .pb(tok::space::MD)
            .child(
                div()
                    .id("rail-search")
                    .role(Role::Button)
                    .aria_label("Go to project or session")
                    .h(tok::row::SEARCH)
                    .px(tok::space::MD)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::MD)
                    .rounded(tok::radius::CONTROL)
                    .bg(ui.overlay)
                    .border_1()
                    .border_color(border_resting)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.close_agents_menu(cx);
                        this.commit_agents_rename(cx);
                        this.open_command_palette(window, cx);
                    }))
                    // The glyph, the label and the chord all move together on
                    // hover, so the whole control reads as one target rather
                    // than as a row of three.
                    .animated_hover_element(move |button, delta| {
                        let label = lerp_color(label_resting, label_hover, delta);
                        button.style().border_color(lerp_color(
                            border_resting,
                            border_hover,
                            delta,
                        ));
                        button.extend([
                            // An asset rather than the `⌕` glyph the prototype
                            // uses: JetBrains Mono has no U+2315, and a
                            // fallback face draws it at the wrong weight and
                            // baseline. `svg()` must state its own colour, so
                            // it is emitted from inside this closure with the
                            // interpolated one.
                            svg()
                                .size(tok::mono::PATH)
                                .flex_none()
                                .path("icons/tool_search.svg")
                                .text_color(label)
                                .into_any_element(),
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(tok::text::CONTROL)
                                .text_color(label)
                                .child("Go to project or session")
                                .into_any_element(),
                            div()
                                .flex_none()
                                .font_family(tok::font::MONO)
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(tok::mono::LABEL)
                                .text_color(ui.faint)
                                .child(chord.clone())
                                .into_any_element(),
                        ]);
                    }),
            )
            .into_any_element()
    }
}
