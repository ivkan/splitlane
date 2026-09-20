//! "Notifications" settings page, in the design's row anatomy.
//!
//! **One control, three positions**. The design used to list five rows,
//! each with its own channel; this app then had one on/off gate over four
//! notifications, and drawing four switches would have drawn the same switch
//! four times - so the design adopted our shape, the gate first and the four
//! below naming what it covers.
//!
//! The ladder replaces that gate. Three rungs, each written as a description of
//! **the work** rather than of the machinery, and **no zero rung**:
//!
//! ```text
//! only when something breaks
//! when an agent is waiting for me     <- the default
//! and when work has finished
//! ```
//!
//! The missing bottom rung is what replaces a lock. A breakage always arrives,
//! because the floor of the scale is still a working product: nothing here is
//! silenced, nothing has to explain itself, and the setting cannot switch off
//! the thing that would tell you the setting was a mistake.
//!
//! **"One switch" is not broken by three positions** - that promise was about
//! count, and a switch is not obliged to be binary. What would break it is a
//! row *plus* a threshold, a row *plus* per-class ticks, a row *plus* a
//! per-session flag. So the admission condition is that the ladder is the only
//! **control** in this section, and it is: everything else here is a stated
//! fact, and each of those says which rung covers it.
//!
//! **The threshold does not become a knob.** It is a number nobody has an
//! opinion about until they have been annoyed by the wrong one, and if it turns
//! out to be wrong it is a number to change rather than a question to ask.
//!
//! Two facts this page states and does not offer: sound answers "never",
//! because we never do; and the forecast row names the projection rather than
//! the 85% threshold, which lives in Settings -> Limits, where the footer is.
//!
//! **And on macOS, a third: what the operating system will actually do.** The
//! rungs are a claim about what this app will send; they were being read as a
//! claim about what arrives, and until T2.6 nothing arrived on macOS at all.
//! Worse, the state survives the fix: `send` answers `ok` under a denial, so a
//! page deriving "covered" from the rung alone says covered to somebody
//! receiving nothing - a lie told by the one section whose whole job is stating
//! facts. So the class rows ask the platform too, and answer **blocked** rather
//! than covered where macOS will stop it - and **unconfirmed** where the answer
//! has not arrived yet, because the read is off-thread and the opening frame of
//! this section genuinely knows nothing. Claiming coverage there would be the
//! same lie, one frame early.
//!
//! **It is still one control.** The permission is not a switch here and gets no
//! button: a `NotDetermined` heals itself, because the next agent session to
//! start asks; and a `Denied` cannot be undone from inside any application at
//! all, so a control offering to would be a control that does nothing. What a
//! dead end is owed is the way out in words, and the row says it.

use gpui::{
    ClickEvent, Context, CursorStyle, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, div, prelude::*,
};
use serde_json::Value;
use splitlane_config::schema::NotifyLevel;

use crate::SplitlaneApp;
use crate::agents::notifications::{NotificationClass, class_is_covered};
use crate::settings::components::{
    ChipTone, select_item, select_menu, setting_choice_row, setting_note, setting_row, value_chip,
};

/// The rungs, in order, as the product says them.
///
/// The editing rule that keeps the ladder honest: every line reads as a
/// description of the work. "When an agent is waiting for me" passes. "All
/// events" does not.
const RUNGS: [(NotifyLevel, &str); 3] = [
    (NotifyLevel::Broke, "Only when something breaks"),
    (NotifyLevel::Waiting, "When an agent is waiting for me"),
    (NotifyLevel::Finished, "And when work has finished"),
];

/// Will a notification this app sends actually reach the reader - **yes, no, or
/// we do not know yet**?
///
/// The third answer is not fussiness. On macOS the operating system's answer is
/// read off the render thread and cached, so the first paint after this section
/// opens genuinely has no answer to show; and a query that fails leaves the app
/// in the same position for a different reason. Both used to collapse into
/// `true`, which made the page's own opening frame claim `covered` on no
/// evidence - the defect this section was being fixed for, arriving one frame
/// early.
///
/// Everywhere else it is a settled `yes`, and that is a reading rather than a
/// shrug: `notify-rust` reaches a desktop notification service with no
/// per-application authorization to report, so there is no gate that could
/// answer `no`.
fn platform_delivery() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        crate::agents::mac_notifications::permission().will_arrive()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(true)
    }
}

