//! Cross-surface UI primitives shared by the Agents view and the Review (Git
//! Diff) view.
//!
//! Before this module, the Review and Agents surfaces re-coded the same recipes
//! inline: a byte-for-byte tooltip struct in each (`DiffHeaderTooltip` ==
//! `HoverActionTooltip`), two near-identical filter fields, two `centered`
//! empty-state helpers, and a dozen ad-hoc icon buttons / pills. Every visual
//! change had to be made twice and the two surfaces had already drifted. This is
//! the single home for those recipes so a later visual change is made once.
//!
//! Layout that depends on view-specific state (the `TextInput` entity, the
//! `cx.listener` handlers, which popover is open) stays with the caller; these
//! helpers only paint the shared skin and accept the dynamic bits as params,
//! mirroring the established pattern in [`crate::settings::components`].

use gpui::{
    AnimationExt, AnyElement, AnyView, App, Bounds, ClickEvent, Div, Element, ElementId,
    FontWeight, GlobalElementId, Hsla, InspectorElementId, InteractiveElement, IntoElement,
    ParentElement, Pixels, Render, Rgba, SharedString, Stateful, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, div, prelude::*, px, svg,
};
use std::time::{Duration, Instant};

use crate::settings::components::with_alpha;
use crate::theme::UiColors;
use crate::ui_tokens as tok;

const HOVER_ANIMATION_DURATION: Duration = Duration::from_millis(120);

#[derive(Clone, Debug)]
struct HoverAnimationState {
    from: f32,
    target: f32,
    started_at: Instant,
    duration: Duration,
    hitbox: Option<gpui::Hitbox>,
}

impl HoverAnimationState {
    fn new() -> Self {
        Self {
            from: 0.0,
            target: 0.0,
            started_at: Instant::now(),
            duration: Duration::ZERO,
            hitbox: None,
        }
    }

    fn progress_at(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return self.target;
        }

        let elapsed = now.duration_since(self.started_at).as_secs_f32();
        let linear = (elapsed / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        self.from + (self.target - self.from) * ease_out_quint(linear)
    }

    fn retarget(&mut self, hovered: bool, now: Instant) -> bool {
        let target = if hovered { 1.0 } else { 0.0 };
        if target == self.target {
            return false;
        }

        let current = self.progress_at(now);
        self.from = current;
        self.target = target;
        self.started_at = now;
        self.duration = HOVER_ANIMATION_DURATION.mul_f32((target - current).abs());
        true
    }

    fn is_animating(&self, now: Instant) -> bool {
        !self.duration.is_zero() && now.duration_since(self.started_at) < self.duration
    }
}

fn ease_out_quint(delta: f32) -> f32 {
    1.0 - (1.0 - delta).powi(5)
}

/// Interpolates colors through RGBA so translucent hover fills and text tints
/// both reach their exact endpoints without hue-wrap artifacts.
pub(crate) fn lerp_color(from: Hsla, to: Hsla, delta: f32) -> Hsla {
    let from = Rgba::from(from);
    let to = Rgba::from(to);
    let delta = delta.clamp(0.0, 1.0);
    Hsla::from(Rgba {
        r: from.r + (to.r - from.r) * delta,
        g: from.g + (to.g - from.g) * delta,
        b: from.b + (to.b - from.b) * delta,
        a: from.a + (to.a - from.a) * delta,
    })
}

/// The shadow under a dialog.
///
/// This and [`menu_shadow`] exist so no call site pairs a token with a colour
/// by hand. The geometry is the design's and identical between the two themes
/// ([`tok::shadow`]); the colour is the theme's, because elevation is the one
/// part of the design the light map does change - every shadow and the scrim
/// drop by roughly a factor of four, since a dark shadow on white reads as
/// dirt rather than as height.
///
/// There is no `window_shadow` to go with them: the window's shadow is the
/// compositor's on all three platforms, and the only one the app paints is the
/// small inset under Linux CSD, which is sized to `RESIZE_BORDER` rather than
/// to the design's `0 40px 100px`. That one takes `UiColors::shadow_window`
/// with its own geometry.
pub(crate) fn dialog_shadow(ui: UiColors) -> Vec<gpui::BoxShadow> {
    box_shadow(tok::shadow::DIALOG, ui.shadow_dialog)
}

