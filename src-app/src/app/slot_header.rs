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
/// and the word is what says it without relying on colour. The copy
/// is the design's own, from its markup - "running", "waiting for you" - with
/// "failed" for the state its prototype has no way to reach.
///
/// The colours are the rail's status dot, deliberately: one surface, one
/// state, one colour wherever it is read.
///
/// `background_shells` is how many background commands the agent left alive
/// beside an idle turn; it changes the idle word and no other. Pass
/// [`crate::rail_state::RailRecord::shells_beside_idle`].
pub(crate) fn slot_header_status_word(
    status: crate::project::ThreadStatus,
    background_shells: u32,
    ui: crate::theme::UiColors,
) -> (SharedString, gpui::Hsla) {
    use crate::project::ThreadStatus;
    match status {
        // Still `idle`, and still its colour: the agent's turn is over and it
        // can be written to. What follows the word is why it may start again
        // unasked - or the dev server somebody asked for. Not `waiting`, which
        // here means a person is being waited on, and not its hue. Where
        // the word is drawn rather than listed, the count alone takes the accent
        // (`idle_with_shells_label`).
        ThreadStatus::Idle if background_shells > 0 => {
            (idle_with_shells_word(background_shells), ui.faint)
        }
        // Muted text, not a state colour. `running` is the accent and
        // `waiting` the attention hue because they are worth a colour -
        // something is happening to their code, or something needs them. `starting` claims nothing: it exists only so the header does
        // not lie for two seconds, and a fifth hue would advertise it far past
        // its importance.
        ThreadStatus::Starting => (SharedString::from("starting"), ui.muted),
        ThreadStatus::Thinking => (SharedString::from("running"), ui.accent),
        ThreadStatus::WaitingForInput => (SharedString::from("waiting for you"), ui.attention),
        ThreadStatus::Failed => (SharedString::from("failed"), ui.agent_error),
        ThreadStatus::Idle => (SharedString::from("idle"), ui.faint),
    }
}

/// `no activity 14m`: what a working session says once its agent has not been
/// heard from for as long as the "may be stuck" notification waits.
///
/// It replaces the word and not the dot. The dot stays the colour of a
/// working agent, because the agent has not failed and the app does not know
/// what it is waiting on; a colour of its own would turn a guess into a
/// state. The word is plain for the same reason.
///
/// "activity", the notification's own word, and not "output": the measure is
/// how long the agent has reported nothing, and a long build can be printing
/// the whole time.
pub(crate) fn quiet_word(minutes: u64) -> SharedString {
    SharedString::from(format!("no activity {}m", minutes.max(1)))
}

/// Whether the quiet word is what a session's status slot shows: only a
/// working session can be one that has gone quiet.
pub(crate) fn shows_quiet(
    status: crate::project::ThreadStatus,
    quiet_minutes: Option<u64>,
) -> Option<u64> {
    quiet_minutes.filter(|_| status == crate::project::ThreadStatus::Thinking)
}

/// `idle · 1 shell` / `idle · 2 shells`: an idle session with background
/// commands of its own still alive, in the word Claude Code's own status line
/// uses for them.
pub(crate) fn idle_with_shells_word(background_shells: u32) -> SharedString {
    SharedString::from(format!("idle \u{b7} {}", shells_word(background_shells)))
}

/// `1 shell` / `2 shells`.
fn shells_word(background_shells: u32) -> SharedString {
    let noun = if background_shells == 1 {
        "shell"
    } else {
        "shells"
    };
    SharedString::from(format!("{background_shells} {noun}"))
}

/// The same words as an element, with the count in the accent of `running`.
///
/// The state is still idle, so `idle` keeps the idle tone and the dot beside
/// it does not change. The count is a fact beside the state rather than a
/// state: a command is alive, whichever kind it is. Drawn in the idle tone it
/// was reported as too pale to notice, on a row that otherwise reads as a
/// session with nothing going on, and a neutral step brighter was still too
/// quiet. Amber says something is running, which is true of the command; the
/// grey dot and the word `idle` are what say it is not the agent.
///
/// Font and size are the caller's; this only splits the tones.
pub(crate) fn idle_with_shells_label(
    background_shells: u32,
    ui: crate::theme::UiColors,
) -> gpui::AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .whitespace_nowrap()
        .child(
            div()
                .text_color(ui.faint)
                // No-break spaces: the gap belongs to the words, not to the
                // layout, and must not be trimmed at the end of a run.
                .child(SharedString::from("idle\u{a0}\u{b7}\u{a0}")),
        )
        .child(
            div()
                .text_color(ui.accent)
                .child(shells_word(background_shells)),
        )
        .into_any_element()
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

#[cfg(test)]
mod tests {
    use super::{idle_with_shells_word, slot_header_status_word};

    #[test]
    fn only_a_working_session_says_it_has_gone_quiet() {
        use crate::project::ThreadStatus;
        assert_eq!(
            super::shows_quiet(ThreadStatus::Thinking, Some(14)),
            Some(14)
        );
        assert_eq!(super::shows_quiet(ThreadStatus::Thinking, None), None);
        // A turn that ended or stopped on a question has a word of its own,
        // and a stale count must not replace it.
        for status in [
            ThreadStatus::Idle,
            ThreadStatus::WaitingForInput,
            ThreadStatus::Failed,
            ThreadStatus::Starting,
        ] {
            assert_eq!(super::shows_quiet(status, Some(14)), None, "{status:?}");
        }
        assert_eq!(super::quiet_word(14).as_ref(), "no activity 14m");
        // The threshold can be under a minute; the word never says zero.
        assert_eq!(super::quiet_word(0).as_ref(), "no activity 1m");
    }
    use crate::project::ThreadStatus;

    /// The count changes the idle word and nothing else: not the status's
    /// colour, and not any other status, which is what keeps a session with a dev server
    /// from reading as working.
    #[test]
    fn background_shells_are_named_beside_idle_only() {
        let ui = crate::theme::ui_colors();
        assert_eq!(idle_with_shells_word(1).as_ref(), "idle \u{b7} 1 shell");
        assert_eq!(idle_with_shells_word(2).as_ref(), "idle \u{b7} 2 shells");

        let (plain, plain_tone) = slot_header_status_word(ThreadStatus::Idle, 0, ui);
        let (with_shell, shell_tone) = slot_header_status_word(ThreadStatus::Idle, 1, ui);
        assert_eq!(plain.as_ref(), "idle");
        assert_eq!(with_shell.as_ref(), "idle \u{b7} 1 shell");
        assert_eq!(plain_tone, shell_tone);

        for status in [
            ThreadStatus::Starting,
            ThreadStatus::Thinking,
            ThreadStatus::WaitingForInput,
            ThreadStatus::Failed,
        ] {
            assert_eq!(
                slot_header_status_word(status, 3, ui),
                slot_header_status_word(status, 0, ui),
                "{status:?}"
            );
        }
    }
}
