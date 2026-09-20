use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Context, Decorations, EventEmitter, IntoElement,
    MouseButton, Render, SharedString, Styled, Transformation, Window, WindowControlArea, div,
    percentage, prelude::*, px, svg,
};

use super::csd::default_button_layout;
use crate::ui_tokens as tok;
use crate::{
    app::constants::{
        SIDEBAR_WIDTH, TITLE_BAR_CONTROL_SIZE, TITLE_BAR_EDGE_INSET, TITLE_BAR_MIN_HEIGHT,
    },
    ui_primitives::{AnimatedHoverExt, lerp_color},
};

pub struct TitleBar {
    should_move: bool,
    /// The active container's path, home-collapsed - the design's centred
    /// "current project path in mono". It replaced a workspace-name
    /// breadcrumb: a name is something the user typed, a path is where the
    /// agents are actually running.
    pub project_path: Option<String>,
    /// What Activity holds, counted across the whole window by
    /// `SplitlaneApp::activity_counts`. All-zero hides the chip entirely - the
    /// design shows it only when there is something to say.
    ///
    /// One struct rather than three numbers, because the chip's rung is a
    /// function of all of them together (`waiting::chip_state`) and a frame
    /// that received them separately could reach a rung the popover does not
    /// draw.
    pub activity: crate::app::waiting::ActivityCounts,
    /// The announcement in flight, when the queue's edge has just been crossed.
    ///
    /// It carries the edge's own instant, which is what binds the pulse to the
    /// transition rather than to this bar being drawn.
    pub announcement: Option<crate::app::announce::AnnouncePhase>,
    /// The chord that actually opens the palette, rendered by
    /// `keybindings::format_keystroke`. The design writes `⌘K` and macOS gets
    /// exactly that; Linux and Windows get whatever they are bound to, because
    /// this row is a control and a control may not name a key the build does
    /// not answer to.
    pub palette_chord: SharedString,
    pub sidebar_visible: bool,
    /// Stable expanded width of the active left rail. The body can animate to
    /// zero independently, while title-bar controls remain stationary and
    /// align with the open rail in CLI, Agents, Diff, and Settings.
    pub left_rail_width: f32,
    pub ipc_state: crate::ipc::IpcState,
    /// Set by SplitlaneApp when a newer version is detected.
    pub update_available: Option<UpdateInfo>,
    /// Whether the title bar owns the update / IPC-offline notices this frame.
    ///
    /// The same two notices have a second home in the rail footer
    /// (`render_sidebar_update_banner`, `render_sidebar_ipc_banner`), so
    /// exactly one of the two must carry them: `false` while the rail is
    /// showing its footer, `true` when it is hidden or replaced by the settings
    /// nav - which is when the pills are the only channel left. PUSHED by
    /// `SplitlaneApp::render`; `TitleBar` never reads `AppMode`.
    pub status_pills_visible: bool,
    /// Whether cockpit chrome should let the native material show through.
    /// Pushed by `SplitlaneApp::render` so the Windows Appearance switch can
    /// control title bar transparency independently from terminal cells.
    pub cockpit_material_active: bool,
    /// #10: subscription that repaints the title bar when the desktop
    /// environment relocates the window-control buttons (e.g. GNOME left↔right).
    /// Registered lazily on the first `render` (where `window` is available, as
    /// `new` has none); `None` until then. Dropping it on `TitleBar` drop
    /// unregisters the observer.
    button_layout_observer: Option<gpui::Subscription>,
}