/// The shadow under a menu, a popover or a drag ghost.
pub(crate) fn menu_shadow(ui: UiColors) -> Vec<gpui::BoxShadow> {
    box_shadow(tok::shadow::MENU, ui.shadow_menu)
}

fn box_shadow((offset_y, blur): (f32, f32), color: Hsla) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow::new(px(0.), px(offset_y), color).blur_radius(px(blur))]
}

/// A reversible hover transition that keeps the wrapped GPUI hitbox as the
/// interactive root. State follows the element ID across consecutive frames
/// and disappears automatically when a transient control is unmounted.
type StyleAnimator = dyn for<'a> Fn(&mut AnimatedStyle<'a>, f32);
type ElementAnimator = dyn for<'a> FnOnce(&mut AnimatedElement<'a>, f32);

pub(crate) struct AnimatedHover {
    element: Stateful<Div>,
    style_animator: Option<Box<StyleAnimator>>,
    element_animator: Option<Box<ElementAnimator>>,
}

/// Mutable adapter around GPUI's value-consuming style builder API.
///
/// Hover callbacks intentionally mutate the wrapped refinement in place so a
/// caller can compose several animated properties without cloning or replacing
/// the element itself.
pub(crate) struct AnimatedStyle<'a>(&'a mut StyleRefinement);

impl AnimatedStyle<'_> {
    pub(crate) fn bg(&mut self, fill: impl Into<gpui::Fill>) -> &mut Self {
        *self.0 = std::mem::take(self.0).bg(fill);
        self
    }

    pub(crate) fn text_color(&mut self, color: impl Into<Hsla>) -> &mut Self {
        *self.0 = std::mem::take(self.0).text_color(color);
        self
    }

    pub(crate) fn border_color(&mut self, color: impl Into<Hsla>) -> &mut Self {
        *self.0 = std::mem::take(self.0).border_color(color);
        self
    }

    pub(crate) fn opacity(&mut self, opacity: f32) -> &mut Self {
        *self.0 = std::mem::take(self.0).opacity(opacity);
        self
    }
}

/// Adapter used by composite hover callbacks that also insert children.
pub(crate) struct AnimatedElement<'a>(&'a mut Stateful<Div>);

impl AnimatedElement<'_> {
    pub(crate) fn style(&mut self) -> AnimatedStyle<'_> {
        AnimatedStyle(self.0.style())
    }

    pub(crate) fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.0.extend(elements);
    }
}

pub(crate) trait AnimatedHoverExt {
    fn animated_hover(
        self,
        animator: impl for<'a> Fn(&mut AnimatedStyle<'a>, f32) + 'static,
    ) -> AnimatedHover;

    /// Variant for composite controls whose hover progress also styles or
    /// inserts child elements. The callback runs once, immediately before the
    /// wrapped div requests layout for the frame.
    fn animated_hover_element(
        self,
        animator: impl for<'a> FnOnce(&mut AnimatedElement<'a>, f32) + 'static,
    ) -> AnimatedHover;

    fn animated_hover_bg(self, resting: Hsla, hovered: Hsla) -> AnimatedHover
    where
        Self: Sized;
}

impl AnimatedHoverExt for Stateful<Div> {
    fn animated_hover(
        self,
        animator: impl for<'a> Fn(&mut AnimatedStyle<'a>, f32) + 'static,
    ) -> AnimatedHover {
        AnimatedHover {
            // The empty hover style lets GPUI invalidate only when the pointer
            // crosses this hitbox. The visual interpolation remains ours.
            element: self.hover(|style| style),
            style_animator: Some(Box::new(animator)),
            element_animator: None,
        }
    }

    fn animated_hover_element(
        self,
        animator: impl for<'a> FnOnce(&mut AnimatedElement<'a>, f32) + 'static,
    ) -> AnimatedHover {
        AnimatedHover {
            element: self.hover(|style| style),
            style_animator: None,
            element_animator: Some(Box::new(animator)),
        }
    }

    fn animated_hover_bg(self, resting: Hsla, hovered: Hsla) -> AnimatedHover {
        self.animated_hover(move |style, delta| {
            style.bg(lerp_color(resting, hovered, delta));
        })
    }
}

