//! Bottom-of-sidebar mode tabs + Settings popover. The CLI, Review, and
//! Agents sidebars share one persistent mode switch, with Settings kept as
//! a compact utility button at the end of the row.
//!
//! The popover state (`SplitlaneApp::sidebar_actions_menu_open`) is
//! shared because only one sidebar is rendered at a time (mode toggle
//! swaps the whole sidebar tree).

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, FontWeight, InteractiveElement,
    IntoElement, MouseButton, ParentElement, SharedString, Styled, Transformation, div, percentage,
    prelude::*, px, svg,
};

use crate::SplitlaneApp;
use crate::settings::components::{select_item, select_menu_surface, with_alpha};
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;
use crate::window_chrome::title_bar::{SelfUpdatePillState, SystemPackageKind, UpdatePillKind};

const SIDEBAR_UPDATE_SHIMMER_MS: u64 = 2600;
const SIDEBAR_UPDATE_IRIS_COLORS: [u32; 5] = [0x2f6fff, 0x1da8ff, 0x8ea7ff, 0xb68cff, 0xf2f7ff];

impl SplitlaneApp {
    /// Update CTA banner at the bottom of the sidebar, above the Settings
    /// trigger. Replaces the title-bar update pill in the cockpit modes
    /// (Cli/Agents), where the title bar is a rail-confined overlay with no
    /// room for pills. Same states, labels, icons, and dismiss rules as the
    /// title-bar pill (`title_bar.rs`); same mouse-DOWN dispatch (Wayland
    /// focus-stealing prevention silently drops the first on_click after a
    /// cold start - see the title-bar pill comment for the full story).
    /// `None` when no update is available.
    pub(crate) fn render_sidebar_update_banner(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let info = self.update_pill_info()?;
        let ui = crate::theme::ui_colors();

        let (label, busy, system_hint): (String, bool, bool) = match info.kind {
            UpdatePillKind::InApp(state) => match state {
                SelfUpdatePillState::Idle => (format!("v{} available", info.version), false, false),
                SelfUpdatePillState::Downloading => ("Downloading update…".into(), true, false),
                SelfUpdatePillState::Installing => ("Installing update…".into(), true, false),
                SelfUpdatePillState::ReadyToRestart => ("Restart Splitlane".into(), false, false),
                SelfUpdatePillState::Errored => ("Update failed".into(), false, false),
            },
            UpdatePillKind::SystemManaged(kind) => {
                let label = match kind {
                    SystemPackageKind::RpmOstree => "Update via rpm-ostree".to_string(),
                    SystemPackageKind::Other => "Update via package manager".to_string(),
                };
                (label, false, true)
            }
        };
        let is_ready_to_restart = matches!(
            info.kind,
            UpdatePillKind::InApp(SelfUpdatePillState::ReadyToRestart)
        );
        let dismissable = matches!(
            info.kind,
            UpdatePillKind::InApp(SelfUpdatePillState::Idle | SelfUpdatePillState::Errored)
                | UpdatePillKind::SystemManaged(_)
        );
        let label_element = if matches!(info.kind, UpdatePillKind::InApp(SelfUpdatePillState::Idle))
        {
            render_update_available_label(&format!("v{}", info.version), ui)
        } else {
            render_update_plain_label(&label, ui)
        };

        let leading_icon: AnyElement = if busy {
            svg()
                .size(px(12.))
                .flex_none()
                .path("icons/loader-circle.svg")
                .text_color(ui.muted)
                .with_animation(
                    "sidebar-update-spinner",
                    Animation::new(Duration::from_secs(1)).repeat(),
                    |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
                )
                .into_any_element()
        } else {
            svg()
                .size(px(12.))
                .flex_none()
                .path(if system_hint {
                    "icons/tool.svg"
                } else if is_ready_to_restart {
                    "icons/refresh.svg"
                } else {
                    "icons/download.svg"
                })
                .text_color(ui.muted)
                .into_any_element()
        };

        let mut banner = div()
            .id("sidebar-update-banner")
            .mx(tok::space::XS)
            .mb(tok::space::XS)
            .h(tok::row::SEARCH)
            .px(tok::space::MD)
            .rounded(crate::app::constants::SIDEBAR_TAB_CORNER_RADIUS)
            .bg(crate::app::constants::sidebar_tab_active_background())
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .child(leading_icon)
            .child(label_element);

        if dismissable {
            let muted = ui.muted;
            let text = ui.text;
            banner = banner.child(
                div()
                    .id("sidebar-update-dismiss")
                    .px(tok::space::XS)
                    .text_color(muted)
                    .text_size(tok::text::ROW)
                    .font_weight(FontWeight::BOLD)
                    .animated_hover(move |style, delta| {
                        style.text_color(lerp_color(muted, text, delta));
                    })
                    // stop_propagation on BOTH mouse-down and click so the
                    // press never reaches the banner's StartSelfUpdate
                    // dispatch - hitting × must not start the update it
                    // just dismissed.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.handle_dismiss_update(&crate::DismissUpdate, window, cx);
                    }))
                    .child("×"),
            );
        }

        let banner = if busy {
            banner.opacity(0.7).into_any_element()
        } else {
            let resting_opacity = if system_hint { 0.8 } else { 1.0 };
            banner
                .opacity(resting_opacity)
                .animated_hover(move |style, delta| {
                    style.opacity(lerp(resting_opacity, 1.0, delta));
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.handle_start_self_update(&crate::StartSelfUpdate, window, cx);
                    }),
                )
                .into_any_element()
        };

        Some(banner)
    }

    /// "IPC offline" notice at the bottom of the sidebar - the cockpit home
    /// of the title-bar IPC pill (same rail-confinement story as the update
    /// banner). Purely informational, like the original pill: no click
    /// handler. `None` while the IPC server is up.
    pub(crate) fn render_sidebar_ipc_banner(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.ipc_status.state() != crate::ipc::IpcState::Disabled {
            return None;
        }
        let ui = crate::theme::ui_colors();
        Some(
            div()
                .id("sidebar-ipc-banner")
                .mx(tok::space::XS)
                .mb(tok::space::XS)
                .px(tok::space::MD)
                .py(tok::space::XS)
                .rounded(tok::radius::SMALL)
                .border_1()
                .border_color(ui.border)
                .bg(ui.subtle)
                .flex()
                .flex_row()
                .items_center()
                .gap(tok::space::XS)
                .child(
                    svg()
                        .size(px(12.))
                        .flex_none()
                        .path("icons/triangle-alert.svg")
                        .text_color(ui.muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(ui.text)
                        .text_size(tok::text::ROW)
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child("IPC offline"),
                )
                .into_any_element(),
        )
    }

    /// Render the bottom of the rail: the one entry to the application layer
    /// (Settings, About - which has no scope and sits apart from the container
    /// tree), with the IPC and update notices stacked above it.
    pub(crate) fn render_sidebar_settings_footer(
        &self,
        items: Vec<SidebarMenuItem>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = crate::theme::ui_colors();
        let settings_open = self.agents_view.sidebar_actions_menu_open;

        let settings_hover_bg = crate::app::constants::sidebar_tab_active_background();
        let settings_resting_bg = if settings_open {
            settings_hover_bg
        } else {
            settings_hover_bg.opacity(0.0)
        };
        // The design makes this its **application row**: full width, 30
        // high, gear plus the word, with a rule above it. It was a 30x30 gear
        // and nothing else, and the designer's own reason for the label is
        // worth keeping written down - the orientation control lost its single
        // -toggle form because its *state* was invisible, not because it was an
        // icon. This row has no state to misread; it needs a name, and the
        // footer is full width, so there is no reason to make the user hover a
        // gear to learn where it goes.
        let settings_trigger = div()
            .id("sidebar-settings-trigger")
            .flex_none()
            // A role, not a number: this is a row in the rail, and the rail's
            // scale has a step for it. It was a bare `px(30.)` - a size the
            // call site named for itself, which is the one thing the styling
            // convention forbids, and it landed between two real steps.
            //
            // `PROJECT` and not `SESSION`: the entry to the application layer
            // is a peer of a container row, not of a surface inside one.
            .h(tok::row::PROJECT)
            .w_full()
            .px(tok::space::MD)
            .rounded(tok::radius::TAG)
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::MD)
            .animated_hover_bg(settings_resting_bg, settings_hover_bg)
            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.agents_view.sidebar_actions_menu_open =
                    !this.agents_view.sidebar_actions_menu_open;
                this.agents_view.sidebar_mode_picker_open = false;
                cx.notify();
            }))
            .child(
                svg()
                    .size(px(13.))
                    .flex_none()
                    .path("icons/settings.svg")
                    .text_color(ui.muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(tok::text::ROW)
                    .text_color(ui.muted)
                    .child(SharedString::from("Settings")),
            );

        let settings_popover: Option<AnyElement> = if settings_open {
            // Vertical menu opening upward from the trigger. Mirrors the
            // Settings "Shell" select menu (`components::select_menu`) that the
            // title-bar "Files" / "Help" dropdowns also use: the same elevated
            // surface, hairline border at 0.6 alpha, soft shadow, 10px radius
            // and 4px padding, so every app menu reads as one consistent menu
            // language. The container is open-coded (not `select_menu`) because
            // this popover stretches to the sidebar width via left/right, which
            // would fight `select_menu`'s fixed 200-280px clamp.
            let mut menu = div()
                .id("sidebar-settings-popover")
                .absolute()
                .left(px(6.))
                .right(px(6.))
                .bottom(px(44.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .p(tok::space::XS)
                .rounded(tok::radius::MENU)
                .bg(select_menu_surface(ui))
                .border_1()
                .border_color(with_alpha(ui.border, 0.6))
                // Click anywhere outside the popover (or its trigger)
                // dismisses it. Same pattern as `profile_menu.rs`.
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if this.agents_view.sidebar_actions_menu_open {
                        this.agents_view.sidebar_actions_menu_open = false;
                        cx.notify();
                    }
                }));
            for item in items {
                menu = menu.child(render_menu_item(item, ui, cx));
            }
            Some(menu.into_any_element())
        } else {
            None
        };

        // The footer used to carry a CLI / Review / Agents segment row - the
        // mode switch. There are no modes; what those three segments selected
        // is now a surface picked in the list above, so only the one entry to
        // the application layer is left.
        // The design puts this row *inside* the limits footer, under a rule of
        // its own - one panel at the foot of the rail, not two stacked blocks.
        // The panel's own padding is the caller's; the row brings the rule.
        let footer_row: AnyElement = div()
            .id("sidebar-footer")
            // The rule above already separates it; the gap only has to keep the
            // row off the line. The whole block was 45px tall for a 30px row.
            .pt(tok::space::XS)
            .border_t_1()
            .border_color(ui.divider)
            .flex()
            .flex_row()
            .items_center()
            .child(settings_trigger)
            .into_any_element();

        let mut footer = div().relative().flex_none();
        if let Some(popover) = settings_popover {
            footer = footer.child(popover);
        }
        footer.child(footer_row).into_any_element()
    }

    /// The notices that stack above the footer panel: "IPC offline" first, then
    /// the update CTA. They are the cockpit homes of the old title-bar pills,
    /// and they sit outside the panel because the panel is the design's limits
    /// footer and has no room in it for news.
    pub(crate) fn render_sidebar_banners(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut banners = Vec::new();
        if let Some(banner) = self.render_sidebar_ipc_banner(cx) {
            banners.push(banner);
        }
        if let Some(banner) = self.render_sidebar_update_banner(cx) {
            banners.push(banner);
        }
        banners
    }
}

