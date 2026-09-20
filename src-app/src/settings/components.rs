//! Agents-style UI primitives shared across every settings tab.
//!
//! Visual recipes mirror `agents_view::view` + `app::agents_sidebar`:
//! - **section_header** - lowercase eyebrow (11px, NORMAL, `ui.muted`), no
//!   border below. Matches `threads_section_header` in the agents sidebar.
//! - **section_header_with_action** - same eyebrow with a right-aligned
//!   secondary button (used by Shortcuts/Appearance "Reset to defaults").
//! - **setting_card** - explicit theme-aware panel (white/`#e5e5ed` in light,
//!   `#232323`/`#303030` in dark) with a 1px border and a generous Apple-
//!   approximating radius. Wraps row groups so each section reads as a card the
//!   way Agents content cards do.
//! - **hairline** - 1px row separator (border at ~50% alpha), used inside
//!   cards to split rows without competing with the card border.
//! - **secondary_button** - filled, agents cancel-button style
//!   (`ui.subtle` bg, no border).
//!
//! All helpers return `impl IntoElement` or `Div`. They take no listeners
//! beyond the explicit on_click on `secondary_button` - parent rows wire
//! their own `.id()` and `.on_click()`.

use gpui::{
    AnyElement, ClickEvent, CursorStyle, Div, ElementId, Hsla, InteractiveElement, IntoElement,
    ParentElement, Pixels, SharedString, Stateful, Styled, deferred, div, img, prelude::*, px, svg,
};

