//! Settings -> Phone: pairing a phone, and the phones already paired.
//!
//! A page with state of its own - the pairing in progress, the code field,
//! the list - so it is an entity of its own rather than fields on
//! `SplitlaneApp`. The entity lives as long as the app, so a pairing keeps
//! going while the person looks at another section; "Cancel" or the five
//! minutes end it.
//!
//! Pairing is `remote::WindowPairing`: the same exchange as `splitlane
//! remote pair`, with the six digits typed here. The phone's record is what
//! the bridge reads before every request, so the switch and "Remove" hold
//! from the phone's next request without restarting anything.

use futures::StreamExt as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, Entity, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Pixels, Render, SharedString, Styled, Window, canvas,
    div, fill, point, px, size,
};

use crate::remote::{PairStep, PairedPhone, WindowPairing};
use crate::settings::components::{
    SETTING_ROW_MAX_WIDTH, secondary_button, setting_action_row, setting_note, setting_toggle_row,
};
use crate::ui_tokens as tok;
use crate::widgets::text_input::TextInput;

/// The QR code's side, quiet zone included. Large enough for a phone held
/// at arm's length to read a version-10 code (the pairing link is about two
/// hundred characters).
const QR_SIDE: f32 = 220.0;
/// The blank border a scanner needs around the code, in modules.
const QR_QUIET_MODULES: usize = 4;

pub(crate) struct PhonePage {
    /// `None` until the first read returns.
    phones: Option<Result<Vec<PairedPhone>, String>>,
    pairing: Option<WindowPairing>,
    /// Counts pairings started, so the steps of one cancelled meanwhile are
    /// told from the current one's.
    pairing_number: u64,
    step: Option<PairStep>,
    /// The code drawn from the link the current step shows.
    qr: Option<QrModules>,
    /// The link shown now was copied: the button says so until the next
    /// step.
    link_copied: bool,
    code_input: Entity<TextInput>,
    /// The last thing that went wrong outside pairing: a phone that could not
    /// be removed, a switch that could not be saved.
    problem: Option<SharedString>,
}