/// Where the application layer's help entries point. They used to hang off a
/// "Help" menu in the title bar; the URLs came with them unchanged.
pub(crate) mod help_url {
    // These used to name the site of the project this one derives from - so
    // every help link in the rail footer sent the user to somebody else's
    // documentation for somebody else's build.
    // The same landmine `update/checker.rs` documents for the release feed
    // and `assets/Info.plist` for the bundle identifier. The docs this build
    // ships live in its own repository, so that is where they point.
    pub(crate) const DOCUMENTATION: &str =
        "https://github.com/ivkan/splitlane/blob/main/docs/user/index.md";
    pub(crate) const RELEASES: &str = "https://github.com/ivkan/splitlane/releases";
    pub(crate) const AUTOMATIONS: &str =
        "https://github.com/ivkan/splitlane/blob/main/docs/user/scripting.md";
    pub(crate) const REVIEW: &str =
        "https://github.com/ivkan/splitlane/blob/main/docs/user/review.md";
    pub(crate) const TROUBLESHOOTING: &str =
        "https://github.com/ivkan/splitlane/blob/main/docs/user/troubleshooting.md";
}

impl SplitlaneApp {
    /// Open one of the [`help_url`] links in the user's browser, reporting a
    /// failure as a toast rather than swallowing it.
    pub(crate) fn open_help_url(&mut self, url: &'static str, cx: &mut Context<Self>) {
        if let Err(err) = crate::external_open::open_url(url) {
            log::warn!("help: open URL failed: {err}");
            self.show_toast(format!("Could not open URL: {err}"), cx);
        }
    }
}