#[derive(Clone)]
pub struct UpdateInfo {
    pub version: String,
    /// Which pill to render - the in-app flow or the system-package hint.
    pub kind: UpdatePillKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UpdatePillKind {
    /// In-app self-update flow (AppImage / tar.gz / unknown fallback).
    InApp(SelfUpdatePillState),
    /// Managed by the host's package manager. Clicking the pill
    /// never downloads - it shows a toast with the exact upgrade command.
    SystemManaged(SystemPackageKind),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SelfUpdatePillState {
    Idle,
    Downloading,
    Installing,
    /// Background install completed; the next click only invokes
    /// `cx.restart()`. Mirrors Zed's "Restart to Update" CTA - the heavy
    /// work happened while the user was busy doing something else, so the
    /// click→restart latency is bounded by GPUI's relauncher only (~100 ms).
    ReadyToRestart,
    Errored,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SystemPackageKind {
    /// Immutable Fedora variants (Silverblue / Kinoite / Bazzite) -
    /// detected via `/run/ostree-booted`. The pill surfaces a
    /// `rpm-ostree upgrade` hint rather than the usual `dnf`/`apt` copy.
    RpmOstree,
    /// `SystemPackage` was detected but neither apt, dnf, zypper, nor ostree
    /// markers were present (e.g., `eopkg` on Solus, `xbps` on Void).
    /// Apt/Dnf are intentionally absent: they route through the in-app
    /// pkexec installer (UpdatePillKind::InApp), not SystemManaged.
    Other,
}

/// Internal visual/interaction mode for the update pill.
#[derive(Clone, Copy)]
enum PillStyle {
    /// Default accent pill with a hover fade. Dispatches the in-app update
    /// action.
    Clickable,
    /// De-emphasized, non-interactive (download/install in flight).
    Busy,
    /// System-managed install: de-emphasized, default cursor, still clickable
    /// to reveal the package-manager hint toast.
    SystemHint,
}

impl TitleBar {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            should_move: false,
            project_path: None,
            activity: crate::app::waiting::ActivityCounts::default(),
            announcement: None,
            palette_chord: "⌘K".into(),
            sidebar_visible: true,
            left_rail_width: SIDEBAR_WIDTH,
            ipc_state: crate::ipc::IpcState::Online,
            update_available: None,
            status_pills_visible: true,
            cockpit_material_active: !cfg!(target_os = "windows"),
            button_layout_observer: None,
        }
    }
}

pub enum TitleBarEvent {
    CloseRequested,
    ToggleSidebar,
    /// The waiting chip was clicked: open the attention queue under it.
    ///
    /// It used to jump straight to the first waiting agent, which spent the
    /// chip's only click on a destination the user had not chosen. The chip
    /// says how many are waiting; the popover says which, and walking them is
    /// what the chord is for.
    ///
    /// An event rather than an action, because it is not a command anyone
    /// would look for in the palette - the chip is only ever on screen when
    /// its destination exists, and the palette already has the queue by name.
    WaitingChipClicked,
}

impl EventEmitter<TitleBarEvent> for TitleBar {}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // #10: repaint when the desktop environment relocates the window-control
        // buttons (GNOME left↔right) so `cx.button_layout()` below is never
        // stale until some unrelated repaint forces a frame. Registered once
        // here (not in `new`, which has no `Window`); the `Subscription` lives
        // in `self`. Mirrors Zed (`title_bar.rs:488`).
        if self.button_layout_observer.is_none() {
            self.button_layout_observer =
                Some(cx.observe_button_layout_changed(window, |_, _, cx| cx.notify()));
        }

        let height = (1.75 * window.rem_size()).max(TITLE_BAR_MIN_HEIGHT);
        let decorations = window.window_decorations();
        let is_csd = matches!(decorations, Decorations::Client { .. });
        // #9: under real server-side decorations (`window_decorations: server`,
        // opt-in; e.g. KDE Plasma) the compositor draws its own caption bar AND
        // this custom bar renders below it - they double up. We can't simply
        // drop this bar under SSD: it carries app chrome the compositor caption
        // does NOT (sidebar toggle, Files/Help menus, workspace tabs). The
        // min/max/close pill IS gated on `is_csd` below so those don't double;
        // the brand/menus row is best-effort under SSD. The default `client`
        // (CSD) path - which Splitlane uses everywhere it can - avoids this
        // entirely, which is why it is the default.

        // The parent window shell owns the active/inactive tint. This child is
        // transparent so blur is composed once and cannot refill rounded CSD
        // corner pixels with a rectangular background.
        let is_window_active = window.is_window_active();
        let chrome_bg = crate::app::constants::cockpit_chrome_background(
            crate::theme::ui_colors().chrome_for(is_window_active),
            self.cockpit_material_active,
        );