impl PhonePage {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let code_input = cx.new(|cx| TextInput::new("", "000000", cx));
        cx.observe(&code_input, |_, _, cx| cx.notify()).detach();
        let mut page = Self {
            phones: None,
            pairing: None,
            pairing_number: 0,
            step: None,
            qr: None,
            link_copied: false,
            code_input,
            problem: None,
        };
        page.reload(cx);
        page
    }

    /// Read the paired phones again, off the main thread.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let phones = smol::unblock(crate::remote::paired_phones).await;
            let _ = this.update(cx, |this, cx| {
                this.phones = Some(phones);
                cx.notify();
            });
        })
        .detach();
    }

    fn start_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (steps, mut arrived) = futures::channel::mpsc::unbounded();
        self.problem = None;
        self.code_input.update(cx, |input, cx| input.clear(cx));
        self.step = Some(PairStep::Connecting);
        self.qr = None;
        self.pairing_number += 1;
        let number = self.pairing_number;
        match WindowPairing::start(move |step| {
            let _ = steps.unbounded_send(step);
        }) {
            Ok(pairing) => self.pairing = Some(pairing),
            Err(reason) => {
                self.step = Some(PairStep::Failed { reason });
                cx.notify();
                return;
            }
        }
        let field = self.code_input.read(cx).focus_handle.clone();
        cx.spawn_in(window, async move |this, cx| {
            while let Some(step) = arrived.next().await {
                let asking = matches!(step, PairStep::Asking { .. });
                let ended = matches!(step, PairStep::Paired { .. } | PairStep::Failed { .. });
                let kept = this.update_in(cx, |this, window, cx| {
                    // A step from a pairing that was cancelled, and perhaps
                    // started again since, is not the current one's.
                    if this.pairing.is_none() || this.pairing_number != number {
                        return;
                    }
                    if let PairStep::Showing { link, .. } = &step {
                        this.qr = QrModules::of(link);
                    }
                    if asking {
                        this.code_input.update(cx, |input, cx| input.clear(cx));
                        window.focus(&field, cx);
                    }
                    if ended {
                        this.pairing = None;
                        this.qr = None;
                        this.reload(cx);
                    }
                    this.step = Some(step);
                    this.link_copied = false;
                    cx.notify();
                });
                if kept.is_err() || ended {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn cancel_pairing(&mut self, cx: &mut Context<Self>) {
        // Dropping the handle is what ends the exchange.
        self.pairing = None;
        self.step = None;
        self.qr = None;
        cx.notify();
    }

    /// Hand the typed code to the pairing. Only while a phone is waiting
    /// for one, and the field is emptied at once: a code sent early, or the
    /// same one sent twice by a second Enter, would wait in line and be
    /// taken as the next try - three tries are all there are.
    fn submit_code(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.step, Some(PairStep::Asking { .. })) {
            return;
        }
        let code = self.code_input.read(cx).value();
        if code.trim().is_empty() {
            return;
        }
        self.code_input.update(cx, |input, cx| input.clear(cx));
        if let Some(pairing) = &self.pairing {
            pairing.submit(code);
        }
    }

    fn set_reply(&mut self, room: String, reply: bool, cx: &mut Context<Self>) {
        self.change(cx, move || crate::remote::set_phone_reply(&room, reply));
    }

    fn remove(&mut self, room: String, cx: &mut Context<Self>) {
        // "Paired …" under the list would be about a phone no longer in it.
        if matches!(self.step, Some(PairStep::Paired { .. })) {
            self.step = None;
        }
        self.change(cx, move || crate::remote::remove_phone(&room));
    }

    /// Write a change to the paired phones off the main thread, then show
    /// the list as it is now.
    fn change(
        &mut self,
        cx: &mut Context<Self>,
        write: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let written = smol::unblock(write).await;
            let _ = this.update(cx, |this, cx| {
                this.problem = written.err().map(SharedString::from);
                this.reload(cx);
            });
        })
        .detach();
    }

    fn render_pairing(&self, ui: crate::theme::UiColors, cx: &mut Context<Self>) -> AnyElement {
        let cancel = secondary_button(
            "phone-pair-cancel",
            "Cancel",
            ui,
            cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_pairing(cx)),
        );
        let caption = |text: SharedString| {
            div()
                .text_size(tok::text::CAPTION)
                .text_color(ui.text_secondary)
                .child(text)
        };
        let body = match &self.step {
            Some(PairStep::Connecting) => div()
                .flex()
                .flex_col()
                .gap(tok::space::LG)
                .child(caption("Connecting to the relay\u{2026}".into())),
            Some(PairStep::Showing { link, .. }) => div()
                .flex()
                .flex_col()
                .gap(tok::space::LG)
                .child(caption(
                    "On the phone, open Splitlane from the home screen, tap Scan code and \
                     point it at this code. It works for five minutes, once."
                        .into(),
                ))
                .children(self.qr.clone().map(QrModules::render))
                .child(
                    div()
                        .max_w(px(SETTING_ROW_MAX_WIDTH))
                        .font_family(tok::font::MONO)
                        .text_size(tok::mono::HINT)
                        .text_color(ui.text_tertiary)
                        .child(SharedString::from(link.clone())),
                )
                .child(div().flex().flex_row().child(secondary_button(
                    "phone-pair-copy",
                    if self.link_copied {
                        "Copied"
                    } else {
                        "Copy link"
                    },
                    ui,
                    // For a phone whose camera cannot read the code:
                    // pasted into the page, the link pairs the same way.
                    cx.listener({
                        let link = link.clone();
                        move |this, _: &ClickEvent, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
                            this.link_copied = true;
                            cx.notify();
                        }
                    }),
                ))),
            Some(PairStep::Asking { phone, wrong }) => {
                let left = crate::remote::CODE_ATTEMPTS.saturating_sub(*wrong);
                let prompt = if *wrong == 0 {
                    format!("{phone} shows six digits. Type them here to pair it.")
                } else {
                    format!(
                        "That code did not match the one {phone} shows. {left} {} left.",
                        if left == 1 { "try" } else { "tries" }
                    )
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(tok::space::LG)
                    .child(caption(prompt.into()))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(tok::space::MD)
                            .child(
                                div()
                                    .id("phone-pair-code")
                                    .w(px(120.))
                                    .px(tok::space::MD)
                                    .py(tok::space::XS)
                                    .rounded(tok::radius::CONTROL)
                                    .bg(ui.subtle)
                                    .cursor_text()
                                    .font_family(tok::font::MONO)
                                    .text_size(tok::text::BODY)
                                    .text_color(ui.text)
                                    .child(self.code_input.clone())
                                    // The field has no Enter of its own;
                                    // here it means "Pair".
                                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                                        if ev.keystroke.key == "enter" {
                                            this.submit_code(cx);
                                            cx.stop_propagation();
                                        }
                                    })),
                            )
                            .child(secondary_button(
                                "phone-pair-submit",
                                "Pair",
                                ui,
                                cx.listener(|this, _: &ClickEvent, _, cx| this.submit_code(cx)),
                            )),
                    )
            }
            // Ended steps are shown by `render_outcome`; there is nothing in
            // progress.
            Some(PairStep::Paired { .. } | PairStep::Failed { .. }) | None => div(),
        };
        div()
            .flex()
            .flex_col()
            .gap(tok::space::LG)
            .pt(tok::space::XL)
            .child(body)
            .child(div().flex().flex_row().child(cancel))
            .into_any_element()
    }

    fn render_outcome(&self, ui: crate::theme::UiColors) -> Option<AnyElement> {
        let (text, color) = match &self.step {
            Some(PairStep::Paired { phone }) => (
                format!("Paired {phone}. It can see your agents now."),
                ui.text_secondary,
            ),
            Some(PairStep::Failed { reason }) => (reason.clone(), ui.agent_error),
            _ => return None,
        };
        Some(
            div()
                .pt(tok::space::MD)
                .text_size(tok::text::CAPTION)
                .text_color(color)
                .child(SharedString::from(text))
                .into_any_element(),
        )
    }

    fn render_phone(
        &self,
        ui: crate::theme::UiColors,
        phone: &PairedPhone,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let paired = chrono::DateTime::from_timestamp(phone.paired_at_unix as i64, 0)
            .map(|at| {
                at.with_timezone(&chrono::Local)
                    .format("%-d %b %Y")
                    .to_string()
            })
            .unwrap_or_default();
        let room = phone.room.clone();
        let reply = phone.reply;
        let remove_room = phone.room.clone();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::LG)
                    .max_w(px(SETTING_ROW_MAX_WIDTH))
                    .pt(tok::space::XL)
                    .pb(tok::space::XS)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(tok::text::ROW)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(ui.text)
                            .child(SharedString::from(phone.name.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(tok::font::MONO)
                            .text_size(tok::mono::HINT)
                            .text_color(ui.text_tertiary)
                            .child(SharedString::from(format!("paired {paired}"))),
                    )
                    .child(secondary_button(
                        SharedString::from(format!("phone-remove-{}", phone.room)),
                        "Remove",
                        ui,
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.remove(remove_room.clone(), cx);
                        }),
                    )),
            )
            .child(setting_toggle_row(
                ui,
                SharedString::from(format!("phone-reply-{}", phone.room)),
                "Can reply",
                Some(SharedString::from(
                    "type into agent sessions, answer prompts, interrupt",
                )),
                reply,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_reply(room.clone(), !reply, cx);
                }),
            ))
            .into_any_element()
    }
}