pub(crate) type SidebarMenuAction =
    Box<dyn Fn(&mut SplitlaneApp, &mut gpui::Window, &mut Context<SplitlaneApp>) + 'static>;

/// A single action row in the Settings popover.
pub(crate) struct SidebarMenuItem {
    pub id: SharedString,
    pub icon: &'static str,
    pub label: SharedString,
    pub on_click: SidebarMenuAction,
}

fn render_menu_item(
    item: SidebarMenuItem,
    ui: crate::theme::UiColors,
    cx: &mut Context<SplitlaneApp>,
) -> AnyElement {
    let handler = item.on_click;
    select_item(item.id, false, ui)
        .on_click(cx.listener(move |this, _: &ClickEvent, w, cx| {
            handler(this, w, cx);
            this.agents_view.sidebar_actions_menu_open = false;
            cx.notify();
        }))
        .child(
            svg()
                .size(px(12.))
                .flex_none()
                .path(item.icon)
                .text_color(ui.muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(ui.text)
                .truncate()
                .child(item.label),
        )
        .into_any_element()
}

fn render_update_available_label(version: &str, ui: crate::theme::UiColors) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .text_size(tok::text::ROW)
        .font_weight(FontWeight::BOLD)
        .truncate()
        .flex()
        .flex_row()
        .child(render_update_shimmer_text(
            version,
            gpui::Hsla::from(gpui::rgb(SIDEBAR_UPDATE_IRIS_COLORS[2])).opacity(0.88),
        ))
        .child(div().text_color(ui.text).child(" available"))
        .into_any_element()
}

