//! About Splitlane modal, styled as a compact native application dialog.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, ObjectFit,
    ParentElement, Styled, deferred, div, img, prelude::*, px, svg,
};

use crate::ui_tokens as tok;
use crate::{
    SplitlaneApp,
    ui_primitives::{AnimatedHoverExt, lerp_color},
};

/// When the running binary was linked, read from the executable's own mtime
/// and formatted as UTC ISO 8601.
///
/// Deliberately a runtime read rather than a constant baked in by `build.rs`:
/// a constant is only refreshed when Cargo re-runs the build script, so it can
/// claim a build newer than the binary actually on screen - the exact failure
/// this line exists to catch. An executable's mtime cannot be wrong about
/// which file is running. UTC because a local-time conversion would cost a
/// dependency for one developer-facing label.
///
/// `None` when the path or its metadata cannot be read (a deleted or replaced
/// executable while running, or a platform that denies the stat); the dialog
/// then shows the revision alone rather than a made-up time.
fn build_timestamp() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let modified = std::fs::metadata(exe).ok()?.modified().ok()?;
    let ms = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(crate::opencode_sessions::unix_ms_to_iso8601(
        i64::try_from(ms).ok()?,
    ))
}

impl SplitlaneApp {
    pub(crate) fn render_about_dialog(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = crate::theme::ui_colors();
        let version = env!("CARGO_PKG_VERSION");
        // Which code is actually running: the commit it was compiled from
        // (`build.rs`) and when this very file was linked. `CARGO_PKG_VERSION`
        // alone is identical across every build of a version, so it can never
        // answer that question.
        let revision = env!("SPLITLANE_GIT_REV");
        let build_line = match build_timestamp() {
            Some(built) => format!("{revision} · built {built}"),
            None => revision.to_owned(),
        };
        // Every fill in this dialog used to be a hardcoded dark hex, which
        // on the light theme drew a black card in the middle of a white app.
        // They are roles now, and the dialog is the application layer's, not
        // the dark theme's.
        let button_hover_bg = ui.subtle;

        let close_x = div()
            .id("about-close-x")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(30.))
            .h(px(30.))
            .rounded(tok::radius::CONTROL)
            .animated_hover(move |style, delta| {
                style.bg(lerp_color(
                    button_hover_bg.opacity(0.0),
                    button_hover_bg,
                    delta,
                ));
            })
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.show_about_dialog = false;
                cx.notify();
                cx.stop_propagation();
            }))
            .child(
                svg()
                    .size(px(16.))
                    .flex_none()
                    .path("icons/close.svg")
                    .text_color(ui.text),
            );

        let header = div()
            .h(tok::row::HEADER)
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .pl(tok::space::MD)
            .pr(tok::space::XS)
            .bg(ui.chrome)
            .border_b_1()
            .border_color(ui.divider)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(tok::space::MD)
                    .child(
                        img("icons/splitlane.png")
                            .w(px(16.))
                            .h(px(16.))
                            .object_fit(ObjectFit::Contain),
                    )
                    .child(
                        div()
                            .text_size(tok::text::ROW)
                            .font_weight(gpui::FontWeight::NORMAL)
                            .text_color(ui.text)
                            .child("About Splitlane"),
                    ),
            )
            .child(close_x);

        let body = div()
            .w_full()
            .h(px(225.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(ui.overlay)
            .child(
                img("icons/splitlane.png")
                    .w(px(64.))
                    .h(px(64.))
                    .object_fit(ObjectFit::Contain),
            )
            .child(
                div()
                    .mt(tok::space::XL)
                    .text_color(ui.text)
                    .text_size(tok::text::DISPLAY)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Splitlane"),
            )
            .child(
                div()
                    .mt(tok::space::XXL)
                    .text_color(ui.muted)
                    .text_size(tok::text::ROW)
                    .child(format!("Version {version}")),
            )
            .child(
                div()
                    .mt(tok::space::XS)
                    .text_color(ui.muted)
                    .text_size(tok::text::CAPTION)
                    .child(build_line),
            )
            .child(
                div()
                    .mt(tok::space::XL)
                    .text_color(ui.muted)
                    .text_size(tok::text::ROW)
                    .child("© 2025 Arthur Jean, 2026 Ivan Kalashnik"),
            );

        let ok_button = div()
            .id("about-ok")
            .w(px(76.))
            .h(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(tok::radius::METER)
            .border_1()
            .border_color(ui.border_hover)
            .bg(ui.subtle)
            .text_size(tok::text::ROW)
            .text_color(ui.text)
            .animated_hover(move |style, delta| {
                style.bg(lerp_color(ui.subtle, ui.border, delta));
            })
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.show_about_dialog = false;
                cx.notify();
                cx.stop_propagation();
            }))
            .child("OK");

        let footer = div()
            .w_full()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .px(tok::space::XL)
            .bg(ui.surface)
            .border_t_1()
            .border_color(ui.divider)
            .child(ok_button);

        let dialog = div()
            .id("about-dialog")
            .occlude()
            .w(px(382.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border_dialog)
            .rounded(tok::radius::WINDOW)
            .shadow(crate::ui_primitives::dialog_shadow(ui))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(body)
            .child(footer);

        deferred(
            div()
                .id("about-dialog-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(ui.scrim)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.show_about_dialog = false;
                        cx.notify();
                    }),
                )
                .child(dialog),
        )
        .with_priority(10)
        .into_any_element()
    }
}
