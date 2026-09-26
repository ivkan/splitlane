// Test-only allow for the CLAUDE.md-mandated clippy restrictions. These
// lints are also demoted to `allow` at crate level in `src-app/Cargo.toml`
// for pre-existing GPUI UI-code unwraps,
// so today this belt is effectively redundant - but it stays in place so
// that when the eventual cleanup story re-promotes the Cargo.toml lints
// to `warn`, tests continue to pass without another edit here.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unwrap_in_result,
        clippy::panic
    )
)]
// Windows deliberately stays a console-subsystem binary. PowerShell/cmd do not
// wait for GUI-subsystem executables, so `splitlane ls` would otherwise return
// immediately with no stdout/stderr and a misleading success code. GUI launches
// still shed the auto-created one-process console at startup; see
// `detach_lonely_windows_console_for_gui_launch`.
//! Splitlane - native terminal workspace for coding agents.
//!
//! App shell with sidebar workspace list, terminal panes, agent surfaces, and
//! diff/review workflows.

mod agent_launcher;
mod agent_sessions;
mod agent_state;
mod agents;
mod agents_view;
mod ai_hooks;
mod ai_types;
mod app;
mod assets;
mod claude_pid_state;
mod claude_sessions;
mod claude_usage;
mod cli;
mod codex_sessions;
mod codex_state;
mod command_sessions;
mod config_writer;
mod diff;
mod editor;
mod external_open;
mod file_view;
mod fonts;
mod ipc;
mod ipc_events;
mod keybindings;
mod keys;
mod launch_cwd;
mod layout;
mod limits;
mod login_shell_env;
mod markdown;
mod mouse;
mod opencode_sessions;
mod pane;
mod pane_drag;
mod pi_sessions;
mod preset;
mod pricing;
mod process_tree;
mod project;
mod runtime_paths;
mod search;
mod settings;
mod telemetry;
mod terminal;
pub mod theme;
mod ui_primitives;
mod ui_tokens;
mod update;
mod vendor_limits;
mod widgets;
mod window_chrome;
mod window_state;
mod windows_app_identity;
mod workspace;

use crate::window_chrome::title_bar;

use gpui::{
    Animation, AnimationExt, App, Context, CursorStyle, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, PathBuilder, Pixels, Point, Render, SharedString, Styled,
    Window, WindowBounds, WindowDecorations, WindowOptions, canvas, div, point, prelude::*, px,
};
use gpui_platform::application;
use notify::Watcher;

use crate::pane::{Pane, TabContent};
use crate::terminal::TerminalView;
use crate::ui_tokens as tok;
use crate::workspace::Workspace;

// Re-export action types at the crate root so existing `crate::SplitHorizontally`
// references in sibling modules keep compiling without a crate-wide import churn.
pub use app::actions::*;
// Items extracted out of `main.rs` are re-exported at crate root
// so callers like `crate::TOAST_HOLD_MS` keep resolving without an
// import-rewrite churn across the workspace.
pub(crate) use app::constants::{
    MAX_CLOSED_PANE_SCROLLBACK_BYTES, MAX_CLOSED_PANES, RESIZE_BORDER, TOAST_HOLD_MS,
};
// `TOAST_ENTER_MS` and `TOAST_EXIT_MS` are used only by the toast
// renderer inside `app::notifications`; not re-exported at crate root.
pub(crate) use app::drag::{WorkspaceDrag, WorkspaceDragPreview};
pub(crate) use app::notifications::{Toast, ToastAction};
// Free helpers extracted to bootstrap.rs but still callable as
// `crate::system_package_update_command` etc. from sibling modules.
#[cfg(target_os = "macos")]
pub(crate) use app::bootstrap::{
    install_macos_menu_action_fallbacks, install_macos_menu_bar, warn_if_rosetta_translated,
};
pub(crate) use app::bootstrap::{system_package_update_command, warn_if_legacy_run_install};

// Terminal-routing helpers (`find_first_terminal`, `find_terminal_by_surface_id`)
// live in `app::ipc_handler` - its only consumer.

// ---------------------------------------------------------------------------
// Root application view
// ---------------------------------------------------------------------------

/// A page in the embedded settings experience (Codex-style: grouped nav on the
/// left rail, the section body on the right). `General` is the landing page.
/// One source of truth - replaces the old 2-variant inline enum *and* the
/// standalone window's copy, now that settings render inline (`settings::chrome`).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SettingsSection {
    General,
    Appearance,
    Shortcuts,
    Terminal,
    Notifications,
    AiAgent,
    McpServers,
    Presets,
    /// Agent skills. In the application layer with the rest, because `FOCUS.md`
    /// says so: "Skills belongs with Settings, in the application layer, not in
    /// a pane. It is not attached to a project or a session, nothing in it is
    /// per-pane, and giving it a pane would make the ladder answer questions
    /// about a surface that has no kind."
    Skills,
    /// The Claude plan limits the rail footer reads: which of the two windows
    /// it shows, when the meter changes colour, and where the numbers come
    /// from. In the application layer for the same reason as Skills - none of
    /// it is per-project or per-pane.
    Limits,
}

/// Light / dark / system selector shown at the top of the Themes settings page.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ThemeMode {
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub(crate) fn from_config(mode: Option<&str>, theme_name: Option<&str>) -> Self {
        match mode.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("light") => Self::Light,
            Some("dark") => Self::Dark,
            Some("system") => Self::System,
            _ => Self::from_theme_name(theme_name.unwrap_or(crate::theme::DEFAULT_THEME_NAME)),
        }
    }

    pub(crate) fn from_theme_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case(crate::theme::DEFAULT_LIGHT_THEME_NAME) {
            Self::Light
        } else {
            Self::Dark
        }
    }

    pub(crate) fn as_config_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub(crate) fn resolved_theme_name(self, appearance: gpui::WindowAppearance) -> &'static str {
        match self {
            Self::Light => crate::theme::DEFAULT_LIGHT_THEME_NAME,
            Self::Dark => crate::theme::DEFAULT_THEME_NAME,
            Self::System => {
                if Self::appearance_is_light(appearance) {
                    crate::theme::DEFAULT_LIGHT_THEME_NAME
                } else {
                    crate::theme::DEFAULT_THEME_NAME
                }
            }
        }
    }

    pub(crate) fn appearance_is_light(appearance: gpui::WindowAppearance) -> bool {
        matches!(
            appearance,
            gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
        )
    }
}

/// Which Terminal-page enum dropdown is currently open (only one at a time).
/// `None` = all closed. Distinct from `font_dropdown_open` (the Terminal page's
/// searchable font picker) so only one popover is active at a time.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TerminalDropdown {
    Shell,
    Scrollback,
    FontSize,
    LineHeight,
    CellWidth,
    FontWeight,
    CursorShape,
    CursorColor,
}

/// Which General-page select dropdown is currently open (only one at a time).
/// `None` = all closed. Mirrors `TerminalDropdown` so navigating away or opening
/// the other select never leaves a ghost popover.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GeneralDropdown {
    Editor,
    /// Settings -> AI Agent: which agent the new-agent chord starts in a
    /// project that has never been asked. One dropdown state serves both
    /// pages; the row anatomy is the same and so is the open/close rule.
    DefaultAgent,
    /// Settings -> Notifications: the ladder, and the only control in its
    /// section.
    NotifyLevel,
}

#[derive(Clone, Copy)]
pub(crate) struct WorkspaceContextMenu {
    pub(crate) idx: usize,
    pub(crate) position: Point<Pixels>,
    /// `Add to group ▸` is open beside the menu.
    pub(crate) group_submenu: bool,
}

/// Open "Move to pane…" tab context menu. Identifies the tab
/// by stable entity id plus owning pane; the destination panes are
/// resolved at render time from the workspace's split tree.
#[derive(Clone)]
pub(crate) struct TabContextMenu {
    pub(crate) source_pane: Entity<Pane>,
    pub(crate) tab_id: gpui::EntityId,
    pub(crate) position: Point<Pixels>,
}

/// Open right-click menu for a Files-sidebar row. Carries the row's absolute
/// path and the click anchor; "Copy relative path" resolves the workspace root
/// at render/action time.
#[derive(Clone)]
pub(crate) struct FilesContextMenu {
    pub(crate) path: std::path::PathBuf,
    /// Which of the two kinds of row this is, captured when the menu opened.
    ///
    /// The tree already knows - every row is built from a `FileNode` - and a
    /// menu that asked disk instead would be asking on the render thread,
    /// once a frame, for an answer that cannot change while the menu is up.
    pub(crate) is_dir: bool,
    /// Whether the open row says "editor" or "the default app", answered on
    /// the right-click.
    ///
    /// It is a field and not a call from the menu's render for the reason a
    /// render is a render: `render_files_context_menu` runs on **every** frame
    /// the menu is up - a hover, a toast, anything that notifies - so a disk
    /// read there is a disk read per frame, on the thread that draws. Once per
    /// gesture is the cost the surface's own menu pays, and this is that.
    pub(crate) opens_in_editor: bool,
    pub(crate) position: Point<Pixels>,
}

/// Captured state of a closed pane for undo-close-pane.
pub(crate) enum ClosedTabRecord {
    Terminal {
        cwd: Option<std::path::PathBuf>,
        scrollback: Option<String>,
        custom_name: Option<String>,
        font_size: Option<f32>,
    },
    Markdown {
        path: std::path::PathBuf,
    },
}

pub(crate) struct ClosedPaneRecord {
    pub(crate) tabs: Vec<ClosedTabRecord>,
    pub(crate) selected_idx: usize,
    pub(crate) workspace_idx: usize,
}

/// In-app self-update flow state, extracted from the `SplitlaneApp`
/// god-struct. Grouped: the background-check slot/result, the live flow
/// status, the detected install method, and the consecutive-failure counter.
struct SelfUpdateState {
    /// Shared slot for the background update checker result.
    pending_update: update::checker::SharedUpdateSlot,
    /// Resolved update status (set once the background check completes).
    update_status: Option<update::checker::UpdateStatus>,
    /// Live state of the in-app self-update flow (download → install → restart).
    self_update_status: update::SelfUpdateStatus,
    /// How the running binary was installed. Detected once at startup -
    /// drives the update pill's label/click behaviour and the
    /// in-app updater's branch selection.
    install_method: update::install_method::InstallMethod,
    /// Count of consecutive in-app update failures since process start
    /// Bumped on every classified error; after 3 failures the
    /// 4th click skips the network and shows the "download manually"
    /// escape hatch toast.
    ///
    /// Never decremented. The only success path for an update calls
    /// `cx.restart()`, which replaces this process - the fresh
    /// `SplitlaneApp::new` initializes the counter back to 0. So "failures
    /// since last success" and "failures since process start" coincide by
    /// construction; the "three consecutive failures" requirement
    /// holds without an explicit reset.
    update_attempt_count: u32,
    /// Monotonic token identifying the current `Downloading` attempt.
    /// Bumped each time the flow enters `Downloading`; the per-attempt
    /// watchdog captures the value and only fires if it still matches - so a
    /// stale watchdog from a superseded attempt can't reset a newer one.
    download_generation: u64,
}

const PRIMARY_SIDEBAR_ANIMATION_MS: u64 = 280;
const PRIMARY_SIDEBAR_MIN_ANIMATION_DELTA: f32 = 0.5;
/// The wordmark, one letter per element because each shimmers on its own clock.
///
/// **Spelling it out is why the rename missed it.** `tests/product_name_policy.rs`
/// greps for the old name, and there was no such string here to find - only
/// eight one-character ones. The app therefore kept announcing itself under the
/// old name on every launch, on the one screen that exists to say what it is.
/// `the_old_name_is_not_spelled_out_letter_by_letter` is the machine form of
/// that lesson; the note in `CLAUDE.md` about names coming back "a word at a
/// time" now has a second shape to it.
const STARTUP_SPLASH_TEXT: [&str; 9] = ["S", "p", "l", "i", "t", "l", "a", "n", "e"];
const STARTUP_SPLASH_LETTER_COUNT: f32 = STARTUP_SPLASH_TEXT.len() as f32;
const STARTUP_SPLASH_TEXT_ALPHA: f32 = 0.54;
const STARTUP_SPLASH_SHIMMER_ALPHA: f32 = 0.82;
const STARTUP_SPLASH_SHIMMER_MS: u64 = 2600;
const STARTUP_SPLASH_MIN_VISIBLE_MS: u64 = 900;

#[derive(Clone, Copy)]
struct SidebarWidthAnimation {
    from_width: f32,
    to_width: f32,
    started_at: std::time::Instant,
}

struct StartupSplashView {
    mount_scheduled: bool,
    native_material_active: bool,
}

impl StartupSplashView {
    fn new(_: &mut Context<Self>) -> Self {
        let config = splitlane_config::loader::load_config();
        Self {
            mount_scheduled: false,
            native_material_active: config.cockpit_chrome_material_enabled()
                || config.windows_terminal_material_enabled(),
        }
    }
}

fn native_backdrop_material_active(
    panes_visible: bool,
    settings_open: bool,
    terminal_material_active: bool,
    chrome_material_active: bool,
) -> bool {
    chrome_material_active || (!settings_open && panes_visible && terminal_material_active)
}

fn should_load_login_shell_env_for_startup(
    is_msi_relay: bool,
    is_mcp_subcommand: bool,
    is_cli_subcommand: bool,
    is_hooks_subcommand: bool,
    is_update_and_exit: bool,
    is_unknown_verb: bool,
) -> bool {
    !(is_msi_relay
        || is_mcp_subcommand
        || is_cli_subcommand
        || is_hooks_subcommand
        || is_update_and_exit
        || is_unknown_verb)
}

fn should_extract_mcp_bridge_for_cli(args: &[String]) -> bool {
    args.get(1).map(String::as_str) == Some("mcp")
        && args.get(2).map(String::as_str) == Some("install")
        && args.len() == 3
}

#[cfg(test)]
mod native_material_tests {
    use super::{
        native_backdrop_material_active, should_extract_mcp_bridge_for_cli,
        should_load_login_shell_env_for_startup,
    };
    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn terminal_material_can_activate_backdrop_without_chrome_material() {
        assert!(native_backdrop_material_active(true, false, true, false));
    }

    #[test]
    fn terminal_material_only_applies_to_a_visible_terminal() {
        // Settings over the panes, and any other surface, both suppress it.
        assert!(!native_backdrop_material_active(true, true, true, false));
        assert!(!native_backdrop_material_active(false, false, true, false));
    }

    #[test]
    fn chrome_material_activates_backdrop_independently() {
        assert!(native_backdrop_material_active(false, true, false, true));
    }

    #[test]
    fn login_shell_env_capture_only_runs_for_gui_launches() {
        assert!(should_load_login_shell_env_for_startup(
            false, false, false, false, false, false
        ));
        assert!(!should_load_login_shell_env_for_startup(
            false, true, false, false, false, false
        ));
        assert!(!should_load_login_shell_env_for_startup(
            false, false, true, false, false, false
        ));
        assert!(!should_load_login_shell_env_for_startup(
            false, false, false, true, false, false
        ));
        assert!(!should_load_login_shell_env_for_startup(
            false, false, false, false, true, false
        ));
        assert!(!should_load_login_shell_env_for_startup(
            false, false, false, false, false, true
        ));
    }

    #[test]
    fn mcp_bridge_extraction_only_runs_for_exact_install_command() {
        assert!(should_extract_mcp_bridge_for_cli(&args(&[
            "splitlane",
            "mcp",
            "install"
        ])));
        assert!(!should_extract_mcp_bridge_for_cli(&args(&[
            "splitlane",
            "mcp",
            "status"
        ])));
        assert!(!should_extract_mcp_bridge_for_cli(&args(&[
            "splitlane",
            "mcp",
            "uninstall"
        ])));
        assert!(!should_extract_mcp_bridge_for_cli(&args(&[
            "splitlane",
            "mcp",
            "install",
            "--help"
        ])));
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PanelCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

pub(crate) fn panel_corner_mask(corner: PanelCorner, background: gpui::Hsla) -> impl IntoElement {
    const KAPPA: f32 = 0.552_284_8;

    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let left = bounds.left();
            let right = bounds.right();
            let top = bounds.top();
            let bottom = bounds.bottom();
            let radius = bounds.size.width.min(bounds.size.height);
            let k = radius * KAPPA;

            let mut builder = PathBuilder::fill();
            match corner {
                PanelCorner::TopLeft => {
                    builder.move_to(point(left, top));
                    builder.line_to(point(right, top));
                    builder.cubic_bezier_to(
                        point(left, bottom),
                        point(right - k, top),
                        point(left, bottom - k),
                    );
                    builder.line_to(point(left, top));
                }
                PanelCorner::TopRight => {
                    builder.move_to(point(left, top));
                    builder.line_to(point(right, top));
                    builder.line_to(point(right, bottom));
                    builder.cubic_bezier_to(
                        point(left, top),
                        point(right, bottom - k),
                        point(left + k, top),
                    );
                }
                PanelCorner::BottomLeft => {
                    builder.move_to(point(left, bottom));
                    builder.line_to(point(right, bottom));
                    builder.cubic_bezier_to(
                        point(left, top),
                        point(right - k, bottom),
                        point(left, top + k),
                    );
                    builder.line_to(point(left, bottom));
                }
                PanelCorner::BottomRight => {
                    builder.move_to(point(left, bottom));
                    builder.line_to(point(right, bottom));
                    builder.line_to(point(right, top));
                    builder.cubic_bezier_to(
                        point(left, bottom),
                        point(right, top + k),
                        point(left + k, bottom),
                    );
                }
            }
            builder.close();

            if let Ok(path) = builder.build() {
                window.paint_path(path, background);
            }
        },
    )
    .size_full()
}

