//! "Terminal" settings page, in the design's row anatomy.
//!
//! The audit against the design's Terminal section - shell, scrollback, font size, copy on select, bell,
//! prompt jumping, copy mode - moved two rows and added two:
//!
//! - **Shell** came in from General, where the app had put it and the design
//!   does not.
//! - **Scrollback** gains its first screen. `terminal.scrollback_lines` was
//!   read and resolved per surface profile, and settable only by hand.
//! - **Jump to previous / next prompt** is the `shell_integration` key told the
//!   truth: prompt jumping is what the OSC 133 marks are *for*, so the row that
//!   turns them off is the row the design draws. The hint carries the two
//!   chords as they are actually bound, not as the (macOS-only) design writes
//!   them.
//! - Cursor shape and colour, line height, cell width, font weight, integrated
//!   glyphs and colour emoji are not on the design's list and stay: each is a
//!   live terminal preference of exactly the kind this section is for, and the
//!   design's list is a prototype's sample rather than a closed set. What was
//!   deleted in this pass is the older decoration, not the typography.
//! - **Mono font** stays here rather than moving to Appearance. Ours is the
//!   terminal's font and nothing else's, and splitting it from the size it is
//!   always adjusted with would cost more than the design's Appearance grouping
//!   bought. The design was changed to match - so the name is the design's now,
//!   and only the home is ours.
//!
//! Copy on select, the bell and copy mode were on the design's list and are
//! gone from it: all three were struck as prototype filler, so they are not
//! a debt this section owes.
//!
//! Every choice writes through [`SplitlaneApp::persist_setting`] - cache-mutate,
//! repaint, off-thread write - with `nested` routing to the `terminal` block
//! (`config_writer::save_terminal_field`) or to a top-level key.

use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Rgba, SharedString, Styled, div, prelude::*, px,
};
use serde_json::{Value, json};

use splitlane_config::schema::{CursorShapeConfig, normalize_hex_color};

use crate::settings::components::thousands;
use crate::settings::components::{
    ChipTone, SETTINGS_CONTROL_CORNER_RADIUS, chip_skin, menu_surface, select_item, select_menu,
    setting_choice_row, setting_note, setting_toggle_row, value_chip,
};
use crate::ui_primitives::AnimatedHoverExt;

use crate::ui_tokens as tok;
use crate::{SplitlaneApp, TerminalDropdown};

const FONT_WEIGHT_OPTIONS: [(&str, &str); 11] = [
    ("Thin", "thin"),
    ("Extra-light", "extra_light"),
    ("Light", "light"),
    ("Semi light", "semi_light"),
    ("Normal", "normal"),
    ("Medium", "medium"),
    ("Semi-bold", "semi_bold"),
    ("Bold", "bold"),
    ("Extra-bold", "extra_bold"),
    ("Black", "black"),
    ("Extra-black", "extra_black"),
];

/// The scrollback depths offered, in lines. The resolver clamps to
/// `[MIN_SCROLLBACK_LINES, MAX_SCROLLBACK_LINES]`, and these sit inside it; a
/// value set by hand outside the list still shows in the chip and simply
/// matches nothing.
const SCROLLBACK_OPTIONS: [usize; 7] = [1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000];

const CURSOR_COLOR_SWATCHES: [u32; 16] = [
    0x007aff, 0x0a84ff, 0x5aa6ff, 0x57d5c4, 0x57d992, 0xffd166, 0xff6f6a, 0xc79bff, 0x3f4451,
    0xf0f3f7, 0x4c6fff, 0x315ecf, 0x40c878, 0xf89850, 0xf87878, 0xd8d0d0,
];

fn hex_string_from_u32(hex: u32) -> String {
    format!("#{hex:06X}")
}

fn hsla_from_u32(hex: u32) -> Hsla {
    Hsla::from(gpui::rgb(hex))
}

fn lighter_control_hover(base: Hsla) -> Hsla {
    Hsla {
        l: (base.l + 0.045).min(1.0),
        ..base
    }
}

