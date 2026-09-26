//! Drawing the palette: the field, the three tabs, the rows, the footer.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, div, prelude::*, px, svg,
};

use crate::SplitlaneApp;
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

use super::{PaletteItem, PaletteTab};

/// How tall the result list gets before it starts scrolling.
const PALETTE_MAX_HEIGHT: f32 = 360.;
/// The design's dialog width.
const PALETTE_WIDTH: f32 = 560.;

impl SplitlaneApp {
    pub(crate) fn render_command_palette(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = crate::theme::ui_colors();
        let Some(state) = &self.command_palette else {
            return div().into_any_element();
        };
        let tab = state.tab;
        let items = self.command_palette_items(cx);
        let selected = state.selected.min(items.len().saturating_sub(1));
        // Keyboard navigation past the fold: the list scrolls, so the selected
        // row has to be pulled back into view every frame.
        self.command_palette_scroll.scroll_to_item(selected);

        let card = div()
            .id("command-palette")
            .occlude()
            .track_focus(&self.command_palette_focus)
            .on_key_down(cx.listener(Self::handle_command_palette_key_down))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.close_command_palette(window, cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .w(px(PALETTE_WIDTH))
            .flex()
            .flex_col()
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .rounded(tok::radius::WINDOW)
            .shadow(crate::ui_primitives::dialog_shadow(ui))
            .overflow_hidden()
            .child(self.palette_field(ui))
            .child(self.palette_tabs(tab, ui, cx))
            .child(self.palette_list(&items, selected, tab, ui, cx))
            .child(self.palette_footer(items.len(), ui));

        gpui::deferred(
            div()
                .id("command-palette-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(80.))
                .bg(ui.scrim)
                .child(card),
        )
        // Above every other overlay: the palette is the way out of a stuck
        // interface, so nothing may paint over it.
        .with_priority(10)
        .into_any_element()
    }

    fn palette_field(&self, ui: crate::theme::UiColors) -> AnyElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .px(tok::space::XL)
            .py(tok::space::MD)
            .child(
                // `svg()` paints its mask in its OWN text colour and inherits
                // nothing - a colourless icon here would simply not draw.
                svg()
                    .size(px(12.))
                    .flex_none()
                    .path("icons/tool_search.svg")
                    .text_color(ui.accent),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(tok::text::ROW)
                    .text_color(ui.text)
                    .child(self.command_palette_query.clone()),
            )
            .into_any_element()
    }

    /// The three tabs, and - on History - the sort the design asks it to state.
    fn palette_tabs(
        &self,
        active: PaletteTab,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::SM)
            .px(tok::space::XL)
            .pb(tok::space::MD)
            .border_b_1()
            .border_color(ui.border);

        for tab in PaletteTab::ALL {
            let is_active = tab == active;
            let (border, background, text) = if is_active {
                (ui.accent_border, ui.accent_surface, ui.accent)
            } else {
                (ui.border, ui.overlay, ui.text_secondary)
            };
            row = row.child(
                div()
                    .id(SharedString::from(format!("palette-tab-{}", tab.label())))
                    .flex_none()
                    .px(tok::space::MD)
                    .py(tok::space::XS)
                    .rounded(tok::radius::SMALL)
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .text_size(tok::text::CONTROL)
                    .text_color(text)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.set_command_palette_tab(tab, cx);
                        cx.stop_propagation();
                    }))
                    .child(tab.label()),
            );
        }

        if matches!(active, PaletteTab::History) {
            row = row.child(div().flex_1().min_w_0()).child(
                div()
                    .flex_none()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(ui.faint)
                    .child("sorted by last activity"),
            );
        }

        row.into_any_element()
    }

    fn palette_list(
        &self,
        items: &[PaletteItem],
        selected: usize,
        tab: PaletteTab,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = div()
            .id("command-palette-list")
            .flex()
            .flex_col()
            .max_h(px(PALETTE_MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.command_palette_scroll);

        if items.is_empty() {
            let message = match tab {
                PaletteTab::Output => self.palette_output_empty_message(),
                PaletteTab::History
                    if self
                        .command_palette
                        .as_ref()
                        .is_some_and(|state| state.history_loading()) =>
                {
                    "Reading the agents' session stores…"
                }
                _ => "Nothing matches.",
            };
            return list
                .child(
                    div()
                        .px(tok::space::XL)
                        .py(tok::space::XL)
                        .text_size(tok::text::ROW)
                        .text_color(ui.muted)
                        .child(message),
                )
                .into_any_element();
        }

        for (idx, item) in items.iter().enumerate() {
            list = list.child(self.palette_row(idx, item, idx == selected, ui, cx));
        }
        list.into_any_element()
    }

    fn palette_row(
        &self,
        idx: usize,
        item: &PaletteItem,
        is_selected: bool,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (label, detail, hint) = self.palette_row_text(item);
        let group = match item {
            PaletteItem::Surface { group, .. } => group.clone(),
            _ => None,
        };
        // `list_selection`, not `subtle`: `subtle` is what hover paints, so
        // the row the arrows are on and a row the pointer happens to rest over
        // used to be the same colour.
        let resting_background = if is_selected {
            ui.list_selection
        } else {
            ui.subtle.opacity(0.0)
        };
        div()
            .id(SharedString::from(format!("command-palette-row-{idx}")))
            .relative()
            // "inset 2px 0 0 accent" - GPUI has no inset shadow, so the bar is
            // a real child pinned to the leading edge, the way the rail's
            // focused row already draws one. It is what keeps the highlight
            // legible when the pointer is somewhere else on the list.
            .when(is_selected, |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(ui.accent),
                )
            })
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .px(tok::space::XL)
            .py(tok::space::XS)
            .text_size(tok::text::ROW)
            .bg(resting_background)
            .cursor_pointer()
            .animated_hover(move |style, delta| {
                // A selected row does not brighten under the pointer: it is
                // already the brighter of the two, and animating it down to
                // hover would read as losing the selection.
                let hovered = if is_selected {
                    ui.list_selection
                } else {
                    ui.subtle
                };
                style.bg(lerp_color(resting_background, hovered, delta));
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.activate_command_palette_row_public(idx, window, cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(ui.text)
                    .child(SharedString::from(label)),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(200.))
                    .truncate()
                    .text_color(ui.muted)
                    .child(SharedString::from(detail)),
            )
            .when_some(group, |row, group| {
                row.child(
                    div()
                        .flex_none()
                        .max_w(px(120.))
                        .truncate()
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::LABEL)
                        .text_color(ui.dim)
                        .child(SharedString::from(group)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(ui.dim)
                    .child(SharedString::from(hint)),
            )
            .into_any_element()
    }

    /// A row's three columns: what it is, where it lives, and the one fact the
    /// design puts at the right end.
    fn palette_row_text(&self, item: &PaletteItem) -> (String, String, String) {
        match item {
            PaletteItem::Action {
                label, group, key, ..
            } => (label.clone(), group.clone(), key.clone()),
            PaletteItem::Surface {
                label,
                detail,
                hint,
                ..
            } => (label.clone(), detail.clone(), hint.clone()),
            PaletteItem::History { index } => {
                let Some(row) = self
                    .command_palette
                    .as_ref()
                    .and_then(|state| state.history.as_ref())
                    .and_then(|rows| rows.get(*index))
                else {
                    return (String::new(), String::new(), String::new());
                };
                let detail = if row.branch.is_empty() {
                    row.project.clone()
                } else {
                    format!("{} · {}", row.project, row.branch)
                };
                (
                    row.title.clone(),
                    detail,
                    format!("last active {}", relative_age(row.last_activity_secs)),
                )
            }
            PaletteItem::Output { index } => {
                let Some(row) = self
                    .command_palette
                    .as_ref()
                    .and_then(|state| state.output.get(*index))
                else {
                    return (String::new(), String::new(), String::new());
                };
                let matches = if row.count == 1 {
                    "output · 1 match".to_string()
                } else {
                    format!("output · {} matches", row.count)
                };
                (row.name.clone(), row.project.clone(), matches)
            }
        }
    }

    fn palette_footer(&self, count: usize, ui: crate::theme::UiColors) -> AnyElement {
        div()
            .px(tok::space::XL)
            .py(tok::space::MD)
            .border_t_1()
            .border_color(ui.border)
            .text_size(tok::text::CAPTION)
            .text_color(ui.muted)
            .child(format!(
                "{count} result(s) · ↑↓ to move · ⇥ switches tab · Enter to run · Esc closes"
            ))
            .into_any_element()
    }
}

/// How long ago, in the coarsest unit that still says something. The design
/// writes an absolute stamp ("closed Mon 11:40"); an age is the fact this list
/// is sorted by, so it is the one worth printing beside the sort.
fn relative_age(unix_secs: i64) -> String {
    if unix_secs <= 0 {
        return "unknown".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let delta = (now - unix_secs).max(0);
    match delta {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86_399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::relative_age;

    #[test]
    fn an_unreadable_stamp_says_so_rather_than_lying() {
        assert_eq!(relative_age(0), "unknown");
        assert_eq!(relative_age(-5), "unknown");
    }

    #[test]
    fn the_age_uses_the_coarsest_unit_that_still_says_something() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(relative_age(now), "just now");
        assert_eq!(relative_age(now - 300), "5m ago");
        assert_eq!(relative_age(now - 7200), "2h ago");
        assert_eq!(relative_age(now - 3 * 86_400), "3d ago");
    }
}