/// Paints the opaque window shell around the one inset card that is allowed to
/// reveal native material. Windows DWM backdrops and macOS AppKit effect views
/// span the host window, so the card is isolated by covering every pixel
/// outside its rounded contour.
fn sidebar_card_backdrop_mask(
    sidebar_width: f32,
    card_horizontal_inset: f32,
    card_width: f32,
    card_vertical_inset: f32,
    title_bar_height: Pixels,
    background: gpui::Hsla,
    preserve_terminal_material: bool,
) -> impl IntoElement {
    let card_right = card_horizontal_inset + card_width;
    let sidebar_right_gap = (sidebar_width - card_right).max(0.);

    div()
        .absolute()
        .left_0()
        .right_0()
        .top_0()
        .bottom_0()
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(card_horizontal_inset))
                .bg(background),
        )
        .when(card_width > 0., |mask| {
            mask.child(
                div()
                    .absolute()
                    .left(px(card_horizontal_inset))
                    .top_0()
                    .w(px(card_width))
                    .h(px(card_vertical_inset))
                    .bg(background),
            )
            .child(
                div()
                    .absolute()
                    .left(px(card_horizontal_inset))
                    .bottom_0()
                    .w(px(card_width))
                    .h(px(card_vertical_inset))
                    .bg(background),
            )
            .child(
                div()
                    .absolute()
                    .left(px(card_horizontal_inset))
                    .top(px(card_vertical_inset))
                    .bottom(px(card_vertical_inset))
                    .w(px(card_width))
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(PanelCorner::TopLeft, background)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .right_0()
                                    .top_0()
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(PanelCorner::TopRight, background)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .bottom_0()
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(PanelCorner::BottomLeft, background)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .right_0()
                                    .bottom_0()
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(PanelCorner::BottomRight, background)),
                            ),
                    ),
            )
        })
        .when(preserve_terminal_material, |mask| {
            mask.child(
                div()
                    .absolute()
                    .left(px(card_right))
                    .top_0()
                    .right_0()
                    .h(title_bar_height)
                    .bg(background),
            )
            .child(
                div()
                    .absolute()
                    .left(px(card_right))
                    .top_0()
                    .bottom_0()
                    .w(px(sidebar_right_gap))
                    .bg(background),
            )
        })
        .when(!preserve_terminal_material, |mask| {
            mask.child(
                div()
                    .absolute()
                    .left(px(card_right))
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .bg(background),
            )
        })
}