fn hex_string_from_hsla(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let channel = |value: f32| -> u8 { (value.clamp(0.0, 1.0) * 255.0).round() as u8 };
    format!(
        "#{:02X}{:02X}{:02X}",
        channel(rgba.r),
        channel(rgba.g),
        channel(rgba.b)
    )
}

/// A numeric option list, `min..=max` by `step`, rendered to `decimals` places.
fn numeric_options(min: f64, max: f64, step: f64, decimals: usize, current: f64) -> Vec<Choice> {
    let factor = 10f64.powi(decimals as i32);
    let round = |v: f64| (v * factor).round() / factor;
    let count = ((max - min) / step).round() as usize;
    (0..=count)
        .map(|i| {
            let value = round(min + step * i as f64);
            Choice {
                label: format!("{value:.decimals$}"),
                value: json!(value),
                selected: (value - current).abs() < step / 2.0,
            }
        })
        .collect()
}

/// One entry of a choice row's menu.
struct Choice {
    label: String,
    value: Value,
    selected: bool,
}

impl SplitlaneApp {
    pub(crate) fn render_terminal_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Read the cached config (no per-frame `load_config()`).
        let config = &self.cached_config;
        let ui = crate::theme::ui_colors();
        let terminal = config.terminal.clone().unwrap_or_default();

        // ── current values ──────────────────────────────────────────────
        let shape = terminal.cursor_shape.unwrap_or_default();
        let integrated_glyphs = terminal.resolved_integrated_glyphs();
        let color_emoji = terminal.resolved_color_emoji();
        let scrollback = terminal.resolved_scrollback_lines();
        let configured_cursor_color = terminal
            .cursor_color
            .as_deref()
            .and_then(normalize_hex_color);
        let theme_cursor_hex = hex_string_from_hsla(crate::theme::active_theme().cursor);
        let cursor_color_hex = configured_cursor_color
            .clone()
            .unwrap_or_else(|| theme_cursor_hex.clone());
        let cursor_uses_theme = configured_cursor_color.is_none();
        let current_font =
            crate::terminal::element::resolve_font_family(config.font_family.as_deref());
        let font_weight_key =
            crate::terminal::element::normalize_font_weight_key(config.font_weight.as_deref());
        let font_size = config
            .font_size
            .unwrap_or(crate::terminal::element::DEFAULT_FONT_SIZE) as f64;
        let line_height = config
            .line_height
            .unwrap_or(crate::terminal::element::DEFAULT_LINE_HEIGHT)
            as f64;
        let cell_width = config
            .cell_width
            .unwrap_or(crate::terminal::element::DEFAULT_CELL_WIDTH)
            as f64;
        let shell_integration = config.shell_integration.unwrap_or(true);

        let shape_label = match shape {
            CursorShapeConfig::Vintage => "Vintage (_\u{2582})",
            CursorShapeConfig::Block => "Filled box (\u{2588})",
            CursorShapeConfig::Beam => "Bar (|)",
            CursorShapeConfig::Underline => "Underline (_)",
            CursorShapeConfig::DoubleUnderline => "Double underline (\u{203f})",
            CursorShapeConfig::Hollow => "Empty box (\u{25a1})",
        };
        let font_weight_label = FONT_WEIGHT_OPTIONS
            .iter()
            .find_map(|(label, key)| (*key == font_weight_key).then_some(*label))
            .unwrap_or("Normal");