/// The macOS row: what the operating system will do, stated and not offered.
///
/// It sits directly under the ladder and above the four class rows because it
/// **qualifies** all of them: the rung says what this app will send, and this
/// says whether any of it arrives. A person reading downwards meets the
/// qualifier before the things it qualifies.
fn platform_permission_row(ui: crate::theme::UiColors) -> Option<gpui::AnyElement> {
    #[cfg(target_os = "macos")]
    {
        use crate::agents::mac_notifications::{MacNotificationPermission, permission};
        let permission = permission();
        let (word, note) = permission.stated();
        let tone = match permission {
            MacNotificationPermission::Allowed => ChipTone::On,
            // Nothing has been read yet, so there is nothing to claim in
            // either direction - the same neutrality the class rows take.
            MacNotificationPermission::Unchecked => ChipTone::Plain,
            // Everything else is a thing that should be working and is not -
            // including `AllowedQuietly`, which is the state that looks like
            // success from every angle except the reader's.
            _ => ChipTone::Attention,
        };
        Some(
            setting_row(
                ui,
                "macOS permission",
                note.map(SharedString::from),
                word,
                tone,
            )
            .into_any_element(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ui;
        None
    }
}

/// What one class row says about itself: the rung's answer, then the
/// platform's.
///
/// **Two questions, and the second one is the fix.** The rung says what this
/// app will send; the platform says whether any of it arrives. This used to ask
/// only the first, so on a macOS that had denied us - or, before T2.6, on any
/// macOS at all - the row said `covered` to somebody receiving nothing. That is
/// the "place that lies" defect in the one section whose entire job is stating
/// facts.
///
/// `silent` does not become `blocked`: a class the person put below their own
/// rung is silent because they chose it, and the platform has nothing to add.
/// Off is a choice and carries the design's own tone; `blocked` is
/// `Attention`, because a class the person asked for and the system swallows is
/// exactly "a thing that should be working and is not".
///
/// And `delivery: None` - the platform has not answered yet - gets its own word
/// rather than being rounded to either neighbour. Rounding it up says covered
/// with nothing behind it, which is the defect itself; rounding it down cries
/// blocked at somebody whose notifications are fine.
fn class_chip(
    level: NotifyLevel,
    class: NotificationClass,
    delivery: Option<bool>,
) -> (&'static str, ChipTone) {
    match (class_is_covered(level, class), delivery) {
        (true, Some(true)) => ("covered", ChipTone::On),
        (true, Some(false)) => ("blocked", ChipTone::Attention),
        (true, None) => ("unconfirmed", ChipTone::Plain),
        (false, _) => ("silent", ChipTone::Off),
    }
}

fn rung_label(level: NotifyLevel) -> &'static str {
    RUNGS
        .iter()
        .find(|(rung, _)| *rung == level)
        .map(|(_, label)| *label)
        .unwrap_or("When an agent is waiting for me")
}

impl SplitlaneApp {
    pub(crate) fn render_notifications_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let level = self
            .cached_config
            .agent_panel
            .as_ref()
            .map_or_else(NotifyLevel::default, |agent_panel| {
                agent_panel.resolved_notify_level()
            });
        let delivery = platform_delivery();
        let covered = |class: NotificationClass| class_chip(level, class, delivery);

        div()
            .flex()
            .flex_col()
            .child(setting_note(
                ui,
                "One switch. Only agent state changes notify \u{2014} shell output never does, \
                 that is what the status dots are for.",
            ))
            .child(self.notify_level_row(level, ui, cx))
            .children(platform_permission_row(ui))
            .child({
                let (value, tone) = covered(NotificationClass::Waiting);
                setting_row(ui, "Agent needs input", None, value, tone)
            })
            .child({
                let (value, tone) = covered(NotificationClass::Finished);
                setting_row(
                    ui,
                    "Run finished",
                    Some(SharedString::from(format!(
                        "runs shorter than {}s pass in silence",
                        crate::agents::notifications::MIN_TURN_FOR_NOTIFICATION.as_secs()
                    ))),
                    value,
                    tone,
                )
            })
            .child({
                let (value, tone) = covered(NotificationClass::Broke);
                setting_row(ui, "Session crashed", None, value, tone)
            })
            .child({
                let (value, tone) = covered(NotificationClass::Broke);
                setting_row(
                    ui,
                    "Agent stalled",
                    Some(SharedString::from("silent for longer than the threshold")),
                    value,
                    tone,
                )
            })
            // **This row and the dispatcher are one thing, and they land
            // together or not at all.** It used to say "Limit above 85% -
            // status bar only", which was true while nothing was dispatched;
            // the moment a notification exists that sentence is a lie in the
            // other direction.
            //
            // What fires is the **projection**, not the threshold. 85% is a
            // state and the rail's footer carries it; a window at 86% crawling
            // slowly will make it to its own reset, and waking somebody for
            // that is the noise that gets a feature switched off. So the row
            // names the forecast, and the warning threshold stays where it
            // always was - in Settings -> Limits, which is about the footer.
            //
            // It rides the bottom rung, with the crashes, and that is an
            // argument rather than a leftover: it is the only class in this app
            // whose price for going unheard is the rest of the window, and it
            // fires once per window, so the quietest rung can carry it without
            // becoming noise.
            .child({
                let (value, tone) = covered(NotificationClass::Broke);
                setting_row(
                    ui,
                    SharedString::from("Limit burning fast"),
                    Some(SharedString::from(
                        "once per window, when a short window is being spent fast enough to run \
                         out before it resets",
                    )),
                    value,
                    tone,
                )
            })
            .child(setting_row(
                ui,
                "Play a sound",
                None,
                "never",
                ChipTone::Off,
            ))
            .child(setting_note(
                ui,
                "A notification asks whether you can see that session, not whether you can \
                 see the app \u{2014} a run that finished in a pane on screen has already \
                 told you.",
            ))
    }

    /// The ladder itself: one `setting_choice_row`, three positions.
    ///
    /// Built here rather than through `general_choice_row` for one reason: that
    /// helper writes top-level config keys, and this one lives under
    /// `agent_panel`. The anatomy and the open/close rule are the same.
    fn notify_level_row(
        &self,
        level: NotifyLevel,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_open = self.general_dropdown == Some(crate::GeneralDropdown::NotifyLevel);
        let menu = is_open.then(|| {
            let mut menu = select_menu("notifications-level-list", ui).on_mouse_down_out(
                cx.listener(|this, _, _w, cx| {
                    if this.general_dropdown == Some(crate::GeneralDropdown::NotifyLevel) {
                        this.general_dropdown = None;
                        cx.notify();
                    }
                }),
            );
            for (index, (rung, label)) in RUNGS.iter().enumerate() {
                let rung = *rung;
                menu = menu.child(
                    select_item(("notify-level", index), rung == level, ui)
                        .cursor(CursorStyle::Arrow)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.general_dropdown = None;
                            this.persist_agent_panel_setting(
                                "notify_level",
                                Value::String(format!("{rung:?}")),
                                cx,
                            );
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(ui.text)
                                .child(*label),
                        ),
                );
            }
            menu.into_any_element()
        });

        setting_choice_row(
            ui,
            "notifications-level",
            "Tell me",
            Some(SharedString::from(
                "a session that fell over always reaches you",
            )),
            value_chip(ui, rung_label(level), ChipTone::Plain),
            menu,
            // Decide open/close from the render-time `is_open` snapshot, not
            // the live state: the menu's `on_mouse_down_out` fires on this same
            // press and may have already cleared it, so a live toggle re-opens.
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.general_dropdown = if is_open {
                    None
                } else {
                    Some(crate::GeneralDropdown::NotifyLevel)
                };
                this.settings_focus.focus(window, cx);
                cx.notify();
            }),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rungs are written in the product's words and in one order, and the
    /// serialised value is the schema's own spelling - a mismatch would write a
    /// key the loader warns about and silently resolves to the default.
    #[test]
    fn every_rung_has_a_line_and_a_value_the_schema_answers_to() {
        assert_eq!(RUNGS.len(), 3, "three rungs, and no zero rung");
        for (rung, label) in RUNGS {
            assert_eq!(rung_label(rung), label);
            let value = format!("{rung:?}");
            let parsed: NotifyLevel =
                serde_json::from_value(serde_json::Value::String(value.clone()))
                    .expect("the schema answers to what the row writes");
            assert_eq!(parsed, rung, "{value} round-trips");
        }
    }

    /// The row states what the reader will actually get, which is the rung's
    /// answer and the platform's together.
    #[test]
    fn a_class_the_platform_will_swallow_does_not_say_covered() {
        // Delivery confirmed: the rung is the whole answer, as it always was.
        assert_eq!(
            class_chip(NotifyLevel::Waiting, NotificationClass::Waiting, Some(true)),
            ("covered", ChipTone::On)
        );
        // Delivery refused: the rung still admits it and nothing arrives, so
        // the row says so rather than claiming coverage.
        assert_eq!(
            class_chip(
                NotifyLevel::Waiting,
                NotificationClass::Waiting,
                Some(false)
            ),
            ("blocked", ChipTone::Attention)
        );
        // A breakage is unsilenceable by the ladder and still blockable by the
        // operating system - two different powers over the same row.
        assert_eq!(
            class_chip(NotifyLevel::Broke, NotificationClass::Broke, Some(false)),
            ("blocked", ChipTone::Attention)
        );
        // **Not yet answered is not a yes.** This is the opening frame of the
        // section, before the off-thread read lands, and claiming coverage
        // there is the same lie one frame early.
        assert_eq!(
            class_chip(NotifyLevel::Waiting, NotificationClass::Waiting, None),
            ("unconfirmed", ChipTone::Plain)
        );
        // Below the rung is a choice the person made, and the platform has
        // nothing to add to it: `silent` never becomes `blocked`, and never
        // becomes `unconfirmed` either.
        for delivery in [Some(true), Some(false), None] {
            assert_eq!(
                class_chip(NotifyLevel::Waiting, NotificationClass::Finished, delivery),
                ("silent", ChipTone::Off)
            );
        }
    }

    /// Each line describes the work rather than the machinery, which is the
    /// editing rule that keeps the ladder honest.
    #[test]
    fn the_rungs_read_as_work_and_the_default_is_the_middle_one() {
        assert_eq!(RUNGS[0].1, "Only when something breaks");
        assert_eq!(RUNGS[1].1, "When an agent is waiting for me");
        assert_eq!(RUNGS[2].1, "And when work has finished");
        assert_eq!(NotifyLevel::default(), NotifyLevel::Waiting);
    }
}