        // --- Read DE button layout ---
        let layout = cx.button_layout().unwrap_or_else(default_button_layout);
        let is_maximized = window.is_maximized();
        let supported = window.window_controls();

        // Close handler: emit CloseRequested so `SplitlaneApp` can intercept
        // (e.g., session save) before the window is removed.
        let close_handle = cx.entity().downgrade();
        let on_close = move |_window: &mut Window, cx: &mut gpui::App| {
            if let Some(entity) = close_handle.upgrade() {
                entity.update(cx, |_this, cx| cx.emit(TitleBarEvent::CloseRequested));
            }
        };

        // Paint our own window controls under CSD (Linux) and always on
        // Windows, where the transparent titlebar (`appears_transparent: true`)
        // hides the native caption buttons while gpui still reports
        // `Decorations::Server` - so `is_csd` is false and, without this guard,
        // the minimize/maximize/close buttons vanish entirely on Windows.
        // macOS keeps its native traffic lights, so it stays gated on `is_csd`
        // (false there). Mirrors the settings title bar (settings/window.rs).
        let render_controls = !window.is_fullscreen() && (is_csd || cfg!(target_os = "windows"));

        let left_controls = if render_controls {
            super::csd::render_button_group(
                "l",
                &layout.left,
                is_maximized,
                height,
                &supported,
                on_close.clone(),
            )
        } else {
            None
        };

        let right_controls = if render_controls {
            super::csd::render_button_group(
                "r",
                &layout.right,
                is_maximized,
                height,
                &supported,
                on_close,
            )
        } else {
            None
        };
        let left_controls_present = left_controls.is_some();
        let right_controls_present = right_controls.is_some();

