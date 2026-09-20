//! Layout & timing constants shared across the app shell.
//!
//! Extracted from `main.rs` (anti edit-thrashing). All items are
//! `pub(crate)` and are addressed by their full path,
//! `crate::app::constants::SIDEBAR_WIDTH`.
//!
//! This paragraph used to promise a re-export at the crate root, so that
//! `crate::SIDEBAR_WIDTH` kept working. That re-export is gone and the promise
//! outlived it: `window_chrome/linux_backdrop.rs` believed it and did not
//! compile on Linux from July 2026 until CI first ran on 6 September. A comment
//! that describes an arrangement nobody maintains is worse than none.

use gpui::{Hsla, Pixels, WindowBackgroundAppearance, px};

use crate::ui_tokens as tok;

/// Sidebar width in pixels - shared between sidebar and title bar for alignment.
pub(crate) const SIDEBAR_WIDTH: f32 = 240.;
/// Outer title-bar inset aligned with workspace rows and the sidebar footer.
pub(crate) const TITLE_BAR_EDGE_INSET: Pixels = px(8.);
/// Inter-button rhythm for compact title-bar controls.
pub(crate) const TITLE_BAR_CONTROL_SPACING: Pixels = px(12.);
/// Compact custom control size used by Linux CSD title bars.
pub(crate) const TITLE_BAR_CONTROL_SIZE: Pixels = px(20.);
/// Minimum title-bar height. The design states 40; the rem-scaled height is
/// only allowed to grow past it, never to shrink under it.
pub(crate) const TITLE_BAR_MIN_HEIGHT: Pixels = tok::row::TITLE_BAR;
/// Inset between the window shell and the primary navigation card.
pub(crate) const SIDEBAR_CARD_INSET: f32 = 4.;
/// The primary navigation rails share the main panel's structural corner language.
pub(crate) const SIDEBAR_CARD_CORNER_RADIUS: Pixels = WINDOW_CORNER_RADIUS;
/// Inner CLI content inset. Combined with the pane's reserved 1px border,
/// this places the tab strip 4px from the main panel edge.
pub(crate) const PANE_CONTENT_INSET: f32 = 3.;
/// The margin between a pane's edge and its terminal grid, on **all four**
/// sides.
///
/// Ten pixels, which is one character cell of clearance at the default 14px
/// mono - the smallest margin that reads as a margin rather than as a
/// rendering fault. It used to be [`PANE_CONTENT_INSET`], applied on the left
/// only, and at three pixels a full-width TUI drew its own box border hard
/// against the pane's.
///
/// The design drew 18 (`16px 18px 20px 18px`); this app took this number
/// instead, with the rule behind it written down: **one cell on all four
/// sides, no more.** 18 a side is 36px, a little over four columns of an 8.4px
/// cell, and on a two-pane 1332pt window that takes each pane under the 80
/// columns a CLI's TUI lays itself out for - those numbers were written for a
/// column of prose, where they are about reading rhythm. The same argument
/// settles the vertical: a full-screen TUI draws to its last row, so 20px
/// under it is two rows of dead field the application inside cannot see or
/// use. One cell all round is the least that keeps the TUI's own box off the
/// pane's border, which is the only thing the gutter is for.
pub(crate) const PANE_TERMINAL_INSET: f32 = 10.;
/// Windows Mica and macOS Sidebar material already supply theme-aware tints.
/// The card remains fully transparent there so the material stays perceptible;
/// its border still defines the inset surface.
#[cfg(any(target_os = "windows", target_os = "macos"))]
const SIDEBAR_CARD_MATERIAL_OPACITY: f32 = 0.;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const SIDEBAR_CARD_MATERIAL_OPACITY: f32 = 0.84;

/// Selected rows carry a stronger lift than hover rows so current navigation
/// remains legible without a separate indicator. Linux precomposes these tints
/// over the opaque chrome color; macOS and Windows keep their native material.
const DARK_SIDEBAR_TAB_TINT: u32 = 0xffffff;
const LIGHT_SIDEBAR_TAB_TINT: u32 = 0x25262b;
const DARK_SIDEBAR_TAB_ACTIVE_OPACITY: f32 = 0.11;
const DARK_SIDEBAR_TAB_HOVER_OPACITY: f32 = 0.07;
const LIGHT_SIDEBAR_TAB_ACTIVE_OPACITY: f32 = 0.08;
const LIGHT_SIDEBAR_TAB_HOVER_OPACITY: f32 = 0.04;