        // ── Shell (default_shell) ───────────────────────────────────────
        // Order mirrors `terminal::shell`'s resolver preference. Any other value
        // still works via config; the chip shows the raw value when it does not
        // match a preset, or "system default" when unset.
        #[cfg(target_os = "windows")]
        let shells: Vec<(&str, String)> = vec![
            ("PowerShell", "pwsh.exe".to_string()),
            ("Windows PowerShell", "powershell.exe".to_string()),
            ("Command Prompt", "cmd.exe".to_string()),
            (
                "Git Bash",
                crate::terminal::shell::find_windows_git_bash()
                    .unwrap_or_else(|| "bash.exe".to_string()),
            ),
        ];
        #[cfg(not(target_os = "windows"))]
        let shells: Vec<(&str, String)> = vec![
            ("zsh", "/bin/zsh".to_string()),
            ("bash", "/bin/bash".to_string()),
            ("sh", "/bin/sh".to_string()),
            ("fish", "/usr/bin/fish".to_string()),
        ];
        let current_shell = config.default_shell.clone().unwrap_or_default();
        let shell_opts: Vec<Choice> = shells
            .iter()
            .map(|(label, val)| Choice {
                label: (*label).to_string(),
                value: Value::String(val.clone()),
                selected: shell_preset_eq(&current_shell, val),
            })
            .collect();
        let shell_label = shell_opts
            .iter()
            .find(|choice| choice.selected)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| {
                if current_shell.is_empty() {
                    "system default".to_string()
                } else {
                    current_shell.clone()
                }
            });

        let scrollback_opts: Vec<Choice> = SCROLLBACK_OPTIONS
            .iter()
            .map(|lines| Choice {
                label: format!("{} lines", thousands(*lines)),
                value: json!(lines),
                selected: *lines == scrollback,
            })
            .collect();

        let shape_opts: Vec<Choice> = [
            ("Vintage (_\u{2582})", "vintage", CursorShapeConfig::Vintage),
            ("Bar (|)", "beam", CursorShapeConfig::Beam),
            ("Underline (_)", "underline", CursorShapeConfig::Underline),
            (
                "Double underline (\u{203f})",
                "double_underline",
                CursorShapeConfig::DoubleUnderline,
            ),
            ("Filled box (\u{2588})", "block", CursorShapeConfig::Block),
            ("Empty box (\u{25a1})", "hollow", CursorShapeConfig::Hollow),
        ]
        .into_iter()
        .map(|(label, key, variant)| Choice {
            label: label.to_string(),
            value: json!(key),
            selected: shape == variant,
        })
        .collect();

        let font_weight_opts: Vec<Choice> = FONT_WEIGHT_OPTIONS
            .iter()
            .map(|(label, key)| Choice {
                label: (*label).to_string(),
                value: json!(*key),
                selected: *key == font_weight_key,
            })
            .collect();

        let jump_hint = self.prompt_jump_chords();

        let rows = div()
            .flex()
            .flex_col()
            .child(self.terminal_choice_row(
                TerminalDropdown::Shell,
                "Shell",
                Some(SharedString::from("new terminals only")),
                shell_label,
                shell_opts,
                "default_shell",
                false,
                ui,
                cx,
            ))
            .child(self.terminal_choice_row(
                TerminalDropdown::Scrollback,
                "Scrollback",
                Some(SharedString::from("per shell")),
                format!("{} lines", thousands(scrollback)),
                scrollback_opts,
                "scrollback_lines",
                true,
                ui,
                cx,
            ))
            .child(self.terminal_font_family_row(current_font, ui, cx))
            .child(self.terminal_choice_row(
                TerminalDropdown::FontSize,
                "Font size",
                None,
                format!("{font_size:.0} pt"),
                numeric_options(8.0, 32.0, 1.0, 0, font_size),
                "font_size",
                false,
                ui,
                cx,
            ))
            .child(self.terminal_choice_row(
                TerminalDropdown::LineHeight,
                "Line height",
                None,
                format!("{line_height:.1}"),
                numeric_options(1.0, 2.5, 0.1, 1, line_height),
                "line_height",
                false,
                ui,
                cx,
            ))
            .child(self.terminal_choice_row(
                TerminalDropdown::CellWidth,
                "Cell width",
                None,
                format!("{cell_width:.1}"),
                numeric_options(0.3, 2.0, 0.1, 1, cell_width),
                "cell_width",
                false,
                ui,
                cx,
            ))
            .child(self.terminal_choice_row(
                TerminalDropdown::FontWeight,
                "Font weight",
                None,
                font_weight_label.to_string(),
                font_weight_opts,
                "font_weight",
                false,
                ui,
                cx,
            ))
            .child(self.terminal_choice_row(
                TerminalDropdown::CursorShape,
                "Cursor shape",
                Some(SharedString::from("until an application overrides it")),
                shape_label.to_string(),
                shape_opts,
                "cursor_shape",
                true,
                ui,
                cx,
            ))
            .child(self.terminal_cursor_color_row(
                cursor_color_hex,
                cursor_uses_theme,
                theme_cursor_hex,
                ui,
                cx,
            ))
            .child(setting_toggle_row(
                ui,
                "term-integrated-glyphs",
                "Integrated glyphs",
                Some(SharedString::from("block elements drawn by us")),
                integrated_glyphs,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(
                        true,
                        "integrated_glyphs",
                        Value::Bool(!integrated_glyphs),
                        cx,
                    );
                }),
            ))
            .child(setting_toggle_row(
                ui,
                "term-color-emoji",
                "Colour emoji",
                None,
                color_emoji,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(true, "color_emoji", Value::Bool(!color_emoji), cx);
                }),
            ))
            .child(setting_toggle_row(
                ui,
                "term-shell-integration",
                "Jump to previous / next prompt",
                Some(jump_hint),
                shell_integration,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(
                        false,
                        "shell_integration",
                        Value::Bool(!shell_integration),
                        cx,
                    );
                }),
            ));

        #[cfg(target_os = "windows")]
        let rows = {
            let material = config.windows_terminal_material_enabled();
            rows.child(setting_toggle_row(
                ui,
                "term-windows-terminal-material",
                "Acrylic material",
                Some(SharedString::from(
                    "a translucent texture behind the window",
                )),
                material,
                cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.persist_setting(
                        false,
                        "windows_terminal_material",
                        Value::Bool(!material),
                        cx,
                    );
                }),
            ))
        };

        div()
            .flex()
            .flex_col()
            .child(setting_note(
                ui,
                "Applies to every surface that is a terminal - a shell, and an agent \
                 session, which is the agent's own terminal.",
            ))
            .child(rows)
    }

    /// The chords `jump_prev_prompt` / `jump_next_prompt` are actually bound to,
    /// resolved through the same registry the Shortcuts screen reads. The
    /// design writes `⌥↑ ⌥↓`, which is its own macOS table rather than ours;
    /// a hint that names a chord the app does not answer is worse than none.
    fn prompt_jump_chords(&self) -> SharedString {
        let shortcuts = crate::keybindings::effective_shortcuts(&self.cached_config.shortcuts);
        let chord = |action: &str| {
            shortcuts
                .iter()
                .find(|entry| entry.action_name == action && entry.key != "Unassigned")
                .map(|entry| entry.key.clone())
        };
        match (chord("jump_prev_prompt"), chord("jump_next_prompt")) {
            (Some(prev), Some(next)) => SharedString::from(format!("{prev} {next}")),
            _ => SharedString::from("shell integration marks"),
        }
    }

    /// One Terminal choice row: the design's anatomy, with the chip opening a
    /// menu of `options`. `nested` routes the write to the `terminal` block vs.
    /// a top-level key.
    #[allow(clippy::too_many_arguments)]
    fn terminal_choice_row(
        &self,
        which: TerminalDropdown,
        label: &'static str,
        hint: Option<SharedString>,
        current_label: String,
        options: Vec<Choice>,
        config_key: &'static str,
        nested: bool,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = self.terminal_dropdown == Some(which);

        let menu = is_open.then(|| {
            let mut menu =
                select_menu(SharedString::from(format!("term-dd-list-{config_key}")), ui)
                    .on_mouse_down_out(cx.listener(move |this, _, _w, cx| {
                        if this.terminal_dropdown == Some(which) {
                            this.terminal_dropdown = None;
                            cx.notify();
                        }
                    }));
            for (i, choice) in options.into_iter().enumerate() {
                let value_for_click = choice.value;
                menu = menu.child(
                    select_item((config_key, i), choice.selected, ui)
                        .cursor(CursorStyle::Arrow)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.terminal_dropdown = None;
                            this.persist_setting(nested, config_key, value_for_click.clone(), cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(ui.text)
                                .child(choice.label),
                        ),
                );
            }
            menu.into_any_element()
        });

        setting_choice_row(
            ui,
            SharedString::from(format!("term-dd-{config_key}")),
            label,
            hint,
            value_chip(ui, current_label, ChipTone::Plain),
            menu,
            // Decide open/close from the render-time snapshot, not the live
            // state: the menu's `on_mouse_down_out` fires on this same press and
            // may have already cleared it, so a live toggle would re-open.
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.font_dropdown_open = false;
                this.font_search.clear();
                this.terminal_dropdown = if is_open { None } else { Some(which) };
                this.settings_focus.focus(window, cx);
                cx.notify();
            }),
        )
        .into_any_element()
    }

    /// The font row: a choice row whose chip becomes the search field while the
    /// menu is open, so a machine with three hundred monospace families is
    /// still one keystroke from the right one.
    fn terminal_font_family_row(
        &self,
        current_font: String,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let default_font = crate::terminal::element::resolve_font_family(None);
        let open = self.font_dropdown_open;
        let (chip_label, searching) = if open {
            if self.font_search.is_empty() {
                ("Search fonts\u{2026}".to_string(), true)
            } else {
                (format!("{}|", self.font_search), false)
            }
        } else {
            (current_font.clone(), false)
        };
        let chip = chip_skin(ui, ChipTone::Plain)
            .when(searching, |chip| chip.text_color(ui.muted))
            .child(chip_label);

        let menu = open.then(|| {
            let search = self.font_search.to_lowercase();
            let default_label = format!("Splitlane default \u{2014} {default_font}");
            let default_matches =
                search.is_empty() || default_label.to_lowercase().contains(&search);
            let filtered: Vec<&String> = self
                .mono_font_names
                .iter()
                .filter(|name| {
                    name.as_str() != default_font.as_str()
                        && (search.is_empty() || name.to_lowercase().contains(&search))
                })
                .collect();

            let mut menu = select_menu("terminal-font-dropdown", ui).on_mouse_down_out(
                cx.listener(|this, _, _w, cx| {
                    if this.font_dropdown_open {
                        this.font_dropdown_open = false;
                        this.font_search.clear();
                        cx.notify();
                    }
                }),
            );

            if default_matches {
                menu = menu.child(
                    select_item(
                        ("terminal-font-default", 0usize),
                        current_font == default_font,
                        ui,
                    )
                    .cursor(CursorStyle::Arrow)
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.font_dropdown_open = false;
                        this.font_search.clear();
                        this.persist_setting(false, "font_family", Value::Null, cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(ui.text)
                            .child(default_label),
                    ),
                );
            }

            for (i, name) in filtered.iter().enumerate() {
                let name_owned = (*name).clone();
                let is_current = **name == current_font;
                menu = menu.child(
                    select_item(("terminal-font", i), is_current, ui)
                        .cursor(CursorStyle::Arrow)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.font_dropdown_open = false;
                            this.font_search.clear();
                            this.persist_setting(
                                false,
                                "font_family",
                                Value::String(name_owned.clone()),
                                cx,
                            );
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(ui.text)
                                .child((*name).clone()),
                        ),
                );
            }

            if !default_matches && filtered.is_empty() {
                menu = menu.child(
                    div()
                        .px(tok::space::MD)
                        .py(tok::space::MD)
                        .text_size(tok::text::ROW)
                        .text_color(ui.muted)
                        .child("No matching fonts"),
                );
            }

            menu.into_any_element()
        });

        setting_choice_row(
            ui,
            "terminal-font-family",
            "Mono font",
            None,
            chip,
            menu,
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.terminal_dropdown = None;
                this.font_dropdown_open = !open;
                this.font_search.clear();
                if this.font_dropdown_open && this.mono_font_names.is_empty() {
                    cx.spawn(async move |this, cx| {
                        let fonts = smol::unblock(crate::fonts::load_mono_fonts).await;
                        let _ = this.update(cx, |this, cx| {
                            this.mono_font_names = fonts;
                            cx.notify();
                        });
                    })
                    .detach();
                }
                this.settings_focus.focus(window, cx);
                cx.notify();
            }),
        )
        .into_any_element()
    }

    /// The cursor colour row. Its chip leads with the colour itself: a colour
    /// named only by its hex is a colour nobody can see, and the row is the one
    /// place in this section where the value is not a word.
    fn terminal_cursor_color_row(
        &self,
        current_hex: String,
        uses_theme: bool,
        theme_hex: String,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = self.terminal_dropdown == Some(TerminalDropdown::CursorColor);
        let current_color = crate::terminal::view::hsla_from_hex_color(&current_hex)
            .unwrap_or_else(|| hsla_from_u32(0x007aff));
        let theme_color = crate::terminal::view::hsla_from_hex_color(&theme_hex)
            .unwrap_or_else(|| hsla_from_u32(0x007aff));

        let chip = chip_skin(ui, ChipTone::Plain)
            .child(
                div()
                    .w(px(10.))
                    .h(px(10.))
                    .flex_none()
                    .rounded(tok::radius::METER)
                    .bg(current_color),
            )
            .child(current_hex.clone());

        let menu = is_open.then(|| {
            let mut swatch_grid = div().flex().flex_col().gap(tok::space::XS);
            for (row_idx, chunk) in CURSOR_COLOR_SWATCHES.chunks(4).enumerate() {
                let mut swatch_row = div().flex().flex_row().gap(tok::space::XS);
                for (col_idx, &hex) in chunk.iter().enumerate() {
                    let hex_string = hex_string_from_u32(hex);
                    let selected = !uses_theme && hex_string == current_hex;
                    let resting_opacity = if selected { 1.0 } else { 0.92 };
                    let value = hex_string.clone();
                    swatch_row = swatch_row.child(
                        div()
                            .id(SharedString::from(format!(
                                "term-cursor-color-{row_idx}-{col_idx}"
                            )))
                            .w(px(28.))
                            .h(px(28.))
                            .rounded(tok::radius::SMALL)
                            .bg(hsla_from_u32(hex))
                            .opacity(resting_opacity)
                            .animated_hover(move |style, delta| {
                                style.opacity(resting_opacity + (1.0 - resting_opacity) * delta);
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                this.terminal_dropdown = None;
                                this.persist_setting(
                                    true,
                                    "cursor_color",
                                    Value::String(value.clone()),
                                    cx,
                                );
                            })),
                    );
                }
                swatch_grid = swatch_grid.child(swatch_row);
            }

            let scheme_bg = if uses_theme {
                ui.accent_surface
            } else {
                ui.subtle
            };
            let scheme_text = if uses_theme { ui.accent } else { ui.text };
            let scheme_hover_bg = if uses_theme {
                scheme_bg
            } else {
                lighter_control_hover(ui.subtle)
            };
            let scheme_row = div()
                .id("term-cursor-color-theme")
                .h(px(28.))
                .px(tok::space::MD)
                .rounded(SETTINGS_CONTROL_CORNER_RADIUS)
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::MD)
                .bg(scheme_bg)
                .animated_hover_bg(scheme_bg, scheme_hover_bg)
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.terminal_dropdown = None;
                    this.persist_setting(true, "cursor_color", Value::Null, cx);
                }))
                .child(
                    div()
                        .w(px(12.))
                        .h(px(12.))
                        .flex_none()
                        .rounded(tok::radius::BADGE)
                        .bg(theme_color),
                )
                .child(
                    div()
                        .text_size(tok::text::ROW)
                        .text_color(scheme_text)
                        .child("From the colour scheme"),
                );

            menu_surface(div().id("term-cursor-color-menu"), ui)
                .flex()
                .flex_col()
                .gap(tok::space::MD)
                .p(tok::space::XL)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(|this, _, _w, cx| {
                    if this.terminal_dropdown == Some(TerminalDropdown::CursorColor) {
                        this.terminal_dropdown = None;
                        cx.notify();
                    }
                }))
                .child(swatch_grid)
                .child(scheme_row)
                .into_any_element()
        });

        setting_choice_row(
            ui,
            "term-cursor-color",
            "Cursor colour",
            uses_theme.then(|| SharedString::from("from the colour scheme")),
            chip,
            menu,
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.font_dropdown_open = false;
                this.font_search.clear();
                this.terminal_dropdown = if is_open {
                    None
                } else {
                    Some(TerminalDropdown::CursorColor)
                };
                this.settings_focus.focus(window, cx);
                cx.notify();
            }),
        )
        .into_any_element()
    }
}