        // --- Left section: brand slot, fixed width aligned with sidebar ---
        let ui = crate::theme::ui_colors();
        // On macOS, reserve the leftmost ~80px of the custom titlebar
        // for the native red/yellow/green traffic lights (positioned at
        // x=12,y=12 by WindowOptions::titlebar::traffic_light_position in
        // main.rs). Linux control groups own the shared 8px edge inset;
        // adding another brand inset would duplicate that spacing.
        //
        // In macOS fullscreen AppKit hides the traffic lights, so the 80px
        // reservation would leave a dead gap before the brand cluster - drop
        // back to the shared 8px inset there.
        let brand_pl = if cfg!(target_os = "macos") && !window.is_fullscreen() {
            gpui::px(80.0)
        } else if left_controls_present {
            gpui::px(0.)
        } else {
            TITLE_BAR_EDGE_INSET
        };
        let toggle_sidebar_handle = cx.entity().downgrade();
        let control_hover_bg = crate::app::constants::sidebar_tab_active_background();
        let toggle_sidebar_resting_bg = if self.sidebar_visible {
            control_hover_bg.opacity(0.0)
        } else {
            control_hover_bg
        };
        let sidebar_tooltip: gpui::SharedString = if self.sidebar_visible {
            "Hide sidebar"
        } else {
            "Show sidebar"
        }
        .into();
        let brand = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .pl(brand_pl)
            .pr(tok::space::XS)
            .overflow_x_hidden()
            .child(
                div()
                    .id("toggle-primary-sidebar")
                    .flex_none()
                    .size(TITLE_BAR_CONTROL_SIZE)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(tok::radius::TAG)
                    .animated_hover(move |style, delta| {
                        style.bg(lerp_color(
                            toggle_sidebar_resting_bg,
                            control_hover_bg,
                            delta,
                        ));
                    })
                    .tooltip(move |_window, cx| {
                        let label = sidebar_tooltip.clone();
                        cx.new(|_| crate::app::sidebar::SidebarTooltip { label })
                            .into()
                    })
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        if let Some(entity) = toggle_sidebar_handle.upgrade() {
                            entity.update(cx, |_this, cx| {
                                cx.emit(TitleBarEvent::ToggleSidebar);
                            });
                        }
                    })
                    .child(
                        svg()
                            .size(px(16.))
                            .path("icons/sidebar.svg")
                            .text_color(ui.muted),
                    ),
            )
            // Nothing else: this end of the bar is the window's controls and
            // the rail's toggle, and that is all it is for.
            //
            // The design draws an app **mark and name** here, and both are
            // gone. The name first: a window does not have to say which
            // application you are in, because the platform already does -
            // macOS puts "Splitlane" in the menu bar
            // (`install_macos_menu_bar`), Windows and Linux in the task list
            // and the switcher, off the same bundle identity. What a title bar
            // is for is the **document**, and this one already centres it: the
            // project's path. Cursor is the same arrangement, and it was the
            // owner's evidence for the change - no name in its own chrome, the
            // name in the menu bar, the file in the window title.
            //
            // The mark went with it rather than standing alone, and that was
            // decided by looking: a 15px accent square two positions from the
            // green traffic light reads as a fourth window control. It existed
            // to introduce the name; with nothing to introduce it was one more
            // round dot of colour in the row that already has three.
            ;
        // The brand slot is the same in every mode. An `if self.is_agents {}`
        // used to stand here with an `else if let Some(title) =
        // self.agents_thread_title` arm drawing `thread title · context` and a
        // `⋯` overflow button. The arm could never run - the push site filled
        // those fields only on the Agents arm and cleared them otherwise, so
        // `else` and `Some` were mutually exclusive - and the `if` body was
        // empty, leaving `is_agents` a field nothing read. All of it is gone.
        let left_rail = div()
            .flex_none()
            .w(px(self.left_rail_width))
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .overflow_x_hidden()
            .children(left_controls)
            .child(brand);

        // --- Center section: the active container's path ---
        // The design centres "current project path in mono" here. It used to
        // centre the workspace *name*, which is whatever the user last typed
        // into a rename box; a path says where the agents are actually running
        // and is the same string the rail's row tooltip shows.
        let mut content = div()
            .flex_1()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .px(tok::space::XL)
            .min_w_0();
        if let Some(path) = self.project_path.as_ref() {
            content = content.child(
                div()
                    .min_w_0()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::ROW)
                    .text_color(ui.dim)
                    .truncate()
                    .child(path.clone()),
            );
        }

        // --- Update available pill ---
        // One notice, one home per frame: the rail footer carries it while the
        // rail is up, the title bar takes over when it is not (see
        // `status_pills_visible`).
        let update_pill_visible = self.status_pills_visible;
        let update_pill = update_pill_visible
            .then(|| self.update_available.clone())
            .flatten()
            .map(|info| {
                // Decide label + visual style per install method. The click handler
                // always dispatches `StartSelfUpdate`; the action handler in
                // `SplitlaneApp` decides whether to download (in-app), trigger
                // an instant restart (ReadyToRestart), or show the
                // package-manager hint toast (system-managed).
                let (label, style): (String, PillStyle) = match info.kind {
                    UpdatePillKind::InApp(state) => match state {
                        SelfUpdatePillState::Idle => {
                            (format!("v{} available", info.version), PillStyle::Clickable)
                        }
                        SelfUpdatePillState::Downloading => {
                            ("Downloading update…".to_string(), PillStyle::Busy)
                        }
                        SelfUpdatePillState::Installing => {
                            ("Installing update…".to_string(), PillStyle::Busy)
                        }
                        SelfUpdatePillState::ReadyToRestart => {
                            ("Restart Splitlane".to_string(), PillStyle::Clickable)
                        }
                        SelfUpdatePillState::Errored => {
                            ("Update failed".to_string(), PillStyle::Clickable)
                        }
                    },
                    UpdatePillKind::SystemManaged(kind) => {
                        let label = match kind {
                            SystemPackageKind::RpmOstree => "Update via rpm-ostree".to_string(),
                            SystemPackageKind::Other => "Update via package manager".to_string(),
                        };
                        (label, PillStyle::SystemHint)
                    }
                };

                // Leading icon. Clickable renders `download.svg` for the
                // pre-install CTA and `refresh.svg` for the post-install
                // "Restart for vX" CTA so the user has a visual cue that the
                // heavy work is already done; Busy renders a `loader-circle.svg`
                // arc continuously rotating via GPUI's declarative
                // Animation+Transformation API (one full revolution per second,
                // repeat forever). Pattern mirrors
                // `crates/gpui/examples/animation.rs` in the upstream Zed repo.
                let is_ready_to_restart = matches!(
                    info.kind,
                    UpdatePillKind::InApp(SelfUpdatePillState::ReadyToRestart)
                );
                let leading_icon: AnyElement = match style {
                    PillStyle::Busy => svg()
                        .size(px(12.))
                        .flex_none()
                        .path("icons/loader-circle.svg")
                        .text_color(ui.muted)
                        .with_animation(
                            "update-pill-spinner",
                            Animation::new(Duration::from_secs(1)).repeat(),
                            |svg, delta| {
                                svg.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        )
                        .into_any_element(),
                    PillStyle::Clickable => svg()
                        .size(px(12.))
                        .flex_none()
                        .path(if is_ready_to_restart {
                            "icons/refresh.svg"
                        } else {
                            "icons/download.svg"
                        })
                        .text_color(ui.muted)
                        .into_any_element(),
                    PillStyle::SystemHint => svg()
                        .size(px(12.))
                        .flex_none()
                        .path("icons/tool.svg")
                        .text_color(ui.muted)
                        .into_any_element(),
                };

                // The pill sits inside the title bar's `WindowControlArea::Drag`
                // region declared on the parent. Its nested mouse-down handlers
                // stop propagation so interaction with the pill does not trigger
                // a window drag while the rest of the title bar remains draggable.
                // A small `×` dismiss affordance on the
                // non-busy states. We deliberately omit it during
                // Downloading/Installing/ReadyToRestart - those have a
                // user-perceivable side effect already in flight (or
                // sitting one click away from `cx.restart()`); a stray
                // dismiss there would be jarring. Errored remains
                // dismissable so a user with a chronic install failure
                // can hide the pill without having to bounce the app.
                let pill_dismissable = matches!(
                    info.kind,
                    UpdatePillKind::InApp(SelfUpdatePillState::Idle | SelfUpdatePillState::Errored)
                        | UpdatePillKind::SystemManaged(_)
                );

                let mut pill = div()
                    .id("update-pill")
                    .ml_auto()
                    .mr_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(tok::space::XS)
                    .px(tok::space::MD)
                    .h(px(24.))
                    .rounded(tok::radius::SMALL)
                    .border_1()
                    .border_color(ui.border)
                    .bg(ui.subtle)
                    .text_color(ui.text)
                    .text_size(tok::text::CAPTION)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(leading_icon)
                    .child(label);

                if pill_dismissable {
                    let muted = ui.muted;
                    let text = ui.text;
                    pill = pill.child(
                        div()
                            .id("update-pill-dismiss")
                            .ml(tok::space::XS)
                            .px(tok::space::XS)
                            .text_color(muted)
                            .text_size(tok::text::ROW)
                            .font_weight(gpui::FontWeight::BOLD)
                            .animated_hover(move |style, delta| {
                                style.text_color(lerp_color(muted, text, delta));
                            })
                            // stop_propagation on BOTH mouse-down and click
                            // so the click never reaches the parent pill's
                            // `on_click` handler that dispatches
                            // `StartSelfUpdate` - otherwise hitting the `×`
                            // would (a) dismiss the pill (b) immediately
                            // start the update we just dismissed.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .cursor_pointer()
                            .on_click(|_, window, cx| {
                                cx.stop_propagation();
                                window.dispatch_action(Box::new(crate::DismissUpdate), cx);
                            })
                            .child("×"),
                    );
                }
                match style {
                    // Dispatch on mouse-DOWN, not on click (mouse-up). At cold
                    // start the update check resolves before the user has
                    // touched the window, so the very first press on the pill
                    // happens against a window the compositor still considers
                    // inactive (Wayland focus-stealing prevention often
                    // rejects `cx.activate(true)`) and a focus chain that
                    // isn't yet initialized. In that state, `on_click`
                    // (which needs a matched press+release pair routed
                    // through the focus chain) silently drops the first
                    // interaction; the user has to click elsewhere to wake
                    // the chain, then re-click. Press-based dispatch avoids
                    // both races and matches the title-bar button idiom in
                    // Zed/VS Code/Discord. The pkexec modal confirms the
                    // action, so we don't lose "drag-out to cancel".
                    PillStyle::Clickable => pill
                        .animated_hover(move |style, delta| {
                            style
                                .bg(lerp_color(ui.subtle, ui.surface, delta))
                                .border_color(lerp_color(ui.border, ui.muted, delta));
                        })
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            window.dispatch_action(Box::new(crate::StartSelfUpdate), cx);
                        })
                        .into_any_element(),
                    PillStyle::Busy => pill.opacity(0.7).into_any_element(),
                    // SystemHint copies the upgrade command to the clipboard
                    // through a toast. It remains clickable with the default
                    // cursor, consistent with the Review/Diff chrome.
                    PillStyle::SystemHint => pill
                        .opacity(0.8)
                        .animated_hover(move |style, delta| {
                            style
                                .bg(lerp_color(ui.subtle, ui.surface, delta))
                                .border_color(lerp_color(ui.border, ui.muted, delta))
                                .opacity(0.8 + 0.2 * delta);
                        })
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            window.dispatch_action(Box::new(crate::StartSelfUpdate), cx);
                        })
                        .into_any_element(),
                }
            });
        // Same handover as the update pill - the rail's `render_sidebar_ipc_banner`
        // owns this notice whenever the rail is up.
        let ipc_pill = (update_pill_visible && self.ipc_state == crate::ipc::IpcState::Disabled)
            .then(|| {
                div()
                    .id("ipc-offline-pill")
                    .mr_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(tok::space::XS)
                    .px(tok::space::MD)
                    .h(px(24.))
                    .rounded(tok::radius::SMALL)
                    .border_1()
                    .border_color(ui.border)
                    .bg(ui.subtle)
                    .text_color(ui.text)
                    .text_size(tok::text::CAPTION)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(
                        svg()
                            .size(px(12.))
                            .flex_none()
                            .path("icons/triangle-alert.svg")
                            .text_color(ui.muted),
                    )
                    .child("IPC offline")
            });

        // --- The activity chip, and the palette hint beside it ---
        // On screen when the popover has something to say - sessions stopped on
        // the user, or runs that finished while they were looking elsewhere.
        // The counts come from `SplitlaneApp`, which sees agent surfaces and pane
        // sessions alike. One destination either way: the popover.
        let waiting_chip = crate::app::waiting::chip_state(self.activity).map(|chip| {
            let chip_handle = cx.entity().downgrade();
            let rung = chip.rung;
            let element = waiting_chip(&chip, self.announcement, ui).on_mouse_down(
                MouseButton::Left,
                move |_, _, cx| {
                    cx.stop_propagation();
                    if let Some(entity) = chip_handle.upgrade() {
                        entity.update(cx, |_this, cx| cx.emit(TitleBarEvent::WaitingChipClicked));
                    }
                },
            );
            // Applied here rather than inside the helper because
            // `animated_hover` changes the element's type and the click
            // listener has to go on first.
            if crate::app::waiting::chip_hovers(rung) {
                element
                    .animated_hover(move |style, delta| {
                        style.border_color(lerp_color(ui.border_strong, ui.border_hover, delta));
                    })
                    .into_any_element()
            } else {
                element.into_any_element()
            }
        });
        let palette_hint = palette_hint(self.palette_chord.clone(), ui);

        let bar = div()
            .id("title-bar")
            .window_control_area(WindowControlArea::Drag)
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(height)
            // The transparent fill reveals either the themed shell or the
            // platform material selected by the parent window.
            .bg(chrome_bg)
            // Windows remains flush for native caption hit targets. Linux
            // right-side controls already own their 8px edge inset; macOS
            // and layouts without right controls keep the bar-level inset.
            .when(
                !cfg!(target_os = "windows") && !right_controls_present,
                |d| d.pr(TITLE_BAR_EDGE_INSET),
            );

        bar
            // Drag-to-move state machine
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.should_move = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.should_move = false;
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.should_move = false;
            }))
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.should_move {
                    this.should_move = false;
                    window.start_window_move();
                }
            }))
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.zoom_window();
                }
            })
            // Right-click opens the DE's native window menu
            .when(supported.window_menu, |bar| {
                bar.on_mouse_down(MouseButton::Right, |ev, window, _| {
                    window.show_window_menu(ev.position);
                })
            })
            .child(left_rail)
            .child(content)
            .children(ipc_pill)
            .children(update_pill)
            .children(waiting_chip)
            .child(palette_hint)
            .children(right_controls)
        // The 1px bottom divider that used to live here was guarded by
        // `!is_agents && !cockpit`, i.e. never drawn since `cockpit` became
        // `!is_agents`. Removed rather than resurrected: every mode has shipped
        // without it, and the seamless chrome it was dropped for is the current
        // design, so bringing it back would be a new visual change, not a fix.
    }
}