fn render_update_plain_label(label: &str, ui: crate::theme::UiColors) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .text_size(tok::text::ROW)
        .font_weight(FontWeight::BOLD)
        .text_color(ui.text)
        .truncate()
        .child(label.to_string())
        .into_any_element()
}

fn render_update_shimmer_text(label: &str, base_color: gpui::Hsla) -> AnyElement {
    let letter_count = label.chars().count() as f32;

    div()
        .flex()
        .flex_row()
        .children(label.chars().enumerate().map(|(index, ch)| {
            div()
                .text_color(base_color)
                .child(ch.to_string())
                .with_animation(
                    SharedString::from(format!("sidebar-update-shimmer-letter-{index}")),
                    Animation::new(Duration::from_millis(SIDEBAR_UPDATE_SHIMMER_MS)).repeat(),
                    move |letter, delta| {
                        letter.text_color(update_shimmer_color(
                            base_color,
                            index,
                            letter_count,
                            delta,
                        ))
                    },
                )
                .into_any_element()
        }))
        .into_any_element()
}

fn update_shimmer_color(
    base_color: gpui::Hsla,
    index: usize,
    letter_count: f32,
    delta: f32,
) -> gpui::Hsla {
    let width = letter_count.max(1.);
    let letter_phase = index as f32 / width;
    let phase = (delta + letter_phase * 0.32).fract();
    let iris_color = update_iris_color_at(phase);
    let highlight = ((phase * std::f32::consts::TAU).sin() + 1.) * 0.5;
    let hue = lerp(base_color.h, iris_color.h, 0.92);
    let saturation = lerp(base_color.s, iris_color.s, 0.9);
    let lightness = (lerp(base_color.l, iris_color.l, 0.9) + highlight * 0.035).min(0.94);
    let alpha = lerp(base_color.a, 0.98, 0.86);

    gpui::hsla(hue, saturation, lightness, alpha)
}

fn update_iris_color_at(phase: f32) -> gpui::Hsla {
    let palette_len = SIDEBAR_UPDATE_IRIS_COLORS.len();
    let scaled = phase * palette_len as f32;
    let start = scaled.floor() as usize % palette_len;
    let end = (start + 1) % palette_len;
    let amount = scaled.fract();
    let a = gpui::Hsla::from(gpui::rgb(SIDEBAR_UPDATE_IRIS_COLORS[start]));
    let b = gpui::Hsla::from(gpui::rgb(SIDEBAR_UPDATE_IRIS_COLORS[end]));

    gpui::hsla(
        lerp(a.h, b.h, amount),
        lerp(a.s, b.s, amount),
        lerp(a.l, b.l, amount),
        lerp(a.a, b.a, amount),
    )
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}