impl Render for PhonePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        let mut page = div().flex().flex_col().child(setting_note(
            ui,
            "A paired phone sees your agent sessions and can answer them. First open \
             splitlane.dev/app on the phone and add it to the home screen; then pair it \
             here. It reaches this computer through relay.splitlane.dev, encrypted between \
             the two: the relay forwards it unread.",
        ));
        match &self.phones {
            None => {}
            Some(Err(e)) => {
                page = page.child(
                    div()
                        .text_size(tok::text::CAPTION)
                        .text_color(ui.agent_error)
                        .child(SharedString::from(format!(
                            "The paired phones cannot be read: {e}"
                        ))),
                );
            }
            Some(Ok(phones)) => {
                for phone in phones {
                    page = page.child(self.render_phone(ui, phone, cx));
                }
            }
        }
        if let Some(problem) = &self.problem {
            page = page.child(
                div()
                    .pt(tok::space::MD)
                    .text_size(tok::text::CAPTION)
                    .text_color(ui.agent_error)
                    .child(problem.clone()),
            );
        }
        if self.pairing.is_some() {
            page = page.child(self.render_pairing(ui, cx));
        } else {
            page = page.child(setting_action_row(
                ui,
                "phone-pair-start",
                "+ Pair a phone",
                true,
                cx.listener(|this, _: &ClickEvent, window, cx| this.start_pairing(window, cx)),
            ));
            page = page.children(self.render_outcome(ui));
        }
        page
    }
}

/// A QR code's dark modules, with the side length in modules.
#[derive(Clone)]
struct QrModules {
    width: usize,
    dark: Vec<bool>,
}

impl QrModules {
    fn of(text: &str) -> Option<Self> {
        let code = qrcode::QrCode::new(text.as_bytes())
            .map_err(|e| log::warn!("pairing link does not fit a QR code: {e}"))
            .ok()?;
        Some(Self {
            width: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|module| module == qrcode::Color::Dark)
                .collect(),
        })
    }

    /// Painted, not laid out: a version-10 code is three thousand modules,
    /// and one quad each in a single canvas is cheaper than as many divs.
    ///
    /// Black on white whatever the theme. A light-on-dark code is one some
    /// phone cameras do not read, and a QR code is not part of the page's
    /// palette: it is an image for a camera.
    fn render(self) -> AnyElement {
        div()
            .size(px(QR_SIDE))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
                        window.paint_quad(fill(bounds, gpui::white()));
                        let side = self.width + 2 * QR_QUIET_MODULES;
                        let module = f32::from(bounds.size.width) / side as f32;
                        for (at, dark) in self.dark.iter().enumerate() {
                            if !dark {
                                continue;
                            }
                            let (x, y) = (at % self.width, at / self.width);
                            let origin = point(
                                bounds.origin.x + px((x + QR_QUIET_MODULES) as f32 * module),
                                bounds.origin.y + px((y + QR_QUIET_MODULES) as f32 * module),
                            );
                            // A hair over a module, so neighbours meet with no
                            // seam where the pixel grid rounds between them.
                            let quad =
                                Bounds::new(origin, size(px(module + 0.5), px(module + 0.5)));
                            window.paint_quad(fill(quad, gpui::black()));
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pairing_link_fits_a_code_a_phone_reads() {
        let link = splitlane_remote::Offer::new(
            &splitlane_remote::DeviceKey::generate().public(),
            1_800_000_000,
            300,
        )
        .to_link("https://splitlane.dev/app");
        let code = QrModules::of(&link).expect("a QR code");
        assert_eq!(code.dark.len(), code.width * code.width);
        // Version 10 at the default error correction is 57 modules a side;
        // past that the modules get too small for the side above.
        assert!(code.width <= 57, "{} modules", code.width);
    }
}