fn startup_splash_letter(
    label: &'static str,
    index: usize,
    base_color: gpui::Hsla,
) -> gpui::AnyElement {
    div()
        // ui-token-exempt: a wordmark, not UI text. The design's type scale
        // tops out at 19 because nothing in the *interface* is bigger than an
        // empty state's headline; putting a logo on it would shrink the logo
        // to fit a rule written for labels.
        .text_size(px(34.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(base_color)
        .child(label)
        .with_animation(
            SharedString::from(format!("startup-splash-shimmer-letter-{index}")),
            Animation::new(std::time::Duration::from_millis(STARTUP_SPLASH_SHIMMER_MS)).repeat(),
            move |letter, delta| {
                let color = startup_splash_shimmer_color(base_color, index, delta);
                letter.text_color(color)
            },
        )
        .into_any_element()
}

fn startup_splash_shimmer_color(base_color: gpui::Hsla, index: usize, delta: f32) -> gpui::Hsla {
    let active_delta = if delta < 0.78 {
        delta / 0.78
    } else {
        return base_color;
    };
    let center = -1.8 + active_delta * (STARTUP_SPLASH_LETTER_COUNT + 3.6);
    let distance = (index as f32 - center).abs();
    let sigma = 0.86;
    let strength = (-(distance * distance) / (2. * sigma * sigma)).exp();
    let lightness = (base_color.l + (1. - base_color.l) * strength * 0.86).min(0.97);
    let saturation = base_color.s * (1. - strength * 0.85).max(0.);
    let alpha = base_color.a + (STARTUP_SPLASH_SHIMMER_ALPHA - base_color.a) * strength;

    gpui::hsla(base_color.h, saturation, lightness, alpha)
}

impl Render for StartupSplashView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.mount_scheduled {
            self.mount_scheduled = true;
            cx.spawn_in(window, async move |_, cx| {
                smol::Timer::after(std::time::Duration::from_millis(
                    STARTUP_SPLASH_MIN_VISIBLE_MS,
                ))
                .await;
                let _ = cx.update(|window, cx| {
                    mount_splitlane_app(window, cx);
                });
            })
            .detach();
        }

        let ui = crate::theme::ui_colors();
        let splash_text_color = gpui::Hsla {
            a: STARTUP_SPLASH_TEXT_ALPHA,
            ..ui.muted
        };
        let is_window_active = window.is_window_active();
        let background = crate::app::constants::cockpit_backdrop_background(
            ui.chrome_for(is_window_active),
            self.native_material_active,
        );
        let content = div()
            .font_family(crate::ui_tokens::font::UI)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .relative()
                    // No fixed width. It used to be 198px, measured against a
                    // word that no longer exists - and a wordmark's width is
                    // not a number this file can know, because the glyphs come
                    // from whatever the platform resolved for the UI font.
                    // The row sizes to its own letters and the parent centres
                    // it, which is correct for any name.
                    .h(px(58.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(
                        STARTUP_SPLASH_TEXT
                            .iter()
                            .enumerate()
                            .map(|(index, label)| {
                                startup_splash_letter(label, index, splash_text_color)
                            }),
                    ),
            );
        crate::window_chrome::csd::client_side_window_shell(content, window, background, ui.border)
    }
}

impl SidebarWidthAnimation {
    fn width_at(self, now: std::time::Instant) -> f32 {
        let duration = std::time::Duration::from_millis(PRIMARY_SIDEBAR_ANIMATION_MS);
        let progress = (now.duration_since(self.started_at).as_secs_f32() / duration.as_secs_f32())
            .clamp(0., 1.);
        let eased = 1. - (1. - progress).powi(3);
        self.from_width + (self.to_width - self.from_width) * eased
    }

    fn is_finished(self, now: std::time::Instant) -> bool {
        now.duration_since(self.started_at)
            >= std::time::Duration::from_millis(PRIMARY_SIDEBAR_ANIMATION_MS)
    }
}

/// Docked agent-sessions sidebar state (visibility, per-agent
/// scanned session lists, the originating pane/cwd, and group UI flags),
/// extracted from the `SplitlaneApp` god-struct.
struct AgentSessionsState {
    /// Whether the docked agent-sessions right sidebar is visible
    /// Toggled by the
    /// tab-bar sessions button; the sidebar renders as a layout child of the
    /// root row, not a `deferred()` overlay.
    sessions_sidebar_open: bool,
    /// Width animation for opening/closing the docked right sidebar. Reuses
    /// the same duration and easing as the primary left sidebar.
    sessions_sidebar_animation: Option<SidebarWidthAnimation>,
    /// Cwd-scoped sessions per supported CLI, indexed by
    /// [`agent_sessions::SessionAgent::index`]. Filled asynchronously by
    /// per-agent background scans.
    sessions_by_agent: [Vec<agent_sessions::SessionMeta>; agent_sessions::SESSION_AGENT_COUNT],
    /// Per-agent count of older sessions omitted by the sidebar memory
    /// cap, indexed by `agent_index()`.
    sessions_omitted: [usize; agent_sessions::SESSION_AGENT_COUNT],
    /// Working directory the sidebar was opened for. Used to filter stale
    /// scan results and as the compact wayfinding label inside the sidebar
    /// header.
    sessions_cwd: Option<String>,
    /// Surface id of the terminal whose tab-bar button opened the sidebar.
    /// Resume commands are sent back only if that exact terminal still exists.
    sessions_surface_id: Option<u64>,
    /// Project the sidebar was opened for in Agents mode. `Some` switches the
    /// resume action from "inject a resume command into the bound pane"
    /// (CLI mode, which needs a pane to already exist) to "open the session as
    /// a thread in this project" - the Agents-mode flow, where there are
    /// threads and no panes. Mutually exclusive with
    /// [`Self::sessions_surface_id`]: whichever open path ran last clears the
    /// other.
    sessions_project_idx: Option<usize>,
    /// Scroll state for the sessions list. Re-created on every open so a fresh
    /// sidebar starts at offset 0.
    sessions_scroll: gpui::ScrollHandle,
    /// Incremented on every open/retarget/close. Async scans must carry the
    /// generation they were spawned under so a stale result for the same cwd
    /// cannot overwrite a newer open.
    sessions_scan_generation: u64,
    /// Keyboard-selected visible session row. The index is over visible rows
    /// only, not group headers or empty/loading states.
    sessions_selected: usize,
    /// Search field filtering the session rows by title. A real single-line
    /// `TextInput`, same pattern as the Agents sidebar and Settings nav.
    sessions_filter_input: Entity<crate::widgets::text_input::TextInput>,
    /// Lowercased mirror of `sessions_filter_input`, refreshed on every
    /// keystroke by the observer in `bootstrap`. Kept as plain state because
    /// the row/navigation helpers take `&self` with no `App` to read the
    /// entity through, and re-lowercasing per row would burn one allocation
    /// per session per frame.
    sessions_query: String,
    /// Focus target for keyboard navigation inside the docked sidebar.
    sessions_focus: FocusHandle,
    /// Per-agent sidebar group state, indexed by
    /// [`agent_sessions::SessionAgent::index`]. All reset on close/open.
    /// `collapsed`: the group's caret has hidden its rows.
    sessions_group_collapsed: [bool; agent_sessions::SESSION_AGENT_COUNT],
    /// `show_all`: the group is past its 5-row cap via "Show more".
    sessions_group_show_all: [bool; agent_sessions::SESSION_AGENT_COUNT],
    /// `scanning`: a background scan for this agent is in flight, so an empty
    /// list should read as "loading" not "none".
    sessions_scanning: [bool; agent_sessions::SESSION_AGENT_COUNT],
}

/// Git Diff mode state (mounted single/multi-repo views + their
/// caches, the worktree/scope/project pickers, and the file-tree filter),
/// extracted from the `SplitlaneApp` god-struct.
struct DiffModeState {
    /// The mounted Git Diff mode
    /// view, when `mode == AppMode::Diff`. Lazily (re)built by
    /// `rebuild_diff_view` on mode entry and on workspace switch;
    /// `None` when no git repo backs the active workspace. Dropping it
    /// releases the DiffView's filesystem watchers.
    diff_view: Option<gpui::Entity<crate::diff::DiffView>>,
    /// The Multi-project host,
    /// mounted when `diff_scope == MultiProject`. Separate from
    /// `diff_view` (the single-repo host for Project / Worktree).
    multi_diff_view: Option<gpui::Entity<crate::diff::MultiRepoDiffView>>,
    /// Warm-resume cache of mounted single-repo `DiffView` entities
    /// (Project / Worktree scopes), keyed by repo + scope + worktree set. A
    /// CLI↔Diff toggle (or a workspace switch back to a visited repo) reuses
    /// the cached entity instead of cold-rebuilding it, so the diff shows in
    /// one frame with its computed rows instead of flashing "Computing diff…".
    /// Non-displayed entries are suspended (watchers released), so at
    /// most one diff entity ever holds live watchers. Mirrors the
    /// `agents_terminal_view_cache` pointer/owner split; bounded by
    /// `DIFF_VIEW_CACHE_CAP` and pruned to open repos on workspace close.
    diff_view_cache: std::collections::HashMap<
        crate::app::diff_view_actions::DiffViewKey,
        gpui::Entity<crate::diff::DiffView>,
    >,
    /// The cache key the current `diff_view` pointer is bound to (which
    /// cache entry it clones). `None` outside Diff mode, in Multi-project scope,
    /// or when no git repo backs the active workspace.
    diff_view_key: Option<crate::app::diff_view_actions::DiffViewKey>,
    /// Retained Multi-project host + the signature of the repo-group set
    /// it was built for. Reused across CLI↔Diff toggles while the open project
    /// set is unchanged; rebuilt when projects open/close. `multi_diff_view` is
    /// the display pointer into this slot.
    multi_diff_view_retained: Option<(u64, gpui::Entity<crate::diff::MultiRepoDiffView>)>,
    /// Diff sidebar: branch sections (keyed by branch name) the user has
    /// collapsed in the multi-branch changed-files panel. Ephemeral UI state
    /// (resets on remount), so a `HashSet` of names is enough.
    diff_collapsed_branches: std::collections::HashSet<String>,
    /// `true` while the Worktree-scope on-disk worktree discovery
    /// (`spawn_worktree_discovery`) is in flight, so the diff sidebar can show a
    /// "Discovering worktrees…" note instead of looking like columns are missing
    /// during the brief cold-mount window.
    diff_discovering: bool,
    /// Repo root for the active worktree-discovery task. Prevents a stale task
    /// from clearing a newer repo's spinner while still letting its own spinner
    /// clear after the user leaves Worktree scope.
    diff_discovering_root: Option<std::path::PathBuf>,
    /// Worktree-scope branch curation: per repo, the set of worktree paths (raw
    /// path strings) the user explicitly chose to show as columns. NO entry for a
    /// repo ⇒ show ALL its worktrees (the default). An entry ⇒ build columns for
    /// exactly those worktrees, so branches the user didn't pick are never diffed
    /// (not merely hidden). Edited by the branches picker; in-memory per session.
    diff_chosen_worktrees:
        std::collections::HashMap<std::path::PathBuf, std::collections::HashSet<String>>,
    /// Whether the Worktree-scope branches multi-select popover is open.
    diff_worktree_picker_open: bool,
    /// All worktrees of `diff_available_repo`, fetched off-thread for the branches
    /// picker so it can offer branches not currently shown. Populated lazily when
    /// the picker opens.
    diff_available_worktrees: Vec<crate::diff::DiffWorktree>,
    /// The repo [`Self::diff_available_worktrees`] was fetched for (guards against
    /// showing a stale list after a workspace/repo switch).
    diff_available_repo: Option<std::path::PathBuf>,
    /// The active Git Diff view scope (Project / Multi-project /
    /// Worktree). Defaults to Project; `rebuild_diff_view` branches on it.
    diff_scope: crate::diff::DiffScope,
    /// Whether the scope-selector popover is open.
    diff_scope_picker_open: bool,
    /// Whether the project-selector popover (Project / Worktree scopes) is
    /// open. Lets the user pick which open workspace's repo the single-repo
    /// diff follows, without leaving Diff mode.
    diff_project_picker_open: bool,
    /// Path of the file row
    /// selected in the diff git panel (presentation-only until the
    /// scroll-to-file wiring lands). `None` = nothing selected.
    diff_selected_file: Option<String>,
    /// Whether the git panel's "Changes" section is collapsed.
    diff_files_collapsed: bool,
    /// Changed-files panel layout: `false` = flat list (default), `true` =
    /// collapsible directory tree (compact-folder chains merged). Toggled from
    /// the "Changes" header.
    diff_files_tree: bool,
    /// Collapsed directory nodes in tree mode, keyed `col_idx\0<dir path>` so a
    /// directory present in two branch sections collapses independently.
    diff_collapsed_dirs: std::collections::HashSet<String>,
    /// Persistent type-to-filter field for the diff changed-files
    /// panel. Observed at construction so each keystroke re-renders the
    /// sidebar (which recomputes the visible matches by path substring).
    diff_file_filter: gpui::Entity<crate::widgets::text_input::TextInput>,
    /// `⤢`: what the window looked like before the review took it over, or
    /// `None` when it has not. The gesture is a toggle, and a toggle that
    /// cannot say what it undid is a one-way door.
    diff_full_window: Option<DiffFullWindowState>,
}

/// What `⤢` has to put back: the Files panel it closed, and the container whose
/// panes it collapsed to one.
///
/// The design calls `⤢` "the full-window gesture" and says pressing it again
/// restores the previous panes - so the state it displaced is recorded rather
/// than recomputed, which is the only way "previous" can mean the same thing
/// twice.
pub(crate) struct DiffFullWindowState {
    /// Whether the Files panel was open when the gesture ran. The diff has its
    /// own file list, so the panel is closed either way; only reopening it is
    /// conditional.
    files_sidebar_was_open: bool,
    /// The container whose panes were collapsed to the one showing the review,
    /// or `None` when the review was already the whole content area (the
    /// container's parked `Changes` surface).
    zoomed_container: Option<u64>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentsGitState {
    pub(crate) branch: String,
    pub(crate) is_repo: bool,
    pub(crate) stats: crate::workspace::GitDiffStats,
}

/// Agents-view sidebar state extracted from the `SplitlaneApp`
/// god-struct (terminal-only Agents view: rename, context menu, skills
/// page, search filter, and the per-thread terminal cache).
struct AgentsViewState {
    /// Which sidebar row is currently in
    /// inline-rename mode (mirrors [`Self::renaming_idx`] but for the
    /// Agents domain). `None` when no rename is active.
    pub(crate) agents_renaming: Option<crate::app::agents_sidebar::AgentsRenameTarget>,
    /// Inline rename input. `Some` only while a rename is in flight;
    /// dropped on commit / cancel. Mirrors the Composer's TextArea
    /// pattern so users get a real text input (cursor, selection,
    /// IME, copy/paste, click-to-position, double-click word select)
    /// instead of a fake `{text}|` shimmer. One entity is enough
    /// because [`Self::agents_renaming`] enforces a single in-flight
    /// rename at a time.
    pub(crate) agents_rename_input: Option<gpui::Entity<crate::widgets::text_area::TextArea>>,
    /// The in-progress rename text. Empty when not renaming.
    pub(crate) agents_rename_text: String,
    /// Open right-click context menu (project header or
    /// thread row). `None` when no menu is open.
    pub(crate) agents_menu_open: Option<crate::app::agents_sidebar::AgentsContextMenu>,
    /// Pending delete confirmation. The actual mutation
    /// happens only after the user confirms in the dialog. Still used by the
    /// context-menu "Delete" path; the hover-trash path uses
    pub(crate) agents_confirm_delete: Option<crate::app::agents_sidebar::AgentsDeleteTarget>,
    /// Inline delete-confirm (ergonomics): the row whose trash icon was just
    /// clicked. While `Some`, that row's action cluster shows a red "Delete"
    /// button (click-to-confirm) instead of opening the confirmation dialog.
    /// Cleared on confirm, on selecting/clicking a row, or on opening a menu.
    /// Active tab on the Skills page. Persists across re-opens of
    /// the Skills view within the session; resets on app restart.
    pub(crate) agents_skills_tab: crate::agents_view::SkillsTab,
    /// Cached skills snapshot. Discovery runs off the GPUI render path and
    /// refreshes this vector when the page opens or the user clicks Refresh.
    pub(crate) agents_skills: Vec<crate::agents_view::SkillEntry>,
    /// True while the background skills discovery is running.
    pub(crate) agents_skills_loading: bool,
    /// Stable id of the skill whose Copy button was just clicked. The card
    /// flips its label to "Copied" while this matches; a timer reverts it.
    pub(crate) agents_skills_copied: Option<String>,
    /// True while the bottom-of-sidebar "Settings" popover is open.
    /// Shared between CLI and Agents sidebars - only one popover is
    /// ever visible because only one sidebar is rendered at a time.
    pub(crate) sidebar_actions_menu_open: bool,
    /// Whether the compact interface picker above the sidebar footer is open.
    pub(crate) sidebar_mode_picker_open: bool,
    /// Open branch selector for the Agents environment card. The menu is
    /// scoped to a cwd because a container's surfaces can point at different
    /// repositories.
    pub(crate) agents_branch_menu: Option<AgentsBranchMenuState>,
    /// Last git metadata refresh by cwd, so the branch picker does not depend
    /// on a matching workspace cache.
    pub(crate) agents_environment_git: std::collections::HashMap<String, AgentsGitState>,
    /// Cache of every Terminal Thread surface mounted this session,
    /// keyed by [`crate::project::Thread::id`]. The Agents view is
    /// terminal-only: selecting a thread reuses the existing
    /// [`crate::terminal::view::TerminalView`] entity so the shell
    /// process, scrollback, and I/O threads survive the round trip.
    /// Drop happens on thread deletion (via `remove_thread`'s cache
    /// cleanup) or on app shutdown.
    pub(crate) agents_terminal_view_cache:
        std::collections::HashMap<u64, gpui::Entity<crate::terminal::view::TerminalView>>,
    /// LRU order for [`Self::agents_terminal_view_cache`], oldest first.
    pub(crate) agents_terminal_cache_lru: Vec<u64>,
    /// Last access timestamp for cached agent terminals. Expired entries are
    /// pruned opportunistically when the cache is touched.
    pub(crate) agents_terminal_cache_touched_at: std::collections::HashMap<u64, std::time::Instant>,
}

#[derive(Clone)]
pub(crate) struct AgentsBranchMenuState {
    pub(crate) cwd: String,
    pub(crate) current: String,
    pub(crate) branches: Vec<String>,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    /// Codex branch picker search field. A real `TextInput` keeps cursor,
    /// selection, paste and IME behavior consistent with the sidebar filter.
    pub(crate) query_input: gpui::Entity<crate::widgets::text_input::TextInput>,
    /// Where the picker opens. The branch belongs to the container, so the
    /// picker is opened from whatever affordance the container owns and
    /// anchors at that click rather than inside one particular panel.
    pub(crate) anchor: gpui::Point<gpui::Pixels>,
}

struct SplitlaneApp {
    workspaces: Vec<Workspace>,
    active_idx: usize,
    renaming_idx: Option<usize>,
    rename_text: String,
    /// Shared slot for config changes from the background `ConfigWatcher` thread.
    /// The watcher writes `Some(config)` on every successful reload; the main
    /// thread `take()`s it in the 50ms poll loop to apply keybindings + theme.
    pending_config:
        std::sync::Arc<std::sync::Mutex<Option<splitlane_config::schema::SplitlaneConfig>>>,
    /// Monotonic save-coalescing token. Every `save_session` bumps it
    /// and the off-thread writer skips its disk write when a newer save has
    /// been scheduled meanwhile, collapsing a burst (e.g. closing 20
    /// workspaces) into a single write - none of it on the render thread.
    save_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Parsed `splitlane.json` cached on the main thread so render paths
    /// never call the blocking `load_config()` (fs read + JSON parse) per frame.
    /// Hydrated at startup, invalidated in [`Self::process_config_changes`] when
    /// the background `ConfigWatcher` reports a reload. Render code reads this;
    /// click handlers that must observe a config write *they just made* still
    /// read fresh from disk (the cache lags the write by the watcher debounce).
    cached_config: splitlane_config::schema::SplitlaneConfig,
    ipc_rx: std::sync::mpsc::Receiver<ipc::IpcRequest>,
    ipc_status: ipc::IpcStatus,
    /// Outbound event bus shared with the IPC
    /// server. `broadcast` is called from the render thread (non-blocking).
    event_bus: std::sync::Arc<ipc_events::EventBus>,
    /// Last `output_generation` broadcast per surface, so the
    /// 50 ms sweep emits `surface_changed` only on an actual change (debounce).
    last_broadcast_gen: std::collections::HashMap<u64, u64>,
    title_bar: Entity<title_bar::TitleBar>,
    /// Visibility of the primary left rail shared by CLI, Agents, and Diff.
    /// Ephemeral by design: each launch starts with navigation visible.
    primary_sidebar_visible: bool,
    /// Transient width interpolation for the primary rail. The boolean above is
    /// the target state; this keeps the rail mounted while its layout width
    /// eases open or closed.
    primary_sidebar_animation: Option<SidebarWidthAnimation>,
    /// File watcher for `.git/HEAD` and `.git/index` across all workspaces.
    /// `None` if the OS watcher could not be created (graceful degradation).
    git_watcher: Option<notify::RecommendedWatcher>,
    /// Receiver for raw notify events from the git file watcher.
    git_event_rx: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    /// Refcount for watched `.git` directories (multiple workspaces may share a repo).
    git_watch_counts: std::collections::HashMap<std::path::PathBuf, usize>,
    /// Active settings section, or `None` if settings is closed.
    settings_section: Option<SettingsSection>,
    /// Scroll state for the inline settings page.
    settings_scroll: gpui::ScrollHandle,
    settings_drag: Option<crate::widgets::scrollbar::ScrollDragState>,
    /// Codex settings nav search box (filters the section list). A real
    /// single-line `TextInput`, observed so each keystroke re-renders the nav.
    settings_search_input: gpui::Entity<crate::widgets::text_input::TextInput>,
    /// The Keyboard Shortcuts page filter. Same `TextInput` + `filter_pill`
    /// recipe as the nav search above; read at render and handed to
    /// `keybindings::group_shortcuts`.
    shortcuts_filter_input: gpui::Entity<crate::widgets::text_input::TextInput>,
    /// Codex settings: which Terminal-page dropdown is open (`None` = closed).
    terminal_dropdown: Option<TerminalDropdown>,
    /// Codex settings: which General-page select is open (`None` = closed).
    general_dropdown: Option<GeneralDropdown>,
    /// Settings -> Appearance: is the theme row's menu open?
    appearance_theme_menu_open: bool,
    /// The presets folder, as last read. One cached copy, three readers: the
    /// Launch pad, the Settings section that lists them, and the project
    /// row's "Run preset". Refreshed at launch and after every save or
    /// delete - never read from disk during a frame.
    presets: Vec<crate::preset::store::StoredPreset>,
    /// Codex settings: cached MCP-bridge status snapshot, refreshed off-thread
    /// so the MCP page never does config I/O during a frame.
    mcp_status: Option<Vec<splitlane_mcp_install::StatusReport>>,
    /// Codex settings: result of the last MCP-bridge install (per-agent recap,
    /// or a wholesale refusal message).
    mcp_install: Option<Result<Vec<splitlane_mcp_install::InstallReport>, String>>,
    /// Codex settings: an MCP-bridge install is running.
    mcp_busy: bool,
    /// Cached HOME directory for sidebar display (avoids per-render syscall).
    home_dir: String,
    /// Scroll state for the persistent sidebar workspace list.
    /// Driven by GPUI's `overflow_y_scroll + track_scroll`; the
    /// visible scroll bar has been removed but the handle is still
    /// useful so the list keeps a stable wheel-scroll offset across
    /// re-renders.
    sidebar_scroll: gpui::ScrollHandle,
    /// Effective keybindings (defaults merged with user overrides) for settings display.
    effective_shortcuts: Vec<keybindings::ShortcutEntry>,
    /// Index of the shortcut row currently being recorded (`None` = not recording).
    recording_shortcut_idx: Option<usize>,
    /// Focus handle for the settings page (receives key events during recording/font search).
    settings_focus: FocusHandle,
    /// Cached list of monospace font family names from the system.
    mono_font_names: Vec<String>,
    /// Whether the font family dropdown is open.
    font_dropdown_open: bool,
    /// Filter text for the font dropdown.
    font_search: String,
    /// Selected segment on the Themes page (Light/Dark/System). UI state for
    /// now - highlights the active segment, ready to drive theme resolution
    /// once the light theme lands.
    theme_mode: ThemeMode,
    /// Workflow action menu currently open in the sidebar (`None` = closed).
    workspace_menu_open: Option<WorkspaceContextMenu>,
    /// "Move to pane…" tab context menu, or `None` when closed.
    tab_menu_open: Option<TabContextMenu>,
    /// Pane to focus on the next render. Set by the
    /// `DropSplit` handler - which runs in a subscription callback without a
    /// `Window` - and consumed in `render`, which has one. One-shot.
    pending_pane_focus: Option<Entity<Pane>>,
    /// Same one-shot channel for a surface that is not a pane (the diff).
    /// Selecting a surface from the rail has to hand it the keyboard, or every
    /// global chord - the palette above all - goes nowhere until the user
    /// clicks into the content.
    pending_focus: Option<gpui::FocusHandle>,
    /// The waiting chip was clicked. Same one-shot channel and the same
    /// reason: the title bar's event handler is a plain `cx.subscribe` with
    /// no `Window`, and the queue the chip opens takes the keyboard, which
    /// needs one.
    pending_waiting_chip: bool,
    /// The status bar's port chip is showing its list.
    ///
    /// Only reachable with more than one dev server running: with one the chip
    /// opens the browser and has no list to show.
    ports_popover_open: bool,
    /// What the last session restore brought back, for the status bar's
    /// "restored 09:41 · 2 sessions". `None` on a first run, where nothing was
    /// restored and the indicator says nothing rather than "restored 1".
    restore_summary: Option<crate::app::status_bar::RestoreSummary>,
    /// Agent-sessions sidebar state (see `AgentSessionsState`).
    agent_sessions: AgentSessionsState,
    /// Whether the docked Files right sidebar is visible. Mutually exclusive with
    /// `sessions_sidebar_open`. Never persisted - always `false` on launch.
    files_sidebar_open: bool,
    /// Width animation for opening/closing the docked Files right sidebar.
    /// Matches the agent-sessions sidebar animation.
    files_sidebar_animation: Option<SidebarWidthAnimation>,
    /// In-memory tree state for the open Files sidebar (root + expanded set +
    /// lazily-cached directory listings). Empty when the sidebar is closed.
    files_tree: app::files_tree::FilesTreeState,
    /// Scroll state for the Files tree body. Re-created on every open so a
    /// fresh sidebar starts at offset 0.
    files_tree_scroll: gpui::ScrollHandle,
    /// Keyboard-selected visible Files row. The index is over visible rows only.
    files_selected: usize,
    /// Focus target for keyboard navigation inside the docked Files sidebar.
    files_focus: FocusHandle,
    /// Somewhere for the keyboard to be when a container has no panes.
    ///
    /// GPUI dispatches an action along the focus chain, so a window with
    /// nothing focused has no path for one to travel: with the panes gone,
    /// every chord and every toolbar button - which dispatches the same way -
    /// silently did nothing. Reported as "the Files button does nothing";
    /// `⌘K` did nothing either, and so did the rest.
    empty_panes_focus: FocusHandle,
    /// Recursive `notify` watcher on the Files tree root.
    /// `None` when the sidebar is closed or the watch could not be installed
    /// (graceful degradation - the tree then refreshes on expand).
    files_watcher: Option<notify::RecommendedWatcher>,
    /// Receiver for raw watch events, drained + debounced by the background
    /// loop in `bootstrap`. `Some` only while a watcher is installed.
    files_event_rx: Option<std::sync::mpsc::Receiver<notify::Result<notify::Event>>>,
    /// Open right-click context menu for a Files-sidebar row,
    /// or `None` when closed. Mutually exclusive with the other popovers.
    files_menu_open: Option<FilesContextMenu>,
    /// The design's `M` / `A` marks: one entry per path `git status` reports as
    /// changed, keyed by absolute path. Read off the render thread and refreshed
    /// from the two watchers the app already runs - the tree's own (a file was
    /// written) and the `.git` one (`HEAD` or `index` moved) - rather than from
    /// a timer of its own. Empty while the Files panel is closed and for a
    /// container that is not a repository.
    files_git_marks: std::collections::HashMap<std::path::PathBuf, crate::workspace::GitMark>,
    /// Generation counter for the marks read above, so a slow read for an old
    /// root can never overwrite a newer one's result.
    files_git_marks_generation: u64,
    /// Ephemeral bottom-right toast.
    toast: Option<Toast>,
    /// Pending toasts waiting for the active one to finish. Runtime bursts
    /// should not overwrite user-visible messages.
    toast_queue: std::collections::VecDeque<Toast>,
    /// Dismiss timer for the active toast - dropped on new toast to cancel the old timer.
    _toast_task: Option<gpui::Task<()>>,
    /// Last light/dark value applied to the Windows native backdrop.
    #[cfg(target_os = "windows")]
    windows_backdrop_light: Option<bool>,
    /// The surface last visited by
    /// `JumpNextWaiting`, so repeated presses cycle through the waiting
    /// agents instead of bouncing on the first one.
    jump_cursor: Option<u64>,
    /// The pane that holds focus now, and the one that held it before -
    /// the two halves of the design's "Previous pane" (`\u{2303}\u{21e5}`).
    ///
    /// Kept as weak handles and refreshed in `render`, because focus reaches a
    /// pane through GPUI's focus chain from a dozen call sites (a click, a
    /// split, a rail row, a jump, a restore) and no one of them owns the
    /// question "what was focused a moment ago". A closed pane's handle simply
    /// fails to upgrade, which is the right answer for a return chord.
    focused_pane_now: Option<gpui::WeakEntity<crate::pane::Pane>>,
    focused_pane_before: Option<gpui::WeakEntity<crate::pane::Pane>>,
    /// The panes area's own size in points, captured each frame by a canvas
    /// laid over it.
    ///
    /// The targeting ladder's third rung asks whether one more pane still
    /// leaves every pane at least 320px on the long axis, and it is asked from
    /// a rail click and a menu item - neither of which has a `Window`, and
    /// neither of which could subtract the rail, the Files panel and the
    /// insets from the viewport without re-deriving the whole layout. The
    /// canvas measures the thing itself.
    pub(crate) panes_area: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    /// The parked surface currently being dragged out of the rail, if any.
    ///
    /// The drop strip for a third pane has to appear only while such a drag is
    /// in flight, and GPUI exposes `has_active_drag()` (any drag at all) but
    /// not the type of the payload. So the rail's `on_drag` records it here
    /// and `render` clears it as soon as GPUI reports no drag - the mouse-up
    /// that ends a drag is GPUI's to observe, not ours.
    pub(crate) dragging_rail_surface: Option<crate::pane_drag::RailSurfaceDrag>,
    /// How wide the Files tree is, in pixels. The user's, dragged from its
    /// left edge and persisted with the session - which is why it is a field
    /// and not the constant it starts at, exactly as `rail_width` is.
    files_width: f32,
    /// Live drag of that edge: the pointer x where the press landed and the
    /// width at that moment. `None` when no drag is in flight.
    files_resize_drag: Option<(f32, f32)>,
    /// A fleet-search request from a find bar, waiting for the `Window` that
    /// focusing the palette's field needs. Consumed in `render`.
    pending_palette_output: Option<String>,
    /// A rail surface dropped onto a slot, waiting for the `Window` that
    /// mounting its PTY and moving focus needs. Consumed in `render`, like
    /// `pending_pane_focus`: a drop handler has no window.
    pending_rail_surface_drop: Option<(usize, usize, Entity<crate::pane::Pane>)>,
    /// Source pane for swap mode, or `None` if not in swap mode.
    swap_source: Option<Entity<crate::pane::Pane>>,
    /// LIFO stack of recently closed panes for undo-close.
    closed_panes: Vec<ClosedPaneRecord>,
    /// Whether the "About Splitlane" dialog is visible.
    show_about_dialog: bool,
    /// Whether the command-palette-style theme picker is visible.
    show_theme_picker: bool,
    /// Typeahead filter for the theme picker (case-insensitive substring).
    theme_picker_query: String,
    /// Index into the *filtered* theme list for the currently highlighted row.
    theme_picker_selected_idx: usize,
    /// Focus handle routing key events to the theme picker while it's open.
    theme_picker_focus: FocusHandle,
    /// Scroll state for the theme picker list (visible scrollbar overlay).
    theme_picker_scroll: gpui::ScrollHandle,
    theme_picker_drag: Option<crate::widgets::scrollbar::ScrollDragState>,
    /// Live Composer session, `None` =
    /// closed. The target pane renders the pushed slot snapshot.
    composer: Option<app::composer::ComposerState>,
    /// Broadcast groups + active index +
    /// per-terminal queued-prompt buffers. Volatile by design (v1).
    broadcast: app::broadcast::BroadcastState,
    /// Broadcast-group picker modal (theme-picker scaffold): visibility,
    /// name-input buffer (create/rename), keyboard cursor, in-place rename
    /// target, inline validation error, and the key-routing focus handle.
    broadcast_picker_open: bool,
    broadcast_picker_query: String,
    broadcast_picker_selected: usize,
    broadcast_picker_renaming: Option<usize>,
    broadcast_picker_error: Option<String>,
    broadcast_picker_focus: FocusHandle,
    /// Attention Queue overlay - visibility,
    /// keyboard cursor, key-routing focus handle. Rows are derived live
    /// from `agent_sessions` on every render, never stored.
    attention_queue_open: bool,
    attention_queue_selected: usize,
    attention_queue_focus: FocusHandle,
    /// Leaving the app while an agent is mid-turn asks first
    /// (`app::exit_guard`). `Some` while the card is up.
    pending_exit: Option<crate::app::exit_guard::ExitIntent>,
    exit_confirm_focus: FocusHandle,
    // The fleet-grep OVERLAY used to be four fields here. The
    // design dissolves fleet search into the palette's "Output" tab, so the
    // overlay is gone and its state lives on `command_palette` - one
    // implementation of "search every session", one place to look for it. The
    // per-tab match badges survived the move; they are the only way a match in
    // a session you are not looking at becomes visible.
    /// Keyboard focus for the Agents environment branch picker so its Codex-style
    /// search field captures typing (live filter + new-branch name). Focused on
    /// open; focus returns to the active thread terminal on close.
    /// Launch Pad modal state, `None` = closed.
    launch_pad: Option<app::launch_pad::LaunchPadState>,
    launch_pad_focus: FocusHandle,
    /// The rail's `+ worktree` dialog, open or not.
    worktree_dialog: Option<app::worktree_dialog::WorktreeDialogState>,
    worktree_dialog_focus: FocusHandle,
    /// Command palette state, `None` = closed. Unlike the three cockpit
    /// overlays above it is NOT mode-gated - the rule that every action stays
    /// reachable made it the floor of reachability in Cli, Review and Agents
    /// alike.
    /// How wide the rail is, in pixels. The user's, dragged from its right
    /// edge and persisted with the session - which is why it is a field and
    /// not the constant it starts at.
    rail_width: f32,
    /// Live drag of that edge: the pointer x where the press landed and the
    /// width at that moment. `None` when no drag is in flight.
    rail_resize_drag: Option<(f32, f32)>,
    /// What the app knows about the Claude usage limits the rail's footer
    /// draws: the poller's own bookkeeping, and the newest reply it got.
    claude_limits: app::agents_sidebar::ClaudeLimits,
    /// Codex's own statement about its limits, in the shared shape, or `None`
    /// when this machine has nothing recent to read.
    ///
    /// No bookkeeping beside it, unlike the polled source: there is no login to
    /// expire, no rate limit to respect and no failure worth a word on screen -
    /// the file is either there and recent or it is not.
    codex_limits: Option<app::agents_sidebar::limits_footer::VendorRowState>,
    /// A read of that file is in flight.
    codex_limits_reading: bool,
    /// Agent surfaces with an out-of-turn state re-read in flight
    /// (`app::agent_state_pass::accelerate_agent_state`). One per surface: a
    /// hook frame arriving while its file is already being read has nothing to
    /// add, because that read has not happened yet.
    agent_state_reading: std::collections::HashSet<u64>,
    /// What each agent surface's process subtree looked like when its agent
    /// last finished a turn, which is how a worker is told from the shim, an
    /// npm wrapper, and any stdio MCP server the agent keeps
    /// (`process_tree::WorkerBaseline`). Absent until that surface's first
    /// finished turn, and absent means the worker question answers
    /// `Worker::Unknown` - never "no worker".
    worker_baselines: std::collections::HashMap<u64, process_tree::WorkerBaseline>,
    /// Per-surface PTY-output bookkeeping for the state pass's third source:
    /// the previous pass's `output_generation`, and whether the status showing
    /// now was put there by that source. Freed with its surface, alongside
    /// `worker_baselines`, in the one place that sees every surface at once.
    pty_flow: std::collections::HashMap<u64, app::agent_state_pass::PtyFlow>,
    /// A session id the agent's own status file reported for a surface that is
    /// not the one Splitlane recorded, seen by the **previous** pass and not yet
    /// acted on. See `app::agent_state_pass` for why re-binding a surface waits
    /// for two passes to agree.
    proposed_sessions: std::collections::HashMap<u64, String>,
    /// When the detector first saw each surface enter a run.
    ///
    /// Not a field on `Thread`: nineteen places write `Thread::status`, and a
    /// stamp maintained at each of them is a stamp one of them will forget.
    /// The detector is the only reader, it sees every transition it cares
    /// about, and `run_ended` is the one place that both fills and empties this
    /// - so an entry cannot outlive the run it measures.
    running_since: std::collections::HashMap<u64, std::time::Instant>,
    /// When the detector first read "the turn is over" about a surface it had
    /// showing as working, while that reading is still waiting to be confirmed.
    /// See `app::agent_state_pass::confirm_run_end` for why a run does not end
    /// on one reading.
    run_end_seen_at: std::collections::HashMap<u64, std::time::Instant>,
    /// What the chip and the popover are looking at, measured once a frame by
    /// `refresh_attention_edge`.
    ///
    /// Both window frames draw the chip and the popover reads the same list;
    /// counting it here is one walk of every pane in every container instead of
    /// three, and it is also what makes the edge and the chip incapable of
    /// disagreeing.
    ///
    /// It used to be a **snapshot of the news, taken when the popover opened**,
    /// which was necessary only because opening cleared every mark in the same
    /// breath - a live query would have rendered an empty section every time.
    /// Clearing was later replaced with lowering, so the list is live again and
    /// the snapshot is gone with the thing that forced it.
    activity_counts_cache: crate::app::waiting::ActivityCounts,
    /// Whether the attention queue was empty at the end of the last frame.
    ///
    /// The whole memory the queue's edge needs. It starts `false` on purpose:
    /// the first frame adopts what it finds without announcing, because showing
    /// a window is not an edge.
    attention_queue_was_empty: bool,
    /// Set when the app has stopped watching the queue and must adopt whatever
    /// it finds on the next frame instead of reading it as an edge.
    ///
    /// Raised on window activation: coming back is not an edge, and while the
    /// window was away nothing painted, so the latch beside this is older than
    /// the queue it would be compared against.
    attention_edge_needs_rebaseline: bool,
    /// When the frame loop last looked at the attention queue.
    ///
    /// The gap between two of these is what says whether the app was on screen
    /// in between; see `refresh_attention_edge`.
    attention_edge_last_seen: Option<std::time::Instant>,
    /// The announcement in flight, if any. See `app/announce.rs`.
    announcement: Option<crate::app::announce::Announcement>,
    /// Bumped once per edge, and the reason a pulse is bound to a transition
    /// rather than to an element appearing: it keys the animation's id.
    announce_generation: u64,
    /// The "Send to agent" overlay's draft, kept across a close - **and the
    /// pane it was written to**.
    ///
    /// Esc closes it and keeps the text, and so does a click
    /// outside - a draft that survived one exit and not the other would be
    /// worse than one that never survived. Cleared once the message is sent.
    ///
    /// Keyed by pane, because this is the one place in the app where the user
    /// writes to a session without looking at it: an unkeyed draft would
    /// pre-fill a message written for one agent into an overlay addressed at
    /// another, and the only thing standing between that and a send is one
    /// Enter. Found by a cross-vendor review.
    composer_draft: Option<(gpui::EntityId, String)>,
    command_palette: Option<app::command_palette::CommandPaletteState>,
    /// The launcher's third part - this container's sessions on disk - read
    /// off the render thread while a launcher is on screen and dropped when
    /// the last one closes, so the next one reads the directory again rather
    /// than showing what was there an hour ago. `Some(vec![])` while the read
    /// is out, which is also what "no stored sessions" looks like: an empty
    /// group is drawn as no group at all, so a late arrival needs no spinner.
    launcher_history: Option<Vec<app::command_palette::HistoryRow>>,
    /// Discards a slower read when a newer one has already landed, the way the
    /// palette's History tab does.
    launcher_history_generation: u64,
    command_palette_focus: FocusHandle,
    /// The palette's query field. Long-lived like every other filter input;
    /// cleared on each open.
    command_palette_query: gpui::Entity<crate::widgets::text_input::TextInput>,
    /// Scroll state for the result list, so keyboard selection can be pulled
    /// back into view past the fold.
    command_palette_scroll: gpui::ScrollHandle,
    /// Self-update flow state (see `SelfUpdateState`).
    self_update: SelfUpdateState,
    /// State of the "Custom Buttons" management modal opened from the
    /// workspace context menu. `None` = closed.
    custom_buttons_modal: Option<app::custom_buttons_modal::CustomButtonsModal>,
    /// Focus handle routing key events to the custom-buttons modal while open.
    custom_buttons_modal_focus: FocusHandle,
    /// Live telemetry handle. `Null` when consent is missing
    /// or `SPLITLANE_NO_TELEMETRY` is set - every `capture`/`flush` call is a
    /// no-op in that state, so callers never branch on consent.
    telemetry: std::sync::Arc<crate::telemetry::client::TelemetryClient>,
    /// Monotonic clock at process start, used to compute
    /// `session_duration_seconds` for the `app_exited` event. Wall-clock-change
    /// proof - a system clock jump mid-session never produces a negative value.
    launch_instant: std::time::Instant,
    /// Last observed `config.telemetry.enabled` value, cached so the config
    /// watcher's reconcile path can detect a transition without
    /// re-reading the file.
    telemetry_enabled_last: Option<bool>,
    /// Shared "theme file changed" signal flipped by the theme
    /// watcher's debounce thread (event-driven invalidation). The 50 ms
    /// IPC poll loop in `process_config_changes` drains this flag and
    /// calls `cx.notify()` so the next render picks up the new theme.
    /// `Arc<AtomicBool>` - Send + Sync, lock-free.
    theme_changed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Git Diff mode state (see `DiffModeState`).
    diff_mode: DiffModeState,
    /// Agents-view sidebar state (rename/menu/skills/filter +
    /// the terminal-thread cache), extracted from the god-struct.
    pub(crate) agents_view: AgentsViewState,
    /// Memoized sidebar display order (worktree grouping). Recomputed
    /// only when the workspace set / order / repo roots change, keyed by a
    /// cheap content signature - `render_sidebar` runs on every app `notify()`,
    /// so the old per-frame `HashMap` + `Vec` rebuild was pure waste. Interior
    /// mutability because the render fn borrows `&self`.
    pub(crate) sidebar_order_cache: std::cell::RefCell<crate::app::sidebar::SidebarOrderCache>,
    /// The rail's project groups, in rail order. Membership is on each
    /// project (`Workspace::group`). See `app::project_groups`.
    pub(crate) project_groups: Vec<crate::app::project_groups::ProjectGroup>,
    /// A group made by `New group…` that has not been named yet, with where
    /// its project came from, so abandoning the name puts everything back.
    pub(crate) pending_new_group: Option<crate::app::project_groups::PendingNewGroup>,
    /// Held while a group's naming field is open: ends the field when focus
    /// leaves it.
    pub(crate) group_field_blur: Option<gpui::Subscription>,
    /// What the rail is dragging, set when a drag starts. See
    /// `app::drag::RailDragKind`.
    pub(crate) rail_drag_kind: std::rc::Rc<std::cell::Cell<Option<crate::app::drag::RailDragKind>>>,
    /// The keyboard focus a project group's label takes when clicked. One
    /// handle for every label; [`Self::focused_rail_group`] says whose.
    pub(crate) rail_group_focus: FocusHandle,
    /// The group whose label tracks [`Self::rail_group_focus`].
    pub(crate) focused_rail_group: Option<u64>,
}

/// Global flag for swap mode, checked by TerminalView to intercept Escape.
/// A process-global `AtomicBool` (rather than threading state through every
/// `TerminalView`) because the check sits on the keystroke hot path.
pub static SWAP_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl SplitlaneApp {
    fn primary_sidebar_expanded_width(&self) -> f32 {
        if self.settings_section.is_some() {
            crate::settings::chrome::SETTINGS_NAV_WIDTH
        } else {
            self.rail_width
        }
    }

    fn primary_sidebar_width_at(&self, now: std::time::Instant) -> f32 {
        if self.settings_section.is_some() {
            return crate::settings::chrome::SETTINGS_NAV_WIDTH;
        }
        if let Some(animation) = self.primary_sidebar_animation {
            animation.width_at(now)
        } else if self.primary_sidebar_visible {
            self.primary_sidebar_expanded_width()
        } else {
            0.
        }
    }

    fn rendered_primary_sidebar_width(&mut self, window: &mut Window) -> f32 {
        if self.settings_section.is_some() {
            self.primary_sidebar_animation = None;
            return crate::settings::chrome::SETTINGS_NAV_WIDTH;
        }

        let now = std::time::Instant::now();
        if let Some(animation) = self.primary_sidebar_animation {
            if animation.is_finished(now) {
                self.primary_sidebar_animation = None;
                animation.to_width
            } else {
                window.request_animation_frame();
                animation.width_at(now)
            }
        } else if self.primary_sidebar_visible {
            self.primary_sidebar_expanded_width()
        } else {
            0.
        }
    }

    pub(crate) fn toggle_primary_sidebar(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let from_width = self.primary_sidebar_width_at(now);
        self.primary_sidebar_visible = !self.primary_sidebar_visible;

        if self.settings_section.is_some() {
            self.primary_sidebar_animation = None;
            cx.notify();
            return;
        }

        let to_width = if self.primary_sidebar_visible {
            self.primary_sidebar_expanded_width()
        } else {
            0.
        };

        self.primary_sidebar_animation =
            if (from_width - to_width).abs() > PRIMARY_SIDEBAR_MIN_ANIMATION_DELTA {
                Some(SidebarWidthAnimation {
                    from_width,
                    to_width,
                    started_at: now,
                })
            } else {
                None
            };
        cx.notify();
    }

    /// Add a workspace's `.git` directory to the file watcher.
    /// Uses refcounting so multiple workspaces sharing a repo don't conflict.
    /// Silently skipped if the workspace is not in a git repo or watcher is unavailable.
    fn watch_git_dir(&mut self, ws: &Workspace) {
        if let Some(ref git_dir) = ws.git_dir {
            let current = self.git_watch_counts.get(git_dir).copied().unwrap_or(0);
            if current == 0 {
                // First workspace watching this git dir - register with OS.
                // Only commit the refcount when `watch()` succeeds. The
                // old form incremented to 1 before checking, so a transient
                // failure pinned the count at 1 and every later workspace
                // sharing the repo saw count>1 and never retried the
                // registration - the dir stayed permanently unwatched. On
                // failure we return without recording the entry so a later
                // workspace re-attempts the watch.
                if let Some(ref mut watcher) = self.git_watcher
                    && let Err(e) = watcher.watch(git_dir, notify::RecursiveMode::NonRecursive)
                {
                    log::warn!("git watcher: failed to watch {}: {e}", git_dir.display());
                    return;
                }
            }
            *self.git_watch_counts.entry(git_dir.clone()).or_insert(0) += 1;
        }
    }

    /// Remove a workspace's `.git` directory from the file watcher.
    /// Only unwatches when the last workspace using this git dir is removed.
    fn unwatch_git_dir(&mut self, git_dir: &std::path::Path) {
        if let Some(count) = self.git_watch_counts.get_mut(git_dir) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.git_watch_counts.remove(git_dir);
                if let Some(ref mut watcher) = self.git_watcher {
                    let _ = watcher.unwatch(git_dir);
                }
            }
        }
    }

    /// Create a new pane wrapping a terminal, and subscribe to its events.
    /// When the pane emits `PaneEvent::Remove` (last tab closed), the pane
    /// is removed from the split tree - following Zed's EventEmitter pattern.
    fn create_pane(
        &mut self,
        terminal: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        cx.subscribe(&terminal, Self::handle_terminal_event)
            .detach();
        let pane = cx.new(|cx| Pane::new(terminal, cx));
        cx.subscribe(&pane, Self::handle_pane_event).detach();
        pane
    }

    /// Create a pane around an existing tab and subscribe to pane-level events.
    /// Terminal tabs passed here have already been wired to app-level terminal
    /// events by their original owner; re-subscribing would duplicate CWD,
    /// port-scan, and exit handling.
    pub(crate) fn create_pane_with_existing_tab(
        &mut self,
        tab: TabContent,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let pane = cx.new(|cx| Pane::new_with_tab(tab, cx));
        cx.subscribe(&pane, Self::handle_pane_event).detach();
        pane
    }

    pub(crate) fn create_pane_with_existing_tabs(
        &mut self,
        tabs: Vec<TabContent>,
        selected_idx: usize,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let pane = cx.new(|cx| Pane::new_with_tabs(tabs, selected_idx, cx));
        cx.subscribe(&pane, Self::handle_pane_event).detach();
        pane
    }

    /// Centralised bookkeeping for a failed update attempt:
    /// classify the error, log it, update state, show the retry toast,
    /// and bump the attempt counter (which gates the 4th-click escape
    /// hatch).
    pub(crate) fn record_update_failure(
        &mut self,
        context: &str,
        err: &anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        log::error!("self-update/{context}: {err:#}");
        let tag = update::UpdateError::classify(err);
        // Single choke-point for the failure telemetry: the
        // classified `UpdateError` collapses into a canonical
        // `error_category` label so no message string ever leaves the
        // machine. Called before `show_update_error_toast` so the event is
        // queued even if toast rendering panics.
        self.emit_update_failure(&tag);
        self.self_update.self_update_status = update::SelfUpdateStatus::Errored(tag.clone());
        self.self_update.update_attempt_count =
            self.self_update.update_attempt_count.saturating_add(1);
        self.show_update_error_toast(&tag, cx);
        cx.notify();
    }

    // --- Sidebar rendering ---
}