impl Styled for AnimatedHover {
    fn style(&mut self) -> &mut StyleRefinement {
        self.element.style()
    }
}

impl InteractiveElement for AnimatedHover {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.element.interactivity()
    }
}

impl StatefulInteractiveElement for AnimatedHover {}

impl ParentElement for AnimatedHover {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.element.extend(elements)
    }
}

impl Element for AnimatedHover {
    type RequestLayoutState = <Stateful<Div> as Element>::RequestLayoutState;
    type PrepaintState = <Stateful<Div> as Element>::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        <Stateful<Div> as Element>::id(&self.element)
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        self.element.source_location()
    }

    fn a11y_role(&self) -> Option<gpui::accesskit::Role> {
        self.element.a11y_role()
    }

    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        self.element.write_a11y_info(node);
    }

    fn a11y_synthetic_children(
        &mut self,
        prepaint: &mut Self::PrepaintState,
        builder: &mut gpui::A11ySubtreeBuilder,
    ) {
        <Stateful<Div> as Element>::a11y_synthetic_children(&mut self.element, prepaint, builder);
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let Some(global_id) = global_id else {
            return self.element.request_layout(None, inspector_id, window, cx);
        };
        let now = Instant::now();
        let (progress, is_animating) =
            window.with_element_state(global_id, |state: Option<HoverAnimationState>, window| {
                let mut state = state.unwrap_or_else(HoverAnimationState::new);
                let hovered = !cx.has_active_drag()
                    && state
                        .hitbox
                        .as_ref()
                        .is_some_and(|hitbox| hitbox.is_hovered(window));
                state.retarget(hovered, now);
                let progress = state.progress_at(now);
                let is_animating = state.is_animating(now);
                ((progress, is_animating), state)
            });

        if is_animating {
            window.request_animation_frame();
        }

        if let Some(animator) = self.style_animator.as_ref() {
            animator(&mut AnimatedStyle(self.element.style()), progress);
        }
        if let Some(animator) = self.element_animator.take() {
            animator(&mut AnimatedElement(&mut self.element), progress);
        }
        self.element
            .request_layout(Some(global_id), inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let Some(global_id) = global_id else {
            return self
                .element
                .prepaint(None, inspector_id, bounds, request_layout, window, cx);
        };
        let prepaint = self.element.prepaint(
            Some(global_id),
            inspector_id,
            bounds,
            request_layout,
            window,
            cx,
        );

        let hitbox = prepaint.clone();
        window.with_element_state(global_id, |state: Option<HoverAnimationState>, _window| {
            let mut state = state.unwrap_or_else(HoverAnimationState::new);
            state.hitbox = hitbox;
            ((), state)
        });

        prepaint
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.element.paint(
            global_id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        )
    }
}

impl IntoElement for AnimatedHover {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

// ── Type scale ────────────────────────────────────────────────────────────
//
// Three named steps plus one that is not named because it has exactly one
// owner. A role picks its step by what it *is*, never by which number it
// happens to be near: a 13px heading belongs at 14, a 13px caption at 11.
//
// The fourth step is 20px and up, and it belongs to About alone - the app
// name on the About screen and the startup splash logotype. Anything else
// reaching for display type is a finding, not a fifth constant.

/// Captions, eyebrows, tooltips, secondary metadata (time, counters, badges).
pub(crate) const LABEL: Pixels = px(11.);
/// Interface body text: list rows, menu entries, buttons, fields.
pub(crate) const BODY: Pixels = px(12.);
/// Section and panel headings.
pub(crate) const TITLE: Pixels = px(14.);

// ── Tooltip ───────────────────────────────────────────────────────────────

/// The shared hover-tooltip body. Replaces the formerly-duplicated
/// `DiffHeaderTooltip` (diff view) and `HoverActionTooltip` (agents sidebar),
/// which were byte-for-byte identical.
pub(crate) struct SplitlaneTooltip {
    pub(crate) label: SharedString,
}

impl Render for SplitlaneTooltip {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = crate::theme::active_theme();
        let ui = crate::theme::ui_colors();
        div()
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::SMALL)
            .bg(theme.title_bar_background)
            .border_1()
            .border_color(ui.border)
            .text_color(ui.text)
            .text_size(LABEL)
            .child(self.label.clone())
    }
}