use crate::ui_primitives::{AnimatedHover, AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

pub(crate) const SETTINGS_CONTROL_CORNER_RADIUS: Pixels = px(8.);

/// The design's setting row: 38 tall, never wider than 620. Both are
/// composition rather than typography, which is why they live here and not in
/// `ui_tokens` - see that module's own note on what it deliberately excludes.
pub const SETTING_ROW_HEIGHT: f32 = 38.;
pub const SETTING_ROW_MAX_WIDTH: f32 = 620.;
const SETTINGS_CARD_CORNER_RADIUS: Pixels = px(8.);

/// Apply an alpha override to an `Hsla` color. GPUI's `Hsla` has no
/// dedicated builder method for alpha, so we update the field manually.
pub fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

/// Lowercase eyebrow section label (11px, NORMAL, muted). No border below.
/// Mirrors `app::agents_sidebar::threads_section_header`.
pub fn section_header(ui: crate::theme::UiColors, label: &'static str) -> impl IntoElement {
    div().pb(tok::space::MD).child(
        div()
            .text_size(crate::ui_primitives::LABEL)
            .font_weight(gpui::FontWeight::NORMAL)
            .text_color(ui.muted)
            .child(label),
    )
}

/// Eyebrow header with a right-aligned action element (typically a
/// `secondary_button`). Same typography as `section_header`.
pub fn section_header_with_action(
    ui: crate::theme::UiColors,
    label: &'static str,
    action: impl IntoElement,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(tok::space::XL)
        .pb(tok::space::MD)
        .child(
            div()
                .text_size(crate::ui_primitives::LABEL)
                .font_weight(gpui::FontWeight::NORMAL)
                .text_color(ui.muted)
                .child(label),
        )
        .child(action)
}

/// Card fill + border shared by every settings card.
///
/// The fill is [`select_menu_surface`] - the app's one rule for "a step above
/// the surface it sits on", white-ish in light and a touch lighter than the
/// card in dark - and the border is the theme's `border`.
///
/// Both used to be hardcoded pairs picked by the active theme's lightness
/// (`#ffffff`/`#e5e5ed` light, `#232323`/`#303030` dark), which reproduced
/// Splitlane Light and One Dark almost exactly and no other theme at all. On
/// Harbor Light in particular the cool `#e5e5ed` border all but vanished
/// against the warm grey page, which is precisely how a light theme breaks:
/// not in the palette, on the screen.
///
/// Exposed so a bespoke card can match `setting_card` exactly.
pub fn card_colors() -> (Hsla, Hsla) {
    let ui = crate::theme::ui_colors();
    (select_menu_surface(ui), ui.border)
}

/// Card container for grouped setting rows. Codex-style polish: an explicit
/// theme-aware fill/border (see [`card_colors`]) - not the theme `ui.surface` -
/// plus a generous Apple-approximating corner radius. `_ui` is retained for
/// call-site compatibility (every tab already has it in scope).
///
/// Returns a `Div` so callers can chain `.child()` to add rows.
pub fn setting_card(_ui: crate::theme::UiColors) -> Div {
    let (bg, border) = card_colors();
    div()
        .flex()
        .flex_col()
        .bg(bg)
        .border_1()
        .border_color(border)
        .rounded(SETTINGS_CARD_CORNER_RADIUS)
        .overflow_hidden()
}

/// 1px hairline divider used between rows inside a `setting_card`.
/// Half-alpha border so it reads as a separator without competing with
/// the card outline.
pub fn hairline(ui: crate::theme::UiColors) -> impl IntoElement {
    div().h(px(1.)).w_full().bg(with_alpha(ui.border, 0.5))
}

/// Filled secondary button (agents cancel-button style): `ui.subtle` bg,
/// no border, whisper-soft text wash on hover. Used for "Reset to defaults"
/// and similar inline actions inside section headers.
pub fn secondary_button(
    id: &'static str,
    label: &'static str,
    ui: crate::theme::UiColors,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let hover_bg = lerp_color(ui.subtle, ui.text, 0.06);

    div()
        .id(id)
        .px(tok::space::MD)
        .py(tok::space::XS)
        .rounded(SETTINGS_CONTROL_CORNER_RADIUS)
        .bg(ui.subtle)
        .text_size(tok::text::ROW)
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(ui.text)
        .animated_hover_bg(ui.subtle, hover_bg)
        .child(label)
        .cursor_pointer()
        .on_click(on_click)
}

// ── Codex-style select / dropdown primitives ─────────────────────────────
//
// Shared by the General, Themes (font picker) and Terminal settings pages so
// every settings dropdown is identical: a subtle-gray trigger pill with an
// up/down selector glyph, opening an elevated, hairline-bordered
// menu with whisper-soft row highlights and click-outside-to-close. Callers own
// the "which dropdown is open" state and wire the handlers; these only style.

/// A leading logo for a select option: `(asset path, multicolor)`. Multicolor
/// brand logos render via `img()` (resvg keeps every fill); monochrome
/// `currentColor` SVGs render via a `text_color`-tinted `svg()` mask so they
/// follow the light/dark theme.
pub type Logo = (&'static str, bool);

/// Render a 14px leading logo (see [`Logo`]).
pub fn render_logo(logo: Logo, ui: crate::theme::UiColors) -> AnyElement {
    let (path, multicolor) = logo;
    if multicolor {
        img(path).size(px(12.)).flex_none().into_any_element()
    } else {
        svg()
            .size(px(12.))
            .flex_none()
            .path(path)
            .text_color(ui.text)
            .into_any_element()
    }
}

/// The elevated surface color used by [`select_menu`]: white-ish lift in light,
/// a touch lighter than the card in dark. Exposed so menus that cannot reuse the
/// fixed-width [`select_menu`] container (e.g. a stretch-to-width sidebar
/// popover) can still match its surface exactly.
pub fn select_menu_surface(ui: crate::theme::UiColors) -> Hsla {
    if ui.surface.l > 0.5 {
        ui.overlay
    } else {
        Hsla {
            l: (ui.surface.l + 0.035).min(1.0),
            ..ui.surface
        }
    }
}

/// Hairline color for dividers *inside* a menu (between item groups). A whisper
/// of `ui.text`, NOT `ui.border`: the menu sits on the elevated surface (see
/// [`select_menu_surface`]), which in dark themes is lighter than `ui.border`
/// (`0x2a2a2a` vs `0x252525`), so a `ui.border` divider has near-zero contrast
/// and vanishes. The structural app borders (sidebar/terminal divider, title
/// bar) read as `ui.border` only because they sit on the near-black terminal -
/// same color, far darker backdrop. A text-tint lifts off the menu surface in
/// either theme; at 0.12 it lands on ~`ui.border` over a light theme's white
/// menu (no regression there) while staying clearly visible on the dark menu.
pub fn menu_divider_color(ui: crate::theme::UiColors) -> Hsla {
    with_alpha(ui.text, 0.12)
}

/// Apply the elevated floating-menu *skin* - radius, lifted surface, and a
/// hairline border at 0.6 alpha - to any element. The single source of
/// truth for the Settings "Shell" select look, shared by [`select_menu`] (the
/// fixed-width container) and by every variable-width app menu/popover that
/// anchors to its own trigger (context menus, the diff scope/base pickers, the
/// sidebar Settings popover). Layout (flex, gap, padding, width, interactivity)
/// stays with the caller; this only paints the surface.
pub fn menu_surface<E: Styled>(el: E, ui: crate::theme::UiColors) -> E {
    el.rounded(tok::radius::MENU)
        .bg(select_menu_surface(ui))
        .border_1()
        .border_color(with_alpha(ui.border, 0.6))
}

/// The elevated floating menu container: the [`menu_surface`] skin plus tight
/// geometry and a fixed 200-280px width clamp. A press inside is swallowed
/// (stop_propagation); the caller adds the rows and an `on_mouse_down_out` to
/// close. Menus that must size to their own content use [`menu_surface`]
/// directly instead (the width clamp here would fight a stretch/auto width).
pub fn select_menu(id: impl Into<ElementId>, ui: crate::theme::UiColors) -> Stateful<Div> {
    menu_surface(div().id(id.into()), ui)
        .flex()
        .flex_col()
        .gap(px(1.))
        .p(tok::space::XS)
        .min_w(px(200.))
        .max_w(px(280.))
        // Tall enough for the longest menu this app builds. It was 320, and a
        // menu that outgrew it did not say so: `overflow_y_scroll` is on, so
        // the surplus items were reachable only by scrolling *inside* a
        // context menu, with no scrollbar, no fade and nothing else to
        // suggest there was more. The project menu is 13 rows, and the row
        // that fell below the fold was "Close Project" - so the app looked
        // like it had no way to remove a project at all. Reported from real
        // use, and the way it failed is the point: a truncated menu is
        // indistinguishable from a shorter one.
        //
        // The scroll stays as the floor for a menu that outgrows even this
        // (a container with many custom buttons), but no menu the app builds
        // today reaches it.
        .max_h(px(560.))
        .overflow_y_scroll()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// One menu row with whisper highlights (selected slightly stronger than hover).
///
/// The caller adds the leading logo + label children and the `on_click`.
pub fn select_item(
    id: impl Into<ElementId>,
    selected: bool,
    ui: crate::theme::UiColors,
) -> AnimatedHover {
    let selected_bg = with_alpha(ui.text, 0.10);
    let resting_bg = if selected {
        selected_bg
    } else {
        with_alpha(ui.text, 0.0)
    };
    let hover_bg = if selected {
        selected_bg
    } else {
        with_alpha(ui.text, 0.05)
    };

    div()
        .id(id.into())
        .flex_none()
        .h(px(28.))
        .px(tok::space::MD)
        .rounded(SETTINGS_CONTROL_CORNER_RADIUS)
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::MD)
        .cursor(CursorStyle::PointingHand)
        .text_size(tok::text::ROW)
        .animated_hover_bg(resting_bg, hover_bg)
}

/// What a value chip is claiming, which is what decides its colours.
///
/// The four tones are the design's, and they are about the *claim* rather
/// than the value: "on" and "running" are the same statement about two
/// different things, and both get the accent. A plain value is not a claim at
/// all and stays neutral - a data directory is neither good nor bad news.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipTone {
    /// On, running, enabled.
    On,
    /// Needs repair - a thing that should be working and is not.
    Attention,
    /// Off, by choice. Not a warning: the user turned it off.
    Off,
    /// A value with no opinion attached.
    Plain,
}

/// The design's setting row: label, an optional mono hint, and a value chip.
///
/// One row anatomy for every section, from the Agents section that the design
/// calls normative: 38 tall, capped at 620 wide, a bottom hairline instead of a
/// card border. The hint sits beside the label in mono rather than under it in
/// prose - a row is one line, and a row that wraps to two stops being scannable
/// next to nine others.
pub fn setting_row(
    ui: crate::theme::UiColors,
    label: impl Into<SharedString>,
    hint: Option<SharedString>,
    value: impl Into<SharedString>,
    tone: ChipTone,
) -> impl IntoElement {
    setting_row_frame(ui, label, hint).child(value_chip(ui, value, tone))
}

/// A section's leading note: one sentence saying what the section is for,
/// above its rows.
pub fn setting_note(ui: crate::theme::UiColors, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .max_w(px(SETTING_ROW_MAX_WIDTH))
        .pb(tok::space::XL)
        .text_size(tok::text::CAPTION)
        .text_color(ui.text_secondary)
        .child(text.into())
}

/// The row's chip with nothing in it: the skin only.
///
/// Split out so a chip whose value is not a word can still be one - the cursor
/// colour's chip leads with the colour itself, which is the only honest way to
/// show a colour in a row that is 38 tall.
pub fn chip_skin(ui: crate::theme::UiColors, tone: ChipTone) -> Div {
    let (chip_fg, chip_bg, chip_border) = match tone {
        ChipTone::On => (ui.accent, ui.accent_surface, Some(ui.accent_border)),
        ChipTone::Attention => (ui.vc_modified, with_alpha(ui.vc_modified, 0.12), None),
        ChipTone::Off => (ui.muted, with_alpha(ui.muted, 0.0), None),
        ChipTone::Plain => (ui.text_body, ui.subtle, None),
    };
    let mut chip = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::SM)
        .px(tok::space::MD)
        .py(px(3.))
        .rounded(tok::radius::BADGE)
        .bg(chip_bg)
        .text_size(tok::text::CONTROL)
        .text_color(chip_fg)
        .whitespace_nowrap();
    if let Some(border) = chip_border {
        chip = chip.border_1().border_color(border);
    }
    chip
}