/// Shared radius for the Agents search field and its primary navigation rows.
pub(crate) const SIDEBAR_TAB_CORNER_RADIUS: Pixels = tok::radius::CONTROL;

/// Height of one row in any list the app draws: the rail, the files tree, the
/// diff sidebar, session history, context menus. One role, one value - a list
/// that picks its own height makes two lists read as two kinds of thing.
///
/// The design splits this in two - 28 for a project row, 25 for a session row
/// under it - and that split is the rail's nesting, which is built separately. Until the
/// rail has two kinds of row, one height is the honest state.
pub(crate) const LIST_ROW_HEIGHT: Pixels = tok::row::PROJECT;

/// Native material used behind the main application window.
///
/// Windows delegates to GPUI's system backdrop support. On macOS Splitlane
/// installs a semantic AppKit sidebar material after the native window opens.
/// Linux starts opaque. Once the native handle exists, the Linux window layer
/// enables explicit alpha only for X11 CSD; Wayland CSD is alpha-capable by
/// construction and can keep opaque text rendering semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowBackdropPreference {
    Auto,
    Mica,
    Blurred,
    Transparent,
    Opaque,
}

pub(crate) fn window_backdrop_preference(config_value: Option<&str>) -> WindowBackdropPreference {
    if let Ok(value) = std::env::var("SPLITLANE_WINDOW_BACKDROP") {
        return parse_window_backdrop_preference(&value);
    }

    config_window_backdrop_preference(config_value)
}

fn config_window_backdrop_preference(config_value: Option<&str>) -> WindowBackdropPreference {
    #[cfg(target_os = "windows")]
    if let Some(value) = config_value.map(str::trim)
        && (value.eq_ignore_ascii_case("blurred") || value.eq_ignore_ascii_case("acrylic"))
    {
        return WindowBackdropPreference::Auto;
    }

    config_value
        .map(parse_window_backdrop_preference)
        .unwrap_or(WindowBackdropPreference::Auto)
}

fn parse_window_backdrop_preference(value: &str) -> WindowBackdropPreference {
    match value.trim().to_ascii_lowercase() {
        value if value.is_empty() || value == "auto" => WindowBackdropPreference::Auto,
        value if value == "mica" => WindowBackdropPreference::Mica,
        value if value == "blurred" || value == "acrylic" => WindowBackdropPreference::Blurred,
        value if value == "transparent" => WindowBackdropPreference::Transparent,
        value if value == "opaque" || value == "off" => WindowBackdropPreference::Opaque,
        value => {
            log::warn!("Invalid window_backdrop value '{value}', using 'auto'");
            WindowBackdropPreference::Auto
        }
    }
}

pub(crate) fn window_background_appearance(
    config_value: Option<&str>,
) -> WindowBackgroundAppearance {
    let preference = window_backdrop_preference(config_value);
    window_background_appearance_for_preference(preference)
}