/// The waiting chip's height. The design gives it 22 and a fully round end,
/// which is a physical shape rather than a step on the radius scale.
const WAITING_CHIP_HEIGHT: f32 = 22.;
/// The chip's leading dot.
const WAITING_CHIP_DOT: f32 = 6.;

/// The chip, drawn by `waiting::chip_body` and given this frame's geometry.
///
/// Built through the shared body rather than here because both frames need the
/// same three rungs, the same two dot shapes and the same announcement: under
/// client decorations the chip sits in this bar, and under a system frame -
/// where this bar is not drawn at all - the toolbar builds the same shape at
/// its right end. Which rung, and what it says, is
/// [`crate::app::waiting::chip_state`]'s, so the two frames cannot come to
/// disagree about when the chip exists, what it says, or how loudly.
fn waiting_chip(
    chip: &crate::app::waiting::ChipState,
    announcement: Option<crate::app::announce::AnnouncePhase>,
    ui: crate::theme::UiColors,
) -> gpui::Stateful<gpui::Div> {
    crate::app::waiting::chip_body(
        "title-bar-waiting-chip",
        WAITING_CHIP_HEIGHT,
        WAITING_CHIP_DOT,
        chip,
        announcement,
        ui,
    )
    .tooltip(crate::ui_primitives::text_tooltip(
        crate::app::waiting::chip_tooltip(chip),
    ))
}

