//! The slot header: what a slot says about itself, and what can be done to it.
//!
//! The design's structure puts an action where its object lives: an action
//! over a surface belongs to the header of the slot showing it, an action over
//! a container to that container's row in the rail.
//! Until now the agent surface had no header at all, so its actions had
//! nowhere to be and lived in a panel of icons floating in the top-right
//! corner of the content area - four unlabelled glyphs belonging to four
//! different objects.
//!
//! The header is that home. Left: what the slot is showing. Right: the
//! operations over it, then `⋯` for the active surface's own menu - which is
//! the menu the rail's row opens, not a second one.
//!
//! A slot with one surface draws no tab strip, which is exactly the
//! agent surface today: the left side is the surface's name, not a one-tab
//! strip. When the docks under this surface become ordinary surfaces of the
//! same slot - the next step, not this one - that left side becomes the tab
//! list, and it must become the tab idiom this app already has rather than a
//! third one.

use gpui::{
    ClickEvent, InteractiveElement, IntoElement, ParentElement, Pixels, Role, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};

use crate::settings::components::with_alpha;
use crate::ui_primitives::AnimatedHoverExt;
use crate::ui_tokens as tok;

/// Height of the strip over a slot's content. The agent surface reserved a
/// 56px band for the floating panel to sit in without painting over the
/// terminal; the header replaces that band and is the thing occupying it.
pub(crate) const SLOT_HEADER_HEIGHT: Pixels = tok::row::HEADER;

/// How wide a one-surface slot's name may grow before it ellipsizes. Bounded
/// rather than open-ended so the header's right-hand operations never get
/// pushed off the edge by a long agent title.
const SLOT_HEADER_NAME_MAX_WIDTH: f32 = 260.0;

/// And how narrow it may get before it stops giving way - the design's own
/// "min-width 34" for the session name.
const SLOT_HEADER_NAME_MIN_WIDTH: f32 = 34.0;

/// What a slot with exactly one surface puts on its left: the surface's glyph
/// and name.
///
/// A slot showing one surface draws no tab strip. A strip of one chip
/// offers a choice that does not exist, and puts a close box on the only thing
/// in the slot - so the one-surface case is a name, and the same name whether
/// the surface is an agent parked full-area or the single terminal of a pane.
/// Content-sized, not `flex_1`: one caller lays it out in a plain header row
/// and the other inside a horizontally scrolling strip, where a flex-basis of
/// zero collapses the name to its ellipsis. Callers that want it to fill wrap
/// it themselves.
pub(crate) fn slot_header_name(
    glyph: gpui::AnyElement,
    title: SharedString,
    status: Option<crate::project::ThreadStatus>,
    ui: crate::theme::UiColors,
) -> gpui::AnyElement {
    div()
        .flex_none()
        .min_w_0()
        // The design's floor: the name shrinks with an ellipsis but never
        // disappears, because a header that has stopped naming what the pane
        // shows has stopped doing its job.
        .min_w(px(SLOT_HEADER_NAME_MIN_WIDTH))
        .max_w(px(SLOT_HEADER_NAME_MAX_WIDTH))
        .flex()
        .flex_row()
        .items_center()
        .gap(tok::space::XS)
        .pl(tok::space::XS)
        .child(glyph)
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(tok::text::ROW)
                .text_color(ui.muted)
                .child(title),
        )
        .when_some(status, |name, status| {
            name.child(crate::app::agents_sidebar::surface_status_dot(status, ui))
        })
        .into_any_element()
}

/// The status a slot header reports in words, and the role that colours it.
///
/// The design gives the header a "status text (mono 9.5px, coloured by status)"
/// beside the dot, not instead of it: the dot is what carries across the room
/// and the word is what says which of the two amber states this is. The copy
/// is the design's own, from its markup - "running", "waiting for you" - with
/// "failed" for the state its prototype has no way to reach.
///
/// The colours are the rail's status dot, deliberately: one surface, one
/// state, one colour wherever it is read.
pub(crate) fn slot_header_status_word(
    status: crate::project::ThreadStatus,
    ui: crate::theme::UiColors,
) -> (SharedString, gpui::Hsla) {
    use crate::project::ThreadStatus;
    match status {
        // Muted text, not a state colour. `running` is amber and
        // `waiting` is the accent because they are claims on the user's
        // attention - something is happening to their code, or something needs
        // them. `starting` claims nothing: it exists only so the header does
        // not lie for two seconds, and a fifth hue would advertise it far past
        // its importance.
        ThreadStatus::Starting => (SharedString::from("starting"), ui.muted),
        ThreadStatus::Thinking => (SharedString::from("running"), ui.vc_modified),
        ThreadStatus::WaitingForInput => (SharedString::from("waiting for you"), ui.accent),
        ThreadStatus::Failed => (SharedString::from("failed"), ui.agent_error),
        ThreadStatus::Idle => (SharedString::from("idle"), ui.faint),
    }
}

/// The `⋯` at the right end of a slot header: the active surface's own menu.
///
/// Free-standing rather than a method so both slot headers - the one the app
/// draws over a parked surface and the one a `Pane` draws over its own slot -
/// are the same button rather than two that resemble each other.
/// `menu_open` suppresses the button's own tooltip. Without it the label of
/// the button you just clicked paints over the first rows of the list that
/// click opened - reported from a screenshot, 28 August.
pub(crate) fn slot_overflow_button(
    id: SharedString,
    ui: crate::theme::UiColors,
    menu_open: bool,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    let resting = with_alpha(ui.text, 0.0);
    let hover = with_alpha(ui.text, 0.08);
    div()
        .id(id)
        .role(Role::Button)
        .aria_label("More actions")
        .flex_none()
        .h(px(28.))
        .w(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(tok::radius::SMALL)
        .bg(resting)
        .text_size(tok::text::BODY)
        .text_color(with_alpha(ui.text, 0.7))
        .animated_hover_bg(resting, hover)
        .when(!menu_open, |b| {
            b.tooltip(crate::ui_primitives::text_tooltip("More actions"))
        })
        .cursor_pointer()
        .on_click(on_click)
        // A text glyph, like the rail's own `⋯`: Splitlane ships no
        // ellipsis asset, and the glyph sidesteps the `svg()` colour trap.
        .child("⋯")
        .into_any_element()
}
