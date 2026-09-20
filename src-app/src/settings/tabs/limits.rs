//! Settings -> Limits: what the rail footer reads and what it does with it.
//!
//! The tenth section, and the one the app had none of. It is the only section
//! whose rows all state facts rather than offer choices, and that is the honest
//! shape of it today: the footer's two thresholds and its switching rule came
//! from the design as constants, the numbers come from wherever the
//! Claude CLI keeps its credentials, and none of the three is a preference the
//! app has ever let anyone set. Naming them here is what the section is for -
//! the footer states a number, and this says where the number comes from and
//! when it changes colour.
//!
//! The mockup's fourth row is now "read limits every", added in place of the
//! weekly sparkline (the sparkline itself was removed, and a
//! setting for something that does not exist is worse than a missing setting).
//! The number is ours - half an hour, not the mockup's 60s - because the
//! endpoint is undocumented and unbilled and every read is traffic to somebody
//! else's server under the user's own credentials; `LIMITS_POLL_INTERVAL` says
//! why at length.

use gpui::{Context, IntoElement, ParentElement, SharedString, Styled, div};

use crate::SplitlaneApp;
use crate::app::agents_sidebar::limits_footer;
use crate::settings::components::{ChipTone, setting_note, setting_row};
use crate::ui_tokens as tok;

impl SplitlaneApp {
    pub(crate) fn render_limits_content(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        div()
            .flex()
            .flex_col()
            .child(setting_note(
                ui,
                "When the source cannot be read the footer says so instead of hiding \u{2014} \
                 an empty footer would read as \u{201c}no limits\u{201d}.",
            ))
            .child(setting_row(
                ui,
                "Show",
                Some(SharedString::from("one row per vendor")),
                "the binding limit",
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "Warn at",
                Some(SharedString::from(format!(
                    "reached at {}%",
                    limits_footer::REACHED_AT.round() as u32
                ))),
                format!("{}%", limits_footer::NEARLY_OUT_AT.round() as u32),
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "Read usage from",
                Some(SharedString::from(limits_footer::usage_source_hint())),
                "Claude CLI",
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "And from",
                Some(SharedString::from(
                    "its own session files \u{2014} nothing is asked of it",
                )),
                "Codex CLI",
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "Read limits every",
                Some(SharedString::from(
                    "10 min while Claude works, 5 while a window burns \u{2014} or press \u{21bb}",
                )),
                SharedString::from(format!(
                    "{} min",
                    limits_footer::LIMITS_POLL_INTERVAL.as_secs() / 60
                )),
                ChipTone::Plain,
            ))
            .child(setting_row(
                ui,
                "Forget a vendor after",
                Some(SharedString::from("of hearing nothing from it")),
                SharedString::from(format!(
                    "{} days",
                    crate::vendor_limits::FRESHNESS_HORIZON / (24 * 60 * 60)
                )),
                ChipTone::Plain,
            ))
            .child(
                div()
                    .pt(tok::space::XL)
                    .max_w(gpui::px(crate::settings::components::SETTING_ROW_MAX_WIDTH))
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.text_tertiary)
                    .child(
                        "The binding limit is whichever of a vendor's windows is closer to its \
                         cap, with a five-point band so the label cannot flap while the values \
                         cross. Below the warning threshold the meter takes no colour and says \
                         nothing; at each threshold the colour and the word change together, so \
                         the state is never carried by hue alone. A reading goes quiet rather \
                         than confident once a fifth of its own window has gone by unread: the \
                         warning stands, because inside one window usage only rises, and the \
                         number greys, because it is no longer the present. None of these is \
                         settable yet.",
                    ),
            )
    }
}