fn window_background_appearance_for_preference(
    preference: WindowBackdropPreference,
) -> WindowBackgroundAppearance {
    #[cfg(target_os = "windows")]
    {
        match preference {
            WindowBackdropPreference::Auto | WindowBackdropPreference::Mica => {
                if windows_supports_system_backdrop() {
                    WindowBackgroundAppearance::MicaBackdrop
                } else {
                    WindowBackgroundAppearance::Opaque
                }
            }
            WindowBackdropPreference::Blurred => WindowBackgroundAppearance::Blurred,
            WindowBackdropPreference::Transparent => WindowBackgroundAppearance::Transparent,
            WindowBackdropPreference::Opaque => WindowBackgroundAppearance::Opaque,
        }
    }

    #[cfg(target_os = "macos")]
    {
        match preference {
            WindowBackdropPreference::Opaque => WindowBackgroundAppearance::Opaque,
            WindowBackdropPreference::Blurred => WindowBackgroundAppearance::Blurred,
            _ => WindowBackgroundAppearance::Transparent,
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = preference;
        WindowBackgroundAppearance::Opaque
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = preference;
        WindowBackgroundAppearance::Opaque
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn window_backdrop_uses_mica(config_value: Option<&str>) -> bool {
    matches!(
        window_background_appearance(config_value),
        WindowBackgroundAppearance::MicaBackdrop
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_sidebar_material_enabled(config_value: Option<&str>) -> bool {
    !matches!(
        window_backdrop_preference(config_value),
        WindowBackdropPreference::Opaque | WindowBackdropPreference::Transparent
    )
}

#[cfg(target_os = "windows")]
fn windows_supports_system_backdrop() -> bool {
    #[repr(C)]
    struct RtlOsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        service_pack: [u16; 128],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(version: *mut RtlOsVersionInfo) -> i32;
    }

    let mut version = RtlOsVersionInfo {
        size: std::mem::size_of::<RtlOsVersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform_id: 0,
        service_pack: [0; 128],
    };

    // NTSTATUS values greater than or equal to zero indicate success.
    unsafe { RtlGetVersion(&mut version) >= 0 && version.build >= 22_621 }
}

/// Fill used by chrome children inside the window shell.
///
/// `chrome` is the [`crate::theme::UiColors::chrome_for`] role, not a terminal
/// slot: this is a surface the design states by name, so the theme's own role
/// is what reaches it.
///
/// Windows must paint an opaque title bar when its dedicated chrome material is
/// disabled. The terminal can still activate the host-window backdrop, so a
/// transparent title bar would otherwise expose that terminal-only material.
/// Other platforms keep the child transparent and let the rounded shell own the
/// tint, avoiding rectangular paint outside GPUI's corner mask.
pub(crate) fn cockpit_chrome_background(chrome: Hsla, material_active: bool) -> Hsla {
    #[cfg(target_os = "windows")]
    {
        if material_active {
            gpui::transparent_black()
        } else {
            Hsla { a: 1.0, ..chrome }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (chrome, material_active);
        gpui::transparent_black()
    }
}

/// Fill for the inset primary navigation card.
///
/// Linux uses a fully opaque surface. Windows and macOS expose their raw native
/// materials, with the same opaque fallback when the material is off.
pub(crate) fn primary_sidebar_card_background(surface: Hsla, material_active: bool) -> Hsla {
    let opaque_surface = Hsla { a: 1.0, ..surface };
    if cfg!(target_os = "linux") || !material_active {
        opaque_surface
    } else {
        opaque_surface.opacity(SIDEBAR_CARD_MATERIAL_OPACITY)
    }
}

/// Window-level backdrop behind the application chrome.
///
/// This is what the rounded panel corners reveal in their clip notch, so it MUST
/// show through the transparent rail ([`cockpit_chrome_background`]) - otherwise
/// the corner exposes a different surface and the radius reads as a square patch.
/// Native semantic materials remain raw on macOS and Windows. Linux always
/// resolves to the opaque theme color: wallpaper never participates in the UI.
pub(crate) fn cockpit_backdrop_background(chrome: Hsla, material_active: bool) -> Hsla {
    #[cfg(target_os = "linux")]
    {
        let _ = material_active;
        Hsla { a: 1.0, ..chrome }
    }

    #[cfg(not(target_os = "linux"))]
    {
        if !material_active {
            chrome
        } else if cfg!(any(target_os = "windows", target_os = "macos")) {
            gpui::transparent_black()
        } else {
            chrome
        }
    }
}

/// Background for the selected tab in the CLI and Agents sidebars.
pub(crate) fn sidebar_tab_active_background() -> Hsla {
    sidebar_tab_background(
        LIGHT_SIDEBAR_TAB_ACTIVE_OPACITY,
        DARK_SIDEBAR_TAB_ACTIVE_OPACITY,
    )
}

/// Background for a hovered, non-selected sidebar tab.
pub(crate) fn sidebar_tab_hover_background() -> Hsla {
    sidebar_tab_background(
        LIGHT_SIDEBAR_TAB_HOVER_OPACITY,
        DARK_SIDEBAR_TAB_HOVER_OPACITY,
    )
}

fn sidebar_tab_background(light_opacity: f32, dark_opacity: f32) -> Hsla {
    let theme = crate::theme::active_theme();
    let is_light = theme.background.l > 0.5;
    let (tint, opacity) = if is_light {
        (LIGHT_SIDEBAR_TAB_TINT, light_opacity)
    } else {
        (DARK_SIDEBAR_TAB_TINT, dark_opacity)
    };
    let tint = Hsla::from(gpui::rgb(tint)).opacity(opacity);

    #[cfg(target_os = "linux")]
    {
        Hsla {
            a: 1.0,
            ..theme.title_bar_background
        }
        .blend(tint)
    }

    #[cfg(not(target_os = "linux"))]
    tint
}

/// Toast animation durations (ms). The `hold_ms` carried on each `Toast`
/// must match the dismiss timer in `push_toast` - otherwise the exit
/// animation plays early and the element persists as a ghost.
pub(crate) const TOAST_ENTER_MS: u64 = 180;
pub(crate) const TOAST_HOLD_MS: u64 = 1440;
pub(crate) const TOAST_EXIT_MS: u64 = 180;

/// Maximum number of closed-pane records kept for undo-close-pane.
pub(crate) const MAX_CLOSED_PANES: usize = 5;

/// Cumulative text budget for undo-close captured scrollback.
pub(crate) const MAX_CLOSED_PANE_SCROLLBACK_BYTES: usize = 2 * 1024 * 1024;

/// Width of the invisible border zone used for CSD edge/corner resize handles.
pub(crate) const RESIZE_BORDER: Pixels = px(10.0);
/// Radius of the visible application shell inside the transparent CSD shadow.
pub(crate) const WINDOW_CORNER_RADIUS: Pixels = tok::radius::WINDOW;
/// Hairline separating the themed shell from its native compositor shadow.
pub(crate) const WINDOW_BORDER_SIZE: Pixels = px(1.0);

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn linux_window_starts_opaque_until_the_display_backend_is_known() {
        for preference in [
            WindowBackdropPreference::Auto,
            WindowBackdropPreference::Mica,
            WindowBackdropPreference::Blurred,
            WindowBackdropPreference::Transparent,
            WindowBackdropPreference::Opaque,
        ] {
            assert_eq!(
                window_background_appearance_for_preference(preference),
                WindowBackgroundAppearance::Opaque
            );
        }
    }
}

#[cfg(test)]
mod material_tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn legacy_blurred_config_falls_back_to_auto_after_mica_subsetting_removal() {
        assert_eq!(
            config_window_backdrop_preference(Some("blurred")),
            WindowBackdropPreference::Auto
        );
        assert_eq!(
            config_window_backdrop_preference(Some("acrylic")),
            WindowBackdropPreference::Auto
        );
        assert_eq!(
            parse_window_backdrop_preference("blurred"),
            WindowBackdropPreference::Blurred
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn cockpit_children_stay_transparent_over_an_opaque_shell() {
        let background = Hsla::from(gpui::rgb(0x141414));

        assert_eq!(
            cockpit_chrome_background(background, false),
            gpui::transparent_black()
        );
        assert_eq!(cockpit_backdrop_background(background, false), background);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn disabled_chrome_material_keeps_title_bar_opaque() {
        let background = Hsla::from(gpui::rgb(0x141414));

        assert_eq!(
            cockpit_chrome_background(background, false),
            Hsla {
                a: 1.0,
                ..background
            }
        );
        assert_eq!(
            cockpit_chrome_background(background, true),
            gpui::transparent_black()
        );
        assert_eq!(cockpit_backdrop_background(background, false), background);
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn native_sidebar_card_exposes_raw_material() {
        let surface = Hsla::from(gpui::rgb(0x212122));
        let card = primary_sidebar_card_background(surface, true);

        assert_eq!(card.a, 0., "the sidebar must not veil native material");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_chrome_stays_opaque_when_material_is_requested() {
        let dark = gpui::hsla(0.71, 0.62, 0.32, 0.42);
        let light = gpui::hsla(0.09, 0.54, 0.78, 0.58);

        assert_eq!(
            cockpit_backdrop_background(dark, true),
            Hsla { a: 1.0, ..dark }
        );
        assert_eq!(
            cockpit_backdrop_background(light, true),
            Hsla { a: 1.0, ..light }
        );
        assert_eq!(primary_sidebar_card_background(dark, true).a, 1.0);
    }
}
