//! "Appearance" settings page, in the design's row anatomy.
//!
//! The design gives this section five rows - theme, interface font, mono font,
//! density, show status bar - and the audit against it is mostly subtraction:
//!
//! - **Theme** is one row with one chip, which is what the design draws. The
//!   Light / Dark / System segment and the four preset tiles below it are gone;
//!   the tiles' one real service, letting someone see a palette before
//!   committing to it, moved into the menu, where each entry carries its own
//!   base / surface / accent swatches.
//! - **Interface font** is gone, deleted as prototype filler along
//!   with density and show-status-bar: there is one embedded interface font and
//!   nothing to choose, so even stating it was one row of noise. Appearance is
//!   one row, and that is the right size for it.
//! - **Sidebar transparency** (macOS) and **Chrome material** (Windows) are
//!   deleted. They are one decoration older than the current design, under two
//!   platform names; they are in no part of the mockup, and the design names
//!   every chrome surface by its colour. Both keys default off and stay readable - `serde` ignores what
//!   it does not know - so a machine that had one on keeps it; we simply stop
//!   offering it.
//! - **Mono font** moved to Terminal, beside the size it is always adjusted
//!   with. The design was changed to match.

use gpui::{
    AnyElement, ClickEvent, Context, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, div, prelude::*, px,
};

use crate::SplitlaneApp;
use crate::app::theme_picker::is_default_theme_name;
use crate::settings::components::{
    ChipTone, SETTING_ROW_MAX_WIDTH, secondary_button, select_item, select_menu,
    setting_choice_row, setting_note, value_chip, with_alpha,
};
use crate::ui_tokens as tok;

/// The label the theme row shows while the theme follows the OS.
const SYSTEM_LABEL: &str = "System";

impl SplitlaneApp {
    pub(crate) fn render_appearance_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let current_theme = self.current_theme_name();

        // "System" is a state of the *pair*: the mode follows the OS and the
        // resolved name is whichever of the two Harbor themes that gives. A
        // preset theme clears the mode, so the mode alone cannot answer this.
        let follows_system = self.theme_mode == crate::ThemeMode::System
            && is_default_theme_name(current_theme.as_str());
        let chip_label = if follows_system {
            SYSTEM_LABEL.to_string()
        } else {
            current_theme.clone()
        };

        let reset = secondary_button(
            "reset-theme",
            "Reset to default",
            ui,
            cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.reset_theme_selection(cx);
            }),
        );

        div()
            .flex()
            .flex_col()
            .child(setting_note(
                ui,
                "Harbor Dark and Harbor Light are the pair the interface is drawn for. \
                 The rest are terminal palettes kept from before it.",
            ))
            .child(self.theme_choice_row(chip_label, follows_system, &current_theme, ui, cx))
            .child(
                div()
                    .max_w(px(SETTING_ROW_MAX_WIDTH))
                    .pt(tok::space::SECTION)
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(reset),
            )
    }

    /// The theme row: one chip, and a menu that shows each theme's palette
    /// before it is chosen.
    fn theme_choice_row(
        &self,
        chip_label: String,
        follows_system: bool,
        current_theme: &str,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = self.appearance_theme_menu_open;
        let current_theme = current_theme.to_string();

        let menu = is_open.then(|| {
            let mut menu = select_menu("appearance-theme-menu", ui).on_mouse_down_out(cx.listener(
                |this, _, _w, cx| {
                    if this.appearance_theme_menu_open {
                        this.appearance_theme_menu_open = false;
                        cx.notify();
                    }
                },
            ));

            menu = menu.child(
                select_item("appearance-theme-system", follows_system, ui)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.appearance_theme_menu_open = false;
                        this.apply_theme_mode(crate::ThemeMode::System, window, cx);
                    }))
                    .child(system_swatches(ui))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(ui.text)
                            .child(SYSTEM_LABEL),
                    ),
            );

            for (index, (name, _)) in crate::theme::THEMES.iter().enumerate() {
                let selected = !follows_system && name.eq_ignore_ascii_case(&current_theme);
                menu = menu.child(
                    select_item(("appearance-theme", index), selected, ui)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.appearance_theme_menu_open = false;
                            this.apply_theme_by_name(name, cx);
                        }))
                        .child(theme_swatches(name))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(ui.text)
                                .child(*name),
                        ),
                );
            }
            menu.into_any_element()
        });

        setting_choice_row(
            ui,
            "appearance-theme",
            "Theme",
            follows_system.then(|| SharedString::from(current_theme)),
            value_chip(ui, chip_label, ChipTone::Plain),
            menu,
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.appearance_theme_menu_open = !is_open;
                this.settings_focus.focus(window, cx);
                cx.notify();
            }),
        )
        .into_any_element()
    }

    pub(crate) fn apply_theme_mode(
        &mut self,
        mode: crate::ThemeMode,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let name = mode.resolved_theme_name(window.appearance());
        self.persist_theme_selection(mode, name, cx);
    }

    pub(crate) fn sync_system_theme_from_window(
        &mut self,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.theme_mode != crate::ThemeMode::System {
            return;
        }
        let name = self.theme_mode.resolved_theme_name(window.appearance());
        if self.cached_config.theme.as_deref() == Some(name) {
            return;
        }
        self.persist_theme_selection(crate::ThemeMode::System, name, cx);
    }
}

/// Three stacked bars in a theme's own base / surface / accent, small enough to
/// lead a menu row and enough to tell a palette apart from the one under it.
/// This is what the four preview tiles were for, at the size a row can afford.
fn theme_swatches(name: &str) -> impl IntoElement {
    let theme = crate::theme::theme_by_name(name).unwrap_or_else(crate::theme::one_dark);
    let colors = crate::theme::ui_colors_with(&theme);
    swatch_stack(colors.base, colors.surface, colors.accent, colors.text)
}

/// The "System" entry's swatches: the pair it chooses between, so the row says
/// what following the OS actually resolves to.
fn system_swatches(ui: crate::theme::UiColors) -> impl IntoElement {
    let dark = crate::theme::theme_by_name(crate::theme::DEFAULT_THEME_NAME)
        .unwrap_or_else(crate::theme::one_dark);
    let light = crate::theme::theme_by_name(crate::theme::DEFAULT_LIGHT_THEME_NAME)
        .unwrap_or_else(crate::theme::one_dark);
    let dark = crate::theme::ui_colors_with(&dark);
    let light = crate::theme::ui_colors_with(&light);
    swatch_stack(dark.base, light.base, ui.accent, ui.text)
}

fn swatch_stack(top: Hsla, middle: Hsla, bottom: Hsla, outline: Hsla) -> impl IntoElement {
    let bar = |color: Hsla| div().w(px(14.)).h(px(4.)).rounded_full().bg(color);
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(2.))
        .rounded(tok::radius::BADGE)
        .border_1()
        .border_color(with_alpha(outline, 0.16))
        .child(bar(top))
        .child(bar(middle))
        .child(bar(bottom))
}
