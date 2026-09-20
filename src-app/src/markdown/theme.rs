//! Semantic markdown palette derived from the active `TerminalTheme`.
//!
//! No hardcoded colors. The terminal theme owns the user-visible colors;
//! markdown borrows from it so a theme switch repaints everything.

use gpui::Hsla;

use crate::theme::{TerminalTheme, active_theme};

/// Resolved palette for a single render pass. Built once per `Render` call
/// from the current `active_theme()` snapshot - `Hsla` is `Copy`, so the
/// whole struct can be passed by value to per-block helpers without lifetime
/// pressure.
#[derive(Clone, Copy)]
pub(crate) struct MarkdownPalette {
    pub background: Hsla,
    /// Body text - the design's prose step, one below the name-and-label
    /// `text` the headings use.
    pub body: Hsla,
    /// A list item, one step below prose. The design draws the distinction
    /// (`#cdd2d8` for a paragraph, `#b9bfc7` for a list item) and it is the
    /// only thing separating a run of items from a run of sentences.
    pub list: Hsla,
    /// Heading color - the brightest text step.
    pub heading: Hsla,
    /// Code-block / inline-code background. The design sets it to the same
    /// wash an added diff line carries: code is quoted, not typed.
    pub code_bg: Hsla,
    pub code_fg: Hsla,
    /// The hairline around a fenced block. Blended from the code foreground
    /// rather than stated: the design names one hex for one theme, and this
    /// app has six.
    pub code_border: Hsla,
    /// Border / accent for blockquote left rail.
    pub blockquote_border: Hsla,
    pub blockquote_text: Hsla,
    /// Link color (matches the terminal's `link_text` so URL highlighting is
    /// consistent across surfaces).
    pub link: Hsla,
    /// Subtle hairline color for table borders + horizontal rules.
    pub rule: Hsla,
}

impl MarkdownPalette {
    pub(crate) fn from_active() -> Self {
        Self::from_terminal(&active_theme())
    }

    pub(crate) fn from_terminal(t: &TerminalTheme) -> Self {
        let ui = crate::theme::ui_colors_with(t);
        Self {
            background: t.background,
            body: ui.text_body,
            list: ui.text_secondary,
            heading: ui.text,
            code_bg: ui.vc_added_background,
            code_fg: t.bright_green,
            code_border: t.bright_green.opacity(0.22),
            blockquote_border: t.link_text,
            blockquote_text: t.dim_foreground,
            link: t.link_text,
            rule: ui.divider,
        }
    }
}