/// The chord line under the empty state's two buttons.
///
/// Built from the live keymap rather than written down: the design spells
/// these for macOS, and on Linux and Windows the same three actions carry
/// different chords - a hardcoded `⌘N` would be wrong on two platforms out of
/// three. An action with no binding is simply left out.
fn empty_panes_hint(app: &SplitlaneApp) -> SharedString {
    let parts: Vec<String> = [
        ("new_agent", "agent"),
        ("new_shell", "shell"),
        ("open_command_palette", "to resume an old session"),
    ]
    .iter()
    .filter_map(|(action, what)| {
        app.shortcut_for_action(action)
            .map(|key| format!("{key} {what}"))
    })
    .collect();
    SharedString::from(parts.join(" \u{b7} "))
}

impl Render for SplitlaneApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = crate::theme::ui_colors();
        // Read only by the two platform blocks just below - the Windows Mica
        // sync and the macOS sidebar material. On Linux nothing reads it, and
        // `-D warnings` in CI makes an unused binding a build failure.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let theme = crate::theme::active_theme();
        #[cfg(target_os = "windows")]
        {
            let is_light = theme.background.l > 0.5;
            if self.windows_backdrop_light != Some(is_light) {
                crate::window_chrome::backdrop::sync_wallpaper_mica_theme(window, is_light);
                self.windows_backdrop_light = Some(is_light);
            }
        }
        #[cfg(target_os = "macos")]
        crate::window_chrome::macos_backdrop::sync_subtle_sidebar_material(
            theme.background.l > 0.5,
            self.cached_config.macos_chrome_material_enabled(),
        );
        // The title bar floats above the full window and the right panel
        // reserves a matching strip so content clears window controls - unless
        // the compositor is drawing the frame, in which case (per the design) the
        // row is not drawn and no strip is reserved.
        let draws_title_bar = crate::window_chrome::csd::app_draws_title_bar(window);
        let title_bar_h = if draws_title_bar {
            (1.75 * window.rem_size()).max(crate::app::constants::TITLE_BAR_MIN_HEIGHT)
        } else {
            px(0.)
        };
        let settings_open = self.settings_section.is_some();
        let sessions_sidebar_width = self.rendered_sessions_sidebar_width(window);
        let sessions_sidebar_mounted = self.agent_sessions.sessions_sidebar_open
            || self.agent_sessions.sessions_sidebar_animation.is_some();
        let sessions_sidebar_opacity = (sessions_sidebar_width
            / crate::app::sessions_sidebar::SESSIONS_SIDEBAR_WIDTH.max(1.))
        .clamp(0., 1.);
        let files_sidebar_width = self.rendered_files_sidebar_width(window);
        let files_sidebar_mounted =
            self.files_sidebar_open || self.files_sidebar_animation.is_some();
        let files_sidebar_opacity = (files_sidebar_width
            / crate::app::files_sidebar::FILES_SIDEBAR_WIDTH.max(1.))
        .clamp(0., 1.);
        let secondary_sidebar_open = sessions_sidebar_mounted || files_sidebar_mounted;
        // Every mode now renders the right area as ONE top-rounded clipped panel
        // (`panel_bg` fill + 16px rail-side top radius + 5px inset), replacing the
        // old Cli/Diff corner-mask trick. GPUI clips the panel's bg fill to the
        // radius, so the window backdrop shows in the corner notch - a clean
        // radius on every platform (Linux, macOS, Windows Mica), where a solid
        // mask would read as a square patch. The 5px inset keeps opaque content
        // (terminal cells, diff rows, settings cards) off the arc, since GPUI
        // does NOT clip children to the radius. The Cli pane grid normally
        // keeps the terminal background; on Windows terminal material it lets
        // the native backdrop show through. Diff / Agents / Settings use the
        // #181818 surface.
        let terminal_material_active = self.cached_config.windows_terminal_material_enabled();
        let chrome_material_active = self.cached_config.cockpit_chrome_material_enabled();
        let terminal_surface_mounted = self.active_workspace().is_some_and(|ws| ws.root.is_some());
        // The content area shows the active container's panes, unless the
        // application layer is up. There is nothing else it can show: every
        // surface - an agent session, a shell, the diff, a document - is a
        // pane's content, and the two full-area branches that used to stand
        // beside this one are gone with the second world they belonged to.
        let showing_panes = !settings_open;
        let terminal_material_visible =
            !settings_open && showing_panes && terminal_surface_mounted && terminal_material_active;
        let native_material_active = native_backdrop_material_active(
            showing_panes,
            settings_open,
            terminal_material_active,
            chrome_material_active,
        );
        let is_window_active = window.is_window_active();
        // The app's own frame comes from the theme's `chrome` role. It used to
        // read `title_bar_background` - a terminal slot - which is why picking
        // Harbor Dark repainted the terminal and left the chrome where it was.
        let shell_color = ui.chrome_for(is_window_active);
        let opaque_shell_bg = gpui::Hsla {
            a: 1.,
            ..shell_color
        };
        let app_backdrop_bg =
            crate::app::constants::cockpit_backdrop_background(shell_color, native_material_active);
        let panel_bg = if settings_open || !showing_panes {
            ui.base
        } else if terminal_material_visible {
            gpui::transparent_black()
        } else {
            // The panes area is the `gutter`, one step below a pane's own
            // surface, which is what lets a pane read as a card with a radius
            // instead of as a region of one flat sheet. It used to be the
            // terminal background - the same colour the panes themselves take,
            // so nothing but the divider said where one ended.
            ui.gutter
        };
        let panel_corner_mask_bg =
            crate::app::constants::cockpit_backdrop_background(shell_color, chrome_material_active);
        let panel_top = title_bar_h;
        let primary_sidebar_width = self.rendered_primary_sidebar_width(window);
        let title_bar_rail_width = self.primary_sidebar_expanded_width();
        let primary_sidebar_mounted = self.settings_section.is_some()
            || self.primary_sidebar_visible
            || self.primary_sidebar_animation.is_some();
        let primary_sidebar_opacity = if self.settings_section.is_some() {
            1.
        } else {
            (primary_sidebar_width / self.primary_sidebar_expanded_width().max(1.)).clamp(0., 1.)
        };
        // Every primary rail, including Settings, uses the same inset card.
        let primary_sidebar_card_mounted = primary_sidebar_mounted;
        let primary_sidebar_card_horizontal_inset =
            crate::app::constants::SIDEBAR_CARD_INSET.min(primary_sidebar_width / 2.);
        let main_panel_left_inset = if primary_sidebar_card_mounted {
            crate::app::constants::SIDEBAR_CARD_INSET - primary_sidebar_card_horizontal_inset
        } else {
            crate::app::constants::SIDEBAR_CARD_INSET
        };
        let primary_sidebar_card_width =
            (primary_sidebar_width - primary_sidebar_card_horizontal_inset * 2.).max(0.);
        let primary_sidebar_card_bg = crate::app::constants::primary_sidebar_card_background(
            ui.surface,
            chrome_material_active,
        );
        let isolate_primary_sidebar_material =
            cfg!(any(target_os = "windows", target_os = "macos")) && chrome_material_active;
        // Native sidebar material is isolated by an opaque shell mask. Reuse
        // that shell color for the main panel's corner wedges: transparent
        // paint cannot cover rectangular child backgrounds on Windows/macOS.
        let main_panel_corner_mask_bg = if isolate_primary_sidebar_material {
            opaque_shell_bg
        } else {
            panel_corner_mask_bg
        };
        #[cfg(target_os = "linux")]
        {
            crate::window_chrome::linux_backdrop::set_chrome_geometry(
                crate::window_chrome::linux_backdrop::ChromeGeometry {
                    left_sidebar_width: primary_sidebar_width,
                    right_sidebar_width: if sessions_sidebar_mounted {
                        sessions_sidebar_width
                    } else if files_sidebar_mounted {
                        files_sidebar_width
                    } else {
                        0.
                    },
                    title_bar_height: f32::from(title_bar_h),
                    title_bar_spans_window: true,
                },
            );
            crate::window_chrome::linux_backdrop::refresh_blur_region(window);
        }

        // What those panes' slot headers say about the surfaces in them:
        // the kind, the badge, the status word, the branch. All of it is a
        // fact about the surface record or the container, which a `Pane`
        // cannot see for itself.
        self.sync_surface_facts(cx);
        // And the diff panes' own chrome: the scope breadcrumb and the file
        // list, both built from app-level filter state a `DiffView` cannot
        // read for itself.
        self.sync_diff_panes(cx);

        // Focus the pane created by a drop-to-split. Deferred
        // here from the `DropSplit` subscription handler (no `Window` there).
        if let Some(pane) = self.pending_pane_focus.take() {
            pane.read(cx).focus_handle(cx).focus(window, cx);
        }
        if let Some(handle) = self.pending_focus.take() {
            handle.focus(window, cx);
        }
        // Whatever the frame above moved focus to, remember it: `\u{2303}\u{21e5}`
        // is answered from this pair and nothing else keeps it.
        if let Some(query) = self.pending_palette_output.take() {
            self.open_palette_output(query, window, cx);
        }
        if let Some((ws_idx, thread_idx, pane)) = self.pending_rail_surface_drop.take() {
            // The same action as the row menu's `Show in ‹pane›`, and so the
            // same function. It used to be a second one that *refused* when
            // the surface was already in a slot; a refusal holds the
            // one-surface-one-pane line too, but silently and differently
            // from the menu, which is how one action grows two answers.
            self.show_surface_in_pane(ws_idx, thread_idx, &pane, window, cx);
        }
        // GPUI owns the end of a drag; the rail's own record of one has to
        // follow it rather than guess.
        if self.dragging_rail_surface.is_some() && !cx.has_active_drag() {
            self.dragging_rail_surface = None;
        }
        self.track_pane_focus(window, cx);
        // And that every terminal in a pane has a record, so the rail draws
        // one kind of surface row. Idempotent, and the only reason it is here
        // rather than at each producer is that there are six of them.
        self.adopt_orphan_pane_surfaces(cx);
        // And what a pane holding nothing can be filled with. Pushed here for
        // the same reason the surface facts are: a `Pane` knows a
        // `workspace_id` and not a directory, so it cannot build this list and
        // must not try.
        self.sync_launcher_rows(cx);
        // **Nothing may be left holding the keyboard that is not on screen.**
        //
        // GPUI dispatches an action along the *focus chain*, and a handle that
        // is not in the rendered frame's dispatch tree falls back to the window
        // root - which sits ABOVE this app's own root `div`, where every
        // `on_action` lives. So a stale focus does not misroute one keystroke:
        // every action in the app dies at once, chord and button alike, and the
        // one people notice is `Add pane`, because it is the one they press
        // next. Measured - `⌘B` worked exactly twice, to open the Files panel
        // and to close it, and then nothing worked at all; found again in
        // use as "open Files, close it, and Add pane stops adding until I
        // click a pane".
        //
        // Focus goes stale in two ways and they need different questions:
        //
        // 1. The holder's `FocusHandle` lives on the app - the two docked
        //    panels, the pickers, the dialogs. It outlives the element, so
        //    `window.focused()` still answers Some and only this list can say
        //    the answer is a surface nobody can see.
        // 2. The holder's handle went with it - a rename field, a composer, a
        //    find bar, a launcher query, each of which owns its input entity.
        //    The handle leaves GPUI's focus map with the entity, so
        //    `window.focused()` answers None while `window.focus` still points
        //    at the dead id.
        //
        // Both hand the keyboard back to the pane the screen already names.
        // The question is **not** "is a pane focused": a find bar, a composer
        // and an inline rename all legitimately hold it while their pane is
        // drawn as focused, and pulling it out from under somebody mid-word
        // would be a second and worse bug.
        //
        // Every surface that takes focus belongs on this list; one that forgets
        // to join it has its keystrokes pulled away a frame later, which is a
        // quiet failure - `tests/focus_holder_policy.rs` is the machine form.
        // The list is the price of doing this once in the render pass instead
        // of at each of the two dozen places a surface closes.
        let (holder_on_screen, stale_holder_focus) = {
            let rail_group_label_shown = self.rail_group_focus.is_focused(window)
                && self
                    .focused_rail_group
                    .is_some_and(|id| self.project_group(id).is_some());
            let holders: [(bool, Option<&FocusHandle>); 13] = [
                (settings_open, Some(&self.settings_focus)),
                (
                    self.command_palette.is_some(),
                    Some(&self.command_palette_focus),
                ),
                (files_sidebar_mounted, Some(&self.files_focus)),
                (
                    sessions_sidebar_mounted,
                    Some(&self.agent_sessions.sessions_focus),
                ),
                (self.attention_queue_open, Some(&self.attention_queue_focus)),
                (self.pending_exit.is_some(), Some(&self.exit_confirm_focus)),
                (
                    self.broadcast_picker_open,
                    Some(&self.broadcast_picker_focus),
                ),
                (self.show_theme_picker, Some(&self.theme_picker_focus)),
                (self.launch_pad.is_some(), Some(&self.launch_pad_focus)),
                (
                    self.worktree_dialog.is_some(),
                    Some(&self.worktree_dialog_focus),
                ),
                (
                    self.custom_buttons_modal.is_some(),
                    Some(&self.custom_buttons_modal_focus),
                ),
                // The rail's menus and its inline rename own their own handles,
                // so they are here to say the rail is holding the keyboard;
                // case 2 above is what covers them when they go.
                (
                    crate::app::agents_sidebar::rail_overlay_open(self),
                    None::<&FocusHandle>,
                ),
                // A group label that was clicked holds the keyboard for `←`,
                // `→` and `⏎`; once its group is gone it is a stale holder and
                // the keyboard goes back to the panes.
                (rail_group_label_shown, Some(&self.rail_group_focus)),
            ];
            let on_screen = holders.iter().any(|(shown, _)| *shown);
            let stale = holders
                .iter()
                .any(|(shown, handle)| !shown && handle.is_some_and(|h| h.is_focused(window)));
            (on_screen, stale)
        };
        if !holder_on_screen {
            if self.active_workspace().is_none_or(|ws| ws.root.is_none()) {
                // A container with no panes, or no container at all, leaves
                // the keyboard with nowhere to be, so the empty state takes it
                // - it is the only thing on screen, and it names the doors
                // out. Both empty states track this handle.
                if !self.empty_panes_focus.is_focused(window) {
                    self.empty_panes_focus.focus(window, cx);
                }
            } else if stale_holder_focus || window.focused(cx).is_none() {
                self.return_focus_to_panes(window, cx);
            }
        }
        if std::mem::take(&mut self.pending_waiting_chip) {
            self.toggle_attention_queue(window, cx);
        }
        // The project toolbar sits above whatever surface the selection shows,
        // inside the panel. Settings is the application layer and has no
        // container, so it has no toolbar either. Under a system frame the
        // toolbar also carries the waiting chip and the palette hint.
        let project_toolbar = (!settings_open)
            .then(|| self.render_project_toolbar(!draws_title_bar, ui, cx))
            .flatten();
        let main_content = if self.settings_section.is_some() {
            // The application layer takes precedence over whatever surface is
            // selected: the left rail becomes the settings nav (below) and
            // this panel shows the active section body.
            self.render_settings_content_panel(cx).into_any_element()
        } else if let Some(ws) = self.active_workspace() {
            if let Some(root) = &ws.root {
                let app_weak = cx.weak_entity();
                let on_resize_end = std::rc::Rc::new(move |cx: &mut App| {
                    let _ = app_weak.update(cx, |app, cx| app.save_session(cx));
                });
                let stacked = matches!(
                    root.root_direction(),
                    Some(crate::layout::SplitDirection::Horizontal)
                );
                let panes = root.render(window, cx, Some(on_resize_end));
                // The drop strip runs along the axis the panes do not, so it
                // sits at the position the appended pane will take. It renders
                // to nothing unless a parked surface of THIS container is in
                // flight and there is room for it.
                let panes = match self.render_pane_drop_strip(ui, cx) {
                    Some(strip) => div()
                        .size_full()
                        .flex()
                        .when(stacked, |row| row.flex_col())
                        .when(!stacked, |row| row.flex_row())
                        .gap(tok::space::SM)
                        .child(div().flex_1().min_w_0().min_h_0().child(panes))
                        .child(strip)
                        .into_any_element(),
                    None => panes,
                };
                // The design's padding around the panes area. It is what
                // leaves the gutter visible on all four sides, so a slot reads
                // as a card lying on the desk rather than as a region cut out
                // of the panel.
                // The ladder's third rung is decided on the panes area's own
                // size, so the area measures itself: a canvas laid over it
                // (absolute + size_full, no effect on flex layout) writes the
                // number every frame, the way a split container already
                // captures its main axis for drag-to-resize.
                //
                // The canvas sits **inside** the padding. An absolute child
                // is sized against its parent's padding box, so hung on the
                // padded div it read 2 x LG more than the panes are given, and
                // the width refusal let through panes up to that much under
                // its own threshold.
                let area = self.panes_area.clone();
                div()
                    .size_full()
                    .p(tok::space::LG)
                    .child(
                        div()
                            .size_full()
                            .relative()
                            .child(
                                gpui::canvas(
                                    move |bounds, _window, _cx| {
                                        area.set((
                                            bounds.size.width.into(),
                                            bounds.size.height.into(),
                                        ));
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .child(panes),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_full()
                    // Holds the keyboard while there are no panes to hold it
                    // (see `empty_panes_focus`), so actions still have a chain
                    // to travel.
                    .track_focus(&self.empty_panes_focus)
                    // A container can now hold no panes at all - closing the
                    // last one leaves it that way rather than inventing a
                    // shell or leaving an empty pane standing. So this says
                    // where the next one comes from, and the answer is the
                    // rail row this project already has: creation lives on
                    // the object, and there is no second door.
                    // The design draws this state rather than leaving it
                    // blank: a dashed frame the size of the pane that is not
                    // there, what is missing, and the two things that fill it.
                    // It used to be a line of prose pointing at the rail,
                    // which is a caption where an affordance belongs.
                    .p(tok::space::LG)
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(tok::space::XL)
                            .rounded(tok::radius::PANEL)
                            .border_1()
                            .border_dashed()
                            // `border`, not `divider`: this outlines the pane
                            // that is not there, which is a surface boundary
                            // rather than a hairline inside one - and on
                            // Harbor Light a divider over the desk is invisible.
                            .border_color(ui.border)
                            .child(
                                div()
                                    .text_color(ui.text_body)
                                    .text_size(tok::text::TITLE)
                                    .child("Nothing open in this project"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(tok::space::MD)
                                    .child(
                                        div()
                                            .id("empty-panes-new-agent")
                                            .flex()
                                            .items_center()
                                            .h(tok::row::SEARCH)
                                            .px(tok::space::BLOCK)
                                            .rounded(tok::radius::CONTROL)
                                            .bg(ui.accent)
                                            .text_size(tok::text::ROW)
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(ui.on_accent)
                                            .child("Start an agent")
                                            // The same act the chord under it
                                            // names, and dispatched rather than
                                            // re-implemented so the two cannot
                                            // drift apart again. `\u{2318}N` starts the
                                            // container's preferred agent
                                            // outright and opens the launcher
                                            // only where there is no remembered
                                            // choice, because choosing IS the
                                            // decision. This button called the
                                            // launcher unconditionally, so a
                                            // project that had already answered
                                            // the question was asked it twice -
                                            // an empty screen offering two
                                            // coarse answers, then a list
                                            // offering every answer there is.
                                            .cursor_pointer()
                                            .on_click(cx.listener(
                                                |_, _: &gpui::ClickEvent, w, cx| {
                                                    w.dispatch_action(
                                                        Box::new(crate::NewAgent),
                                                        cx,
                                                    );
                                                },
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id("empty-panes-new-shell")
                                            .flex()
                                            .items_center()
                                            .h(tok::row::SEARCH)
                                            .px(tok::space::BLOCK)
                                            .rounded(tok::radius::CONTROL)
                                            .border_1()
                                            .border_color(ui.border)
                                            .text_size(tok::text::ROW)
                                            .text_color(ui.text_body)
                                            .child("Open a shell")
                                            .cursor_pointer()
                                            .on_click(cx.listener(
                                                |this, _: &gpui::ClickEvent, _w, cx| {
                                                    let idx = this.active_idx;
                                                    this.create_terminal_thread_in(idx, cx);
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .font_family(tok::font::MONO)
                                    .text_size(tok::mono::LABEL)
                                    .text_color(ui.dim)
                                    .child(empty_panes_hint(self)),
                            ),
                    )
                    .into_any_element()
            }
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                // With no container the render pass above still hands the
                // keyboard to `empty_panes_focus`, and a handle no element
                // tracks is no node in the dispatch tree: an action then
                // starts at the root, never passes `app_content`'s listeners,
                // and dies in the menu fallback, which cannot re-enter a
                // window already being updated. That silently swallowed both
                // this screen's `Open a folder` and `⇧⌘O`.
                .track_focus(&self.empty_panes_focus)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .text_center()
                        .gap(tok::space::MD)
                        .w(px(460.))
                        .px(tok::space::BLOCK)
                        .child(
                            div()
                                .text_color(ui.text)
                                .text_size(tok::text::TITLE)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Welcome to Splitlane"),
                        )
                        .child(div().text_color(ui.muted).text_size(tok::text::ROW).child(
                            "Run your coding agents side by side. Open a project, \
                                     put an agent, a shell, its diff or a file in each pane, \
                                     and see at a glance which session is waiting for you.",
                        ))
                        // This is the first screen of a first launch from the
                        // desktop, which opens no project on its own (see
                        // `launch_cwd::first_container_cwd`), so it offers the
                        // act rather than a caption pointing at the rail - the
                        // rule the empty-panes state above already follows.
                        .child(
                            div()
                                .id("welcome-open-folder")
                                .mt(tok::space::XS)
                                .flex()
                                .items_center()
                                .h(tok::row::SEARCH)
                                .px(tok::space::BLOCK)
                                .rounded(tok::radius::CONTROL)
                                .bg(ui.accent)
                                .text_size(tok::text::ROW)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(ui.on_accent)
                                .child("Open a folder")
                                // The same act as the chord below and File ->
                                // New Project, dispatched rather than
                                // re-implemented so the doors cannot drift.
                                .cursor_pointer()
                                .on_click(cx.listener(|_, _: &gpui::ClickEvent, w, cx| {
                                    w.dispatch_action(Box::new(crate::NewWorkspace), cx);
                                })),
                        )
                        .children(self.shortcut_for_action("new_workspace").map(|key| {
                            div()
                                .font_family(tok::font::MONO)
                                .text_size(tok::mono::LABEL)
                                .text_color(ui.dim)
                                .child(format!("{key} from anywhere"))
                        })),
                )
                .into_any_element()
        };
        // The centred path. Settings is the application layer and has no
        // container, so the centre is left empty there - as it was for the
        // workspace-name breadcrumb this replaced. Every other surface of a
        // container keeps it: the path is a fact about the container, not
        // about which of its surfaces happens to be on screen.
        let project_path = if self.settings_section.is_some() {
            None
        } else {
            self.active_workspace()
                .map(|ws| crate::app::sidebar::collapse_home(&ws.cwd, &self.home_dir))
        };
        // Measured once a frame by `refresh_attention_edge`, above, so this
        // frame's chip, the popover under it and the queue's own edge cannot
        // be looking at three different lists.
        let activity = self.activity();
        let announcement = self.chip_announcement();
        let palette_chord: gpui::SharedString = self
            .shortcut_for_action("open_command_palette")
            .unwrap_or("Unassigned")
            .to_string()
            .into();
        // Update CTA state - extracted to `update_pill_info()` so the Cli/
        // Agents sidebar banner and the Diff title-bar pill share one source.
        let update_info = self.update_pill_info();
        self.title_bar.update(cx, |tb, _| {
            tb.project_path = project_path;
            tb.activity = activity;
            tb.announcement = announcement;
            tb.palette_chord = palette_chord.clone();
            tb.sidebar_visible = self.primary_sidebar_visible;
            tb.left_rail_width = title_bar_rail_width;
            tb.update_available = update_info;
            tb.ipc_state = self.ipc_status.state();
            // The update / IPC-offline notices have two homes - the rail footer
            // and the title-bar pills - and exactly one must carry them per
            // frame. The rail footer is rendered by all three rails but NOT by
            // the settings nav that replaces the rail, so the title bar takes
            // over when the rail is hidden or settings are open. Previously
            // this was `cockpit = !is_agents`, which made the pills' guard
            // (`!is_agents && !cockpit`) identically false: both notices were
            // unreachable whenever the rail was down.
            tb.status_pills_visible =
                !self.primary_sidebar_visible || self.settings_section.is_some();
            tb.cockpit_material_active = chrome_material_active;
        });

        // The inner app content (title bar + sidebar + main). UI tree
        // uses Geist (bundled, registered at boot via
        // `Assets::load_fonts`). TerminalElement resolves its own
        // monospace family from `splitlane.json#font_family`, so the
        // terminal output is unaffected.
        let mut app_content = div()
            .font_family(crate::ui_tokens::font::UI)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .cursor(CursorStyle::Arrow)
            .on_action(cx.listener(Self::handle_split_h))
            .on_action(cx.listener(Self::handle_split_v))
            .on_action(cx.listener(Self::handle_close_pane))
            .on_action(cx.listener(Self::handle_new_tab))
            .on_action(cx.listener(Self::handle_close_tab))
            .on_action(cx.listener(Self::handle_focus_left))
            .on_action(cx.listener(Self::handle_focus_right))
            .on_action(cx.listener(Self::handle_focus_up))
            .on_action(cx.listener(Self::handle_focus_down))
            .on_action(cx.listener(Self::handle_focus_previous_pane))
            .on_action(cx.listener(Self::handle_jump_next_waiting))
            .on_action(cx.listener(Self::handle_new_workspace))
            .on_action(cx.listener(Self::handle_close_workspace))
            .on_action(cx.listener(Self::handle_copy_workspace_path))
            .on_action(cx.listener(Self::handle_reveal_workspace_in_file_manager))
            .on_action(cx.listener(Self::handle_open_workspace_in_zed))
            .on_action(cx.listener(Self::handle_open_workspace_in_cursor))
            .on_action(cx.listener(Self::handle_open_workspace_in_vscode))
            .on_action(cx.listener(Self::handle_open_workspace_in_windsurf))
            .on_action(cx.listener(Self::handle_next_workspace))
            .on_action(cx.listener(Self::handle_toggle_zoom))
            .on_action(cx.listener(Self::handle_layout_even_h))
            .on_action(cx.listener(Self::handle_layout_even_v))
            .on_action(cx.listener(Self::handle_layout_grid))
            .on_action(cx.listener(Self::handle_open_file_in_editor))
            .on_action(cx.listener(Self::handle_add_pane))
            .on_action(cx.listener(Self::handle_split_equalize))
            .on_action(cx.listener(Self::handle_swap_pane))
            .on_action(cx.listener(Self::handle_undo_close_pane))
            .on_action(cx.listener(Self::handle_open_multi_diff))
            .on_action(cx.listener(Self::handle_open_diff_view))
            .on_action(cx.listener(Self::handle_diff_full_window))
            .on_action(cx.listener(Self::handle_ws1))
            .on_action(cx.listener(Self::handle_ws2))
            .on_action(cx.listener(Self::handle_ws3))
            .on_action(cx.listener(Self::handle_ws4))
            .on_action(cx.listener(Self::handle_ws5))
            .on_action(cx.listener(Self::handle_ws6))
            .on_action(cx.listener(Self::handle_ws7))
            .on_action(cx.listener(Self::handle_ws8))
            .on_action(cx.listener(Self::handle_ws9))
            .on_action(
                cx.listener(|this: &mut Self, _: &CloseWindow, _window, cx| {
                    this.request_exit(crate::app::exit_guard::ExitIntent::Quit, cx);
                }),
            )
            // macOS menu-bar actions. `Quit` mirrors `CloseWindow`;
            // `About` opens the in-app About dialog. `Copy` / `Paste` are
            // routed to whatever has focus - see `bootstrap::route_os_copy`,
            // which is also where the macOS fallback sends them, so the two
            // doors onto one chord cannot answer differently. There is
            // deliberately no `SelectAll`:
            // the terminal exposes no select-all, and a menu entry wired to a
            // stub is worse than no entry.
            .on_action(cx.listener(|this: &mut Self, _: &Quit, _window, cx| {
                this.request_exit(crate::app::exit_guard::ExitIntent::Quit, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &About, _window, cx| {
                this.show_about_dialog = true;
                cx.notify();
            }))
            .on_action(cx.listener(|_this: &mut Self, _: &Copy, window, cx| {
                crate::app::bootstrap::route_os_copy_in(window, cx);
            }))
            .on_action(cx.listener(|_this: &mut Self, _: &Paste, window, cx| {
                crate::app::bootstrap::route_os_paste_in(window, cx);
            }))
            .on_action(cx.listener(|_this: &mut Self, _: &OpenHelp, _window, _cx| {
                if let Err(e) =
                    crate::external_open::open_url("https://github.com/ivkan/splitlane#readme")
                {
                    log::warn!("Help > Splitlane Help: could not open browser: {e}");
                }
            }))
            .on_action(cx.listener(Self::handle_start_self_update))
            .on_action(cx.listener(Self::handle_dismiss_update))
            .on_action(cx.listener(Self::handle_toggle_files_sidebar))
            // Title-bar `⋯` overflow menu for the current Agents thread.
            .on_action(cx.listener(Self::handle_open_agents_thread_menu))
            .on_action(cx.listener(Self::handle_new_home_agent))
            .on_action(cx.listener(Self::handle_new_agent))
            .on_action(cx.listener(Self::handle_new_shell))
            // Composer + broadcast groups.
            .on_action(cx.listener(Self::handle_open_composer))
            .on_action(cx.listener(Self::handle_toggle_broadcast_member))
            .on_action(cx.listener(Self::handle_open_broadcast_groups))
            // Attention Queue + Launch Pad.
            .on_action(cx.listener(Self::handle_open_attention_queue))
            .on_action(cx.listener(Self::handle_open_launch_pad))
            .on_action(cx.listener(Self::handle_new_worktree))
            .on_action(cx.listener(Self::handle_start_preset_1))
            .on_action(cx.listener(Self::handle_start_preset_2))
            .on_action(cx.listener(Self::handle_start_preset_3))
            // The command palette (also the title bar's "Commands").
            .on_action(cx.listener(Self::handle_open_command_palette))
            // The design's Alt-Cmd-C: the focused agent surface's last answer.
            .on_action(cx.listener(Self::handle_copy_last_answer))
            // Escape cancels an in-flight tab drag. Capture
            // phase runs ancestor-before-descendant, so this pre-empts the
            // focused terminal's own Escape->PTY forwarding - but only while a
            // drag is active; otherwise we leave the key untouched so normal
            // terminal Escape behaviour is unaffected. Drop-outside-target is
            // handled by GPUI itself (it clears the active drag on mouse-up
            // over a non-target), so no extra wiring is needed there.
            .capture_key_down(cx.listener(|_this, e: &gpui::KeyDownEvent, window, cx| {
                if cx.has_active_drag() && e.keystroke.key == "escape" {
                    cx.stop_active_drag(window);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(|_e, _, cx| cx.stop_propagation())
            // Sidebar + main content area. Branch on the
            // top-level UI mode so the CLI sidebar (workspace list)
            // and the Agents sidebar (projects + threads)
            // swap atomically with the main content.
            .child(
                div()
                    .id("app-body-row")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .overflow_hidden()
                    .relative()
                    // Dragging the rail's edge is tracked here rather than on
                    // the 5px strip itself: the pointer outruns a 5px target
                    // instantly, and a drag that stops the moment the cursor
                    // leaves the handle is not a drag.
                    .on_mouse_move(cx.listener(Self::handle_rail_resize_move))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(Self::handle_rail_resize_end),
                    )
                    // The Files tree's own edge, for the same reason. The two
                    // drags are mutually exclusive by construction - each
                    // handler returns immediately unless its own drag is the
                    // one in flight.
                    .on_mouse_move(cx.listener(Self::handle_files_resize_move))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(Self::handle_files_resize_end),
                    )
                    // The rounded window surface owns the backdrop. Keeping
                    // this row transparent is load-bearing: GPUI clips child
                    // overflow to a rectangle, so a row fill would repaint the
                    // transparent pixels outside the surface's corner radius.
                    // Sidebar content fades during width animations. Keep a
                    // stable chrome fill behind it so terminal-only material
                    // never exposes unblurred desktop pixels in the rail area.
                    .when(
                        terminal_material_visible && primary_sidebar_mounted,
                        |row| {
                            row.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .w(px(primary_sidebar_width))
                                    .bg(panel_corner_mask_bg),
                            )
                        },
                    )
                    .when(terminal_material_visible && secondary_sidebar_open, |row| {
                        row.child(
                            div()
                                .absolute()
                                .right_0()
                                .top_0()
                                .bottom_0()
                                .w(px(if sessions_sidebar_mounted {
                                    sessions_sidebar_width
                                } else {
                                    files_sidebar_width
                                }))
                                .bg(if isolate_primary_sidebar_material {
                                    opaque_shell_bg
                                } else {
                                    panel_corner_mask_bg
                                }),
                        )
                    })
                    // Native sidebar material belongs visually to the inset
                    // navigation card only. The platform backdrop still spans
                    // the host window, so this opaque mask covers the rest of
                    // the shell. A separately enabled Windows terminal material
                    // keeps its transparent panel while the chrome stays opaque.
                    .when(isolate_primary_sidebar_material, |row| {
                        row.child(sidebar_card_backdrop_mask(
                            primary_sidebar_width,
                            primary_sidebar_card_horizontal_inset,
                            primary_sidebar_card_width,
                            crate::app::constants::SIDEBAR_CARD_INSET,
                            title_bar_h,
                            opaque_shell_bg,
                            terminal_material_visible,
                        ))
                    })
                    // One childless decorative layer spans every primary rail
                    // and the title-bar overlay. Keeping it absolute preserves
                    // each mode's reflow width and follows the existing
                    // open/close animation without a second layout path.
                    .when(primary_sidebar_card_mounted, |row| {
                        row.child(
                            div()
                                .absolute()
                                .left(px(primary_sidebar_card_horizontal_inset))
                                .top(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                .bottom(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                .w(px(primary_sidebar_card_width))
                                .rounded(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                .bg(primary_sidebar_card_bg)
                                .border_1()
                                .border_color(ui.border)
                                .opacity(primary_sidebar_opacity),
                        )
                    })
                    // While settings is open the left rail becomes the Codex
                    // settings nav (kept visible even if the user had hidden the
                    // primary rail, so the back button is always reachable).
                    .when(primary_sidebar_mounted, |row| {
                        if self.settings_section.is_some() {
                            return row.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .h_full()
                                    .w(px(primary_sidebar_width))
                                    .flex_shrink_0()
                                    .overflow_hidden()
                                    // Clear the transparent title-bar overlay so the
                                    // settings header sits below the floating controls.
                                    .pt(title_bar_h)
                                    .child(self.render_settings_nav(window, cx))
                                    .into_any_element(),
                            );
                        }
                        row.child(
                            div()
                                .flex()
                                .flex_col()
                                .h_full()
                                .w(px(primary_sidebar_width))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .opacity(primary_sidebar_opacity)
                                // Clear the transparent title-bar overlay so the
                                // first rail row sits below the floating window
                                // controls.
                                .pt(title_bar_h)
                                .child(self.render_rail(window, cx))
                                .into_any_element(),
                        )
                        .child(self.render_rail_resize_handle(cx))
                    })
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .overflow_hidden()
                            // Anchor the absolutely-positioned border contour (below).
                            .relative()
                            .flex()
                            .flex_col()
                            // Every mode renders the right area with the same
                            // 10px corner language and 4px side/bottom inset as
                            // the CLI sidebar card. The content remains flush at
                            // the top; corner masks below preserve all four arcs
                            // because GPUI does not clip children to the radius.
                            .child(div().h(title_bar_h).flex_none())
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .overflow_hidden()
                                    .bg(panel_bg)
                                    .ml(px(main_panel_left_inset))
                                    .mr(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                    .mb(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                    .rounded(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .capture_any_mouse_down(cx.listener(
                                        |this, event: &gpui::MouseDownEvent, _window, cx| {
                                            if event.button == gpui::MouseButton::Left
                                                && this.panes_surface_visible()
                                                && let Some(workspace) = this.active_workspace_mut()
                                                && workspace
                                                    .agent_completion_notification
                                                    .is_unread()
                                            {
                                                workspace
                                                    .agent_completion_notification
                                                    .acknowledge();
                                                cx.notify();
                                            }
                                        },
                                    ))
                                    .children(project_toolbar)
                                    .child(main_content),
                            )
                            // Windows Acrylic spans the host window. Cover the
                            // panel's outer insets so native material cannot
                            // continue past the four rounded corner wedges.
                            .when(terminal_material_visible, |panel_shell| {
                                panel_shell
                                    .child(
                                        div()
                                            .absolute()
                                            .right_0()
                                            .top(panel_top)
                                            .bottom_0()
                                            .w(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                            .bg(opaque_shell_bg),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .right_0()
                                            .bottom_0()
                                            .h(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                            .bg(opaque_shell_bg),
                                    )
                                    .when(main_panel_left_inset > 0., |panel_shell| {
                                        panel_shell.child(
                                            div()
                                                .absolute()
                                                .left_0()
                                                .top(panel_top)
                                                .bottom_0()
                                                .w(px(main_panel_left_inset))
                                                .bg(opaque_shell_bg),
                                        )
                                    })
                            })
                            // GPUI clips overflow with a rectangular content
                            // mask, so rounded panel children can still paint
                            // square backgrounds in the corners. These masks
                            // restore the visual radius with surrounding chrome.
                            .child(
                                div()
                                    .absolute()
                                    .left(px(main_panel_left_inset))
                                    .top(panel_top)
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(
                                        PanelCorner::TopLeft,
                                        main_panel_corner_mask_bg,
                                    )),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .right(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                    .top(panel_top)
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(
                                        PanelCorner::TopRight,
                                        main_panel_corner_mask_bg,
                                    )),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(px(main_panel_left_inset))
                                    .bottom(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(
                                        PanelCorner::BottomLeft,
                                        main_panel_corner_mask_bg,
                                    )),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .right(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                    .bottom(px(crate::app::constants::SIDEBAR_CARD_INSET))
                                    .size(crate::app::constants::SIDEBAR_CARD_CORNER_RADIUS)
                                    .child(panel_corner_mask(
                                        PanelCorner::BottomRight,
                                        main_panel_corner_mask_bg,
                                    )),
                            ),
                    )
                    // Docked agent-sessions sidebar (right edge). A layout child
                    // - not an overlay - so it reflows the content and persists
                    // while the user works.
                    .when(sessions_sidebar_mounted, |row| {
                        row.child(
                            div()
                                .flex()
                                .flex_col()
                                .h_full()
                                .w(px(sessions_sidebar_width))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .opacity(sessions_sidebar_opacity)
                                // Keep the right rail below the full-width
                                // title bar, aligned with the main panel.
                                .pt(title_bar_h)
                                .child(self.render_sessions_sidebar(window, cx))
                                .into_any_element(),
                        )
                    })
                    // Docked Files sidebar (right edge) - same layout child as
                    // the sessions sidebar, mutually exclusive with it.
                    .when(files_sidebar_mounted && !sessions_sidebar_mounted, |row| {
                        row.child(
                            div()
                                .flex()
                                .flex_col()
                                .h_full()
                                .w(px(files_sidebar_width))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .opacity(files_sidebar_opacity)
                                // Keep the right rail below the full-width
                                // title bar, aligned with the main panel.
                                .pt(title_bar_h)
                                .child(self.render_files_sidebar(window, cx))
                                .into_any_element(),
                        )
                    }),
            );

        // The bottom of the window's vertical stack. It spans the rails as
        // well as the content area, which is what makes it the *window's*
        // status line rather than the panel's. Settings is the application
        // layer and has no container, so - like the toolbar - it has none.
        if !settings_open {
            let content_width = crate::app::status_bar::content_area_width(
                window,
                if primary_sidebar_mounted {
                    primary_sidebar_width
                } else {
                    0.
                },
                if sessions_sidebar_mounted {
                    sessions_sidebar_width
                } else if files_sidebar_mounted {
                    files_sidebar_width
                } else {
                    0.
                },
            );
            app_content = app_content.child(self.render_status_bar(content_width, window, ui, cx));
        }

        if draws_title_bar {
            // The title bar floats as an overlay above the rail and panel. It
            // still owns window drag and custom controls where the platform
            // needs them.
            app_content = app_content.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    // Supported desktop platforms span the full window. The
                    // right panel reserves a matching top strip, so content
                    // clears native or custom window controls.
                    .w_full()
                    .overflow_hidden()
                    .child(self.title_bar.clone()),
            );
        }

        if let Some(toast) = &self.toast {
            app_content = app_content.child(self.render_toast(toast, ui));
        }

        if self.show_theme_picker {
            app_content = app_content.child(self.render_theme_picker(cx));
        }

        // Broadcast-group picker modal.
        if self.broadcast_picker_open {
            app_content = app_content.child(self.render_broadcast_picker(cx));
        }

        // Attention Queue overlay + Launch Pad modal.
        // Mode-gated: a mode switch while a launch runs in the
        // background must not paint cockpit chrome over Agents/Diff - the
        // modal reappears (or finishes) back in Cli mode.
        // These four overlays act on the panes of a container - the Composer
        // targets a pane, the Launch Pad splits into panes, Broadcast writes
        // to them. They render only while the panes surface is the one on
        // screen; the same predicate refuses the action at its entry point,
        // so the refusal is visible rather than silent.
        if self.attention_queue_open && showing_panes {
            // The queue hangs off the waiting chip, and the chip is not always
            // in the same row: it sits in the title bar when the app draws one
            // and moves into the project toolbar when the platform draws the
            // frame instead. The popover follows it either way.
            let queue_anchor = if draws_title_bar {
                title_bar_h
            } else {
                crate::ui_tokens::row::TOOLBAR
            };
            app_content = app_content.child(self.render_attention_queue(queue_anchor, cx));
        }
        if self.worktree_dialog.is_some() {
            app_content = app_content.child(self.render_worktree_dialog(cx));
        }
        if self.launch_pad.is_some() {
            app_content = app_content.child(self.render_launch_pad(cx));
        }
        // The command palette. Deliberately outside the panes gate - it is
        // the floor of reachability (every action stays reachable), so it opens
        // over every surface.
        // The fleet-grep overlay used to sit beside it, gated to panes; it is
        // the palette's "Output" tab now.
        if self.command_palette.is_some() {
            app_content = app_content.child(self.render_command_palette(cx));
        }

        if self.custom_buttons_modal.is_some() {
            app_content = app_content.child(self.render_custom_buttons_modal(cx));
        }

        if self.show_about_dialog {
            app_content = app_content.child(self.render_about_dialog(cx));
        }

        if let Some(menu) = self.workspace_menu_open
            && menu.idx < self.workspaces.len()
        {
            app_content =
                app_content.children(self.render_workspace_context_menu(menu, ui, window, cx));
        }

        // "Move to pane…" tab context menu.
        if let Some(menu) = self.tab_menu_open.clone() {
            app_content = app_content.child(self.render_tab_context_menu(menu, ui, window, cx));
        }

        // Per-file copy-path context menu.
        if let Some(menu) = self.files_menu_open.clone() {
            app_content = app_content.child(self.render_files_context_menu(menu, ui, window, cx));
        }

        // Agents-mode right-click context
        // menu (project header or thread row) + delete-confirmation
        // dialog. Both render only when the corresponding state field
        // is `Some`; the dispatcher fns guard against stale indices.
        if let Some(menu) = self.agents_view.agents_menu_open
            && let Some(el) =
                crate::app::agents_sidebar::render_open_agents_menu(self, menu, ui, window, cx)
        {
            app_content = app_content.child(el);
        }
        if let Some(target) = self.agents_view.agents_confirm_delete {
            app_content =
                app_content.child(self.render_agents_confirm_delete_dialog(target, ui, cx));
        }
        if let Some(intent) = self.pending_exit {
            app_content = app_content.child(self.render_exit_confirm_dialog(intent, ui, cx));
        }
        // The branch picker: a container's menu, not a panel's, so it renders
        // here with the other context menus and anchors where it was opened.
        if let Some(el) = self.render_agents_branch_menu_overlay(ui, window, cx) {
            app_content = app_content.child(el);
        }

        crate::window_chrome::csd::client_side_window_shell(
            app_content,
            window,
            app_backdrop_bg,
            if terminal_material_visible {
                gpui::transparent_black()
            } else {
                ui.border
            },
        )
    }
}

// ---------------------------------------------------------------------------
// `--update-and-exit` (e2e auto-update harness)
// ---------------------------------------------------------------------------

/// Synchronous self-update entry point invoked by the e2e harness
/// (`scripts/test-update-e2e.sh`). Mirrors the GUI flow's check + per-format
/// install steps but never initializes GPUI - so it runs cleanly in headless
/// CI containers without Xvfb. Honours `SPLITLANE_UPDATE_FEED_URL`
/// ([`update::checker::update_feed_url`]) so the harness can point the
/// checker at a localhost fixture.
///
/// Returns the process exit code (see `--update-and-exit` doc-comment in
/// `main` for the full table). The split between exit-3 (feed unreachable)
/// and exit-1 (other) is deliberate - the harness asserts a specific code,
/// not a substring of the generic "update failed" toast.
fn run_update_and_exit() -> i32 {
    use crate::update::checker::{UpdateStatus, check_github_release};
    use crate::update::install_method::{self, InstallMethod};

    let method = install_method::detect();
    log::info!("--update-and-exit: install method = {method:?}");

    // The harness MUST NOT emit telemetry - the test runs are not user
    // sessions and would skew funnels. Use a Null client (no-op
    // capture, no HTTP).
    let null_telemetry = crate::telemetry::client::TelemetryClient::Null;
    let status = check_github_release(&null_telemetry);
    let (version, asset_url) = match status {
        UpdateStatus::Available {
            version,
            asset_url: Some(url),
            ..
        } => (version, url),
        UpdateStatus::Available {
            asset_url: None, ..
        } => {
            eprintln!("splitlane-update: no asset matched the install method - nothing to install");
            return 5;
        }
        UpdateStatus::UpToDate => {
            eprintln!("splitlane-update: already up to date");
            return 2;
        }
        UpdateStatus::Failed => {
            // The checker logs whether the failure was DNS/HTTP/parse via
            // `log::warn!`; we can't easily distinguish here without a
            // structured error, so print the explicit feed-unreachable
            // hint - the dominant failure mode the harness
            // exercises (kill miniserve before invocation).
            eprintln!(
                "splitlane-update: feed unreachable at {} - check SPLITLANE_UPDATE_FEED_URL",
                crate::update::checker::update_feed_url()
            );
            return 3;
        }
        UpdateStatus::Checking => {
            eprintln!("splitlane-update: checker returned Checking - should never happen");
            return 1;
        }
    };

    log::info!("--update-and-exit: installing v{version} from {asset_url}");

    match method {
        InstallMethod::TarGz { .. } => match crate::update::linux::targz::run_update(&asset_url) {
            Ok(new_bin) => {
                println!("splitlane-update: ok new={}", new_bin.display());
                0
            }
            Err(err) => {
                let classified = crate::update::error::UpdateError::classify(&err);
                if matches!(
                    classified,
                    crate::update::error::UpdateError::IntegrityMismatch { .. }
                ) {
                    eprintln!("splitlane-update: hash mismatch - {err}");
                    return 4;
                }
                eprintln!("splitlane-update: install failed - {err}");
                1
            }
        },
        InstallMethod::AppImage { source_path, .. } => {
            // Deferred: appimageupdatetool isn't part of the default
            // CI image, and it has no in-process SHA verify path (the tool
            // fetches via embedded zsync metadata). The tar.gz path covers
            // the same regression surface (download + SHA verify + atomic
            // swap + restart-path). Leaving the wiring in place so a
            // follow-up can opt in by installing the tool.
            match crate::update::linux::appimage::run_update(&source_path, &asset_url) {
                Ok(new_bin) => {
                    println!("splitlane-update: ok new={}", new_bin.display());
                    0
                }
                Err(err) => {
                    eprintln!("splitlane-update: AppImage install failed - {err}");
                    1
                }
            }
        }
        // SystemPackage (.deb/.rpm/dnf/apt) updates need pkexec + a
        // running polkit agent - neither belongs in `--update-and-exit`,
        // which is designed to be deterministic and non-interactive.
        // AppBundle/WindowsMsi: the e2e harness is Linux-only (Windows e2e
        // is covered separately).
        other => {
            eprintln!(
                "splitlane-update: --update-and-exit does not support install method {other:?}"
            );
            5
        }
    }
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

/// Whether a Windows startup console belongs only to Splitlane and can be shed.
///
/// Console-subsystem executables launched from Explorer / Start Menu get a new
/// console before Rust code runs. When that console contains only Splitlane, the
/// launch is a GUI launch and the console is visual noise. When it contains the
/// parent shell too, the user is running a CLI/scriptable path and stdout,
/// stderr, waiting, and exit codes must remain intact.
#[cfg(windows)]
fn should_detach_windows_console(
    is_scriptable_invocation: bool,
    console_process_count: u32,
) -> bool {
    !is_scriptable_invocation && console_process_count == 1
}

/// Detach the one-process console Windows creates for Explorer/Start launches.
#[cfg(windows)]
fn detach_lonely_windows_console_for_gui_launch(is_scriptable_invocation: bool) {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};

    let mut processes = [0_u32; 2];
    // SAFETY: GetConsoleProcessList writes at most the buffer length we pass
    // and returns the number of attached console processes. A return larger
    // than the buffer means "there are multiple processes", which is exactly
    // the keep-attached case for terminal-launched CLI paths.
    let count = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) };
    if should_detach_windows_console(is_scriptable_invocation, count) {
        // SAFETY: FreeConsole only detaches this process from its console. It
        // has no Rust aliasing or lifetime implications; failure is harmless
        // and simply leaves the console visible.
        unsafe {
            let _ = FreeConsole();
        }
    }
}

#[cfg(all(test, windows))]
mod windows_startup_console_tests {
    use super::should_detach_windows_console;

    #[test]
    fn gui_launch_detaches_only_a_lonely_console() {
        assert!(should_detach_windows_console(false, 1));
        assert!(!should_detach_windows_console(false, 0));
        assert!(!should_detach_windows_console(false, 2));
    }

    #[test]
    fn scriptable_invocation_keeps_console_even_when_lonely() {
        assert!(!should_detach_windows_console(true, 1));
    }
}

fn mount_splitlane_app(window: &mut Window, cx: &mut App) -> Entity<SplitlaneApp> {
    let view = window.replace_root(cx, |_, cx| SplitlaneApp::new(cx));
    view.update(cx, |_, cx| {
        let subscription = cx.observe_window_bounds(window, |this, window, cx| {
            crate::window_state::record_windowed_size(window);
            #[cfg(target_os = "linux")]
            crate::window_chrome::linux_backdrop::refresh_blur_region(window);
            if this.settings_section.is_some() {
                this.reset_settings_scroll();
                cx.notify();
                cx.on_next_frame(window, |this, _window, cx| {
                    if this.settings_section.is_some() {
                        cx.notify();
                    }
                });
            } else {
                cx.notify();
            }
        });
        subscription.detach();
    });
    window.on_window_should_close(cx, {
        let view = view.clone();
        move |_window, cx| {
            // The OS close button. The window never closes on its own: either
            // the app quits (and with it the window), or the exit card is up
            // and the window stays to show it. Session save, the `app_exited`
            // flush and the Linux backdrop release all live on the one exit
            // path now (`exit_guard::perform_exit`).
            view.update(cx, |app, cx| {
                app.request_exit(crate::app::exit_guard::ExitIntent::Quit, cx);
            });
            false
        }
    });
    // Track window-activation state
    // for OS notification gating.
    view.update(cx, |_, cx| {
        let subscription =
            cx.observe_window_activation(window, |app: &mut SplitlaneApp, window, cx| {
                let active = window.is_window_active();
                crate::agents::notifications::set_window_active(active);
                if active {
                    // Coming back to the window is when the limits get looked at.
                    // The read is floored (and backs off on failure) inside
                    // `refresh_claude_limits`, so alt-tabbing cannot turn into a
                    // poller.
                    app.refresh_claude_limits(crate::app::agents_sidebar::LimitsRefresh::Focus, cx);
                    // And it is when the attention queue's edge has to forget
                    // what it thought it knew. **Coming back is not an edge**,
                    // and while the window was minimised or the machine asleep
                    // nothing painted, so the frame that observes the queue did
                    // not run: without this, the first frame back compares a
                    // latch from before the absence against a queue filled
                    // during it, and announces. That is the failure the design
                    // names in as many words - never make an absence cost
                    // somebody a noise they cannot act on faster for.
                    app.rebaseline_attention_edge();
                    // And if the Notifications page is open, coming back is
                    // very likely coming back *from* System Settings, which is
                    // the only place the answer it states can be changed. A
                    // page that kept showing "denied" after the person had
                    // just allowed it would be the same lie as the one this
                    // row was added to stop.
                    #[cfg(target_os = "macos")]
                    if app.settings_section == Some(SettingsSection::Notifications) {
                        cx.spawn(async move |this, cx| {
                            smol::unblock(|| {
                                crate::agents::mac_notifications::refresh();
                            })
                            .await;
                            let _ = this.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    }
                }
                #[cfg(target_os = "linux")]
                crate::window_chrome::linux_backdrop::refresh_blur_region(window);
                cx.notify();
            });
        subscription.detach();
    });
    crate::agents::notifications::set_window_active(window.is_window_active());

    view.update(cx, |app, cx| {
        app.sync_system_theme_from_window(window, cx);
        let subscription = cx.observe_window_appearance(window, |this, window, cx| {
            this.sync_system_theme_from_window(window, cx);
            cx.notify();
        });
        subscription.detach();
    });

    view.update(cx, |app, cx| {
        if let Some(ws) = app.workspaces.get(app.active_idx) {
            // `FOCUS.md`: the restored window focuses the FIRST pane, and
            // `prevFocus` is the second (the first when there is only one).
            // Restoring the previously focused pane would be defensible;
            // restoring focus to a shell that came back empty is not.
            ws.focus_first(window, cx);
            if let Some(root) = ws.root.as_ref() {
                let leaves = root.collect_leaves();
                // Both halves of the pair, not just the previous one:
                // `track_pane_focus` runs on the first frame, sees a focused
                // pane it has no record of, and would push the record it does
                // have (nothing) into `prevFocus` - wiping the priming before
                // the first keystroke.
                let now = leaves.first().map(gpui::Entity::downgrade);
                let before = leaves
                    .get(1)
                    .or_else(|| leaves.first())
                    .map(gpui::Entity::downgrade);
                app.focused_pane_now = now;
                app.focused_pane_before = before;
            }
        }
    });
    view
}

fn main() {
    // Handle --help and --version before initializing GPUI
    let args: Vec<String> = std::env::args().collect();
    #[cfg(unix)]
    if args.get(1).map(String::as_str) == Some(agents::parent_guard::PTY_GUARD_SUBCOMMAND) {
        std::process::exit(agents::parent_guard::run_pty_guard_from_args(&args));
    }
    #[cfg(target_os = "windows")]
    if external_open::is_open_url_helper_invocation(&args) {
        std::process::exit(external_open::run_open_url_helper_from_args(&args));
    }
    #[cfg(windows)]
    let is_msi_relay = update::windows::msi::is_relay_invocation(&args);
    #[cfg(not(windows))]
    let is_msi_relay = false;
    // Detect the `mcp` subcommand BEFORE the global flag scans. Those
    // scans look at *every* arg, so `splitlane mcp install --help` would
    // otherwise match the global `--help` and print the top-level help instead
    // of routing to the `mcp` handler (which forwards `--help` to its own
    // subcommand parser). Gating the global scans on `!is_mcp_subcommand`
    // hands `splitlane mcp …` straight to the dispatcher below.
    let is_mcp_subcommand = args.get(1).map(String::as_str) == Some("mcp");
    // Same gating rationale as the `mcp`
    // flag. When argv[1] is a known CLI verb (`splitlane ls --help`,
    // `splitlane read … --json`), the global flag scans below must NOT fire -
    // clap owns per-subcommand `--help`/`--version`, and the CLI dispatch runs
    // after the manual intercepts.
    let is_cli_subcommand = cli::is_cli_verb(args.get(1).map(String::as_str));
    // `splitlane hooks <cmd>` is intercepted
    // before clap (like `mcp`) and mutates agent config files offline - so the
    // global flag scans must not eat its `--help`.
    let is_hooks_subcommand = args.get(1).map(String::as_str) == Some("hooks");
    let is_global_help = !is_msi_relay
        && !is_mcp_subcommand
        && !is_cli_subcommand
        && !is_hooks_subcommand
        && args.iter().any(|a| a == "--help" || a == "-h");
    let is_global_version = !is_msi_relay
        && !is_mcp_subcommand
        && !is_cli_subcommand
        && !is_hooks_subcommand
        && args.iter().any(|a| a == "--version" || a == "-v");
    let is_update_and_exit = !is_msi_relay
        && !is_mcp_subcommand
        && !is_cli_subcommand
        && !is_hooks_subcommand
        && args.iter().any(|a| a == "--update-and-exit");
    let is_unknown_verb = args
        .get(1)
        .is_some_and(|verb| cli::looks_like_unknown_verb(Some(verb.as_str())));

    #[cfg(windows)]
    detach_lonely_windows_console_for_gui_launch(
        is_msi_relay
            || is_mcp_subcommand
            || is_cli_subcommand
            || is_hooks_subcommand
            || is_global_help
            || is_global_version
            || is_update_and_exit
            || is_unknown_verb,
    );

    #[cfg(windows)]
    if is_msi_relay {
        std::process::exit(update::windows::msi::run_relay_from_args(&args));
    }

    if is_global_help {
        println!(
            "Splitlane {version} - native terminal workspace for coding agents\n\
             \n\
             Usage: splitlane [OPTIONS]\n\
             \x20      splitlane <ls|read|send|up|wait|...>  Script the running app\n\
             \x20      splitlane mcp <install|status|uninstall>\n\
             \n\
             Options:\n\
             \x20 -h, --help       Print this help message\n\
             \x20 -v, --version    Print version\n\
             \x20 --update-and-exit  Check for an update and exit (CI harness)\n\
             \n\
             Agent workflow:\n\
             \x20 Launch Claude Code, Codex, opencode, Pi, or any CLI agent in panes\n\
             \x20 Use `splitlane mcp install` so capable agents can read pane output\n\
             \n\
             Keybindings (Cmd instead of Ctrl on macOS):\n\
             \x20 Alt+\\            Add pane\n\
             \x20 Ctrl+Shift+W     Close pane\n\
             \x20 Alt+Arrow        Focus adjacent pane\n\
             \x20 Ctrl+Shift+N     New agent (Cmd+N on macOS)\n\
             \x20 Ctrl+1-9         Switch to project N\n\
             \n\
             Settings > Shortcuts lists every action. Config paths, scripting\n\
             commands and the IPC socket are documented in docs/user/.\n\
             https://github.com/ivkan/splitlane",
            version = env!("CARGO_PKG_VERSION")
        );
        return;
    }
    if is_global_version {
        println!("splitlane {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Scrub the inherited agent-session
    // env markers BEFORE any thread::spawn / tokio runtime / smol /
    // GPUI init reads or mutates env. Rust 1.85 made
    // `std::env::remove_var` `unsafe` precisely because it races
    // with concurrent `getenv` calls; the only race-free place to
    // mutate process env is the top of `main()` before any other
    // thread exists.
    //
    // The scrub belongs here rather than in the PTY env assembly: the terminal
    // backend spawns children against the inherited environment with no
    // `env_clear`, so dropping a key from the overlay map leaves the inherited
    // value reaching the child untouched. Launching Splitlane from an agent
    // session or an IDE terminal otherwise hands every terminal a
    // `CLAUDE_CODE_CHILD_SESSION`, and an agent that sees it silently stops
    // saving its transcript - which also costs the user a resumable session.
    // SAFETY: this is still before env_logger, GPUI, IPC, config watchers,
    // async executors, and any app-owned thread.
    unsafe { splitlane_acp::scrub_inherited_agent_session_env() };
    // Same route for the host terminal's identity (`WT_SESSION`, `TMUX`, ...):
    // a pane answers "which emulator is this" itself, and a stale answer makes
    // an agent CLI change its wire behaviour. SAFETY: as above.
    unsafe { splitlane_acp::spawn::scrub_inherited_host_terminal_env() };

    // Quiet by default: a plain `cargo run` (or a shipped binary) shows only
    // warnings + errors. `RUST_LOG=info` restores the startup/runtime
    // diagnostics (GPU selection, IPC, session restore, …) and `RUST_LOG=debug`
    // adds the per-operation diff/git trace - matching the documented
    // "RUST_LOG=info cargo run # with logging" workflow.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "warn,wgpu_hal=off,naga=warn,gpui_macos::text_system=error,zbus=warn,tracing::span=warn",
    ))
    .init();

    // Install the process-wide kill-on-parent-death guard BEFORE any
    // agent CLI or ConPTY spawns so children inherit the Job Object (Windows).
    match agents::parent_guard::install_process_job() {
        Ok(agents::parent_guard::ParentGuardStatus::Installed) => {}
        Ok(agents::parent_guard::ParentGuardStatus::Unsupported) => {
            log::debug!(
                "parent_guard: process-wide job guard unsupported on Unix; PTY shells use per-PTY guards and shim-wrapped agents use shim guards"
            );
        }
        Err(err) => {
            log::warn!(
                "parent_guard: failed to install Job Object; kill -9 of Splitlane may orphan agent CLIs ({err})"
            );
        }
    }

    // Adopt the user's login-shell environment only for the real GUI path
    // (Finder / Dock / `.desktop`), where the inherited launchd / systemd-user
    // PATH omits Homebrew, Nix, version managers, and `~/.zprofile` additions.
    // Scriptable CLI/MCP/hooks/update invocations must not execute an
    // interactive login shell as a side effect. Runs before the static prepend
    // below so per-user bin dirs stay first. Must run before any other thread
    // spawns - it mutates the process environment (see the module's safety note).
    if should_load_login_shell_env_for_startup(
        is_msi_relay,
        is_mcp_subcommand,
        is_cli_subcommand,
        is_hooks_subcommand,
        is_update_and_exit,
        is_unknown_verb,
    ) {
        login_shell_env::load_login_shell_env();
    }

    // Patch PATH BEFORE GPUI starts so agent launch and CLI helper lookups find
    // binaries installed under `~/.bun/bin` when Splitlane is launched from a
    // `.desktop` file / Finder / Start Menu (those inherit a minimal
    // systemd-user / launchd / Explorer PATH that does not source the user's
    // shell rc). Must run before any other thread spawns - see safety note on
    // `augment_path_for_gui_launch`.
    runtime_paths::augment_path_for_gui_launch();

    // Synchronous update flow for the e2e harness. Runs the same
    // checker + per-format installer the GUI calls, but without ever
    // initializing GPUI - exits with status 0 on a successful swap, 2 on
    // "no update needed", 3 on a feed-unreachable error (an explicit
    // "feed unreachable" requirement vs the generic "update failed"),
    // 4 on integrity / hash mismatch, 5 on unsupported install method,
    // 1 on any other error. Pair with `SPLITLANE_UPDATE_FEED_URL` to
    // point the checker at a localhost fixture.
    // Gate the global `--update-and-exit` scan on the SAME three intercepts as
    // the `--help`/`--version` scans above, not just `mcp`. Otherwise a literal
    // `--update-and-exit` token appearing as a CLI/hooks *argument* (e.g.
    // `splitlane send <t> "--update-and-exit"`, `splitlane search x --update-and-exit`)
    // is captured by this `args.iter().any(...)` scan and hijacks the verb into
    // the self-updater (a verb argument must never be captured by a global scan).
    if is_update_and_exit {
        std::process::exit(run_update_and_exit());
    }

    // `splitlane mcp <subcommand>` runs as a scriptable CLI
    // and exits - it never initializes GPUI / opens a window. Placed after
    // `augment_path_for_gui_launch` (so agent-CLI detection sees `~/.bun/bin`
    // etc.) and after `--update-and-exit`, before any GUI bootstrap. The
    // install engine lives in the GPU-free `splitlane-mcp-install` crate.
    // Only `install` extracts the bridge; `status` and `uninstall` must stay
    // read-only with respect to Splitlane's own data dir.
    // Diagnostics go to stderr (env_logger), the per-agent report to stdout.
    if args.get(1).map(String::as_str) == Some("mcp") {
        let bridge_path = if should_extract_mcp_bridge_for_cli(&args) {
            match ai_hooks::extract::ensure_bridge_extracted() {
                Ok(p) => Some(p),
                Err(e) => {
                    log::warn!("splitlane mcp: bridge extraction failed ({e:#})");
                    // Fall back to the resolved-but-maybe-missing path so the
                    // engine can emit the precise "binary missing at <path>"
                    // refusal rather than a vaguer "data dir unresolved".
                    runtime_paths::bridge_binary_path()
                }
            }
        } else {
            runtime_paths::bridge_binary_path()
        };
        std::process::exit(splitlane_mcp_install::run_cli(&args[2..], bridge_path));
    }

    // `splitlane hooks <cmd>` installs the
    // persistent agent-notification hooks and exits - like `mcp`, it mutates
    // external config files offline and never initializes GPUI. Extract the
    // ai-hook callback to its stable path first so the path written into agent
    // configs is guaranteed to exist; fall back to the resolved-but-maybe-
    // missing path so the engine can emit a precise refusal.
    if is_hooks_subcommand {
        let hook_path = match ai_hooks::extract::ensure_ai_hook_extracted() {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn!("splitlane hooks: ai-hook extraction failed ({e:#})");
                runtime_paths::ai_hook_binary_path()
            }
        };
        std::process::exit(splitlane_mcp_install::run_hooks_cli(&args[2..], hook_path));
    }

    // The `splitlane <verb>` scriptable CLI
    // drives a RUNNING instance over the existing IPC socket and exits - it
    // never initializes GPUI. Gated on a known verb in argv[1] (same pattern as
    // `mcp`) so unknown args still fall through to the GUI below. Placed after
    // the logger + PATH augmentation so the CLI inherits `RUST_LOG` and the
    // same binary-resolution environment as the GUI.
    if is_cli_subcommand {
        std::process::exit(cli::run());
    }

    // An argv[1] shaped like a verb but not one we own
    // (`splitlane blah`, a mistyped `splitlane searh`, or the MCP tool name had
    // an alias not been wired) is a typo, not a GUI launch. The `mcp`/`hooks`/
    // known-verb intercepts above have all exited by now, so anything still
    // here is genuinely unknown: print an actionable error and exit non-zero
    // (clap's usage-error code 2) instead of falling through to the bootstrap,
    // which would silently trip the single-instance guard. A bare `splitlane`
    // (no argv[1]) and any `-`/`--` flag are NOT flagged, so the GUI and the
    // global-flag scans keep their existing behaviour.
    if is_unknown_verb && let Some(verb) = args.get(1) {
        eprintln!("splitlane: unknown verb '{verb}'; see `splitlane --help` for the verb list");
        std::process::exit(2);
    }

    warn_if_legacy_run_install();
    #[cfg(target_os = "macos")]
    warn_if_rosetta_translated();

    // Materialize the embedded `splitlane-mcp` bridge to its
    // stable, non-versioned path so a registered MCP server keeps resolving
    // across Splitlane updates. SHA-compared + atomic: a no-op when the
    // on-disk bytes already match the embedded version. Non-fatal - the GUI
    // must still open if data_dir is unwritable; `splitlane mcp install`
    // refuses cleanly later rather than write a dangling path.
    match ai_hooks::extract::ensure_bridge_extracted() {
        Ok(path) => log::info!("splitlane: MCP bridge ready at {}", path.display()),
        Err(e) => log::warn!(
            "splitlane: MCP bridge extraction failed ({e:#}); `splitlane mcp install` will be unavailable until resolved"
        ),
    }

    #[cfg(target_os = "windows")]
    if let Err(err) = windows_app_identity::ensure_process_app_user_model_id() {
        log::warn!("splitlane: Windows app identity setup failed: {err}");
    }

    application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            // Load config early - needed for keybindings and window decorations
            let config = splitlane_config::loader::load_config();
            // Match Windows Terminal/PowerShell-style grayscale text
            // antialiasing. GPUI's platform default can pick subpixel
            // rendering on Windows/Linux; Splitlane's dark terminal surfaces
            // read cleaner without colored LCD fringes on thin mono glyphs.
            cx.set_text_rendering_mode(gpui::TextRenderingMode::Grayscale);
            // `apply_keybindings` clears the whole registry, so it now also
            // (re-)registers the TextInput / TextArea widget bindings itself
            // (agents composer textarea included) - no separate startup
            // call is needed, and a later re-apply can no longer strip them.
            keybindings::apply_keybindings(cx, &config.shortcuts);

            // Register every embedded `.ttf` under `assets/fonts/` BEFORE
            // any window opens, so GPUI's text system can resolve the
            // `Geist Mono` family (mono) and `Geist`
            // family (sans, 4 weights) Splitlane ships as the default
            // primaries - same strategy Zed uses with `.ZedMono` /
            // `.ZedSans` (`zed/assets/settings/default.json:29,57`).
            // Picking embedded families as the **primary** instead of
            // system families (Menlo / Cascadia Mono / DejaVu) sidesteps
            // the c3e2331 failure mode: Core Text inside a signed .app
            // bundle could return valid glyph_ids for a system family
            // and rasterize them as empty bitmaps; GPUI's per-Font
            // fallback chain only walks on missing-glyph not on
            // empty-raster, so the system primary "rendered" zero glyphs
            // and nothing fell through. With Geist Mono as the registered
            // primary, GPUI owns the font tables end-to-end. Iterates
            // the rust-embed registry (Zed pattern,
            // `zed/crates/assets/src/assets.rs:42`) so adding a new font
            // face is "drop a .ttf into assets/fonts/" with no Rust
            // change needed.
            if let Err(e) = assets::Assets.load_fonts(cx) {
                log::warn!(
                    "Assets::load_fonts failed: {e}; text rendering may fail on \
                     systems without a system monospace font"
                );
            }


            // macOS native menu bar. On Linux/Windows the call is
            // elided - GPUI's non-macOS platforms don't render a menu bar
            // and Linux gets no UI change.
            #[cfg(target_os = "macos")]
            {
                install_macos_menu_bar(cx);
                install_macos_menu_action_fallbacks(cx);
            }

            let bounds = crate::window_state::initial_bounds(cx);
            let decorations = match config.window_decorations.as_deref() {
                Some("server") => WindowDecorations::Server,
                Some("client") | None => WindowDecorations::Client,
                Some(other) => {
                    log::warn!(
                        "Invalid window_decorations value '{}', using 'client'",
                        other
                    );
                    WindowDecorations::Client
                }
            };

            // Reserve space on the left of the custom titlebar
            // for macOS traffic lights. The three red/yellow/green circles
            // live at x≈12-78px; the sidebar-aligned title-bar slot starts at
            // x=80 (see title_bar.rs). `..Default::default()` is load-bearing on
            // non-macOS (GPUI's TitlebarOptions may grow platform-specific
            // fields we don't set); clippy only flags it needless under
            // target_os = "macos" where traffic_light_position makes the
            // field list complete.
            #[cfg_attr(target_os = "macos", allow(clippy::needless_update))]
            let titlebar_options = gpui::TitlebarOptions {
                title: None,
                appears_transparent: true,
                #[cfg(target_os = "macos")]
                traffic_light_position: Some(point(px(12.0), px(12.0))),
                ..Default::default()
            };

            let window_result = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(crate::window_state::minimum_size()),
                    window_decorations: Some(decorations),
                    titlebar: Some(titlebar_options),
                    window_background: crate::app::constants::window_background_appearance(
                        config.window_backdrop.as_deref(),
                    ),
                    app_id: Some("splitlane".into()),
                    ..Default::default()
                },
                |window, cx| {
                    #[cfg(target_os = "windows")]
                    if crate::app::constants::window_backdrop_uses_mica(
                        config.window_backdrop.as_deref(),
                    ) {
                        crate::window_chrome::backdrop::apply_wallpaper_mica(
                            window,
                            crate::theme::active_theme().background.l > 0.5,
                        );
                    }
                    #[cfg(target_os = "macos")]
                    if crate::app::constants::macos_sidebar_material_enabled(
                        config.window_backdrop.as_deref(),
                    ) {
                        crate::window_chrome::macos_backdrop::apply_subtle_sidebar_material(
                            window,
                            crate::theme::active_theme().background.l > 0.5,
                            config.macos_chrome_material_enabled(),
                        );
                    }
                    #[cfg(target_os = "linux")]
                    crate::window_chrome::linux_backdrop::apply_subtle_chrome_material(window);

                    cx.new(StartupSplashView::new)
                },
            );

            match window_result {
                Ok(_) => cx.activate(true),
                Err(e) => {
                    log::error!("Failed to open Splitlane window: {e}");
                    #[cfg(target_os = "linux")]
                    eprintln!(
                        "Error: Splitlane requires a GPU with Vulkan support.\n\n\
                         Install mesa-vulkan-drivers (AMD/Intel) or your GPU's proprietary driver.\n\n\
                         Install commands:\n\
                         \x20 Debian/Ubuntu:  sudo apt install mesa-vulkan-drivers\n\
                         \x20 Fedora/RHEL:    sudo dnf install mesa-vulkan-drivers\n\
                         \x20 Arch:           sudo pacman -S vulkan-radeon vulkan-intel or nvidia-utils\n\n\
                         Run `vulkaninfo` to verify Vulkan support.\n\
                         If drivers are already installed, run with RUST_LOG=error for details.\n\n\
                         Underlying error: {e}"
                    );
                    #[cfg(target_os = "windows")]
                    eprintln!(
                        "Error: Splitlane could not create its GPU-backed window on Windows.\n\n\
                         Update your GPU driver from NVIDIA, AMD, Intel, or your PC vendor, then restart Splitlane.\n\
                         If this started after enabling a native backdrop, launch once with:\n\
                         \x20 SPLITLANE_WINDOW_BACKDROP=off\n\n\
                         Underlying error: {e}"
                    );
                    #[cfg(target_os = "macos")]
                    eprintln!(
                        "Error: Splitlane could not create its GPU-backed window on macOS.\n\n\
                         Update macOS and restart Splitlane. If this started after enabling a native backdrop, launch once with:\n\
                         \x20 SPLITLANE_WINDOW_BACKDROP=off\n\n\
                         Underlying error: {e}"
                    );
                    std::process::exit(1);
                }
            }
        });
}