/// Convenience builder for `.tooltip(text_tooltip("…"))` - a plain text tooltip
/// A tooltip of several lines, the first at full strength and the rest one
/// step down.
///
/// The single-line [`SplitlaneTooltip`] answers "what is this control"; this
/// answers a question whose honest answer is a few facts - the context meter's
/// numbers and where its ceiling came from - and a control with several things
/// to say should not be made to say them in one run-on line.
pub(crate) struct MultiLineTooltip {
    pub(crate) lines: Vec<SharedString>,
}

impl Render for MultiLineTooltip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = crate::theme::active_theme();
        let ui = crate::theme::ui_colors();
        div()
            .px(tok::space::MD)
            .py(tok::space::XS)
            .rounded(tok::radius::SMALL)
            .bg(theme.title_bar_background)
            .border_1()
            .border_color(ui.border)
            .text_size(LABEL)
            .flex()
            .flex_col()
            .gap(tok::space::XS)
            .children(self.lines.iter().enumerate().map(|(i, line)| {
                div()
                    .text_color(if i == 0 { ui.text } else { ui.text_tertiary })
                    .child(line.clone())
            }))
    }
}

/// Like [`text_tooltip`], for a tooltip that has more than one thing to say.
pub(crate) fn lines_tooltip(
    lines: Vec<String>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let lines: Vec<SharedString> = lines.into_iter().map(SharedString::from).collect();
    move |_w, cx| {
        cx.new(|_| MultiLineTooltip {
            lines: lines.clone(),
        })
        .into()
    }
}

/// using [`SplitlaneTooltip`].
pub(crate) fn text_tooltip(
    label: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let label: SharedString = label.into();
    move |_w, cx| {
        cx.new(|_| SplitlaneTooltip {
            label: label.clone(),
        })
        .into()
    }
}

// ── Icon buttons ────────────────────────────────────────────────────────────

fn icon_button(
    id: impl Into<ElementId>,
    outer: Pixels,
    icon: &'static str,
    icon_size: Pixels,
    icon_color: Hsla,
    hover_bg: Hsla,
) -> AnimatedHover {
    div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(outer)
        .rounded(tok::radius::BADGE)
        .cursor_pointer()
        .animated_hover_bg(hover_bg.opacity(0.0), hover_bg)
        .child(
            svg()
                .size(icon_size)
                .flex_none()
                .path(icon)
                .text_color(icon_color),
        )
}

/// 20×20 icon button (16px glyph). The caller chains `.on_click` / `.tooltip`
/// and any resting-state `.bg(..)`.
pub(crate) fn icon_button_sm(
    id: impl Into<ElementId>,
    icon: &'static str,
    icon_color: Hsla,
    hover_bg: Hsla,
) -> AnimatedHover {
    icon_button(id, px(20.), icon, px(16.), icon_color, hover_bg)
}

/// 24×24 icon button (16px glyph). The caller chains `.on_click` / `.tooltip`
/// and any resting-state `.bg(..)`.
pub(crate) fn icon_button_md(
    id: impl Into<ElementId>,
    icon: &'static str,
    icon_color: Hsla,
    hover_bg: Hsla,
) -> AnimatedHover {
    icon_button(id, px(24.), icon, px(16.), icon_color, hover_bg)
}

// ── Toolbar pill ─────────────────────────────────────────────────────────────