/// The row's chip: the value, in the tone of the claim it is making.
///
/// Split out of [`setting_row`] when the row learned to be pressed - the three
/// row forms differ in what a press does, never in what the chip looks like.
pub fn value_chip(
    ui: crate::theme::UiColors,
    value: impl Into<SharedString>,
    tone: ChipTone,
) -> Div {
    chip_skin(ui, tone).child(value.into())
}

/// The row's frame: 38 tall, capped at 620, a bottom hairline, and the
/// label/hint/spacer run. The caller adds the trailing chip.
///
/// Shared by all three row forms so a pressable row cannot drift a pixel from a
/// stated one - the design's anatomy is one anatomy, and the difference
/// between the forms is only what a press does.
fn setting_row_frame(
    ui: crate::theme::UiColors,
    label: impl Into<SharedString>,
    hint: Option<SharedString>,
) -> Div {
    setting_row_frame_marked(ui, label, None, hint)
}

/// The same frame with one mark between the label and the hint - a chip that is
/// a **fact about the thing named**, not about the setting's value.
///
/// The value's chip is the row's right end and says on or off; this one sits
/// with the name because it qualifies the name: an agent's capability tier is
/// true of the agent whether the row is switched on or not.
fn setting_row_frame_marked(
    ui: crate::theme::UiColors,
    label: impl Into<SharedString>,
    mark: Option<AnyElement>,
    hint: Option<SharedString>,
) -> Div {
    div()
        .w_full()
        .max_w(px(SETTING_ROW_MAX_WIDTH))
        .h(px(SETTING_ROW_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::LG)
        .border_b_1()
        .border_color(ui.divider)
        .child(
            div()
                .flex_none()
                .text_size(tok::text::ROW)
                .text_color(ui.text)
                .child(label.into()),
        )
        .children(mark)
        .children(hint.map(|hint| {
            div()
                .min_w_0()
                .truncate()
                .font_family(tok::font::MONO)
                .text_size(tok::mono::HINT)
                .text_color(ui.text_tertiary)
                .child(hint)
        }))
        .child(div().flex_1().min_w_0())
}

/// The resting/hover pair for a row that can be pressed. A whisper of text over
/// the page - the row has no fill of its own, and anything stronger would draw
/// a box the design's row deliberately does not have.
fn pressable_row_backgrounds(ui: crate::theme::UiColors) -> (Hsla, Hsla) {
    (with_alpha(ui.text, 0.0), with_alpha(ui.text, 0.04))
}

/// A row whose chip is a switch: pressing anywhere on it flips the value.
///
/// The chip *is* the state - "on" / "off" in the design's two tones - rather
/// than a pill beside a label repeating it in a second alphabet. The whole row
/// is the hit target, which is what the mockup does for its own pressable rows
/// (`cursor: pointer` on the row, nothing extra drawn on the chip).
pub fn setting_toggle_row(
    ui: crate::theme::UiColors,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    hint: Option<SharedString>,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let (resting, hovered) = pressable_row_backgrounds(ui);
    let (value, tone) = if on {
        ("on", ChipTone::On)
    } else {
        ("off", ChipTone::Off)
    };
    setting_row_frame(ui, label, hint)
        .id(id.into())
        .cursor(CursorStyle::PointingHand)
        .child(value_chip(ui, value, tone))
        .animated_hover_bg(resting, hovered)
        .on_click(on_click)
}

/// [`setting_toggle_row`] carrying one mark beside its label.
pub fn setting_toggle_row_marked(
    ui: crate::theme::UiColors,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    mark: AnyElement,
    hint: Option<SharedString>,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let (resting, hovered) = pressable_row_backgrounds(ui);
    let (value, tone) = if on {
        ("on", ChipTone::On)
    } else {
        ("off", ChipTone::Off)
    };
    setting_row_frame_marked(ui, label, Some(mark), hint)
        .id(id.into())
        .cursor(CursorStyle::PointingHand)
        .child(value_chip(ui, value, tone))
        .animated_hover_bg(resting, hovered)
        .on_click(on_click)
}

/// A row whose chip opens a menu: pressing anywhere on it opens the choices.
///
/// The caller owns "which row is open" (one field on the app, the way the
/// settings selects already do) and builds the menu with [`select_menu`] /
/// [`select_item`]; this positions it under the row's right edge and keeps the
/// anatomy identical to a stated row.
pub fn setting_choice_row(
    ui: crate::theme::UiColors,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    hint: Option<SharedString>,
    chip: Div,
    menu: Option<AnyElement>,
    on_press: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let (resting, hovered) = pressable_row_backgrounds(ui);
    setting_row_frame(ui, label, hint)
        .id(id.into())
        .relative()
        .cursor(CursorStyle::PointingHand)
        .child(chip)
        .children(menu.map(|menu| {
            deferred(
                div()
                    .absolute()
                    .top(px(SETTING_ROW_HEIGHT))
                    .right(px(0.))
                    .occlude()
                    .child(menu),
            )
            .with_priority(1)
        }))
        .animated_hover_bg(resting, hovered)
        .on_mouse_down(gpui::MouseButton::Left, on_press)
}

/// The dashed "+ add …" row that closes a list of rows.
///
/// The design gives one to Agents, MCP and Workspaces, all in the same place:
/// last, at the row's own width, dashed so it reads as an opening rather than
/// as one more entry. `enabled` false leaves it drawn and inert - the label is
/// then saying why there is nothing to press.
pub fn setting_action_row(
    ui: crate::theme::UiColors,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let (resting, hovered) = pressable_row_backgrounds(ui);
    let row = div()
        .id(id.into())
        .w_full()
        .max_w(px(SETTING_ROW_MAX_WIDTH))
        .h(px(SETTING_ROW_HEIGHT))
        .mt(tok::space::XL)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .rounded(tok::radius::SMALL)
        .border_dashed()
        .border_1()
        .border_color(ui.border)
        .text_size(tok::text::ROW)
        .text_color(if enabled { ui.text_secondary } else { ui.faint })
        .child(label.into());
    if enabled {
        row.cursor(CursorStyle::PointingHand)
            .animated_hover_bg(resting, hovered)
            .on_click(on_click)
            .into_any_element()
    } else {
        row.into_any_element()
    }
}

/// `1874` as `1 874`. A thin space would be typographically right and is not
/// worth a font risk; a plain space matches the design's own rendering.
///
/// It lived in `allow_rules.rs`, beside the count of permission rules Settings
/// -> General used to state. That module went with the permission bar and this
/// did not: Settings -> Terminal groups a scrollback depth the same way, and a
/// second copy would let the two chips drift apart.
pub(crate) fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}