/// The design's `⌘K` at the right end of the bar.
///
/// It is a hint in the design and a button here, and deliberately so: this row
/// is the one mouse path to the palette that survives hiding the rail. The
/// label is the chord that is really bound, which is the design's `⌘K` on
/// macOS and whatever Linux and Windows could safely take.
fn palette_hint(chord: SharedString, ui: crate::theme::UiColors) -> AnyElement {
    let resting = ui.dim;
    let hovered = ui.text_secondary;
    div()
        .id("title-bar-command-palette-trigger")
        .role(gpui::Role::Button)
        .aria_label("Open the command palette")
        .flex_none()
        .h(TITLE_BAR_CONTROL_SIZE)
        .px(tok::space::SM)
        .flex()
        .items_center()
        .rounded(tok::radius::SMALL)
        .font_family(tok::font::MONO)
        .text_size(tok::mono::PATH)
        .text_color(resting)
        .tooltip(crate::ui_primitives::text_tooltip("Command palette"))
        .animated_hover(move |style, delta| {
            style.text_color(lerp_color(resting, hovered, delta));
        })
        // Dispatch on press, not on click: at cold start the first press can
        // land on a window the compositor still considers inactive, and
        // `on_click` needs a matched press+release through the focus chain.
        // Same reasoning as the update pill above.
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(crate::OpenCommandPalette), cx);
        })
        .child(chord)
        .into_any_element()
}
