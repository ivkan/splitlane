//! Terminal theming with 36 color slots (see [`model::TerminalTheme`]),
//! compatible with Zed's terminal theme format.

mod builtin;
mod model;
mod watcher;

pub use builtin::{
    DEFAULT_LIGHT_THEME_NAME, DEFAULT_THEME_NAME, THEMES, ThemeEntry, harbor_dark, one_dark,
    theme_by_name,
};
pub use model::{
    DiffColors, SyntaxPalette, TerminalTheme, UiColors, text_on_fill, ui_colors, ui_colors_with,
};
pub use watcher::{
    ThemeWatcher, active_theme, config_mtime, invalidate_theme_cache, theme_generation,
};

// `sync_markdown_global_theme` lived here to keep a handful of slots in Zed's
// `GlobalTheme` aligned with Splitlane's palette, because Zed's `MarkdownElement`
// paint pass read them for table rows and borders. Splitlane renders markdown
// with its own `crate::markdown` (pulldown-cmark) widget, which reads
// `ui_colors()` directly and never touches Zed's global, so the sync - along
// with the `theme::init` bootstrap and the `ThemeSettingsProvider` shim in
// `main.rs` - maintained state nobody read. All of it went with the Zed
// `markdown` dependency.