/// Case-insensitive comparison for shell presets. Bare configured names match
/// by basename (`bash.exe` should still select Git Bash), while two explicit
/// paths must point at the same executable (`C:\Windows\System32\bash.exe`
/// should not be presented as Git Bash).
fn shell_preset_eq(stored: &str, chip: &str) -> bool {
    fn has_separator(s: &str) -> bool {
        s.contains(['/', '\\'])
    }

    fn path_key(s: &str) -> String {
        s.replace('/', "\\").to_ascii_lowercase()
    }

    fn stem(s: &str) -> String {
        let base = s
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(s)
            .to_ascii_lowercase();
        base.trim_end_matches(".exe").to_string()
    }

    if stored.is_empty() {
        false
    } else if has_separator(stored) && has_separator(chip) {
        path_key(stored) == path_key(chip)
    } else {
        stem(stored) == stem(chip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splitlane_config::schema::TerminalConfig;

    #[test]
    fn shell_preset_matches_bare_names_by_basename() {
        assert!(shell_preset_eq(
            "bash.exe",
            r"C:\Program Files\Git\bin\bash.exe"
        ));
        assert!(shell_preset_eq(
            r"C:\Program Files\Git\bin\bash.exe",
            "bash.exe"
        ));
    }

    #[test]
    fn shell_preset_does_not_label_explicit_wsl_bash_as_git_bash() {
        assert!(!shell_preset_eq(
            r"C:\Windows\System32\bash.exe",
            r"C:\Program Files\Git\bin\bash.exe"
        ));
    }

    /// The chip reads "10 000 lines", not "10000 lines" - the design groups
    /// the number and the row is read at a glance.
    #[test]
    fn a_scrollback_depth_is_grouped() {
        assert_eq!(thousands(10_000), "10 000");
        assert_eq!(thousands(100_000), "100 000");
        assert_eq!(thousands(500), "500");
    }

    /// Every offered scrollback depth has to survive the resolver, or the row
    /// would show a value the terminal does not use.
    #[test]
    fn every_offered_scrollback_depth_is_within_the_resolver_range() {
        for lines in SCROLLBACK_OPTIONS {
            assert!(lines >= TerminalConfig::MIN_SCROLLBACK_LINES);
            assert!(lines <= TerminalConfig::MAX_SCROLLBACK_LINES);
            let config = TerminalConfig {
                scrollback_lines: Some(lines),
                ..Default::default()
            };
            assert_eq!(config.resolved_scrollback_lines(), lines);
        }
    }

    /// The numeric menus have to contain the value the chip shows, or the open
    /// menu would highlight nothing on a freshly installed machine.
    #[test]
    fn a_numeric_menu_selects_the_current_value() {
        let options = numeric_options(8.0, 32.0, 1.0, 0, 14.0);
        assert_eq!(options.len(), 25);
        let selected: Vec<&str> = options
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(selected, vec!["14"]);

        let options = numeric_options(1.0, 2.5, 0.1, 1, 1.2);
        let selected: Vec<&str> = options
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(selected, vec!["1.2"]);
    }
}