/// An icon+label toolbar control (24px tall, subtle-gray resting/hover fill).
/// `active` paints the resting highlight (open popover / toggle on). The caller
/// chains `.on_click` and the icon/label children.
pub(crate) fn toolbar_pill(id: impl Into<ElementId>, ui: UiColors, active: bool) -> AnimatedHover {
    let resting_bg = if active {
        ui.subtle
    } else {
        ui.subtle.opacity(0.0)
    };

    div()
        .id(id.into())
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::XS)
        .h(px(24.))
        .px(tok::space::MD)
        .rounded(tok::radius::SMALL)
        .bg(resting_bg)
        .text_size(BODY)
        .text_color(ui.text)
        .cursor_pointer()
        .animated_hover_bg(resting_bg, ui.subtle)
}

// ── Filter pill ──────────────────────────────────────────────────────────────

/// A search/filter field as a filled `ui.subtle` pill (the canonical Agents
/// look). Builds the shared anatomy - leading magnifier, the caller's
/// `TextInput` child, and an optional trailing clear (×) - and returns the
/// stateful container so the caller can layer its own `.on_key_down`
/// (Escape/Enter) and `.on_mouse_down_out` (blur) handlers.
/// The field keeps the text cursor; the clear button is a button and takes the
/// pointer, like every other thing in this app that answers a click.
///
/// There were **two** of these, and the second was called
/// `filter_pill_with_arrow_clear` - "explicit-arrow alias used by Review", a
/// distinction from a desktop cursor policy that both wrappers then passed
/// `CursorStyle::Arrow` for. It named a difference it did not make, which is
/// worse than either answer, and it went when the app settled on one.
pub(crate) fn filter_pill(
    id: impl Into<ElementId>,
    clear_id: impl Into<ElementId>,
    ui: UiColors,
    input: impl IntoElement,
    show_clear: bool,
    on_clear: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let clear_id = clear_id.into();
    let mut field = div()
        .id(id.into())
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::XS)
        .px(tok::space::MD)
        .py(tok::space::XS)
        .rounded(crate::app::constants::SIDEBAR_TAB_CORNER_RADIUS)
        .bg(ui.subtle)
        .cursor_text()
        .child(
            svg()
                .size(px(12.))
                .flex_none()
                .path("icons/tool_search.svg")
                .text_color(ui.muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(BODY)
                .text_color(ui.text)
                .child(input),
        );
    if show_clear {
        field = field.child(
            div()
                .id(clear_id)
                .flex_none()
                .w(px(20.))
                .h(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(tok::radius::METER)
                .cursor_pointer()
                .text_color(ui.muted)
                // Emitted from inside the hover closure: `svg()` paints its
                // mask in its own style's text color and never inherits the
                // parent's, so a bare `svg()` child renders as nothing - the
                // clear button stays clickable while its glyph is invisible.
                .animated_hover_element(move |button, delta| {
                    let icon_color = lerp_color(ui.muted, ui.text, delta);
                    button
                        .style()
                        .bg(lerp_color(
                            with_alpha(ui.text, 0.0),
                            with_alpha(ui.text, 0.10),
                            delta,
                        ))
                        .text_color(icon_color);
                    button.extend([svg()
                        .size(px(16.))
                        .flex_none()
                        .path("icons/close.svg")
                        .text_color(icon_color)
                        .into_any_element()]);
                })
                .on_click(on_clear),
        );
    }
    field
}

// ── Capability chip ─────────────────────────────────────────────────────────

/// The one word beside an agent's name, wherever an agent is chosen.
///
/// Accent for the rung worth advertising, muted for the rest. An earlier design also
/// allowed a caveat beside the word - the warning role, with the reason spelled
/// out - for a rung the user could not use **until they did something**. There
/// were exactly two of those and both were about the intervention contour
/// ("hook needs your approval", "intervention turned off"); with that contour
/// gone there is no state left that a user has to leave, so the chip is the
/// word alone.
pub(crate) fn capability_word_chip(
    agent: crate::agent_launcher::TerminalAgent,
    ui: UiColors,
) -> AnyElement {
    let tier = agent.capability_tier();
    let (fill, text, border) = if tier.is_advertised() {
        (ui.accent_surface, ui.accent, ui.accent_border)
    } else {
        (with_alpha(ui.text, 0.05), ui.text_tertiary, ui.border)
    };
    div()
        .flex_none()
        .whitespace_nowrap()
        .px(tok::space::SM)
        .py(px(2.))
        .rounded(tok::radius::TAG)
        .bg(fill)
        .border_1()
        .border_color(border)
        .font_family(tok::font::MONO)
        .text_size(tok::mono::HINT)
        .text_color(text)
        .child(SharedString::from(tier.word()))
        .into_any_element()
}

// ── Section eyebrow ──────────────────────────────────────────────────────────

/// A section eyebrow label (11px SEMIBOLD muted). Returned as a bare `Div` so
/// the caller can chain layout (`.flex_1().min_w_0().truncate()` in a sidebar
/// list, `.flex_none()` next to a spacer).
pub(crate) fn section_eyebrow(label: impl Into<SharedString>, ui: UiColors) -> Div {
    // A correction to the design settles what a section label is, once, for
    // every list in the app: "JetBrains Mono 10px / weight 500 / labels grey,
    // uppercase, no tracking". It was sans 11 semibold here, which read as a
    // small heading rather than as an eyebrow.
    div()
        .font_family(crate::ui_tokens::font::MONO)
        .text_size(crate::ui_tokens::mono::LABEL)
        .font_weight(FontWeight::MEDIUM)
        .text_color(ui.muted)
        .child(label.into())
}

// ── Empty / loading state ────────────────────────────────────────────────────

/// A centered panel empty/loading/onboarding state: an optional leading icon
/// (the animated `loader-circle.svg` when `animate`), an optional `title`
/// (14px), and a muted body `message` (12px). Replaces the ad-hoc `centered`
/// helpers duplicated across the diff sidebar and diff view.
pub(crate) fn panel_empty_state(
    ui: UiColors,
    icon: Option<&'static str>,
    title: Option<SharedString>,
    message: impl Into<SharedString>,
    animate: bool,
) -> Div {
    let mut col = div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(tok::space::MD)
        .p(tok::space::XL);
    if let Some(path) = icon {
        let glyph = svg()
            .size(px(20.))
            .flex_none()
            .path(path)
            .text_color(with_alpha(ui.muted, 0.8));
        col = col.child(if animate {
            glyph
                .with_animation(
                    "panel-empty-spin",
                    gpui::Animation::new(std::time::Duration::from_secs(1)).repeat(),
                    |s, delta| {
                        s.with_transformation(gpui::Transformation::rotate(gpui::percentage(delta)))
                    },
                )
                .into_any_element()
        } else {
            glyph.into_any_element()
        });
    }
    if let Some(title) = title {
        col = col.child(
            div()
                .text_size(TITLE)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui.text)
                .child(title),
        );
    }
    col.child(
        div()
            .text_size(BODY)
            .text_color(ui.muted)
            .child(message.into()),
    )
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc, thread};

    use gpui::{InputEvent, Modifiers, MouseMoveEvent, TestAppContext, point, size};

    use super::*;

    struct HoverHarness {
        progress: Rc<Cell<f32>>,
    }

    impl Render for HoverHarness {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let progress = self.progress.clone();
            div()
                .id("animated-hover-regression")
                .w(px(50.))
                .h(px(50.))
                .animated_hover(move |style, delta| {
                    progress.set(delta);
                    style.opacity(0.5 + delta * 0.5);
                })
        }
    }

    #[gpui::test]
    fn animated_hover_progresses_after_pointer_entry(cx: &mut TestAppContext) {
        let progress = Rc::new(Cell::new(0.0));
        let progress_for_view = progress.clone();
        let (_view, cx) = cx.add_window_view(move |_, _| HoverHarness {
            progress: progress_for_view,
        });
        cx.simulate_resize(size(px(100.), px(100.)));

        cx.update(|window, cx| {
            window.draw(cx).clear();
            window.dispatch_event(
                MouseMoveEvent {
                    position: point(px(25.), px(25.)),
                    modifiers: Modifiers::default(),
                    pressed_button: None,
                }
                .to_platform_input(),
                cx,
            );
            window.draw(cx).clear();
        });

        thread::sleep(Duration::from_millis(10));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });

        assert!(
            progress.get() > 0.0,
            "hover progress stayed at zero after pointer entry"
        );
    }
}
